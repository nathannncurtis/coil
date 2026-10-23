//! Per-thread page caches: the current page and a free-list head for every
//! size class, owned by the thread state (spec sections 8.6 and 9.1). The
//! fast path pops the head; refill and page acquisition are calls into
//! the pool. Exhausted or unowned pages with live blocks are not lost:
//! they sit on the pool's abandoned lists until any cache adopts them.
//! When `tstate.rs` grows allocation caches (section 10.2) this moves
//! into `ThreadState`; until then each thread reaches its cache through
//! the TLS slot below, whose destruction — thread-local destructors
//! registered before it run after it — must not take allocation and
//! freeing down with it, so every access tolerates the slot being gone.

use core::sync::atomic::{AtomicU32, Ordering};

use std::cell::RefCell;

use super::page::PageHeader;
use super::size_class::NUM_CLASSES;

fn next_cache_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    // Id 0 means "no owner"; start at 1.
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct ThreadCache {
    pub id: u32,
    /// The page currently served for each class.
    current: [Option<&'static PageHeader>; NUM_CLASSES],
    /// Local free-list head per class; blocks link through their first
    /// word.
    free: [*mut u8; NUM_CLASSES],
}

impl ThreadCache {
    fn new() -> ThreadCache {
        ThreadCache {
            id: next_cache_id(),
            current: [None; NUM_CLASSES],
            free: [core::ptr::null_mut(); NUM_CLASSES],
        }
    }

    /// Pop a block of `class`, refilling from the pool when the local list
    /// runs out. Returns null when the OS is out of memory.
    pub fn alloc(&mut self, class: usize) -> *mut u8 {
        let head = self.free[class];
        if !head.is_null() {
            // SAFETY: free-list blocks are fully initialised and linked.
            self.free[class] = unsafe { (head as *mut *mut u8).read() };
            return head;
        }
        self.refill(class)
    }

    fn refill(&mut self, class: usize) -> *mut u8 {
        // `take` clears the slot first: if acquiring a fresh page fails we
        // must not keep serving from a page we already unhand below.
        if let Some(page) = self.current[class].take() {
            let stolen = page
                .thread_free
                .swap(core::ptr::null_mut(), Ordering::Acquire);
            if !stolen.is_null() {
                self.free[class] = stolen;
                self.current[class] = Some(page);
                return self.alloc(class);
            }
            self.abandon(page, page.base());
        }
        // Before growing the heap, adopt a page another cache gave up on;
        // its thread-free list is exactly the free capacity to reuse.
        if let Some(page) = super::pool::adopt_page(class, self.id) {
            self.current[class] = Some(page);
            let stolen = page
                .thread_free
                .swap(core::ptr::null_mut(), Ordering::Acquire);
            self.free[class] = stolen;
            return self.alloc(class);
        }
        let Some(page) = super::pool::acquire_page(class, self.id) else {
            return core::ptr::null_mut();
        };
        self.current[class] = Some(page);
        let stolen = page
            .thread_free
            .swap(core::ptr::null_mut(), Ordering::Acquire);
        self.free[class] = stolen;
        self.alloc(class)
    }

    /// Push `block` onto the local list of `class` (owner free of its own
    /// current page).
    pub fn push_local(&mut self, class: usize, block: *mut u8) {
        // SAFETY: the block is free and exclusively held by this thread.
        unsafe {
            (block as *mut *mut u8).write(self.free[class]);
        }
        self.free[class] = block;
    }

    /// Give up ownership of `page` (its local list is empty by
    /// construction): a fully free page goes idle, anything else joins
    /// the abandoned pages for another cache to adopt.
    fn abandon(&mut self, page: &'static PageHeader, base: usize) {
        debug_assert_eq!(page.current_for.load(Ordering::Acquire), self.id);
        page.current_for.store(0, Ordering::Release);
        super::pool::release_page(page, base);
    }

    /// Release every current page at thread death: fully free pages go
    /// idle, the rest are abandoned for other caches to adopt.
    fn release_all(&mut self) {
        for class in 0..NUM_CLASSES {
            let Some(page) = self.current[class].take() else {
                continue;
            };
            let base = page.base();
            // Hand the local list back to the page's thread-free list.
            let mut block = self.free[class];
            while !block.is_null() {
                // SAFETY: we own the list; each block is free.
                let next = unsafe { (block as *mut *mut u8).read() };
                super::free::push_thread_free(page, block);
                block = next;
            }
            self.free[class] = core::ptr::null_mut();
            self.abandon(page, base);
        }
    }
}

impl Drop for ThreadCache {
    fn drop(&mut self) {
        self.release_all();
    }
}

thread_local! {
    static CACHE: RefCell<ThreadCache> = RefCell::new(ThreadCache::new());
}

/// Pop a block of `class` through this thread's cache. After the thread's
/// TLS slot has been destroyed the request is served without a cache.
/// The block's `used` count is raised here. Null means out of memory.
pub fn alloc(class: usize) -> *mut u8 {
    match CACHE.try_with(|cache| cache.borrow_mut().alloc(class)) {
        Ok(block) => {
            if !block.is_null() {
                super::page::header_of(block)
                    .used
                    .fetch_add(1, Ordering::Relaxed);
            }
            block
        }
        Err(_) => alloc_without_cache(class),
    }
}

/// Serve one allocation once this thread's cache is gone (a destructor
/// registered before the cache may still allocate): take an unowned page
/// — adopting an abandoned one if possible — pop a single block from its
/// thread-free list, and park the page again so the rest stays reachable.
/// The pool lock per allocation only ever runs on this teardown path.
fn alloc_without_cache(class: usize) -> *mut u8 {
    let Some(page) =
        super::pool::adopt_page(class, 0).or_else(|| super::pool::acquire_page(class, 0))
    else {
        return core::ptr::null_mut();
    };
    let block = super::free::pop_thread_free(page);
    if !block.is_null() {
        page.used.fetch_add(1, Ordering::Relaxed);
    }
    super::pool::release_page(page, page.base());
    block
}

/// Owner-local free push. Returns false when this thread's cache is gone
/// (its TLS slot already destroyed), in which case the caller must use
/// the page's thread-free list.
pub fn try_push_local(class: usize, block: *mut u8) -> bool {
    CACHE
        .try_with(|cache| cache.borrow_mut().push_local(class, block))
        .is_ok()
}

/// This thread's cache id; 0 while there is none — including after its
/// TLS slot is destroyed, which routes owner-local frees to the pages'
/// thread-free lists.
pub fn current_id() -> u32 {
    CACHE.try_with(|cache| cache.borrow().id).unwrap_or(0)
}

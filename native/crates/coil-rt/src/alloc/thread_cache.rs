//! Per-thread page caches: the current page and a free-list head for every
//! size class, owned by the thread state (spec sections 8.6 and 9.1). The
//! fast path pops the head; refill and page acquisition are calls into
//! the pool. When `tstate.rs` grows allocation caches (section 10.2) this
//! moves into `ThreadState`; until then each thread reaches its cache
//! through the TLS slot below.

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

    /// Give up ownership of an exhausted `page` (its local list is empty
    /// by construction) and let a fully free page go idle.
    fn abandon(&mut self, page: &'static PageHeader, base: usize) {
        debug_assert_eq!(page.current_for.load(Ordering::Acquire), self.id);
        page.current_for.store(0, Ordering::Release);
        if page.used.load(Ordering::Acquire) == 0 {
            super::pool::recycle_page(page, base);
        }
    }

    /// Release every current page at thread death so the pool can recycle
    /// fully free pages.
    fn release_all(&mut self) {
        for class in 0..NUM_CLASSES {
            let Some(page) = self.current[class] else {
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
            page.current_for.store(0, Ordering::Release);
            if page.used.load(Ordering::Acquire) == 0 {
                super::pool::recycle_page(page, base);
            }
            self.current[class] = None;
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

/// Run `f` with this thread's cache.
pub fn with<R>(f: impl FnOnce(&mut ThreadCache) -> R) -> R {
    CACHE.with(|cache| f(&mut cache.borrow_mut()))
}

/// This thread's cache id; 0 before the cache exists.
pub fn current_id() -> u32 {
    CACHE.with(|cache| cache.borrow().id)
}

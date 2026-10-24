//! Freeing: any thread may free any block. The owner's frees push to its
//! local list; other threads push to the page's lock-free `thread_free`
//! list, which only the owning cache pops (spec sections 8.6 and 9.1).

use core::sync::atomic::Ordering;

use super::page::{DATA_OFFSET, GRANULE, PAGE_SIZE, PageHeader};
use super::{page_table, pool, thread_cache};

/// Push `block` onto `page`'s cross-thread free list (Treiber push; the
/// owning cache drains it with a swap).
pub(super) fn push_thread_free(page: &PageHeader, block: *mut u8) {
    let mut head = page.thread_free.load(Ordering::Relaxed);
    loop {
        // SAFETY: the block is free and exclusively held by this thread
        // until the compare-exchange publishes it.
        unsafe { (block as *mut *mut u8).write(head) };
        match page.thread_free.compare_exchange_weak(
            head,
            block,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => head = observed,
        }
    }
}

/// Pop one block from `page`'s cross-thread free list (Treiber pop);
/// null when the list is empty.
pub(super) fn pop_thread_free(page: &PageHeader) -> *mut u8 {
    let mut head = page.thread_free.load(Ordering::Relaxed);
    loop {
        if head.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: free-list blocks are linked through their first word.
        let next = unsafe { (head as *mut *mut u8).read() };
        match page.thread_free.compare_exchange_weak(
            head,
            next,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => return head,
            Err(observed) => head = observed,
        }
    }
}

/// Free the block at `ptr`, whatever regime or kind it is. Returns true
/// when `ptr` addressed a live GC-heap allocation (an object address for
/// counted-regime blocks, the block address otherwise).
///
/// During a collection's sweep window the free is queued instead (spec
/// section 9.3: nothing is reused before phase 9); the flush after the
/// sweep frees the queue for real.
///
/// # Safety
///
/// `ptr` must be null or a pointer this allocator returned, and must not
/// already have been freed.
pub unsafe fn free_block(ptr: *mut u8) -> bool {
    let addr = ptr as usize;
    if addr == 0 {
        return false;
    }
    let Some(page) = page_table::lookup(addr) else {
        return false;
    };
    let base = page.base();
    if !page.is_small() {
        // Large objects free their whole run; only the run's data address
        // (or a counted object 16 bytes past it) identifies one.
        let data = base + DATA_OFFSET;
        if addr != data && !(addr == data + GRANULE && page.has_prehdr(0)) {
            return false;
        }
        if crate::gc::counted::is_sweeping() {
            // SAFETY: the address was just validated against the run.
            unsafe { crate::gc::counted::queue_sweep_free(addr) };
            return true;
        }
        #[cfg(feature = "debug-heap")]
        if unsafe {
            release_or_quarantine(
                page,
                if page.has_prehdr(0) {
                    data + GRANULE
                } else {
                    data
                },
            )
        } {
            return true;
        }
        return pool::free_large(base);
    }
    let data = base + DATA_OFFSET;
    if addr < data || addr >= base + PAGE_SIZE {
        return false;
    }
    let block_size = page.block_size();
    let block = match (addr - data) % block_size {
        0 => addr,
        // A counted-regime pointer sits 16 bytes into its block, after
        // the pre-header.
        GRANULE if page.has_prehdr(page.granule_of(base, addr - GRANULE)) => addr - GRANULE,
        _ => return false,
    };
    let g0 = page.granule_of(base, block);
    let granules = block_size / GRANULE;
    if !page.allocated(g0) {
        return false;
    }
    if crate::gc::counted::is_sweeping() {
        // SAFETY: the block was just validated against the page's bits.
        unsafe { crate::gc::counted::queue_sweep_free(addr) };
        return true;
    }
    #[cfg(feature = "debug-heap")]
    if unsafe {
        let object = if page.has_prehdr(g0) {
            block + GRANULE
        } else {
            block
        };
        release_or_quarantine(page, object)
    } {
        return true;
    }
    page.clear_object_bits(g0, granules);
    let owner = page.current_for.load(Ordering::Acquire);
    if owner != 0
        && owner == thread_cache::current_id()
        && thread_cache::try_push_local(page.class as usize, block as *mut u8)
    {
        // Owner-local free: the block joins the cache's local list.
    } else {
        // Another thread's page, or this thread's cache is already
        // destroyed: the block rides the page's thread-free list.
        push_thread_free(page, block as *mut u8);
    }
    // The Release decrement publishes the bit clears and the list push.
    if page.used.fetch_sub(1, Ordering::Release) == 1 {
        pool::recycle_page(page, base);
    }
    true
}

/// The `--features debug-heap` treatment of a freed object: a counted
/// object's block is quarantined for one more collection (poisoned, hidden
/// from the walks, kept allocated — true, and the caller frees nothing);
/// every other object's body is poisoned on its way to the free list. The
/// allocator's free-list link takes the block's first word either way.
///
/// # Safety
///
/// `object` must be the object address of a live block this free is about
/// to release, whose page bits are still intact.
#[cfg(feature = "debug-heap")]
unsafe fn release_or_quarantine(page: &PageHeader, object: usize) -> bool {
    let granule = page.granule_of(page.base(), object);
    if !page.is_object(granule) {
        return false;
    }
    // SAFETY: `object` is a live object whose page bits are intact.
    if unsafe { crate::gc::counted::quarantine_counted(object) } {
        return true;
    }
    let base = page.base();
    let block = if page.is_small() {
        page.block_start(base, object)
    } else {
        base + DATA_OFFSET
    };
    // SAFETY: the block is about to be freed and is exclusively this
    // thread's.
    let capacity = unsafe { crate::alloc::block_capacity(block as *const u8) }.unwrap_or(0);
    if capacity > 8 {
        // SAFETY: the body is `capacity` bytes at the block.
        unsafe {
            core::ptr::write_bytes(
                (block + 8) as *mut u8,
                crate::alloc::page::POISON_BYTE,
                capacity - 8,
            )
        };
    }
    false
}

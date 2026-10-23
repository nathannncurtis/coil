//! The allocator: segments, pages, size classes, side bitmaps, large
//! objects and thread caches, plus the plain allocator behind `PyMem_*`
//! (spec section 9.1). The fast paths are per-thread and lock-free; only
//! the global page pool takes a lock (section 8.6).

pub mod foreign;
mod free;
pub mod page;
pub mod page_table;
pub mod pool;
pub mod pymem;
pub mod size_class;
pub mod sys;
pub mod thread_cache;

use core::ptr;
use core::sync::atomic::Ordering;

pub use free::free_block;
pub use page::{GRANULE, PageHeader};
pub use size_class::MAX_SMALL;

/// The 16-byte pre-header every counted-regime object carries (spec
/// section 9.3).
pub const PREHEADER: usize = GRANULE;

/// Allocate `size` bytes on the GC heap, zeroed, with `is_object` set on
/// the first granule: a compiled-code object. Null means out of memory.
pub fn alloc_object(size: usize) -> *mut u8 {
    let size = size.max(1);
    if size > MAX_SMALL {
        return pool::alloc_large(size, true);
    }
    alloc_small(size, true)
}

/// Allocate `size` bytes on the GC heap, zeroed, with `is_object` clear:
/// what `PyObject_Malloc` hands out until `PyObject_Init` runs. Null
/// means out of memory.
pub fn alloc_block(size: usize) -> *mut u8 {
    let size = size.max(1);
    if size > MAX_SMALL {
        return pool::alloc_large(size, false);
    }
    alloc_small(size, false)
}

/// Allocate a counted-regime object of `size` bytes with its 16-byte
/// pre-header: `has_prehdr` marks the block's first granule, `is_object`
/// the object start, and the returned pointer skips the pre-header, the
/// way `PyObject_GC_New` prepends `PyGC_Head` (spec section 9.3). The
/// pre-header is zeroed. Null means out of memory.
pub fn alloc_counted(size: usize) -> *mut u8 {
    let size = size.max(1);
    let total = size + PREHEADER;
    if total > MAX_SMALL {
        let block = pool::alloc_large(total, false);
        if block.is_null() {
            return block;
        }
        let header = page::header_of(block);
        header.set_has_prehdr(0);
        header.set_is_object(1);
        // SAFETY: the run covers `total` bytes starting at the block.
        return unsafe { block.add(PREHEADER) };
    }
    let class = size_class::class_of(total);
    let block = thread_cache::with(|cache| cache.alloc(class));
    if block.is_null() {
        return block;
    }
    let header = page::header_of(block);
    header.used.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the block was just popped and is exclusively ours.
    unsafe { ptr::write_bytes(block, 0, header.block_size()) };
    let base = header.base();
    let g0 = header.granule_of(base, block as usize);
    header.set_allocated_block(g0, header.block_size() / GRANULE, false);
    header.set_has_prehdr(g0);
    header.set_is_object(g0 + 1);
    // SAFETY: the pre-header sits at the block start.
    unsafe { block.add(PREHEADER) }
}

/// The fast small-object path: pop the thread's free-list head for the
/// class (a call when it is empty), zero the block, set its bits.
fn alloc_small(size: usize, is_object: bool) -> *mut u8 {
    let class = size_class::class_of(size);
    let block = thread_cache::with(|cache| cache.alloc(class));
    if block.is_null() {
        return block;
    }
    let header = page::header_of(block);
    header.used.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the block was just popped and is exclusively ours.
    unsafe { ptr::write_bytes(block, 0, header.block_size()) };
    let base = header.base();
    let g0 = header.granule_of(base, block as usize);
    header.set_allocated_block(g0, header.block_size() / GRANULE, is_object);
    block
}

/// `PyObject_Realloc` on GC-heap memory: the page header supplies the old
/// capacity. A null `ptr` allocates; a zero `new_size` frees.
///
/// # Safety
///
/// `ptr` must be null or a pointer this allocator returned; the result is
/// freed with `free_block`.
pub unsafe fn realloc_block(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() {
        return alloc_block(new_size);
    }
    if new_size == 0 {
        // SAFETY: `ptr` was returned by this allocator.
        unsafe { free_block(ptr) };
        return ptr::null_mut();
    }
    // SAFETY: `ptr` was returned by this allocator.
    let Some(old_capacity) = (unsafe { block_capacity(ptr) }) else {
        return ptr::null_mut();
    };
    if old_capacity >= new_size {
        return ptr;
    }
    let fresh = alloc_block(new_size);
    if fresh.is_null() {
        return fresh;
    }
    // SAFETY: both blocks are valid for the copied length.
    unsafe { ptr::copy_nonoverlapping(ptr, fresh, old_capacity.min(new_size)) };
    // SAFETY: `ptr` was returned by this allocator.
    unsafe { free_block(ptr) };
    fresh
}

/// Whether `addr` lies in a committed page of the GC heap.
pub fn contains(addr: usize) -> bool {
    page_table::lookup(addr).is_some()
}

/// The page header for `addr`, carrying the side bitmaps, or `None` when
/// `addr` is outside the GC heap.
pub fn page_of(addr: usize) -> Option<&'static PageHeader> {
    page_table::lookup(addr)
}

/// The capacity in bytes of the allocation at `ptr` (its block size, or
/// the run size for a large object), or `None` outside the heap.
///
/// # Safety
///
/// `ptr` must be null or a pointer this allocator returned.
pub unsafe fn block_capacity(ptr: *const u8) -> Option<usize> {
    let header = page_table::lookup(ptr as usize)?;
    if header.is_small() {
        if !header.allocated(header.granule_of(header.base(), ptr as usize)) {
            return None;
        }
        Some(header.block_size())
    } else {
        Some(header.run_pages as usize * page::PAGE_SIZE - page::DATA_OFFSET)
    }
}

/// The address of the object containing `addr`, found by walking back to
/// the granule with `is_object` set. `None` outside the heap or when no
/// object starts at or below `addr` within its block. For counted-regime
/// objects this is the object address, not the block base.
pub fn object_start(addr: usize) -> Option<usize> {
    let header = page_table::lookup(addr)?;
    let base = header.base();
    if !header.is_small() {
        return Some(base + page::DATA_OFFSET);
    }
    let block = header.block_start(base, addr);
    let first = header.granule_of(base, block);
    let last = header.granule_of(base, addr);
    (first..=last)
        .find(|&g| header.is_object(g))
        .map(|g| header.granule_addr(base, g))
}

/// The 16-byte pre-header of a counted-regime object, or `None` when
/// `object` does not carry one.
///
/// # Safety
///
/// `object` must be null or an object address this allocator returned.
pub unsafe fn preheader(object: *const u8) -> Option<*mut u8> {
    let header = page_table::lookup(object as usize)?;
    let g = header.granule_of(header.base(), object as usize);
    if g == 0 || !header.is_object(g) || !header.has_prehdr(g - 1) {
        return None;
    }
    Some((object as usize - PREHEADER) as *mut u8)
}

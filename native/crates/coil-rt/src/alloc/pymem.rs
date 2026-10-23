//! The plain allocator behind `PyMem_RawMalloc` and `PyMem_Malloc` — a
//! separate mimalloc-style size-class allocator, not the GC heap — and the
//! exported C symbols of the `PyMem_*` / `PyObject_Malloc` family with
//! their 3.12 signatures (spec section 9.1). `PyObject_Malloc` returns
//! GC-heap memory with `is_object` clear. The declarations live here until
//! `coil-capi-sys` exists.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use std::alloc::{Layout, alloc as sys_alloc, dealloc};
use std::cell::RefCell;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::size_class::{self, MAX_SMALL, NUM_CLASSES};

/// Every plain block carries a 16-byte header with its capacity.
const HEADER: usize = 16;

const CHUNK: usize = 1024 * 1024;

// ---------------------------------------------------------- plain allocator

struct PlainPool {
    free: [Vec<*mut u8>; NUM_CLASSES],
    chunk: *mut u8,
    chunk_used: usize,
}

// SAFETY: the raw chunk pointer is only touched while the pool mutex is
// held, so access is single-threaded despite the pointer type.
unsafe impl Send for PlainPool {}

static PLAIN_POOL: OnceLock<Mutex<PlainPool>> = OnceLock::new();

fn plain_pool() -> MutexGuard<'static, PlainPool> {
    PLAIN_POOL
        .get_or_init(|| {
            Mutex::new(PlainPool {
                free: [(); NUM_CLASSES].map(|()| Vec::new()),
                chunk: ptr::null_mut(),
                chunk_used: CHUNK,
            })
        })
        .lock()
        .expect("plain pool poisoned")
}

struct PlainCache {
    head: [*mut u8; NUM_CLASSES],
    len: [u32; NUM_CLASSES],
}

thread_local! {
    static PLAIN_CACHE: RefCell<PlainCache> = const {
        RefCell::new(PlainCache {
            head: [ptr::null_mut(); NUM_CLASSES],
            len: [0; NUM_CLASSES],
        })
    };
}

/// Capacity recorded in a plain block's header.
///
/// SAFETY: `data` must be a plain-allocator block.
unsafe fn capacity(data: *mut u8) -> usize {
    // SAFETY: the header is the 16 bytes before a plain block.
    unsafe { (data.sub(HEADER) as *const usize).read() }
}

/// Carve a class block from the current chunk. Pool lock held.
fn carve(pool: &mut PlainPool, class: usize) -> *mut u8 {
    let block_size = size_class::class_size(class);
    if pool.chunk_used + HEADER + block_size > CHUNK {
        let layout = Layout::from_size_align(CHUNK, 16).expect("chunk layout");
        // SAFETY: the layout has nonzero size and valid alignment.
        let chunk = unsafe { sys_alloc(layout) };
        if chunk.is_null() {
            return ptr::null_mut();
        }
        pool.chunk = chunk;
        pool.chunk_used = 0;
    }
    let block = unsafe { pool.chunk.add(pool.chunk_used) };
    pool.chunk_used += HEADER + block_size;
    unsafe { (block as *mut usize).write(block_size) };
    unsafe { block.add(HEADER) }
}

/// Allocate `size` bytes from the plain allocator. Contents are
/// unspecified; null means out of memory.
pub fn plain_malloc(size: usize) -> *mut u8 {
    let size = size.max(1);
    if size > MAX_SMALL {
        let capacity = size.next_multiple_of(HEADER);
        let layout = Layout::from_size_align(capacity + HEADER, 16).expect("layout");
        // SAFETY: nonzero size, valid alignment.
        let block = unsafe { sys_alloc(layout) };
        if block.is_null() {
            return ptr::null_mut();
        }
        unsafe { (block as *mut usize).write(capacity) };
        return unsafe { block.add(HEADER) };
    }
    let class = size_class::class_of(size);
    let mut block = ptr::null_mut();
    PLAIN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.head[class].is_null() {
            block = cache.head[class];
            // SAFETY: free-list blocks are linked through their first word.
            cache.head[class] = unsafe { (block as *mut *mut u8).read() };
            cache.len[class] -= 1;
        }
    });
    if block.is_null() {
        let mut pool = plain_pool();
        block = match pool.free[class].pop() {
            Some(block) => block,
            None => carve(&mut pool, class),
        };
    }
    block
}

/// Allocate `nelem * elsize` zeroed bytes; null on size overflow.
pub fn plain_calloc(nelem: usize, elsize: usize) -> *mut u8 {
    let Some(size) = nelem.checked_mul(elsize) else {
        return ptr::null_mut();
    };
    let data = plain_malloc(size);
    if !data.is_null() {
        // SAFETY: `capacity` bytes belong to this block.
        unsafe { ptr::write_bytes(data, 0, capacity(data)) };
    }
    data
}

/// Resize a plain block; contents up to the old capacity are preserved.
///
/// # Safety
///
/// `data` must be null or a plain-allocator block, freed at most once.
pub unsafe fn plain_realloc(data: *mut u8, new_size: usize) -> *mut u8 {
    if data.is_null() {
        return plain_malloc(new_size);
    }
    // SAFETY: caller passed a plain-allocator block.
    let old_capacity = unsafe { capacity(data) };
    if new_size == 0 {
        // SAFETY: `data` is a plain block.
        unsafe { plain_free(data) };
        return ptr::null_mut();
    }
    if old_capacity >= new_size {
        return data;
    }
    let fresh = plain_malloc(new_size);
    if fresh.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: both blocks are valid for their capacities.
    unsafe { ptr::copy_nonoverlapping(data, fresh, old_capacity) };
    // SAFETY: `data` is a plain block.
    unsafe { plain_free(data) };
    fresh
}

/// Free a plain block. Null is ignored.
///
/// # Safety
///
/// `data` must be null or a plain-allocator block, freed at most once.
pub unsafe fn plain_free(data: *mut u8) {
    if data.is_null() {
        return;
    }
    // SAFETY: caller passed a plain-allocator block.
    let capacity = unsafe { capacity(data) };
    if capacity > MAX_SMALL {
        let layout = Layout::from_size_align(capacity + HEADER, 16).expect("layout");
        // SAFETY: allocated with the same layout in `plain_malloc`.
        unsafe { dealloc(data.sub(HEADER), layout) };
        return;
    }
    let class = size_class::class_of(capacity);
    let overflow = PLAIN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // SAFETY: the block is free; link it ahead of the local head.
        unsafe { (data as *mut *mut u8).write(cache.head[class]) };
        cache.head[class] = data;
        cache.len[class] += 1;
        cache.len[class] > 1024
    });
    if overflow {
        // Hand the whole local list for the class to the global pool so a
        // freeing thread cannot accumulate blocks without bound.
        PLAIN_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let mut block = cache.head[class];
            let mut moved = Vec::new();
            while !block.is_null() {
                // SAFETY: we own the list.
                let next = unsafe { (block as *mut *mut u8).read() };
                moved.push(block);
                block = next;
            }
            cache.head[class] = ptr::null_mut();
            cache.len[class] = 0;
            plain_pool().free[class].append(&mut moved);
        });
    }
}

// ------------------------------------------------------------ C API surface

pub type PyMemAllocatorDomain = i32;

pub const PYMEM_DOMAIN_RAW: PyMemAllocatorDomain = 0;
pub const PYMEM_DOMAIN_MEM: PyMemAllocatorDomain = 1;
pub const PYMEM_DOMAIN_OBJ: PyMemAllocatorDomain = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyMemAllocatorEx {
    pub ctx: *mut c_void,
    pub malloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub calloc: Option<unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void>,
    pub realloc: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void>,
    pub free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

// Overrides are published by `PyMem_SetAllocator` through one atomic
// pointer per domain, so reading one is a plain acquire load and the
// `PyMem_*` / `PyObject_*` entry points take no lock (spec section 8.6);
// a replaced override is never freed, so a reader that already loaded it
// keeps a valid reference.
static RAW_OVERRIDE: AtomicPtr<PyMemAllocatorEx> = AtomicPtr::new(ptr::null_mut());
static MEM_OVERRIDE: AtomicPtr<PyMemAllocatorEx> = AtomicPtr::new(ptr::null_mut());
static OBJ_OVERRIDE: AtomicPtr<PyMemAllocatorEx> = AtomicPtr::new(ptr::null_mut());

fn override_slot(domain: PyMemAllocatorDomain) -> Option<&'static AtomicPtr<PyMemAllocatorEx>> {
    match domain {
        PYMEM_DOMAIN_RAW => Some(&RAW_OVERRIDE),
        PYMEM_DOMAIN_MEM => Some(&MEM_OVERRIDE),
        PYMEM_DOMAIN_OBJ => Some(&OBJ_OVERRIDE),
        _ => None,
    }
}

/// The allocator installed for `domain`, if any. Lock-free.
fn override_for(domain: PyMemAllocatorDomain) -> Option<&'static PyMemAllocatorEx> {
    let slot = override_slot(domain)?;
    let allocator = slot.load(Ordering::Acquire);
    if allocator.is_null() {
        None
    } else {
        // SAFETY: published by `PyMem_SetAllocator` from a live value and
        // never freed, so the reference stays valid for the process.
        Some(unsafe { &*allocator })
    }
}

unsafe fn domain_malloc(domain: PyMemAllocatorDomain, size: usize) -> *mut c_void {
    if let Some(PyMemAllocatorEx {
        malloc: Some(malloc),
        ctx,
        ..
    }) = override_for(domain).copied()
    {
        // SAFETY: the allocator came from `PyMem_SetAllocator`; calling it
        // through its own context is the documented contract.
        return unsafe { malloc(ctx, size) };
    }
    let data = if domain == PYMEM_DOMAIN_OBJ {
        super::alloc_block(size)
    } else {
        plain_malloc(size)
    };
    data as *mut c_void
}

unsafe fn domain_calloc(domain: PyMemAllocatorDomain, nelem: usize, elsize: usize) -> *mut c_void {
    if let Some(PyMemAllocatorEx {
        calloc: Some(calloc),
        ctx,
        ..
    }) = override_for(domain).copied()
    {
        // SAFETY: see `domain_malloc`.
        return unsafe { calloc(ctx, nelem, elsize) };
    }
    let Some(size) = nelem.checked_mul(elsize) else {
        return ptr::null_mut();
    };
    let data = if domain == PYMEM_DOMAIN_OBJ {
        super::alloc_block(size)
    } else {
        plain_calloc(nelem, elsize)
    };
    data as *mut c_void
}

unsafe fn domain_realloc(
    domain: PyMemAllocatorDomain,
    data: *mut c_void,
    new_size: usize,
) -> *mut c_void {
    if let Some(PyMemAllocatorEx {
        realloc: Some(realloc),
        ctx,
        ..
    }) = override_for(domain).copied()
    {
        // SAFETY: see `domain_malloc`.
        return unsafe { realloc(ctx, data, new_size) };
    }
    if domain == PYMEM_DOMAIN_OBJ {
        // SAFETY: the caller passed a GC-heap block (C API contract).
        return (unsafe { super::realloc_block(data as *mut u8, new_size) }) as *mut c_void;
    }
    // SAFETY: the caller passed a plain block (C API contract).
    (unsafe { plain_realloc(data as *mut u8, new_size) }) as *mut c_void
}

unsafe fn domain_free(domain: PyMemAllocatorDomain, data: *mut c_void) {
    if let Some(PyMemAllocatorEx {
        free: Some(free),
        ctx,
        ..
    }) = override_for(domain).copied()
    {
        // SAFETY: see `domain_malloc`.
        unsafe { free(ctx, data) };
        return;
    }
    if domain == PYMEM_DOMAIN_OBJ {
        // SAFETY: the caller passed a GC-heap block (C API contract).
        unsafe { super::free_block(data as *mut u8) };
    } else {
        // SAFETY: the caller passed a plain block (C API contract).
        unsafe { plain_free(data as *mut u8) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyMem_RawMalloc(size: usize) -> *mut c_void {
    // SAFETY: routes to the plain allocator or an installed override.
    unsafe { domain_malloc(PYMEM_DOMAIN_RAW, size) }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyMem_RawCalloc(nelem: usize, elsize: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_calloc(PYMEM_DOMAIN_RAW, nelem, elsize) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_RawRealloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_realloc(PYMEM_DOMAIN_RAW, ptr, new_size) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_RawFree(ptr: *mut c_void) {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_free(PYMEM_DOMAIN_RAW, ptr) }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyMem_Malloc(size: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_malloc(PYMEM_DOMAIN_MEM, size) }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyMem_Calloc(nelem: usize, elsize: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_calloc(PYMEM_DOMAIN_MEM, nelem, elsize) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_Realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_realloc(PYMEM_DOMAIN_MEM, ptr, new_size) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_Free(ptr: *mut c_void) {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_free(PYMEM_DOMAIN_MEM, ptr) }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyObject_Malloc(size: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_malloc(PYMEM_DOMAIN_OBJ, size) }
}

#[unsafe(no_mangle)]
pub extern "C" fn PyObject_Calloc(nelem: usize, elsize: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_calloc(PYMEM_DOMAIN_OBJ, nelem, elsize) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_realloc(PYMEM_DOMAIN_OBJ, ptr, new_size) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Free(ptr: *mut c_void) {
    // SAFETY: see `PyMem_RawMalloc`.
    unsafe { domain_free(PYMEM_DOMAIN_OBJ, ptr) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_SetAllocator(
    domain: PyMemAllocatorDomain,
    allocator: *mut PyMemAllocatorEx,
) {
    let Some(slot) = override_slot(domain) else {
        return;
    };
    let published = if allocator.is_null() {
        ptr::null_mut()
    } else {
        // SAFETY: the caller passed a live allocator by reference; copy it.
        let allocator = unsafe { ptr::read(allocator) };
        // The previous override, if any, is intentionally left allocated:
        // a concurrent reader may still hold a reference, and installing
        // an allocator is a rare startup-time call.
        Box::into_raw(Box::new(allocator))
    };
    slot.swap(published, Ordering::Release);
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// Pointer arguments follow the C API: passing anything but null or a
/// block the matching allocator returned is undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_GetAllocator(
    domain: PyMemAllocatorDomain,
    allocator: *mut PyMemAllocatorEx,
) {
    if allocator.is_null() {
        return;
    }
    let current = override_for(domain)
        .copied()
        .unwrap_or_else(|| default_allocator(domain));
    // SAFETY: the caller passed writable storage for one allocator.
    unsafe { ptr::write(allocator, current) };
}

// The builtin allocator of each domain, reported to `PyMem_GetAllocator`
// when no override is installed; the context word is unused.
unsafe extern "C" fn plain_shim_malloc(_ctx: *mut c_void, size: usize) -> *mut c_void {
    plain_malloc(size).cast()
}

unsafe extern "C" fn plain_shim_calloc(
    _ctx: *mut c_void,
    nelem: usize,
    elsize: usize,
) -> *mut c_void {
    plain_calloc(nelem, elsize).cast()
}

unsafe extern "C" fn plain_shim_realloc(
    _ctx: *mut c_void,
    data: *mut c_void,
    new_size: usize,
) -> *mut c_void {
    // SAFETY: plain allocator entry point.
    unsafe { plain_realloc(data.cast(), new_size) }.cast()
}

unsafe extern "C" fn plain_shim_free(_ctx: *mut c_void, data: *mut c_void) {
    // SAFETY: plain allocator entry point.
    unsafe { plain_free(data.cast()) };
}

unsafe extern "C" fn gc_shim_malloc(_ctx: *mut c_void, size: usize) -> *mut c_void {
    super::alloc_block(size).cast()
}

unsafe extern "C" fn gc_shim_calloc(_ctx: *mut c_void, nelem: usize, elsize: usize) -> *mut c_void {
    let Some(size) = nelem.checked_mul(elsize) else {
        return ptr::null_mut();
    };
    super::alloc_block(size).cast()
}

unsafe extern "C" fn gc_shim_realloc(
    _ctx: *mut c_void,
    data: *mut c_void,
    new_size: usize,
) -> *mut c_void {
    // SAFETY: GC-heap entry point.
    unsafe { super::realloc_block(data.cast(), new_size) }.cast()
}

unsafe extern "C" fn gc_shim_free(_ctx: *mut c_void, data: *mut c_void) {
    // SAFETY: GC-heap entry point.
    unsafe { super::free_block(data.cast()) };
}

fn default_allocator(domain: PyMemAllocatorDomain) -> PyMemAllocatorEx {
    let plain = || PyMemAllocatorEx {
        ctx: ptr::null_mut(),
        malloc: Some(plain_shim_malloc),
        calloc: Some(plain_shim_calloc),
        realloc: Some(plain_shim_realloc),
        free: Some(plain_shim_free),
    };
    let gc = PyMemAllocatorEx {
        ctx: ptr::null_mut(),
        malloc: Some(gc_shim_malloc),
        calloc: Some(gc_shim_calloc),
        realloc: Some(gc_shim_realloc),
        free: Some(gc_shim_free),
    };
    if domain == PYMEM_DOMAIN_OBJ {
        gc
    } else {
        plain()
    }
}

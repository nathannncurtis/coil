//! Allocator conformance: bitmaps, page-table lookups, large objects,
//! thread safety and reuse (spec section 9.1, task r04).

use std::sync::atomic::{AtomicUsize, Ordering};

use python312::alloc::{
    self, alloc_block, alloc_counted, alloc_object, contains, object_start, page_of,
};

// Test-level wrappers for the unsafe allocator entry points: every
// pointer handed to them in this file comes from the same allocator and
// is freed at most once.
fn free(ptr: *mut u8) -> bool {
    // SAFETY: see above.
    unsafe { alloc::free_block(ptr) }
}

fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    // SAFETY: see above.
    unsafe { alloc::realloc_block(ptr, new_size) }
}

fn preheader(object: *const u8) -> Option<*mut u8> {
    // SAFETY: see above.
    unsafe { alloc::preheader(object) }
}

fn block_capacity(ptr: *const u8) -> Option<usize> {
    // SAFETY: see above.
    unsafe { alloc::block_capacity(ptr) }
}

#[test]
fn alloc_object_is_zeroed_and_marked() {
    let ptr = alloc_object(40);
    assert!(!ptr.is_null());
    let header = page_of(ptr as usize).expect("object in heap");
    let base = header.base();
    let g = header.granule_of(base, ptr as usize);
    assert!(header.is_object(g));
    assert!(header.allocated(g));
    assert_eq!(header.block_size(), 48);
    // The three granules of the 48-byte block are allocated.
    assert!(header.allocated(g + 1) && header.allocated(g + 2));
    // Zeroed.
    for i in 0..40 {
        assert_eq!(unsafe { *ptr.add(i) }, 0);
    }
    // Outside every object: is_object clear on a fresh page's neighbours
    // is not assertable (other tests share the heap), so only check ours.
    assert!(free(ptr));
}

#[test]
fn bitmap_bookkeeping_survives_free() {
    let ptr = alloc_object(200);
    let header = page_of(ptr as usize).unwrap();
    let base = header.base();
    let g = header.granule_of(base, ptr as usize);
    header.set_mark(g);
    header.set_regime(g);
    header.set_c_created(g);
    header.set_escaped(g);
    header.set_counted_slots(g);
    header.set_finalized(g);
    header.set_has_finalizer(g);
    assert!(header.mark(g));
    assert!(header.regime(g));
    assert!(header.c_created(g));
    assert!(header.escaped(g));
    assert!(header.counted_slots(g));
    assert!(header.finalized(g));
    assert!(header.has_finalizer(g));
    header.clear_mark(g);
    assert!(!header.mark(g));

    assert!(free(ptr));
    // Freeing clears every bit of the block in all ten bitmaps.
    for granule in g..g + header.block_size() / 16 {
        assert!(!header.allocated(granule));
        assert!(!header.is_object(granule));
        assert!(!header.mark(granule));
        assert!(!header.regime(granule));
        assert!(!header.c_created(granule));
        assert!(!header.escaped(granule));
        assert!(!header.counted_slots(granule));
        assert!(!header.has_prehdr(granule));
        assert!(!header.finalized(granule));
        assert!(!header.has_finalizer(granule));
    }
    // A freed address is not an object any more.
    assert!(object_start(ptr as usize).is_none());
}

#[test]
fn counted_objects_carry_a_preheader() {
    let _guard = LARGE_RUN_LOCK.lock().unwrap();
    let ptr = alloc_counted(100);
    assert!(!ptr.is_null());
    let header = page_of(ptr as usize).unwrap();
    let base = header.base();
    let object_g = header.granule_of(base, ptr as usize);
    let block = header.block_start(base, ptr as usize);
    let block_g = header.granule_of(base, block);
    assert_eq!(object_g, block_g + 1);
    assert!(header.has_prehdr(block_g));
    assert!(header.is_object(object_g));
    assert_eq!(header.block_size(), 128); // 100 rounded up + 16 pre-header
    // The pre-header is the 16 bytes before the object and is zeroed.
    let pre = preheader(ptr).expect("counted object has a pre-header");
    assert_eq!(pre as usize, ptr as usize - 16);
    for i in 0..16 {
        assert_eq!(unsafe { *pre.add(i) }, 0);
    }
    // Freeing through the object address works.
    assert!(free(ptr));
    assert!(preheader(ptr).is_none());

    // Large counted object: `object_start` reports the object address,
    // not the pre-header address, exactly as on small pages.
    let big = alloc_counted(40_000);
    assert!(!big.is_null());
    let header = page_of(big as usize).unwrap();
    assert!(!header.is_small());
    assert!(header.has_prehdr(0));
    assert!(header.is_object(1));
    assert_eq!(object_start(big as usize), Some(big as usize));
    assert_eq!(object_start(big as usize + 32), Some(big as usize));
    let pre = preheader(big).expect("counted large object has a pre-header");
    assert_eq!(pre as usize, big as usize - 16);
    assert!(free(big));
    assert!(object_start(big as usize).is_none());
    assert!(preheader(big).is_none());
}

#[test]
fn page_table_lookups() {
    let ptr = alloc_object(64);
    assert!(contains(ptr as usize));
    let header = page_of(ptr as usize).unwrap();
    assert_eq!(header.magic, python312::alloc::page::MAGIC);
    assert_eq!(object_start(ptr as usize), Some(ptr as usize));
    // Interior addresses map to their object.
    assert_eq!(object_start(ptr as usize + 48), Some(ptr as usize));

    // Out-of-heap addresses: stack, system heap, and plain PyMem blocks.
    let stack = &header as *const _ as usize;
    assert!(!contains(stack));
    let boxed = Box::new(0u8);
    assert!(!contains(&*boxed as *const u8 as usize));
    drop(boxed);
    let plain = python312::alloc::pymem::plain_malloc(100);
    assert!(!plain.is_null());
    assert!(!contains(plain as usize));
    assert!(page_of(plain as usize).is_none());
    // SAFETY: the pointer came from `plain_malloc` in this test.
    unsafe { python312::alloc::pymem::plain_free(plain) };

    assert!(free(ptr));
}

#[test]
fn large_objects() {
    let _guard = LARGE_RUN_LOCK.lock().unwrap();
    // A multi-page run: lookups on interior pages resolve through the run
    // head.
    let run = alloc_object(300_000); // 5 pages
    assert!(!run.is_null());
    assert_eq!(
        object_start(run as usize + 2 * 64 * 1024 + 8),
        Some(run as usize)
    );
    let header = page_of(run as usize + 2 * 64 * 1024).unwrap();
    assert!(!header.is_small());
    assert_eq!(header.run_pages, 5);
    assert!(free(run));

    // Just past the last size class.
    for size in [32 * 1024 + 1, 100_000, 5 * 1024 * 1024] {
        let ptr = alloc_object(size);
        assert!(!ptr.is_null());
        assert!(contains(ptr as usize));
        let header = page_of(ptr as usize).unwrap();
        assert!(!header.is_small());
        assert!(header.is_object(0));
        let capacity = block_capacity(ptr).expect("capacity");
        assert!(capacity >= size);
        for i in 0..size {
            assert_eq!(unsafe { *ptr.add(i) }, 0, "not zeroed at {i}");
        }
        // Interior addresses resolve to the large object start.
        assert_eq!(object_start(ptr as usize + 1000), Some(ptr as usize));
        assert!(free(ptr));
        // The run stays committed — address space is held — but no object
        // claims it any more.
        assert!(contains(ptr as usize));
        assert_eq!(object_start(ptr as usize), None);
        assert_eq!(object_start(ptr as usize + 1000), None);
    }
    // A freed run is reused for a later large object of the same size.
    let a = alloc_object(100_000);
    assert!(free(a));
    let b = alloc_object(100_000);
    assert_eq!(a, b, "first-fit should reuse the freed run");
    assert!(free(b));
}

#[test]
fn memory_is_reused_after_free() {
    // Same thread, same class: the freed block tops the local free list.
    let a = alloc_object(700);
    assert!(free(a));
    let b = alloc_object(700);
    assert_eq!(a, b);
    assert!(free(b));

    // A whole page's worth of one class, freed and reallocated.
    let mut blocks = Vec::new();
    for _ in 0..10_000 {
        blocks.push(alloc_object(300));
    }
    let mut seen = std::collections::HashSet::new();
    for &ptr in &blocks {
        assert!(seen.insert(ptr as usize), "duplicate block handed out");
    }
    let first = blocks[0];
    for ptr in blocks.drain(..) {
        assert!(free(ptr));
    }
    let fresh = alloc_object(300);
    assert!(
        seen.contains(&(fresh as usize)),
        "fresh allocation must reuse freed memory"
    );
    let _ = first;

    // PyObject_Malloc / PyObject_Free round-trip through the GC heap with
    // is_object clear until init.
    let raw = alloc_block(96);
    let header = page_of(raw as usize).unwrap();
    assert!(!header.is_object(header.granule_of(header.base(), raw as usize)));
    assert!(header.allocated(header.granule_of(header.base(), raw as usize)));
    assert!(free(raw));
}

#[test]
fn realloc_moves_and_preserves() {
    let ptr = alloc_block(64);
    for i in 0..64 {
        unsafe { *ptr.add(i) = i as u8 };
    }
    let bigger = realloc(ptr, 200);
    assert!(!bigger.is_null());
    for i in 0..64 {
        assert_eq!(unsafe { *bigger.add(i) }, i as u8);
    }
    // Shrinking in place.
    let same = realloc(bigger, 16);
    assert_eq!(same, bigger);
    assert!(free(same));
}

/// The allocator-override tests install process-wide state for the PyMem
/// domains, so they must not overlap each other.
static PYMEM_OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn pymem_plain_allocator() {
    let _guard = PYMEM_OVERRIDE_LOCK.lock().unwrap();
    unsafe extern "C" {
        fn PyMem_RawMalloc(size: usize) -> *mut core::ffi::c_void;
        fn PyMem_RawCalloc(nelem: usize, elsize: usize) -> *mut core::ffi::c_void;
        fn PyMem_RawRealloc(ptr: *mut core::ffi::c_void, new_size: usize)
        -> *mut core::ffi::c_void;
        fn PyMem_RawFree(ptr: *mut core::ffi::c_void);
        fn PyMem_Malloc(size: usize) -> *mut core::ffi::c_void;
        fn PyObject_Malloc(size: usize) -> *mut core::ffi::c_void;
        fn PyObject_Free(ptr: *mut core::ffi::c_void);
    }
    // SAFETY: plain C entry points exercised with ordinary arguments.
    unsafe {
        let raw = PyMem_RawMalloc(100);
        assert!(!raw.is_null());
        assert!(!contains(raw as usize), "PyMem is not the GC heap");
        let zeroed = PyMem_RawCalloc(25, 4);
        assert!(!zeroed.is_null());
        for i in 0..100 {
            assert_eq!(*(zeroed as *const u8).add(i), 0);
        }
        let grown = PyMem_RawRealloc(raw, 5_000);
        assert!(!grown.is_null());
        assert!(!contains(grown as usize));
        PyMem_RawFree(grown);
        PyMem_RawFree(zeroed);
        PyMem_RawFree(core::ptr::null_mut());

        let mem = PyMem_Malloc(3_000_000); // large, system-backed
        assert!(!mem.is_null());
        PyMem_RawFree(mem);

        // PyObject_Malloc lands in the GC heap with is_object clear.
        let obj = PyObject_Malloc(48);
        assert!(!obj.is_null());
        assert!(contains(obj as usize));
        let header = page_of(obj as usize).unwrap();
        assert!(!header.is_object(header.granule_of(header.base(), obj as usize)));
        PyObject_Free(obj);
    }
}

#[test]
fn foreign_object_table() {
    let ptr = alloc_object(16);
    let foreign = &ptr as *const *mut u8 as usize;
    alloc::foreign::register(foreign);
    assert!(alloc::foreign::contains(foreign));
    assert!(alloc::foreign::unregister(foreign));
    assert!(!alloc::foreign::contains(foreign));
    assert!(free(ptr));
}

#[test]
fn pymem_set_and_get_allocator() {
    let _guard = PYMEM_OVERRIDE_LOCK.lock().unwrap();
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" {
        fn PyMem_Malloc(size: usize) -> *mut core::ffi::c_void;
        fn PyMem_SetAllocator(
            domain: python312::alloc::pymem::PyMemAllocatorDomain,
            allocator: *mut python312::alloc::pymem::PyMemAllocatorEx,
        );
        fn PyMem_GetAllocator(
            domain: python312::alloc::pymem::PyMemAllocatorDomain,
            allocator: *mut python312::alloc::pymem::PyMemAllocatorEx,
        );
    }
    unsafe extern "C" fn counting_malloc(
        _ctx: *mut core::ffi::c_void,
        size: usize,
    ) -> *mut core::ffi::c_void {
        CALLS.fetch_add(1, Ordering::Relaxed);
        python312::alloc::pymem::plain_malloc(size).cast()
    }
    unsafe extern "C" fn counting_free(_ctx: *mut core::ffi::c_void, p: *mut core::ffi::c_void) {
        // SAFETY: the pointer came from `plain_malloc` above.
        unsafe { python312::alloc::pymem::plain_free(p.cast()) };
    }
    let mut allocator = python312::alloc::pymem::PyMemAllocatorEx {
        ctx: core::ptr::null_mut(),
        malloc: Some(counting_malloc),
        calloc: None,
        realloc: None,
        free: Some(counting_free),
    };
    // SAFETY: C entry points driven with well-formed arguments.
    unsafe {
        PyMem_SetAllocator(python312::alloc::pymem::PYMEM_DOMAIN_MEM, &mut allocator);
        let routed = PyMem_Malloc(64);
        assert!(!routed.is_null());
        assert_eq!(CALLS.load(Ordering::Relaxed), 1, "override was not routed");
        let mut observed = python312::alloc::pymem::PyMemAllocatorEx {
            ctx: core::ptr::null_mut(),
            malloc: None,
            calloc: None,
            realloc: None,
            free: None,
        };
        PyMem_GetAllocator(python312::alloc::pymem::PYMEM_DOMAIN_MEM, &mut observed);
        let observed_malloc = observed.malloc.expect("installed override is visible");
        let via_observed = observed_malloc(core::ptr::null_mut(), 64);
        assert!(!via_observed.is_null());
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            2,
            "get returned the override"
        );
        // A null allocator resets the domain to the builtin.
        PyMem_SetAllocator(
            python312::alloc::pymem::PYMEM_DOMAIN_MEM,
            core::ptr::null_mut(),
        );
        let direct = PyMem_Malloc(64);
        assert!(!direct.is_null());
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            2,
            "reset did not restore the builtin"
        );
        python312::alloc::pymem::plain_free(routed.cast());
        python312::alloc::pymem::plain_free(via_observed.cast());
        python312::alloc::pymem::plain_free(direct.cast());
    }
}

/// Tests that assert exact placement of large runs (first fit over a
/// shared pool) run under this lock so concurrent large allocations from
/// other tests cannot split the runs between free and realloc.
static LARGE_RUN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A tiny xorshift so the stress test needs no crates.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn eight_threads_mixed_sizes_cross_thread_frees() {
    let _guard = LARGE_RUN_LOCK.lock().unwrap();
    const THREADS: usize = 8;
    const ITERS: usize = 30_000;
    let arenas: std::sync::Arc<_> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<(usize, usize, u64)>>::new()));
    {
        let mut guard = arenas.lock().unwrap();
        for _ in 0..THREADS {
            guard.push(Vec::new());
        }
    }
    let ready = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let arenas = std::sync::Arc::clone(&arenas);
        let ready = std::sync::Arc::clone(&ready);
        handles.push(std::thread::spawn(move || {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ ((t as u64 + 1) << 32));
            ready.wait();
            for i in 0..ITERS {
                // Mostly allocate, sometimes free another thread's block —
                // and our own — verifying contents as we go.
                let action = rng.next() % 4;
                if action != 0 {
                    let size = match rng.next() % 6 {
                        0 => 16,
                        1 => 100,
                        2 => 1_500,
                        3 => 30_000,
                        4 => (rng.next() % 32_000 + 1) as usize,
                        _ => 40_000 + (rng.next() % 200_000) as usize,
                    };
                    let stamp = (t as u64) << 48 | i as u64;
                    let ptr = alloc_object(size);
                    assert!(!ptr.is_null(), "out of memory in stress");
                    // SAFETY: `size` bytes belong to this allocation.
                    unsafe {
                        std::ptr::write_unaligned(ptr as *mut u64, stamp);
                        if size >= 16 {
                            std::ptr::write_unaligned(ptr.add(size - 8) as *mut u64, stamp);
                        }
                    }
                    arenas.lock().unwrap()[t].push((ptr as usize, size, stamp));
                } else {
                    let victim = (rng.next() as usize) % THREADS;
                    let entry = arenas.lock().unwrap()[victim].pop();
                    if let Some((addr, size, stamp)) = entry {
                        // SAFETY: the block is still allocated; the stamps
                        // must be intact if no block was handed out twice.
                        unsafe {
                            assert_eq!(
                                std::ptr::read_unaligned(addr as *const u64),
                                stamp,
                                "block corrupted"
                            );
                            if size >= 16 {
                                assert_eq!(
                                    std::ptr::read_unaligned((addr + size - 8) as *const u64),
                                    stamp,
                                    "block tail corrupted"
                                );
                            }
                        }
                        assert!(free(addr as *mut u8), "double free or foreign pointer");
                    }
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("stress thread panicked");
    }
    // Drain: every remaining block still holds its stamps.
    let mut guard = arenas.lock().unwrap();
    for arena in guard.iter_mut() {
        for (addr, size, stamp) in arena.drain(..) {
            // SAFETY: allocated and untouched since writing the stamps.
            unsafe {
                assert_eq!(std::ptr::read_unaligned(addr as *const u64), stamp);
                if size >= 16 {
                    assert_eq!(
                        std::ptr::read_unaligned((addr + size - 8) as *const u64),
                        stamp
                    );
                }
            }
            assert!(free(addr as *mut u8));
        }
    }
}

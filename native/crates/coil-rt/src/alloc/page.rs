//! The 64 KiB page: a header with side bitmaps indexed by 16-byte granule,
//! followed by the object area (spec section 9.1). Small pages hold one
//! size class; a large object's first page carries the run header instead.
//! Bitmap words are atomic because frees (which clear bits) run on other
//! threads than allocation; the surrounding free-list publication and the
//! collector's stop order the words themselves.

use core::sync::atomic::{AtomicIsize, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Pages are committed, and page headers found, in 64 KiB units.
pub const PAGE_SIZE: usize = 64 * 1024;

/// Bitmaps and object addresses advance in 16-byte granules.
pub const GRANULE: usize = 16;

/// The header and bitmaps occupy the first 8 KiB of every page.
pub const DATA_OFFSET: usize = 8192;

/// Bytes of object area per page.
pub const DATA_BYTES: usize = PAGE_SIZE - DATA_OFFSET;

/// Granules of object area per page; the bitmap length.
pub const GRANULES: usize = DATA_BYTES / GRANULE;

const BM_WORDS: usize = GRANULES / 64;

/// `PageHeader::magic` for every committed page.
pub const MAGIC: u32 = u32::from_le_bytes(*b"PAGE");

/// `SMALL` pages hold blocks of one size class; `LARGE` is the first page
/// of a page run holding one large object.
pub const KIND_SMALL: u8 = 0;
pub const KIND_LARGE: u8 = 1;

/// Page lifecycle: active while owned by a thread cache or holding live
/// blocks, idle (reusable for the same class) once fully free. Touched
/// only under the pool lock.
pub const STATE_ACTIVE: u8 = 0;
pub const STATE_IDLE: u8 = 1;

type Bitmap = [AtomicU64; BM_WORDS];

#[repr(C, align(64))]
pub struct PageHeader {
    pub magic: u32,
    pub kind: u8,
    /// Size class for small pages, unused for large.
    pub class: u8,
    /// Block size for small pages; 0 for large.
    pub block_size: u32,
    /// Page count of the run, large pages only.
    pub run_pages: u32,
    /// Number of blocks handed out and not yet freed (small pages).
    pub used: AtomicIsize,
    /// Owning thread cache id, 0 when none. Read on free paths without the
    /// pool lock.
    pub current_for: AtomicU32,
    /// Lock-free list of blocks freed by other threads; only the owning
    /// cache pops it.
    pub thread_free: AtomicPtr<u8>,
    /// `STATE_ACTIVE` or `STATE_IDLE`; written under the pool lock only.
    state: AtomicU8,
    pub allocated: Bitmap,
    pub is_object: Bitmap,
    pub mark: Bitmap,
    pub regime: Bitmap,
    pub c_created: Bitmap,
    pub escaped: Bitmap,
    pub counted_slots: Bitmap,
    pub has_prehdr: Bitmap,
    pub finalized: Bitmap,
    pub has_finalizer: Bitmap,
}

const _: () = assert!(core::mem::size_of::<PageHeader>() <= DATA_OFFSET);

/// The header of the page containing `ptr`. `ptr` must lie in a committed
/// GC-heap page.
pub fn header_of(ptr: *const u8) -> &'static PageHeader {
    // SAFETY: the caller guarantees a committed page, whose header was
    // initialised before the page became visible; the reference lives as
    // long as the segment, which is never released.
    unsafe { &*((ptr as usize & !(PAGE_SIZE - 1)) as *const PageHeader) }
}

impl PageHeader {
    /// The header of the page based at `base`, which must be page-aligned
    /// and committed.
    pub fn header_at(base: usize) -> &'static PageHeader {
        header_of(base as *const u8)
    }
}

macro_rules! bitmap_fns {
    ($get:ident, $set:ident, $clear:ident, $field:ident) => {
        pub fn $get(&self, granule: usize) -> bool {
            self.$field[granule >> 6].load(Ordering::Relaxed) >> (granule & 63) & 1 == 1
        }

        pub fn $set(&self, granule: usize) {
            self.$field[granule >> 6].fetch_or(1 << (granule & 63), Ordering::Relaxed);
        }

        pub fn $clear(&self, granule: usize) {
            self.$field[granule >> 6].fetch_and(!(1 << (granule & 63)), Ordering::Relaxed);
        }
    };
}

impl PageHeader {
    /// Prepare the freshly committed page at `base` to serve `class`.
    /// Only called under the pool lock, before the page is visible.
    ///
    /// # Safety
    ///
    /// `base` must be page-aligned, committed, and exclusively owned by
    /// the caller.
    pub unsafe fn init_small_at(base: *mut u8, class: usize) {
        let block_size = crate::alloc::size_class::class_size(class);
        let mut header = PageHeader::empty(KIND_SMALL);
        header.class = class as u8;
        header.block_size = block_size as u32;
        // Link every block into the thread-free list; the adopting cache
        // pops it and serves blocks from its local free list.
        let blocks = DATA_BYTES / block_size;
        let mut head: *mut u8 = core::ptr::null_mut();
        // SAFETY: the page is freshly committed and its whole object area
        // is writable; each block's first word holds the list link.
        unsafe {
            for i in 0..blocks {
                let block = base.add(DATA_OFFSET + i * block_size);
                (block as *mut *mut u8).write(head);
                head = block;
            }
        }
        header.thread_free = AtomicPtr::new(head);
        // SAFETY: guaranteed by the caller.
        unsafe { core::ptr::write(base as *mut PageHeader, header) };
    }

    /// Prepare the freshly committed page at `base` as the first page of
    /// a large-object run of `run_pages` pages. Only the first page gets a
    /// header: the object spans the rest of the run contiguously, so a
    /// header written in an interior page would be overwritten by it —
    /// interior pages resolve through the segment's run table instead.
    /// Pool lock only.
    ///
    /// # Safety
    ///
    /// `base` must be page-aligned, committed, and exclusively owned by
    /// the caller.
    pub unsafe fn init_large_at(base: *mut u8, run_pages: usize) {
        let mut header = PageHeader::empty(KIND_LARGE);
        header.run_pages = run_pages as u32;
        // SAFETY: the page was just committed and is exclusively ours; the
        // caller holds the pool lock.
        unsafe { core::ptr::write(base as *mut PageHeader, header) };
    }

    fn empty(kind: u8) -> PageHeader {
        PageHeader {
            magic: MAGIC,
            kind,
            class: 0,
            block_size: 0,
            run_pages: 0,
            used: AtomicIsize::new(0),
            current_for: AtomicU32::new(0),
            thread_free: AtomicPtr::new(core::ptr::null_mut()),
            state: AtomicU8::new(STATE_ACTIVE),
            allocated: zeroed_bitmap(),
            is_object: zeroed_bitmap(),
            mark: zeroed_bitmap(),
            regime: zeroed_bitmap(),
            c_created: zeroed_bitmap(),
            escaped: zeroed_bitmap(),
            counted_slots: zeroed_bitmap(),
            has_prehdr: zeroed_bitmap(),
            finalized: zeroed_bitmap(),
            has_finalizer: zeroed_bitmap(),
        }
    }

    pub fn is_small(&self) -> bool {
        self.kind == KIND_SMALL
    }

    /// Address of the page this header leads.
    pub fn base(&self) -> usize {
        self as *const PageHeader as usize & !(PAGE_SIZE - 1)
    }

    /// The page lifecycle state (pool lock for writes).
    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }

    pub fn set_state(&self, state: u8) {
        self.state.store(state, Ordering::Relaxed);
    }

    /// Granule index of `ptr` within this page's object area.
    pub fn granule_of(&self, page_base: usize, ptr: usize) -> usize {
        (ptr - page_base - DATA_OFFSET) / GRANULE
    }

    /// Address of granule `g` in this page.
    pub fn granule_addr(&self, page_base: usize, g: usize) -> usize {
        page_base + DATA_OFFSET + g * GRANULE
    }

    /// Start address of the block containing `ptr`, derived from the page's
    /// fixed block size. `ptr` must lie in this page's object area.
    pub fn block_start(&self, page_base: usize, ptr: usize) -> usize {
        let data = page_base + DATA_OFFSET;
        data + (ptr - data) / self.block_size() * self.block_size()
    }

    pub fn block_size(&self) -> usize {
        self.block_size as usize
    }

    /// Clear every per-object bit of granules `g0..g0 + granules` in all
    /// ten bitmaps, as `free_block` does for a freed block.
    pub fn clear_object_bits(&self, g0: usize, granules: usize) {
        for g in g0..g0 + granules {
            self.clear_allocated(g);
            self.clear_is_object(g);
            self.clear_mark(g);
            self.clear_regime(g);
            self.clear_c_created(g);
            self.clear_escaped(g);
            self.clear_counted_slots(g);
            self.clear_has_prehdr(g);
            self.clear_finalized(g);
            self.clear_has_finalizer(g);
        }
    }

    /// Mark the block's whole extent allocated and, when it starts an
    /// object, its first granule in `is_object`.
    pub fn set_allocated_block(&self, g0: usize, granules: usize, is_object: bool) {
        for g in g0..g0 + granules {
            self.set_allocated(g);
        }
        if is_object {
            self.set_is_object(g0);
        }
    }

    bitmap_fns!(allocated, set_allocated, clear_allocated, allocated);
    bitmap_fns!(is_object, set_is_object, clear_is_object, is_object);
    bitmap_fns!(mark, set_mark, clear_mark, mark);
    bitmap_fns!(regime, set_regime, clear_regime, regime);
    bitmap_fns!(c_created, set_c_created, clear_c_created, c_created);
    bitmap_fns!(escaped, set_escaped, clear_escaped, escaped);
    bitmap_fns!(
        counted_slots,
        set_counted_slots,
        clear_counted_slots,
        counted_slots
    );
    bitmap_fns!(has_prehdr, set_has_prehdr, clear_has_prehdr, has_prehdr);
    bitmap_fns!(finalized, set_finalized, clear_finalized, finalized);
    bitmap_fns!(
        has_finalizer,
        set_has_finalizer,
        clear_has_finalizer,
        has_finalizer
    );
}

fn zeroed_bitmap() -> Bitmap {
    [const { AtomicU64::new(0) }; BM_WORDS]
}

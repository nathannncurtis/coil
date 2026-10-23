//! The two-level page table: any address to its segment and page in a
//! couple of dependent loads, answering "is this address in the GC heap"
//! and giving the page header with the side bitmaps (spec section 9.1).

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use super::page::{PAGE_SIZE, PageHeader};

/// `run_head` value for a page that is not part of a large run. The head
/// page of a run stores 0; interior pages store their offset back to it.
const NO_RUN: u32 = u32::MAX;

/// A free page run in a segment, kept sorted by start page.
pub struct Run {
    pub start: u32,
    pub pages: u32,
}

/// One reserved segment: its address range, which pages are committed, and
/// which pages belong to a large run. Segments are registered once and live
/// for the process; `committed` grows only, so lookups are lock-free. The
/// mutable placement state (the unassigned tail and the free-run list) lives
/// in the pool, which owns every segment.
pub struct Segment {
    pub base: usize,
    pub page_count: u32,
    committed: Vec<AtomicU64>,
    run_head: Vec<AtomicU32>,
}

impl Segment {
    pub fn new(base: usize, page_count: u32) -> Segment {
        Segment {
            base,
            page_count,
            committed: (0..(page_count as usize).div_ceil(64))
                .map(|_| AtomicU64::new(0))
                .collect(),
            run_head: (0..page_count).map(|_| AtomicU32::new(NO_RUN)).collect(),
        }
    }

    fn is_committed(&self, page: u32) -> bool {
        self.committed[(page >> 6) as usize].load(Ordering::Acquire) >> (page & 63) & 1 == 1
    }

    /// Publish pages `[from, from + len)` as committed. Pool lock only;
    /// the Release store pairs with the Acquire loads of `lookup`.
    pub fn set_committed(&self, from: u32, len: u32) {
        for page in from..from + len {
            let word = &self.committed[(page >> 6) as usize];
            word.fetch_or(1 << (page & 63), Ordering::Release);
        }
    }

    /// Record pages `[from, from + len)` as a large run. Only the head page
    /// carries a header; an object spanning the run would overwrite any
    /// header in the interior pages, so the back-pointers live here. Pool
    /// lock only; the stores happen before `set_committed`, whose Release
    /// pairs with `lookup`'s Acquire, so a reader that sees the run
    /// committed also sees these entries.
    pub fn set_run(&self, from: u32, len: u32) {
        for page in from..from + len {
            self.run_head[page as usize].store(page - from, Ordering::Release);
        }
    }

    /// Mark a page as standing alone (not part of a large run) when it is
    /// handed to the small-object path. Pool lock only.
    pub fn clear_run(&self, page: u32) {
        self.run_head[page as usize].store(NO_RUN, Ordering::Release);
    }
}

const L2_BITS: u32 = 12;
const L1_LEN: usize = 1 << (48 - SEGMENT_SHIFT - L2_BITS);
const SEGMENT_SHIFT: u32 = 22; // log2(SEGMENT_SIZE), L2 key shift

struct Level2 {
    segments: [AtomicPtr<Segment>; 1 << L2_BITS],
}

static L1: [AtomicPtr<Level2>; L1_LEN] = [const { AtomicPtr::new(core::ptr::null_mut()) }; L1_LEN];

const MASK48: usize = (1usize << 48) - 1;

/// Register `segment` so every address it covers resolves through
/// `lookup`. Segments are never unregistered.
pub fn register(segment: &'static Segment) {
    let first = segment.base >> SEGMENT_SHIFT;
    let last = (segment.base + segment.page_count as usize * PAGE_SIZE - 1) >> SEGMENT_SHIFT;
    let mut idx = first;
    while idx <= last {
        let l1 = idx >> L2_BITS;
        let l2 = idx & ((1 << L2_BITS) - 1);
        let mut node = L1[l1].load(Ordering::Acquire);
        if node.is_null() {
            let fresh = Box::into_raw(Box::new(Level2 {
                segments: [const { AtomicPtr::new(core::ptr::null_mut()) }; 1 << L2_BITS],
            }));
            match L1[l1].compare_exchange(
                core::ptr::null_mut(),
                fresh,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => node = fresh,
                Err(existing) => {
                    // SAFETY: `fresh` was never published; only we can free it.
                    unsafe { drop(Box::from_raw(fresh)) };
                    node = existing;
                }
            }
        }
        // SAFETY: node lives forever once published.
        let node = unsafe { node.as_ref().unwrap() };
        node.segments[l2]
            .compare_exchange(
                core::ptr::null_mut(),
                segment as *const Segment as *mut Segment,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("segment slot already registered");
        idx += 1;
    }
}

/// The segment containing `addr`, or `None` when the address is outside
/// every reserved segment.
pub fn segment_of(addr: usize) -> Option<&'static Segment> {
    let addr = addr & MASK48;
    let l1 = addr >> (SEGMENT_SHIFT + L2_BITS);
    let node = L1[l1].load(Ordering::Acquire);
    if node.is_null() {
        return None;
    }
    let l2 = (addr >> SEGMENT_SHIFT) & ((1 << L2_BITS) - 1);
    // SAFETY: nodes live forever once published.
    let seg = unsafe { node.as_ref().unwrap().segments[l2].load(Ordering::Acquire) };
    if seg.is_null() {
        return None;
    }
    // SAFETY: segments live forever once registered.
    Some(unsafe { seg.as_ref().unwrap() })
}

/// The page header for `addr`, or `None` when `addr` is outside the GC
/// heap or its page is not committed. Interior pages of a large run have no
/// header of their own (the object runs over them), so they resolve to the
/// run head. The header carries the side bitmaps.
pub fn lookup(addr: usize) -> Option<&'static PageHeader> {
    let segment = segment_of(addr)?;
    let page = ((addr - segment.base) / PAGE_SIZE) as u32;
    if page >= segment.page_count || !segment.is_committed(page) {
        return None;
    }
    // Relaxed: the Acquire load of `committed` above already ordered this
    // read after the pool's Release stores, since `set_run` precedes
    // `set_committed`.
    let offset = segment.run_head[page as usize].load(Ordering::Relaxed);
    let head = if offset == NO_RUN {
        page
    } else {
        page - offset
    };
    Some(super::page::header_of(
        (segment.base + head as usize * PAGE_SIZE) as *const u8,
    ))
}

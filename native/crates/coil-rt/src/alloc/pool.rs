//! The global page pool: segment creation, small-page acquisition and
//! recycling, and large objects on page runs from a first-fit free list
//! (spec section 9.1). Everything here runs behind the pool lock; the
//! allocation fast path never takes it (section 8.6).

use core::sync::atomic::Ordering;

use std::sync::{Mutex, MutexGuard, OnceLock};

use super::page::{DATA_OFFSET, KIND_LARGE, PAGE_SIZE, PageHeader, STATE_ACTIVE, STATE_IDLE};
use super::page_table::{self, Run, Segment};
use super::size_class::NUM_CLASSES;
use super::sys::{self, SEGMENT_SIZE};

/// A segment's placement state, owned by the pool and mutated only under
/// its lock: the unassigned tail `[cursor, page_count)` backs new small
/// pages and large runs; `runs` holds free large runs, sorted by start.
struct SegEntry {
    segment: &'static Segment,
    cursor: u32,
    runs: Vec<Run>,
}

struct Pool {
    segments: Vec<SegEntry>,
    /// Fully free pages, reusable for their class only.
    idle: [Vec<usize>; NUM_CLASSES],
    /// Active pages with no owner: their live blocks keep them out of the
    /// idle list and their free blocks are parked on the page's
    /// thread-free list, waiting for a cache to adopt them.
    abandoned: [Vec<usize>; NUM_CLASSES],
}

static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();

fn pool() -> MutexGuard<'static, Pool> {
    POOL.get_or_init(|| {
        Mutex::new(Pool {
            segments: Vec::new(),
            idle: [(); NUM_CLASSES].map(|()| Vec::new()),
            abandoned: [(); NUM_CLASSES].map(|()| Vec::new()),
        })
    })
    .lock()
    .expect("page pool poisoned")
}

/// Reserve a segment of `page_count` pages (a 4 MiB multiple) and
/// register it. Caller holds the pool lock.
fn new_segment(pool: &mut Pool, page_count: u32) -> Option<&'static Segment> {
    let base = sys::reserve(page_count as usize * PAGE_SIZE);
    if base.is_null() {
        return None;
    }
    let segment: &'static Segment = Box::leak(Box::new(Segment::new(base as usize, page_count)));
    page_table::register(segment);
    pool.segments.push(SegEntry {
        segment,
        cursor: 0,
        runs: Vec::new(),
    });
    Some(segment)
}

/// Pages needed for a large object of `size` bytes.
fn large_pages(size: usize) -> u32 {
    (size + DATA_OFFSET).div_ceil(PAGE_SIZE) as u32
}

/// Take a page serving `class` for thread cache `owner`. On success the
/// page is active, owned by `owner`, and its free blocks are linked on
/// `thread_free`, ready for the cache to pop.
pub fn acquire_page(class: usize, owner: u32) -> Option<&'static PageHeader> {
    let mut pool = pool();
    if let Some(base) = pool.idle[class].pop() {
        let header = PageHeader::header_at(base);
        debug_assert_eq!(header.state(), STATE_IDLE);
        header.set_state(STATE_ACTIVE);
        header.current_for.store(owner, Ordering::Release);
        return Some(header);
    }
    let mut chosen: Option<(u32, &'static Segment)> = pool
        .segments
        .iter()
        .find(|entry| entry.cursor < entry.segment.page_count)
        .map(|entry| (entry.cursor, entry.segment));
    if chosen.is_none() {
        chosen = Some((
            0,
            new_segment(&mut pool, (SEGMENT_SIZE / PAGE_SIZE) as u32)?,
        ));
    }
    let (page, segment) = chosen.expect("just set");
    if let Some(entry) = pool
        .segments
        .iter_mut()
        .find(|entry| core::ptr::eq(entry.segment, segment))
    {
        entry.cursor = page + 1;
    }
    commit_small_page(segment, page, class, owner)
}

/// Commit page `page` of `segment` and prepare it for `class`. Pool lock
/// only.
fn commit_small_page(
    segment: &'static Segment,
    page: u32,
    class: usize,
    owner: u32,
) -> Option<&'static PageHeader> {
    let base = segment.base + page as usize * PAGE_SIZE;
    if !sys::commit(base as *mut u8, PAGE_SIZE) {
        return None;
    }
    // SAFETY: the page was just committed and is exclusively ours under
    // the pool lock; nobody else can reach it before it is published.
    unsafe { PageHeader::init_small_at(base as *mut u8, class) };
    let header = PageHeader::header_at(base);
    header.current_for.store(owner, Ordering::Release);
    segment.clear_run(page);
    segment.set_committed(page, 1);
    Some(header)
}

/// Move a fully free, unowned page back to the idle list. Called when a
/// free drops `used` to zero; rechecks the conditions under the lock and
/// delists the page if it was abandoned. `header` must be the page at
/// `base`.
pub fn recycle_page(header: &'static PageHeader, base: usize) {
    let mut pool = pool();
    if header.state() != STATE_ACTIVE
        || header.current_for.load(Ordering::Acquire) != 0
        || header.used.load(Ordering::Acquire) != 0
    {
        return;
    }
    header.set_state(STATE_IDLE);
    let class = header.class as usize;
    pool.abandoned[class].retain(|&listed| listed != base);
    pool.idle[class].push(base);
}

/// A page stops being owned; the caller has just stored `current_for` 0.
/// Fully free pages go idle for their class; pages with anything left
/// (live blocks, or free blocks parked on the thread-free list) join the
/// abandoned list so another thread's cache can adopt them instead of
/// growing the heap. `header` must be the page at `base`.
pub fn release_page(header: &'static PageHeader, base: usize) {
    let mut pool = pool();
    if header.state() != STATE_ACTIVE || header.current_for.load(Ordering::Acquire) != 0 {
        // Recycled or re-adopted while we were releasing; not ours to
        // place.
        return;
    }
    let class = header.class as usize;
    if header.used.load(Ordering::Acquire) == 0 {
        header.set_state(STATE_IDLE);
        pool.idle[class].push(base);
    } else {
        pool.abandoned[class].push(base);
    }
}

/// Take an abandoned page of `class` for `owner`: an active, unowned
/// page whose free blocks sit on its thread-free list, ready to be
/// drained. Pages with an empty thread-free list (nothing to serve yet)
/// stay listed for a later refill.
pub fn adopt_page(class: usize, owner: u32) -> Option<&'static PageHeader> {
    let mut pool = pool();
    let list = &mut pool.abandoned[class];
    let mut index = 0;
    while index < list.len() {
        let base = list[index];
        let header = PageHeader::header_at(base);
        if header.state() == STATE_ACTIVE
            && header.current_for.load(Ordering::Acquire) == 0
            && !header.thread_free.load(Ordering::Relaxed).is_null()
        {
            list.remove(index);
            header.current_for.store(owner, Ordering::Release);
            return Some(header);
        }
        index += 1;
    }
    None
}

/// Allocate a large object: a run of pages, first fit over the free-run
/// lists and unassigned tails of existing segments (in address order per
/// segment), else a fresh segment. The run is committed and zeroed;
/// `is_object` marks the first granule when requested.
pub fn alloc_large(size: usize, is_object: bool) -> *mut u8 {
    let pages = large_pages(size);
    let mut pool = pool();
    // (entry index, start page, taken from a free run or the tail)
    let mut chosen: Option<(usize, u32, bool)> = None;
    for (index, entry) in pool.segments.iter().enumerate() {
        if let Some(run) = entry.runs.iter().find(|run| run.pages >= pages) {
            chosen = Some((index, run.start, true));
            break;
        }
        if entry.segment.page_count - entry.cursor >= pages {
            chosen = Some((index, entry.cursor, false));
            break;
        }
    }
    let (index, start, from_run) = match chosen {
        Some(found) => found,
        None => {
            let pages = pages.max((SEGMENT_SIZE / PAGE_SIZE) as u32);
            match new_segment(&mut pool, pages) {
                Some(_) => (pool.segments.len() - 1, 0, false),
                None => return core::ptr::null_mut(),
            }
        }
    };
    let entry = &mut pool.segments[index];
    if from_run {
        let run_idx = entry
            .runs
            .iter()
            .position(|run| run.start == start)
            .expect("just found");
        let run = &entry.runs[run_idx];
        let remainder = run.pages - pages;
        if remainder == 0 {
            entry.runs.remove(run_idx);
        } else {
            entry.runs[run_idx] = Run {
                start: run.start + pages,
                pages: remainder,
            };
        }
    } else {
        entry.cursor = start + pages;
    }
    let segment = entry.segment;
    let base = segment.base + start as usize * PAGE_SIZE;
    if !sys::commit(base as *mut u8, pages as usize * PAGE_SIZE) {
        return core::ptr::null_mut();
    }
    // SAFETY: the run was just committed and is exclusively ours under
    // the pool lock.
    unsafe { PageHeader::init_large_at(base as *mut u8, pages as usize) };
    let header = PageHeader::header_at(base);
    segment.set_run(start, pages);
    segment.set_committed(start, pages);
    let data = base + DATA_OFFSET;
    // SAFETY: the object area of the freshly committed run is ours to
    // write.
    unsafe { core::ptr::write_bytes(data as *mut u8, 0, size) };
    if is_object {
        header.set_is_object(0);
    }
    data as *mut u8
}

/// Free the large object whose run starts at page base `base`. Returns
/// false when `base` is not a large-run page.
pub fn free_large(base: usize) -> bool {
    let header = PageHeader::header_at(base);
    if header.magic != super::page::MAGIC || header.kind != KIND_LARGE {
        return false;
    }
    let mut pool = pool();
    let Some(segment) = page_table::segment_of(base) else {
        return false;
    };
    let index = pool
        .segments
        .iter()
        .position(|entry| core::ptr::eq(entry.segment, segment))
        .expect("committed run has an owning segment");
    let entry = &mut pool.segments[index];
    let start = ((base - segment.base) / PAGE_SIZE) as u32;
    let pages = header.run_pages;
    // Drop the object markers so a freed run no longer looks live to heap
    // walks; a later allocation of the run sets them again.
    header.clear_is_object(0);
    header.clear_has_prehdr(0);
    if start + pages == entry.cursor {
        // Returning to the unassigned tail; absorb runs that now border it.
        entry.cursor = start;
        while entry
            .runs
            .last()
            .is_some_and(|run| run.start + run.pages == entry.cursor)
        {
            entry.cursor = entry.runs.pop().expect("just checked").start;
        }
    } else {
        let idx = entry.runs.partition_point(|run| run.start < start);
        let mut begin = start;
        let mut len = pages;
        if idx > 0 && entry.runs[idx - 1].start + entry.runs[idx - 1].pages == begin {
            begin = entry.runs[idx - 1].start;
            len += entry.runs[idx - 1].pages;
            entry.runs.remove(idx - 1);
        }
        let idx = entry.runs.partition_point(|run| run.start < begin);
        if entry
            .runs
            .get(idx)
            .is_some_and(|run| begin + len == run.start)
        {
            len += entry.runs.remove(idx).pages;
        }
        entry.runs.insert(
            idx,
            Run {
                start: begin,
                pages: len,
            },
        );
    }
    true
}

//! PEP 703-style per-object locks (spec section 8.6): a one-byte mutex
//! embedded in each mutable container, with the states and hand-off
//! protocol of CPython's `PyMutex`. The uncontended path is one CAS
//! 0→1; on contention the byte moves to LOCKED_PARKED and the loser
//! parks on the lock's address in a parking lot, so an unlocker can
//! wake exactly one waiter. Under the v1 GIL the lock is always
//! uncontended and costs the CAS; nothing in the containers assumes
//! that, which is what the free-threaded build (spec section 10.3)
//! changes nothing for.
//!
//! The lock is not re-entrant. `Guard` is the RAII form every runtime
//! container path uses; the raw `lock`/`unlock` pair is for future
//! `Py_BEGIN_CRITICAL_SECTION` mapping.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::thread::Thread;

/// The unlocked state.
pub const UNLOCKED: u8 = 0;
/// Locked with no parked waiters: the CAS fast path's value.
pub const LOCKED: u8 = 1;
/// Locked with at least one parked waiter: unlock must wake one.
pub const LOCKED_PARKED: u8 = 2;

struct Waiter {
    thread: Thread,
    woken: AtomicBool,
}

/// The parking lot: waiter queues keyed by the address of the lock byte,
/// CPython's `_PyParkingLot` shape. One global mutex guards the table;
/// queues are FIFO, so waiters are woken in enqueue order.
fn parking_lot() -> &'static Mutex<HashMap<usize, VecDeque<Arc<Waiter>>>> {
    static LOT: OnceLock<Mutex<HashMap<usize, VecDeque<Arc<Waiter>>>>> = OnceLock::new();
    LOT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Park the current thread until an unlock hands the lock over. True
/// means the hand-off happened: the caller owns the lock without
/// touching the byte. False means the lock was released before the
/// registration took: retry the fast path.
fn park(byte: &AtomicU8) -> bool {
    let waiter = Arc::new(Waiter {
        thread: std::thread::current(),
        woken: AtomicBool::new(false),
    });
    let mut lot = parking_lot().lock().expect("parking lot poisoned");
    lot.entry(byte as *const AtomicU8 as usize)
        .or_default()
        .push_back(waiter.clone());
    // An unlock hands the lock off while holding the table lock: it pops
    // this waiter and sets `woken` together, and only then (or by
    // releasing the byte under the same table lock) does the byte leave
    // LOCKED_PARKED — so the two checks below cannot both miss.
    if waiter.woken.load(Ordering::Acquire) {
        return true;
    }
    if byte.load(Ordering::Relaxed) != LOCKED_PARKED {
        remove_waiter(&mut lot, byte, &waiter);
        return false;
    }
    drop(lot);
    // `Thread::unpark` leaves a permit, so a grant between the checks
    // above and the park call still releases us immediately.
    while !waiter.woken.load(Ordering::Acquire) {
        std::thread::park();
    }
    true
}

/// Drop `waiter` from its queue, removing the queue when it empties.
fn remove_waiter(
    lot: &mut HashMap<usize, VecDeque<Arc<Waiter>>>,
    byte: &AtomicU8,
    waiter: &Arc<Waiter>,
) {
    let key = byte as *const AtomicU8 as usize;
    if let Some(queue) = lot.get_mut(&key) {
        queue.retain(|other| !Arc::ptr_eq(other, waiter));
        if queue.is_empty() {
            lot.remove(&key);
        }
    }
}

/// Acquire the lock. The fast path is a single CAS UNLOCKED→LOCKED; the
/// slow path moves the byte to LOCKED_PARKED and parks until an unlock
/// hands it over (a fresh contender may still barge in first — PEP 703
/// allows this).
pub fn lock(byte: &AtomicU8) {
    loop {
        if byte
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        // Advertise a waiter. If the lock was released between the failed
        // CAS and this swap, we now hold it (in LOCKED_PARKED).
        if byte.swap(LOCKED_PARKED, Ordering::AcqRel) == UNLOCKED {
            return;
        }
        // The wake is a hand-off: a true return means we own the lock.
        if park(byte) {
            return;
        }
    }
}

/// Release the lock. The plain LOCKED state releases with one store;
/// LOCKED_PARKED hands the lock directly to the first waiter (the byte
/// stays LOCKED_PARKED, so bargers queue rather than steal the hand-off
/// and no wake-up can be lost).
pub fn unlock(byte: &AtomicU8) {
    if byte
        .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
        .is_ok()
    {
        return;
    }
    let mut lot = parking_lot().lock().expect("parking lot poisoned");
    let Some(waiter) = lot
        .get_mut(&(byte as *const AtomicU8 as usize))
        .and_then(|queue| queue.pop_front())
    else {
        // The waiters all deregistered; release for real.
        let _ = byte.compare_exchange(
            LOCKED_PARKED,
            UNLOCKED,
            Ordering::Release,
            Ordering::Relaxed,
        );
        return;
    };
    drop(lot);
    grant(waiter);
}

/// Hand the lock to `waiter`: the flag is the grant, and the unpark's
/// permit covers the race with a waiter that has not parked yet.
fn grant(waiter: Arc<Waiter>) {
    waiter.woken.store(true, Ordering::Release);
    waiter.thread.unpark();
}

/// `PyMutex_TryLock`: acquire only if uncontended.
pub fn try_lock(byte: &AtomicU8) -> bool {
    byte.compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// The RAII guard the container paths hold: acquires on construction and
/// releases on drop, so no early return can leak a lock.
pub struct Guard {
    byte: *const AtomicU8,
}

impl Guard {
    /// Acquire `byte` and hold it until drop.
    pub fn new(byte: &AtomicU8) -> Guard {
        lock(byte);
        Guard {
            byte: byte as *const AtomicU8,
        }
    }

    /// The guarded lock byte, for nested helpers that must re-check state.
    pub fn byte(&self) -> &AtomicU8 {
        // SAFETY: the byte outlives the guard (the owning container does).
        unsafe { &*self.byte }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the byte outlives the guard (the owning container does).
        unlock(unsafe { &*self.byte });
    }
}

#[cfg(test)]
mod tests {
    use super::parking_lot;
    use super::{Guard, LOCKED, LOCKED_PARKED, UNLOCKED, lock, try_lock, unlock};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU8;
    use std::sync::atomic::AtomicUsize;
    use std::thread::ThreadId;

    /// The queued waiters' thread ids, in dequeue order: the order the
    /// hand-off must follow.
    fn parked_queue_for_test(byte: &AtomicU8) -> Vec<ThreadId> {
        let lot = parking_lot().lock().expect("parking lot poisoned");
        lot.get(&(byte as *const AtomicU8 as usize))
            .map(|queue| queue.iter().map(|waiter| waiter.thread.id()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn uncontended_lock_cycles_through_states() {
        let byte = AtomicU8::new(UNLOCKED);
        let g = Guard::new(&byte);
        assert_eq!(g.byte().load(std::sync::atomic::Ordering::Relaxed), LOCKED);
        drop(g);
        assert_eq!(byte.load(std::sync::atomic::Ordering::Relaxed), UNLOCKED);
        assert!(try_lock(&byte));
        assert!(!try_lock(&byte));
        unlock(&byte);
    }

    #[test]
    fn parked_waiters_are_woken_in_order() {
        let byte = Box::leak(Box::new(AtomicU8::new(UNLOCKED)));
        lock(byte);
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let order = Arc::clone(&order);
            let byte = &*byte;
            handles.push(std::thread::spawn(move || {
                lock(byte);
                order.lock().unwrap().push(std::thread::current().id());
                unlock(byte);
            }));
        }
        // Spawn order does not pin queue order (a thread signals nothing
        // before it holds the lot mutex), so the queue itself — read once
        // every contender has parked — is the expected grant order.
        let queued = loop {
            let snapshot = parked_queue_for_test(byte);
            if snapshot.len() == 4 {
                break snapshot;
            }
            std::thread::yield_now();
        };
        assert_eq!(
            byte.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_PARKED
        );
        unlock(byte);
        for handle in handles {
            handle.join().expect("waiter thread panicked");
        }
        assert_eq!(*order.lock().unwrap(), queued);
        assert_eq!(byte.load(std::sync::atomic::Ordering::Relaxed), UNLOCKED);
    }

    #[test]
    fn concurrent_guards_make_counts_exact() {
        // A leaked byte gives the threads a plain shared reference with
        // 'static lifetime, no raw pointers.
        let byte = Box::leak(Box::new(AtomicU8::new(UNLOCKED)));
        let counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let counter = Arc::clone(&counter);
            let byte = &*byte;
            handles.push(std::thread::spawn(move || {
                let guard = Guard::new(byte);
                for _ in 0..1000 {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                drop(guard);
            }));
        }
        for handle in handles {
            handle.join().expect("hammering thread panicked");
        }
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 8000);
        assert_eq!(byte.load(std::sync::atomic::Ordering::Relaxed), UNLOCKED);
    }
}

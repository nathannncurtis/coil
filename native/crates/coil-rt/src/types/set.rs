//! `set` and `frozenset`: CPython's prefix layout (`fill`/`used` at
//! 16/24, so `PySet_GET_SIZE` is inline) with the table mechanics of
//! CPython 3.12 — open addressing with nine linear probes, dummy slots
//! on discard, and `set_table_resize` scanning the old table from
//! index 0, so iteration order matches CPython for the same
//! add/discard sequence (spec sections 8.2 and 8.5). The CPython
//! struct is kept whole and the per-object lock of section 8.6
//! follows it at offset 200; `tp_basicsize` (208) includes it.
//!
//! The set/frozenset type checks are subtype checks against the two
//! type objects (CPython has no fast-subclass flag for sets), and the
//! `dummy` deletion marker is a never-counted static object compared
//! by address.

use core::ffi::c_int;

use std::sync::atomic::AtomicU8;

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, PyObject, PyTypeObject,
};

use crate::object::header;
use crate::object::lock;
use crate::object::typeobject::{PyType_GenericAlloc, PyType_IsSubtype, zero_type};
use crate::types::exc as exceptions;
use crate::types::{object_hash, objects_equal};

/// The offset of the per-object lock within a set.
pub const LOCK: usize = 200;

/// CPython's `PySet_MINSIZE`.
const MINSIZE: usize = 8;
/// `LINEAR_PROBES` (setobject.c).
const LINEAR_PROBES: usize = 9;
/// `PERTURB_SHIFT` (setobject.c).
const PERTURB_SHIFT: u32 = 5;

/// CPython's `setentry`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SetEntry {
    key: *mut PyObject,
    hash: isize,
}

/// CPython's `PySetObject` with the lock appended: the header, the
/// prefix fields, the table state, the inline small table, and the
/// weakref slot Coil leaves in place.
#[repr(C)]
pub struct PySetObject {
    pub ob_refcnt: isize,
    pub ob_type: *mut PyTypeObject,
    pub fill: isize,
    pub used: isize,
    pub mask: isize,
    pub table: *mut SetEntry,
    pub hash: isize,
    pub finger: isize,
    pub smalltable: [SetEntry; MINSIZE],
    pub weakreflist: *mut PyObject,
    pub ob_lock: AtomicU8,
    pub _pad: [u8; 7],
}

/// The deletion marker (`dummy`): a static object never refcounted,
/// never freed, and compared by address.
static mut DUMMY_MARKER: PyObject = PyObject {
    ob_refcnt: 1,
    ob_type: core::ptr::null_mut(),
};

fn dummy() -> *mut PyObject {
    &raw mut DUMMY_MARKER
}

/// `PyFrozenSet_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_frozenset(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: both type objects are readied by init.
    unsafe {
        core::ptr::eq(ty, &raw mut PyFrozenSet_Type)
            || PyType_IsSubtype(ty, &raw mut PyFrozenSet_Type) != 0
    }
}

/// `PySet_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_set(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: both type objects are readied by init.
    unsafe {
        core::ptr::eq(ty, &raw mut PySet_Type) || PyType_IsSubtype(ty, &raw mut PySet_Type) != 0
    }
}

/// `PyAnySet_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_anyset(op: *const PyObject) -> bool {
    // SAFETY: each check reads only the object header.
    unsafe { is_set(op) || is_frozenset(op) }
}

unsafe fn lock_of(op: *mut PyObject) -> &'static AtomicU8 {
    // SAFETY: the lock is a fixed field of the set layout.
    unsafe { &*((op as *const u8).add(LOCK) as *const AtomicU8) }
}

/// The lock guard every mutating entry point holds.
///
/// # Safety
///
/// `op` must be a set object.
unsafe fn locked(op: *mut PyObject) -> lock::Guard {
    // SAFETY: the lock field is within the set.
    unsafe { lock::Guard::new(lock_of(op)) }
}

/// `set_lookkey`: the entry slot for `key`, or null with the exception
/// set when a comparison fails. An empty slot (hash 0, key null) is
/// returned as the "not found" answer.
///
/// # Safety
///
/// `so` must be a set object; `key` must be a valid object.
unsafe fn lookkey(so: *mut PySetObject, key: *mut PyObject, hash: isize) -> *mut SetEntry {
    // SAFETY: the probe stays within the table's allocation.
    unsafe {
        let mut mask = (*so).mask as usize;
        let mut i = hash as usize & mask;
        let mut perturb = hash as usize;
        loop {
            let mut entry = (*so).table.add(i);
            let mut probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            loop {
                if (*entry).hash == 0 && (*entry).key.is_null() {
                    return entry;
                }
                if (*entry).hash == hash {
                    if (*entry).key == key {
                        return entry;
                    }
                    // The comparisons this runtime has are pure (no
                    // Python reentry), so CPython's restart-on-mutation
                    // dance cannot trigger here.
                    match objects_equal((*entry).key, key) {
                        Ok(true) => return entry,
                        Ok(false) => mask = (*so).mask as usize,
                        Err(()) => return core::ptr::null_mut(),
                    }
                }
                // `} while (probes--)`: one pass, plus the linear run.
                if probes == 0 {
                    break;
                }
                probes -= 1;
                entry = entry.add(1);
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb) & mask;
        }
    }
}

/// `set_insert_clean`: place `key` in a table known to lack it.
///
/// # Safety
///
/// `table` has `mask + 1` slots.
unsafe fn insert_clean(table: *mut SetEntry, mask: usize, key: *mut PyObject, hash: isize) {
    // SAFETY: the probe finds a null slot within the table.
    unsafe {
        let mut i = hash as usize & mask;
        let mut perturb = hash as usize;
        loop {
            let entry = table.add(i);
            if (*entry).key.is_null() {
                (*entry).key = key;
                (*entry).hash = hash;
                return;
            }
            if i + LINEAR_PROBES <= mask {
                let mut probe = table.add(i + 1);
                for _ in 0..LINEAR_PROBES {
                    if (*probe).key.is_null() {
                        (*probe).key = key;
                        (*probe).hash = hash;
                        return;
                    }
                    probe = probe.add(1);
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb) & mask;
        }
    }
}

/// `set_table_resize` (the caller holds the lock): the smallest table
/// above `minused`, rebuilt by scanning the old table from index 0.
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn table_resize(so: *mut PySetObject, minused: isize) -> bool {
    // SAFETY: the scan covers exactly the old table.
    unsafe {
        let mut newsize: usize = MINSIZE;
        while newsize <= minused as usize {
            newsize <<= 1;
        }
        let oldtable = (*so).table;
        let oldmask = (*so).mask;
        let oldtable_malloced = !core::ptr::eq(oldtable, (*so).smalltable.as_mut_ptr());
        let mut small_copy: [SetEntry; MINSIZE] = zeroed_entries();

        let newtable: *mut SetEntry;
        if newsize == MINSIZE {
            newtable = (*so).smalltable.as_mut_ptr();
            if core::ptr::eq(newtable, oldtable) {
                if (*so).fill == (*so).used {
                    return true;
                }
                small_copy = *oldtable.cast::<[SetEntry; MINSIZE]>();
            }
        } else {
            let block =
                crate::alloc::pymem::plain_calloc(newsize, core::mem::size_of::<SetEntry>());
            if block.is_null() {
                crate::tstate::set_string(exceptions::memory_error(), b"");
                return false;
            }
            newtable = block.cast();
        }

        // Rebuild from the copy of a shrinking small table, or from the
        // old table itself.
        let source: *mut SetEntry = if core::ptr::eq(oldtable, newtable) {
            small_copy.as_mut_ptr()
        } else {
            oldtable
        };
        core::ptr::write_bytes(
            newtable.cast::<u8>(),
            0,
            newsize * core::mem::size_of::<SetEntry>(),
        );
        (*so).mask = (newsize - 1) as isize;
        (*so).table = newtable;
        if (*so).fill == (*so).used {
            for i in 0..=oldmask as usize {
                let entry = source.add(i);
                if !(*entry).key.is_null() {
                    insert_clean(newtable, newsize - 1, (*entry).key, (*entry).hash);
                }
            }
        } else {
            (*so).fill = (*so).used;
            for i in 0..=oldmask as usize {
                let entry = source.add(i);
                if !(*entry).key.is_null() && !core::ptr::eq((*entry).key, dummy()) {
                    insert_clean(newtable, newsize - 1, (*entry).key, (*entry).hash);
                }
            }
        }
        if oldtable_malloced {
            crate::alloc::pymem::plain_free(oldtable.cast());
        }
    }
    true
}

const fn zeroed_entries() -> [SetEntry; MINSIZE] {
    [SetEntry {
        key: core::ptr::null_mut(),
        hash: 0,
    }; MINSIZE]
}

/// The outcome of `set_add_entry`'s probe.
enum Found {
    Active,
    Unused(*mut SetEntry),
    /// The free dummy slot to reuse.
    Dummy(*mut SetEntry),
}

/// The probe of `set_add_entry`: the first usable slot, the first
/// dummy seen, or a match.
///
/// # Safety
///
/// `so` must be a set object; `key` must be a valid object.
unsafe fn add_probe(so: *mut PySetObject, key: *mut PyObject, hash: isize) -> Result<Found, ()> {
    // SAFETY: the probe stays within the table's allocation.
    unsafe {
        let mut mask = (*so).mask as usize;
        let mut i = hash as usize & mask;
        let mut freeslot: *mut SetEntry = core::ptr::null_mut();
        let mut perturb = hash as usize;
        loop {
            let mut entry = (*so).table.add(i);
            let mut probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            loop {
                if (*entry).hash == 0 && (*entry).key.is_null() {
                    if freeslot.is_null() {
                        return Ok(Found::Unused(entry));
                    }
                    return Ok(Found::Dummy(freeslot));
                }
                if (*entry).hash == hash {
                    if (*entry).key == key {
                        return Ok(Found::Active);
                    }
                    // Pure comparisons: the restart path cannot trigger.
                    match objects_equal((*entry).key, key) {
                        Ok(true) => return Ok(Found::Active),
                        Ok(false) => mask = (*so).mask as usize,
                        Err(()) => return Err(()),
                    }
                } else if (*entry).hash == -1 {
                    freeslot = entry;
                }
                if probes == 0 {
                    break;
                }
                probes -= 1;
                entry = entry.add(1);
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb) & mask;
        }
    }
}

/// `set_add_entry` (the caller holds the lock).
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn add_entry(so: *mut PySetObject, key: *mut PyObject, hash: isize) -> bool {
    // SAFETY: the probe reads only the table.
    match unsafe { add_probe(so, key, hash) } {
        Ok(Found::Active) => true,
        Err(()) => false,
        Ok(Found::Unused(entry)) => {
            // SAFETY: the entry and field writes are within the table.
            unsafe {
                (*entry).key = key;
                (*entry).hash = hash;
                (*so).fill += 1;
                (*so).used += 1;
                if ((*so).fill as usize * 5) < (*so).mask as usize * 3 {
                    return true;
                }
                let minused = if (*so).used > 50000 {
                    (*so).used * 2
                } else {
                    (*so).used * 4
                };
                table_resize(so, minused)
            }
        }
        Ok(Found::Dummy(freeslot)) => {
            // SAFETY: the freeslot came from the table.
            unsafe {
                (*freeslot).key = key;
                (*freeslot).hash = hash;
                (*so).used += 1;
                true
            }
        }
    }
}

/// `set_add_key`.
///
/// # Safety
///
/// The caller holds the set's lock; `key` must be a valid object.
unsafe fn add_key(so: *mut PySetObject, key: *mut PyObject) -> bool {
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    hash != -1 && unsafe { add_entry(so, key, hash) }
}

/// `set_contains_key`: -1/0/1.
///
/// # Safety
///
/// `so` must be a set object; `key` must be a valid object.
unsafe fn contains_key(so: *mut PySetObject, key: *mut PyObject) -> c_int {
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return -1;
    }
    // SAFETY: the lookup reads only the table.
    let entry = unsafe { lookkey(so, key, hash) };
    if entry.is_null() {
        return -1;
    }
    // SAFETY: the entry pointer came from the table probe.
    unsafe { (!(*entry).key.is_null()) as c_int }
}

/// `set_discard_entry` (the caller holds the lock): 0 (absent) or 1
/// (removed), -1 on error.
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn discard_entry(so: *mut PySetObject, key: *mut PyObject, hash: isize) -> c_int {
    // SAFETY: the lookup reads only the table.
    let entry = unsafe { lookkey(so, key, hash) };
    if entry.is_null() {
        return -1;
    }
    // SAFETY: the entry write is within the table.
    unsafe {
        if (*entry).key.is_null() {
            return 0;
        }
        (*entry).key = dummy();
        (*entry).hash = -1;
        (*so).used -= 1;
    }
    1
}

/// `set_discard_key`.
///
/// # Safety
///
/// The caller holds the set's lock; `key` must be a valid object.
unsafe fn discard_key(so: *mut PySetObject, key: *mut PyObject) -> c_int {
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return -1;
    }
    // SAFETY: the discard reads and writes only the table.
    unsafe { discard_entry(so, key, hash) }
}

/// `set_pop` (the caller holds the lock): the next element from the
/// search finger, or null with `KeyError` set.
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn pop(so: *mut PySetObject) -> *mut PyObject {
    // SAFETY: the scan wraps within the table.
    unsafe {
        if (*so).used == 0 {
            crate::tstate::set_string(exceptions::key_error(), b"pop from an empty set");
            return core::ptr::null_mut();
        }
        let mask = (*so).mask;
        let mut entry = (*so).table.add(((*so).finger as usize) & (mask as usize));
        let limit = (*so).table.add(mask as usize);
        while (*entry).key.is_null() || core::ptr::eq((*entry).key, dummy()) {
            entry = entry.add(1);
            if entry > limit {
                entry = (*so).table;
            }
        }
        let key = (*entry).key;
        (*entry).key = dummy();
        (*entry).hash = -1;
        (*so).used -= 1;
        (*so).finger = entry.offset_from((*so).table) + 1;
        key
    }
}

/// `set_empty_to_minsize` (the caller holds the lock).
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn empty_to_minsize(so: *mut PySetObject) {
    // SAFETY: the small table is part of the set allocation.
    unsafe {
        (*so).smalltable = zeroed_entries();
        (*so).fill = 0;
        (*so).used = 0;
        (*so).mask = (MINSIZE - 1) as isize;
        (*so).table = (*so).smalltable.as_mut_ptr();
        (*so).hash = -1;
    }
}

/// `set_clear_internal` (the caller holds the lock).
///
/// # Safety
///
/// The caller holds the set's lock.
unsafe fn clear_internal(so: *mut PySetObject) -> c_int {
    // SAFETY: after the swap the old table is exclusively ours.
    unsafe {
        let table = (*so).table;
        let fill = (*so).fill;
        let used = (*so).used;
        let table_malloced = !core::ptr::eq(table, (*so).smalltable.as_mut_ptr());
        let mut small_copy: [SetEntry; MINSIZE] = zeroed_entries();
        if table_malloced {
            empty_to_minsize(so);
        } else if fill > 0 {
            small_copy = *table.cast::<[SetEntry; MINSIZE]>();
            empty_to_minsize(so);
        }
        let source: *mut SetEntry = if table_malloced {
            table
        } else {
            small_copy.as_mut_ptr()
        };
        let mut remaining = used;
        let mut entry = source;
        while remaining > 0 {
            if !(*entry).key.is_null() && !core::ptr::eq((*entry).key, dummy()) {
                remaining -= 1;
            }
            entry = entry.add(1);
        }
        if table_malloced {
            crate::alloc::pymem::plain_free(table.cast());
        }
    }
    0
}

/// `set_next`: walk the live entries, skipping nulls and dummies.
///
/// # Safety
///
/// `so` must be a set object; `pos` must point to a cursor the caller
/// owns.
unsafe fn set_next(so: *mut PySetObject, pos: *mut isize) -> Option<*mut SetEntry> {
    // SAFETY: the scan covers exactly the table.
    unsafe {
        let mask = (*so).mask;
        let mut i = *pos;
        let mut entry = (*so).table.add(i.max(0) as usize);
        while i <= mask && ((*entry).key.is_null() || core::ptr::eq((*entry).key, dummy())) {
            i += 1;
            if i <= mask {
                entry = entry.add(1);
            }
        }
        *pos = i + 1;
        if i > mask {
            return None;
        }
        Some(entry)
    }
}

/// `_shuffle_bits` (setobject.c), in wrapping arithmetic.
fn shuffle_bits(h: u64) -> u64 {
    (h ^ 89869747 ^ (h << 16)).wrapping_mul(3644798167)
}

/// `frozenset_hash`: xor of shuffled bits over every table slot
/// (including nulls and dummies, whose contributions are removed
/// afterwards), so the hash depends on the table state exactly as
/// CPython's does.
///
/// # Safety
///
/// `so` must be a frozenset object.
unsafe extern "C" fn frozenset_hash(object: *mut PyObject) -> isize {
    let so = object.cast::<PySetObject>();
    // SAFETY: the table scan covers exactly the table.
    unsafe {
        if (*so).hash != -1 {
            return (*so).hash;
        }
        let mut hash: u64 = 0;
        let mut entry = (*so).table;
        let mask = (*so).mask as usize;
        for _ in 0..=mask {
            hash ^= shuffle_bits((*entry).hash as u64);
            entry = entry.add(1);
        }
        // Remove the effect of an odd number of null/dummy entries.
        if (((*so).mask + 1 - (*so).fill) & 1) != 0 {
            hash ^= shuffle_bits(0);
        }
        if (((*so).fill - (*so).used) & 1) != 0 {
            hash ^= shuffle_bits(u64::MAX);
        }
        hash ^= ((*so).used as u64 + 1).wrapping_mul(1927868237);
        hash ^= (hash >> 11) ^ (hash >> 25);
        hash = hash.wrapping_mul(69069).wrapping_add(907133923);
        if hash == u64::MAX {
            hash = 590923713;
        }
        (*so).hash = hash as isize;
        (*so).hash
    }
}

unsafe extern "C" fn set_dealloc(object: *mut PyObject) {
    let so = object.cast::<PySetObject>();
    // SAFETY: the scan covers exactly the table; the object came from
    // `PyType_GenericAlloc`.
    unsafe {
        let table = (*so).table;
        let mut used = (*so).used;
        let mut entry = table;
        while used > 0 {
            if !(*entry).key.is_null() && !core::ptr::eq((*entry).key, dummy()) {
                used -= 1;
            }
            entry = entry.add(1);
        }
        if !core::ptr::eq(table, (*so).smalltable.as_mut_ptr()) {
            crate::alloc::pymem::plain_free(table.cast());
        }
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn set_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> c_int {
    let so = object.cast::<PySetObject>();
    // SAFETY: the scan covers exactly the table.
    unsafe {
        let mut pos: isize = 0;
        while let Some(entry) = set_next(so, &raw mut pos) {
            if visit((*entry).key, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn set_iter(so: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_anyset(so) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the type is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(&raw mut SET_ITER_TYPE, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let si = object.cast::<SetIterObject>();
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*si).si_set = so;
        (*si).si_used = (*so.cast::<PySetObject>()).used;
        (*si).si_pos = 0;
    }
    object
}

const fn set_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"set".as_ptr();
    t.tp_basicsize = (coil_rt_abi::cp312::set::SIZEOF + 8) as isize;
    t.tp_dealloc = Some(set_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
    t.tp_traverse = Some(set_traverse);
    t.tp_iter = Some(set_iter);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn frozenset_type() -> PyTypeObject {
    let mut t = set_type();
    t.tp_name = c"frozenset".as_ptr();
    t.tp_hash = Some(frozenset_hash);
    t
}

/// The `set` type object (CPython's `PySet_Type`).
#[unsafe(no_mangle)]
pub static mut PySet_Type: PyTypeObject = set_type();

/// The `frozenset` type object (CPython's `PyFrozenSet_Type`).
#[unsafe(no_mangle)]
pub static mut PyFrozenSet_Type: PyTypeObject = frozenset_type();

/// CPython's `setiterobject` (a free-layout object): the set, the size
/// it started at, and the cursor. (`si_len` only feeds
/// `__length_hint__`, which has no protocol yet.)
#[repr(C)]
struct SetIterObject {
    ob_base: PyObject,
    si_set: *mut PyObject,
    si_used: isize,
    si_pos: isize,
}

unsafe extern "C" fn set_iter_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    let si = object.cast::<SetIterObject>();
    // SAFETY: the field read is within the iterator's allocation.
    unsafe {
        let set = (*si).si_set;
        if !set.is_null() && visit(set, arg) != 0 {
            return -1;
        }
    }
    0
}

unsafe extern "C" fn set_iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

/// `setiter_iternext`: stop when the set's size changed.
unsafe extern "C" fn set_iter_iternext(object: *mut PyObject) -> *mut PyObject {
    let si = object.cast::<SetIterObject>();
    // SAFETY: the iterator fields are within the allocation.
    unsafe {
        let so = (*si).si_set;
        if so.is_null() {
            return core::ptr::null_mut();
        }
        let set = so.cast::<PySetObject>();
        if (*si).si_used != (*set).used {
            (*si).si_used = -1;
            crate::tstate::set_string(
                exceptions::runtime_error(),
                b"Set changed size during iteration",
            );
            return core::ptr::null_mut();
        }
        let mask = (*set).mask;
        let table = (*set).table;
        let mut i = (*si).si_pos;
        while i <= mask
            && ((*table.add(i as usize)).key.is_null()
                || core::ptr::eq((*table.add(i as usize)).key, dummy()))
        {
            i += 1;
        }
        (*si).si_pos = i + 1;
        if i > mask {
            (*si).si_set = core::ptr::null_mut();
            return core::ptr::null_mut();
        }
        (*table.add(i as usize)).key
    }
}

const fn set_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"set_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<SetIterObject>() as isize;
    t.tp_dealloc = Some(set_iter_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(set_iter_traverse);
    t.tp_iter = Some(crate::types::tuple::self_iter);
    t.tp_iternext = Some(set_iter_iternext);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `set_iterator` type (CPython's `PySetIter_Type`).
static mut SET_ITER_TYPE: PyTypeObject = set_iter_type();

/// The set iterator type, for init.
pub(crate) fn set_iter_type_ptr() -> *mut PyTypeObject {
    &raw mut SET_ITER_TYPE
}

/// `make_new_set`: a fresh `type_` set, optionally filled from
/// `iterable`.
///
/// # Safety
///
/// `type_` must be `PySet_Type` or `PyFrozenSet_Type`; `iterable` must
/// be null or a valid iterable object.
unsafe fn make_new_set(type_: *mut PyTypeObject, iterable: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(type_, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let so = object.cast::<PySetObject>();
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*so).fill = 0;
        (*so).used = 0;
        (*so).mask = (MINSIZE - 1) as isize;
        (*so).table = (*so).smalltable.as_mut_ptr();
        (*so).hash = -1;
        (*so).finger = 0;
        (*so).weakreflist = core::ptr::null_mut();
        if !iterable.is_null() && update_internal(so, iterable).is_err() {
            if !core::ptr::eq((*so).table, (*so).smalltable.as_mut_ptr()) {
                crate::alloc::pymem::plain_free((*so).table.cast());
            }
            crate::alloc::free_block(object.cast());
            return core::ptr::null_mut();
        }
    }
    object
}

/// `set_update_internal`: the set fast path (`set_merge`), the dict
/// fast path (the dict's keys), then any iterable. The attribute
/// protocol does not exist yet, so CPython's mapping branch is named
/// as a gap by the iterable branch's own error.
///
/// # Safety
///
/// `so` must be a set object; `other` must be a valid object.
unsafe fn update_internal(so: *mut PySetObject, other: *mut PyObject) -> Result<(), ()> {
    // SAFETY: the checks read only headers.
    unsafe {
        if is_anyset(other) {
            return merge(so, other.cast::<PySetObject>());
        }
        if crate::types::dict::is_dict(other) {
            let dictsize = (*other.cast::<crate::types::dict::PyDictObject>()).ma_used;
            if ((*so).fill + dictsize) as usize * 5 >= (*so).mask as usize * 3
                && !table_resize(so, ((*so).used + dictsize) * 2)
            {
                return Err(());
            }
            let mut pos: isize = 0;
            let mut key: *mut PyObject = core::ptr::null_mut();
            let mut value: *mut PyObject = core::ptr::null_mut();
            while crate::types::dict::PyDict_Next(other, &raw mut pos, &raw mut key, &raw mut value)
                != 0
            {
                if !add_key(so, key) {
                    return Err(());
                }
            }
            return Ok(());
        }
        for key in crate::types::list::collect_iterable(other)? {
            if !add_key(so, key) {
                return Err(());
            }
        }
        Ok(())
    }
}

/// `set_merge` (the caller holds the lock on `so` only; `other` is
/// read without mutating).
///
/// # Safety
///
/// Both must be set objects.
unsafe fn merge(so: *mut PySetObject, other: *mut PySetObject) -> Result<(), ()> {
    // SAFETY: the reads of `other` and writes to `so` stay within the
    // two tables.
    unsafe {
        if core::ptr::eq(other as *mut PyObject, so as *mut PyObject) || (*other).used == 0 {
            return Ok(());
        }
        if ((*so).fill + (*other).used) as usize * 5 >= (*so).mask as usize * 3
            && !table_resize(so, ((*so).used + (*other).used) * 2)
        {
            return Err(());
        }
        // An empty table of the same size with no dummies copies
        // wholesale.
        if (*so).fill == 0 && (*so).mask == (*other).mask && (*other).fill == (*other).used {
            for i in 0..=(*other).mask as usize {
                let other_entry = (*other).table.add(i);
                if !(*other_entry).key.is_null() {
                    let so_entry = (*so).table.add(i);
                    (*so_entry).key = (*other_entry).key;
                    (*so_entry).hash = (*other_entry).hash;
                }
            }
            (*so).fill = (*other).fill;
            (*so).used = (*other).used;
            return Ok(());
        }
        if (*so).fill == 0 {
            (*so).fill = (*other).used;
            (*so).used = (*other).used;
            for i in 0..=(*other).mask as usize {
                let other_entry = (*other).table.add(i);
                if !(*other_entry).key.is_null() && !core::ptr::eq((*other_entry).key, dummy()) {
                    insert_clean(
                        (*so).table,
                        (*so).mask as usize,
                        (*other_entry).key,
                        (*other_entry).hash,
                    );
                }
            }
            return Ok(());
        }
        for i in 0..=(*other).mask as usize {
            let other_entry = (*other).table.add(i);
            if !(*other_entry).key.is_null()
                && !core::ptr::eq((*other_entry).key, dummy())
                && !add_entry(so, (*other_entry).key, (*other_entry).hash)
            {
                return Err(());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------- C API entry

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `iterable` must be null or a valid iterable object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_New(iterable: *mut PyObject) -> *mut PyObject {
    // SAFETY: `PySet_Type` is readied by init.
    unsafe { make_new_set(&raw mut PySet_Type, iterable) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `iterable` must be null or a valid iterable object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrozenSet_New(iterable: *mut PyObject) -> *mut PyObject {
    // SAFETY: `PyFrozenSet_Type` is readied by init.
    unsafe { make_new_set(&raw mut PyFrozenSet_Type, iterable) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `anyset` must be a set or frozenset object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Size(anyset: *mut PyObject) -> isize {
    if !(unsafe { is_anyset(anyset) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the prefix field is within the set.
    unsafe { (*anyset.cast::<PySetObject>()).used }
}

/// `PySet_Add`: allowed on a set, or on a frozenset still exclusively
/// owned (`Py_REFCNT == 1`).
///
/// # Safety
///
/// `anyset` must be a valid object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Add(anyset: *mut PyObject, key: *mut PyObject) -> c_int {
    // SAFETY: the checks read only headers and the refcount.
    unsafe {
        if !is_set(anyset) && (!is_frozenset(anyset) || header::refcnt(anyset) != 1) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        let _guard = locked(anyset);
        if add_key(anyset.cast::<PySetObject>(), key) {
            return 0;
        }
        -1
    }
}

/// See the CPython 3.12 contract for this entry point: 0 (absent) or
/// 1 (present).
///
/// # Safety
///
/// `anyset` must be a set or frozenset object; `key` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Contains(anyset: *mut PyObject, key: *mut PyObject) -> c_int {
    if !(unsafe { is_anyset(anyset) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the set and key are valid.
    unsafe { contains_key(anyset.cast::<PySetObject>(), key) }
}

/// See the CPython 3.12 contract for this entry point: 0 (absent) or
/// 1 (removed).
///
/// # Safety
///
/// `set` must be a set object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Discard(set: *mut PyObject, key: *mut PyObject) -> c_int {
    if !(unsafe { is_set(set) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the guard covers the discard.
    unsafe {
        let _guard = locked(set);
        discard_key(set.cast::<PySetObject>(), key)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `set` must be a set object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Pop(set: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_set(set) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the guard covers the pop.
    unsafe {
        let _guard = locked(set);
        pop(set.cast::<PySetObject>())
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `set` must be a set object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Clear(set: *mut PyObject) -> c_int {
    if !(unsafe { is_set(set) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the guard covers the clear.
    unsafe {
        let _guard = locked(set);
        clear_internal(set.cast::<PySetObject>())
    }
}

/// `_PySet_Update`: fill `set` from `iterable`.
///
/// # Safety
///
/// `set` must be a set object; `iterable` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PySet_Update(set: *mut PyObject, iterable: *mut PyObject) -> c_int {
    if !(unsafe { is_set(set) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the guard covers the whole update.
    unsafe {
        let _guard = locked(set);
        if update_internal(set.cast::<PySetObject>(), iterable).is_ok() {
            return 0;
        }
        -1
    }
}

/// `_PySet_NextEntry`: the next live entry, or 0 at the end (-1 when
/// `set` is not a set).
///
/// # Safety
///
/// `set` must be a set object; `pos` must point to a cursor the caller
/// owns; `key`/`hash` must be null or valid out-pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PySet_NextEntry(
    set: *mut PyObject,
    pos: *mut isize,
    key: *mut *mut PyObject,
    hash: *mut isize,
) -> c_int {
    if !(unsafe { is_anyset(set) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the scan reads only the table.
    unsafe {
        match set_next(set.cast::<PySetObject>(), pos) {
            Some(entry) => {
                if !key.is_null() {
                    *key = (*entry).key;
                }
                if !hash.is_null() {
                    *hash = (*entry).hash;
                }
                1
            }
            None => 0,
        }
    }
}

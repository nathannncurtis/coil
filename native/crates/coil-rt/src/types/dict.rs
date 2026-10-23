//! `dict`: CPython 3.12's prefix layout (`ma_used`, `ma_version_tag` at
//! 16/24, so `PyDict_GET_SIZE` is inline) over the compact ordered dict
//! of PEP 468-era CPython: a combined table of index array plus
//! hash/key/value entries in insertion order, dummy slots on deletion,
//! and CPython's growth and compaction policy, so iteration order
//! matches CPython for the same insertion sequence (spec sections 8.2
//! and 8.5). Views and iterators are free-layout objects; the dict's
//! Coil tail carries the per-object lock of section 8.6 at offset 32,
//! and `tp_basicsize` (48) includes it.
//!
//! Split and unicode-specialised key tables do not exist yet — only
//! general entries, which accept every key type this runtime has; the
//! unicode task adds the specialised kind. `PyDict_Merge`/
//! `PyDict_Update` take their CPython fast path (a dict argument) and
//! name the missing attribute protocol otherwise.

use core::ffi::c_int;

use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_DICT_SUBCLASS, Py_TPFLAGS_HAVE_GC,
    PyMappingMethods, PyObject, PyTypeObject,
};

use crate::object::exceptions;
use crate::object::header;
use crate::object::lock;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};
use crate::types::{object_hash, objects_equal};

/// `dict`'s private extra flag (`_Py_TPFLAGS_MATCH_SELF`).
const PY_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

/// The offset of the per-object lock within a dict.
pub const LOCK: usize = 32;

/// CPython's `PERTURB_SHIFT` (dictobject.c).
const PERTURB_SHIFT: u32 = 5;
/// `PyDict_MINSIZE`.
const MINSIZE: usize = 8;
/// `DKIX_EMPTY`: an index-array slot that never held an entry.
const DKIX_EMPTY: isize = -1;
/// `DKIX_DUMMY`: an index-array slot whose entry was deleted.
const DKIX_DUMMY: isize = -2;
/// `USABLE_FRACTION(n) = ((n) << 1)/3`: entries a table of `n` slots holds.
const fn usable_fraction(n: usize) -> isize {
    ((n << 1) / 3) as isize
}
/// `GROWTH_RATE(d) = ma_used*3` (dictobject.c, 3.12).
fn growth_rate(mp: *const PyDictObject) -> isize {
    unsafe { (*mp).ma_used * 3 }
}
/// `DICT_MAX_WATCHERS` is 8, so versions step by 256
/// (`DICT_VERSION_INCREMENT`), leaving watcher bits at the bottom.
const VERSION_INCREMENT: u64 = 1 << 8;

fn next_version() -> u64 {
    static GLOBAL_VERSION: AtomicU64 = AtomicU64::new(0);
    GLOBAL_VERSION.fetch_add(VERSION_INCREMENT, Ordering::Relaxed) + VERSION_INCREMENT
}

/// CPython 3.12's `PyDictObject`: the header, the prefix fields, then
/// Coil's tail — the lock and the combined-table pointer.
#[repr(C)]
pub struct PyDictObject {
    pub ob_refcnt: isize,
    pub ob_type: *mut PyTypeObject,
    pub ma_used: isize,
    pub ma_version_tag: u64,
    pub ob_lock: AtomicU8,
    pub _pad: [u8; 7],
    pub ma_keys: *mut PyDictKeysObject,
}

/// One entry of the combined table (`PyDictKeyEntry`, general kind).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct PyDictKeyEntry {
    me_hash: isize,
    me_key: *mut PyObject,
    me_value: *mut PyObject,
}

/// The combined table (`_dictkeysobject`): sizing fields, then the index
/// array (1/2/4/8 bytes per slot as the size requires), then the entries.
/// Allocation is one plain block; Coil owns it exclusively (no split
/// tables to share with).
#[repr(C)]
pub struct PyDictKeysObject {
    dk_log2_size: u8,
    dk_log2_index_bytes: u8,
    _pad: [u8; 6],
    dk_usable: isize,
    dk_nentries: isize,
}

/// The shared, never-mutated empty table every fresh dict starts from
/// (CPython's `Py_EMPTY_KEYS`): size 8, no usable slots, so the first
/// insertion swaps in a fresh table instead of touching this one.
struct EmptyKeys {
    head: PyDictKeysObject,
    indices: [i8; MINSIZE],
}

static mut EMPTY_KEYS_RAW: EmptyKeys = EmptyKeys {
    head: PyDictKeysObject {
        dk_log2_size: 3,
        dk_log2_index_bytes: 3,
        _pad: [0; 6],
        dk_usable: 0,
        dk_nentries: 0,
    },
    indices: [DKIX_EMPTY as i8; MINSIZE],
};

fn empty_keys() -> *mut PyDictKeysObject {
    // SAFETY: the address of a static is constant and never null; the
    // table is never mutated, so its index array stays all-EMPTY.
    unsafe {
        let indices = core::ptr::addr_of!(EMPTY_KEYS_RAW.indices) as *const i8;
        debug_assert!((0..MINSIZE as isize).all(|i| {
            // SAFETY: the read is within the static's index array.
            *indices.offset(i) == DKIX_EMPTY as i8
        }));
        &raw mut EMPTY_KEYS_RAW.head
    }
}

/// `DK_SIZE`.
fn size_of(dk: *const PyDictKeysObject) -> usize {
    1usize << unsafe { (*dk).dk_log2_size }
}

/// Where the entries begin: after the fixed header and the index array.
fn entries_of(dk: *mut PyDictKeysObject) -> *mut PyDictKeyEntry {
    let base = dk as *mut u8;
    // SAFETY: the allocation covers header plus index array plus entries.
    unsafe {
        base.add(core::mem::size_of::<PyDictKeysObject>() + (1usize << (*dk).dk_log2_index_bytes))
            .cast()
    }
}

/// `dictkeys_get_index` by the table's index width.
unsafe fn get_index(dk: *mut PyDictKeysObject, i: usize) -> isize {
    // SAFETY: the index array follows the header.
    unsafe {
        let indices = (dk as *mut u8).add(core::mem::size_of::<PyDictKeysObject>());
        match (*dk).dk_log2_size {
            n if n < 8 => indices.add(i).cast::<i8>().read() as isize,
            n if n < 16 => indices.cast::<i16>().add(i).read() as isize,
            n if n >= 32 => indices.cast::<i64>().add(i).read() as isize,
            _ => indices.cast::<i32>().add(i).read() as isize,
        }
    }
}

/// `dictkeys_set_index` by the table's index width.
unsafe fn set_index(dk: *mut PyDictKeysObject, i: usize, ix: isize) {
    // SAFETY: the index array follows the header.
    unsafe {
        let indices = (dk as *mut u8).add(core::mem::size_of::<PyDictKeysObject>());
        match (*dk).dk_log2_size {
            n if n < 8 => indices.add(i).cast::<i8>().write(ix as i8),
            n if n < 16 => indices.cast::<i16>().add(i).write(ix as i16),
            n if n >= 32 => indices.cast::<i64>().add(i).write(ix as i64),
            _ => indices.cast::<i32>().add(i).write(ix as i32),
        }
    }
}

/// `new_keys_object`: a zeroed table of `2^log2_size` slots. Null with
/// `MemoryError` set.
fn new_keys_object(log2_size: u8) -> *mut PyDictKeysObject {
    let log2_bytes = if log2_size < 8 {
        log2_size
    } else if log2_size < 16 {
        log2_size + 1
    } else if log2_size >= 32 {
        log2_size + 3
    } else {
        log2_size + 2
    };
    let usable = usable_fraction(1usize << log2_size);
    let total = core::mem::size_of::<PyDictKeysObject>()
        + (1usize << log2_bytes)
        + usable as usize * core::mem::size_of::<PyDictKeyEntry>();
    let block = crate::alloc::pymem::plain_calloc(total, 1);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    let dk = block.cast::<PyDictKeysObject>();
    // SAFETY: the fresh zeroed block is exclusively ours; every index
    // width reads an all-0xff pattern as DKIX_EMPTY.
    unsafe {
        (*dk).dk_log2_size = log2_size;
        (*dk).dk_log2_index_bytes = log2_bytes;
        (*dk).dk_usable = usable;
        core::ptr::write_bytes(
            (dk as *mut u8).add(core::mem::size_of::<PyDictKeysObject>()),
            0xff,
            1usize << log2_bytes,
        );
    }
    dk
}

/// `calculate_log2_keysize`: the smallest table size (a power of two, at
/// least 8) that beats `minsize`.
fn calculate_log2_keysize(minsize: isize) -> u8 {
    let bits = ((minsize as usize | MINSIZE) - 1) | (MINSIZE - 1);
    (bits.ilog2() + 1) as u8
}

/// Free a table the dict owns (never the shared empty one).
unsafe fn free_keys(dk: *mut PyDictKeysObject) {
    if dk.is_null() || core::ptr::eq(dk, empty_keys()) {
        return;
    }
    // SAFETY: `dk` came from `new_keys_object`.
    unsafe { crate::alloc::pymem::plain_free(dk.cast()) };
}

/// `PyDict_Check` by the fast subclass flag.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_dict(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: the flag read is within the type object.
    !ty.is_null() && unsafe { (*ty).tp_flags } & Py_TPFLAGS_DICT_SUBCLASS != 0
}

unsafe fn lock_of(op: *mut PyObject) -> &'static AtomicU8 {
    // SAFETY: the lock is a fixed field of the dict layout.
    unsafe { &*((op as *const u8).add(LOCK) as *const AtomicU8) }
}

/// The lock guard every mutating entry point holds.
///
/// # Safety
///
/// `op` must be a dict object.
unsafe fn locked(op: *mut PyObject) -> lock::Guard {
    // SAFETY: the lock field is within the dict.
    unsafe { lock::Guard::new(lock_of(op)) }
}

/// `build_indices_generic`: lay every entry into the fresh index array.
unsafe fn build_indices(dk: *mut PyDictKeysObject, entries: *mut PyDictKeyEntry, n: isize) {
    let mask = size_of(dk) - 1;
    // SAFETY: every access stays within the table's allocation.
    unsafe {
        for ix in 0..n {
            let hash = (*entries.add(ix as usize)).me_hash as usize;
            let mut i = hash & mask;
            let mut perturb = hash;
            while get_index(dk, i) != DKIX_EMPTY {
                perturb >>= PERTURB_SHIFT;
                i = (i * 5 + perturb + 1) & mask;
            }
            set_index(dk, i, ix);
        }
    }
}

/// `find_empty_slot`: the first free (empty or dummy) slot for `hash`.
unsafe fn find_empty_slot(dk: *mut PyDictKeysObject, hash: isize) -> usize {
    let mask = size_of(dk) - 1;
    let mut i = hash as usize & mask;
    // SAFETY: the probe stays within the index array.
    unsafe {
        let mut perturb = hash as usize;
        while get_index(dk, i) >= 0 {
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + perturb + 1) & mask;
        }
    }
    i
}

/// `_Py_dict_lookup` for combined general tables: the entry index, or
/// `DKIX_EMPTY`; `Err` carries a raised comparison error.
unsafe fn lookup(mp: *mut PyDictObject, key: *mut PyObject, hash: isize) -> Result<isize, ()> {
    let dk = unsafe { (*mp).ma_keys };
    let entries = entries_of(dk);
    let mask = size_of(dk) - 1;
    let mut i = hash as usize & mask;
    let mut perturb = hash as usize;
    // SAFETY: the probe stays within the table's allocation.
    unsafe {
        loop {
            let ix = get_index(dk, i);
            if ix >= 0 {
                let ep = entries.add(ix as usize);
                if (*ep).me_key == key {
                    return Ok(ix);
                }
                if (*ep).me_hash == hash {
                    // The comparisons this runtime has are pure (no
                    // Python reentry), so CPython's restart-on-mutation
                    // dance cannot trigger here.
                    match objects_equal((*ep).me_key, key) {
                        Ok(true) => return Ok(ix),
                        Ok(false) => {}
                        Err(()) => return Err(()),
                    }
                }
            } else if ix == DKIX_EMPTY {
                return Ok(DKIX_EMPTY);
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + perturb + 1) & mask;
        }
    }
}

/// `lookdict_index`: the index-array slot that entry `index` occupies.
unsafe fn lookdict_index(dk: *mut PyDictKeysObject, hash: isize, index: isize) -> isize {
    let mask = size_of(dk) - 1;
    let mut i = hash as usize & mask;
    let mut perturb = hash as usize;
    // SAFETY: the probe stays within the index array.
    unsafe {
        loop {
            let ix = get_index(dk, i);
            if ix == index {
                return i as isize;
            }
            if ix == DKIX_EMPTY {
                return DKIX_EMPTY;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + perturb + 1) & mask;
        }
    }
}

/// `dictresize` (the caller holds the lock): a fresh table of
/// `2^calculate_log2_keysize(minused)` slots, entries compacted in
/// order (dummies dropped), so insertion order survives.
unsafe fn dictresize(mp: *mut PyDictObject, minused: isize) -> bool {
    if calculate_log2_keysize(minused) as usize >= usize::BITS as usize {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return false;
    }
    let oldkeys = unsafe { (*mp).ma_keys };
    let newkeys = new_keys_object(calculate_log2_keysize(minused));
    if newkeys.is_null() {
        return false;
    }
    let oldentries = entries_of(oldkeys);
    let newentries = entries_of(newkeys);
    // SAFETY: sizes cover the entries of both tables.
    unsafe {
        let numentries = (*mp).ma_used;
        if (*oldkeys).dk_nentries == numentries {
            core::ptr::copy_nonoverlapping(oldentries, newentries, numentries as usize);
        } else {
            // Compact: skip the deleted entries, keeping the live order.
            let mut src = oldentries;
            for i in 0..numentries {
                while (*src).me_value.is_null() {
                    src = src.add(1);
                }
                *newentries.add(i as usize) = *src;
                src = src.add(1);
            }
        }
        (*newkeys).dk_nentries = numentries;
        (*newkeys).dk_usable = usable_fraction(size_of(newkeys)) - numentries;
        build_indices(newkeys, newentries, numentries);
        (*mp).ma_keys = newkeys;
    }
    // SAFETY: `oldkeys` came from `new_keys_object`.
    unsafe { free_keys(oldkeys) };
    true
}

/// `insertion_resize`.
///
/// # Safety
///
/// The caller holds the dict's lock.
unsafe fn insertion_resize(mp: *mut PyDictObject) -> bool {
    // SAFETY: growth_rate reads `ma_used` within the dict.
    unsafe { dictresize(mp, growth_rate(mp)) }
}

/// `insertdict` (the caller holds the lock): set `key`→`value`, bumping
/// the version tag; `Err` carries a raised error.
///
/// # Safety
///
/// The caller holds the dict's lock; `key` and `value` are valid objects.
unsafe fn insertdict(
    mp: *mut PyDictObject,
    key: *mut PyObject,
    hash: isize,
    value: *mut PyObject,
) -> Result<(), ()> {
    // SAFETY: the lookup reads the dict's table only.
    let ix = unsafe { lookup(mp, key, hash) }?;
    if ix == DKIX_EMPTY {
        // SAFETY: the field reads/writes are within the dict and table.
        unsafe {
            if (*(*mp).ma_keys).dk_usable <= 0 && !insertion_resize(mp) {
                return Err(());
            }
            let dk = (*mp).ma_keys;
            let slot = find_empty_slot(dk, hash);
            let n = (*dk).dk_nentries;
            set_index(dk, slot, n);
            let ep = entries_of(dk).add(n as usize);
            (*ep).me_key = key;
            (*ep).me_hash = hash;
            (*ep).me_value = value;
            (*mp).ma_used += 1;
            (*mp).ma_version_tag = next_version();
            (*dk).dk_usable -= 1;
            (*dk).dk_nentries += 1;
        }
        return Ok(());
    }
    // SAFETY: the entry write is within the table.
    unsafe {
        let ep = entries_of((*mp).ma_keys).add(ix as usize);
        if (*ep).me_value != value {
            (*ep).me_value = value;
            (*mp).ma_version_tag = next_version();
        }
    }
    Ok(())
}

/// `delitem_common` (the caller holds the lock).
///
/// # Safety
///
/// The caller holds the dict's lock; `ix` is a live entry index.
unsafe fn del_entry(mp: *mut PyDictObject, hash: isize, ix: isize) {
    // SAFETY: the field reads/writes are within the dict and table.
    unsafe {
        let dk = (*mp).ma_keys;
        let hashpos = lookdict_index(dk, hash, ix);
        (*mp).ma_used -= 1;
        (*mp).ma_version_tag = next_version();
        set_index(dk, hashpos as usize, DKIX_DUMMY);
        let ep = entries_of(dk).add(ix as usize);
        (*ep).me_key = core::ptr::null_mut();
        (*ep).me_value = core::ptr::null_mut();
        (*ep).me_hash = 0;
    }
}

/// `new_dict` with the shared empty table and a fresh version.
fn new_dict() -> *mut PyObject {
    // SAFETY: `PyDict_Type` is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(&raw mut PyDict_Type, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let mp = object.cast::<PyDictObject>();
    // SAFETY: the fresh zeroed block is exclusively ours.
    unsafe {
        (*mp).ma_used = 0;
        (*mp).ma_version_tag = next_version();
        (*mp).ma_keys = empty_keys();
    }
    object
}

unsafe extern "C" fn dict_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe {
        free_keys((*object.cast::<PyDictObject>()).ma_keys);
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn dict_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> c_int {
    let mp = object.cast::<PyDictObject>();
    // SAFETY: the entry reads are within the table's allocation.
    unsafe {
        let dk = (*mp).ma_keys;
        if core::ptr::eq(dk, empty_keys()) {
            return 0;
        }
        let entries = entries_of(dk);
        for i in 0..(*dk).dk_nentries {
            let ep = entries.add(i as usize);
            for referent in [(*ep).me_key, (*ep).me_value] {
                if !referent.is_null() && visit(referent, arg) != 0 {
                    return -1;
                }
            }
        }
    }
    0
}

const fn dict_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"dict".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::dict::SIZEOF as isize;
    t.tp_dealloc = Some(dict_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_HAVE_GC
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_DICT_SUBCLASS
        | PY_TPFLAGS_MATCH_SELF;
    t.tp_traverse = Some(dict_traverse);
    t.tp_iter = Some(dict_iter_key);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `dict` type object (CPython's `PyDict_Type`).
#[unsafe(no_mangle)]
pub static mut PyDict_Type: PyTypeObject = dict_type();

// ------------------------------------------------------------- C API entry

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyDict_New() -> *mut PyObject {
    new_dict()
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Size(mp: *mut PyObject) -> isize {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the prefix field is within the dict.
    unsafe { (*mp.cast::<PyDictObject>()).ma_used }
}

/// `PyDict_GetItem`: a borrowed value, swallowing errors (CPython's
/// documented behaviour).
///
/// # Safety
///
/// `mp` must be a dict object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItem(mp: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    // SAFETY: same lookup contract as `PyDict_GetItemWithError`.
    let result = unsafe { PyDict_GetItemWithError(mp, key) };
    crate::tstate::clear();
    result
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mp` must be a dict object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemWithError(
    mp: *mut PyObject,
    key: *mut PyObject,
) -> *mut PyObject {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return core::ptr::null_mut();
    }
    // SAFETY: the dict and hash are valid.
    match unsafe { lookup(mp.cast::<PyDictObject>(), key, hash) } {
        Ok(ix) if ix >= 0 => unsafe {
            // SAFETY: the entry read is within the table.
            (*entries_of((*mp.cast::<PyDictObject>()).ma_keys).add(ix as usize)).me_value
        },
        Ok(_) => core::ptr::null_mut(),
        Err(()) => core::ptr::null_mut(),
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mp` must be a dict object; `key`/`item` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetItem(
    mp: *mut PyObject,
    key: *mut PyObject,
    item: *mut PyObject,
) -> c_int {
    if !(unsafe { is_dict(mp) }) || key.is_null() || item.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return -1;
    }
    // SAFETY: the guard covers the whole insertion.
    unsafe {
        let _guard = locked(mp);
        if insertdict(mp.cast::<PyDictObject>(), key, hash, item).is_err() {
            return -1;
        }
    }
    0
}

/// `PyDict_DelItem`; missing keys raise `KeyError` with the key as its
/// detail (a decimal rendering for int keys, until str objects exist).
///
/// # Safety
///
/// `mp` must be a dict object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_DelItem(mp: *mut PyObject, key: *mut PyObject) -> c_int {
    if !(unsafe { is_dict(mp) }) || key.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return -1;
    }
    // SAFETY: the guard covers the deletion.
    unsafe {
        let _guard = locked(mp);
        let d = mp.cast::<PyDictObject>();
        match lookup(d, key, hash) {
            Ok(ix) if ix >= 0 => {
                del_entry(d, hash, ix);
                0
            }
            Ok(_) => {
                set_key_error(key);
                -1
            }
            Err(()) => -1,
        }
    }
}

/// `PyDict_Pop`: remove and return the value, or `default` when missing
/// (null default and missing key raise `KeyError`).
///
/// # Safety
///
/// `mp` must be a dict object; `key`/`deflt` must be null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Pop(
    mp: *mut PyObject,
    key: *mut PyObject,
    deflt: *mut PyObject,
) -> *mut PyObject {
    if !(unsafe { is_dict(mp) }) || key.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return core::ptr::null_mut();
    }
    // SAFETY: the guard covers the removal.
    unsafe {
        let _guard = locked(mp);
        let d = mp.cast::<PyDictObject>();
        match lookup(d, key, hash) {
            Ok(ix) if ix >= 0 => {
                let value = (*entries_of((*d).ma_keys).add(ix as usize)).me_value;
                del_entry(d, hash, ix);
                value
            }
            Ok(_) if !deflt.is_null() => deflt,
            Ok(_) => {
                set_key_error(key);
                core::ptr::null_mut()
            }
            Err(()) => core::ptr::null_mut(),
        }
    }
}

/// Record a `KeyError` whose detail is the offending key.
///
/// # Safety
///
/// `key` must be a valid object.
unsafe fn set_key_error(key: *mut PyObject) {
    // SAFETY: `format` reads the int layout.
    let detail = unsafe { crate::types::long::format(key.cast(), 10, false) }.unwrap_or_default();
    crate::tstate::set_string(exceptions::key_error(), detail.as_bytes());
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Clear(mp: *mut PyObject) {
    if !(unsafe { is_dict(mp) }) {
        return;
    }
    // SAFETY: the guard covers the swap; freeing the old table is safe
    // only under it.
    unsafe {
        let _guard = locked(mp);
        let d = mp.cast::<PyDictObject>();
        let old = (*d).ma_keys;
        (*d).ma_keys = empty_keys();
        (*d).ma_used = 0;
        (*d).ma_version_tag = next_version();
        free_keys(old);
    }
}

/// See the CPython 3.12 contract for this entry point: the stored value
/// (a new reference), `defaultobj` when absent.
///
/// # Safety
///
/// `mp` must be a dict object; `key` and `defaultobj` must be valid
/// objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetDefault(
    mp: *mut PyObject,
    key: *mut PyObject,
    defaultobj: *mut PyObject,
) -> *mut PyObject {
    if !(unsafe { is_dict(mp) }) || key.is_null() || defaultobj.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return core::ptr::null_mut();
    }
    let d = mp.cast::<PyDictObject>();
    // SAFETY: the lockless lookup only reads the table.
    if let Ok(ix) = unsafe { lookup(d, key, hash) }
        && ix >= 0
    {
        // SAFETY: the entry read is within the table.
        return unsafe { (*entries_of((*d).ma_keys).add(ix as usize)).me_value };
    }
    // SAFETY: the guard covers the insert; `insertdict` re-looks-up, so
    // a racing insertion is left in place.
    unsafe {
        let _guard = locked(mp);
        if insertdict(d, key, hash, defaultobj).is_err() {
            return core::ptr::null_mut();
        }
    }
    defaultobj
}

/// See the CPython 3.12 contract for this entry point: -1/0/1.
///
/// # Safety
///
/// `mp` must be a dict object; `key` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Contains(mp: *mut PyObject, key: *mut PyObject) -> c_int {
    if !(unsafe { is_dict(mp) }) || key.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `key` is a valid object of a readied type.
    let hash = unsafe { object_hash(key) };
    if hash == -1 {
        return -1;
    }
    // SAFETY: the dict and hash are valid.
    match unsafe { lookup(mp.cast::<PyDictObject>(), key, hash) } {
        Ok(ix) => (ix >= 0) as c_int,
        Err(()) => -1,
    }
}

/// `_PyDict_Next`: the next live entry in insertion order, or 0 at the
/// end. `*ppos` is the opaque cursor.
///
/// # Safety
///
/// `op` must be a dict object; `ppos` must point to a cursor the caller
/// owns; `pkey`/`pvalue` must be null or valid out-pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Next(
    op: *mut PyObject,
    ppos: *mut isize,
    pkey: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
) -> c_int {
    if !(unsafe { is_dict(op) }) {
        return 0;
    }
    // SAFETY: the reads stay within the dict and its table.
    unsafe {
        let d = op.cast::<PyDictObject>();
        let dk = (*d).ma_keys;
        let n = (*dk).dk_nentries;
        let mut i = *ppos;
        if i < 0 || i >= n {
            return 0;
        }
        let entries = entries_of(dk);
        while i < n && (*entries.add(i as usize)).me_value.is_null() {
            i += 1;
        }
        if i >= n {
            return 0;
        }
        *ppos = i + 1;
        if !pkey.is_null() {
            *pkey = (*entries.add(i as usize)).me_key;
        }
        if !pvalue.is_null() {
            *pvalue = (*entries.add(i as usize)).me_value;
        }
    }
    1
}

/// The dict's entries as (key, value) pairs in insertion order; Err with
/// the exception set. The views, `PyDict_Keys` and friends build on it.
///
/// # Safety
///
/// `mp` must be a dict object.
pub(crate) unsafe fn dict_items(
    mp: *mut PyObject,
) -> Result<Vec<(*mut PyObject, *mut PyObject)>, ()> {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return Err(());
    }
    let mut items = Vec::new();
    // SAFETY: `PyDict_Next` walks the live entries in insertion order.
    unsafe {
        let mut pos: isize = 0;
        let mut key: *mut PyObject = core::ptr::null_mut();
        let mut value: *mut PyObject = core::ptr::null_mut();
        while PyDict_Next(mp, &raw mut pos, &raw mut key, &raw mut value) != 0 {
            items.push((key, value));
        }
    }
    Ok(items)
}

/// See the CPython 3.12 contract for this entry point: a list of keys.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Keys(mp: *mut PyObject) -> *mut PyObject {
    // SAFETY: `dict_items` validates `mp` and sets the error on a miss.
    let items = match unsafe { dict_items(mp) } {
        Ok(items) => items,
        Err(()) => return core::ptr::null_mut(),
    };
    view_or_list(items.into_iter().map(|(k, _)| k).collect())
}

/// See the CPython 3.12 contract for this entry point: a list of values.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Values(mp: *mut PyObject) -> *mut PyObject {
    // SAFETY: `dict_items` validates `mp` and sets the error on a miss.
    let items = match unsafe { dict_items(mp) } {
        Ok(items) => items,
        Err(()) => return core::ptr::null_mut(),
    };
    view_or_list(items.into_iter().map(|(_, v)| v).collect())
}

/// See the CPython 3.12 contract for this entry point: a list of
/// (key, value) tuples.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Items(mp: *mut PyObject) -> *mut PyObject {
    // SAFETY: `dict_items` validates `mp` and sets the error on a miss.
    let items = match unsafe { dict_items(mp) } {
        Ok(items) => items,
        Err(()) => return core::ptr::null_mut(),
    };
    let pairs: Vec<*mut PyObject> = items
        .into_iter()
        .map(|(k, v)| crate::types::tuple::from_array(&[k, v]))
        .collect();
    view_or_list(pairs)
}

/// A list of `items`: the snapshot `PyDict_Keys` and friends return.
fn view_or_list(items: Vec<*mut PyObject>) -> *mut PyObject {
    // SAFETY: `PyList_New` and the stores build a valid list.
    unsafe {
        let list = crate::types::list::PyList_New(items.len() as isize);
        if list.is_null() {
            return core::ptr::null_mut();
        }
        for (i, item) in items.iter().enumerate() {
            *(*list.cast::<crate::types::list::PyListObject>())
                .ob_item
                .add(i) = *item;
        }
        list
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mp` must be a dict object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Copy(mp: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `mp` is a dict object.
    let items = match unsafe { dict_items(mp) } {
        Ok(items) => items,
        Err(()) => return core::ptr::null_mut(),
    };
    let copy = new_dict();
    if copy.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the copy is exclusively ours here.
    unsafe {
        let d = copy.cast::<PyDictObject>();
        let _guard = locked(copy);
        for (key, value) in items {
            let hash = object_hash(key);
            let ok = hash != -1 && insertdict(d, key, hash, value).is_ok();
            if !ok {
                free_keys((*d).ma_keys);
                crate::alloc::free_block(copy.cast());
                return core::ptr::null_mut();
            }
        }
    }
    copy
}

/// `PyDict_Merge` for the dict fast path (CPython's `dict_merge`): copy
/// entries in insertion order, overwriting when `override` or inserting
/// when missing. A non-dict `other` needs the attribute protocol, which
/// this runtime does not have yet; that case names the gap.
///
/// # Safety
///
/// `mp` must be a dict object; `other` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Merge(
    mp: *mut PyObject,
    other: *mut PyObject,
    override_: c_int,
) -> c_int {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    if !(unsafe { is_dict(other) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"coil: PyDict_Merge from a non-dict mapping needs the attribute protocol",
        );
        return -1;
    }
    // SAFETY: both are dict objects.
    unsafe {
        let items = match dict_items(other) {
            Ok(items) => items,
            Err(()) => return -1,
        };
        let d = mp.cast::<PyDictObject>();
        let _guard = locked(mp);
        for (key, value) in items {
            let hash = object_hash(key);
            if hash == -1 {
                return -1;
            }
            if override_ == 0 && matches!(lookup(d, key, hash), Ok(ix) if ix >= 0) {
                continue;
            }
            if insertdict(d, key, hash, value).is_err() {
                return -1;
            }
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point: `PyDict_Merge`
/// with override.
///
/// # Safety
///
/// `mp` must be a dict object; `other` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Update(mp: *mut PyObject, other: *mut PyObject) -> c_int {
    // SAFETY: same contract as `PyDict_Merge`.
    unsafe { PyDict_Merge(mp, other, 1) }
}

/// `PyDict_MergeFromSeq2`: merge from an iterable of key/value pairs.
///
/// # Safety
///
/// `d` must be a dict object; `seq2` must be a valid iterable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_MergeFromSeq2(
    d: *mut PyObject,
    seq2: *mut PyObject,
    override_: c_int,
) -> c_int {
    if !(unsafe { is_dict(d) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `seq2` is a valid object.
    let pairs = match unsafe { crate::types::list::collect_iterable(seq2) } {
        Ok(pairs) => pairs,
        Err(()) => return -1,
    };
    // SAFETY: `d` is a dict object.
    unsafe {
        let mp = d.cast::<PyDictObject>();
        let _guard = locked(d);
        for pair in pairs {
            let fields = match crate::types::list::collect_iterable(pair) {
                Ok(fields) if fields.len() == 2 => fields,
                _ => {
                    crate::tstate::set_string(
                        exceptions::value_error(),
                        b"dictionary update sequence element #0 has length 1; 2 is required",
                    );
                    return -1;
                }
            };
            let hash = object_hash(fields[0]);
            if hash == -1 {
                return -1;
            }
            if override_ == 0 && matches!(lookup(mp, fields[0], hash), Ok(ix) if ix >= 0) {
                continue;
            }
            if insertdict(mp, fields[0], hash, fields[1]).is_err() {
                return -1;
            }
        }
    }
    0
}

// ------------------------------------------------------ views, iterators

/// The view kinds (`PyDictIterKind`).
const VIEW_KEYS: u8 = 0;
const VIEW_VALUES: u8 = 1;
const VIEW_ITEMS: u8 = 2;

/// CPython's `_PyDictViewObject` (a free-layout object): the dict it
/// watches; the kind lives in the view's type object.
#[repr(C)]
struct DictViewObject {
    ob_base: PyObject,
    dv_dict: *mut PyObject,
}

/// CPython's `dictiterobject` (a free-layout object): the dict, the size
/// it started at (a changed size ends iteration), and the cursor.
#[repr(C)]
struct DictIterObject {
    ob_base: PyObject,
    di_dict: *mut PyObject,
    di_used: isize,
    di_pos: isize,
    di_kind: u8,
}

unsafe extern "C" fn view_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

/// A view's length: the watched dict's `ma_used`.
unsafe extern "C" fn dictview_len(object: *mut PyObject) -> isize {
    let view = object.cast::<DictViewObject>();
    // SAFETY: the dict pointer and prefix read are valid while the view
    // is alive.
    unsafe {
        let d = (*view).dv_dict;
        if d.is_null() {
            return 0;
        }
        (*d.cast::<PyDictObject>()).ma_used
    }
}

/// The view's kind, by identity against the three view types.
unsafe fn view_kind(object: *mut PyObject) -> u8 {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(object) };
    if core::ptr::eq(ty, &raw mut PyDictKeys_Type) {
        VIEW_KEYS
    } else if core::ptr::eq(ty, &raw mut PyDictValues_Type) {
        VIEW_VALUES
    } else {
        VIEW_ITEMS
    }
}

unsafe extern "C" fn dictview_iter(object: *mut PyObject) -> *mut PyObject {
    let view = object.cast::<DictViewObject>();
    // SAFETY: the kind lookup reads only the view header; the dict is
    // valid while the view is alive.
    unsafe { new_dict_iter((*view).dv_dict, view_kind(object)) }
}

static mut VIEW_AS_MAPPING: PyMappingMethods = PyMappingMethods {
    mp_length: Some(dictview_len),
    mp_subscript: None,
    mp_ass_subscript: None,
};

/// The view types: length by mapping protocol, iteration delegating to
/// the matching dict iterator.
macro_rules! view_type {
    ($(#[$doc:meta])* $name:ident, $tp_name:literal) => {
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub static mut $name: PyTypeObject = {
            let mut t = zero_type();
            t.tp_name = concat!($tp_name, "\0").as_ptr() as *const core::ffi::c_char;
            t.tp_basicsize = core::mem::size_of::<DictViewObject>() as isize;
            t.tp_dealloc = Some(view_dealloc);
            t.tp_flags = Py_TPFLAGS_DEFAULT;
            t.tp_as_mapping = &raw mut VIEW_AS_MAPPING;
            t.tp_iter = Some(dictview_iter);
            t.tp_alloc = Some(PyType_GenericAlloc);
            t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
            t
        };
    };
}

view_type!(
    /// The `dict_keys` type (CPython's `PyDictKeys_Type`).
    PyDictKeys_Type,
    "dict_keys"
);
view_type!(
    /// The `dict_values` type (CPython's `PyDictValues_Type`).
    PyDictValues_Type,
    "dict_values"
);
view_type!(
    /// The `dict_items` type (CPython's `PyDictItems_Type`).
    PyDictItems_Type,
    "dict_items"
);

/// `_PyDictView_New`: a view of `type_` (one of the three view types)
/// over `dict`.
///
/// # Safety
///
/// `dict` must be null or a dict object; `type_` must be null or one of
/// the three view type objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyDictView_New(
    dict: *mut PyObject,
    type_: *mut PyTypeObject,
) -> *mut PyObject {
    if dict.is_null() || !(unsafe { is_dict(dict) }) || type_.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the type is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(type_, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let view = object.cast::<DictViewObject>();
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*view).dv_dict = dict;
    }
    object
}

unsafe extern "C" fn iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn dict_iter_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    let di = object.cast::<DictIterObject>();
    // SAFETY: the field read is within the iterator's allocation.
    unsafe {
        let dict = (*di).di_dict;
        if !dict.is_null() && visit(dict, arg) != 0 {
            return -1;
        }
    }
    0
}

/// `dictiter_iternext` shared by the three kinds: stop when the dict's
/// size changed, skip deleted entries, hand out key/value/item.
unsafe extern "C" fn dict_iter_iternext(object: *mut PyObject) -> *mut PyObject {
    let it = object.cast::<DictIterObject>();
    // SAFETY: the iterator fields are within the allocation.
    unsafe {
        let d = (*it).di_dict;
        if d.is_null() {
            return core::ptr::null_mut();
        }
        let dict = d.cast::<PyDictObject>();
        if (*it).di_used != (*dict).ma_used {
            (*it).di_dict = core::ptr::null_mut();
            crate::tstate::set_string(
                exceptions::runtime_error(),
                b"dictionary changed size during iteration",
            );
            return core::ptr::null_mut();
        }
        let dk = (*dict).ma_keys;
        let n = (*dk).dk_nentries;
        let mut i = (*it).di_pos;
        if i < 0 || i >= n {
            (*it).di_dict = core::ptr::null_mut();
            return core::ptr::null_mut();
        }
        let entries = entries_of(dk);
        while i < n && (*entries.add(i as usize)).me_value.is_null() {
            i += 1;
        }
        if i >= n {
            (*it).di_dict = core::ptr::null_mut();
            return core::ptr::null_mut();
        }
        let key = (*entries.add(i as usize)).me_key;
        let value = (*entries.add(i as usize)).me_value;
        (*it).di_pos = i + 1;
        match (*it).di_kind {
            VIEW_KEYS => key,
            VIEW_VALUES => value,
            _ => crate::types::tuple::from_array(&[key, value]),
        }
    }
}

/// The iterator types.
macro_rules! iter_type {
    ($(#[$doc:meta])* $name:ident, $tp_name:literal) => {
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub static mut $name: PyTypeObject = {
            let mut t = zero_type();
            t.tp_name = concat!($tp_name, "\0").as_ptr() as *const core::ffi::c_char;
            t.tp_basicsize = core::mem::size_of::<DictIterObject>() as isize;
            t.tp_dealloc = Some(iter_dealloc);
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_traverse = Some(dict_iter_traverse);
            t.tp_iter = Some(crate::types::tuple::self_iter);
            t.tp_iternext = Some(dict_iter_iternext);
            t.tp_alloc = Some(PyType_GenericAlloc);
            t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
            t
        };
    };
}

iter_type!(
    /// The `dict_keyiterator` type (CPython's `PyDictIterKey_Type`).
    PyDictIterKey_Type,
    "dict_keyiterator"
);
iter_type!(
    /// The `dict_valueiterator` type (CPython's `PyDictIterValue_Type`).
    PyDictIterValue_Type,
    "dict_valueiterator"
);
iter_type!(
    /// The `dict_itemiterator` type (CPython's `PyDictIterItem_Type`).
    PyDictIterItem_Type,
    "dict_itemiterator"
);

/// An iterator of `kind` over `mp`.
///
/// # Safety
///
/// `mp` must be a dict object.
unsafe fn new_dict_iter(mp: *mut PyObject, kind: u8) -> *mut PyObject {
    let ty = match kind {
        VIEW_KEYS => &raw mut PyDictIterKey_Type,
        VIEW_VALUES => &raw mut PyDictIterValue_Type,
        _ => &raw mut PyDictIterItem_Type,
    };
    // SAFETY: the type is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(ty, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let it = object.cast::<DictIterObject>();
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*it).di_dict = mp;
        (*it).di_used = (*mp.cast::<PyDictObject>()).ma_used;
        (*it).di_pos = 0;
        (*it).di_kind = kind;
    }
    object
}

/// `dict.__iter__`: a key iterator.
unsafe extern "C" fn dict_iter_key(mp: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_dict(mp) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `mp` is a dict object.
    unsafe { new_dict_iter(mp, VIEW_KEYS) }
}

/// The static view and iterator types, for init.
pub(crate) fn dict_static_types() -> Vec<*mut PyTypeObject> {
    vec![
        &raw mut PyDictKeys_Type,
        &raw mut PyDictValues_Type,
        &raw mut PyDictItems_Type,
        &raw mut PyDictIterKey_Type,
        &raw mut PyDictIterValue_Type,
        &raw mut PyDictIterItem_Type,
    ]
}

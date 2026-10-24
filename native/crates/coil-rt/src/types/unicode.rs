//! `str`: CPython 3.12's PEP 393 unicode objects — compact ASCII and
//! compact Unicode layouts over 1/2/4-byte kinds so `PyUnicode_DATA`
//! inline access from extensions works byte for byte — with SipHash-1-3
//! under a per-process key, the UTF-8 cache, interning, the string
//! methods behind `PyUnicode_Type`, `%`-formatting and the `format()`
//! spec mini-language (spec sections 8.2 and 8.5).
//!
//! The algorithms are CPython's (`unicodeobject.c`, `unicodectype.c`,
//! `pyhash.c`) over the character tables of `unicode_data`. Search
//! algorithms are the straightforward scans — the adaptive fast-search
//! optimisation changes constants, never results. Where CPython keeps a
//! `_PyUnicodeWriter` accumulator this code grows a `String` of the same
//! characters, since Rust strings and compact 1-byte unicode share their
//! representation only in spirit.

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::hash::{BuildHasher, Hasher};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_UNICODE_SUBCLASS, PyMappingMethods,
    PyNumberMethods, PyObject, PySequenceMethods, PyTypeObject,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{
    Py_EQ as CMP_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE as CMP_NE, zero_type,
};
use crate::types::exc as exceptions;
use crate::types::unicode_data;
use crate::util;

/// `PyUnicode_1BYTE_KIND`.
pub const PY_UNICODE_1BYTE_KIND: c_int = 1;
/// `PyUnicode_2BYTE_KIND`.
pub const PY_UNICODE_2BYTE_KIND: c_int = 2;
/// `PyUnicode_4BYTE_KIND`.
pub const PY_UNICODE_4BYTE_KIND: c_int = 4;

/// `MAX_UNICODE`, the highest defined code point.
pub const MAX_UNICODE: u32 = 0x10FFFF + 1;

/// `SSTATE_NOT_INTERNED`.
pub const SSTATE_NOT_INTERNED: u32 = 0;
/// `SSTATE_INTERNED_MORTAL`.
pub const SSTATE_INTERNED_MORTAL: u32 = 1;
/// `SSTATE_INTERNED_IMMORTAL`.
pub const SSTATE_INTERNED_IMMORTAL: u32 = 2;
/// `SSTATE_INTERNED_IMMORTAL_STATIC`.
pub const SSTATE_INTERNED_IMMORTAL_STATIC: u32 = 3;

/// The `PyASCIIObject.state` bitfield: `interned:2, kind:3, compact:1,
/// ascii:1, statically_allocated:1` in the low byte, 24 padding bits above
/// (the C bitfield order of little-endian MSVC). Code touches it through
/// `State` accessors so the packed layout stays exact.
#[derive(Clone, Copy)]
#[repr(C)]
pub union PyUnicodeState {
    bits: u32,
    layout: StateBits,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct StateBits {
    _interned: u32,
}

impl PyUnicodeState {
    pub const fn new(interned: u32, kind: c_int, compact: bool, ascii: bool) -> Self {
        let bits =
            interned | ((kind as u32) << 2) | ((compact as u32) << 5) | ((ascii as u32) << 6);
        // SAFETY: `bits` is a valid initialisation of either union field.
        PyUnicodeState { bits }
    }

    pub fn interned(self) -> u32 {
        // SAFETY: both fields read the same 32 bits.
        (unsafe { self.bits }) & 3
    }

    pub fn set_interned(&mut self, interned: u32) {
        // SAFETY: both fields address the same 32 bits.
        let bits = unsafe { &mut self.bits };
        *bits = (*bits & !3) | interned;
    }

    pub fn kind(self) -> c_int {
        // SAFETY: both fields read the same 32 bits.
        (((unsafe { self.bits }) >> 2) & 7) as c_int
    }

    pub fn compact(self) -> bool {
        // SAFETY: both fields read the same 32 bits.
        (unsafe { self.bits }) & 0x20 != 0
    }

    pub fn ascii(self) -> bool {
        // SAFETY: both fields read the same 32 bits.
        (unsafe { self.bits }) & 0x40 != 0
    }

    pub fn statically_allocated(self) -> bool {
        // SAFETY: both fields read the same 32 bits.
        (unsafe { self.bits }) & 0x80 != 0
    }
}

/// CPython 3.12's `PyASCIIObject` for compact ASCII strings (spec section
/// 8.2's exact class): header, length, cached hash and the packed state.
#[repr(C)]
pub struct PyASCIIObject {
    pub ob_base: PyObject,
    pub length: isize,
    pub hash: isize,
    pub state: PyUnicodeState,
}

/// `PyCompactUnicodeObject` for compact non-ASCII strings, with the UTF-8
/// cache (`utf8_length` is 0 until computed, `utf8` null until computed;
/// CPython's -1 sentinel predates 3.12's lazy scheme).
#[repr(C)]
pub struct PyCompactUnicodeObject {
    pub _base: PyASCIIObject,
    pub utf8_length: isize,
    pub utf8: *mut c_char,
}

/// `PyUnicodeObject` for legacy (subclass-created) strings.
#[repr(C)]
pub struct PyUnicodeObject {
    pub _base: PyCompactUnicodeObject,
    pub data: *mut c_void,
}

/// `PyUnicodeErrorObject`: the exact layout behind `UnicodeDecodeError`,
/// `UnicodeEncodeError` and `UnicodeTranslateError` instances — the
/// `PyBaseExceptionObject` header, then encoding, object, start, end and
/// reason (spec section 8.2).
#[repr(C)]
pub struct PyUnicodeErrorObject {
    pub ob_base: PyObject,
    pub dict: *mut PyObject,
    pub args: *mut PyObject,
    pub notes: *mut PyObject,
    pub traceback: *mut PyObject,
    pub context: *mut PyObject,
    pub cause: *mut PyObject,
    pub suppress_context: c_int,
    pub encoding: *mut PyObject,
    pub object: *mut PyObject,
    pub start: isize,
    pub end: isize,
    pub reason: *mut PyObject,
}

// ------------------------------------------------------------- accessors

/// `PyUnicode_GET_LENGTH`.
///
/// # Safety
///
/// `op` must be null or point to a valid string object.
pub unsafe fn get_length(op: *const PyObject) -> isize {
    if op.is_null() {
        return 0;
    }
    // SAFETY: `length` is the first payload field of every string layout.
    unsafe { (*(op as *const PyASCIIObject)).length }
}

/// `PyUnicode_KIND`.
///
/// # Safety
///
/// `op` must be null or point to a valid string object.
pub unsafe fn kind(op: *const PyObject) -> c_int {
    // SAFETY: `state` is at a fixed offset in every string layout.
    unsafe { (*(op as *const PyASCIIObject)).state.kind() }
}

/// Whether the string is a `str` or a `str` subclass.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject`.
pub(crate) unsafe fn check(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    !ty.is_null()
        && unsafe { crate::object::typeobject::chain_is_subtype(ty, &raw mut PyUnicode_Type) }
}

/// `PyUnicode_CheckExact`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject`.
pub(crate) unsafe fn check_exact(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    core::ptr::eq(ty, core::ptr::addr_of_mut!(PyUnicode_Type))
}

/// `PyUnicode_DATA`: the raw character buffer of any string layout, the
/// pointer extensions read inline.
///
/// # Safety
///
/// `op` must point to a valid string object.
pub unsafe fn data(op: *const PyObject) -> *mut c_void {
    // SAFETY: the state read is within the object; each branch reads a
    // pointer or computes a header-relative offset of that layout.
    unsafe {
        if (*(op as *const PyASCIIObject)).state.compact() {
            if (*(op as *const PyASCIIObject)).state.ascii() {
                (op as *mut u8).add(coil_rt_abi::cp312::ascii::SIZEOF) as *mut c_void
            } else {
                (op as *mut u8).add(coil_rt_abi::cp312::compact_unicode::SIZEOF) as *mut c_void
            }
        } else {
            (*(op as *const PyUnicodeObject)).data
        }
    }
}

/// `PyUnicode_1BYTE_DATA`.
///
/// # Safety
///
/// `op` must be a string of kind 1.
pub unsafe fn data_u8(op: *const PyObject) -> *mut u8 {
    // SAFETY: the caller guarantees kind 1.
    unsafe { data(op) as *mut u8 }
}

/// `PyUnicode_2BYTE_DATA`.
///
/// # Safety
///
/// `op` must be a string of kind 2.
pub unsafe fn data_u16(op: *const PyObject) -> *mut u16 {
    // SAFETY: the caller guarantees kind 2.
    unsafe { data(op) as *mut u16 }
}

/// `PyUnicode_4BYTE_DATA`.
///
/// # Safety
///
/// `op` must be a string of kind 4.
pub unsafe fn data_u32(op: *const PyObject) -> *mut u32 {
    // SAFETY: the caller guarantees kind 4.
    unsafe { data(op) as *mut u32 }
}

/// `PyUnicode_READ` for one character of any kind.
///
/// # Safety
///
/// `kind` and `data` must come from the same valid string.
pub unsafe fn read(kind: c_int, data: *const c_void, index: isize) -> u32 {
    // SAFETY: the index is within the string by the caller's contract.
    unsafe {
        match kind {
            PY_UNICODE_2BYTE_KIND => *(data as *const u16).offset(index) as u32,
            PY_UNICODE_4BYTE_KIND => *(data as *const u32).offset(index),
            _ => *(data as *const u8).offset(index) as u32,
        }
    }
}

/// `PyUnicode_MAX_CHAR_VALUE`.
///
/// # Safety
///
/// `op` must point to a valid string object.
pub unsafe fn max_char_value(op: *const PyObject) -> u32 {
    // SAFETY: state and kind reads are within the object.
    unsafe {
        if (*(op as *const PyASCIIObject)).state.ascii() {
            0x7F
        } else {
            match kind(op) {
                PY_UNICODE_1BYTE_KIND => 0xFF,
                PY_UNICODE_2BYTE_KIND => 0xFFFF,
                _ => MAX_UNICODE - 1,
            }
        }
    }
}

/// `PyUnicode_WRITE` for one character of any kind.
///
/// # Safety
///
/// `kind` and `data` must come from the same valid writable string and the
/// index must be within it.
pub unsafe fn write(kind: c_int, data: *mut c_void, index: isize, ch: u32) {
    // SAFETY: the range and kind are the caller's contract.
    unsafe {
        match kind {
            PY_UNICODE_2BYTE_KIND => *(data as *mut u16).offset(index) = ch as u16,
            PY_UNICODE_4BYTE_KIND => *(data as *mut u32).offset(index) = ch,
            _ => *(data as *mut u8).offset(index) = ch as u8,
        }
    }
}

/// The cached hash slot of a string (`_PyUnicode_HASH`): -1 until computed.
///
/// # Safety
///
/// `op` must point to a valid string object.
unsafe fn hash_slot(op: *const PyObject) -> *mut isize {
    // SAFETY: `hash` is at a fixed offset in every string layout.
    unsafe { core::ptr::addr_of_mut!((*(op as *mut PyASCIIObject)).hash) }
}

// -------------------------------------------------------------- hashing

/// The SipHash-1-3 key, drawn once per process. CPython seeds it from OS
/// entropy at startup (spec section 8.5); the two halves here come from two
/// independent `RandomState`s, whose seeds are OS entropy.
static HASH_KEY0: AtomicU64 = AtomicU64::new(0);
static HASH_KEY1: AtomicU64 = AtomicU64::new(0);
static HASH_KEY_SET: AtomicBool = AtomicBool::new(false);

fn hash_key() -> (u64, u64) {
    if HASH_KEY_SET.load(Ordering::Relaxed) {
        return (
            HASH_KEY0.load(Ordering::Relaxed),
            HASH_KEY1.load(Ordering::Relaxed),
        );
    }
    // `RandomState::build_hasher().finish()` over distinct values yields
    // unrelated 64-bit words per process.
    let seed = std::collections::hash_map::RandomState::new();
    let k0 = seed.build_hasher().finish();
    let mut other = seed.build_hasher();
    other.write_u64(0x5851_F42D_4C95_7F2D);
    let k1 = other.finish();
    HASH_KEY0.store(k0 | 1, Ordering::Relaxed);
    HASH_KEY1.store(k1, Ordering::Relaxed);
    HASH_KEY_SET.store(true, Ordering::Relaxed);
    (k0 | 1, k1)
}

/// Test hook: pin the SipHash-1-3 key so hash results become comparable
/// with a CPython running under the matching `PYTHONHASHSEED`. With seed 0
/// CPython zeroes the secret, so `(0, 0)` reproduces `PYTHONHASHSEED=0`.
pub fn set_hash_key_for_tests(k0: u64, k1: u64) {
    HASH_KEY0.store(k0, Ordering::Relaxed);
    HASH_KEY1.store(k1, Ordering::Relaxed);
    HASH_KEY_SET.store(true, Ordering::Relaxed);
}

/// CPython's `siphash13` (`pyhash.c`): SipHash with one compression round
/// per block and three finalisation rounds.
pub fn siphash13(k0: u64, k1: u64, src: &[u8]) -> u64 {
    // pyhash.c SINGLE_ROUND: the second HALF_ROUND permutes the lanes
    // (v2,v1,v0,v3), unlike stock SipHash.
    fn single_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
        *v0 = v0.wrapping_add(*v1);
        *v1 = v1.rotate_left(13) ^ *v0;
        *v2 = v2.wrapping_add(*v3);
        *v3 = v3.rotate_left(16) ^ *v2;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v1);
        *v1 = v1.rotate_left(17) ^ *v2;
        *v0 = v0.wrapping_add(*v3);
        *v3 = v3.rotate_left(21) ^ *v0;
        *v2 = v2.rotate_left(32);
    }

    let mut b = (src.len() as u64) << 56;
    let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
    let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
    let mut v3 = k1 ^ 0x7465_6462_7974_6573;

    let (chunks, tail) = src.as_chunks::<8>();
    for chunk in chunks {
        let mi = u64::from_le_bytes(*chunk);
        v3 ^= mi;
        single_round(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= mi;
    }
    let mut padded = [0u8; 8];
    padded[..tail.len()].copy_from_slice(tail);
    b |= u64::from_le_bytes(padded);
    v3 ^= b;
    single_round(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^= b;
    v2 ^= 0xff;
    for _ in 0..3 {
        single_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    (v0 ^ v1) ^ (v2 ^ v3)
}

/// `_Py_HashBytes`: 0 for empty, the SipHash-1-3 of the raw bytes
/// otherwise, with -1 mapped to -2.
pub fn hash_bytes(src: &[u8]) -> isize {
    if src.is_empty() {
        return 0;
    }
    let (k0, k1) = hash_key();
    let hash = siphash13(k0, k1, src) as isize;
    if hash == -1 { -2 } else { hash }
}

/// `unicode_hash`: SipHash-1-3 over the canonical 1/2/4-byte data, cached
/// in the object.
///
/// # Safety
///
/// `self_` must point to a valid string object.
pub(crate) unsafe fn unicode_hash(self_: *mut PyObject) -> isize {
    // SAFETY: the length, hash and data reads are within the string.
    unsafe {
        let slot = hash_slot(self_);
        let cached = *slot;
        if cached != -1 {
            return cached;
        }
        let length = get_length(self_);
        let bytes = core::slice::from_raw_parts(
            data(self_) as *const u8,
            (length * kind(self_) as isize) as usize,
        );
        let hash = hash_bytes(bytes);
        *slot = hash;
        hash
    }
}
// ------------------------------------------------------------ singletons

/// CPython's static immortal strings (`_Py_STR(empty)`,
/// `_PyUnicode_ASCII_BASE_INIT` + data, `_PyUnicode_LATIN1_INIT`): the
/// compact data follows the header inside the same static block, so the
/// inline `PyUnicode_DATA` arithmetic lands on the character bytes.
#[repr(C)]
struct StaticAscii {
    header: PyASCIIObject,
    data: [u8; 2],
}

#[repr(C)]
struct StaticLatin1 {
    header: PyCompactUnicodeObject,
    data: [u8; 2],
}

/// One-character compact-ASCII immortal string, `_PyUnicode_ASCII_BASE_INIT`
/// shape: hash unset, not interned, statically allocated.
const fn ascii_char(ch: u8) -> StaticAscii {
    StaticAscii {
        header: PyASCIIObject {
            ob_base: PyObject {
                ob_refcnt: header::IMMORTAL_REFCNT,
                ob_type: &raw mut PyUnicode_Type,
            },
            length: 1,
            hash: -1,
            state: PyUnicodeState::new(SSTATE_NOT_INTERNED, PY_UNICODE_1BYTE_KIND, true, true),
        },
        data: [ch, 0],
    }
}

/// One-character compact latin-1 immortal string with its two-byte UTF-8
/// cache pre-populated, `_PyUnicode_LATIN1_INIT` shape.
const fn latin1_char(ch: u8, utf8: *mut c_char) -> StaticLatin1 {
    StaticLatin1 {
        header: PyCompactUnicodeObject {
            _base: PyASCIIObject {
                ob_base: PyObject {
                    ob_refcnt: header::IMMORTAL_REFCNT,
                    ob_type: &raw mut PyUnicode_Type,
                },
                length: 1,
                hash: -1,
                state: PyUnicodeState::new(SSTATE_NOT_INTERNED, PY_UNICODE_1BYTE_KIND, true, false),
            },
            utf8_length: 2,
            utf8,
        },
        data: [ch, 0],
    }
}

/// The UTF-8 encoding of one latin-1 code point, null-terminated.
const fn latin1_utf8(ch: u8) -> [u8; 3] {
    [0xC0 | (ch >> 6), 0x80 | (ch & 0x3F), 0]
}

static mut EMPTY_STRING: StaticAscii = StaticAscii {
    header: PyASCIIObject {
        ob_base: PyObject {
            ob_refcnt: header::IMMORTAL_REFCNT,
            ob_type: &raw mut PyUnicode_Type,
        },
        length: 0,
        hash: -1,
        state: PyUnicodeState::new(SSTATE_NOT_INTERNED, PY_UNICODE_1BYTE_KIND, true, true),
    },
    data: [0; 2],
};

static mut ASCII_SINGLETONS: [StaticAscii; 128] = {
    let mut table = [const { ascii_char(0) }; 128];
    let mut ch = 0usize;
    while ch < 128 {
        table[ch] = ascii_char(ch as u8);
        ch += 1;
    }
    table
};

static LATIN1_UTF8: [[u8; 3]; 128] = {
    let mut table = [[0u8; 3]; 128];
    let mut ch = 0usize;
    while ch < 128 {
        table[ch] = latin1_utf8(0x80 + ch as u8);
        ch += 1;
    }
    table
};

static mut LATIN1_SINGLETONS: [StaticLatin1; 128] = {
    let mut table = [const { latin1_char(0x80, core::ptr::null_mut()) }; 128];
    let mut ch = 0usize;
    while ch < 128 {
        table[ch] = latin1_char((0x80 + ch) as u8, LATIN1_UTF8[ch].as_ptr() as *mut c_char);
        ch += 1;
    }
    table
};

/// `unicode_get_empty`: a borrowed reference to the empty singleton. The
/// wrapper's header is its first field, so the object address is the block
/// address.
pub(crate) fn empty() -> *mut PyObject {
    // SAFETY: the singletons are never written after init.
    unsafe { core::ptr::addr_of_mut!(EMPTY_STRING.header.ob_base) }
}

/// `get_latin1_char`: the shared singleton for one latin-1 character.
pub(crate) fn latin1_char_object(ch: u8) -> *mut PyObject {
    // SAFETY: the singletons are never written after init.
    unsafe {
        if ch < 0x80 {
            core::ptr::addr_of_mut!(ASCII_SINGLETONS[ch as usize].header.ob_base)
        } else {
            core::ptr::addr_of_mut!(LATIN1_SINGLETONS[(ch - 0x80) as usize].header._base.ob_base)
        }
    }
}

// ---------------------------------------------------------- construction

/// `unicode_result`: replace an empty or one-latin1-character result with
/// the shared singleton, consuming the reference.
pub(crate) fn result(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: `unicode` came from this module's constructors.
    unsafe {
        let length = get_length(unicode);
        if length == 0 {
            let empty = empty();
            if !core::ptr::eq(unicode, empty) {
                free_unicode(unicode);
            }
            return empty;
        }
        if length == 1 && kind(unicode) == PY_UNICODE_1BYTE_KIND {
            let ch = *(data_u8(unicode));
            let shared = latin1_char_object(ch);
            if !core::ptr::eq(unicode, shared) {
                free_unicode(unicode);
            }
            return shared;
        }
        unicode
    }
}

/// Free a string object and its UTF-8 cache. Static singletons are never
/// passed here: `result` routes them away first.
unsafe fn free_unicode(unicode: *mut PyObject) {
    // SAFETY: the string is a heap object this module built and no longer
    // shares.
    unsafe {
        if !(*(unicode as *const PyASCIIObject)).state.compact()
            || !(*(unicode as *const PyASCIIObject)).state.ascii()
        {
            let utf8 = (*(unicode as *mut PyCompactUnicodeObject)).utf8;
            if !utf8.is_null() {
                crate::alloc::pymem::PyMem_Free(utf8.cast());
            }
        }
        crate::alloc::free_block(unicode.cast());
    }
}

/// `PyUnicode_New`: an empty compact string of the smallest kind that
/// holds `maxchar`, data zeroed.
///
/// # Safety
///
/// The result's data must be written before it is read; the terminator is
/// in place.
pub unsafe fn unicode_new(size: isize, maxchar: u32) -> *mut PyObject {
    // SAFETY: allocation and header writes stay within the new block.
    unsafe {
        if maxchar >= MAX_UNICODE {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"invalid maximum character passed to PyUnicode_New",
            );
            return core::ptr::null_mut();
        }
        if size < 0 {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Negative size passed to PyUnicode_New",
            );
            return core::ptr::null_mut();
        }
        let is_ascii = maxchar < 0x80;
        let (struct_size, char_size, kind) = if is_ascii {
            (
                coil_rt_abi::cp312::ascii::SIZEOF,
                1usize,
                PY_UNICODE_1BYTE_KIND,
            )
        } else if maxchar < 0x100 {
            (
                coil_rt_abi::cp312::compact_unicode::SIZEOF,
                1usize,
                PY_UNICODE_1BYTE_KIND,
            )
        } else if maxchar < 0x10000 {
            (
                coil_rt_abi::cp312::compact_unicode::SIZEOF,
                2usize,
                PY_UNICODE_2BYTE_KIND,
            )
        } else {
            (
                coil_rt_abi::cp312::compact_unicode::SIZEOF,
                4usize,
                PY_UNICODE_4BYTE_KIND,
            )
        };
        let data_size = (size as usize)
            .checked_add(1)
            .and_then(|n| n.checked_mul(char_size));
        let Some(total) = data_size.and_then(|n| struct_size.checked_add(n)) else {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        };
        let block = crate::alloc::alloc_object(total);
        if block.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        header::c_init(block.cast(), &raw mut PyUnicode_Type);
        let ascii = block as *mut PyASCIIObject;
        (*ascii).length = size;
        (*ascii).hash = -1;
        (*ascii).state = PyUnicodeState::new(SSTATE_NOT_INTERNED, kind, true, is_ascii);
        if !is_ascii {
            let compact = block as *mut PyCompactUnicodeObject;
            (*compact).utf8 = core::ptr::null_mut();
            (*compact).utf8_length = 0;
        }
        // The terminating null character of the smallest representation.
        let terminator =
            (data(block as *const PyObject) as *mut u8).add((size as usize) * char_size);
        core::ptr::write_bytes(terminator, 0, char_size);
        block.cast()
    }
}

/// `unicode_char`: one code point as a shared latin-1 singleton or a fresh
/// 2/4-byte string.
///
/// # Safety
///
/// `ch` must be at most `MAX_UNICODE`.
pub(crate) unsafe fn from_char(ch: u32) -> *mut PyObject {
    // SAFETY: the kind matches the value written.
    unsafe {
        if ch < 0x100 {
            return latin1_char_object(ch as u8);
        }
        let unicode = unicode_new(1, ch);
        if unicode.is_null() {
            return unicode;
        }
        match kind(unicode) {
            PY_UNICODE_2BYTE_KIND => *(data_u16(unicode)) = ch as u16,
            _ => *(data_u32(unicode)) = ch,
        }
        unicode
    }
}

/// Copy `n` characters between raw buffers of two kinds (CPython's
/// `_PyUnicode_FastCopyCharacters` core). panics never: the callers size
/// the buffers.
///
/// # Safety
///
/// Both buffers must hold `count` characters of their kind.
pub(crate) unsafe fn copy_chars_raw(
    to_data: *mut c_void,
    to_kind: c_int,
    from_data: *const c_void,
    from_kind: c_int,
    count: isize,
) {
    // SAFETY: per the contract, both regions are in bounds.
    unsafe {
        let mut i = 0isize;
        while i < count {
            let ch = read(from_kind, from_data, i);
            match to_kind {
                PY_UNICODE_2BYTE_KIND => *(to_data as *mut u16).offset(i) = ch as u16,
                PY_UNICODE_4BYTE_KIND => *(to_data as *mut u32).offset(i) = ch,
                _ => *(to_data as *mut u8).offset(i) = ch as u8,
            }
            i += 1;
        }
    }
}

/// `_PyUnicode_FastCopyCharacters`: copy a range between two strings.
///
/// # Safety
///
/// Both arguments must be valid string objects with room for the range.
pub unsafe fn fast_copy_characters(
    to: *mut PyObject,
    to_start: isize,
    from: *const PyObject,
    from_start: isize,
    how_many: isize,
) {
    // SAFETY: the range checks are the caller's contract, as in CPython.
    unsafe {
        if how_many == 0 {
            return;
        }
        copy_chars_raw(
            (data(to) as *mut u8).add((to_start * kind(to) as isize) as usize) as *mut c_void,
            kind(to),
            (data(from) as *const u8).add((from_start * kind(from) as isize) as usize)
                as *const c_void,
            kind(from),
            how_many,
        );
    }
}

/// Copy the character data of `unicode` into a fresh string (CPython's
/// `_PyUnicode_Copy`): the same value as an exact `str`, whatever subtype
/// `unicode` belongs to.
///
/// # Safety
///
/// `unicode` must be null or a valid string object.
pub unsafe fn copy(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string; the new buffer is sized by its
    // length.
    unsafe {
        if unicode.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let length = get_length(unicode);
        if length == 0 {
            return empty();
        }
        let fresh = unicode_new(length, max_char_value(unicode));
        if fresh.is_null() {
            return fresh;
        }
        fast_copy_characters(fresh, 0, unicode, 0, length);
        fresh
    }
}

/// The maximum character of a UTF-8 slice, with its byte length; returns
/// `None` with `UnicodeDecodeError` set on invalid input. This is CPython's
/// `utf8_to_utf8` half: it does not reject surrogates, which the strict
/// handler pass below does.
/// The smallest kind's maxchar for a set of characters: 0x7F below the
/// ASCII boundary, else the largest code point.
fn maxchar_of(chars: &[u32]) -> u32 {
    let mut maxchar: u32 = 0;
    for &ch in chars {
        if ch > maxchar {
            maxchar = ch;
        }
    }
    if maxchar < 0x80 { 0x7F } else { maxchar }
}

/// Decode one UTF-8 sequence starting at `s[0]`; `None` on invalid input.
/// The width is the number of bytes consumed.
pub fn utf8_decode_one(s: &[u8]) -> Option<(u32, usize)> {
    if s.is_empty() {
        return None;
    }
    let byte = s[0];
    if byte < 0x80 {
        return Some((byte as u32, 1));
    }
    let need = if byte & 0xE0 == 0xC0 {
        2
    } else if byte & 0xF0 == 0xE0 {
        3
    } else if byte & 0xF8 == 0xF0 {
        4
    } else {
        return None;
    };
    if s.len() < need {
        return None;
    }
    let mut ch = match need {
        2 => (byte & 0x1F) as u32,
        3 => (byte & 0x0F) as u32,
        _ => (byte & 0x07) as u32,
    };
    for &cont in &s[1..need] {
        if cont & 0xC0 != 0x80 {
            return None;
        }
        ch = (ch << 6) | (cont & 0x3F) as u32;
    }
    // Reject overlong encodings and surrogates the way the UTF-8 decoder
    // does (the strict error pass distinguishes the messages).
    let valid_min = match need {
        2 => 0x80,
        3 => 0x800,
        _ => 0x10000,
    };
    if ch < valid_min || ch >= MAX_UNICODE || (0xD800..0xE000).contains(&ch) {
        return None;
    }
    Some((ch, need))
}

/// Encode one code point as UTF-8 into `out`.
pub(crate) fn utf8_encode_one(out: &mut Vec<u8>, ch: u32) {
    if ch < 0x80 {
        out.push(ch as u8);
    } else if ch < 0x800 {
        out.push(0xC0 | (ch >> 6) as u8);
        out.push(0x80 | (ch & 0x3F) as u8);
    } else if ch < 0x10000 {
        out.push(0xE0 | (ch >> 12) as u8);
        out.push(0x80 | ((ch >> 6) & 0x3F) as u8);
        out.push(0x80 | (ch & 0x3F) as u8);
    } else {
        out.push(0xF0 | (ch >> 18) as u8);
        out.push(0x80 | ((ch >> 12) & 0x3F) as u8);
        out.push(0x80 | ((ch >> 6) & 0x3F) as u8);
        out.push(0x80 | (ch & 0x3F) as u8);
    }
}

/// The character content of a string as a Rust UTF-8 `String`. Allocation
/// is the price of the boundary with Rust code that needs `&str`; the
/// object's own UTF-8 cache serves the same need for the C ABI.
///
/// # Safety
///
/// `op` must point to a valid string object.
pub unsafe fn as_rust(op: *mut PyObject) -> String {
    // SAFETY: reads stay within the string.
    unsafe {
        let length = get_length(op);
        let object_kind = kind(op);
        let raw = data(op);
        let mut out = String::with_capacity(length as usize);
        for i in 0..length {
            let ch = read(object_kind, raw, i);
            out.push(char::from_u32(ch).unwrap_or('\u{FFFD}'));
        }
        out
    }
}

/// Build a string object from Rust characters, sharing singletons.
pub(crate) fn from_rust(s: &str) -> *mut PyObject {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => {
            // SAFETY: a Rust char is at most MAX_UNICODE - 1.
            return unsafe { from_char(ch as u32) };
        }
        (None, _) => return empty(),
        _ => {}
    }
    let maxchar = s.chars().map(|c| c as u32).max().unwrap_or(0);
    // SAFETY: the new buffer is exactly `s.chars().count()` characters and
    // is fully written below.
    let unicode = unsafe { unicode_new(s.chars().count() as isize, maxchar) };
    if unicode.is_null() {
        return unicode;
    }
    // SAFETY: the buffer has room for every character.
    unsafe {
        let object_kind = kind(unicode);
        let raw = data(unicode);
        for (index, ch) in s.chars().enumerate() {
            match object_kind {
                PY_UNICODE_2BYTE_KIND => *(raw as *mut u16).add(index) = ch as u32 as u16,
                PY_UNICODE_4BYTE_KIND => *(raw as *mut u32).add(index) = ch as u32,
                _ => *(raw as *mut u8).add(index) = ch as u32 as u8,
            }
        }
    }
    unicode
}

/// The current per-process SipHash key, drawing it if unset; the `bytes`
/// hasher shares it (one key process-wide, as in CPython).
pub fn current_hash_key() -> (u64, u64) {
    hash_key()
}

/// SipHash-1-3 over `bytes` under an explicit key.
pub(crate) fn hash_bytes_with(key: (u64, u64), bytes: &[u8]) -> u64 {
    siphash13(key.0, key.1, bytes)
}

// -------------------------------------------------------------- interning

/// The interned strings, keyed by content. The table holds strong
/// references: interned strings live for the process (spec section 9.2
/// roots the interned table with the module table).
fn interned_table() -> &'static std::sync::Mutex<std::collections::HashMap<Vec<u8>, usize>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<Vec<u8>, usize>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// The table's key for a string: its UTF-8 bytes, computed through the
/// same path `PyUnicode_AsUTF8AndSize` uses.
///
/// # Safety
///
/// `op` must point to a valid string object.
unsafe fn intern_key(op: *mut PyObject) -> Vec<u8> {
    // SAFETY: the read is within the string.
    unsafe {
        // The empty and one-character singletons are their own identity;
        // keys of interned content never collide with a distinct string.
        as_rust(op).into_bytes()
    }
}

/// `PyUnicode_InternInPlace`: intern `p`, replacing a heap duplicate with
/// the table's copy. Mortal interning keeps the object refcount-managed
/// while the table holds it.
///
/// # Safety
///
/// `p` must point to a valid string object pointer.
pub unsafe fn intern_in_place(p: *mut *mut PyObject) {
    // SAFETY: the string reads are within the object; the table is
    // process-global.
    unsafe {
        let unicode = *p;
        if !check_exact(unicode) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return;
        }
        let state_interned = (*(unicode as *const PyASCIIObject)).state.interned();
        if state_interned != SSTATE_NOT_INTERNED {
            return;
        }
        let key = intern_key(unicode);
        let mut table = interned_table().lock().expect("interned table poisoned");
        if let Some(existing) = table.get(&key).copied() {
            let existing = existing as *mut PyObject;
            if !core::ptr::eq(existing, unicode) {
                *p = existing;
                free_unicode(unicode);
                return;
            }
        }
        table.insert(key, unicode as usize);
        (*(unicode as *mut PyASCIIObject))
            .state
            .set_interned(SSTATE_INTERNED_MORTAL);
    }
}

/// `_PyUnicode_InternImmortal`: intern and make immortal (the identifier
/// path, spec section 8.1).
///
/// # Safety
///
/// `p` must point to a valid string object pointer.
pub unsafe fn intern_immortal(p: *mut *mut PyObject) {
    // SAFETY: the string reads are within the object.
    unsafe {
        intern_in_place(p);
        let unicode = *p;
        (*(unicode as *mut PyASCIIObject))
            .state
            .set_interned(SSTATE_INTERNED_IMMORTAL);
        (*unicode).ob_refcnt = header::IMMORTAL_REFCNT;
    }
}

/// `PyUnicode_InternFromString`: a new reference to an interned copy of
/// `text`.
///
/// # Safety
///
/// `text` must be null or a null-terminated C string.
pub unsafe fn intern_from_string(text: *const c_char) -> *mut PyObject {
    // SAFETY: the call only reads the C string.
    unsafe {
        if text.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let cstr = core::ffi::CStr::from_ptr(text);
        let unicode = from_rust(core::str::from_utf8(cstr.to_bytes()).unwrap_or("\u{FFFD}"));
        let mut p = unicode;
        intern_in_place(&mut p);
        incref(p);
        p
    }
}

/// `Py_NewRef` for this runtime's reference conventions: immortal objects
/// ignore counts, heap objects gain one.
pub(crate) fn incref(object: *mut PyObject) {
    // SAFETY: the header write is within the object.
    unsafe {
        if (*object).ob_refcnt != header::IMMORTAL_REFCNT {
            (*object).ob_refcnt += 1;
        }
    }
}

/// The reverse: drop one reference, handing a heap object at zero to its
/// type's `tp_dealloc` the way `_Py_Dealloc` does.
pub(crate) fn decref(object: *mut PyObject) {
    // SAFETY: the header read and the slot dispatch are within the object.
    unsafe {
        if (*object).ob_refcnt == header::IMMORTAL_REFCNT {
            return;
        }
        (*object).ob_refcnt -= 1;
        if (*object).ob_refcnt == 0
            && let Some(dealloc) = (*(*object).ob_type).tp_dealloc
        {
            dealloc(object);
        }
    }
}

// ------------------------------------------------------- the UTF-8 cache

/// `PyUnicode_AsUTF8AndSize`: the UTF-8 form of the string, computing and
/// caching it for heap non-ASCII strings. The returned pointer stays valid
/// for the string's lifetime.
///
/// # Safety
///
/// `op` must point to a valid string object; `size` may be null.
pub unsafe fn as_utf8_and_size(op: *mut PyObject, size: *mut isize) -> *const c_char {
    // SAFETY: the reads and the cache write are within the string; the
    // cache is immutable once written, and the runtime keeps the GIL (v1),
    // so a plain write cannot tear.
    unsafe {
        if check_exact(op) && (*(op as *const PyASCIIObject)).state.ascii() {
            if !size.is_null() {
                *size = get_length(op);
            }
            return data(op) as *const c_char;
        }
        let length = get_length(op);
        let cache = op as *mut PyCompactUnicodeObject;
        if (*cache).utf8.is_null() {
            let mut bytes = Vec::with_capacity(length as usize);
            let object_kind = kind(op);
            let raw = data(op);
            for i in 0..length {
                utf8_encode_one(&mut bytes, read(object_kind, raw, i));
            }
            bytes.push(0);
            let stored = crate::alloc::pymem::PyMem_Malloc(bytes.len());
            if stored.is_null() {
                crate::tstate::set_string(exceptions::memory_error(), b"");
                return core::ptr::null();
            }
            // SAFETY: `stored` holds at least `bytes.len()` writable bytes.
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), stored.cast(), bytes.len());
            (*cache).utf8 = stored.cast();
            (*cache).utf8_length = bytes.len() as isize - 1;
        }
        if !size.is_null() {
            *size = (*cache).utf8_length;
        }
        (*cache).utf8 as *const c_char
    }
}

/// The UTF-8 bytes of a string as a Rust slice of the cached buffer.
///
/// # Safety
///
/// `op` must point to a valid string object.
pub unsafe fn as_utf8_vec(op: *mut PyObject) -> Vec<u8> {
    // SAFETY: the pointer and size describe the cached buffer.
    unsafe {
        let mut size = 0isize;
        let utf8 = as_utf8_and_size(op, &mut size);
        if utf8.is_null() {
            return Vec::new();
        }
        core::slice::from_raw_parts(utf8.cast::<u8>(), size as usize).to_vec()
    }
}

// ------------------------------------------------- equality and ordering

/// `unicode_compare_eq`: byte equality of the canonical buffers.
///
/// # Safety
///
/// Both arguments must be valid string objects.
pub(crate) unsafe fn compare_eq(str1: *const PyObject, str2: *const PyObject) -> bool {
    // SAFETY: all reads are within the two strings.
    unsafe {
        let len = get_length(str1);
        if get_length(str2) != len {
            return false;
        }
        let object_kind = kind(str1);
        if kind(str2) != object_kind {
            return false;
        }
        core::slice::from_raw_parts(
            data(str1) as *const u8,
            (len * object_kind as isize) as usize,
        ) == core::slice::from_raw_parts(
            data(str2) as *const u8,
            (len * object_kind as isize) as usize,
        )
    }
}

/// `_PyUnicode_EQ`.
///
/// # Safety
///
/// Both arguments must be valid string objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EQ(str1: *mut PyObject, str2: *mut PyObject) -> c_int {
    // SAFETY: both arguments are strings per the contract.
    unsafe { compare_eq(str1, str2) as c_int }
}

/// `_PyUnicode_Equal`.
///
/// # Safety
///
/// Both arguments must be valid string objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_Equal(str1: *mut PyObject, str2: *mut PyObject) -> c_int {
    if core::ptr::eq(str1, str2) {
        return 1;
    }
    // SAFETY: both arguments are strings per the contract.
    unsafe { compare_eq(str1, str2) as c_int }
}

/// `unicode_compare`: code-point lexicographic order.
///
/// # Safety
///
/// Both arguments must be valid string objects.
pub(crate) unsafe fn compare(left: *const PyObject, right: *const PyObject) -> c_int {
    // SAFETY: all reads are within the two strings.
    unsafe {
        let len1 = get_length(left);
        let len2 = get_length(right);
        let kind1 = kind(left);
        let kind2 = kind(right);
        let data1 = data(left);
        let data2 = data(right);
        let mut i = 0isize;
        while i < len1 && i < len2 {
            let c1 = read(kind1, data1, i);
            let c2 = read(kind2, data2, i);
            if c1 != c2 {
                return if c1 < c2 { -1 } else { 1 };
            }
            i += 1;
        }
        match (len1 - len2).signum() {
            -1 => -1,
            0 => 0,
            _ => 1,
        }
    }
}

/// `PyUnicode_Compare`.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Compare(left: *mut PyObject, right: *mut PyObject) -> c_int {
    // SAFETY: both are valid objects; strings are checked before reads.
    unsafe {
        if check(left) && check(right) {
            if core::ptr::eq(left, right) {
                return 0;
            }
            return compare(left, right);
        }
        let name1 = util::cstr((*header::type_of(left)).tp_name);
        let name2 = util::cstr((*header::type_of(right)).tp_name);
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("Can't compare {name1} and {name2}").as_bytes(),
        );
        -1
    }
}

/// `PyUnicode_CompareWithASCIIString`.
///
/// # Safety
///
/// `uni` must be a valid string; `str` a null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_CompareWithASCIIString(
    uni: *mut PyObject,
    str_: *const c_char,
) -> c_int {
    // SAFETY: both operands satisfy their contracts.
    unsafe {
        let other = core::ffi::CStr::from_ptr(str_).to_bytes();
        let len = get_length(uni);
        let object_kind = kind(uni);
        let raw = data(uni);
        let mut i = 0usize;
        while i < len as usize && i < other.len() {
            let ch = read(object_kind, raw, i as isize);
            if ch != other[i] as u32 {
                return if ch < other[i] as u32 { -1 } else { 1 };
            }
            i += 1;
        }
        if len as usize > other.len() {
            return 1;
        }
        if (len as usize) < other.len() {
            return -1;
        }
        0
    }
}

/// `_PyUnicode_EqualToASCIIString`.
///
/// # Safety
///
/// `unicode` must be a valid string; `str` a null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EqualToASCIIString(
    unicode: *mut PyObject,
    str_: *const c_char,
) -> c_int {
    // SAFETY: both operands satisfy their contracts.
    unsafe {
        let other = core::ffi::CStr::from_ptr(str_).to_bytes();
        if !(*(unicode as *const PyASCIIObject)).state.ascii() {
            return 0;
        }
        (get_length(unicode) as usize == other.len()
            && core::slice::from_raw_parts(data(unicode) as *const u8, other.len()) == other)
            as c_int
    }
}

/// `PyUnicode_RichCompare`.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_RichCompare(
    left: *mut PyObject,
    right: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: both operands are valid objects; strings are checked.
    unsafe {
        if !check(left) || !check(right) {
            return singletons::not_implemented();
        }
        if core::ptr::eq(left, right) {
            return match op {
                CMP_EQ | Py_LE | Py_GE => singletons::true_(),
                CMP_NE | Py_LT | Py_GT => singletons::false_(),
                _ => {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    core::ptr::null_mut()
                }
            };
        }
        if op == CMP_EQ || op == CMP_NE {
            let result = compare_eq(left, right) ^ (op == CMP_NE);
            return if result {
                singletons::true_()
            } else {
                singletons::false_()
            };
        }
        let result = compare(left, right);
        let truth = match op {
            Py_LT => result < 0,
            Py_LE => result <= 0,
            Py_GT => result > 0,
            Py_GE => result >= 0,
            _ => {
                crate::tstate::set_string(exceptions::system_error(), b"");
                return core::ptr::null_mut();
            }
        };
        if truth {
            singletons::true_()
        } else {
            singletons::false_()
        }
    }
}

/// Return `op` as a shared exact string: incref it when it already is
/// one, copy it when it is a subclass instance.
///
/// # Safety
///
/// `op` must be a valid string object.
pub(crate) unsafe fn shared(op: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a valid string.
    unsafe {
        if check_exact(op) {
            incref(op);
            op
        } else {
            copy(op)
        }
    }
}

/// `PyUnicode_Concat`.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Concat(
    left: *mut PyObject,
    right: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: both operands are checked before their layout is read.
    unsafe {
        if !check(left) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"PyUnicode_Concat: unexpected non-string left operand",
            );
            return core::ptr::null_mut();
        }
        if !check(right) {
            let name = util::cstr((*header::type_of(right)).tp_name);
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("can only concatenate str (not \"{name}\") to str").as_bytes(),
            );
            return core::ptr::null_mut();
        }
        if core::ptr::eq(left, empty()) {
            return shared(right);
        }
        if core::ptr::eq(right, empty()) {
            return shared(left);
        }
        let left_len = get_length(left);
        let right_len = get_length(right);
        let Some(new_len) = left_len.checked_add(right_len) else {
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"strings are too large to concat",
            );
            return core::ptr::null_mut();
        };
        let maxchar = max_char_value(left).max(max_char_value(right));
        let result = unicode_new(new_len, maxchar);
        if result.is_null() {
            return result;
        }
        fast_copy_characters(result, 0, left, 0, left_len);
        fast_copy_characters(result, left_len, right, 0, right_len);
        result
    }
}

/// `PyUnicode_Contains`.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Contains(str_: *mut PyObject, substr: *mut PyObject) -> c_int {
    // SAFETY: operands are checked before layout reads.
    unsafe {
        if !check(substr) {
            let name = util::cstr((*header::type_of(substr)).tp_name);
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("'in <string>' requires string as left operand, not {name}").as_bytes(),
            );
            return -1;
        }
        let Some(hay) = as_chars(str_) else {
            return -1;
        };
        let Some(needle) = as_chars(substr) else {
            return -1;
        };
        (find(hay, needle, 0) != -1) as c_int
    }
}

/// A borrowed character view of any string object, the moral equivalent of
/// a `(kind, data, length)` triple in CPython's loops.
///
/// # Safety
///
/// `op` must be a valid string object.
pub unsafe fn as_chars(op: *const PyObject) -> Option<Chars> {
    if op.is_null() {
        return None;
    }
    Some(Chars {
        object_kind: unsafe { kind(op) },
        raw: unsafe { data(op) },
        len: unsafe { get_length(op) },
    })
}

#[derive(Clone, Copy)]
pub struct Chars {
    pub object_kind: c_int,
    pub raw: *const c_void,
    pub len: isize,
}

/// A zero-length view; reads never touch the data pointer.
pub(crate) const EMPTY_CHARS: Chars = Chars {
    object_kind: PY_UNICODE_1BYTE_KIND,
    raw: core::ptr::null(),
    len: 0,
};

impl Chars {
    pub fn get(&self, index: isize) -> u32 {
        // SAFETY: callers keep the index within `len`.
        unsafe { read(self.object_kind, self.raw, index) }
    }

    pub fn len(&self) -> isize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A slice `[start, end)` as owned characters.
    pub fn slice(&self, start: isize, end: isize) -> Vec<u32> {
        (start..end).map(|i| self.get(i)).collect()
    }

    pub fn as_vec(&self) -> Vec<u32> {
        self.slice(0, self.len)
    }
}

/// The lowest `i >= start` with `hay[i..].starts_with(needle)`, else -1.
pub(crate) fn find(hay: Chars, needle: Chars, start: isize) -> isize {
    let n = needle.len();
    let end = hay.len() - n;
    let mut i = start.max(0);
    while i <= end {
        let mut j = 0isize;
        while j < n && hay.get(i + j) == needle.get(j) {
            j += 1;
        }
        if j == n {
            return i;
        }
        i += 1;
    }
    -1
}

/// The highest `i <= start` with `hay[i..].starts_with(needle)`, else -1
/// (CPython's `rfind` scan).
pub(crate) fn rfind(hay: Chars, needle: Chars, start: isize) -> isize {
    let n = needle.len();
    let mut i = start.min(hay.len() - n);
    while i >= 0 {
        let mut j = 0isize;
        while j < n && hay.get(i + j) == needle.get(j) {
            j += 1;
        }
        if j == n {
            return i;
        }
        i -= 1;
    }
    -1
}

/// `PyUnicode_Find`: search `sub` within `str[start:end]`.
///
/// # Safety
///
/// `str_` and `sub` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Find(
    str_: *mut PyObject,
    sub: *mut PyObject,
    start: isize,
    end: isize,
    direction: c_int,
) -> isize {
    // SAFETY: operands are checked before layout reads.
    unsafe {
        let hay = match as_chars(str_) {
            Some(hay) => hay,
            None => return -2,
        };
        let needle = match as_chars(sub) {
            Some(needle) => needle,
            None => return -2,
        };
        let (start, end) = adjust_indices(hay.len(), start, end);
        // Searching the `str[start:end]` view bounds the scan by `end`.
        let view = Chars {
            object_kind: hay.object_kind,
            raw: hay.raw,
            len: end,
        };
        if direction < 0 {
            let i = rfind(view, needle, end - needle.len());
            if i < start { -1 } else { i }
        } else {
            find(view, needle, start)
        }
    }
}

/// `PyUnicode_FindChar`: search one character.
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FindChar(
    str_: *mut PyObject,
    ch: u32,
    start: isize,
    end: isize,
    direction: c_int,
) -> isize {
    // SAFETY: reads stay within the string.
    unsafe {
        let hay = match as_chars(str_) {
            Some(hay) => hay,
            None => return -2,
        };
        let (start, end) = adjust_indices(hay.len(), start, end);
        let mut i = if direction < 0 { end - 1 } else { start };
        while if direction < 0 { i >= start } else { i < end } {
            if hay.get(i) == ch {
                return i;
            }
            i += if direction < 0 { -1 } else { 1 };
        }
        -1
    }
}

/// CPython's `ADJUST_INDICES` macro: `end` clamps to `[0, len]` the
/// slice way, `start` only gains negative adjustment — a `start` past
/// the end leaves an empty (or negative) range that callers test for.
pub(crate) fn adjust_indices(len: isize, start: isize, end: isize) -> (isize, isize) {
    let end = if end > len {
        len
    } else if end < 0 {
        let e = len + end;
        if e < 0 { 0 } else { e }
    } else {
        end
    };
    let start = if start < 0 {
        let s = len + start;
        if s < 0 { 0 } else { s }
    } else {
        start
    };
    (start, end)
}

/// `PyUnicode_Count`: non-overlapping occurrences in `str[start:end]`.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Count(
    str_: *mut PyObject,
    sub: *mut PyObject,
    start: isize,
    end: isize,
) -> isize {
    // SAFETY: operands are checked before layout reads.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return -1;
        };
        let Some(needle) = as_chars(sub) else {
            return -1;
        };
        let (start, end) = adjust_indices(hay.len(), start, end);
        let mut count = 0isize;
        let mut i = start;
        while i + needle.len() <= end {
            let found = find(
                Chars {
                    object_kind: hay.object_kind,
                    raw: hay.raw,
                    len: end,
                },
                needle,
                i,
            );
            if found == -1 {
                break;
            }
            count += 1;
            i = found + needle.len().max(1);
        }
        count
    }
}

/// `PyUnicode_Tailmatch`: `startswith`/`endswith` over a range.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Tailmatch(
    str_: *mut PyObject,
    substr: *mut PyObject,
    start: isize,
    end: isize,
    direction: c_int,
) -> c_int {
    // SAFETY: operands are checked before layout reads.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return -1;
        };
        let Some(needle) = as_chars(substr) else {
            return -1;
        };
        let (start, end) = adjust_indices(hay.len(), start, end);
        if direction < 0 {
            // endswith
            if end - start < needle.len() {
                return 0;
            }
            let offset = end - needle.len();
            let mut j = 0isize;
            while j < needle.len() {
                if hay.get(offset + j) != needle.get(j) {
                    return 0;
                }
                j += 1;
            }
            1
        } else {
            if end - start < needle.len() {
                return 0;
            }
            let mut j = 0isize;
            while j < needle.len() {
                if hay.get(start + j) != needle.get(j) {
                    return 0;
                }
                j += 1;
            }
            1
        }
    }
}

/// `PyUnicode_Replace`: replace non-overlapping `sub` with `repl` in
/// `str[start:end]`; `count` of -1 means all.
///
/// # Safety
///
/// All three arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Replace(
    str_: *mut PyObject,
    sub: *mut PyObject,
    repl: *mut PyObject,
    count: isize,
) -> *mut PyObject {
    // SAFETY: operands are checked before layout reads.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return core::ptr::null_mut();
        };
        let Some(needle) = as_chars(sub) else {
            return core::ptr::null_mut();
        };
        let Some(replace) = as_chars(repl) else {
            return core::ptr::null_mut();
        };
        let maxchar = max_char_value(str_).max(max_char_value(repl));
        replace_all(hay, needle, replace, count, maxchar)
    }
}

/// The `stringlib` replace over character views, producing a fresh object.
pub(crate) fn replace_all(
    hay: Chars,
    needle: Chars,
    repl: Chars,
    count: isize,
    maxchar: u32,
) -> *mut PyObject {
    let length = hay.len();
    if needle.is_empty() {
        // An empty pattern matches at every boundary between characters:
        // `count` copies of the replacement go in first.
        let boundaries = length + 1;
        let limit = if count < 0 {
            boundaries
        } else {
            count.min(boundaries)
        };
        let mut out: Vec<u32> = Vec::with_capacity((length + limit.max(0) * repl.len()) as usize);
        for i in 0..=length {
            if i < limit {
                out.extend(repl.as_vec());
            }
            if i < length {
                out.push(hay.get(i));
            }
        }
        return chars_to_object(&out, maxchar);
    }
    let limit = if count < 0 { isize::MAX } else { count };
    let mut pieces: Vec<Vec<u32>> = Vec::new();
    let mut i = 0isize;
    let mut done = 0isize;
    let mut last = 0isize;
    while done < limit {
        let found = find(hay, needle, i);
        if found == -1 {
            break;
        }
        pieces.push(hay.slice(last, found));
        i = found + needle.len();
        last = i;
        done += 1;
    }
    pieces.push(hay.slice(last, length));
    let new_len: isize = pieces.iter().map(|p| p.len() as isize).sum::<isize>() + done * repl.len();
    let mut out: Vec<u32> = Vec::with_capacity(new_len.max(0) as usize);
    for (index, piece) in pieces.iter().enumerate() {
        out.extend_from_slice(piece);
        if index + 1 < pieces.len() {
            out.extend(repl.as_vec());
        }
    }
    chars_to_object(&out, maxchar)
}

/// A fresh string object from owned characters, honouring singletons.
/// A `str` object from UTF-8 text; the `tp_repr` slots use this.
pub(crate) fn str_from_utf8(text: &str) -> *mut PyObject {
    let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
    chars_to_object(&chars, maxchar_of(&chars))
}

pub(crate) fn chars_to_object(chars: &[u32], maxchar: u32) -> *mut PyObject {
    if chars.is_empty() {
        return empty();
    }
    if chars.len() == 1 && chars[0] < 0x100 {
        // SAFETY: the value is a code point.
        return unsafe { from_char(chars[0]) };
    }
    let maxchar = maxchar.max(chars.iter().copied().max().unwrap_or(0));
    // SAFETY: the new buffer is exactly `chars.len()` characters.
    let unicode = unsafe { unicode_new(chars.len() as isize, maxchar) };
    if unicode.is_null() {
        return unicode;
    }
    // SAFETY: the buffer holds exactly `chars.len()` slots.
    unsafe {
        let object_kind = kind(unicode);
        let raw = data(unicode);
        match object_kind {
            PY_UNICODE_2BYTE_KIND => {
                for (i, ch) in chars.iter().enumerate() {
                    *(raw as *mut u16).add(i) = *ch as u16;
                }
            }
            PY_UNICODE_4BYTE_KIND => {
                for (i, ch) in chars.iter().enumerate() {
                    *(raw as *mut u32).add(i) = *ch;
                }
            }
            _ => {
                for (i, ch) in chars.iter().enumerate() {
                    *(raw as *mut u8).add(i) = *ch as u8;
                }
            }
        }
    }
    unicode
}

/// A fresh copy of `hay` as an object (CPython's `_PyUnicode_Copy` path).
fn copy_chars_object(hay: Chars, maxchar: u32) -> *mut PyObject {
    let chars = hay.as_vec();
    chars_to_object(&chars, maxchar)
}

/// `PyUnicode_Substring`: `str[start:end]` as a fresh object.
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Substring(
    str_: *mut PyObject,
    start: isize,
    end: isize,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return core::ptr::null_mut();
        };
        // CPython clamps only `start >= 0` and `end <= len` here.
        let len = hay.len();
        let start = start.max(0);
        let end = end.min(len);
        if start == 0 && end == len && check_exact(str_) {
            incref(str_);
            return str_;
        }
        if end <= start {
            return empty();
        }
        copy_chars_object(
            Chars {
                object_kind: hay.object_kind,
                raw: hay.raw,
                len: end,
            }
            .offset_view(start),
            max_char_value(str_),
        )
    }
}

impl Chars {
    /// The view rebased at `start` (bounds remain the caller's duty).
    pub fn offset_view(&self, start: isize) -> Chars {
        Chars {
            object_kind: self.object_kind,
            // SAFETY: `start` is within the view.
            raw: unsafe {
                (self.raw as *const u8).add((start * self.object_kind as isize) as usize)
                    as *const c_void
            },
            len: self.len - start,
        }
    }
}

/// `str[i]` through the subscript slot: an index in range, one character.
///
/// # Safety
///
/// `self_` must be a valid string object; `key` a valid object.
pub(crate) unsafe fn subscript(self_: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    // SAFETY: all reads are within the operands.
    unsafe {
        let Some(index) = long_index(key) else {
            return core::ptr::null_mut();
        };
        let length = get_length(self_);
        let index = if index < 0 { length + index } else { index };
        if index < 0 || index >= length {
            crate::tstate::set_string(exceptions::index_error(), b"string index out of range");
            return core::ptr::null_mut();
        }
        let ch = as_chars(self_).map(|c| c.get(index)).unwrap_or(0);
        from_char(ch)
    }
}

/// `PyNumber_AsSsize_t` for the index paths: an exact int in range, else
/// the error set and `None` returned.
///
/// # Safety
///
/// `key` must be a valid object.
pub(crate) unsafe fn long_index(key: *mut PyObject) -> Option<isize> {
    // SAFETY: the value read is within the int object.
    unsafe {
        let ty = header::type_of(key);
        if crate::types::long::is_long(key) {
            // Every index this runtime can hold fits: the int type's own
            // conversion decides overflow.
            let value = crate::types::long::PyLong_AsLongLong(key);
            if crate::tstate::occurred().is_null() {
                return Some(value as isize);
            }
            // OverflowError from the conversion is replaced by CPython's
            // index message.
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"cannot fit 'int' into an index-sized integer",
            );
            return None;
        }
        let name = util::cstr((*ty).tp_name);
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("string indices must be integers, not '{name}'").as_bytes(),
        );
        None
    }
}

/// `sq_repeat`/`PyUnicode repeat`: `n` copies.
///
/// # Safety
///
/// `str_` must be a valid string object.
pub(crate) unsafe fn repeat(str_: *const PyObject, n: isize) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let length = get_length(str_);
        if length == 0 || n <= 0 {
            return empty();
        }
        if length > isize::MAX / n {
            crate::tstate::set_string(exceptions::overflow_error(), b"repeated string is too long");
            return core::ptr::null_mut();
        }
        let Some(total) = length.checked_mul(n) else {
            crate::tstate::set_string(exceptions::overflow_error(), b"repeated string is too long");
            return core::ptr::null_mut();
        };
        let result = unicode_new(total, max_char_value(str_));
        if result.is_null() {
            return result;
        }
        // Copy once, then double the prefix — CPython's trick.
        fast_copy_characters(result, 0, str_, 0, length);
        let mut copied = length;
        while copied < total {
            let take = copied.min(total - copied);
            fast_copy_characters(result, copied, result, 0, take);
            copied += take;
        }
        result
    }
}

/// The join core: items already checked to be strings.
///
/// # Safety
///
/// Every item must be a valid string object.
pub unsafe fn join_items(sep: *mut PyObject, items: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: all reads are within the strings.
    unsafe {
        if items.is_empty() {
            return empty();
        }
        if items.len() == 1 {
            return shared(items[0]);
        }
        let mut total = (items.len() as isize - 1) * get_length(sep);
        let mut maxchar = max_char_value(sep);
        for item in items {
            total += get_length(*item);
            if total < 0 {
                crate::tstate::set_string(
                    exceptions::overflow_error(),
                    b"join() result is too long",
                );
                return core::ptr::null_mut();
            }
            maxchar = maxchar.max(max_char_value(*item));
        }
        let result = unicode_new(total, maxchar);
        if result.is_null() {
            return result;
        }
        let mut offset = 0isize;
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                fast_copy_characters(result, offset, sep, 0, get_length(sep));
                offset += get_length(sep);
            }
            fast_copy_characters(result, offset, *item, 0, get_length(*item));
            offset += get_length(*item);
        }
        result
    }
}

// ---------------------------------------------------------- split family

/// Split into character vectors around `sep` (`None` splits on whitespace
/// runs), honouring `maxsplit` (-1: unlimited) from the left or right.
pub fn split_chars(
    hay: Chars,
    sep: Option<Chars>,
    maxsplit: isize,
    from_right: bool,
) -> Option<Vec<Vec<u32>>> {
    let length = hay.len();
    let mut pieces: Vec<Vec<u32>> = Vec::new();
    match sep {
        None => {
            // Whitespace runs; i walks the string, skipping runs.
            let mut i = 0isize;
            let mut splits_left = if maxsplit < 0 { isize::MAX } else { maxsplit };
            if from_right {
                let mut end = length;
                while end > 0 && is_space(hay.get(end - 1)) {
                    end -= 1;
                }
                let mut start = end;
                while start > 0 && splits_left > 0 {
                    let ch = hay.get(start - 1);
                    if is_space(ch) {
                        pieces.push(hay.slice(start, end));
                        splits_left -= 1;
                        end = start;
                        while end > 0 && is_space(hay.get(end - 1)) {
                            end -= 1;
                        }
                        start = end;
                    } else {
                        start -= 1;
                    }
                }
                // The remainder is everything left of `end`, with its
                // own trailing whitespace skipped.
                while end > 0 && is_space(hay.get(end - 1)) {
                    end -= 1;
                }
                if end > 0 {
                    pieces.push(hay.slice(0, end));
                }
                pieces.reverse();
            } else {
                while i < length && is_space(hay.get(i)) {
                    i += 1;
                }
                let mut start = i;
                while i < length {
                    if splits_left == 0 {
                        break;
                    }
                    if is_space(hay.get(i)) {
                        pieces.push(hay.slice(start, i));
                        splits_left -= 1;
                        while i < length && is_space(hay.get(i)) {
                            i += 1;
                        }
                        start = i;
                    } else {
                        i += 1;
                    }
                }
                if start < length {
                    pieces.push(hay.slice(start, length));
                }
            }
        }
        Some(sep) => {
            if sep.is_empty() {
                crate::tstate::set_string(exceptions::value_error(), b"empty separator");
                return None;
            }
            if from_right {
                let mut end = length;
                let mut splits_left = if maxsplit < 0 { isize::MAX } else { maxsplit };
                while splits_left > 0 {
                    let found = rfind(hay, sep, end - sep.len());
                    if found == -1 {
                        break;
                    }
                    pieces.push(hay.slice(found + sep.len(), end));
                    end = found;
                    splits_left -= 1;
                }
                pieces.push(hay.slice(0, end));
                pieces.reverse();
            } else {
                let mut start = 0isize;
                let mut splits_left = if maxsplit < 0 { isize::MAX } else { maxsplit };
                while splits_left > 0 {
                    let found = find(hay, sep, start);
                    if found == -1 {
                        break;
                    }
                    pieces.push(hay.slice(start, found));
                    start = found + sep.len();
                    splits_left -= 1;
                }
                pieces.push(hay.slice(start, length));
            }
        }
    }
    Some(pieces)
}

/// `Py_UNICODE_ISSPACE`: the narrow `IsWhitespace` table plus C0 controls
/// 0x1C..0x1F and the ASCII block below 0x20 (see `_Py_ascii_whitespace`).
pub(crate) fn is_space(ch: u32) -> bool {
    if ch < 128 {
        matches!(ch, 0x09..=0x0D | 0x1C..=0x1F | 0x20)
    } else {
        unicode_data::WHITESPACE.binary_search(&ch).is_ok()
    }
}

/// `str.splitlines(keepends=False)` over the linebreak table.
pub fn splitlines(hay: Chars, keepends: bool) -> Vec<Vec<u32>> {
    let length = hay.len();
    let mut pieces: Vec<Vec<u32>> = Vec::new();
    let mut start = 0isize;
    let mut i = 0isize;
    while i < length {
        let ch = hay.get(i);
        if is_linebreak(ch) {
            // \r\n is one boundary.
            let mut end = i + 1;
            if ch == 0x0D && i + 1 < length && hay.get(i + 1) == 0x0A {
                end += 1;
            }
            pieces.push(hay.slice(start, if keepends { end } else { i }));
            i = end;
            start = i;
        } else {
            i += 1;
        }
    }
    if start < length {
        pieces.push(hay.slice(start, length));
    }
    pieces
}

/// `Py_UNICODE_ISLINEBREAK`: the character-type table's set.
pub fn is_linebreak(ch: u32) -> bool {
    unicode_data::LINEBREAK.binary_search(&ch).is_ok()
}

/// `str.partition`/`rpartition` as three pieces `(head, sep, tail)`; a
/// miss yields `(whole, "", "")` forward and `("", "", whole)` reversed.
/// The method table wraps these in tuples once the container task lands.
pub fn partition_pieces(
    hay: &[u32],
    sep: &[u32],
    from_right: bool,
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let hay_view = chars_from_owned(hay);
    let sep_view = chars_from_owned(sep);
    let length = hay_view.len;
    let found = if from_right {
        rfind(hay_view, sep_view, length - sep_view.len)
    } else {
        find(hay_view, sep_view, 0)
    };
    if found == -1 {
        return if from_right {
            (Vec::new(), Vec::new(), hay.to_vec())
        } else {
            (hay.to_vec(), Vec::new(), Vec::new())
        };
    }
    let head = hay[0..found as usize].to_vec();
    let middle = sep.to_vec();
    let tail = hay[(found as usize) + sep.len()..].to_vec();
    (head, middle, tail)
}

/// A `Chars` view over owned characters (kind 4): the search helpers work
/// on views, so the piece-based methods borrow one without an object.
pub fn chars_from_owned(chars: &[u32]) -> Chars {
    // The slice outlives the view by the caller's borrow; the raw pointer
    // is never written through.
    Chars {
        object_kind: PY_UNICODE_4BYTE_KIND,
        raw: chars.as_ptr() as *const c_void,
        len: chars.len() as isize,
    }
}

// ------------------------------------------------- character-type database

/// `_PyUnicode_ToDecimalDigit`: the digit value 0-9 of a decimal
/// character, -1 otherwise.
pub fn to_decimal_digit(ch: u32) -> i32 {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::DECIMAL != 0 {
        ctype.decimal as i32
    } else {
        -1
    }
}

/// `_PyUnicode_IsDecimalDigit`.
pub fn is_decimal_digit(ch: u32) -> bool {
    to_decimal_digit(ch) >= 0
}

/// `_PyUnicode_ToDigit`.
pub fn to_digit(ch: u32) -> i32 {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::DIGIT != 0 {
        ctype.digit as i32
    } else {
        -1
    }
}

/// `_PyUnicode_IsDigit`.
pub fn is_digit(ch: u32) -> bool {
    to_digit(ch) >= 0
}

/// `_PyUnicode_IsNumeric`.
pub fn is_numeric_char(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::NUMERIC != 0
}

/// `_PyUnicode_IsPrintable`.
pub fn is_printable(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::PRINTABLE != 0
}

/// `_PyUnicode_IsLowercase`.
pub fn is_lowercase(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::LOWER != 0
}

/// `_PyUnicode_IsUppercase`.
pub fn is_uppercase(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::UPPER != 0
}

/// `_PyUnicode_IsTitlecase`.
pub fn is_titlecase(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::TITLE != 0
}

/// `_PyUnicode_ToUppercase`: the single-character mapping.
pub fn to_uppercase(ch: u32) -> u32 {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 {
        unicode_data::EXTENDED_CASE[(ctype.upper & 0xFFFF) as usize]
    } else {
        (ch as i64 + ctype.upper as i64) as u32
    }
}

/// `_PyUnicode_ToLowercase`.
pub fn to_lowercase(ch: u32) -> u32 {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 {
        unicode_data::EXTENDED_CASE[(ctype.lower & 0xFFFF) as usize]
    } else {
        (ch as i64 + ctype.lower as i64) as u32
    }
}

/// `_PyUnicode_ToLowerFull`: one character's full lowercase mapping.
pub fn to_lower_full(ch: u32, res: &mut [u32; 3]) -> usize {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 {
        let index = (ctype.lower & 0xFFFF) as usize;
        let n = (ctype.lower >> 24) as usize;
        res[..n].copy_from_slice(&unicode_data::EXTENDED_CASE[index..index + n]);
        n
    } else {
        res[0] = (ch as i64 + ctype.lower as i64) as u32;
        1
    }
}

/// `_PyUnicode_ToTitleFull`.
pub fn to_title_full(ch: u32, res: &mut [u32; 3]) -> usize {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 {
        let index = (ctype.title & 0xFFFF) as usize;
        let n = (ctype.title >> 24) as usize;
        res[..n].copy_from_slice(&unicode_data::EXTENDED_CASE[index..index + n]);
        n
    } else {
        res[0] = (ch as i64 + ctype.title as i64) as u32;
        1
    }
}

/// `_PyUnicode_ToUpperFull`.
pub fn to_upper_full(ch: u32, res: &mut [u32; 3]) -> usize {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 {
        let index = (ctype.upper & 0xFFFF) as usize;
        let n = (ctype.upper >> 24) as usize;
        res[..n].copy_from_slice(&unicode_data::EXTENDED_CASE[index..index + n]);
        n
    } else {
        res[0] = (ch as i64 + ctype.upper as i64) as u32;
        1
    }
}

/// `_PyUnicode_ToFoldedFull`: the fold mapping, the lower mapping's
/// continuation when the record carries one.
pub fn to_folded_full(ch: u32, res: &mut [u32; 3]) -> usize {
    let ctype = unicode_data::record(ch);
    if ctype.flags & unicode_data::flags::EXTENDED_CASE != 0 && (ctype.lower >> 20) & 7 != 0 {
        let index = ((ctype.lower & 0xFFFF) + (ctype.lower >> 24)) as usize;
        let n = ((ctype.lower >> 20) & 7) as usize;
        res[..n].copy_from_slice(&unicode_data::EXTENDED_CASE[index..index + n]);
        n
    } else {
        to_lower_full(ch, res)
    }
}

/// `_PyUnicode_IsCased`.
pub fn is_cased(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::CASED != 0
}

/// `_PyUnicode_IsCaseIgnorable`.
pub fn is_case_ignorable(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::CASE_IGNORABLE != 0
}

/// `_PyUnicode_IsAlpha`.
pub fn is_alpha(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::ALPHA != 0
}

/// `_PyUnicode_IsXidStart`.
pub fn is_xid_start(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::XID_START != 0
}

/// `_PyUnicode_IsXidContinue`.
pub fn is_xid_continue(ch: u32) -> bool {
    unicode_data::record(ch).flags & unicode_data::flags::XID_CONTINUE != 0
}

/// `_PyUnicode_ToNumeric`: the numeric value of a numeric character, -1.0
/// when there is none. The generated table holds the exact C switch.
pub fn to_numeric(ch: u32) -> f64 {
    match unicode_data::NUMERIC.binary_search_by_key(&ch, |entry| entry.0) {
        Ok(index) => unicode_data::NUMERIC[index].1,
        Err(_) => -1.0,
    }
}

// ---------------------------------------------------------- case operations

/// `handle_capital_sigma`: U+03A3 maps to U+03C2 (final) when it follows
/// cased characters and nothing cased follows it.
fn handle_capital_sigma(hay: Chars, i: isize) -> u32 {
    let length = hay.len();
    let mut j = i - 1;
    let mut c = 0u32;
    while j >= 0 {
        c = hay.get(j);
        if !is_case_ignorable(c) {
            break;
        }
        j -= 1;
    }
    let mut final_sigma = j >= 0 && is_cased(c);
    if final_sigma {
        j = i + 1;
        while j < length {
            c = hay.get(j);
            if !is_case_ignorable(c) {
                break;
            }
            j += 1;
        }
        final_sigma = j == length || !is_cased(c);
    }
    if final_sigma { 0x3C2 } else { 0x3C3 }
}

/// `lower_ucs4`: the one special case of the capital sigma before the
/// table mapping.
fn lower_ucs4(hay: Chars, i: isize, c: u32, mapped: &mut [u32; 3]) -> usize {
    if c == 0x3A3 {
        mapped[0] = handle_capital_sigma(hay, i);
        1
    } else {
        to_lower_full(c, mapped)
    }
}

fn do_upper_or_lower(hay: Chars, lower: bool) -> Vec<u32> {
    let mut res = Vec::with_capacity(hay.len() as usize);
    let mut mapped = [0u32; 3];
    for i in 0..hay.len() {
        let c = hay.get(i);
        let n_res = if lower {
            lower_ucs4(hay, i, c, &mut mapped)
        } else {
            to_upper_full(c, &mut mapped)
        };
        res.extend_from_slice(&mapped[..n_res]);
    }
    res
}

fn do_upper(hay: Chars) -> Vec<u32> {
    do_upper_or_lower(hay, false)
}

fn do_lower(hay: Chars) -> Vec<u32> {
    do_upper_or_lower(hay, true)
}

fn do_casefold(hay: Chars) -> Vec<u32> {
    let mut res = Vec::with_capacity(hay.len() as usize);
    let mut mapped = [0u32; 3];
    for i in 0..hay.len() {
        let n_res = to_folded_full(hay.get(i), &mut mapped);
        res.extend_from_slice(&mapped[..n_res]);
    }
    res
}

fn do_title(hay: Chars) -> Vec<u32> {
    let mut res = Vec::with_capacity(hay.len() as usize);
    let mut mapped = [0u32; 3];
    let mut previous_is_cased = false;
    for i in 0..hay.len() {
        let c = hay.get(i);
        let n_res = if previous_is_cased {
            lower_ucs4(hay, i, c, &mut mapped)
        } else {
            to_title_full(c, &mut mapped)
        };
        res.extend_from_slice(&mapped[..n_res]);
        previous_is_cased = is_cased(c);
    }
    res
}

fn do_capitalize(hay: Chars) -> Vec<u32> {
    let mut res = Vec::with_capacity(hay.len() as usize);
    let mut mapped = [0u32; 3];
    if !hay.is_empty() {
        let n_res = to_title_full(hay.get(0), &mut mapped);
        res.extend_from_slice(&mapped[..n_res]);
    }
    for i in 1..hay.len() {
        let n_res = lower_ucs4(hay, i, hay.get(i), &mut mapped);
        res.extend_from_slice(&mapped[..n_res]);
    }
    res
}

fn do_swapcase(hay: Chars) -> Vec<u32> {
    let mut res = Vec::with_capacity(hay.len() as usize);
    let mut mapped = [0u32; 3];
    for i in 0..hay.len() {
        let c = hay.get(i);
        let n_res = if is_uppercase(c) {
            lower_ucs4(hay, i, c, &mut mapped)
        } else if is_lowercase(c) {
            to_upper_full(c, &mut mapped)
        } else {
            mapped[0] = c;
            1
        };
        res.extend_from_slice(&mapped[..n_res]);
    }
    res
}

/// `PyUnicode_Upper`.
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Upper(str_: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&do_upper(hay), 0)
    }
}

/// `PyUnicode_Lower`.
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Lower(str_: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&do_lower(hay), 0)
    }
}

// `str.casefold` has no public C API alias; this is the method core.
/// The `str.casefold` core.
pub fn casefold_core(hay: &[u32]) -> Vec<u32> {
    do_casefold(chars_from_owned(hay))
}

/// The `str.title` core.
pub fn title_core(hay: &[u32]) -> Vec<u32> {
    do_title(chars_from_owned(hay))
}

/// The `str.capitalize` core.
pub fn capitalize_core(hay: &[u32]) -> Vec<u32> {
    do_capitalize(chars_from_owned(hay))
}

/// The `str.swapcase` core.
pub fn swapcase_core(hay: &[u32]) -> Vec<u32> {
    do_swapcase(chars_from_owned(hay))
}

// ------------------------------------------------------------------ strip

/// The strip kinds (`LEFTSTRIP`, `RIGHTSTRIP`, `BOTHSTRIP`).
#[derive(Clone, Copy, PartialEq)]
pub enum StripKind {
    Left,
    Right,
    Both,
}

/// `do_strip`: whitespace stripping; ASCII fast path over the C table.
pub fn strip_core(hay: &[u32], striptype: StripKind) -> Vec<u32> {
    let length = hay.len() as isize;
    let is_ascii = hay.iter().all(|ch| *ch < 0x80);
    let mut i = 0isize;
    if striptype != StripKind::Right {
        while i < length {
            let ch = hay[i as usize];
            let space = if is_ascii {
                is_ascii_whitespace(ch as u8)
            } else {
                is_space(ch)
            };
            if !space {
                break;
            }
            i += 1;
        }
    }
    let mut j = length;
    if striptype != StripKind::Left {
        j -= 1;
        while j >= i {
            let ch = hay[j as usize];
            let space = if is_ascii {
                is_ascii_whitespace(ch as u8)
            } else {
                is_space(ch)
            };
            if !space {
                break;
            }
            j -= 1;
        }
        j += 1;
    }
    hay[i as usize..j as usize].to_vec()
}

/// `_Pyascii_whitespace`: the C locale's set, CPython's `do_strip` ASCII
/// table (tab through CR, space).
fn is_ascii_whitespace(ch: u8) -> bool {
    matches!(ch, 0x09..=0x0D | 0x20)
}

/// `_PyUnicode_XStrip`: stripping with an explicit character set.
///
/// # Safety
///
/// Both arguments must be valid string objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_XStrip(
    self_: *mut PyObject,
    striptype: c_int,
    sepobj: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: reads stay within the strings.
    unsafe {
        let Some(hay) = as_chars(self_) else {
            return core::ptr::null_mut();
        };
        let Some(sep) = as_chars(sepobj) else {
            return core::ptr::null_mut();
        };
        let kind_ = match striptype {
            1 => StripKind::Right,
            2 => StripKind::Left,
            _ => StripKind::Both,
        };
        let stripped = xstrip_core(&hay.as_vec(), &sep.as_vec(), kind_);
        // SAFETY: the input is a string object.
        result(copy_chars_object(
            chars_from_owned(&stripped),
            max_char_value(self_).min(0x10FFFF),
        ))
    }
}

/// The separator-set strip over owned characters.
pub(crate) fn xstrip_core(hay: &[u32], sep: &[u32], striptype: StripKind) -> Vec<u32> {
    let length = hay.len() as isize;
    let mut i = 0isize;
    if striptype != StripKind::Right {
        while i < length && sep.contains(&hay[i as usize]) {
            i += 1;
        }
    }
    let mut j = length;
    if striptype != StripKind::Left {
        j -= 1;
        while j >= i && sep.contains(&hay[j as usize]) {
            j -= 1;
        }
        j += 1;
    }
    hay[i as usize..j as usize].to_vec()
}

// -------------------------------------------------------------------- pad

/// The pad helper behind `ljust`/`rjust`/`center`/`zfill`: a copy with
/// `left` and `right` fill characters. No-op padding shares the input's
/// value (`return_self`).
pub fn pad_core(hay: &[u32], left: isize, right: isize, fill: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity((left.max(0) + right.max(0) + hay.len() as isize) as usize);
    out.extend(core::iter::repeat_n(fill, left.max(0) as usize));
    out.extend_from_slice(hay);
    out.extend(core::iter::repeat_n(fill, right.max(0) as usize));
    out
}

/// The `str.ljust` core.
pub fn ljust_core(hay: &[u32], width: isize, fillchar: u32) -> Vec<u32> {
    if hay.len() as isize >= width {
        return hay.to_vec();
    }
    pad_core(hay, 0, width - hay.len() as isize, fillchar)
}

/// The `str.rjust` core.
pub fn rjust_core(hay: &[u32], width: isize, fillchar: u32) -> Vec<u32> {
    if hay.len() as isize >= width {
        return hay.to_vec();
    }
    pad_core(hay, width - hay.len() as isize, 0, fillchar)
}

/// The `str.center` core (`stringlib_center_impl`'s margin split).
pub fn center_core(hay: &[u32], width: isize, fillchar: u32) -> Vec<u32> {
    let len = hay.len() as isize;
    if len >= width {
        return hay.to_vec();
    }
    let marg = width - len;
    let left = marg / 2 + (marg & width & 1);
    pad_core(hay, left, marg - left, fillchar)
}

/// The `str.zfill` core: zero pad, then move a leading sign after the
/// padding.
pub fn zfill_core(hay: &[u32], width: isize) -> Vec<u32> {
    let len = hay.len() as isize;
    if len >= width {
        return hay.to_vec();
    }
    let fill = width - len;
    let mut out = pad_core(hay, fill, 0, '0' as u32);
    if len > 0 && (out[fill as usize] == '+' as u32 || out[fill as usize] == '-' as u32) {
        out[0] = out[fill as usize];
        out[fill as usize] = '0' as u32;
    }
    out
}

/// The `str.expandtabs` core (`stringlib_expandtabs_impl`, two passes over
/// the column counter).
pub fn expandtabs_core(hay: &[u32], tabsize: isize) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut col = 0isize;
    for &ch in hay {
        if ch == '\t' as u32 {
            if tabsize > 0 {
                let incr = tabsize - (col % tabsize);
                col += incr;
                out.extend(core::iter::repeat_n(' ' as u32, incr as usize));
            }
        } else {
            col += 1;
            out.push(ch);
            if ch == '\n' as u32 || ch == '\r' as u32 {
                col = 0;
            }
        }
    }
    out
}

// ------------------------------------------------------------- predicates

/// The `str.islower` core: every cased character lowercase and at least
/// one cased character.
pub fn islower_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    if hay.len() == 1 {
        return is_lowercase(hay.get(0));
    }
    let mut cased = false;
    for i in 0..hay.len() {
        let c = hay.get(i);
        if is_lowercase(c) {
            cased = true;
        } else if is_uppercase(c) || is_titlecase(c) {
            return false;
        }
    }
    cased
}

/// The `str.isupper` core.
pub fn isupper_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    if hay.len() == 1 {
        return is_uppercase(hay.get(0));
    }
    let mut cased = false;
    for i in 0..hay.len() {
        let c = hay.get(i);
        if is_uppercase(c) {
            cased = true;
        } else if is_lowercase(c) || is_titlecase(c) {
            return false;
        }
    }
    cased
}

/// The `str.istitle` core: uppercase/title characters never follow cased
/// ones, and something is cased.
pub fn istitle_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    let mut cased = false;
    let mut previous_is_cased = false;
    for i in 0..hay.len() {
        let c = hay.get(i);
        if is_uppercase(c) || is_titlecase(c) {
            if previous_is_cased {
                return false;
            }
            previous_is_cased = true;
            cased = true;
        } else if is_lowercase(c) {
            return false;
        } else {
            previous_is_cased = false;
        }
    }
    cased
}

/// The `str.isspace` core.
pub fn isspace_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| is_space(hay.get(i)))
}

/// The `str.isalpha` core.
pub fn isalpha_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| is_alpha(hay.get(i)))
}

/// The `str.isalnum` core.
pub fn isalnum_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| {
        let c = hay.get(i);
        is_alpha(c) || is_decimal_digit(c) || is_digit(c) || is_numeric_char(c)
    })
}

/// The `str.isdecimal` core.
pub fn isdecimal_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| is_decimal_digit(hay.get(i)))
}

/// The `str.isdigit` core.
pub fn isdigit_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| is_digit(hay.get(i)))
}

/// The `str.isnumeric` core.
pub fn isnumeric_core(hay: Chars) -> bool {
    if hay.is_empty() {
        return false;
    }
    (0..hay.len()).all(|i| is_numeric_char(hay.get(i)))
}

/// The `str.isprintable` core.
pub fn isprintable_core(hay: Chars) -> bool {
    (0..hay.len()).all(|i| is_printable(hay.get(i)))
}

/// The `str.isascii` core.
pub fn isascii_core(hay: Chars) -> bool {
    (0..hay.len()).all(|i| hay.get(i) < 0x80)
}

/// `_PyUnicode_IsIdentifier`: XID start (or `_`) then XID continue (or
/// `_`).
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_IsIdentifier(str_: *mut PyObject) -> c_int {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(str_) else {
            return 0;
        };
        if hay.is_empty() {
            return 0;
        }
        let first = hay.get(0);
        if !is_xid_start(first) && first != '_' as u32 {
            return 0;
        }
        for i in 1..hay.len() {
            let c = hay.get(i);
            if !is_xid_continue(c) && c != '_' as u32 {
                return 0;
            }
        }
        1
    }
}

// --------------------------------------------------------------- translate

/// The `str.translate` core: `table` maps code points to replacement
/// strings (`None` keeps them), `delete` removes characters outright
/// (`PyUnicode_Translate`'s `ch`/`delch` degenerate to table entries).
pub fn translate_core(
    hay: Chars,
    table: &dyn Fn(u32) -> Option<Option<Vec<u32>>>,
    delete: &[u32],
) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for i in 0..hay.len() {
        let ch = hay.get(i);
        if delete.contains(&ch) {
            continue;
        }
        match table(ch) {
            None => out.push(ch),
            Some(None) => {}
            Some(Some(mapped)) => out.extend_from_slice(&mapped),
        }
    }
    out
}

// ------------------------------------------------------------------ codecs

/// The error handlers `unicodeobject.c` applies inline. The `codecs`
/// registry and Python-level handlers (among them `namereplace`, which
/// needs unicodedata's name database) come with their own tasks; any
/// other name raises `LookupError` like an unregistered handler.
#[derive(Clone, Copy, PartialEq)]
enum ErrHandler {
    Strict,
    Ignore,
    Replace,
    BackslashReplace,
    XmlCharRefReplace,
    SurrogateEscape,
    SurrogatePass,
}

fn error_handler(errors: Option<&str>) -> ErrHandler {
    match errors {
        None | Some("strict") => ErrHandler::Strict,
        Some("ignore") => ErrHandler::Ignore,
        Some("replace") => ErrHandler::Replace,
        Some("backslashreplace") => ErrHandler::BackslashReplace,
        Some("xmlcharrefreplace") => ErrHandler::XmlCharRefReplace,
        Some("surrogateescape") => ErrHandler::SurrogateEscape,
        Some("surrogatepass") => ErrHandler::SurrogatePass,
        Some(other) => {
            crate::tstate::set_string(
                exceptions::lookup_error(),
                format!("unknown error handler name '{other}'").as_bytes(),
            );
            ErrHandler::Strict
        }
    }
}

fn lookup_error_message(message: String) {
    crate::tstate::set_string(exceptions::lookup_error(), message.as_bytes());
}

/// The `UnicodeDecodeError.__str__` rendering, raised through the interim
/// string exception state; the instances come with the exceptions task.
/// A recoverable escape error under the built-in handlers, matching the
/// live interpreter: `ignore` drops the escape, `replace` writes U+FFFD,
/// `backslashreplace` re-escapes the raw bytes; every other handler
/// raises and returns false.
fn escape_error(
    out: &mut Vec<u32>,
    s: &[u8],
    start: isize,
    end: isize,
    handler: ErrHandler,
    encoding: &str,
    message: &str,
) -> bool {
    let from = start.max(0) as usize;
    let to = (end.max(0) as usize).min(s.len());
    match handler {
        ErrHandler::Ignore => true,
        ErrHandler::Replace => {
            out.push(0xFFFD);
            true
        }
        ErrHandler::BackslashReplace => {
            for &b in &s[from..to] {
                out.push(b'\\' as u32);
                out.push(b'x' as u32);
                out.push(HEX_DIGITS[(b >> 4) as usize] as u32);
                out.push(HEX_DIGITS[(b & 0xf) as usize] as u32);
            }
            true
        }
        _ => {
            raise_decode_error(encoding, s, start, end, message);
            false
        }
    }
}

pub(crate) fn raise_decode_error(
    encoding: &str,
    object: &[u8],
    start: isize,
    end: isize,
    reason: &str,
) {
    let message = if start >= 0
        && (start as usize) < object.len()
        && end >= 0
        && end as usize <= object.len()
        && end == start + 1
    {
        format!(
            "'{encoding}' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            object[start as usize]
        )
    } else {
        format!(
            "'{encoding}' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    };
    crate::tstate::set_string(exceptions::unicode_decode_error(), message.as_bytes());
}

/// The `UnicodeEncodeError.__str__` rendering.
pub(crate) fn raise_encode_error(
    encoding: &str,
    object: &[u32],
    start: isize,
    end: isize,
    reason: &str,
) {
    let len = object.len() as isize;
    let message = if start >= 0 && start < len && end >= 0 && end <= len && end == start + 1 {
        let badchar = object[start as usize];
        let escaped = if badchar <= 0xff {
            format!("\\x{badchar:02x}")
        } else if badchar <= 0xffff {
            format!("\\u{badchar:04x}")
        } else {
            format!("\\U{badchar:08x}")
        };
        format!(
            "'{encoding}' codec can't encode character '{escaped}' in position {start}: {reason}"
        )
    } else {
        format!(
            "'{encoding}' codec can't encode characters in position {start}-{}: {reason}",
            end - 1
        )
    };
    crate::tstate::set_string(exceptions::unicode_encode_error(), message.as_bytes());
}

/// The `UnicodeTranslateError.__str__` rendering.
pub fn raise_translate_error(object: &[u32], start: isize, end: isize, reason: &str) {
    let len = object.len() as isize;
    let message = if start >= 0 && start < len && end >= 0 && end <= len && end == start + 1 {
        let badchar = object[start as usize];
        let escaped = if badchar <= 0xff {
            format!("\\x{badchar:02x}")
        } else if badchar <= 0xffff {
            format!("\\u{badchar:04x}")
        } else {
            format!("\\U{badchar:08x}")
        };
        format!("can't translate character '{escaped}' in position {start}: {reason}")
    } else {
        format!(
            "can't translate characters in position {start}-{}: {reason}",
            end - 1
        )
    };
    crate::tstate::set_string(exceptions::unicode_translate_error(), message.as_bytes());
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut Vec<u8>, value: u32, digits: usize) {
    for shift in (0..digits * 4).step_by(4).rev() {
        out.push(HEX_DIGITS[((value >> shift) & 0xf) as usize]);
    }
}

fn is_surrogate(ch: u32) -> bool {
    (0xD800..0xE000).contains(&ch)
}

fn is_high_surrogate(ch: u32) -> bool {
    (0xD800..0xDC00).contains(&ch)
}

fn is_low_surrogate(ch: u32) -> bool {
    (0xDC00..0xE000).contains(&ch)
}

fn join_surrogates(high: u32, low: u32) -> u32 {
    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
}

/// The UTF-8 byte scanner (`codecs.h`'s `STRINGLIB(utf8_decode)`): the
/// decoded character, or the negative error codes 1-4 as in CPython.
/// The scanner code for a sequence cut off by the end of the input.
const TRUNCATED: i32 = -5;

fn utf8_scan_one(s: &[u8]) -> (i32, u32) {
    let ch = s[0] as u32;
    if ch < 0x80 {
        return (0, ch);
    }
    if ch < 0xE0 {
        if ch < 0xC2 {
            return (-1, 0);
        }
        if s.len() < 2 {
            return (TRUNCATED, 0);
        }
        let ch2 = s[1] as u32;
        if !(0x80..0xC0).contains(&ch2) {
            return (-2, 0);
        }
        return (0, (ch << 6) + ch2 - ((0xC0 << 6) + 0x80));
    }
    if ch < 0xF0 {
        if s.len() < 3 {
            if s.len() < 2 {
                return (TRUNCATED, 0);
            }
            let ch2 = s[1] as u32;
            if !(0x80..0xC0).contains(&ch2)
                || (ch2 < 0xA0 && ch == 0xE0)
                || (ch == 0xED && ch2 >= 0xA0)
            {
                return (-2, 0);
            }
            return (TRUNCATED, 0);
        }
        let ch2 = s[1] as u32;
        let ch3 = s[2] as u32;
        if !(0x80..0xC0).contains(&ch2) {
            return (-2, 0);
        }
        if ch == 0xE0 && ch2 < 0xA0 {
            return (-2, 0);
        }
        if ch == 0xED && ch2 >= 0xA0 {
            return (-2, 0);
        }
        if !(0x80..0xC0).contains(&ch3) {
            return (-3, 0);
        }
        return (
            0,
            (ch << 12) + (ch2 << 6) + ch3 - ((0xE0 << 12) + (0x80 << 6) + 0x80),
        );
    }
    if ch < 0xF5 {
        if s.len() < 4 {
            if s.len() < 2 {
                return (TRUNCATED, 0);
            }
            let ch2 = s[1] as u32;
            if !(0x80..0xC0).contains(&ch2)
                || (ch2 < 0x90 && ch == 0xF0)
                || (ch == 0xF4 && ch2 >= 0x90)
            {
                return (-2, 0);
            }
            if s.len() < 3 {
                return (TRUNCATED, 0);
            }
            if !(0x80..0xC0).contains(&(s[2] as u32)) {
                return (-3, 0);
            }
            return (TRUNCATED, 0);
        }
        let ch2 = s[1] as u32;
        let ch3 = s[2] as u32;
        let ch4 = s[3] as u32;
        if !(0x80..0xC0).contains(&ch2) {
            return (-2, 0);
        }
        if ch == 0xF0 && ch2 < 0x90 {
            return (-2, 0);
        }
        if ch == 0xF4 && ch2 >= 0x90 {
            return (-2, 0);
        }
        if !(0x80..0xC0).contains(&ch3) {
            return (-3, 0);
        }
        if !(0x80..0xC0).contains(&ch4) {
            return (-4, 0);
        }
        return (
            0,
            (ch << 18) + (ch2 << 12) + (ch3 << 6) + ch4
                - ((0xF0 << 18) + (0x80 << 12) + (0x80 << 6) + 0x80),
        );
    }
    (-1, 0)
}

/// `unicode_decode_utf8`: the scanner plus the inline error handlers.
fn decode_utf8_bytes(s: &[u8], handler: ErrHandler) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::with_capacity(s.len());
    let mut pos = 0usize;
    while pos < s.len() {
        let (error, ch) = utf8_scan_one(&s[pos..]);
        let (start, end, reason) = match error {
            0 => {
                out.push(ch);
                pos += if ch < 0x80 {
                    1
                } else if ch < 0x800 {
                    2
                } else if ch < 0x10000 {
                    3
                } else {
                    4
                };
                continue;
            }
            // The trailing bytes form a truncated sequence.
            TRUNCATED => (pos as isize, s.len() as isize, "unexpected end of data"),
            -1 => (pos as isize, pos as isize + 1, "invalid start byte"),
            code => (
                pos as isize,
                pos as isize + code as isize - 1,
                "invalid continuation byte",
            ),
        };
        match handler {
            ErrHandler::Ignore => pos = end as usize,
            ErrHandler::Replace => {
                out.push(0xFFFD);
                pos = end as usize;
            }
            ErrHandler::SurrogateEscape => {
                for i in start..end {
                    out.push(s[i as usize] as u32 + 0xDC00);
                }
                pos = end as usize;
            }
            _ => {
                raise_decode_error("utf-8", s, start, end, reason);
                return None;
            }
        }
    }
    Some(out)
}

/// The `errors` C-string argument: `None` when absent.
unsafe fn errors_arg(errors: *const c_char) -> Option<&'static str> {
    // SAFETY: the caller owns the C string for the call's duration.
    unsafe {
        if errors.is_null() {
            None
        } else {
            Some(util::cstr(errors))
        }
    }
}

/// The one-byte encoders (`unicode_encode_ucs1`), `limit` 128 or 256.
fn encode_ucs1(chars: &[u32], limit: u32, handler: ErrHandler) -> Option<Vec<u8>> {
    let encoding = if limit == 256 { "latin-1" } else { "ascii" };
    let reason = if limit == 256 {
        "ordinal not in range(256)"
    } else {
        "ordinal not in range(128)"
    };
    let mut out = Vec::with_capacity(chars.len());
    let mut pos = 0usize;
    while pos < chars.len() {
        let ch = chars[pos];
        if ch < limit {
            out.push(ch as u8);
            pos += 1;
        } else {
            let collstart = pos;
            let mut collend = collstart + 1;
            while collend < chars.len() && chars[collend] >= limit {
                collend += 1;
            }
            match handler {
                ErrHandler::Strict => {
                    raise_encode_error(
                        encoding,
                        chars,
                        collstart as isize,
                        collend as isize,
                        reason,
                    );
                    return None;
                }
                ErrHandler::Replace => {
                    out.extend(core::iter::repeat_n(b'?', collend - collstart));
                    pos = collend;
                }
                ErrHandler::Ignore => pos = collend,
                ErrHandler::BackslashReplace => {
                    for &ch in &chars[collstart..collend] {
                        out.push(b'\\');
                        if ch >= 0x10000 {
                            out.push(b'U');
                            push_hex(&mut out, ch, 8);
                        } else if ch >= 0x100 {
                            out.push(b'u');
                            push_hex(&mut out, ch, 4);
                        } else {
                            out.push(b'x');
                            push_hex(&mut out, ch, 2);
                        }
                    }
                    pos = collend;
                }
                ErrHandler::XmlCharRefReplace => {
                    for &ch in &chars[collstart..collend] {
                        out.extend_from_slice(format!("&#{};", ch).as_bytes());
                    }
                    pos = collend;
                }
                ErrHandler::SurrogateEscape => {
                    let mut i = collstart;
                    while i < collend && (0xDC80..=0xDCFF).contains(&chars[i]) {
                        out.push((chars[i] - 0xDC00) as u8);
                        pos = i + 1;
                        i += 1;
                    }
                    if i < collend {
                        raise_encode_error(encoding, chars, pos as isize, collend as isize, reason);
                        return None;
                    }
                }
                ErrHandler::SurrogatePass => {
                    raise_encode_error(
                        encoding,
                        chars,
                        collstart as isize,
                        collend as isize,
                        reason,
                    );
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// The UTF-8 encoder (`utf8_encode` in `codecs.h`): strict rejects lone
/// surrogates, `surrogatepass` writes their three-byte form.
fn encode_utf8_bytes(chars: &[u32], handler: ErrHandler) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(chars.len());
    for (pos, &ch) in chars.iter().enumerate() {
        if is_surrogate(ch) {
            match handler {
                ErrHandler::SurrogatePass => {
                    out.push(0xE0 | (ch >> 12) as u8);
                    out.push(0x80 | ((ch >> 6) & 0x3F) as u8);
                    out.push(0x80 | (ch & 0x3F) as u8);
                }
                ErrHandler::Ignore => {}
                ErrHandler::BackslashReplace => {
                    out.push(b'\\');
                    if ch >= 0x10000 {
                        out.push(b'U');
                        push_hex(&mut out, ch, 8);
                    } else {
                        out.push(b'u');
                        push_hex(&mut out, ch, 4);
                    }
                }
                _ => {
                    raise_encode_error(
                        "utf-8",
                        chars,
                        pos as isize,
                        pos as isize + 1,
                        "surrogates not allowed",
                    );
                    return None;
                }
            }
        } else {
            utf8_encode_one(&mut out, ch);
        }
    }
    Some(out)
}

/// `PyUnicode_DecodeASCII`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeASCII(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let handler = error_handler(errors_arg(errors));
        let Some(chars) = decode_ascii_bytes(bytes, handler) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0x7F)
    }
}

fn decode_ascii_bytes(s: &[u8], handler: ErrHandler) -> Option<Vec<u32>> {
    let mut chars: Vec<u32> = Vec::with_capacity(s.len());
    for (pos, &byte) in s.iter().enumerate() {
        if byte < 0x80 {
            chars.push(byte as u32);
            continue;
        }
        match handler {
            ErrHandler::Ignore => {}
            ErrHandler::Replace => chars.push(0xFFFD),
            ErrHandler::SurrogateEscape => chars.push(0xDC00 + byte as u32),
            _ => {
                raise_decode_error(
                    "ascii",
                    s,
                    pos as isize,
                    pos as isize + 1,
                    "ordinal not in range(128)",
                );
                return None;
            }
        }
    }
    Some(chars)
}

/// `PyUnicode_DecodeLatin1`: every byte maps directly.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLatin1(
    s: *const c_char,
    size: isize,
    _errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        chars_to_object(&bytes.iter().map(|b| *b as u32).collect::<Vec<u32>>(), 0xFF)
    }
}

/// `PyUnicode_AsASCIIString`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsASCIIString(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(chars) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        encode_ucs1_object(&chars.as_vec(), 0x80, error_handler(errors_arg(errors)))
    }
}

/// `PyUnicode_AsLatin1String`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsLatin1String(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(chars) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        encode_ucs1_object(&chars.as_vec(), 0x100, error_handler(errors_arg(errors)))
    }
}

fn encode_ucs1_object(chars: &[u32], limit: u32, handler: ErrHandler) -> *mut PyObject {
    let Some(bytes) = encode_ucs1(chars, limit, handler) else {
        return core::ptr::null_mut();
    };
    match crate::types::bytes::from_vec(Some(bytes)) {
        Some(object) => object,
        None => core::ptr::null_mut(),
    }
}

/// The escape decoder core (`_PyUnicode_DecodeUnicodeEscapeInternal`).
/// `\N{...}` raises CPython's error for an unloaded unicodedata capsule;
/// the name database arrives with the unicodedata task.
fn decode_unicode_escape(
    s: &[u8],
    handler: ErrHandler,
    consumed: Option<&mut isize>,
) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::with_capacity(s.len());
    let mut pos = 0usize;
    while pos < s.len() {
        let c = s[pos];
        pos += 1;
        if c != b'\\' {
            out.push(c as u32);
            continue;
        }
        let startinpos = pos as isize - 1;
        if pos >= s.len() {
            // "\ at end of string"
            if let Some(consumed) = consumed {
                *consumed = startinpos;
                break;
            }
            if !escape_error(
                &mut out,
                s,
                startinpos,
                pos as isize,
                handler,
                "unicodeescape",
                "\\ at end of string",
            ) {
                return None;
            }
            continue;
        }
        let c = s[pos];
        pos += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(b'\\' as u32),
            b'\'' => out.push(b'\'' as u32),
            b'"' => out.push(b'"' as u32),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b't' => out.push(b'\t' as u32),
            b'n' => out.push(b'\n' as u32),
            b'r' => out.push(b'\r' as u32),
            b'v' => out.push(0x0B),
            b'a' => out.push(0x07),
            b'0'..=b'7' => {
                let mut ch = (c - b'0') as u32;
                if pos < s.len() && (b'0'..=b'7').contains(&s[pos]) {
                    ch = (ch << 3) + (s[pos] - b'0') as u32;
                    pos += 1;
                    if pos < s.len() && (b'0'..=b'7').contains(&s[pos]) {
                        ch = (ch << 3) + (s[pos] - b'0') as u32;
                        pos += 1;
                    }
                }
                // An octal escape above 0377 is deprecated in CPython,
                // which warns and keeps the character.
                out.push(ch);
            }
            b'x' | b'u' | b'U' => {
                let count = match c {
                    b'x' => 2,
                    b'u' => 4,
                    _ => 8,
                };
                let message = match c {
                    b'x' => "truncated \\xXX escape",
                    b'u' => "truncated \\uXXXX escape",
                    _ => "truncated \\UXXXXXXXX escape",
                };
                let mut ch: u32 = 0;
                let mut complete = true;
                for _ in 0..count {
                    if pos >= s.len() {
                        complete = false;
                        break;
                    }
                    let digit = s[pos];
                    ch <<= 4;
                    match digit {
                        b'0'..=b'9' => ch += (digit - b'0') as u32,
                        b'a'..=b'f' => ch += (digit - b'a' + 10) as u32,
                        b'A'..=b'F' => ch += (digit - b'A' + 10) as u32,
                        _ => {
                            complete = false;
                            break;
                        }
                    }
                    pos += 1;
                }
                if !complete {
                    if let Some(consumed) = consumed {
                        *consumed = startinpos;
                        break;
                    }
                    if !escape_error(
                        &mut out,
                        s,
                        startinpos,
                        pos as isize,
                        handler,
                        "unicodeescape",
                        message,
                    ) {
                        return None;
                    }
                    continue;
                }
                if ch >= MAX_UNICODE {
                    // Lone surrogates stay legal here; only points beyond
                    // Unicode are the error CPython reports.
                    if !escape_error(
                        &mut out,
                        s,
                        startinpos,
                        pos as isize,
                        handler,
                        "unicodeescape",
                        "illegal Unicode character",
                    ) {
                        return None;
                    }
                    continue;
                }
                out.push(ch);
            }
            b'N' => {
                raise_unicode_error_not_supported(s);
                return None;
            }
            _ => {
                // Unknown escape: keep the backslash and the character.
                out.push(b'\\' as u32);
                out.push(c as u32);
            }
        }
    }
    Some(out)
}

/// CPython's error when the unicodedata capsule is not loaded.
fn raise_unicode_error_not_supported(s: &[u8]) {
    crate::tstate::set_string(
        exceptions::unicode_error(),
        b"\\N escapes not supported (can't load unicodedata module)",
    );
    let _ = s;
}

/// `PyUnicode_DecodeUnicodeEscape`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUnicodeEscape(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let Some(chars) = decode_unicode_escape(bytes, error_handler(errors_arg(errors)), None)
        else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

/// `_PyUnicode_DecodeUnicodeEscapeStateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes; `consumed` must be null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_DecodeUnicodeEscapeStateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let mut consumed_value: isize = 0;
        let tracked = if consumed.is_null() {
            None
        } else {
            Some(&mut consumed_value)
        };
        let Some(chars) = decode_unicode_escape(bytes, error_handler(errors_arg(errors)), tracked)
        else {
            return core::ptr::null_mut();
        };
        if !consumed.is_null() {
            *consumed = consumed_value;
        }
        chars_to_object(&chars, 0)
    }
}

/// `_PyUnicode_DecodeUnicodeEscapeInternal`: like the Stateful entry with
/// the first invalid escape reported instead of a warning being raised.
///
/// # Safety
///
/// `s` must point to `size` bytes; the out-pointers must be null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_DecodeUnicodeEscapeInternal(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
    first_invalid_escape: *mut *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let mut consumed_value: isize = 0;
        let tracked = if consumed.is_null() {
            None
        } else {
            Some(&mut consumed_value)
        };
        let Some(chars) = decode_unicode_escape(bytes, error_handler(errors_arg(errors)), tracked)
        else {
            return core::ptr::null_mut();
        };
        if !consumed.is_null() {
            *consumed = consumed_value;
        }
        if !first_invalid_escape.is_null() {
            *first_invalid_escape = core::ptr::null();
        }
        chars_to_object(&chars, 0)
    }
}

/// `PyUnicode_DecodeRawUnicodeEscape` (`\u`/`\U` escapes only).
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeRawUnicodeEscape(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let Some(chars) = decode_raw_unicode_escape(bytes, error_handler(errors_arg(errors)), None)
        else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

/// `_PyUnicode_DecodeRawUnicodeEscapeStateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes; `consumed` must be null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_DecodeRawUnicodeEscapeStateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let mut consumed_value: isize = 0;
        let tracked = if consumed.is_null() {
            None
        } else {
            Some(&mut consumed_value)
        };
        let Some(chars) =
            decode_raw_unicode_escape(bytes, error_handler(errors_arg(errors)), tracked)
        else {
            return core::ptr::null_mut();
        };
        if !consumed.is_null() {
            *consumed = consumed_value;
        }
        chars_to_object(&chars, 0)
    }
}

fn decode_raw_unicode_escape(
    s: &[u8],
    handler: ErrHandler,
    consumed: Option<&mut isize>,
) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::with_capacity(s.len());
    let mut pos = 0usize;
    while pos < s.len() {
        let c = s[pos];
        pos += 1;
        if c != b'\\' || pos >= s.len() {
            out.push(c as u32);
            continue;
        }
        let c = s[pos];
        pos += 1;
        if c != b'u' && c != b'U' {
            out.push(b'\\' as u32);
            out.push(c as u32);
            continue;
        }
        let count = if c == b'u' { 4 } else { 8 };
        let mut ch: u32 = 0;
        let mut complete = true;
        for _ in 0..count {
            if pos >= s.len() {
                complete = false;
                break;
            }
            let digit = s[pos];
            ch <<= 4;
            match digit {
                b'0'..=b'9' => ch += (digit - b'0') as u32,
                b'a'..=b'f' => ch += (digit - b'a' + 10) as u32,
                b'A'..=b'F' => ch += (digit - b'A' + 10) as u32,
                _ => {
                    complete = false;
                    break;
                }
            }
            pos += 1;
        }
        if !complete {
            // The error range starts at the backslash.
            let startinpos = pos as isize - 2;
            if let Some(consumed) = consumed {
                *consumed = startinpos;
                break;
            }
            if !escape_error(
                &mut out,
                s,
                startinpos,
                pos as isize,
                handler,
                "rawunicodeescape",
                "truncated \\uXXXX escape",
            ) {
                return None;
            }
            continue;
        }
        if ch >= MAX_UNICODE {
            if !escape_error(
                &mut out,
                s,
                pos as isize - 2 - count as isize,
                pos as isize,
                handler,
                "rawunicodeescape",
                "\\Uxxxxxxxx out of range",
            ) {
                return None;
            }
            continue;
        }
        out.push(ch);
    }
    Some(out)
}

/// `PyUnicode_AsUnicodeEscapeString`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUnicodeEscapeString(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        encode_escape_bytes(&hay.as_vec(), true)
    }
}

/// `PyUnicode_AsRawUnicodeEscapeString`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsRawUnicodeEscapeString(
    unicode: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        encode_escape_bytes(&hay.as_vec(), false)
    }
}

/// The escape encoders: `unicode-escape` spells backslash, quotes and
/// control characters with escapes; `raw-unicode-escape` copies 8-bit
/// characters as-is.
fn encode_escape_bytes(chars: &[u32], full: bool) -> *mut PyObject {
    let mut out: Vec<u8> = Vec::new();
    for &ch in chars {
        if ch < 0x100 {
            if !full {
                out.push(ch as u8);
            } else if (0x20..0x7F).contains(&ch) {
                if ch == b'\\' as u32 {
                    out.push(b'\\');
                    out.push(b'\\');
                } else {
                    out.push(ch as u8);
                }
            } else if ch == 0x09 {
                out.extend_from_slice(b"\\t");
            } else if ch == 0x0A {
                out.extend_from_slice(b"\\n");
            } else if ch == 0x0D {
                out.extend_from_slice(b"\\r");
            } else {
                out.extend_from_slice(b"\\x");
                push_hex(&mut out, ch, 2);
            }
        } else if ch < 0x10000 {
            out.extend_from_slice(b"\\u");
            push_hex(&mut out, ch, 4);
        } else {
            out.extend_from_slice(b"\\U");
            out.push(b'0');
            out.push(b'0');
            push_hex(&mut out, ch, 6);
        }
    }
    match crate::types::bytes::from_vec(Some(out)) {
        Some(object) => object,
        None => core::ptr::null_mut(),
    }
}

/// The UTF-16 scanner (`codecs.h`'s `STRINGLIB(utf16_decode)`): returns
/// the character, or the negative codes 1-3 (unexpected end, illegal
/// encoding, illegal surrogate) as CPython's switch expects.
fn utf16_scan_one(q: &[u8], native_ordering: bool) -> (i32, u32) {
    if q.len() < 2 {
        return (-1, 0);
    }
    let read = |i: usize| -> u32 {
        if native_ordering {
            (q[i + 1] as u32) << 8 | q[i] as u32
        } else {
            (q[i] as u32) << 8 | q[i + 1] as u32
        }
    };
    let ch = read(0);
    if !is_surrogate(ch) {
        return (0, ch);
    }
    if !is_high_surrogate(ch) {
        return (-2, 0);
    }
    if q.len() < 4 {
        return (-1, 0);
    }
    let ch2 = read(2);
    if !is_low_surrogate(ch2) {
        return (-3, 0);
    }
    (0, join_surrogates(ch, ch2))
}

fn decode_utf16_bytes(
    s: &[u8],
    handler: ErrHandler,
    byteorder: Option<&mut i32>,
) -> Option<Vec<u32>> {
    let mut bo: i32 = 0;
    if let Some(order) = &byteorder {
        bo = **order;
    }
    let mut offset = 0usize;
    if bo == 0 && s.len() >= 2 {
        let bom = ((s[1] as u32) << 8) | s[0] as u32;
        if bom == 0xFEFF {
            offset = 2;
            bo = -1;
        } else if bom == 0xFFFE {
            offset = 2;
            bo = 1;
        }
        if let Some(order) = byteorder {
            *order = bo;
        }
    }
    let rest = &s[offset..];
    let native_ordering = bo <= 0;
    let encoding = if native_ordering {
        "utf-16-le"
    } else {
        "utf-16-be"
    };
    let mut out: Vec<u32> = Vec::with_capacity(rest.len() / 2);
    let mut pos = 0usize;
    loop {
        let (error, ch) = if rest.len() - pos >= 2 {
            utf16_scan_one(&rest[pos..], native_ordering)
        } else {
            (TRUNCATED, 0)
        };
        match error {
            0 => {
                out.push(ch);
                pos += if ch < 0x10000 { 2 } else { 4 };
            }
            TRUNCATED => {
                if pos == rest.len() {
                    break;
                }
                // An odd trailing byte.
                match handler {
                    ErrHandler::Ignore | ErrHandler::Replace => break,
                    _ => {
                        raise_decode_error(
                            encoding,
                            s,
                            s.len() as isize - 1,
                            s.len() as isize,
                            "truncated data",
                        );
                        return None;
                    }
                }
            }
            -1 => match handler {
                ErrHandler::Ignore | ErrHandler::Replace => break,
                _ => {
                    raise_decode_error(
                        encoding,
                        s,
                        s.len() as isize - 2,
                        s.len() as isize,
                        "unexpected end of data",
                    );
                    return None;
                }
            },
            -2 => {
                let start = (offset + pos - 2) as isize;
                match handler {
                    ErrHandler::Ignore => pos += 2,
                    ErrHandler::Replace => {
                        out.push(0xFFFD);
                        pos += 2;
                    }
                    ErrHandler::SurrogatePass => {
                        out.push(
                            u16::from_ne_bytes([s[start as usize], s[start as usize + 1]]) as u32,
                        );
                        pos += 2;
                    }
                    _ => {
                        raise_decode_error(encoding, s, start, start + 2, "illegal encoding");
                        return None;
                    }
                }
            }
            -3 => {
                let start = (offset + pos - 4) as isize;
                match handler {
                    ErrHandler::Ignore => pos += 2,
                    ErrHandler::Replace => {
                        out.push(0xFFFD);
                        pos += 2;
                    }
                    ErrHandler::SurrogatePass => {
                        out.push(
                            u16::from_ne_bytes([s[start as usize], s[start as usize + 1]]) as u32,
                        );
                        pos += 2;
                    }
                    _ => {
                        raise_decode_error(
                            encoding,
                            s,
                            start,
                            start + 2,
                            "illegal UTF-16 surrogate",
                        );
                        return None;
                    }
                }
            }
            _ => unreachable!("utf16_scan_one returns -3..0"),
        }
        if pos >= rest.len() {
            break;
        }
    }
    Some(out)
}

/// `PyUnicode_DecodeUTF16`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF16(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    byteorder: *mut c_int,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let mut order = if byteorder.is_null() {
            None
        } else {
            Some(*byteorder)
        };
        let Some(chars) =
            decode_utf16_bytes(bytes, error_handler(errors_arg(errors)), order.as_mut())
        else {
            return core::ptr::null_mut();
        };
        if !byteorder.is_null() {
            *byteorder = order.unwrap_or(0);
        }
        chars_to_object(&chars, 0)
    }
}

/// `PyUnicode_DecodeUTF16Stateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF16Stateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    byteorder: *mut c_int,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: reads and writes stay within the caller's contract.
    unsafe {
        // The interim runtime consumes whole inputs; partial decodes come
        // with the stream task, so `consumed` reports the full size.
        let result = PyUnicode_DecodeUTF16(s, size, errors, byteorder);
        if !result.is_null() && !consumed.is_null() {
            *consumed = size;
        }
        result
    }
}

fn encode_utf16_bytes(chars: &[u32], byteorder: i32, handler: ErrHandler) -> Option<Vec<u8>> {
    let little = if byteorder == 0 {
        cfg!(target_endian = "little")
    } else {
        byteorder < 0
    };
    let mut out: Vec<u8> = Vec::with_capacity(chars.len() * 2 + 2);
    if byteorder == 0 {
        // Native byte order writes the BOM.
        if little {
            out.extend_from_slice(&0xFEFF_u16.to_le_bytes());
        } else {
            out.extend_from_slice(&0xFEFF_u16.to_be_bytes());
        }
    }
    for (pos, &ch) in chars.iter().enumerate() {
        let write_unit = |out: &mut Vec<u8>, unit: u32| {
            if little {
                out.extend_from_slice(&(unit as u16).to_le_bytes());
            } else {
                out.extend_from_slice(&(unit as u16).to_be_bytes());
            }
        };
        if is_surrogate(ch) {
            match handler {
                ErrHandler::SurrogatePass => write_unit(&mut out, ch),
                ErrHandler::Ignore => {}
                ErrHandler::BackslashReplace => {
                    let mut escaped = Vec::new();
                    out.push(b'\\');
                    out.push(b'u');
                    push_hex(&mut escaped, ch, 4);
                    out.extend_from_slice(&escaped);
                }
                _ => {
                    raise_encode_error(
                        "utf-16",
                        chars,
                        pos as isize,
                        pos as isize + 1,
                        "surrogates not allowed",
                    );
                    return None;
                }
            }
        } else if ch < 0x10000 {
            write_unit(&mut out, ch);
        } else {
            let value = ch - 0x10000;
            write_unit(&mut out, 0xD800 | (value >> 10));
            write_unit(&mut out, 0xDC00 | (value & 0x3FF));
        }
    }
    Some(out)
}

/// `_PyUnicode_EncodeUTF16`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EncodeUTF16(
    data: *mut PyObject,
    outlen: *mut isize,
    byteorder: c_int,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(data) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) =
            encode_utf16_bytes(&hay.as_vec(), byteorder, error_handler(errors_arg(errors)))
        else {
            return core::ptr::null_mut();
        };
        if !outlen.is_null() {
            *outlen = bytes.len() as isize;
        }
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_AsUTF16String`: native byte order with BOM.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF16String(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) = encode_utf16_bytes(&hay.as_vec(), 0, ErrHandler::Strict) else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_DecodeUTF32`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF32(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    byteorder: *mut c_int,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let mut order = if byteorder.is_null() { 0 } else { *byteorder };
        let Some(chars) = decode_utf32_bytes(bytes, error_handler(errors_arg(errors)), &mut order)
        else {
            return core::ptr::null_mut();
        };
        if !byteorder.is_null() {
            *byteorder = order;
        }
        chars_to_object(&chars, 0)
    }
}

fn decode_utf32_bytes(s: &[u8], handler: ErrHandler, byteorder: &mut i32) -> Option<Vec<u32>> {
    let mut bo = *byteorder;
    let mut offset = 0usize;
    if bo == 0 && s.len() >= 4 {
        let bom =
            ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | s[3] as u32;
        if bom == 0xFFFE0000 {
            offset = 4;
            bo = -1;
        } else if bom == 0x0000FEFF {
            offset = 4;
            bo = 1;
        }
        *byteorder = bo;
    }
    let rest = &s[offset..];
    let le = bo <= 0;
    let encoding = if le { "utf-32-le" } else { "utf-32-be" };
    let read = |i: usize| -> u32 {
        if le {
            ((rest[i + 3] as u32) << 24)
                | ((rest[i + 2] as u32) << 16)
                | ((rest[i + 1] as u32) << 8)
                | rest[i] as u32
        } else {
            ((rest[i] as u32) << 24)
                | ((rest[i + 1] as u32) << 16)
                | ((rest[i + 2] as u32) << 8)
                | rest[i + 3] as u32
        }
    };
    let mut out: Vec<u32> = Vec::with_capacity(rest.len() / 4);
    let mut pos = 0usize;
    while pos + 4 <= rest.len() {
        let ch = read(pos);
        let (start, end) = ((offset + pos) as isize, (offset + pos + 4) as isize);
        let bad = if is_surrogate(ch) {
            Some("code point in surrogate code point range(0xd800, 0xe000)")
        } else if ch >= 0x110000 {
            Some("code point not in range(0x110000)")
        } else {
            None
        };
        match (bad, handler) {
            (None, _) => {
                out.push(ch);
                pos += 4;
            }
            (Some(_), ErrHandler::Ignore) => pos += 4,
            (Some(_), ErrHandler::Replace) => {
                out.push(0xFFFD);
                pos += 4;
            }
            (Some(_), ErrHandler::SurrogatePass) if is_surrogate(ch) => {
                out.push(ch);
                pos += 4;
            }
            (Some(reason), _) => {
                raise_decode_error(encoding, s, start, end, reason);
                return None;
            }
        }
    }
    if pos < rest.len() {
        match handler {
            ErrHandler::Ignore | ErrHandler::Replace => {}
            _ => {
                raise_decode_error(
                    encoding,
                    s,
                    (offset + pos) as isize,
                    s.len() as isize,
                    "truncated data",
                );
                return None;
            }
        }
    }
    Some(out)
}

/// `PyUnicode_DecodeUTF32Stateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF32Stateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    byteorder: *mut c_int,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: reads and writes stay within the caller's contract.
    unsafe {
        // Whole-input decoding; partial consumption comes with the
        // stream task.
        let result = PyUnicode_DecodeUTF32(s, size, errors, byteorder);
        if !result.is_null() && !consumed.is_null() {
            *consumed = size;
        }
        result
    }
}

/// `_PyUnicode_EncodeUTF32`.
///
/// # Safety
///
/// `data` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EncodeUTF32(
    data: *mut PyObject,
    outlen: *mut isize,
    byteorder: c_int,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(data) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) =
            encode_utf32_bytes(&hay.as_vec(), byteorder, error_handler(errors_arg(errors)))
        else {
            return core::ptr::null_mut();
        };
        if !outlen.is_null() {
            *outlen = bytes.len() as isize;
        }
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

fn encode_utf32_bytes(chars: &[u32], byteorder: i32, handler: ErrHandler) -> Option<Vec<u8>> {
    let little = if byteorder == 0 {
        cfg!(target_endian = "little")
    } else {
        byteorder < 0
    };
    let mut out: Vec<u8> = Vec::with_capacity(chars.len() * 4 + 4);
    if byteorder == 0 {
        if little {
            out.extend_from_slice(&0xFEFF_u32.to_le_bytes());
        } else {
            out.extend_from_slice(&0xFEFF_u32.to_be_bytes());
        }
    }
    for (pos, &ch) in chars.iter().enumerate() {
        if is_surrogate(ch) && !matches!(handler, ErrHandler::SurrogatePass) {
            if matches!(handler, ErrHandler::Ignore) {
                continue;
            }
            raise_encode_error(
                "utf-32",
                chars,
                pos as isize,
                pos as isize + 1,
                "code point in surrogate code point range(0xd800, 0xe000)",
            );
            return None;
        }
        let unit = ch;
        if little {
            out.extend_from_slice(&unit.to_le_bytes());
        } else {
            out.extend_from_slice(&unit.to_be_bytes());
        }
    }
    Some(out)
}

/// `PyUnicode_AsUTF32String`: native byte order with BOM.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF32String(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) = encode_utf32_bytes(&hay.as_vec(), 0, ErrHandler::Strict) else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// The charmap decoder for string tables (`charmap_decode_string`'s
/// general path): byte-to-code-point, U+FFFE meaning unmapped. Bytes and
/// dict tables need the container runtime and raise a typed error.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeCharmap(
    s: *const c_char,
    size: isize,
    mapping: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract, as in CPython.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        if mapping.is_null() {
            return PyUnicode_DecodeLatin1(s, size, errors);
        }
        let handler = error_handler(errors_arg(errors));
        let Some(chars) = decode_charmap_bytes(bytes, mapping, handler) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

fn decode_charmap_bytes(s: &[u8], mapping: *mut PyObject, handler: ErrHandler) -> Option<Vec<u32>> {
    // SAFETY: `mapping` is a valid object.
    let (maplen, mapdata) = unsafe {
        if !check_exact(mapping) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"coil: charmap tables of bytes or dict type need the container runtime",
            );
            return None;
        }
        (get_length(mapping) as usize, data(mapping))
    };
    let mut out: Vec<u32> = Vec::with_capacity(s.len());
    for &byte in s {
        let x = if (byte as usize) < maplen {
            // SAFETY: index is below the mapping's length.
            unsafe { read(kind(mapping), mapdata, byte as isize) }
        } else {
            0xFFFE
        };
        if x == 0xFFFE {
            match handler {
                ErrHandler::Ignore => {}
                ErrHandler::Replace => out.push(0xFFFD),
                _ => {
                    raise_decode_error(
                        "charmap",
                        s,
                        out.len() as isize,
                        out.len() as isize + 1,
                        "character maps to <undefined>",
                    );
                    return None;
                }
            }
        } else {
            out.push(x);
        }
    }
    Some(out)
}

/// `PyUnicode_AsCharmapString`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsCharmapString(
    unicode: *mut PyObject,
    mapping: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) =
            encode_charmap_bytes(&hay.as_vec(), mapping, error_handler(errors_arg(errors)))
        else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

fn encode_charmap_bytes(
    chars: &[u32],
    mapping: *mut PyObject,
    handler: ErrHandler,
) -> Option<Vec<u8>> {
    if mapping.is_null() {
        return encode_ucs1(chars, 0x100, handler);
    }
    // SAFETY: `mapping` is a valid object.
    let (maplen, mapdata) = unsafe {
        if !check_exact(mapping) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"coil: charmap tables of bytes or dict type need the container runtime",
            );
            return None;
        }
        (get_length(mapping) as usize, data(mapping))
    };
    let mut out = Vec::with_capacity(chars.len());
    for (pos, &ch) in chars.iter().enumerate() {
        let x = if (ch as usize) < maplen {
            // SAFETY: index is below the mapping's length.
            unsafe { read(kind(mapping), mapdata, ch as isize) }
        } else {
            0xFFFE
        };
        if x == 0xFFFE {
            match handler {
                ErrHandler::Ignore => {}
                ErrHandler::Replace => out.push(b'?'),
                _ => {
                    raise_encode_error(
                        "charmap",
                        chars,
                        pos as isize,
                        pos as isize + 1,
                        "character maps to <undefined>",
                    );
                    return None;
                }
            }
        } else {
            out.push(x as u8);
        }
    }
    Some(out)
}

/// `_PyUnicode_EncodeCharmap`.
///
/// # Safety
///
/// `data` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EncodeCharmap(
    data: *mut PyObject,
    mapping: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(data) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) =
            encode_charmap_bytes(&hay.as_vec(), mapping, error_handler(errors_arg(errors)))
        else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_BuildEncodingMap` builds the trie object CPython's codecs
/// module consumes; the object model for it arrives with the container
/// task, so the interim runtime raises a typed error.
///
/// # Safety
///
/// `string` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_BuildEncodingMap(string: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        if !check(string) || get_length(string) == 0 {
            crate::tstate::set_string(
                exceptions::value_error(),
                "map must be non-empty".as_bytes(),
            );
            return core::ptr::null_mut();
        }
        crate::tstate::set_string(
            exceptions::system_error(),
            b"coil: EncodingMap objects need the container runtime",
        );
        core::ptr::null_mut()
    }
}

/// `_Py_normalize_encoding`: lowercased, blanks removed, `-` folded to
/// `_`, truncated to `buflen - 1`.
fn normalize_encoding(name: &str, buflen: usize) -> String {
    let mut out = String::with_capacity(buflen - 1);
    for c in name.chars() {
        let mapped = if c == ' ' || c == '\t' {
            continue;
        } else if c == '-' {
            '_'
        } else {
            c.to_ascii_lowercase()
        };
        if out.len() + mapped.len_utf8() < buflen {
            out.push(mapped);
        }
    }
    out
}

/// The built-in decode codec set; names beyond it raise `LookupError`
/// until the codecs registry task.
fn decode_by_name(name: Option<&str>, bytes: &[u8], handler: ErrHandler) -> Option<Vec<u32>> {
    match name {
        None | Some("utf8") | Some("utf_8") => decode_utf8_bytes(bytes, handler),
        Some("utf16") | Some("utf_16") | Some("u16") => {
            let mut order = 0i32;
            decode_utf16_bytes(bytes, handler, Some(&mut order))
        }
        Some("utf32") | Some("utf_32") | Some("u32") => {
            let mut order = 0i32;
            decode_utf32_bytes(bytes, handler, &mut order)
        }
        Some("utf_16le") | Some("utf_16_le") => {
            let mut order = -1i32;
            decode_utf16_bytes(bytes, handler, Some(&mut order))
        }
        Some("utf_16be") | Some("utf_16_be") => {
            let mut order = 1i32;
            decode_utf16_bytes(bytes, handler, Some(&mut order))
        }
        Some("utf_32le") | Some("utf_32_le") => {
            let mut order = -1i32;
            decode_utf32_bytes(bytes, handler, &mut order)
        }
        Some("utf_32be") | Some("utf_32_be") => {
            let mut order = 1i32;
            decode_utf32_bytes(bytes, handler, &mut order)
        }
        Some("ascii")
        | Some("us_ascii")
        | Some("646")
        | Some("ansi_x34_1968")
        | Some("ansi_x3_4_1968") => decode_ascii_bytes(bytes, handler),
        Some("latin1") | Some("latin_1") | Some("iso_8859_1") | Some("iso8859_1") | Some("l1")
        | Some("latin") | Some("8859") | Some("cp819") => {
            Some(bytes.iter().map(|b| *b as u32).collect::<Vec<u32>>())
        }
        Some("unicode_escape") => decode_unicode_escape(bytes, handler, None),
        Some("raw_unicode_escape") => decode_raw_unicode_escape(bytes, handler, None),
        Some(other) => {
            lookup_error_message(format!("unknown encoding: {other}"));
            None
        }
    }
}

/// `PyUnicode_Decode`: the built-in codec set; names beyond it raise
/// `LookupError` until the codecs registry task.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Decode(
    s: *const c_char,
    size: isize,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range and C strings are the caller's contract.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let handler = error_handler(errors_arg(errors));
        let name = if encoding.is_null() {
            None
        } else {
            Some(normalize_encoding(util::cstr(encoding), 32))
        };
        let decoded = decode_by_name(name.as_deref(), bytes, handler);
        let Some(chars) = decoded else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

/// The built-in encode codec set (see `PyUnicode_AsEncodedString`).
fn encode_by_name(name: &str, chars: &[u32], handler: ErrHandler) -> Option<Vec<u8>> {
    match name {
        "utf8" | "utf_8" => encode_utf8_bytes(chars, handler),
        "utf16" | "utf_16" | "u16" => encode_utf16_bytes(chars, 0, handler),
        "utf32" | "utf_32" | "u32" => encode_utf32_bytes(chars, 0, handler),
        "utf_16le" | "utf_16_le" => encode_utf16_bytes(chars, -1, handler),
        "utf_16be" | "utf_16_be" => encode_utf16_bytes(chars, 1, handler),
        "utf_32le" | "utf_32_le" => encode_utf32_bytes(chars, -1, handler),
        "utf_32be" | "utf_32_be" => encode_utf32_bytes(chars, 1, handler),
        "ascii" | "us_ascii" | "646" | "ansi_x34_1968" | "ansi_x3_4_1968" => {
            encode_ucs1(chars, 0x80, handler)
        }
        "latin1" | "latin_1" | "iso_8859_1" | "iso8859_1" | "l1" | "latin" | "8859" | "cp819" => {
            encode_ucs1(chars, 0x100, handler)
        }
        "unicode_escape" | "raw_unicode_escape" => {
            let mut out = Vec::new();
            for &ch in chars {
                if ch < 0x100 {
                    if name.starts_with("raw") {
                        out.push(ch as u8);
                    } else if (0x20..0x7F).contains(&ch) {
                        if ch == b'\\' as u32 {
                            out.push(b'\\');
                            out.push(b'\\');
                        } else {
                            out.push(ch as u8);
                        }
                    } else if ch == 0x09 {
                        out.extend_from_slice(b"\\t");
                    } else if ch == 0x0A {
                        out.extend_from_slice(b"\\n");
                    } else if ch == 0x0D {
                        out.extend_from_slice(b"\\r");
                    } else {
                        out.extend_from_slice(b"\\x");
                        push_hex(&mut out, ch, 2);
                    }
                } else if ch < 0x10000 {
                    out.extend_from_slice(b"\\u");
                    push_hex(&mut out, ch, 4);
                } else {
                    out.extend_from_slice(b"\\U00");
                    push_hex(&mut out, ch, 6);
                }
            }
            Some(out)
        }
        "charmap" => encode_charmap_bytes(chars, core::ptr::null_mut(), handler),
        other => {
            lookup_error_message(format!("unknown encoding: {other}"));
            None
        }
    }
}

/// `PyUnicode_AsEncodedString`: the encoder side of the same table.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedString(
    unicode: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let chars = hay.as_vec();
        let handler = error_handler(errors_arg(errors));
        let name = if encoding.is_null() {
            "utf8".to_string()
        } else {
            normalize_encoding(util::cstr(encoding), 32)
        };
        let bytes = encode_by_name(&name, &chars, handler);
        let Some(bytes) = bytes else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_AsEncodedObject`: like `AsEncodedString` for this runtime —
/// every encoder produces bytes.
///
/// # Safety
///
/// `unicode` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedObject(
    unicode: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: forwards to the checked string path.
    unsafe {
        if !check(unicode) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!(
                    "encoding objects of type {} is not supported",
                    type_name_of(unicode)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        PyUnicode_AsEncodedString(unicode, encoding, errors)
    }
}

fn type_name_of(object: *const PyObject) -> &'static str {
    // SAFETY: the object's type pointer is valid for the object's life.
    unsafe { util::cstr((*header::type_of(object)).tp_name) }
}

/// `PyUnicode_AsDecodedObject`: decode the object's bytes with the named
/// codec.
///
/// # Safety
///
/// `object` must be a valid object; the interim runtime only decodes
/// `bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsDecodedObject(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the object.
    unsafe {
        if !crate::types::bytes::check(object) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!(
                    "decoding objects of type {} is not supported",
                    type_name_of(object)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        let (size, ptr) = crate::types::bytes::as_string_parts(object);
        let bytes = core::slice::from_raw_parts(ptr.cast::<u8>(), size as usize);
        let handler = error_handler(errors_arg(errors));
        let name = if encoding.is_null() {
            "utf8".to_string()
        } else {
            normalize_encoding(util::cstr(encoding), 32)
        };
        let Some(decoded) = decode_by_name(Some(&name), bytes, handler) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&decoded, 0)
    }
}

/// `PyUnicode_AsDecodedUnicode`: the unicode-flavoured alias.
///
/// # Safety
///
/// `object` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsDecodedUnicode(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: forwards to the checked decode path.
    unsafe { PyUnicode_AsDecodedObject(object, encoding, errors) }
}

/// `PyUnicode_AsEncodedUnicode`: the unicode-flavoured alias.
///
/// # Safety
///
/// `unicode` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedUnicode(
    unicode: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: forwards to the checked encode path.
    unsafe { PyUnicode_AsEncodedString(unicode, encoding, errors) }
}

/// `PyUnicode_DecodeFSDefault`: the filesystem codec is UTF-8 with
/// surrogateescape (PEP 529 and the POSIX default).
///
/// # Safety
///
/// `s` must be a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefault(s: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is the caller's contract.
    unsafe {
        let mut len = 0usize;
        while *s.add(len) != 0 {
            len += 1;
        }
        PyUnicode_DecodeFSDefaultAndSize(s, len as isize)
    }
}

/// `PyUnicode_DecodeFSDefaultAndSize`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefaultAndSize(
    s: *const c_char,
    size: isize,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract.
    unsafe {
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let Some(chars) = decode_utf8_bytes(bytes, ErrHandler::SurrogateEscape) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

/// `PyUnicode_EncodeFSDefault`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeFSDefault(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) = encode_utf8_bytes(&hay.as_vec(), ErrHandler::SurrogateEscape) else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_DecodeLocale`: UTF-8 with the handler, the locale encoding
/// on every platform since PEP 529 and UTF-8 mode.
///
/// # Safety
///
/// `str_` must be a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLocale(
    str_: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the C string is the caller's contract.
    unsafe {
        let mut len = 0usize;
        while *str_.add(len) != 0 {
            len += 1;
        }
        PyUnicode_DecodeLocaleAndSize(str_, len as isize, errors)
    }
}

/// `PyUnicode_DecodeLocaleAndSize`.
///
/// # Safety
///
/// `str_` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLocaleAndSize(
    str_: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract.
    unsafe {
        let bytes = core::slice::from_raw_parts(str_.cast::<u8>(), size.max(0) as usize);
        let handler = error_handler(errors_arg(errors));
        let Some(chars) = decode_utf8_bytes(bytes, handler) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, 0)
    }
}

/// `PyUnicode_EncodeLocale`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeLocale(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let Some(bytes) = encode_utf8_bytes(&hay.as_vec(), error_handler(errors_arg(errors)))
        else {
            return core::ptr::null_mut();
        };
        match crate::types::bytes::from_vec(Some(bytes)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

// -------------------------------------------------------------------- repr

/// `unicode_repr`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Repr(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the string.
    unsafe {
        let Some(hay) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let mut out: Vec<u32> = Vec::new();
        let mut squote = 0isize;
        let mut dquote = 0isize;
        for i in 0..hay.len() {
            match hay.get(i) {
                ch if ch == '\'' as u32 => squote += 1,
                ch if ch == '"' as u32 => dquote += 1,
                _ => {}
            }
        }
        let mut quote = '\'' as u32;
        if squote > 0 {
            if dquote > 0 {
                // Both quote kinds present: single quotes, escaped.
            } else {
                quote = '"' as u32;
            }
        }
        out.push(quote);
        for i in 0..hay.len() {
            let ch = hay.get(i);
            if ch == quote || ch == '\\' as u32 {
                out.push('\\' as u32);
                out.push(ch);
            } else if ch == '\t' as u32 {
                out.extend_from_slice(&['\\' as u32, 't' as u32]);
            } else if ch == '\n' as u32 {
                out.extend_from_slice(&['\\' as u32, 'n' as u32]);
            } else if ch == '\r' as u32 {
                out.extend_from_slice(&['\\' as u32, 'r' as u32]);
            } else if ch < 0x20 || ch == 0x7F {
                out.extend_from_slice(&['\\' as u32, 'x' as u32]);
                let hi = (ch >> 4) & 0xF;
                let lo = ch & 0xF;
                out.push(*HEX_DIGITS.as_ptr().wrapping_add(hi as usize) as u32);
                out.push(*HEX_DIGITS.as_ptr().wrapping_add(lo as usize) as u32);
            } else if ch < 0x7F {
                out.push(ch);
            } else if !is_printable(ch) {
                if ch < 0x100 {
                    out.extend_from_slice(&['\\' as u32, 'x' as u32]);
                    let hi = (ch >> 4) & 0xF;
                    let lo = ch & 0xF;
                    out.push(*HEX_DIGITS.as_ptr().wrapping_add(hi as usize) as u32);
                    out.push(*HEX_DIGITS.as_ptr().wrapping_add(lo as usize) as u32);
                } else if ch < 0x10000 {
                    out.extend_from_slice(&['\\' as u32, 'u' as u32]);
                    for shift in (0..4).rev() {
                        let digit = (ch >> (shift * 4)) & 0xF;
                        out.push(*HEX_DIGITS.as_ptr().wrapping_add(digit as usize) as u32);
                    }
                } else {
                    out.extend_from_slice(&['\\' as u32, 'U' as u32]);
                    for shift in (0..8).rev() {
                        let digit = (ch >> (shift * 4)) & 0xF;
                        out.push(*HEX_DIGITS.as_ptr().wrapping_add(digit as usize) as u32);
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out.push(quote);
        chars_to_object(&out, 0)
    }
}

// ---------------------------------------------------- the generic object API

/// `PyObject_Repr`: the `tp_repr` slot, or CPython's placeholder for
/// slot-less types.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Repr(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: slot reads and calls respect the object's type.
    unsafe {
        if v.is_null() {
            return core::ptr::null_mut();
        }
        let ty = header::type_of(v);
        let repr = if (*ty).tp_repr.is_none() {
            from_rust(&format!(
                "<{} object at {:?}>",
                util::cstr((*ty).tp_name),
                v as *const c_void
            ))
        } else {
            ((*ty).tp_repr.unwrap())(v)
        };
        if repr.is_null() {
            return repr;
        }
        if !check(repr) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("__repr__ returned non-string (type {})", type_name_of(repr)).as_bytes(),
            );
            return core::ptr::null_mut();
        }
        repr
    }
}

/// `PyObject_Str`: the `tp_str` slot, falling back to the repr.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Str(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: slot reads and calls respect the object's type.
    unsafe {
        if v.is_null() {
            return core::ptr::null_mut();
        }
        if check_exact(v) {
            incref(v);
            return v;
        }
        let ty = header::type_of(v);
        if (*ty).tp_str.is_none() {
            return PyObject_Repr(v);
        }
        let res = ((*ty).tp_str.unwrap())(v);
        if res.is_null() {
            return res;
        }
        if !check(res) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("__str__ returned non-string (type {})", type_name_of(res)).as_bytes(),
            );
            return core::ptr::null_mut();
        }
        res
    }
}

/// `PyObject_ASCII`: the ASCII-safe spelling, which is `repr` for every
/// type whose repr escapes (all of them here).
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_ASCII(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: forwards to the checked repr path.
    unsafe { PyObject_Repr(v) }
}

// ---------------------------------------------------------- %-formatting

/// The flags of CPython's `PyUnicode_Format` state.
const F_LJUST: c_int = 0x01;
const F_SIGN: c_int = 0x02;
const F_BLANK: c_int = 0x04;
const F_ALT: c_int = 0x08;
const F_ZERO: c_int = 0x10;

/// `PyNumber_Long` for the `%d` family: exact values pass through, others
/// through their `nb_int` slot.
pub(crate) unsafe fn number_long(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: slot reads and calls respect the object's type.
    unsafe {
        if crate::types::long::is_long(v) {
            incref(v);
            return v;
        }
        let ty = header::type_of(v);
        let as_number = (*ty).tp_as_number;
        if as_number.is_null() {
            return core::ptr::null_mut();
        }
        match (*as_number).nb_int {
            Some(nb_int) => nb_int(v),
            None => core::ptr::null_mut(),
        }
    }
}

/// `_PyUnicode_FormatLong`: the digits from the long formatter, prefix
/// stripped unless alternate, zero-filled to the precision.
///
/// # Safety
///
/// `val` must be an int (or subclass).
#[allow(non_snake_case)]
pub unsafe extern "C" fn _PyUnicode_FormatLong(
    val: *mut PyObject,
    alt: c_int,
    prec: c_int,
    type_: c_char,
) -> *mut PyObject {
    // SAFETY: the object is a long per the contract.
    unsafe {
        let base: u32 = match type_ as u8 {
            b'o' => 8,
            b'x' | b'X' => 16,
            _ => 10,
        };
        let a = val as *const crate::types::long::PyLongObject;
        let Some(digits) = crate::types::long::format(a, base, true) else {
            return core::ptr::null_mut();
        };
        let mut body: Vec<u8> = digits.into_bytes();
        let sign = !body.is_empty() && body[0] == b'-';
        let mut nondigits = if base == 10 { 0 } else { 2 };
        if alt == 0 && base != 10 {
            // Strip the 0o/0x marker.
            let at = if sign { 1 } else { 0 };
            body.drain(at..at + 2);
            nondigits -= 2;
            if sign {
                body[0] = b'-';
            }
        }
        nondigits += sign as isize as usize;
        let mut len = body.len();
        let numdigits = len - nondigits;
        if prec as isize > numdigits as isize {
            let mut filled = Vec::with_capacity(nondigits + prec as usize);
            let head = if sign { 1 } else { 0 };
            filled.extend_from_slice(&body[..head + nondigits - sign as usize]);
            filled.extend(core::iter::repeat_n(b'0', prec as usize - numdigits));
            filled.extend_from_slice(&body[head + nondigits - sign as usize..]);
            body = filled;
            len = body.len();
            nondigits += prec as usize - numdigits;
        }
        if type_ as u8 == b'X' && base != 10 {
            for byte in body.iter_mut() {
                if byte.is_ascii_lowercase() {
                    byte.make_ascii_uppercase();
                }
            }
        }
        let Some(text) = String::from_utf8(body).ok() else {
            crate::tstate::set_string(exceptions::system_error(), b"non-ASCII long digits");
            return core::ptr::null_mut();
        };
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        let _ = len;
        let _ = nondigits;
        chars_to_object(&chars, 0x7F)
    }
}

/// `formatchar`: one code point from a one-character string or an int.
unsafe fn formatchar(v: *mut PyObject) -> Option<u32> {
    // SAFETY: the object is valid.
    unsafe {
        if check(v) {
            if get_length(v) == 1 {
                return Some(as_chars(v)?.get(0));
            }
            crate::tstate::set_string(exceptions::type_error(), b"%c requires int or char");
            return None;
        }
        if !crate::types::long::is_long(v) {
            crate::tstate::set_string(exceptions::type_error(), b"%c requires int or char");
            return None;
        }
        let mut overflow: c_int = 0;
        let x = crate::types::long::PyLong_AsLongAndOverflow(v, &mut overflow);
        if x == -1 && !crate::tstate::occurred().is_null() {
            return None;
        }
        if x < 0 || x as u64 > MAX_UNICODE as u64 {
            // The overflow path lands here too.
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"%c arg not in range(0x110000)",
            );
            return None;
        }
        Some(x as u32)
    }
}

/// `PyUnicode_Format`: `%`-formatting with a single replacement value —
/// the `"fmt" % value` form. Tuple and mapping arguments arrive with the
/// container task, like the `nb_divmod` slot did in the numbers task.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Format(
    format: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: reads stay within the format; args dispatch is type-checked.
    unsafe {
        if format.is_null() || args.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        if !check(format) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("format requires str, not {}", type_name_of(format)).as_bytes(),
            );
            return core::ptr::null_mut();
        }
        let Some(fmt) = as_chars(format) else {
            return core::ptr::null_mut();
        };
        // The single-value form: one argument available, then no more.
        let arglen: isize = -1;
        let mut argidx: isize = -2;
        let mut out: Vec<u32> = Vec::with_capacity(fmt.len() as usize);
        let mut fmtpos: isize = 0;
        let mut fmtcnt: isize = fmt.len();
        let mut ok = true;
        while fmtcnt > 0 {
            fmtcnt -= 1;
            if fmt.get(fmtpos) != '%' as u32 {
                let nonfmtpos = fmtpos;
                fmtpos += 1;
                while fmtcnt >= 0 && fmt.get(fmtpos) != '%' as u32 {
                    fmtpos += 1;
                    fmtcnt -= 1;
                }
                if fmtcnt < 0 {
                    fmtpos -= 1;
                }
                out.extend_from_slice(&fmt.slice(nonfmtpos, fmtpos));
            } else {
                fmtpos += 1;
                if !format_one_arg(
                    fmt,
                    &mut fmtpos,
                    &mut fmtcnt,
                    args,
                    arglen,
                    &mut argidx,
                    &mut out,
                ) {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            return core::ptr::null_mut();
        }
        // `unicode_format`'s tail: every provided argument must have been
        // consumed by a placeholder.
        if (arglen >= 0 && argidx < arglen) || (arglen < 0 && argidx == -2) {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"not all arguments converted during string formatting",
            );
            return core::ptr::null_mut();
        }
        chars_to_object(&out, 0)
    }
}

/// `unicode_format_getnextarg`: the whole single argument, then no more.
/// Tuple arguments need the container runtime (see `PyUnicode_Format`).
unsafe fn next_arg(args: *mut PyObject, arglen: isize, argidx: &mut isize) -> *mut PyObject {
    if *argidx < arglen {
        *argidx += 1;
        if arglen < 0 {
            return args;
        }
        crate::tstate::set_string(
            exceptions::system_error(),
            b"coil: PyUnicode_Format needs tuple arguments from the container runtime",
        );
        return core::ptr::null_mut();
    }
    crate::tstate::set_string(
        exceptions::type_error(),
        b"not enough arguments for format string",
    );
    core::ptr::null_mut()
}

/// `unicode_format_arg`: parse flags/width/precision, format the value,
/// and append the padded output.
unsafe fn format_one_arg(
    fmt: Chars,
    fmtpos: &mut isize,
    fmtcnt: &mut isize,
    args: *mut PyObject,
    arglen: isize,
    argidx: &mut isize,
    out: &mut Vec<u32>,
) -> bool {
    // SAFETY: reads stay within the format; argument dispatch checks types.
    unsafe {
        let mut ch = fmt.get(*fmtpos);
        if ch == '%' as u32 {
            *fmtpos += 1;
            *fmtcnt -= 1;
            out.push('%' as u32);
            return true;
        }
        let mut flags: c_int = 0;
        let mut width: isize = -1;
        let mut prec: c_int = -1;
        let mut sign_flag = false;

        // "%(name)" needs a mapping; none exists in this runtime yet.
        if ch == '(' as u32 {
            crate::tstate::set_string(exceptions::type_error(), b"format requires a mapping");
            return false;
        }

        while *fmtcnt > 0 {
            *fmtcnt -= 1;
            ch = fmt.get(*fmtpos);
            *fmtpos += 1;
            match ch as u8 as char {
                '-' => {
                    flags |= F_LJUST;
                    continue;
                }
                '+' => {
                    flags |= F_SIGN;
                    continue;
                }
                ' ' => {
                    flags |= F_BLANK;
                    continue;
                }
                '#' => {
                    flags |= F_ALT;
                    continue;
                }
                '0' => {
                    flags |= F_ZERO;
                    continue;
                }
                _ => {}
            }
            break;
        }

        if ch == '*' as u32 {
            let v = next_arg(args, arglen, argidx);
            if v.is_null() {
                return false;
            }
            if !crate::types::long::is_long(v) {
                crate::tstate::set_string(exceptions::type_error(), b"* wants int");
                return false;
            }
            width = crate::types::long::PyLong_AsSsize_t(v);
            if width == -1 && !crate::tstate::occurred().is_null() {
                return false;
            }
            if width < 0 {
                flags |= F_LJUST;
                width = -width;
            }
            if *fmtcnt > 0 {
                *fmtcnt -= 1;
                ch = fmt.get(*fmtpos);
                *fmtpos += 1;
            }
        } else if (b'0' as u32..=b'9' as u32).contains(&ch) {
            width = (ch as u8 - b'0') as isize;
            while *fmtcnt > 0 {
                *fmtcnt -= 1;
                ch = fmt.get(*fmtpos);
                *fmtpos += 1;
                if !(b'0' as u32..=b'9' as u32).contains(&ch) {
                    break;
                }
                if width > (isize::MAX - (ch as u8 - b'0') as isize) / 10 {
                    crate::tstate::set_string(exceptions::value_error(), b"width too big");
                    return false;
                }
                width = width * 10 + (ch as u8 - b'0') as isize;
            }
        }

        if ch == '.' as u32 {
            prec = 0;
            if *fmtcnt > 0 {
                *fmtcnt -= 1;
                ch = fmt.get(*fmtpos);
                *fmtpos += 1;
            }
            if ch == '*' as u32 {
                let v = next_arg(args, arglen, argidx);
                if v.is_null() {
                    return false;
                }
                if !crate::types::long::is_long(v) {
                    crate::tstate::set_string(exceptions::type_error(), b"* wants int");
                    return false;
                }
                prec = crate::types::long::PyLong_AsSsize_t(v) as c_int;
                if prec < 0 {
                    prec = 0;
                }
                if *fmtcnt > 0 {
                    *fmtcnt -= 1;
                    ch = fmt.get(*fmtpos);
                    *fmtpos += 1;
                }
            } else if (b'0' as u32..=b'9' as u32).contains(&ch) {
                prec = (ch as u8 - b'0') as c_int;
                while *fmtcnt > 0 {
                    *fmtcnt -= 1;
                    ch = fmt.get(*fmtpos);
                    *fmtpos += 1;
                    if !(b'0' as u32..=b'9' as u32).contains(&ch) {
                        break;
                    }
                    if prec > (c_int::MAX - (ch as u8 - b'0') as c_int) / 10 {
                        crate::tstate::set_string(exceptions::value_error(), b"precision too big");
                        return false;
                    }
                    prec = prec * 10 + (ch as u8 - b'0') as c_int;
                }
            }
        }

        if *fmtcnt > 0 && (ch == 'h' as u32 || ch == 'l' as u32 || ch == 'L' as u32) {
            *fmtcnt -= 1;
            ch = fmt.get(*fmtpos);
            *fmtpos += 1;
        }
        if *fmtcnt < 0 {
            crate::tstate::set_string(exceptions::value_error(), b"incomplete format");
            return false;
        }

        // Format the value.
        let v = next_arg(args, arglen, argidx);
        if v.is_null() {
            return false;
        }
        let piece = match ch as u8 as char {
            's' | 'r' | 'a' => {
                let str_obj = if ch == 's' as u32 {
                    PyObject_Str(v)
                } else {
                    PyObject_Repr(v)
                };
                if str_obj.is_null() {
                    return false;
                }
                let Some(view) = as_chars(str_obj) else {
                    decref(str_obj);
                    return false;
                };
                let mut text = view.as_vec();
                decref(str_obj);
                if prec >= 0 && text.len() as c_int > prec {
                    text.truncate(prec as usize);
                }
                Some(text)
            }
            'i' | 'd' | 'u' | 'o' | 'x' | 'X' => {
                let type_ = ch as u8;
                sign_flag = true;
                if !crate::types::long::is_long(v) {
                    let iobj = number_long(v);
                    if iobj.is_null() {
                        crate::tstate::set_string(
                            exceptions::type_error(),
                            format!(
                                "%{} format: a real number is required, not {}",
                                ch as u8 as char,
                                type_name_of(v)
                            )
                            .as_bytes(),
                        );
                        return false;
                    }
                    let res = _PyUnicode_FormatLong(
                        iobj,
                        (flags & F_ALT) as c_int,
                        prec,
                        type_ as c_char,
                    );
                    decref(iobj);
                    if res.is_null() {
                        return false;
                    }
                    // Copy before the decref: `_Py_Dealloc` may free —
                    // and with debug-heap poison — the string the view
                    // borrows from.
                    let text = as_chars(res).unwrap().as_vec();
                    decref(res);
                    Some(text)
                } else {
                    let res =
                        _PyUnicode_FormatLong(v, (flags & F_ALT) as c_int, prec, type_ as c_char);
                    if res.is_null() {
                        return false;
                    }
                    // As above: the copy precedes the decref it outlives.
                    let text = as_chars(res).unwrap().as_vec();
                    decref(res);
                    Some(text)
                }
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let x = crate::types::float_::PyFloat_AsDouble(v);
                if x.is_nan() && !crate::tstate::occurred().is_null() {
                    return false;
                }
                let mut p = prec;
                if p < 0 {
                    p = 6;
                }
                let mode = ch as u8;
                let mut dtoa_flags: c_int = 0;
                if flags & F_ALT != 0 {
                    dtoa_flags |= 16; // Py_DTSF_ALT
                }
                let Some(text) = crate::types::float_::double_to_string(
                    x,
                    mode,
                    p,
                    dtoa_flags,
                    core::ptr::null_mut(),
                ) else {
                    return false;
                };
                sign_flag = true;
                Some(text.chars().map(|c| c as u32).collect())
            }
            'c' => {
                let Some(single) = formatchar(v) else {
                    return false;
                };
                if width == -1 && prec == -1 {
                    out.push(single);
                    return true;
                }
                Some(vec![single])
            }
            _ => {
                let display = if (31..=126).contains(&ch) {
                    ch as u8 as char
                } else {
                    '?'
                };
                crate::tstate::set_string(
                    exceptions::value_error(),
                    format!(
                        "unsupported format character '{}' (0x{:x}) at index {}",
                        display,
                        ch,
                        *fmtpos - 1
                    )
                    .as_bytes(),
                );
                return false;
            }
        };
        let Some(piece) = piece else {
            return false;
        };
        // `unicode_format_arg_output`: sign, zero fill, alignment.
        let fill = if sign_flag && flags & F_ZERO != 0 {
            '0' as u32
        } else {
            ' ' as u32
        };
        let mut len = piece.len() as isize;
        if (width == -1 || width <= len)
            && (prec == -1 || prec as isize >= len)
            && flags & (F_SIGN | F_BLANK) == 0
        {
            out.extend_from_slice(&piece);
            return true;
        }
        let mut pindex = 0usize;
        let mut signchar = 0u32;
        if sign_flag {
            let first = piece[0];
            if first == '-' as u32 || first == '+' as u32 {
                signchar = first;
                len -= 1;
                pindex = 1;
            } else if flags & F_SIGN != 0 {
                signchar = '+' as u32;
            } else if flags & F_BLANK != 0 {
                signchar = ' ' as u32;
            }
        }
        if width < len {
            width = len;
        }
        if signchar != 0 && fill != ' ' as u32 {
            out.push(signchar);
            if width > len {
                width -= 1;
            }
        }
        if flags & F_ALT != 0
            && matches!(ch as u8 as char, 'x' | 'X' | 'o')
            && piece.len() >= pindex + 2
        {
            if fill != ' ' as u32 {
                out.push('0' as u32);
                out.push(ch);
                pindex += 2;
            }
            width -= 2;
            if width < 0 {
                width = 0;
            }
            len -= 2;
        }
        if width > len && flags & F_LJUST == 0 {
            out.extend(core::iter::repeat_n(fill, (width - len) as usize));
            width = len;
        }
        if fill == ' ' as u32 {
            if signchar != 0 {
                out.push(signchar);
            }
            if flags & F_ALT != 0
                && matches!(ch as u8 as char, 'x' | 'X' | 'o')
                && piece.len() >= pindex + 2
            {
                out.push('0' as u32);
                out.push(ch);
                pindex += 2;
            }
        }
        let end = ((pindex as isize) + len).max(pindex as isize) as usize;
        out.extend_from_slice(&piece[pindex..end.min(piece.len())]);
        if width > len {
            out.extend(core::iter::repeat_n(' ' as u32, (width - len) as usize));
        }
        true
    }
}

// ------------------------------------------------------ format() mini-language

/// `_PyUnicode_FormatAdvancedWriter` for an exact string: parse the spec
/// with the str defaults and pad or truncate. Returns `None` with the
/// exception set.
pub fn format_advanced(value: &[u32], spec: &str) -> Option<Vec<u32>> {
    if spec.is_empty() {
        return Some(value.to_vec());
    }
    let format = crate::types::float_::parse_format_spec(spec, "str", 's', '<')?;
    if format.type_ != 's' {
        crate::types::float_::unknown_presentation_type(format.type_, "str");
        return None;
    }
    // `format_string_internal`: the string restrictions.
    if format.sign != '\0' {
        if format.sign == ' ' {
            crate::types::float_::value_error("Space not allowed in string format specifier");
        } else {
            crate::types::float_::value_error("Sign not allowed in string format specifier");
        }
        return None;
    }
    if format.no_neg_0 {
        crate::types::float_::value_error(
            "Negative zero coercion (z) not allowed in string format specifier",
        );
        return None;
    }
    if format.alternate {
        crate::types::float_::value_error(
            "Alternate form (#) not allowed in string format specifier",
        );
        return None;
    }
    if format.align == '=' {
        crate::types::float_::value_error("'=' alignment not allowed in string format specifier");
        return None;
    }
    let mut len = value.len() as isize;
    if (format.width == -1 || format.width <= len)
        && (format.precision == -1 || format.precision >= len)
    {
        return Some(value.to_vec());
    }
    if format.precision >= 0 && len >= format.precision {
        len = format.precision;
    }
    let total = if format.width >= 0 {
        len.max(format.width)
    } else {
        len
    };
    let lpad = match format.align {
        '>' => total - len,
        '^' => (total - len) / 2,
        _ => 0,
    };
    let rpad = total - len - lpad;
    let mut out: Vec<u32> = Vec::with_capacity(total as usize);
    out.extend(core::iter::repeat_n(format.fill_char as u32, lpad as usize));
    out.extend_from_slice(&value[..len as usize]);
    out.extend(core::iter::repeat_n(format.fill_char as u32, rpad as usize));
    Some(out)
}

// -------------------------------------------------------------- iterators

/// The `str_iterator` object: exact layout — header, `it_index`, `it_seq`.
#[repr(C)]
pub struct PyUnicodeIterObject {
    pub ob_base: PyObject,
    pub it_index: isize,
    pub it_seq: *mut PyObject,
}

unsafe extern "C" fn unicode_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    // SAFETY: the iterator layout reads stay in bounds.
    unsafe {
        let iter = iterator as *mut PyUnicodeIterObject;
        let seq = (*iter).it_seq;
        if (*iter).it_index < get_length(seq) {
            let ch = read(kind(seq), data(seq), (*iter).it_index);
            (*iter).it_index += 1;
            from_char(ch)
        } else {
            core::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn unicode_iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from the runtime allocator.
    unsafe {
        let iter = object as *mut PyUnicodeIterObject;
        decref((*iter).it_seq);
        crate::alloc::free_block(object.cast());
    }
}

const fn unicode_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"str_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyUnicodeIterObject>() as isize;
    t.tp_flags = coil_capi_sys::Py_TPFLAGS_DEFAULT;
    t.tp_dealloc = Some(unicode_iter_dealloc);
    t.tp_iternext = Some(unicode_iter_next);
    t
}

/// CPython's `PyUnicodeIter_Type`.
#[unsafe(no_mangle)]
pub static mut PyUnicodeIter_Type: PyTypeObject = unicode_iter_type();

/// `PyObject_GetIter` for strings.
///
/// # Safety
///
/// `str_` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unicode_iter(str_: *mut PyObject) -> *mut PyObject {
    // SAFETY: allocation and header writes stay within the new block.
    unsafe {
        let block = crate::alloc::alloc_object(core::mem::size_of::<PyUnicodeIterObject>());
        if block.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        header::c_init(block.cast(), &raw mut PyUnicodeIter_Type);
        let iter = block as *mut PyUnicodeIterObject;
        (*iter).it_index = 0;
        (*iter).it_seq = str_;
        incref(str_);
        block.cast()
    }
}

// ----------------------------------------------------------- resize/append

/// `PyUnicode_Resize`: shrink in place, grow within the block's capacity,
/// copy on grow past it. The string must be unshared (refcount 1, not
/// interned, not static).
///
/// # Safety
///
/// `unicode` must point to a writable string pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Resize(unicode: *mut *mut PyObject, length: isize) -> isize {
    // SAFETY: layout writes stay within the resized block.
    unsafe {
        let self_ = *unicode;
        if !unicode_modifiable(self_) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Cannot resize a shared unicode object",
            );
            return -1;
        }
        let old = get_length(self_);
        let char_size = match kind(self_) {
            PY_UNICODE_2BYTE_KIND => 2usize,
            PY_UNICODE_4BYTE_KIND => 4,
            _ => 1,
        };
        if length <= old {
            let data = data_u8(self_).add((length.max(0) as usize) * char_size);
            core::ptr::write_bytes(data, 0, (old - length.max(0)) as usize * char_size);
            (*(self_ as *mut PyASCIIObject)).length = length;
            return 0;
        }
        let new_total = coil_rt_abi::cp312::ascii::SIZEOF + (length as usize + 1) * char_size;
        let capacity = crate::alloc::block_capacity(self_ as *const u8);
        match capacity {
            Some(capacity) if capacity >= new_total => {
                let data = data_u8(self_).add(old as usize * char_size);
                core::ptr::write_bytes(data, 0, (length - old + 1) as usize * char_size);
                (*(self_ as *mut PyASCIIObject)).length = length;
                0
            }
            _ => {
                let maxchar = max_char_value(self_);
                let fresh = unicode_new(length, maxchar);
                if fresh.is_null() {
                    return -1;
                }
                fast_copy_characters(fresh, 0, self_, 0, old);
                crate::alloc::free_block(self_.cast());
                *unicode = fresh;
                0
            }
        }
    }
}

/// `unicode_modifiable`: unshared, not interned, not statically
/// allocated.
pub(crate) unsafe fn unicode_modifiable(unicode: *mut PyObject) -> bool {
    // SAFETY: header and state reads stay in bounds.
    unsafe {
        header::refcnt(unicode) == 1
            && (*(unicode as *const PyASCIIObject)).state.interned() == SSTATE_NOT_INTERNED
            && !(*(unicode as *const PyASCIIObject))
                .state
                .statically_allocated()
    }
}

/// `PyUnicode_Append`: in-place append onto an unshared string.
///
/// # Safety
///
/// `pleft` must point to a writable string pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Append(pleft: *mut *mut PyObject, right: *mut PyObject) {
    // SAFETY: layout writes stay within the reallocated block.
    unsafe {
        let left = *pleft;
        if left.is_null() || right.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return;
        }
        if !unicode_modifiable(left) {
            let fresh = PyUnicode_Concat(left, right);
            decref(left);
            *pleft = fresh;
            return;
        }
        let left_len = get_length(left);
        let right_len = get_length(right);
        if right_len == 0 {
            return;
        }
        if left_len == 0 {
            *pleft = right;
            return;
        }
        let maxchar = max_char_value(left).max(max_char_value(right));
        let target_kind = if maxchar < 0x80
            || kind(left) == PY_UNICODE_1BYTE_KIND
                && maxchar < 0x100
                && kind(right) == PY_UNICODE_1BYTE_KIND
        {
            PY_UNICODE_1BYTE_KIND
        } else if maxchar < 0x10000
            && kind(left) != PY_UNICODE_4BYTE_KIND
            && kind(right) != PY_UNICODE_4BYTE_KIND
        {
            PY_UNICODE_2BYTE_KIND
        } else {
            PY_UNICODE_4BYTE_KIND
        };
        if target_kind != kind(left) {
            // Kind widening: build a fresh string.
            let fresh = PyUnicode_Concat(left, right);
            decref(left);
            *pleft = fresh;
            return;
        }
        let old_total = coil_rt_abi::cp312::ascii::SIZEOF
            + (left_len as usize + 1) * char_size_of_kind(target_kind);
        let new_total = coil_rt_abi::cp312::ascii::SIZEOF
            + (left_len as usize + right_len as usize + 1) * char_size_of_kind(target_kind);
        let capacity = crate::alloc::block_capacity(left as *const u8);
        if matches!(capacity, Some(capacity) if capacity >= new_total) {
            fast_copy_characters(left, left_len, right, 0, right_len);
            (*(left as *mut PyASCIIObject)).length = left_len + right_len;
        } else {
            let fresh = PyUnicode_Concat(left, right);
            decref(left);
            *pleft = fresh;
        }
        let _ = old_total;
        decref(right);
    }
}

fn char_size_of_kind(object_kind: c_int) -> usize {
    match object_kind {
        PY_UNICODE_2BYTE_KIND => 2,
        PY_UNICODE_4BYTE_KIND => 4,
        _ => 1,
    }
}

/// `PyUnicode_AppendAndDel`.
///
/// # Safety
///
/// Both pointer slots must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AppendAndDel(pleft: *mut *mut PyObject, right: *mut PyObject) {
    // SAFETY: forwards to the checked append.
    unsafe {
        let left = *pleft;
        PyUnicode_Append(pleft, right);
        decref(right);
        let _ = left;
    }
}

// ----------------------------------------------------------- the type object

unsafe extern "C" fn unicode_str(self_: *mut PyObject) -> *mut PyObject {
    // `str.__str__` is the identity; the write is `incref`'s own word.
    incref(self_);
    self_
}

unsafe extern "C" fn unicode_hash_slot(self_: *mut PyObject) -> isize {
    // SAFETY: the string is a valid object.
    unsafe { unicode_hash(self_) }
}

unsafe extern "C" fn unicode_length(self_: *mut PyObject) -> isize {
    // SAFETY: the length read is within the header.
    unsafe { get_length(self_) }
}

unsafe extern "C" fn unicode_getitem(self_: *mut PyObject, index: isize) -> *mut PyObject {
    // SAFETY: the index is checked before the read.
    unsafe {
        if index < 0 || index >= get_length(self_) {
            crate::tstate::set_string(exceptions::index_error(), b"string index out of range");
            return core::ptr::null_mut();
        }
        let ch = read(kind(self_), data(self_), index);
        from_char(ch)
    }
}

const fn unicode_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"str".as_ptr();
    // CPython uses the full legacy struct size even for compact strings.
    t.tp_basicsize = coil_rt_abi::cp312::unicode::SIZEOF as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_UNICODE_SUBCLASS;
    t.tp_dealloc = Some(unicode_dealloc);
    t.tp_repr = Some(PyUnicode_Repr);
    t.tp_str = Some(unicode_str);
    t.tp_hash = Some(unicode_hash_slot);
    t.tp_richcompare = Some(PyUnicode_RichCompare);
    t.tp_iter = Some(unicode_iter);
    t.tp_as_number = &raw mut UNICODE_NUMBER;
    t.tp_as_sequence = &raw mut UNICODE_SEQUENCE;
    t.tp_as_mapping = &raw mut UNICODE_MAPPING;
    t
}

unsafe extern "C" fn unicode_mod(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
    // SAFETY: the check is the slot contract.
    unsafe {
        if !check(v) {
            return crate::object::singletons::not_implemented();
        }
        PyUnicode_Format(v, w)
    }
}

static mut UNICODE_NUMBER: PyNumberMethods = PyNumberMethods {
    nb_add: None,
    nb_subtract: None,
    nb_multiply: None,
    nb_remainder: Some(unicode_mod),
    nb_divmod: None,
    nb_power: None,
    nb_negative: None,
    nb_positive: None,
    nb_absolute: None,
    nb_bool: None,
    nb_invert: None,
    nb_lshift: None,
    nb_rshift: None,
    nb_and: None,
    nb_xor: None,
    nb_or: None,
    nb_int: None,
    nb_reserved: core::ptr::null_mut(),
    nb_float: None,
    nb_inplace_add: None,
    nb_inplace_subtract: None,
    nb_inplace_multiply: None,
    nb_inplace_remainder: None,
    nb_inplace_power: None,
    nb_inplace_lshift: None,
    nb_inplace_rshift: None,
    nb_inplace_and: None,
    nb_inplace_xor: None,
    nb_inplace_or: None,
    nb_floor_divide: None,
    nb_true_divide: None,
    nb_inplace_floor_divide: None,
    nb_inplace_true_divide: None,
    nb_index: None,
    nb_matrix_multiply: None,
    nb_inplace_matrix_multiply: None,
};

unsafe extern "C" fn unicode_dealloc(object: *mut PyObject) {
    // SAFETY: the string is a heap object this module allocated; static
    // singletons are never deallocated because their refcounts never drop.
    unsafe {
        if (*(object as *const PyASCIIObject))
            .state
            .statically_allocated()
        {
            return;
        }
        free_unicode(object);
    }
}

/// CPython's `PyUnicode_Type`.
#[unsafe(no_mangle)]
pub static mut PyUnicode_Type: PyTypeObject = unicode_type();

static mut UNICODE_SEQUENCE: PySequenceMethods = PySequenceMethods {
    sq_length: Some(unicode_length),
    sq_concat: Some(PyUnicode_Concat),
    sq_repeat: Some(unicode_sq_repeat),
    sq_item: Some(unicode_getitem),
    was_sq_slice: core::ptr::null_mut(),
    sq_ass_item: None,
    was_sq_ass_slice: core::ptr::null_mut(),
    sq_contains: Some(PyUnicode_Contains),
    sq_inplace_concat: None,
    sq_inplace_repeat: None,
};

static mut UNICODE_MAPPING: PyMappingMethods = PyMappingMethods {
    mp_length: Some(unicode_length),
    mp_subscript: Some(unicode_mp_subscript),
    mp_ass_subscript: None,
};

unsafe extern "C" fn unicode_sq_repeat(self_: *mut PyObject, n: isize) -> *mut PyObject {
    // SAFETY: the object is a valid string.
    unsafe { repeat(self_, n) }
}

unsafe extern "C" fn unicode_mp_subscript(
    self_: *mut PyObject,
    key: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe { subscript(self_, key) }
}

// ----------------------------------------------------- remaining C API

/// `PyUnicode_FromKindAndData`.
///
/// # Safety
///
/// `s` must hold `size` characters of `kind`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromKindAndData(
    kind_: c_int,
    s: *const c_void,
    size: isize,
) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        if size < 0 {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let maxchar = match kind_ {
            PY_UNICODE_1BYTE_KIND => 0xFF,
            PY_UNICODE_2BYTE_KIND => 0xFFFF,
            PY_UNICODE_4BYTE_KIND => MAX_UNICODE,
            _ => {
                crate::tstate::set_string(exceptions::system_error(), b"invalid kind");
                return core::ptr::null_mut();
            }
        };
        let unicode = unicode_new(size, maxchar);
        if unicode.is_null() {
            return unicode;
        }
        copy_chars_raw(data(unicode), kind(unicode), s, kind_, size);
        let max = max_char_value(unicode);
        if max != maxchar {
            // CPython compacts after reading the data.
            let compacted = unicode_new(size, max);
            if compacted.is_null() {
                return compacted;
            }
            fast_copy_characters(compacted, 0, unicode, 0, size);
            free_unicode(unicode);
            return compacted;
        }
        unicode
    }
}

/// `PyUnicode_FromObject`: exact strings pass through, subclasses are
/// copied, others raise `TypeError`.
///
/// # Safety
///
/// `obj` must be a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromObject(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: reads stay within the object.
    unsafe {
        if obj.is_null() || core::ptr::eq(obj, singletons::none()) {
            crate::tstate::set_string(
                exceptions::type_error(),
                "expected str, got NoneType".as_bytes(),
            );
            return core::ptr::null_mut();
        }
        if check_exact(obj) {
            incref(obj);
            return obj;
        }
        if check(obj) {
            return copy(obj);
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!(
                "coercing to str: need str, bytes-like object or {} found",
                type_name_of(obj)
            )
            .as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// `PyUnicode_FromEncodedObject`: bytes and bytearray decode; str copies.
///
/// # Safety
///
/// `obj` must be a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromEncodedObject(
    obj: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: reads stay within the object.
    unsafe {
        if obj.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        if check(obj) {
            if get_length(obj) == 0 {
                crate::tstate::set_string(
                    exceptions::value_error(),
                    "encoding with a 'None' codec is not allowed".as_bytes(),
                );
                return core::ptr::null_mut();
            }
            if encoding.is_null() {
                crate::tstate::set_string(
                    exceptions::type_error(),
                    "decoding str is not supported".as_bytes(),
                );
                return core::ptr::null_mut();
            }
            return copy(obj);
        }
        if crate::types::bytes::check(obj) {
            let (size, ptr) = crate::types::bytes::as_string_parts(obj);
            let decoded = PyUnicode_Decode(ptr.cast(), size, encoding, errors);
            return decoded;
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!(
                "decoding to str: need a bytes-like object, {} found",
                type_name_of(obj)
            )
            .as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// `PyUnicode_FromOrdinal`.
///
/// # Safety
///
/// `ordinal` must be at most `MAX_UNICODE`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromOrdinal(ordinal: c_int) -> *mut PyObject {
    // SAFETY: the range check guards the kind selection.
    unsafe {
        if ordinal < 0 || ordinal as u32 > MAX_UNICODE {
            crate::tstate::set_string(
                exceptions::value_error(),
                "%c arg not in range(0x110000)".as_bytes(),
            );
            return core::ptr::null_mut();
        }
        from_char(ordinal as u32)
    }
}

/// `PyUnicode_FromWideChar` over 16-bit `wchar_t` (the Windows model):
/// surrogate pairs join, lone surrogates pass through.
///
/// # Safety
///
/// `w` must point to `size` 16-bit units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromWideChar(w: *const c_void, size: isize) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        if size < 0 {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let units = core::slice::from_raw_parts(w.cast::<u16>(), size as usize);
        let mut chars: Vec<u32> = Vec::with_capacity(units.len());
        let mut i = 0usize;
        while i < units.len() {
            let u = units[i] as u32;
            if is_high_surrogate(u) && i + 1 < units.len() && is_low_surrogate(units[i + 1] as u32)
            {
                chars.push(join_surrogates(u, units[i + 1] as u32));
                i += 2;
            } else {
                chars.push(u);
                i += 1;
            }
        }
        chars_to_object(&chars, 0)
    }
}

/// `PyUnicode_AsUCS4`: copy `len` code points into `buffer`.
///
/// # Safety
///
/// `u` must be a valid string; `buffer` room for `len` units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4(
    u: *mut PyObject,
    buffer: *mut u32,
    buflen: isize,
    copy_null: c_int,
) -> *mut u32 {
    // SAFETY: the ranges are the caller's contract.
    unsafe {
        let Some(hay) = as_chars(u) else {
            return core::ptr::null_mut();
        };
        if buflen < hay.len() + copy_null as isize {
            crate::tstate::set_string(exceptions::system_error(), b"buffer overflow");
            return core::ptr::null_mut();
        }
        for i in 0..hay.len() {
            *buffer.offset(i) = hay.get(i);
        }
        if copy_null != 0 {
            *buffer.offset(hay.len()) = 0;
        }
        buffer
    }
}

/// `PyUnicode_AsUCS4Copy`.
///
/// # Safety
///
/// `u` must be a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4Copy(u: *mut PyObject) -> *mut u32 {
    // SAFETY: the string is valid; the allocation is sized by its length.
    unsafe {
        let Some(hay) = as_chars(u) else {
            return core::ptr::null_mut();
        };
        let buffer = crate::alloc::pymem::PyMem_Malloc(
            (hay.len() as usize + 1) * core::mem::size_of::<u32>(),
        ) as *mut u32;
        if buffer.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        if PyUnicode_AsUCS4(u, buffer, hay.len() + 1, 1).is_null() {
            crate::alloc::pymem::PyMem_Free(buffer.cast());
            return core::ptr::null_mut();
        }
        buffer
    }
}

/// `PyUnicode_ReadChar`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_ReadChar(unicode: *mut PyObject, index: isize) -> u32 {
    // SAFETY: the index is checked before the read.
    unsafe {
        if !check(unicode) || index < 0 || index >= get_length(unicode) {
            return u32::MAX;
        }
        read(kind(unicode), data(unicode), index)
    }
}

/// `PyUnicode_WriteChar`.
///
/// # Safety
///
/// `unicode` must be a writable string with room at `index`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_WriteChar(
    unicode: *mut PyObject,
    index: isize,
    character: u32,
) -> c_int {
    // SAFETY: the index and kind are checked before the write.
    unsafe {
        if !unicode_modifiable(unicode)
            || index < 0
            || index >= get_length(unicode)
            || character >= MAX_UNICODE
        {
            return -1;
        }
        if character > max_char_value(unicode) as u64 as u32 {
            return -1;
        }
        write(kind(unicode), data(unicode), index, character);
        0
    }
}

/// `PyUnicode_Fill`.
///
/// # Safety
///
/// `to` must be a writable string with room for the range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Fill(
    to: *mut PyObject,
    start: isize,
    length: isize,
    fill_char: u32,
) -> isize {
    // SAFETY: the range is checked before the writes.
    unsafe {
        if !unicode_modifiable(to)
            || start < 0
            || length < 0
            || start + length > get_length(to)
            || fill_char >= MAX_UNICODE
        {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        if start + length > get_length(to) || fill_char > max_char_value(to) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        fill_raw(kind(to), data(to), start, length, fill_char);
        length
    }
}

fn fill_raw(object_kind: c_int, raw: *const c_void, start: isize, length: isize, ch: u32) {
    for i in start..start + length {
        match object_kind {
            PY_UNICODE_2BYTE_KIND => unsafe { *(raw as *mut u16).offset(i) = ch as u16 },
            PY_UNICODE_4BYTE_KIND => unsafe { *(raw as *mut u32).offset(i) = ch },
            _ => unsafe { *(raw as *mut u8).offset(i) = ch as u8 },
        }
    }
}

/// `PyUnicode_CopyCharacters`.
///
/// # Safety
///
/// Both strings must be valid and writable with room for the range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_CopyCharacters(
    to: *mut PyObject,
    to_start: isize,
    from: *mut PyObject,
    from_start: isize,
    how_many: isize,
) -> isize {
    // SAFETY: the ranges are checked before the copy.
    unsafe {
        if !check(from) {
            crate::tstate::set_string(exceptions::system_error(), b"Invalid string");
            return -1;
        }
        if to_start < 0
            || from_start < 0
            || to_start + how_many > get_length(to)
            || from_start + how_many > get_length(from)
        {
            crate::tstate::set_string(exceptions::system_error(), b"Out of bounds");
            return -1;
        }
        if how_many == 0 {
            return 0;
        }
        let char_max = max_char_value(from);
        if char_max > max_char_value(to) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Cannot copy a character into a too narrow string",
            );
            return -1;
        }
        fast_copy_characters(to, to_start, from, from_start, how_many);
        how_many
    }
}

/// `PyUnicode_New` C API entry.
///
/// # Safety
///
/// The data must be written before being read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_New(size: isize, maxchar: u32) -> *mut PyObject {
    // SAFETY: forwards to the checked constructor.
    unsafe { unicode_new(size, maxchar) }
}

/// `PyUnicode_GetLength`.
///
/// # Safety
///
/// `unicode` must be a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetLength(unicode: *mut PyObject) -> isize {
    // SAFETY: the type check guards the layout read.
    unsafe {
        if !check(unicode) {
            return -1;
        }
        get_length(unicode)
    }
}

/// `PyUnicode_GetSize`: the deprecated alias.
///
/// # Safety
///
/// `unicode` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetSize(unicode: *mut PyObject) -> isize {
    // SAFETY: forwards to the checked length.
    unsafe { PyUnicode_GetLength(unicode) }
}

/// `PyUnicode_GetDefaultEncoding`.
#[unsafe(no_mangle)]
pub extern "C" fn PyUnicode_GetDefaultEncoding() -> *const c_char {
    c"utf-8".as_ptr()
}

/// `PyUnicode_InternInPlace`.
///
/// # Safety
///
/// `p` must point to a writable string pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternInPlace(p: *mut *mut PyObject) {
    // SAFETY: the string is valid; the table is behind a mutex.
    unsafe {
        intern_in_place(p);
    }
}

/// `PyUnicode_InternImmortal`.
///
/// # Safety
///
/// `p` must point to a writable string pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternImmortal(p: *mut *mut PyObject) {
    // SAFETY: the string is valid; the table is behind a mutex.
    unsafe {
        intern_immortal(p);
    }
}

/// `PyUnicode_InternFromString`.
///
/// # Safety
///
/// `v` must be a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternFromString(v: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is NUL-terminated per contract; the length scan
    // stays within it.
    unsafe { intern_from_string(v) }
}

/// `PyUnicode_AsUTF8AndSize`: the string's cached UTF-8 form.
///
/// # Safety
///
/// `unicode` must be a valid string object and `size` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8AndSize(
    unicode: *mut PyObject,
    size: *mut isize,
) -> *const c_char {
    // SAFETY: the string is valid; the cache pointer stays valid for the
    // string's lifetime.
    unsafe { as_utf8_and_size(unicode, size) }
}

/// `PyUnicode_AsUTF8`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8(unicode: *mut PyObject) -> *const c_char {
    // SAFETY: forwards to the sized variant, discarding the length.
    unsafe { PyUnicode_AsUTF8AndSize(unicode, core::ptr::null_mut()) }
}

/// `PyUnicode_AsUTF8String` (the underscore alias).
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_AsUTF8String(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: the string is valid; encoding cannot fail for UTF-8.
    unsafe {
        let Some(chars) = as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        match bytes_from_chars(&chars.as_vec()) {
            Some(bytes) => bytes,
            None => core::ptr::null_mut(),
        }
    }
}

/// UTF-8 encode `chars` into a fresh `bytes` object.
fn bytes_from_chars(chars: &[u32]) -> Option<*mut PyObject> {
    crate::types::bytes::from_vec(encode_utf8_bytes(chars, ErrHandler::Strict))
}

/// `PyUnicode_AsASCIIString` (the underscore alias).
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_AsASCIIString(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: forwards to the checked encoder.
    unsafe { PyUnicode_AsASCIIString(unicode, errors) }
}

/// `PyUnicode_AsLatin1String` (the underscore alias).
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_AsLatin1String(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: forwards to the checked encoder.
    unsafe { PyUnicode_AsLatin1String(unicode, errors) }
}

/// `PyUnicode_DecodeUTF8`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the byte range and C string are the caller's contract.
    unsafe { decode_utf8_range(s, size, errors, None) }
}

/// `PyUnicode_DecodeUTF8Stateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes; `consumed` must be null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8Stateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: the byte range and C string are the caller's contract.
    unsafe {
        let mut used = size;
        let result = decode_utf8_range(s, size, errors, Some(&mut used));
        if !consumed.is_null() && !result.is_null() {
            *consumed = used;
        }
        result
    }
}

/// The shared body of the two UTF-8 decode entries: strict decoding of the
/// first `size` bytes, reporting how many bytes were consumed when the tail
/// holds an incomplete sequence.
unsafe fn decode_utf8_range(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: Option<&mut isize>,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract.
    unsafe {
        if s.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"NULL argument to PyUnicode_DecodeUTF8Stateful",
            );
            return core::ptr::null_mut();
        }
        let bytes = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let handler = error_handler(errors_arg(errors));
        // Trim an incomplete trailing sequence the way CPython's scanner
        // does: its error positions end at the sequence start.
        let mut end = bytes.len();
        if consumed.is_some() {
            let mut i = bytes.len();
            while i > bytes.len().saturating_sub(3) && i > 0 {
                let b = bytes[i - 1];
                if !(0x80..0xC0).contains(&b) {
                    break;
                }
                i -= 1;
            }
            // A lead byte whose sequence would run past the buffer ends
            // the decodable prefix.
            let mut lead = bytes.len();
            let mut scan = bytes.len();
            while scan > 0 {
                let b = bytes[scan - 1];
                if b >= 0xC0 {
                    lead = scan - 1;
                    break;
                }
                if b < 0x80 {
                    break;
                }
                scan -= 1;
            }
            if scan > 0 && bytes[lead] >= 0xC0 {
                let need = match bytes[lead] {
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    _ => 4,
                };
                if lead + need > bytes.len() {
                    end = lead;
                }
            }
        }
        let handler2 = handler;
        match decode_utf8_bytes(&bytes[..end], handler2) {
            Some(chars) => {
                if let Some(slot) = consumed {
                    *slot = end as isize;
                }
                chars_to_object(&chars, maxchar_of(&chars))
            }
            None => {
                if let Some(slot) = consumed {
                    *slot = 0;
                }
                core::ptr::null_mut()
            }
        }
    }
}

/// `PyUnicode_FromStringAndSize`: validates UTF-8 and compacts.
///
/// # Safety
///
/// `u` must point to `size` bytes, or to a NUL-terminated string when
/// `size` is negative.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromStringAndSize(
    u: *const c_char,
    size: isize,
) -> *mut PyObject {
    // SAFETY: the byte range is the caller's contract.
    unsafe {
        if size == 0 {
            return empty();
        }
        let bytes = if size < 0 {
            let mut len = 0usize;
            while *u.add(len) != 0 {
                len += 1;
            }
            core::slice::from_raw_parts(u.cast::<u8>(), len)
        } else {
            core::slice::from_raw_parts(u.cast::<u8>(), size as usize)
        };
        let mut maxchar: u32 = 0;
        for &b in bytes {
            if b >= 0x80 {
                maxchar = 0x80;
                break;
            }
        }
        if maxchar == 0 {
            return from_ascii_unchecked(bytes);
        }
        let Some(chars) = decode_utf8_bytes(bytes, ErrHandler::Strict) else {
            return core::ptr::null_mut();
        };
        chars_to_object(&chars, maxchar_of(&chars))
    }
}

/// `PyUnicode_FromString`.
///
/// # Safety
///
/// `u` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromString(u: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is NUL-terminated per contract.
    unsafe { PyUnicode_FromStringAndSize(u, -1) }
}

/// `_PyUnicode_FromASCII`: bytes are trusted to be ASCII.
///
/// # Safety
///
/// `buffer` must hold `size` ASCII bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FromASCII(buffer: *const c_char, size: isize) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        if size == 0 {
            return empty();
        }
        let bytes = if size < 0 {
            let mut len = 0usize;
            while *buffer.add(len) != 0 {
                len += 1;
            }
            core::slice::from_raw_parts(buffer.cast::<u8>(), len)
        } else {
            core::slice::from_raw_parts(buffer.cast::<u8>(), size as usize)
        };
        from_ascii_unchecked(bytes)
    }
}

/// An ASCII-kind string over `bytes` without a validation pass; kind 1,
/// ASCII bit set, per `_PyUnicode_FromASCII`'s contract.
unsafe fn from_ascii_unchecked(bytes: &[u8]) -> *mut PyObject {
    // SAFETY: allocation and inline write within the fresh object.
    unsafe {
        let unicode = unicode_new(bytes.len() as isize, 0x7F);
        if unicode.is_null() {
            return unicode;
        }
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), data(unicode) as *mut u8, bytes.len());
        unicode
    }
}

/// `_PyUnicode_FindMaxChar`: 127 for an empty range.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FindMaxChar(
    unicode: *mut PyObject,
    start: isize,
    end: isize,
) -> u32 {
    // SAFETY: the type check guards the layout reads; the range is clamped.
    unsafe {
        if !check(unicode) {
            return u32::MAX;
        }
        let len = get_length(unicode);
        let start = start.clamp(0, len);
        let end = end.clamp(0, len);
        let mut maxchar: u32 = 0;
        let object_kind = kind(unicode);
        let raw = data(unicode);
        for i in start..end {
            let ch = read(object_kind, raw, i);
            if ch > maxchar {
                maxchar = ch;
                if maxchar >= 0x10FFFF {
                    break;
                }
            }
        }
        if maxchar < 0x80 { 0x7F } else { maxchar }
    }
}

/// `_PyUnicode_CheckConsistency`: validate the compact layout invariants.
///
/// # Safety
///
/// `op` must point to a string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_CheckConsistency(
    op: *mut PyObject,
    check_content: c_int,
) -> c_int {
    // SAFETY: every field read is within the layout the state bits select.
    unsafe {
        let ascii = (*(op as *const PyASCIIObject)).state.ascii();
        let compact = (*(op as *const PyASCIIObject)).state.compact();
        let length = get_length(op);
        let _ = (ascii, compact);
        if !compact && ascii {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"non-compact ASCII string is impossible",
            );
            return -1;
        }
        let object_kind = kind(op);
        if !(object_kind == PY_UNICODE_1BYTE_KIND
            || object_kind == PY_UNICODE_2BYTE_KIND
            || object_kind == PY_UNICODE_4BYTE_KIND)
        {
            crate::tstate::set_string(exceptions::system_error(), b"invalid kind");
            return -1;
        }
        if check_content != 0 {
            let maxchar = max_char_value(op);
            let raw = data(op);
            for i in 0..length {
                let ch = read(object_kind, raw, i);
                if ch > maxchar {
                    crate::tstate::set_string(
                        exceptions::system_error(),
                        b"string contains a character above its maximum",
                    );
                    return -1;
                }
            }
        }
        0
    }
}

/// `_PyUnicode_Copy`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_Copy(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: the string is valid.
    unsafe { copy(unicode) }
}

/// `_PyUnicode_FastCopyCharacters`.
///
/// # Safety
///
/// The ranges must fit both strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FastCopyCharacters(
    to: *mut PyObject,
    to_start: isize,
    from: *mut PyObject,
    from_start: isize,
    how_many: isize,
) {
    // SAFETY: the unchecked contract is the caller's.
    unsafe { fast_copy_characters(to, to_start, from, from_start, how_many) }
}

/// `_PyUnicode_FastFill`.
///
/// # Safety
///
/// `unicode[start..start+length]` must be writable and fit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FastFill(
    unicode: *mut PyObject,
    start: isize,
    length: isize,
    fill_char: u32,
) {
    // SAFETY: the unchecked contract is the caller's.
    unsafe { fill_raw(kind(unicode), data(unicode), start, length, fill_char) }
}

/// `_PyUnicode_EqualToASCIIId`: CPython's `_Py_Identifier` has a `next`
/// pointer before its `string` member; both are read from the caller's
/// static struct.
///
/// # Safety
///
/// `left` must be a valid object; `right` must point at an initialised
/// `_Py_Identifier`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EqualToASCIIId(
    left: *mut PyObject,
    right: *mut c_void,
) -> c_int {
    // SAFETY: the identifier's second member is the `const char*` string
    // in CPython 3.12's `_Py_Identifier`.
    unsafe { _PyUnicode_EqualToASCIIString(left, *(right as *mut *const c_char).add(1)) }
}

/// `_PyUnicode_XStrip`.
///
/// # Safety
///
/// `self` must be a valid string; `sepobj` a valid string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_XStrip(
    self_: *mut PyObject,
    striptype: c_int,
    sepobj: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the strings are valid.
    unsafe {
        let Some(hay) = as_chars(self_) else {
            return core::ptr::null_mut();
        };
        let kind_ = if striptype < 0 {
            StripKind::Left
        } else if striptype == 0 {
            StripKind::Both
        } else {
            StripKind::Right
        };
        let sep = if sepobj.is_null() {
            Vec::new()
        } else {
            let Some(sep_chars) = as_chars(sepobj) else {
                return core::ptr::null_mut();
            };
            sep_chars.as_vec()
        };
        chars_to_object(&xstrip_core(&hay.as_vec(), &sep, kind_), 0x7F)
    }
}

/// `_PyUnicode_JoinArray`.
///
/// # Safety
///
/// `items` must hold `seqlen` valid objects; the separator a valid string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_JoinArray(
    separator: *mut PyObject,
    items: *const *mut PyObject,
    seqlen: isize,
) -> *mut PyObject {
    // SAFETY: the array and separator contracts are the caller's.
    unsafe {
        let slice = core::slice::from_raw_parts(items, seqlen.max(0) as usize);
        join_items(separator, slice)
    }
}

/// `_PyUnicode_ScanIdentifier`.
///
/// # Safety
///
/// `self` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ScanIdentifier(self_: *mut PyObject) -> isize {
    // SAFETY: the string is valid.
    unsafe {
        let len = get_length(self_);
        if len == 0 {
            return 0;
        }
        let first = read(kind(self_), data(self_), 0);
        if !is_xid_start(first) && first != 0x5F {
            return 0;
        }
        let mut i = 1isize;
        while i < len {
            if !is_xid_continue(read(kind(self_), data(self_), i)) {
                return i;
            }
            i += 1;
        }
        i
    }
}

/// `_PyUnicode_InternMortal`: the same table; Coil keeps one interned
/// entry per value (CPython's mortal/immortal distinction is a refcount
/// policy its owner task adjusts).
///
/// # Safety
///
/// `p` must point to a writable string pointer slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_InternMortal(p: *mut *mut PyObject) {
    // SAFETY: the string is valid; the table is behind a mutex.
    unsafe { intern_in_place(p) }
}

/// `_PyUnicode_TransformDecimalAndSpaceToASCII`.
///
/// # Safety
///
/// `unicode` must be a valid string object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_TransformDecimalAndSpaceToASCII(
    unicode: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the string is valid; the result is written in bounds.
    unsafe {
        if (*(unicode as *const PyASCIIObject)).state.ascii() {
            incref(unicode);
            return unicode;
        }
        let len = get_length(unicode);
        let result = unicode_new(len, 0x7F);
        if result.is_null() {
            return result;
        }
        let out = data(result) as *mut u8;
        let object_kind = kind(unicode);
        let raw = data(unicode);
        let mut i = 0isize;
        while i < len {
            let ch = read(object_kind, raw, i);
            if ch < 127 {
                *out.offset(i) = ch as u8;
            } else if is_space(ch) {
                *out.offset(i) = b' ';
            } else {
                let decimal = to_decimal_digit(ch);
                if decimal < 0 {
                    *out.offset(i) = b'?';
                    *out.offset(i + 1) = 0;
                    set_length(result, i + 1);
                    return result;
                }
                *out.offset(i) = b'0' + decimal as u8;
            }
            i += 1;
        }
        result
    }
}

/// `_PyUnicodeLength` write access: `_PyUnicode_LENGTH()` in CPython.
///
/// # Safety
///
/// `op` must point to a valid string object.
unsafe fn set_length(op: *mut PyObject, length: isize) {
    // SAFETY: `length` sits at a fixed offset in every string layout.
    unsafe { core::ptr::addr_of_mut!((*(op as *mut PyASCIIObject)).length).write(length) }
}

/// `PyUnicode_AsWideChar`: 16-bit `wchar_t` (the pinned ABI is the Windows
/// model); astral characters split into surrogate pairs.
///
/// # Safety
///
/// `unicode` must be a valid string; `w` room for `size` units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsWideChar(
    unicode: *mut PyObject,
    w: *mut c_void,
    size: isize,
) -> isize {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        let Some(chars) = as_chars(unicode) else {
            return -1;
        };
        let units = wide_units(&chars.as_vec());
        let n = if size < 0 {
            units.len()
        } else {
            units.len().min(size as usize)
        };
        core::ptr::copy_nonoverlapping(units.as_ptr(), w as *mut u16, n);
        n as isize
    }
}

/// `PyUnicode_AsWideCharString`.
///
/// # Safety
///
/// `unicode` must be a valid string; `size` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsWideCharString(
    unicode: *mut PyObject,
    size: *mut isize,
) -> *mut c_void {
    // SAFETY: the allocation is sized by the string.
    unsafe {
        let Some(chars) = as_chars(unicode) else {
            if !size.is_null() {
                *size = -1;
            }
            return core::ptr::null_mut();
        };
        let units = wide_units(&chars.as_vec());
        if !size.is_null() {
            *size = units.len() as isize;
        }
        let buffer =
            crate::alloc::pymem::PyMem_Malloc((units.len() + 1) * core::mem::size_of::<u16>())
                as *mut u16;
        if buffer.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            if !size.is_null() {
                *size = -1;
            }
            return core::ptr::null_mut();
        }
        core::ptr::copy_nonoverlapping(units.as_ptr(), buffer, units.len());
        *buffer.add(units.len()) = 0;
        buffer.cast()
    }
}

/// A string as UTF-16 units with surrogate pairs for astral characters.
fn wide_units(chars: &[u32]) -> Vec<u16> {
    let mut units = Vec::with_capacity(chars.len());
    for &ch in chars {
        if ch < 0x10000 {
            units.push(ch as u16);
        } else {
            let v = ch - 0x10000;
            units.push(0xD800 + (v >> 10) as u16);
            units.push(0xDC00 + (v & 0x3FF) as u16);
        }
    }
    units
}

/// `PyUnicode_FSConverter`: a filesystem path argument to a `bytes`
/// object. `os.PathLike` needs the container runtime.
///
/// # Safety
///
/// `result` must point to a writable `*mut PyObject` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FSConverter(obj: *mut PyObject, result: *mut c_void) -> c_int {
    // SAFETY: the object and slot are valid.
    unsafe {
        let path = if obj.is_null() || core::ptr::eq(obj, singletons::none()) {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"expected str, bytes or os.PathLike object, not NoneType",
            );
            return 0;
        } else if check(obj) {
            let Some(chars) = as_chars(obj) else {
                return 0;
            };
            let Some(encoded) = bytes_from_chars(&chars.as_vec()) else {
                return 0;
            };
            encoded
        } else if crate::types::bytes::check(obj) {
            incref(obj);
            obj
        } else {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"coercing to str: need str, bytes-like object or os.PathLike found",
            );
            return 0;
        };
        let (len, ptr) = crate::types::bytes::as_string_parts(path);
        if len > 0 && *ptr.offset(len - 1) == 0 {
            decref(path);
            crate::tstate::set_string(exceptions::value_error(), b"embedded null byte");
            return 0;
        }
        *(result as *mut *mut PyObject) = path;
        1
    }
}

/// `PyUnicode_FSDecoder`: a filesystem path argument to a `str` object.
///
/// # Safety
///
/// `result` must point to a writable `*mut PyObject` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FSDecoder(obj: *mut PyObject, result: *mut c_void) -> c_int {
    // SAFETY: the object and slot are valid.
    unsafe {
        let text = if obj.is_null() || core::ptr::eq(obj, singletons::none()) {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"expected str, bytes or os.PathLike object, not NoneType",
            );
            return 0;
        } else if check(obj) {
            incref(obj);
            obj
        } else if crate::types::bytes::check(obj) {
            let (len, ptr) = crate::types::bytes::as_string_parts(obj);
            let bytes = core::slice::from_raw_parts(ptr.cast::<u8>(), len.max(0) as usize);
            let Some(chars) = decode_utf8_bytes(bytes, ErrHandler::Strict) else {
                return 0;
            };
            chars_to_object(&chars, maxchar_of(&chars))
        } else {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"coercing to str: need str, bytes-like object or os.PathLike found",
            );
            return 0;
        };
        let len = get_length(text);
        let has_nul = as_chars(text).unwrap_or(EMPTY_CHARS).as_vec().contains(&0);
        if has_nul {
            decref(text);
            crate::tstate::set_string(exceptions::value_error(), b"embedded null character");
            return 0;
        }
        let _ = len;
        *(result as *mut *mut PyObject) = text;
        1
    }
}

/// `PyUnicode_Translate`: a `str` table maps ordinals directly (beyond its
/// end the character passes through; the error handler then cannot fire,
/// which is CPython's behaviour for str tables too). `dict` tables need
/// the container runtime.
///
/// # Safety
///
/// `str_` must be a valid string; `mapping` a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Translate(
    str_: *mut PyObject,
    mapping: *mut PyObject,
    _errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the objects are valid.
    unsafe {
        if !check(str_) || mapping.is_null() {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let hay = as_chars(str_).unwrap_or(EMPTY_CHARS);
        let chars = hay.as_vec();
        if chars.is_empty() {
            incref(str_);
            return str_;
        }
        let table = if check(mapping) {
            as_chars(mapping).unwrap_or(EMPTY_CHARS).as_vec()
        } else {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"coil: PyUnicode_Translate needs dict tables from the container runtime",
            );
            return core::ptr::null_mut();
        };
        // A str table maps ordinals directly; past its end the character
        // passes through (CPython's cleared IndexError path).
        let mut out: Vec<u32> = Vec::with_capacity(chars.len());
        for &ch in &chars {
            match table.get(ch as usize) {
                Some(&m) => out.push(m),
                None => out.push(ch),
            }
        }
        chars_to_object(&out, 0)
    }
}

// -------------------------------------------- character-type C API surface

/// `_PyUnicode_ToLowercase`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToLowercase(ch: u32) -> u32 {
    to_lowercase(ch)
}

/// `_PyUnicode_ToUppercase`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToUppercase(ch: u32) -> u32 {
    to_uppercase(ch)
}

/// `_PyUnicode_ToTitlecase`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToTitlecase(ch: u32) -> u32 {
    let mut mapped = [0u32; 3];
    if to_title_full(ch, &mut mapped) == 1 {
        mapped[0]
    } else {
        ch
    }
}

/// `_PyUnicode_ToDecimalDigit`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToDecimalDigit(ch: u32) -> c_int {
    to_decimal_digit(ch)
}

/// `_PyUnicode_ToDigit`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToDigit(ch: u32) -> c_int {
    to_digit(ch)
}

/// `_PyUnicode_ToNumeric`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToNumeric(ch: u32) -> f64 {
    to_numeric(ch)
}

/// `_PyUnicode_ToLowerFull`.
///
/// # Safety
///
/// `res` must have room for three units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ToLowerFull(ch: u32, res: *mut u32) -> c_int {
    // SAFETY: the buffer contract is CPython's `[Py_UCS4; 3]`.
    unsafe {
        let mut mapped = [0u32; 3];
        let n = to_lower_full(ch, &mut mapped);
        core::ptr::copy_nonoverlapping(mapped.as_ptr(), res, n);
        n as c_int
    }
}

/// `_PyUnicode_ToUpperFull`.
///
/// # Safety
///
/// `res` must have room for three units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ToUpperFull(ch: u32, res: *mut u32) -> c_int {
    // SAFETY: the buffer contract is CPython's `[Py_UCS4; 3]`.
    unsafe {
        let mut mapped = [0u32; 3];
        let n = to_upper_full(ch, &mut mapped);
        core::ptr::copy_nonoverlapping(mapped.as_ptr(), res, n);
        n as c_int
    }
}

/// `_PyUnicode_ToTitleFull`.
///
/// # Safety
///
/// `res` must have room for three units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ToTitleFull(ch: u32, res: *mut u32) -> c_int {
    // SAFETY: the buffer contract is CPython's `[Py_UCS4; 3]`.
    unsafe {
        let mut mapped = [0u32; 3];
        let n = to_title_full(ch, &mut mapped);
        core::ptr::copy_nonoverlapping(mapped.as_ptr(), res, n);
        n as c_int
    }
}

/// `_PyUnicode_ToFoldedFull`.
///
/// # Safety
///
/// `res` must have room for three units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_ToFoldedFull(ch: u32, res: *mut u32) -> c_int {
    // SAFETY: the buffer contract is CPython's `[Py_UCS4; 3]`.
    unsafe {
        let mut mapped = [0u32; 3];
        let n = to_folded_full(ch, &mut mapped);
        core::ptr::copy_nonoverlapping(mapped.as_ptr(), res, n);
        n as c_int
    }
}

macro_rules! char_predicate {
    ($(#[$doc:meta])* $export:ident, $inner:path) => {
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $export(ch: u32) -> c_int {
            $inner(ch) as c_int
        }
    };
}

char_predicate!(
    /// `_PyUnicode_IsAlpha`.
    _PyUnicode_IsAlpha,
    is_alpha
);
char_predicate!(
    /// `_PyUnicode_IsDecimalDigit`.
    _PyUnicode_IsDecimalDigit,
    is_decimal_digit
);
char_predicate!(
    /// `_PyUnicode_IsDigit`.
    _PyUnicode_IsDigit,
    is_digit
);
char_predicate!(
    /// `_PyUnicode_IsNumeric`.
    _PyUnicode_IsNumeric,
    is_numeric_char
);
char_predicate!(
    /// `_PyUnicode_IsPrintable`.
    _PyUnicode_IsPrintable,
    is_printable
);
char_predicate!(
    /// `_PyUnicode_IsLowercase`.
    _PyUnicode_IsLowercase,
    is_lowercase
);
char_predicate!(
    /// `_PyUnicode_IsUppercase`.
    _PyUnicode_IsUppercase,
    is_uppercase
);
char_predicate!(
    /// `_PyUnicode_IsTitlecase`.
    _PyUnicode_IsTitlecase,
    is_titlecase
);
char_predicate!(
    /// `_PyUnicode_IsCased`.
    _PyUnicode_IsCased,
    is_cased
);
char_predicate!(
    /// `_PyUnicode_IsCaseIgnorable`.
    _PyUnicode_IsCaseIgnorable,
    is_case_ignorable
);
char_predicate!(
    /// `_PyUnicode_IsXidStart`.
    _PyUnicode_IsXidStart,
    is_xid_start
);
char_predicate!(
    /// `_PyUnicode_IsXidContinue`.
    _PyUnicode_IsXidContinue,
    is_xid_continue
);

/// `_PyUnicode_IsWhitespace`: the Unicode White_Space set.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsWhitespace(ch: u32) -> c_int {
    crate::types::unicode_data::WHITESPACE
        .binary_search(&ch)
        .is_ok() as c_int
}

/// `_PyUnicode_IsLinebreak`.
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsLinebreak(ch: u32) -> c_int {
    crate::types::unicode_data::LINEBREAK
        .binary_search(&ch)
        .is_ok() as c_int
}

// ------------------------------------------- codec error C API surface

/// The instance check behind the error accessors: CPython uses
/// `PyErr_GivenExceptionMatches`, which walks the base chain.
unsafe fn is_exception_instance(exc: *mut PyObject, expected: *mut PyObject) -> bool {
    // SAFETY: the object and type are valid; the chain is static.
    unsafe {
        let mut t = (*(exc as *const PyObject)).ob_type;
        while !t.is_null() {
            if core::ptr::eq(t, expected.cast::<PyTypeObject>()) {
                return true;
            }
            t = (*(t as *const PyTypeObject)).tp_base;
        }
        false
    }
}

macro_rules! unicode_error_accessors {
    ($get_encoding:ident, $get_object:ident, $get_start:ident, $set_start:ident,
     $get_end:ident, $set_end:ident, $get_reason:ident, $set_reason:ident, $acc:ident) => {
        /// CPython's `GetEncoding`: a new reference to the encoding, or
        /// null with `SystemError` set for a foreign object.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_encoding(exc: *mut PyObject) -> *mut PyObject {
            // SAFETY: the object is valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return core::ptr::null_mut();
                }
                let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).encoding);
                let value = *field;
                incref(value);
                value
            }
        }

        /// CPython's `GetObject`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_object(exc: *mut PyObject) -> *mut PyObject {
            // SAFETY: the object is valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return core::ptr::null_mut();
                }
                let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).object);
                let value = *field;
                incref(value);
                value
            }
        }

        /// CPython's `GetStart`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object; `start` writable.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_start(exc: *mut PyObject, start: *mut isize) -> c_int {
            // SAFETY: the object and slot are valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return -1;
                }
                *start = core::ptr::read(
                    (exc as *const u8).add(coil_rt_abi::cp312::unicode_error::START)
                        as *const isize,
                );
                0
            }
        }

        /// CPython's `SetStart`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set_start(exc: *mut PyObject, start: isize) -> c_int {
            // SAFETY: the object is valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return -1;
                }
                core::ptr::write(
                    (exc as *mut u8).add(coil_rt_abi::cp312::unicode_error::START) as *mut isize,
                    start,
                );
                0
            }
        }

        /// CPython's `GetEnd`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object; `end` writable.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_end(exc: *mut PyObject, end: *mut isize) -> c_int {
            // SAFETY: the object and slot are valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return -1;
                }
                *end = core::ptr::read(
                    (exc as *const u8).add(coil_rt_abi::cp312::unicode_error::END) as *const isize,
                );
                0
            }
        }

        /// CPython's `SetEnd`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set_end(exc: *mut PyObject, end: isize) -> c_int {
            // SAFETY: the object is valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return -1;
                }
                core::ptr::write(
                    (exc as *mut u8).add(coil_rt_abi::cp312::unicode_error::END) as *mut isize,
                    end,
                );
                0
            }
        }

        /// CPython's `GetReason`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get_reason(exc: *mut PyObject) -> *mut PyObject {
            // SAFETY: the object is valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return core::ptr::null_mut();
                }
                let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).reason);
                let value = *field;
                incref(value);
                value
            }
        }

        /// CPython's `SetReason`.
        ///
        /// # Safety
        ///
        /// `exc` must be null or a valid object; `reason` a C string.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $set_reason(exc: *mut PyObject, reason: *const c_char) -> c_int {
            // SAFETY: the object and C string are valid or null.
            unsafe {
                if !is_exception_instance(exc, exceptions::$acc()) {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return -1;
                }
                let new_reason = from_rust(util::cstr(reason));
                if new_reason.is_null() {
                    return -1;
                }
                let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).reason);
                let old = *field;
                *field = new_reason;
                decref(old);
                0
            }
        }
    };
}

unicode_error_accessors!(
    PyUnicodeDecodeError_GetEncoding,
    PyUnicodeDecodeError_GetObject,
    PyUnicodeDecodeError_GetStart,
    PyUnicodeDecodeError_SetStart,
    PyUnicodeDecodeError_GetEnd,
    PyUnicodeDecodeError_SetEnd,
    PyUnicodeDecodeError_GetReason,
    PyUnicodeDecodeError_SetReason,
    unicode_decode_error
);
unicode_error_accessors!(
    PyUnicodeEncodeError_GetEncoding,
    PyUnicodeEncodeError_GetObject,
    PyUnicodeEncodeError_GetStart,
    PyUnicodeEncodeError_SetStart,
    PyUnicodeEncodeError_GetEnd,
    PyUnicodeEncodeError_SetEnd,
    PyUnicodeEncodeError_GetReason,
    PyUnicodeEncodeError_SetReason,
    unicode_encode_error
);

/// CPython's `PyUnicodeTranslateError_GetObject`.
///
/// # Safety
///
/// `exc` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_GetObject(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).object);
        let value = *field;
        incref(value);
        value
    }
}

/// CPython's `PyUnicodeTranslateError_GetStart`.
///
/// # Safety
///
/// `exc` must be null or a valid object; `start` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_GetStart(
    exc: *mut PyObject,
    start: *mut isize,
) -> c_int {
    // SAFETY: the object and slot are valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        *start = core::ptr::read(
            (exc as *const u8).add(coil_rt_abi::cp312::unicode_error::START) as *const isize,
        );
        0
    }
}

/// CPython's `PyUnicodeTranslateError_SetStart`.
///
/// # Safety
///
/// `exc` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_SetStart(
    exc: *mut PyObject,
    start: isize,
) -> c_int {
    // SAFETY: the object is valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        core::ptr::write(
            (exc as *mut u8).add(coil_rt_abi::cp312::unicode_error::START) as *mut isize,
            start,
        );
        0
    }
}

/// CPython's `PyUnicodeTranslateError_GetEnd`.
///
/// # Safety
///
/// `exc` must be null or a valid object; `end` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_GetEnd(
    exc: *mut PyObject,
    end: *mut isize,
) -> c_int {
    // SAFETY: the object and slot are valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        *end = core::ptr::read(
            (exc as *const u8).add(coil_rt_abi::cp312::unicode_error::END) as *const isize,
        );
        0
    }
}

/// CPython's `PyUnicodeTranslateError_SetEnd`.
///
/// # Safety
///
/// `exc` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_SetEnd(exc: *mut PyObject, end: isize) -> c_int {
    // SAFETY: the object is valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        core::ptr::write(
            (exc as *mut u8).add(coil_rt_abi::cp312::unicode_error::END) as *mut isize,
            end,
        );
        0
    }
}

/// CPython's `PyUnicodeTranslateError_GetReason`.
///
/// # Safety
///
/// `exc` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_GetReason(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return core::ptr::null_mut();
        }
        let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).reason);
        let value = *field;
        incref(value);
        value
    }
}

/// CPython's `PyUnicodeTranslateError_SetReason`.
///
/// # Safety
///
/// `exc` must be null or a valid object; `reason` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeTranslateError_SetReason(
    exc: *mut PyObject,
    reason: *const c_char,
) -> c_int {
    // SAFETY: the object and C string are valid or null.
    unsafe {
        if !is_exception_instance(exc, exceptions::unicode_translate_error()) {
            crate::tstate::set_string(exceptions::system_error(), b"");
            return -1;
        }
        let new_reason = from_rust(util::cstr(reason));
        if new_reason.is_null() {
            return -1;
        }
        let field = core::ptr::addr_of_mut!((*(exc as *mut PyUnicodeErrorObject)).reason);
        let old = *field;
        *field = new_reason;
        decref(old);
        0
    }
}

// --------------------------------------------------- designed stubs (10.1)

/// CPython's `_PyUnicodeWriter` layout, for the stub signatures below; the
/// machinery needs the container runtime's object model and lands with it.
#[repr(C)]
pub struct PyUnicodeWriter {
    pub buffer: *mut PyObject,
    pub data: *mut c_void,
    pub kind: c_int,
    pub maxchar: u32,
    pub size: isize,
    pub pos: isize,
    pub min_length: isize,
    pub min_char: u32,
    pub overallocate: u8,
    pub start_char: u32,
    pub end_char: u32,
    pub ascii: c_int,
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Init(writer: *mut PyUnicodeWriter) {
    let _ = writer;
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_Init is not implemented",
    );
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_PrepareInternal(
    writer: *mut PyUnicodeWriter,
    length: isize,
    maxchar: u32,
) -> c_int {
    let _ = (writer, length, maxchar);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_PrepareInternal is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_PrepareKindInternal(
    writer: *mut PyUnicodeWriter,
    kind_: c_int,
) -> c_int {
    let _ = (writer, kind_);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_PrepareKindInternal is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteChar(
    writer: *mut PyUnicodeWriter,
    ch: u32,
) -> c_int {
    let _ = (writer, ch);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_WriteChar is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteStr(
    writer: *mut PyUnicodeWriter,
    str_: *mut PyObject,
) -> c_int {
    let _ = (writer, str_);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_WriteStr is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteSubstring(
    writer: *mut PyUnicodeWriter,
    str_: *mut PyObject,
    start: isize,
    end: isize,
) -> c_int {
    let _ = (writer, str_, start, end);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_WriteSubstring is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteASCIIString(
    writer: *mut PyUnicodeWriter,
    str_: *const c_char,
    len: isize,
) -> c_int {
    let _ = (writer, str_, len);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_WriteASCIIString is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteLatin1String(
    writer: *mut PyUnicodeWriter,
    str_: *const c_char,
    len: isize,
) -> c_int {
    let _ = (writer, str_, len);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_WriteLatin1String is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Finish(writer: *mut PyUnicodeWriter) -> *mut PyObject {
    let _ = writer;
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_Finish is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: the writer needs the container runtime.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Dealloc(writer: *mut PyUnicodeWriter) {
    let _ = writer;
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeWriter_Dealloc is not implemented",
    );
}

/// A spec-section-10.1 stub: needs the container runtime's `_Py_Identifier`
/// statics.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FromId(_identifier: *mut c_void) -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_FromId is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: grouped number formatting rides with the
/// writer machinery it writes through.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_InsertThousandsGrouping(
    writer: *mut PyUnicodeWriter,
    n_buffer: isize,
    digits: *mut PyObject,
    d_pos: isize,
    n_digits: isize,
    min_width: *mut isize,
    grouping: *const c_char,
    thousands_sep: *mut PyObject,
    maxchar: *mut u32,
) -> isize {
    let _ = (
        writer,
        n_buffer,
        digits,
        d_pos,
        n_digits,
        min_width,
        grouping,
        thousands_sep,
        maxchar,
    );
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_InsertThousandsGrouping is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: advanced formatting rides with the writer
/// machinery.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FormatAdvancedWriter(
    writer: *mut PyUnicodeWriter,
    obj: *mut PyObject,
    format_spec: *mut PyObject,
    start: isize,
    end: isize,
) -> c_int {
    let _ = (writer, obj, format_spec, start, end);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_FormatAdvancedWriter is not implemented",
    );
    -1
}

/// A spec-section-10.1 stub: the converter protocol needs the argument
/// machinery.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_WideCharString_Converter(
    obj: *mut PyObject,
    target: *mut c_void,
) -> c_int {
    let _ = (obj, target);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_WideCharString_Converter is not implemented",
    );
    0
}

/// A spec-section-10.1 stub: the converter protocol needs the argument
/// machinery.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_WideCharString_Opt_Converter(
    obj: *mut PyObject,
    target: *mut c_void,
) -> c_int {
    let _ = (obj, target);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_WideCharString_Opt_Converter is not implemented",
    );
    0
}

/// A spec-section-10.1 stub: UTF-7 is a platform codec Coil does not ship.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_EncodeUTF7(
    _unicode: *mut PyObject,
    _base64_set_o: c_int,
    _encode_set_o: c_int,
    _encode_set_s: c_int,
    _errors: *const c_char,
) -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicode_EncodeUTF7 is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: UTF-7 is a platform codec Coil does not ship.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF7(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    let _ = (s, size, errors);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_DecodeUTF7 is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: UTF-7 is a platform codec Coil does not ship.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF7Stateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    let _ = (s, size, errors, consumed);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_DecodeUTF7Stateful is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: `mbcs` needs `MultiByteToWideChar`.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeMBCS(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
) -> *mut PyObject {
    let _ = (s, size, errors);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_DecodeMBCS is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: `mbcs` needs `MultiByteToWideChar`.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeMBCSStateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    let _ = (s, size, errors, consumed);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_DecodeMBCSStateful is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: `mbcs` needs `MultiByteToWideChar`.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsMBCSString(
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    let _ = (unicode, errors);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_AsMBCSString is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: code pages need `MultiByteToWideChar`.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeCodePageStateful(
    code_page: c_int,
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    let _ = (code_page, s, size, errors, consumed);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_DecodeCodePageStateful is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: code pages need `WideCharToMultiByte`.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeCodePage(
    code_page: c_int,
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    let _ = (code_page, unicode, errors);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_EncodeCodePage is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: the printf-style parser needs the argument
/// machinery (and stable Rust cannot read C varargs).
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromFormat(
    format: *const c_char,
    mut _args: ...
) -> *mut PyObject {
    let _ = format;
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_FromFormat is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: the printf-style parser needs the argument
/// machinery (and stable Rust cannot walk a `va_list`).
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromFormatV(
    format: *const c_char,
    args: *mut c_void,
) -> *mut PyObject {
    let _ = (format, args);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicode_FromFormatV is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: creating codec error instances needs the
/// exceptions task's instance machinery.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeDecodeError_Create(
    encoding: *const c_char,
    object: *const c_char,
    length: isize,
    start: isize,
    end: isize,
    reason: *const c_char,
) -> *mut PyObject {
    let _ = (encoding, object, length, start, end, reason);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyUnicodeDecodeError_Create is not implemented",
    );
    core::ptr::null_mut()
}

/// A spec-section-10.1 stub: creating codec error instances needs the
/// exceptions task's instance machinery.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeTranslateError_Create(
    object: *mut PyObject,
    start: isize,
    end: isize,
    reason: *const c_char,
) -> *mut PyObject {
    let _ = (object, start, end, reason);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: _PyUnicodeTranslateError_Create is not implemented",
    );
    core::ptr::null_mut()
}

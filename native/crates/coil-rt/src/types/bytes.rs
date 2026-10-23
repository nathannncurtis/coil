//! `bytes` and `bytearray` over the exact CPython 3.12 layouts (spec
//! sections 8.2 and 8.5): the hash cache and inline byte buffer for bytes,
//! the `ob_alloc`/`ob_bytes`/`ob_start` triple for bytearray. The buffer
//! protocol is live on both types. `tp_methods` stays unset — method
//! dispatch needs the argument machinery — so the method cores below are
//! `pub(crate)` for the oracle tests, and the container-returning C API
//! entries (`_PyBytes_Join`, iterables in `PyBytes_FromObject`) name their
//! limitation, matching the other types until their tasks land.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_BYTES_SUBCLASS, Py_TPFLAGS_DEFAULT, Py_buffer, PyObject,
    PyTypeObject, c_long,
};
use coil_rt_abi::cp312::bytes as bytes_layout;

use crate::object::exceptions;
use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{
    Py_EQ as CMP_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE as CMP_NE, zero_type,
};
use crate::types::unicode::{decref, incref};
use crate::util;

/// `_Py_TPFLAGS_MATCH_SELF`: the bytes type's private extra flag.
const PY_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

// ---------------------------------------------------------------- layout

/// The terminating byte after `ob_sval[ob_size]`.
const OB_SVAL_TERMINATOR: usize = 1;

/// `PyBytesObject` (exact): the var header (CPython 3.12 made bytes a
/// var object), deprecated hash cache, inline bytes.
#[repr(C)]
pub struct PyBytesObject {
    pub ob_base: PyObject,
    pub ob_size: isize,
    pub ob_shash: isize,
    pub ob_sval: [u8; OB_SVAL_TERMINATOR],
}

/// `PyByteArrayObject` (exact): header, allocation, the canonical and
/// logical buffer starts, export count.
#[repr(C)]
pub struct PyByteArrayObject {
    pub ob_base: PyObject,
    pub ob_size: isize,
    pub ob_alloc: isize,
    pub ob_bytes: *mut c_char,
    pub ob_start: *mut c_char,
    pub ob_exports: isize,
}

// ------------------------------------------------------------ singletons

/// CPython's shared empty bytes singleton (`bytes_new_empty`).
#[repr(C)]
struct StaticBytes {
    header: PyBytesObject,
    nul: u8,
}

const fn static_bytes(size: isize, b: u8) -> StaticBytes {
    StaticBytes {
        header: PyBytesObject {
            ob_base: PyObject {
                ob_refcnt: header::IMMORTAL_REFCNT,
                ob_type: &raw mut PyBytes_Type,
            },
            ob_size: size,
            ob_shash: -1,
            ob_sval: [b],
        },
        nul: 0,
    }
}

static mut EMPTY_BYTES: StaticBytes = static_bytes(0, 0);

/// The 256 shared one-byte objects CPython caches in `characters[]`.
static mut CHARACTERS: [StaticBytes; 256] = {
    let mut table = [const { static_bytes(1, 0) }; 256];
    let mut b = 0usize;
    while b < 256 {
        table[b] = static_bytes(1, b as u8);
        b += 1;
    }
    table
};

/// The shared empty `bytes` object (`PyBytes_FromStringAndSize(NULL, 0)`).
pub(crate) fn empty() -> *mut PyObject {
    // SAFETY: the singletons are never written after init.
    unsafe { core::ptr::addr_of_mut!(EMPTY_BYTES.header.ob_base) }
}

/// The shared one-byte object for `b` (`getcharacter`).
pub(crate) fn character(b: u8) -> *mut PyObject {
    // SAFETY: the singletons are never written after init.
    unsafe { core::ptr::addr_of_mut!(CHARACTERS[b as usize].header.ob_base) }
}

// ----------------------------------------------------------- accessors

/// `PyBytes_Check`.
///
/// # Safety
///
/// `op` must be null or a valid object.
pub(crate) unsafe fn check(op: *const PyObject) -> bool {
    // SAFETY: the type read is within the object.
    unsafe {
        !op.is_null() && {
            let t = (*op).ob_type;
            let mut cursor = t;
            while !cursor.is_null() {
                if core::ptr::eq(cursor, &raw mut PyBytes_Type) {
                    return true;
                }
                cursor = (*(cursor as *const PyTypeObject)).tp_base;
            }
            false
        }
    }
}

/// `PyBytes_CheckExact`.
///
/// # Safety
///
/// `op` must be null or a valid object.
pub(crate) unsafe fn check_exact(op: *const PyObject) -> bool {
    // SAFETY: the type read is within the object.
    unsafe { !op.is_null() && core::ptr::eq((*op).ob_type, &raw mut PyBytes_Type) }
}

/// `PyByteArray_Check`.
///
/// # Safety
///
/// `op` must be null or a valid object.
pub(crate) unsafe fn bytearray_check(op: *const PyObject) -> bool {
    // SAFETY: the type read is within the object.
    unsafe {
        !op.is_null() && {
            let mut cursor = (*op).ob_type;
            while !cursor.is_null() {
                if core::ptr::eq(cursor, &raw mut PyByteArray_Type) {
                    return true;
                }
                cursor = (*(cursor as *const PyTypeObject)).tp_base;
            }
            false
        }
    }
}

/// `PyByteArray_CheckExact`.
///
/// # Safety
///
/// `op` must be null or a valid object.
pub(crate) unsafe fn bytearray_check_exact(op: *const PyObject) -> bool {
    // SAFETY: the type read is within the object.
    unsafe { !op.is_null() && core::ptr::eq((*op).ob_type, &raw mut PyByteArray_Type) }
}

/// `PyBytes_GET_SIZE`.
///
/// # Safety
///
/// `op` must be a bytes object.
pub(crate) unsafe fn size(op: *const PyObject) -> isize {
    // SAFETY: the size is the first payload field.
    unsafe { (*(op as *const coil_capi_sys::PyVarObject)).ob_size }
}

/// `PyBytes_AS_STRING`.
///
/// # Safety
///
/// `op` must be a bytes object.
pub(crate) unsafe fn as_string(op: *const PyObject) -> *mut c_char {
    // SAFETY: `ob_sval` sits at the pinned offset.
    unsafe { (op as *mut u8).add(bytes_layout::OB_SVAL) as *mut c_char }
}

/// The bytes contents as a slice.
///
/// # Safety
///
/// `op` must be a bytes object.
pub(crate) unsafe fn bytes_of<'a>(op: *const PyObject) -> &'a [u8] {
    // SAFETY: the inline buffer holds `size` bytes plus the terminator.
    unsafe {
        core::slice::from_raw_parts(
            (op as *const u8).add(bytes_layout::OB_SVAL),
            size(op) as usize,
        )
    }
}

/// The shared `(size, ptr)` pair the unicode module decodes through.
///
/// # Safety
///
/// `op` must be a bytes object.
pub(crate) unsafe fn as_string_parts(op: *mut PyObject) -> (isize, *mut c_char) {
    // SAFETY: the object is a bytes object.
    unsafe { (size(op), as_string(op)) }
}

/// `PyByteArray_GET_SIZE`.
///
/// # Safety
///
/// `op` must be a bytearray object.
pub(crate) unsafe fn bytearray_size(op: *const PyObject) -> isize {
    // SAFETY: the object is a bytearray.
    unsafe { (*(op as *const coil_capi_sys::PyVarObject)).ob_size }
}

/// The bytearray contents as a slice.
///
/// # Safety
///
/// `op` must be a bytearray object.
pub(crate) unsafe fn bytearray_bytes<'a>(op: *const PyObject) -> &'a [u8] {
    // SAFETY: the logical view holds `size` bytes.
    unsafe {
        core::slice::from_raw_parts(
            bytearray_as_string(op) as *const u8,
            bytearray_size(op) as usize,
        )
    }
}

/// A mutable bytearray slice; the caller must hold the resize conditions.
///
/// # Safety
///
/// `op` must be a bytearray object not shared with another view.
pub unsafe fn bytearray_bytes_mut<'a>(op: *mut PyObject) -> &'a mut [u8] {
    // SAFETY: the logical view holds `size` bytes.
    unsafe {
        core::slice::from_raw_parts_mut(
            bytearray_as_string(op) as *mut u8,
            bytearray_size(op) as usize,
        )
    }
}

// --------------------------------------------------------- construction

/// Build a `bytes` object over `bytes` (an empty input yields the shared
/// singleton, a known single byte the shared character). `None` means an
/// all-zero buffer of `size` (`PyBytes_FromStringAndSize(NULL, size)`),
/// which must stay a fresh writable object.
pub(crate) fn from_parts(bytes: Option<&[u8]>, size: isize) -> Option<*mut PyObject> {
    // SAFETY: allocation and writes land within the fresh object.
    unsafe {
        if size < 0 {
            return None;
        }
        if size == 0 {
            return Some(empty());
        }
        if let Some(data) = bytes
            && size == 1
        {
            return Some(character(data[0]));
        }
        let total = bytes_layout::OB_SVAL + size as usize + OB_SVAL_TERMINATOR;
        let block = crate::alloc::alloc_object(total);
        if block.is_null() {
            return None;
        }
        header::c_init(block.cast(), &raw mut PyBytes_Type);
        let object = block as *mut coil_capi_sys::PyVarObject;
        (*object).ob_size = size;
        let sval = block.add(bytes_layout::OB_SVAL);
        match bytes {
            Some(data) => {
                core::ptr::copy_nonoverlapping(data.as_ptr(), sval, size as usize);
            }
            None => core::ptr::write_bytes(sval, 0, size as usize),
        }
        *sval.add(size as usize) = 0;
        (*(block as *mut PyBytesObject)).ob_shash = -1;
        Some(block.cast())
    }
}

/// The `Option<Vec<u8>>` form the codecs hand back: `None` is the raised
/// case, `Some(v)` builds; the empty vector yields the shared singleton.
pub(crate) fn from_vec(bytes: Option<Vec<u8>>) -> Option<*mut PyObject> {
    match bytes {
        None => None,
        Some(v) => from_parts(Some(&v), v.len() as isize),
    }
}

/// `PyBytes_FromStringAndSize`.
///
/// # Safety
///
/// `s` must hold `size` bytes (or be null for `size` zero bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromStringAndSize(s: *const c_char, size: isize) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        let data = if s.is_null() {
            None
        } else {
            Some(core::slice::from_raw_parts(
                s.cast::<u8>(),
                size.max(0) as usize,
            ))
        };
        match from_parts(data, size) {
            Some(object) => object,
            None => {
                if size < 0 {
                    crate::tstate::set_string(
                        exceptions::system_error(),
                        b"Negative size passed to PyBytes_FromStringAndSize",
                    );
                } else {
                    crate::tstate::set_string(exceptions::memory_error(), b"");
                }
                core::ptr::null_mut()
            }
        }
    }
}

/// `PyBytes_FromString`.
///
/// # Safety
///
/// `s` must be a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromString(s: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is NUL-terminated per contract.
    unsafe {
        let mut len = 0usize;
        while *s.add(len) != 0 {
            len += 1;
        }
        PyBytes_FromStringAndSize(s, len as isize)
    }
}

// ------------------------------------------------------------- hashing

/// The SipHash-1-3 key state, shared with `str`: one per-process key.
pub(crate) fn hash_key() -> (u64, u64) {
    crate::types::unicode::current_hash_key()
}

/// `bytes_hash`: SipHash-1-3 over the raw bytes, cached in `ob_shash`.
///
/// # Safety
///
/// `op` must be a bytes object.
pub(crate) unsafe fn hash(op: *const PyObject) -> isize {
    // SAFETY: the object is a bytes object.
    unsafe {
        let slot = core::ptr::addr_of_mut!((*(op as *mut PyBytesObject)).ob_shash);
        let cached = *slot;
        if cached != -1 {
            return cached;
        }
        let data = bytes_of(op);
        // `_Py_HashBytes`: empty content hashes to 0, and the SipHash
        // result -1 is remapped so it never collides with the uncached
        // sentinel.
        let hash = if data.is_empty() {
            0
        } else {
            let key = hash_key();
            let hash = crate::types::unicode::hash_bytes_with(key, data) as isize;
            if hash == -1 { -2 } else { hash }
        };
        *slot = hash;
        hash
    }
}

// --------------------------------------------------------- comparison

/// `bytes_richcompare`: lexicographic over the raw bytes (padding the
/// shorter operand, which memcmp semantics give for free).
///
/// # Safety
///
/// Both arguments must be valid objects.
pub(crate) unsafe fn richcompare(
    left: *const PyObject,
    right: *const PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: both are bytes objects; slices are in bounds.
    unsafe {
        let a = bytes_of(left);
        let b = bytes_of(right);
        let order = a.cmp(b);
        let result = match op {
            Py_LT => order.is_lt(),
            Py_LE => order.is_le(),
            CMP_EQ => order.is_eq(),
            CMP_NE => order.is_ne(),
            Py_GT => order.is_gt(),
            Py_GE => order.is_ge(),
            _ => {
                crate::tstate::set_string(exceptions::system_error(), b"");
                return core::ptr::null_mut();
            }
        };
        if result {
            singletons::true_()
        } else {
            singletons::false_()
        }
    }
}

// ------------------------------------------------------------- iter

/// `PyBytesIterObject` (exact): iterator state and the sequence.
#[repr(C)]
pub struct PyBytesIterObject {
    pub ob_base: PyObject,
    pub it_index: isize,
    pub it_seq: *mut PyObject,
}

unsafe extern "C" fn bytes_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytes iterator.
    unsafe {
        let state = iterator as *mut PyBytesIterObject;
        let seq = (*state).it_seq;
        if check(seq) {
            let index = (*state).it_index;
            if index < size(seq) {
                (*state).it_index = index + 1;
                let b = bytes_of(seq)[index as usize];
                return crate::types::long::PyLong_FromLong(b as c_long);
            }
        } else if bytearray_check(seq) {
            let index = (*state).it_index;
            if index < bytearray_size(seq) {
                (*state).it_index = index + 1;
                let b = bytearray_bytes(seq)[index as usize];
                return crate::types::long::PyLong_FromLong(b as c_long);
            }
        }
        crate::tstate::set_string(exceptions::stop_iteration(), b"");
        core::ptr::null_mut()
    }
}

unsafe extern "C" fn bytes_iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from this module's allocator.
    unsafe {
        decref((*(object as *mut PyBytesIterObject)).it_seq);
        crate::alloc::free_block(object.cast());
    }
}

/// `bytes_iter`: a fresh iterator over the sequence's bytes.
///
/// # Safety
///
/// `seq` must be a bytes or bytearray object.
pub(crate) unsafe fn iter(seq: *mut PyObject) -> *mut PyObject {
    // SAFETY: allocation and init land within the fresh object.
    unsafe {
        let object = crate::alloc::alloc_object(core::mem::size_of::<PyBytesIterObject>());
        if object.is_null() {
            return object.cast();
        }
        header::c_init(object.cast(), &raw mut PyBytesIter_Type);
        let state = object as *mut PyBytesIterObject;
        (*state).it_index = 0;
        (*state).it_seq = seq;
        incref(seq);
        object.cast()
    }
}

/// `bytes_iternext` for the bytearray iterator (shares the bytes state
/// layout).
unsafe extern "C" fn bytearray_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytearray iterator.
    unsafe {
        let state = iterator as *mut PyBytesIterObject;
        let seq = (*state).it_seq;
        if bytearray_check(seq) {
            let index = (*state).it_index;
            if index < bytearray_size(seq) {
                (*state).it_index = index + 1;
                let b = bytearray_bytes(seq)[index as usize];
                return crate::types::long::PyLong_FromLong(b as c_long);
            }
        }
        crate::tstate::set_string(exceptions::stop_iteration(), b"");
        core::ptr::null_mut()
    }
}

// ---------------------------------------------------------- method cores

/// The default strip set: ASCII space and the C0 whitespace controls.
const DEFAULT_STRIP: &[u8] = b" \t\n\r\x0b\x0c";

/// `bytes.strip/lstrip/rstrip` (`do_xstrip`).
pub fn strip(bytes: &[u8], sep: &[u8], striptype: StripKind) -> Vec<u8> {
    let set = if sep.is_empty() { DEFAULT_STRIP } else { sep };
    let mut start = 0usize;
    let mut end = bytes.len();
    if striptype != StripKind::Right {
        while start < end && set.contains(&bytes[start]) {
            start += 1;
        }
    }
    if striptype != StripKind::Left {
        while end > start && set.contains(&bytes[end - 1]) {
            end -= 1;
        }
    }
    bytes[start..end].to_vec()
}

/// Which sides `strip` trims.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StripKind {
    Both,
    Left,
    Right,
}

/// `bytes.ljust/rjust/center` (`stringlib` padding).
pub fn pad(bytes: &[u8], width: isize, ch: u8, kind: PadKind) -> Vec<u8> {
    let len = bytes.len() as isize;
    if len >= width {
        return bytes.to_vec();
    }
    let marg = width - len;
    let left = match kind {
        PadKind::Right => 0,
        PadKind::Left => marg,
        PadKind::Center => marg / 2 + (marg & width & 1),
    };
    let mut out = Vec::with_capacity(width as usize);
    out.extend(core::iter::repeat_n(ch, left as usize));
    out.extend_from_slice(bytes);
    out.extend(core::iter::repeat_n(ch, (marg - left) as usize));
    out
}

/// Which side the text goes on.
#[derive(Clone, Copy)]
pub enum PadKind {
    Right,
    Left,
    Center,
}

/// `bytes.zfill`: sign-aware zero fill.
pub fn zfill(bytes: &[u8], width: isize) -> Vec<u8> {
    let len = bytes.len() as isize;
    if len >= width {
        return bytes.to_vec();
    }
    let mut sign_len = 0usize;
    if !bytes.is_empty() && (bytes[0] == b'+' || bytes[0] == b'-') {
        sign_len = 1;
    }
    let mut out = Vec::with_capacity(width as usize);
    out.extend_from_slice(&bytes[..sign_len]);
    out.extend(core::iter::repeat_n(b'0', (width - len) as usize));
    out.extend_from_slice(&bytes[sign_len..]);
    out
}

/// `bytes.count` over the adjusted range.
pub fn count(hay: &[u8], needle: &[u8], start: isize, end: isize) -> isize {
    let (start, end) = adjust_indices(hay.len() as isize, start, end);
    if end < start || needle.is_empty() {
        return if needle.is_empty() && end >= start {
            end - start + 1
        } else {
            0
        };
    }
    let hay = &hay[start as usize..end as usize];
    let mut count: isize = 0;
    let mut pos = 0usize;
    while pos + needle.len() <= hay.len() {
        if &hay[pos..pos + needle.len()] == needle {
            count += 1;
            pos += needle.len();
        } else {
            pos += 1;
        }
    }
    count
}

/// CPython's `ADJUST_INDICES`.
pub fn adjust_indices(len: isize, start: isize, end: isize) -> (isize, isize) {
    let mut end = end;
    if end > len {
        end = len;
    } else if end < 0 {
        end += len;
        if end < 0 {
            end = 0;
        }
    }
    let mut start = start;
    if start < 0 {
        start += len;
        if start < 0 {
            start = 0;
        }
    }
    (start, end)
}

/// `bytes.find` over the adjusted range (`stringlib_find`).
pub fn find(hay: &[u8], needle: &[u8], start: isize, end: isize) -> isize {
    let (start, end) = adjust_indices(hay.len() as isize, start, end);
    if end < start {
        return -1;
    }
    let hay = &hay[start as usize..end as usize];
    if needle.is_empty() {
        return start;
    }
    if let Some(pos) = memfind(hay, needle, 0) {
        return start + pos as isize;
    }
    -1
}

/// `bytes.rfind` over the adjusted range.
pub fn rfind(hay: &[u8], needle: &[u8], start: isize, end: isize) -> isize {
    let (start, end) = adjust_indices(hay.len() as isize, start, end);
    if end < start {
        return -1;
    }
    let hay = &hay[start as usize..end as usize];
    if needle.is_empty() {
        return end;
    }
    if hay.len() < needle.len() {
        return -1;
    }
    let mut pos = hay.len() - needle.len();
    loop {
        if &hay[pos..pos + needle.len()] == needle {
            return start + pos as isize;
        }
        if pos == 0 {
            return -1;
        }
        pos -= 1;
    }
}

/// A forward substring search (CPython's simplified Boyer-Moore-Horspool
/// is a plain scan at these sizes).
pub fn memfind(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    let mut i = from;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `bytes.startswith` over the adjusted range.
pub fn tailmatch(hay: &[u8], needle: &[u8], start: isize, end: isize, forward: bool) -> bool {
    let (start, end) = adjust_indices(hay.len() as isize, start, end);
    if end < start {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    if forward {
        let hay = &hay[start as usize..end as usize];
        hay.len() >= needle.len() && &hay[..needle.len()] == needle
    } else {
        let hay = &hay[start as usize..end as usize];
        hay.len() >= needle.len() && &hay[hay.len() - needle.len()..] == needle
    }
}

/// `bytes.replace` (`stringlib_replace`).
pub fn replace(hay: &[u8], old: &[u8], new: &[u8], count: isize) -> Vec<u8> {
    if old.is_empty() {
        // An empty pattern inserts before every character and at the end.
        let limit = if count < 0 { isize::MAX } else { count };
        let mut out = Vec::with_capacity(hay.len() + new.len() * (hay.len() + 1));
        let mut done = 0isize;
        if done < limit {
            out.extend_from_slice(new);
            done += 1;
        }
        for &b in hay {
            out.push(b);
            if done < limit {
                out.extend_from_slice(new);
                done += 1;
            }
        }
        return out;
    }
    let limit = if count < 0 { isize::MAX } else { count };
    let mut out = Vec::with_capacity(hay.len());
    let mut done = 0isize;
    let mut pos = 0usize;
    while done < limit {
        match memfind(hay, old, pos) {
            Some(found) => {
                out.extend_from_slice(&hay[pos..found]);
                out.extend_from_slice(new);
                pos = found + old.len();
                done += 1;
            }
            None => break,
        }
    }
    out.extend_from_slice(&hay[pos..]);
    out
}

/// `bytes.removeprefix/removesuffix`.
pub fn remove_affix(bytes: &[u8], affix: &[u8], prefix: bool) -> Vec<u8> {
    if prefix {
        if bytes.starts_with(affix) {
            bytes[affix.len()..].to_vec()
        } else {
            bytes.to_vec()
        }
    } else if bytes.ends_with(affix) {
        bytes[..bytes.len() - affix.len()].to_vec()
    } else {
        bytes.to_vec()
    }
}

/// ASCII letter test for the bytes case operations.
pub fn is_alpha_byte(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

/// `bytes.upper/lower` (ASCII only, as in CPython).
pub fn to_upper_or_lower(bytes: &[u8], upper: bool) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| {
            if upper {
                b.to_ascii_uppercase()
            } else {
                b.to_ascii_lowercase()
            }
        })
        .collect()
}

/// `bytes.capitalize`.
pub fn capitalize(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if let Some(first) = out.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    for b in out.iter_mut().skip(1) {
        *b = b.to_ascii_lowercase();
    }
    out
}

/// `bytes.title` (`stringlib` title case: cased characters continue a
/// word, everything else starts one).
pub fn title(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut previous_is_cased = false;
    for b in out.iter_mut() {
        if b.is_ascii_uppercase() {
            if previous_is_cased {
                *b = b.to_ascii_lowercase();
            }
            previous_is_cased = true;
        } else if b.is_ascii_lowercase() {
            if !previous_is_cased {
                *b = b.to_ascii_uppercase();
            }
            previous_is_cased = true;
        } else {
            previous_is_cased = false;
        }
    }
    out
}

/// `bytes.swapcase`.
pub fn swapcase(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_lowercase() {
                b.to_ascii_uppercase()
            } else if b.is_ascii_uppercase() {
                b.to_ascii_lowercase()
            } else {
                b
            }
        })
        .collect()
}

/// `bytes.isdigit` (ASCII digits only).
pub fn is_digit(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

/// `bytes.isalpha`.
pub fn is_alpha(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|b| is_alpha_byte(*b))
}

/// `bytes.isalnum`.
pub fn is_alnum(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| is_alpha_byte(*b) || b.is_ascii_digit())
}

/// `bytes.isascii`.
pub fn is_ascii(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| *b < 128)
}

/// `bytes.isspace` (ASCII whitespace).
pub fn is_space(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|b| DEFAULT_STRIP.contains(b))
}

/// `bytes.islower`: at least one cased character, all lowercase.
pub fn is_lower(bytes: &[u8]) -> bool {
    let mut cased = false;
    for &b in bytes {
        if b.is_ascii_uppercase() {
            return false;
        }
        cased |= b.is_ascii_lowercase();
    }
    cased
}

/// `bytes.isupper`.
pub fn is_upper(bytes: &[u8]) -> bool {
    let mut cased = false;
    for &b in bytes {
        if b.is_ascii_lowercase() {
            return false;
        }
        cased |= b.is_ascii_uppercase();
    }
    cased
}

/// `bytes.istitle`.
pub fn is_title(bytes: &[u8]) -> bool {
    let mut cased = false;
    let mut previous_is_cased = false;
    for &b in bytes {
        if b.is_ascii_uppercase() {
            if previous_is_cased {
                return false;
            }
            previous_is_cased = true;
            cased = true;
        } else if b.is_ascii_lowercase() {
            if !previous_is_cased {
                return false;
            }
            previous_is_cased = true;
            cased = true;
        } else {
            previous_is_cased = false;
        }
    }
    cased
}

/// `bytes.expandtabs`: a tab size below 1 removes tabs entirely.
pub fn expandtabs(bytes: &[u8], tabsize: isize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut column: isize = 0;
    for &b in bytes {
        if b == b'\t' {
            if tabsize > 0 {
                let incr = tabsize - column % tabsize;
                out.extend(core::iter::repeat_n(b' ', incr as usize));
                column += incr;
            }
        } else {
            out.push(b);
            column += 1;
            if b == b'\n' || b == b'\r' {
                column = 0;
            }
        }
    }
    out
}

/// `bytes.split/rsplit`: a `None` separator splits on ASCII whitespace
/// runs (`stringlib_split_whitespace`).
pub fn split(hay: &[u8], sep: Option<&[u8]>, maxsplit: isize, right: bool) -> Vec<Vec<u8>> {
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    match sep {
        None => {
            let mut count: isize = 0;
            if right {
                // Skip trailing whitespace, then take words from the
                // right; the remainder keeps its leading whitespace.
                let mut i = hay.len() as isize - 1;
                while i >= 0 && DEFAULT_STRIP.contains(&hay[i as usize]) {
                    i -= 1;
                }
                let mut end = (i + 1) as usize;
                while (count < maxsplit || maxsplit < 0) && i >= 0 {
                    while i >= 0 && !DEFAULT_STRIP.contains(&hay[i as usize]) {
                        i -= 1;
                    }
                    pieces.push(hay[(i + 1) as usize..end].to_vec());
                    count += 1;
                    while i >= 0 && DEFAULT_STRIP.contains(&hay[i as usize]) {
                        i -= 1;
                    }
                    end = (i + 1) as usize;
                }
                if end > 0 {
                    pieces.push(hay[..end].to_vec());
                }
                pieces.reverse();
            } else {
                // Skip leading whitespace, then take words; the remainder
                // keeps its trailing whitespace.
                let mut i = 0usize;
                while i < hay.len() && DEFAULT_STRIP.contains(&hay[i]) {
                    i += 1;
                }
                let mut start = i;
                while (count < maxsplit || maxsplit < 0) && i < hay.len() {
                    while i < hay.len() && !DEFAULT_STRIP.contains(&hay[i]) {
                        i += 1;
                    }
                    if i >= hay.len() {
                        break;
                    }
                    pieces.push(hay[start..i].to_vec());
                    count += 1;
                    while i < hay.len() && DEFAULT_STRIP.contains(&hay[i]) {
                        i += 1;
                    }
                    start = i;
                }
                if start < hay.len() {
                    pieces.push(hay[start..].to_vec());
                }
            }
            pieces
        }
        Some(sep) => {
            if sep.is_empty() {
                return pieces;
            }
            if right {
                let mut end = hay.len();
                let mut splits_left = maxsplit;
                let mut found = memfind_rfind(hay, sep, end);
                while let Some(pos) = found {
                    if splits_left == 0 {
                        break;
                    }
                    pieces.push(hay[pos + sep.len()..end].to_vec());
                    end = pos;
                    splits_left -= 1;
                    found = if pos == 0 {
                        None
                    } else {
                        memfind_rfind(hay, sep, pos)
                    };
                }
                pieces.push(hay[..end].to_vec());
                pieces.reverse();
            } else {
                let mut start = 0usize;
                let mut splits_left = maxsplit;
                while let Some(pos) = memfind(hay, sep, start) {
                    if splits_left == 0 {
                        break;
                    }
                    pieces.push(hay[start..pos].to_vec());
                    start = pos + sep.len();
                    splits_left -= 1;
                }
                pieces.push(hay[start..].to_vec());
            }
            pieces
        }
    }
}

/// A reverse substring search from `before` (exclusive upper bound).
pub fn memfind_rfind(hay: &[u8], needle: &[u8], before: usize) -> Option<usize> {
    if needle.is_empty() || before < needle.len() {
        return None;
    }
    let mut pos = before - needle.len();
    loop {
        if &hay[pos..pos + needle.len()] == needle {
            return Some(pos);
        }
        if pos == 0 {
            return None;
        }
        pos -= 1;
    }
}

/// `bytes.splitlines`: `\n`, `\r` and `\r\n` boundaries.
pub fn splitlines(hay: &[u8], keepends: bool) -> Vec<Vec<u8>> {
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < hay.len() {
        match hay[i] {
            b'\n' => {
                let end = i + 1;
                pieces.push(hay[start..if keepends { end } else { i }].to_vec());
                i = end;
                start = end;
            }
            b'\r' => {
                let mut end = i + 1;
                if end < hay.len() && hay[end] == b'\n' {
                    end += 1;
                }
                pieces.push(hay[start..if keepends { end } else { i }].to_vec());
                i = end;
                start = end;
            }
            _ => i += 1,
        }
    }
    if start < hay.len() {
        pieces.push(hay[start..].to_vec());
    }
    pieces
}

/// `bytes.partition/rpartition` over the found position.
pub fn partition(hay: &[u8], sep: &[u8], right: bool) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if sep.is_empty() {
        // An empty separator is found at the far end of the scan
        // direction; the reverse scan raises instead.
        if right {
            return None;
        }
        return Some((hay.to_vec(), Vec::new(), Vec::new()));
    }
    let found = if right {
        memfind_rfind(hay, sep, hay.len()).map(|p| p as isize)
    } else {
        memfind(hay, sep, 0).map(|p| p as isize)
    };
    // A separator that never occurs yields `(self, b"", b"")`, or
    // `(b"", b"", self)` for the reverse scan.
    let found = match found {
        None => {
            return if right {
                Some((Vec::new(), Vec::new(), hay.to_vec()))
            } else {
                Some((hay.to_vec(), Vec::new(), Vec::new()))
            };
        }
        Some(found) => found as usize,
    };
    let head = hay[..found].to_vec();
    let middle = sep.to_vec();
    let tail = hay[found + sep.len()..].to_vec();
    Some((head, middle, tail))
}

/// `bytes.join` over objects exposing their bytes.
///
/// # Safety
///
/// Every item must be a bytes or bytearray object.
pub unsafe fn join_bytes(sep: *const PyObject, items: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: the objects are bytes-like per the contract.
    unsafe {
        let sep_bytes = if bytearray_check(sep) {
            bytearray_bytes(sep)
        } else {
            bytes_of(sep)
        };
        let mut total = (items.len() as isize - 1).max(0) * sep_bytes.len() as isize;
        for &item in items {
            total += if bytearray_check(item) {
                bytearray_size(item)
            } else {
                size(item)
            };
        }
        let mut out = Vec::with_capacity(total.max(0) as usize);
        for (i, &item) in items.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(sep_bytes);
            }
            if bytearray_check(item) {
                out.extend_from_slice(bytearray_bytes(item));
            } else {
                out.extend_from_slice(bytes_of(item));
            }
        }
        from_vec(Some(out)).unwrap_or(core::ptr::null_mut())
    }
}

/// `bytes.maketrans(frm, to, delete)`: the 256-entry identity table with
/// the mappings and deletions applied.
pub fn maketrans(frm: &[u8], to: &[u8], delete: &[u8]) -> Option<Vec<u8>> {
    if frm.len() != to.len() {
        return None;
    }
    let mut table: Vec<u8> = (0..=255u8).collect();
    for &b in delete {
        table[b as usize] = 0;
    }
    for (i, &b) in frm.iter().enumerate() {
        table[b as usize] = to[i];
    }
    Some(table)
}

/// `bytes.translate`: `None` tables map 1:1.
pub fn translate(bytes: &[u8], table: Option<&[u8]>, delete: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if delete.contains(&b) {
            continue;
        }
        match table {
            Some(t) => out.push(t[b as usize]),
            None => out.push(b),
        }
    }
    out
}

/// `bytes.hex` (`_Py_strhex_impl`): a negative group size anchors the
/// groups at the left end, a positive one at the right end.
pub fn hex(bytes: &[u8], sep: &[u8], bytes_per_sep: isize) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = Vec::with_capacity(bytes.len() * 2);
    let abs_bp = bytes_per_sep.unsigned_abs();
    if bytes_per_sep == 0 || abs_bp >= bytes.len() {
        for &b in bytes {
            result.push(HEX[(b >> 4) as usize]);
            result.push(HEX[(b & 0xF) as usize]);
        }
        return result;
    }
    let chunks = (bytes.len() - 1) / abs_bp;
    let sep_char = sep[0];
    if bytes_per_sep < 0 {
        let mut i = 0usize;
        for _ in 0..chunks {
            for _ in 0..abs_bp {
                let c = bytes[i];
                i += 1;
                result.push(HEX[(c >> 4) as usize]);
                result.push(HEX[(c & 0xF) as usize]);
            }
            result.push(sep_char);
        }
        while i < bytes.len() {
            let c = bytes[i];
            i += 1;
            result.push(HEX[(c >> 4) as usize]);
            result.push(HEX[(c & 0xF) as usize]);
        }
    } else {
        let mut i = bytes.len() as isize - 1;
        for _ in 0..chunks {
            for _ in 0..abs_bp {
                let c = bytes[i as usize];
                i -= 1;
                result.push(HEX[(c & 0xF) as usize]);
                result.push(HEX[(c >> 4) as usize]);
            }
            result.push(sep_char);
        }
        while i >= 0 {
            let c = bytes[i as usize];
            i -= 1;
            result.push(HEX[(c & 0xF) as usize]);
            result.push(HEX[(c >> 4) as usize]);
        }
        result.reverse();
    }
    result
}

/// `bytes.fromhex` (`_PyBytes_FromHex`); the caller picks the result
/// type. Every invalid input raises the same error, positioned at the
/// first offending character.
pub fn from_hex(string: &[u32]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(string.len() / 2);
    let len = string.len();
    let mut i = 0usize;
    while i < len {
        if is_ctype_space(string[i]) {
            while i < len && is_ctype_space(string[i]) {
                i += 1;
            }
            if i >= len {
                break;
            }
        }
        let top = hex_digit(string[i]);
        if top >= 16 {
            raise_from_hex(i);
            return None;
        }
        i += 1;
        let bot = if i < len { hex_digit(string[i]) } else { 99 };
        if bot >= 16 {
            raise_from_hex(i);
            return None;
        }
        i += 1;
        out.push(((top << 4) + bot) as u8);
    }
    Some(out)
}

/// C's `Py_ISSPACE`: ASCII tab, newline, vertical tab, form feed,
/// carriage return and space.
fn is_ctype_space(ch: u32) -> bool {
    matches!(ch, 0x09..=0x0D | 0x20)
}

fn hex_digit(ch: u32) -> u32 {
    match ch {
        0x30..=0x39 => ch - 0x30,
        0x61..=0x66 => ch - 0x61 + 10,
        0x41..=0x46 => ch - 0x41 + 10,
        _ => 99,
    }
}

fn raise_from_hex(position: usize) {
    crate::tstate::set_string(
        exceptions::value_error(),
        format!("non-hexadecimal number found in fromhex() arg at position {position}").as_bytes(),
    );
}

/// `_PyBytes_DecodeEscape`: the bytes literal escape processor. The
/// deprecation warning for unknown escapes is the warnings task's, so
/// unknown escapes pass through silently as CPython's non-warning build
/// does.
pub fn decode_escape(s: &[u8], errors: Option<&str>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        if s[i] != b'\\' {
            out.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i == s.len() {
            crate::tstate::set_string(exceptions::value_error(), b"Trailing \\ in string");
            return None;
        }
        let c = s[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b'v' => out.push(0x0B),
            b'a' => out.push(0x07),
            b'0'..=b'7' => {
                let mut value = (c - b'0') as u32;
                for _ in 0..2 {
                    if i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                        value = (value << 3) + (s[i] - b'0') as u32;
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(value as u8);
            }
            b'x' => {
                if i + 1 < s.len() {
                    let digit1 = hex_digit(s[i] as u32);
                    let digit2 = hex_digit(s[i + 1] as u32);
                    if digit1 < 16 && digit2 < 16 {
                        out.push(((digit1 << 4) + digit2) as u8);
                        i += 2;
                        continue;
                    }
                }
                match errors {
                    None | Some("strict") => {
                        crate::tstate::set_string(
                            exceptions::value_error(),
                            format!("invalid \\x escape at position {}", i - 2).as_bytes(),
                        );
                        return None;
                    }
                    Some("replace") => out.push(b'?'),
                    Some("ignore") => {
                        if i < s.len() && s[i].is_ascii_hexdigit() {
                            i += 1;
                        }
                    }
                    Some(other) => {
                        crate::tstate::set_string(
                            exceptions::value_error(),
                            format!("decoding error; unknown error handling code: {other}")
                                .as_bytes(),
                        );
                        return None;
                    }
                }
                if i < s.len() && s[i].is_ascii_hexdigit() {
                    i += 1;
                }
            }
            _ => {
                out.push(b'\\');
                i -= 1;
            }
        }
    }
    Some(out)
}

// ------------------------------------------------------------- bytes C API

/// `PyBytes_Size`.
///
/// # Safety
///
/// `op` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Size(op: *mut PyObject) -> isize {
    // SAFETY: the object is checked before the size read.
    unsafe {
        if op.is_null() || !check(op) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        size(op)
    }
}

/// `PyBytes_AsString`.
///
/// # Safety
///
/// `op` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_AsString(op: *mut PyObject) -> *mut c_char {
    // SAFETY: the object is checked before the pointer read.
    unsafe {
        if op.is_null() || !check(op) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        as_string(op)
    }
}

/// `PyBytes_AsStringAndSize`.
///
/// # Safety
///
/// `op` must be null or a valid object; both out-pointers writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_AsStringAndSize(
    op: *mut PyObject,
    buf: *mut *mut c_char,
    len: *mut isize,
) -> c_int {
    // SAFETY: the out-pointers are writable per the contract.
    unsafe {
        if op.is_null() || !check(op) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        if buf.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"NULL buffer argument passed to PyBytes_AsStringAndSize",
            );
            return -1;
        }
        if len.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"NULL length argument passed to PyBytes_AsStringAndSize",
            );
            return -1;
        }
        *buf = as_string(op);
        *len = size(op);
        0
    }
}

/// `bytes_concat`: both operands must be bytes.
///
/// # Safety
///
/// Both arguments must be valid objects.
pub(crate) unsafe fn concat(a: *const PyObject, b: *const PyObject) -> *mut PyObject {
    // SAFETY: both operands are valid objects.
    unsafe {
        if !check(a) || !check(b) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("can't concat {} to {}", name_of(b), name_of(a)).as_bytes(),
            );
            return core::ptr::null_mut();
        }
        let (alen, adata) = as_string_parts(a as *mut PyObject);
        let (blen, bdata) = as_string_parts(b as *mut PyObject);
        let total = match alen.checked_add(blen) {
            Some(total) => total,
            None => {
                crate::tstate::set_string(
                    exceptions::overflow_error(),
                    b"joined bytes is too long",
                );
                return core::ptr::null_mut();
            }
        };
        let Some(object) = from_parts(None, total) else {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        };
        let target = as_string(object);
        core::ptr::copy_nonoverlapping(adata, target, alen as usize);
        core::ptr::copy_nonoverlapping(bdata, target.add(alen as usize), blen as usize);
        object
    }
}

unsafe fn name_of(op: *const PyObject) -> &'static str {
    // SAFETY: the type name is a valid C string on live types.
    unsafe { util::cstr((*header::type_of(op)).tp_name) }
}

/// `PyBytes_Concat`.
///
/// # Safety
///
/// `pv` must be a writable object slot; `w` a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Concat(pv: *mut *mut PyObject, w: *mut PyObject) {
    // SAFETY: the slot is writable per the contract.
    unsafe {
        let v = *pv;
        if v.is_null() {
            incref(w);
            *pv = w;
            return;
        }
        *pv = concat(v, w);
    }
}

/// `PyBytes_ConcatAndDel`.
///
/// # Safety
///
/// `pv` must be a writable object slot; `w` a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_ConcatAndDel(pv: *mut *mut PyObject, w: *mut PyObject) {
    // SAFETY: the slot is writable per the contract.
    unsafe {
        let v = *pv;
        if v.is_null() {
            *pv = w;
            return;
        }
        *pv = concat(v, w);
        decref(w);
    }
}

/// `PyBytes_FromObject`: an exact object is shared, a bytes or bytearray
/// copy; anything else must offer iteration of small ints.
///
/// # Safety
///
/// `x` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromObject(x: *mut PyObject) -> *mut PyObject {
    // SAFETY: all reads are within valid objects.
    unsafe {
        if x.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        if check_exact(x) {
            incref(x);
            return x;
        }
        if check(x) {
            let data = bytes_of(x).to_vec();
            return from_vec(Some(data)).unwrap_or(core::ptr::null_mut());
        }
        if bytearray_check(x) {
            let data = bytearray_bytes(x).to_vec();
            return from_vec(Some(data)).unwrap_or(core::ptr::null_mut());
        }
        let ty = header::type_of(x);
        if (*ty).tp_iter.is_some() {
            // An iterable of integers in 0..256.
            let iterator = ((*ty).tp_iter.unwrap())(x);
            if iterator.is_null() {
                return core::ptr::null_mut();
            }
            let mut out: Vec<u8> = Vec::new();
            while let Some(item) = iter_next_item(iterator) {
                let value = long_as_byte(item);
                decref(item);
                let Some(byte) = value else {
                    decref(iterator);
                    return core::ptr::null_mut();
                };
                out.push(byte);
            }
            decref(iterator);
            if !crate::tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return from_vec(Some(out)).unwrap_or(core::ptr::null_mut());
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("cannot convert '{}' object to bytes", name_of(x)).as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// One `PyIter_Next` step: `None` when the iterator is exhausted (the
/// StopIteration signal consumed, like CPython's `PyIter_Next`) or when a
/// real error stays pending for the caller.
///
/// # Safety
///
/// `iterator` must be a valid iterator object.
pub(crate) unsafe fn iter_next_item(iterator: *mut PyObject) -> Option<*mut PyObject> {
    // SAFETY: the type read is within the object.
    unsafe {
        let next = (*(header::type_of(iterator))).tp_iternext?;
        let result = next(iterator);
        if result.is_null() {
            if crate::tstate::occurred() == exceptions::stop_iteration() {
                crate::tstate::clear();
            }
            return None;
        }
        Some(result)
    }
}

/// An exact small int as a byte, or the raised error.
///
/// # Safety
///
/// `item` must be a valid object.
pub(crate) unsafe fn long_as_byte(item: *mut PyObject) -> Option<u8> {
    // SAFETY: the int conversion validates its own operand.
    unsafe {
        if !crate::types::long::is_long(item) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!(
                    "'{}' object cannot be interpreted as an integer",
                    name_of(item)
                )
                .as_bytes(),
            );
            return None;
        }
        let value = crate::types::long::PyLong_AsLongLong(item);
        if !crate::tstate::occurred().is_null() {
            return None;
        }
        if !(0..=255).contains(&value) {
            crate::tstate::set_string(exceptions::value_error(), b"bytes must be in range(0, 256)");
            return None;
        }
        Some(value as u8)
    }
}

/// `PyObject_Bytes`: `bytes(v)` through the C API. The `__bytes__`
/// dispatch needs the descriptor machinery; no object can define it yet,
/// so CPython's fallback applies.
///
/// # Safety
///
/// `v` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Bytes(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the checks precede every read.
    unsafe {
        if v.is_null() {
            return from_parts(Some(b"<NULL>"), 6).unwrap_or(core::ptr::null_mut());
        }
        if check_exact(v) {
            incref(v);
            return v;
        }
        PyBytes_FromObject(v)
    }
}

/// `_Py_HashBytes`: the SipHash-1-3 of a byte range under the process
/// key; 0 for empty, -1 mapped to -2.
///
/// # Safety
///
/// `src` must hold `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_HashBytes(src: *const c_void, len: isize) -> isize {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        if len <= 0 {
            return 0;
        }
        crate::types::unicode::hash_bytes(core::slice::from_raw_parts(
            src.cast::<u8>(),
            len as usize,
        ))
    }
}

/// `_Py_strhex_bytes`: the hex form of a byte range as `bytes`.
///
/// # Safety
///
/// `argbuf` must hold `arglen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_strhex_bytes(argbuf: *const c_char, arglen: isize) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's.
    unsafe {
        let data = core::slice::from_raw_parts(argbuf.cast::<u8>(), arglen.max(0) as usize);
        from_vec(Some(hex(data, b"", 0))).unwrap_or(core::ptr::null_mut())
    }
}

/// `_Py_strhex_bytes_with_sep`: the hex form grouped by `bytes_per_group`
/// around a one-byte separator.
///
/// # Safety
///
/// `argbuf` must hold `arglen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_strhex_bytes_with_sep(
    argbuf: *const c_char,
    arglen: isize,
    sep: *mut PyObject,
    bytes_per_group: c_int,
) -> *mut PyObject {
    // SAFETY: the buffer contract is the caller's, and the object reads
    // follow the checks.
    unsafe {
        let sep_char = if sep.is_null() {
            0u8
        } else {
            let length = if crate::types::unicode::check(sep) {
                crate::types::unicode::get_length(sep)
            } else if check(sep) {
                size(sep)
            } else {
                crate::tstate::set_string(exceptions::type_error(), b"sep must be str or bytes.");
                return core::ptr::null_mut();
            };
            if length != 1 {
                crate::tstate::set_string(exceptions::value_error(), b"sep must be length 1.");
                return core::ptr::null_mut();
            }
            if crate::types::unicode::check(sep) {
                if crate::types::unicode::kind(sep) != crate::types::unicode::PY_UNICODE_1BYTE_KIND
                {
                    crate::tstate::set_string(exceptions::value_error(), b"sep must be ASCII.");
                    return core::ptr::null_mut();
                }
                crate::types::unicode::read(
                    crate::types::unicode::kind(sep),
                    crate::types::unicode::data(sep),
                    0,
                ) as u8
            } else {
                *as_string(sep) as u8
            }
        };
        let data = core::slice::from_raw_parts(argbuf.cast::<u8>(), arglen.max(0) as usize);
        let seps: &[u8] = if sep.is_null() { b"" } else { &[sep_char] };
        from_vec(Some(hex(data, seps, bytes_per_group as isize))).unwrap_or(core::ptr::null_mut())
    }
}

/// `PyBytes_Repr` (`smartquotes` picks `"` when the bytes hold only
/// single quotes).
///
/// # Safety
///
/// `obj` must be a bytes object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Repr(obj: *mut PyObject, smartquotes: c_int) -> *mut PyObject {
    // SAFETY: the object is a bytes object.
    unsafe {
        let bytes = bytes_of(obj);
        let text = repr_bytes(bytes, smartquotes != 0);
        match crate::types::unicode::PyUnicode_FromStringAndSize(
            text.as_ptr().cast(),
            text.len() as isize,
        ) {
            object if !object.is_null() => object,
            _ => core::ptr::null_mut(),
        }
    }
}

/// The `b'...'` escape rendering (`PyBytes_Repr` over raw bytes).
pub(crate) fn repr_bytes(bytes: &[u8], smartquotes: bool) -> Vec<u8> {
    let squotes = bytes.iter().filter(|&&b| b == b'\'').count();
    let dquotes = bytes.iter().filter(|&&b| b == b'"').count();
    let quote = if smartquotes && squotes > 0 && dquotes == 0 {
        b'"'
    } else {
        b'\''
    };
    let mut out = Vec::with_capacity(bytes.len() * 4 + 3);
    out.push(b'b');
    out.push(quote);
    for &c in bytes {
        if c == quote || c == b'\\' {
            out.push(b'\\');
            out.push(c);
        } else if c == b'\t' {
            out.extend_from_slice(b"\\t");
        } else if c == b'\n' {
            out.extend_from_slice(b"\\n");
        } else if c == b'\r' {
            out.extend_from_slice(b"\\r");
        } else if !(0x20..0x7f).contains(&c) {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            out.extend_from_slice(b"\\x");
            out.push(HEX[((c & 0xf0) >> 4) as usize]);
            out.push(HEX[(c & 0xf) as usize]);
        } else {
            out.push(c);
        }
    }
    out.push(quote);
    out
}

/// `bytes_repr` slot: `PyBytes_Repr` with smartquotes.
unsafe extern "C" fn bytes_repr_slot(op: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a valid bytes object.
    unsafe { PyBytes_Repr(op, 1) }
}

/// `bytes_str`: the `BytesWarning` runtime flag is a warnings-task item,
/// so this is the repr.
unsafe extern "C" fn bytes_str(op: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a valid bytes object.
    unsafe { bytes_repr_slot(op) }
}

/// `PyBytes_DecodeEscape`: the bytes-literal escape processor.
///
/// # Safety
///
/// `s` must hold `len` bytes; `errors`/`unicode` NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_DecodeEscape(
    s: *const c_char,
    len: isize,
    errors: *const c_char,
    unicode: *const c_char,
) -> *mut PyObject {
    // SAFETY: the C strings are NUL-terminated per contract.
    unsafe {
        let errors = if errors.is_null() {
            None
        } else {
            Some(util::cstr(errors))
        };
        // The DeprecationWarning on unknown escapes (CPython's
        // `first_invalid_escape`, raised when `unicode` is given) belongs
        // to the warnings task; strict decoding is in place.
        let _ = unicode;
        let data = core::slice::from_raw_parts(s.cast::<u8>(), len.max(0) as usize);
        match decode_escape(data, errors) {
            Some(out) => from_vec(Some(out)).unwrap_or(core::ptr::null_mut()),
            None => core::ptr::null_mut(),
        }
    }
}

/// `_PyBytes_DecodeEscape`: the internal form without the warning hook.
///
/// # Safety
///
/// `s` must hold `len` bytes; `errors` NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_DecodeEscape(
    s: *const c_char,
    len: isize,
    errors: *const c_char,
    first_invalid_escape: *mut *const c_char,
) -> *mut PyObject {
    // SAFETY: the C strings are NUL-terminated per contract.
    unsafe {
        let result = PyBytes_DecodeEscape(s, len, errors, core::ptr::null());
        if !result.is_null() && !first_invalid_escape.is_null() {
            *first_invalid_escape = core::ptr::null();
        }
        result
    }
}

/// `PyBytes_FromFormat`: variadic formatting arrives with the argument
/// machinery.
///
/// # Safety
///
/// `format` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromFormat(
    _format: *const c_char,
    mut _args: ...
) -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyBytes_FromFormat is not implemented",
    );
    core::ptr::null_mut()
}

/// `PyBytes_FromFormatV`: variadic formatting arrives with the argument
/// machinery (and stable Rust cannot walk a `va_list`).
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromFormatV(
    format: *const c_char,
    args: *mut c_void,
) -> *mut PyObject {
    let _ = (format, args);
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyBytes_FromFormatV is not implemented",
    );
    core::ptr::null_mut()
}

/// `_PyBytes_Find`: substring search with adjusted bounds.
///
/// # Safety
///
/// `v` must hold `len_v` bytes; `w` `len_w` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_Find(
    v: *const c_char,
    len_v: isize,
    w: *const c_char,
    len_w: isize,
    start: isize,
    direction: c_int,
) -> isize {
    // SAFETY: both buffers hold their lengths.
    unsafe {
        let hay = core::slice::from_raw_parts(v.cast::<u8>(), len_v.max(0) as usize);
        let needle = core::slice::from_raw_parts(w.cast::<u8>(), len_w.max(0) as usize);
        if direction > 0 {
            find(hay, needle, start, len_v)
        } else {
            rfind(hay, needle, start, len_v)
        }
    }
}

/// `_PyBytes_ReverseFind`.
///
/// # Safety
///
/// `s` must hold `n` bytes; `sub` `sublen` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_ReverseFind(
    s: *const c_char,
    n: isize,
    sub: *const c_char,
    sublen: isize,
) -> isize {
    // SAFETY: both buffers hold their lengths.
    unsafe {
        let hay = core::slice::from_raw_parts(s.cast::<u8>(), n.max(0) as usize);
        let needle = core::slice::from_raw_parts(sub.cast::<u8>(), sublen.max(0) as usize);
        if needle.is_empty() {
            return n;
        }
        if hay.len() < needle.len() {
            return -1;
        }
        memfind_rfind(hay, needle, hay.len())
            .map(|p| p as isize)
            .unwrap_or(-1)
    }
}

/// `_PyBytes_Repeat`: fill `dest` with `len_src` bytes repeated.
///
/// # Safety
///
/// `dest` must hold `len_dest` bytes; `src` `len_src` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_Repeat(
    dest: *mut c_char,
    len_dest: isize,
    src: *const c_char,
    len_src: isize,
) {
    // SAFETY: both buffers hold their lengths.
    unsafe {
        if len_dest == 0 {
            return;
        }
        if len_src == 1 {
            core::ptr::write_bytes(dest, *src as u8, len_dest as usize);
            return;
        }
        core::ptr::copy_nonoverlapping(src, dest, len_src as usize);
        let mut copied = len_src;
        while copied < len_dest {
            let take = copied.min(len_dest - copied);
            core::ptr::copy(dest, dest.add(copied as usize), take as usize);
            copied += take;
        }
    }
}

/// `_PyBytes_Resize`: shrink or grow a uniquely owned bytes object.
///
/// # Safety
///
/// `pv` must be a writable object slot holding a bytes object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_Resize(pv: *mut *mut PyObject, newsize: isize) -> c_int {
    // SAFETY: the slot and the object it holds are valid.
    unsafe {
        let v = *pv;
        if v.is_null() || !check(v) || newsize < 0 {
            *pv = core::ptr::null_mut();
            if !v.is_null() {
                decref(v);
            }
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        if size(v) == newsize {
            return 0;
        }
        if size(v) == 0 {
            // The empty object is shared: swap in a fresh one.
            let fresh = from_parts(None, newsize);
            *pv = fresh.unwrap_or(core::ptr::null_mut());
            decref(v);
            return if fresh.is_some() { 0 } else { -1 };
        }
        if header::refcnt(v) != 1 {
            *pv = core::ptr::null_mut();
            decref(v);
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        if newsize == 0 {
            *pv = empty();
            decref(v);
            return 0;
        }
        let old = size(v);
        let new_total = bytes_layout::OB_SVAL + newsize as usize + OB_SVAL_TERMINATOR;
        let block = v as *mut u8;
        let capacity = crate::alloc::block_capacity(block);
        let resized = match capacity {
            Some(capacity) if capacity >= new_total => {
                // The block already fits: adjust in place.
                let sval = block.add(bytes_layout::OB_SVAL);
                *sval.add(newsize as usize) = 0;
                (*(v as *mut coil_capi_sys::PyVarObject)).ob_size = newsize;
                (*(v as *mut PyBytesObject)).ob_shash = -1;
                Some(v)
            }
            _ => {
                let fresh = crate::alloc::alloc_object(new_total);
                if fresh.is_null() {
                    None
                } else {
                    header::c_init(fresh.cast(), &raw mut PyBytes_Type);
                    let take = old.min(newsize);
                    core::ptr::copy(
                        block.add(bytes_layout::OB_SVAL),
                        fresh.add(bytes_layout::OB_SVAL),
                        take as usize,
                    );
                    *fresh.add(bytes_layout::OB_SVAL + newsize as usize) = 0;
                    (*(fresh as *mut coil_capi_sys::PyVarObject)).ob_size = newsize;
                    (*(fresh as *mut PyBytesObject)).ob_shash = -1;
                    crate::alloc::free_block(block);
                    Some(fresh.cast())
                }
            }
        };
        match resized {
            Some(object) => {
                *pv = object;
                0
            }
            None => {
                *pv = core::ptr::null_mut();
                crate::tstate::set_string(exceptions::memory_error(), b"");
                -1
            }
        }
    }
}

/// `_PyBytes_FromHex`.
///
/// # Safety
///
/// `string` must be a str object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_FromHex(
    string: *mut PyObject,
    use_bytearray: c_int,
) -> *mut PyObject {
    // SAFETY: the object is a str object.
    unsafe {
        let Some(chars) = crate::types::unicode::as_chars(string) else {
            return core::ptr::null_mut();
        };
        let parsed = from_hex(&chars.as_vec());
        if parsed.is_none() {
            return core::ptr::null_mut();
        }
        if use_bytearray != 0 {
            bytearray_from_vec(parsed)
        } else {
            from_vec(parsed).unwrap_or(core::ptr::null_mut())
        }
    }
}

// -------------------------------------------------------- %-formatting

const F_LJUST: c_int = 0x01;
const F_SIGN: c_int = 0x02;
const F_BLANK: c_int = 0x04;
const F_ALT: c_int = 0x08;
const F_ZERO: c_int = 0x10;

/// `_PyBytes_FormatEx`: the bytes `%`-formatter. The tuple form arrives
/// with the container runtime, so like `PyUnicode_Format` this is the
/// single-value form: one argument available, then no more.
///
/// # Safety
///
/// `format` must hold `format_len` bytes; `args` a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_FormatEx(
    format: *const c_char,
    format_len: isize,
    args: *mut PyObject,
    use_bytearray: c_int,
) -> *mut PyObject {
    // SAFETY: the format buffer holds its length.
    unsafe {
        if args.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        let fmt = core::slice::from_raw_parts(format.cast::<u8>(), format_len.max(0) as usize);
        let out = match format_core(fmt, args) {
            Some(out) => out,
            None => return core::ptr::null_mut(),
        };
        if use_bytearray != 0 {
            bytearray_from_vec(Some(out))
        } else {
            from_vec(Some(out)).unwrap_or(core::ptr::null_mut())
        }
    }
}

/// The formatter body: `None` means an error was raised.
fn format_core(fmt: &[u8], args: *mut PyObject) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(fmt.len());
    // The single-value bookkeeping from CPython's formatter.
    let arglen: isize = -1;
    let mut argidx: isize = -2;
    let mut pos = 0usize;
    while pos < fmt.len() {
        if fmt[pos] != b'%' {
            out.push(fmt[pos]);
            pos += 1;
            continue;
        }
        pos += 1;
        if !unsafe { format_one(fmt, &mut pos, args, arglen, &mut argidx, &mut out) } {
            return None;
        }
    }
    if argidx < arglen {
        crate::tstate::set_string(
            exceptions::type_error(),
            b"not all arguments converted during bytes formatting",
        );
        return None;
    }
    Some(out)
}

/// The whole single argument, then no more (CPython's `getnextarg`).
///
/// # Safety
///
/// `args` must be a valid object.
unsafe fn get_next_arg(args: *mut PyObject, arglen: isize, argidx: &mut isize) -> *mut PyObject {
    if *argidx < arglen {
        *argidx += 1;
        if arglen < 0 {
            return args;
        }
        crate::tstate::set_string(
            exceptions::system_error(),
            b"coil: _PyBytes_FormatEx needs tuple arguments from the container runtime",
        );
        return core::ptr::null_mut();
    }
    crate::tstate::set_string(
        exceptions::type_error(),
        b"not enough arguments for format string",
    );
    core::ptr::null_mut()
}

/// One `%` directive: parse flags/width/precision, format the argument,
/// and append the padded piece.
///
/// # Safety
///
/// `args` must be a valid object.
unsafe fn format_one(
    fmt: &[u8],
    pos: &mut usize,
    args: *mut PyObject,
    arglen: isize,
    argidx: &mut isize,
    out: &mut Vec<u8>,
) -> bool {
    // SAFETY: reads stay within the format; argument dispatch checks types.
    unsafe {
        // The characters left to scan, starting at the one after '%'.
        let mut remaining = fmt.len() as isize - *pos as isize;
        let mut flags: c_int = 0;
        let mut width: isize = -1;
        let mut prec: c_int = -1;
        // The candidate type char: `None` once the format ends before one.
        let mut tychar: Option<u8> = None;
        let mut v: *mut PyObject;

        if remaining > 0 && fmt[*pos] == b'%' {
            *pos += 1;
            out.push(b'%');
            return true;
        }

        // Mapping keys (`%(name)s`) need PyObject_GetItem; the container
        // runtime owns mappings.
        if remaining > 0 && fmt[*pos] == b'(' {
            crate::tstate::set_string(exceptions::type_error(), b"format requires a mapping");
            return false;
        }

        // Flags.
        while remaining > 0 {
            let c = fmt[*pos];
            *pos += 1;
            remaining -= 1;
            match c {
                b'-' => {
                    flags |= F_LJUST;
                    continue;
                }
                b'+' => {
                    flags |= F_SIGN;
                    continue;
                }
                b' ' => {
                    flags |= F_BLANK;
                    continue;
                }
                b'#' => {
                    flags |= F_ALT;
                    continue;
                }
                b'0' => {
                    flags |= F_ZERO;
                    continue;
                }
                _ => {
                    tychar = Some(c);
                    break;
                }
            }
        }

        // Width.
        if tychar == Some(b'*') {
            v = get_next_arg(args, arglen, argidx);
            if v.is_null() {
                return false;
            }
            let Some(parsed) = long_as_ssize(v, b"* wants int") else {
                return false;
            };
            if parsed < 0 {
                flags |= F_LJUST;
                width = -parsed;
            } else {
                width = parsed;
            }
            if remaining > 0 {
                tychar = Some(fmt[*pos]);
                *pos += 1;
                remaining -= 1;
            } else {
                tychar = None;
            }
        } else if matches!(tychar, Some(c) if c.is_ascii_digit()) {
            width = (tychar.unwrap() - b'0') as isize;
            while remaining > 0 {
                let c = fmt[*pos];
                *pos += 1;
                remaining -= 1;
                if !c.is_ascii_digit() {
                    tychar = Some(c);
                    break;
                }
                if width > (isize::MAX - (c - b'0') as isize) / 10 {
                    crate::tstate::set_string(exceptions::value_error(), b"width too big");
                    return false;
                }
                width = width * 10 + (c - b'0') as isize;
                tychar = None;
            }
        }

        // Precision.
        if tychar == Some(b'.') {
            prec = 0;
            if remaining > 0 {
                tychar = Some(fmt[*pos]);
                *pos += 1;
                remaining -= 1;
            } else {
                tychar = None;
            }
            if tychar == Some(b'*') {
                v = get_next_arg(args, arglen, argidx);
                if v.is_null() {
                    return false;
                }
                let Some(parsed) = long_as_ssize(v, b"* wants int") else {
                    return false;
                };
                if parsed > c_int::MAX as isize {
                    crate::tstate::set_string(
                        exceptions::overflow_error(),
                        b"Python int too large to convert to C int",
                    );
                    return false;
                }
                if parsed < 0 {
                    prec = 0;
                } else {
                    prec = parsed as c_int;
                }
                if remaining > 0 {
                    tychar = Some(fmt[*pos]);
                    *pos += 1;
                    remaining -= 1;
                } else {
                    tychar = None;
                }
            } else if matches!(tychar, Some(c) if c.is_ascii_digit()) {
                prec = (tychar.unwrap() - b'0') as c_int;
                while remaining > 0 {
                    let c = fmt[*pos];
                    *pos += 1;
                    remaining -= 1;
                    if !c.is_ascii_digit() {
                        tychar = Some(c);
                        break;
                    }
                    if prec > (c_int::MAX - (c - b'0') as c_int) / 10 {
                        crate::tstate::set_string(exceptions::value_error(), b"prec too big");
                        return false;
                    }
                    prec = prec * 10 + (c - b'0') as c_int;
                    tychar = None;
                }
            }
        }

        // Length modifiers are accepted and skipped.
        if matches!(tychar, Some(b'h' | b'l' | b'L')) {
            if remaining > 0 {
                tychar = Some(fmt[*pos]);
                *pos += 1;
            } else {
                tychar = None;
            }
        }
        let Some(c) = tychar else {
            crate::tstate::set_string(exceptions::value_error(), b"incomplete format");
            return false;
        };

        v = get_next_arg(args, arglen, argidx);
        if v.is_null() {
            return false;
        }

        // The formatted piece before padding, plus whether it carries a
        // sign CPython must manage.
        let piece: Vec<u8>;
        let mut sign_flag = false;
        let mut pbuf_offset = 0usize;
        match c {
            b'r' | b'a' => {
                let repr = crate::types::unicode::PyObject_Repr(v);
                if repr.is_null() {
                    return false;
                }
                let Some(view) = crate::types::unicode::as_chars(repr) else {
                    crate::types::unicode::decref(repr);
                    return false;
                };
                let mut text = view.as_vec();
                crate::types::unicode::decref(repr);
                if prec >= 0 && text.len() as c_int > prec {
                    text.truncate(prec as usize);
                }
                piece = text.iter().map(|&ch| ch as u8).collect();
            }
            b's' | b'b' => {
                let Some(obj) = format_obj(v) else {
                    return false;
                };
                let data = if bytearray_check(obj) {
                    bytearray_bytes(obj)
                } else {
                    bytes_of(obj)
                }
                .to_vec();
                crate::types::unicode::decref(obj);
                if prec >= 0 && data.len() as c_int > prec {
                    piece = data[..prec as usize].to_vec();
                } else {
                    piece = data;
                }
            }
            b'i' | b'd' | b'u' | b'o' | b'x' | b'X' => {
                let type_ = c;
                if crate::types::long::is_long(v) {
                    let res = crate::types::unicode::_PyUnicode_FormatLong(
                        v,
                        (flags & F_ALT) as c_int,
                        prec,
                        if type_ == b'i' { b'd' } else { type_ } as c_char,
                    );
                    if res.is_null() {
                        return false;
                    }
                    let Some(view) = crate::types::unicode::as_chars(res) else {
                        crate::types::unicode::decref(res);
                        return false;
                    };
                    piece = view.as_vec().iter().map(|&ch| ch as u8).collect();
                    crate::types::unicode::decref(res);
                } else {
                    let iobj = crate::types::unicode::number_long(v);
                    if iobj.is_null() {
                        crate::tstate::set_string(
                            exceptions::type_error(),
                            format!(
                                "%{} format: {} is required, not {}",
                                type_ as char,
                                if matches!(type_, b'o' | b'x' | b'X') {
                                    "an integer"
                                } else {
                                    "a real number"
                                },
                                name_of(v)
                            )
                            .as_bytes(),
                        );
                        return false;
                    }
                    let res = crate::types::unicode::_PyUnicode_FormatLong(
                        iobj,
                        (flags & F_ALT) as c_int,
                        prec,
                        if type_ == b'i' { b'd' } else { type_ } as c_char,
                    );
                    crate::types::unicode::decref(iobj);
                    if res.is_null() {
                        return false;
                    }
                    let Some(view) = crate::types::unicode::as_chars(res) else {
                        crate::types::unicode::decref(res);
                        return false;
                    };
                    piece = view.as_vec().iter().map(|&ch| ch as u8).collect();
                    crate::types::unicode::decref(res);
                }
                sign_flag = true;
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                let x = crate::types::float_::PyFloat_AsDouble(v);
                if !crate::tstate::occurred().is_null() {
                    crate::tstate::set_string(
                        exceptions::type_error(),
                        format!("float argument required, not {}", name_of(v)).as_bytes(),
                    );
                    return false;
                }
                let p = if prec < 0 { 6 } else { prec };
                let mut dtoa_flags: c_int = 0;
                if flags & F_ALT != 0 {
                    dtoa_flags |= 16; // Py_DTSF_ALT
                }
                let Some(text) = crate::types::float_::double_to_string(
                    x,
                    c,
                    p,
                    dtoa_flags,
                    core::ptr::null_mut(),
                ) else {
                    return false;
                };
                piece = text.into_bytes();
                sign_flag = true;
            }
            b'c' => {
                let Some(single) = byte_converter(v) else {
                    return false;
                };
                if width == -1 {
                    out.push(single);
                    return true;
                }
                piece = vec![single];
            }
            _ => {
                let display = if (31..=126).contains(&c) {
                    c as char
                } else {
                    '?'
                };
                crate::tstate::set_string(
                    exceptions::value_error(),
                    format!(
                        "unsupported format character '{}' (0x{:x}) at index {}",
                        display,
                        c,
                        *pos - 1
                    )
                    .as_bytes(),
                );
                return false;
            }
        }

        // `unicode_format_arg_output`: sign, zero fill, alignment.
        let mut len = piece.len() as isize;
        let fill = if sign_flag && flags & F_ZERO != 0 {
            b'0'
        } else {
            b' '
        };
        let mut sign_char = 0u8;
        if sign_flag {
            let first = piece[pbuf_offset];
            if first == b'-' || first == b'+' {
                sign_char = first;
                pbuf_offset += 1;
                len -= 1;
            } else if flags & F_SIGN != 0 {
                sign_char = b'+';
            } else if flags & F_BLANK != 0 {
                sign_char = b' ';
            } else {
                sign_char = 0;
            }
        }
        if width < len {
            width = len;
        }
        if sign_char != 0 {
            if fill != b' ' {
                out.push(sign_char);
            }
            if width > len {
                width -= 1;
            }
        }
        if flags & F_ALT != 0 && matches!(c, b'o' | b'x' | b'X') && piece.len() >= pbuf_offset + 2 {
            if fill != b' ' {
                out.push(piece[pbuf_offset]);
                out.push(piece[pbuf_offset + 1]);
                pbuf_offset += 2;
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
        if fill == b' ' {
            if sign_char != 0 {
                out.push(sign_char);
            }
            if flags & F_ALT != 0
                && matches!(c, b'o' | b'x' | b'X')
                && piece.len() >= pbuf_offset + 2
            {
                out.push(piece[pbuf_offset]);
                out.push(piece[pbuf_offset + 1]);
                pbuf_offset += 2;
            }
        }
        out.extend_from_slice(
            &piece[pbuf_offset..(pbuf_offset + len.max(0) as usize).min(piece.len())],
        );
        if width > len {
            out.extend(core::iter::repeat_n(b' ', (width - len) as usize));
        }
        true
    }
}

/// `PyLong_AsSsize_t` with a fixed TypeError on the non-int path.
///
/// # Safety
///
/// `v` must be a valid object.
unsafe fn long_as_ssize(v: *mut PyObject, message: &[u8]) -> Option<isize> {
    // SAFETY: the conversion validates its own operand.
    unsafe {
        if !crate::types::long::is_long(v) {
            crate::tstate::set_string(exceptions::type_error(), message);
            return None;
        }
        let value = crate::types::long::PyLong_AsLongLong(v);
        if !crate::tstate::occurred().is_null() {
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"Python int too large to convert to C ssize_t",
            );
            return None;
        }
        Some(value as isize)
    }
}

/// `format_obj`: bytes or bytearray only; `__bytes__` lookups need the
/// attribute machinery, and other bytes-like objects reach here once the
/// `PyObject_CheckBuffer` abstract layer lands.
///
/// # Safety
///
/// `v` must be a valid object.
unsafe fn format_obj(v: *mut PyObject) -> Option<*mut PyObject> {
    // SAFETY: the type checks read within the object.
    unsafe {
        if check(v) || bytearray_check(v) {
            incref(v);
            return Some(v);
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!(
                "%b requires a bytes-like object, or an object that implements __bytes__, not '{}'",
                name_of(v)
            )
            .as_bytes(),
        );
        None
    }
}

/// `byte_converter`: `%c`'s argument rules.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn byte_converter(arg: *mut PyObject) -> Option<u8> {
    // SAFETY: all reads are within checked objects.
    unsafe {
        if check(arg) && size(arg) == 1 {
            return Some(*as_string(arg) as u8);
        }
        if bytearray_check(arg) && bytearray_size(arg) == 1 {
            return Some(bytearray_as_string(arg) as *const u8).map(|p| *p);
        }
        if crate::types::long::is_long(arg) {
            let value = crate::types::long::PyLong_AsLongLong(arg);
            if !crate::tstate::occurred().is_null() {
                return None;
            }
            if !(0..=255).contains(&value) {
                crate::tstate::set_string(
                    exceptions::overflow_error(),
                    b"%c arg not in range(256)",
                );
                return None;
            }
            return Some(value as u8);
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            b"%c requires an integer in range(256) or a single byte",
        );
        None
    }
}

// -------------------------------------------------------- bytearray C API

/// CPython's `_PyByteArray_empty_string`: the shared terminator for empty
/// bytearrays.
#[unsafe(no_mangle)]
pub static mut _PyByteArray_empty_string: [c_char; 1] = [0];

/// `PyByteArray_AS_STRING`'s inline: an empty bytearray reads the shared
/// empty string.
///
/// # Safety
///
/// `op` must be a bytearray object.
pub(crate) unsafe fn bytearray_as_string(op: *const PyObject) -> *mut c_char {
    // SAFETY: the object is a bytearray.
    unsafe {
        if bytearray_size(op) == 0 {
            core::ptr::addr_of_mut!(_PyByteArray_empty_string[0])
        } else {
            (*(op as *const PyByteArrayObject)).ob_start
        }
    }
}

/// Build a bytearray over `bytes`; `None` is the already-raised case.
pub(crate) fn bytearray_from_vec(bytes: Option<Vec<u8>>) -> *mut PyObject {
    match bytes {
        None => core::ptr::null_mut(),
        Some(data) => {
            let len = data.len();
            let slice = data.leak();
            // SAFETY: the buffer holds the leaked slice's bytes.
            unsafe { PyByteArray_FromStringAndSize(slice.as_ptr().cast(), len as isize) }
        }
    }
}

/// `PyByteArray_FromStringAndSize`.
///
/// # Safety
///
/// `bytes` must hold `size` bytes or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_FromStringAndSize(
    bytes: *const c_char,
    size: isize,
) -> *mut PyObject {
    // SAFETY: allocation and writes land within the fresh object.
    unsafe {
        if size < 0 {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Negative size passed to PyByteArray_FromStringAndSize",
            );
            return core::ptr::null_mut();
        }
        let block = crate::alloc::alloc_object(core::mem::size_of::<PyByteArrayObject>());
        if block.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        header::c_init(block.cast(), &raw mut PyByteArray_Type);
        let object = block as *mut PyByteArrayObject;
        if size == 0 {
            (*object).ob_bytes = core::ptr::null_mut();
        } else {
            let buffer = crate::alloc::alloc_block(size as usize + 1);
            if buffer.is_null() {
                crate::alloc::free_block(block);
                crate::tstate::set_string(exceptions::memory_error(), b"");
                return core::ptr::null_mut();
            }
            if !bytes.is_null() && size > 0 {
                core::ptr::copy_nonoverlapping(bytes, buffer.cast(), size as usize);
            }
            *buffer.add(size as usize) = 0;
            (*object).ob_bytes = buffer.cast();
        }
        (*(block as *mut coil_capi_sys::PyVarObject)).ob_size = size;
        (*object).ob_alloc = if size == 0 { 0 } else { size + 1 };
        (*object).ob_start = (*object).ob_bytes;
        (*object).ob_exports = 0;
        block.cast()
    }
}

/// `PyByteArray_FromObject`: an exact object is shared, a bytearray copy;
/// anything else must offer iteration of small ints.
///
/// # Safety
///
/// `input` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_FromObject(input: *mut PyObject) -> *mut PyObject {
    // SAFETY: all reads are within valid objects.
    unsafe {
        if input.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        if bytearray_check_exact(input) {
            incref(input);
            return input;
        }
        if bytearray_check(input) {
            let data = bytearray_bytes(input).to_vec();
            return bytearray_from_vec(Some(data));
        }
        let ty = header::type_of(input);
        if (*ty).tp_iter.is_some() {
            let iterator = ((*ty).tp_iter.unwrap())(input);
            if iterator.is_null() {
                return core::ptr::null_mut();
            }
            let mut out: Vec<u8> = Vec::new();
            while let Some(item) = iter_next_item(iterator) {
                let value = long_as_byte(item);
                decref(item);
                let Some(byte) = value else {
                    decref(iterator);
                    return core::ptr::null_mut();
                };
                out.push(byte);
            }
            decref(iterator);
            if !crate::tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return bytearray_from_vec(Some(out));
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("cannot convert '{}' object to bytearray", name_of(input)).as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// `PyByteArray_Size`.
///
/// # Safety
///
/// `self_` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Size(self_: *mut PyObject) -> isize {
    // SAFETY: the object is checked before the read.
    unsafe {
        if self_.is_null() || !bytearray_check(self_) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return -1;
        }
        bytearray_size(self_)
    }
}

/// `PyByteArray_AsString`.
///
/// # Safety
///
/// `self_` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_AsString(self_: *mut PyObject) -> *mut c_char {
    // SAFETY: the object is checked before the read.
    unsafe {
        if self_.is_null() || !bytearray_check(self_) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        bytearray_as_string(self_)
    }
}

/// `_canresize`: exports pin the buffer.
///
/// # Safety
///
/// `obj` must be a bytearray object.
unsafe fn canresize(obj: *mut PyByteArrayObject) -> bool {
    // SAFETY: the read is within the object.
    unsafe {
        if (*obj).ob_exports > 0 {
            crate::tstate::set_string(
                exceptions::buffer_error(),
                b"Existing exports of data: object cannot be re-sized",
            );
            return false;
        }
        true
    }
}

/// `PyByteArray_Resize`.
///
/// # Safety
///
/// `self_` must be a bytearray object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Resize(self_: *mut PyObject, requested_size: isize) -> c_int {
    // SAFETY: the object is a bytearray; the allocation math mirrors
    // CPython's.
    unsafe {
        let object = self_ as *mut PyByteArrayObject;
        if requested_size < 0 {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Negative size passed to PyByteArray_Resize",
            );
            return -1;
        }
        if requested_size == (*object).ob_size {
            return 0;
        }
        if !canresize(object) {
            return -1;
        }
        let mut alloc: isize = (*object).ob_alloc;
        let size = requested_size;
        if size < alloc {
            if size < alloc / 2 {
                alloc = size + 1;
            } else {
                (*object).ob_size = size;
                *(*object).ob_start.add(size as usize) = 0;
                return 0;
            }
        } else if size <= alloc + (alloc >> 3) {
            alloc = size + (size >> 3) + (if size < 9 { 3 } else { 6 });
        } else {
            alloc = size + 1;
        }
        if (*object).ob_bytes.is_null() {
            // Growing an empty (alloc 0) bytearray.
            let buffer = crate::alloc::alloc_block(alloc as usize);
            if buffer.is_null() {
                crate::tstate::set_string(exceptions::memory_error(), b"");
                return -1;
            }
            *buffer.add(size as usize) = 0;
            (*object).ob_bytes = buffer.cast();
            (*object).ob_start = (*object).ob_bytes;
            (*object).ob_alloc = alloc;
            (*object).ob_size = size;
            return 0;
        }
        let grown = crate::alloc::realloc_block((*object).ob_bytes.cast::<u8>(), alloc as usize);
        if grown.is_null() {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return -1;
        }
        (*object).ob_bytes = grown.cast();
        (*object).ob_start = (*object).ob_bytes;
        (*object).ob_alloc = alloc;
        (*object).ob_size = size;
        *(*object).ob_start.add(size as usize) = 0;
        0
    }
}

/// `PyByteArray_Concat`: both operands through the buffer protocol.
///
/// # Safety
///
/// Both arguments must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: both operands are valid objects.
    unsafe {
        let Some(va) = get_buffer(a) else {
            return core::ptr::null_mut();
        };
        let Some(vb) = get_buffer(b) else {
            release_buffer(va);
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("can't concat {} to {}", name_of(b), name_of(a)).as_bytes(),
            );
            return core::ptr::null_mut();
        };
        let total = match va.len.checked_add(vb.len) {
            Some(total) => total,
            None => {
                release_buffer(va);
                release_buffer(vb);
                crate::tstate::set_string(
                    exceptions::overflow_error(),
                    b"joined bytearray is too long",
                );
                return core::ptr::null_mut();
            }
        };
        let result = PyByteArray_FromStringAndSize(core::ptr::null(), total);
        if !result.is_null() {
            let target = (*(result as *mut PyByteArrayObject)).ob_start;
            core::ptr::copy_nonoverlapping(va.buf.cast::<c_char>(), target, va.len as usize);
            core::ptr::copy_nonoverlapping(
                vb.buf.cast::<c_char>(),
                target.add(va.len as usize),
                vb.len as usize,
            );
        }
        release_buffer(va);
        release_buffer(vb);
        result
    }
}

/// `bytearray_concat` slot wrapper over the export.
unsafe extern "C" fn bytearray_concat_slot(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe { PyByteArray_Concat(a, b) }
}

/// `bytearray_length` slot.
unsafe extern "C" fn bytearray_length_slot(self_: *mut PyObject) -> isize {
    // SAFETY: the object is a bytearray.
    unsafe { bytearray_size(self_) }
}

/// `bytearray_repeat` slot: `n` copies.
unsafe extern "C" fn bytearray_repeat(self_: *mut PyObject, count: isize) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe {
        let count = count.max(0);
        let mysize = bytearray_size(self_);
        if count > 0 && mysize > isize::MAX / count {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        let size = mysize * count;
        let result = PyByteArray_FromStringAndSize(core::ptr::null(), size);
        if !result.is_null() && size != 0 {
            _PyBytes_Repeat(
                (*(result as *mut PyByteArrayObject)).ob_bytes,
                size,
                bytearray_as_string(self_),
                mysize,
            );
        }
        result
    }
}

/// `bytearray_iconcat`: extend in place.
unsafe extern "C" fn bytearray_iconcat(
    self_: *mut PyObject,
    other: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe {
        let Some(vo) = get_buffer(other) else {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("can't concat {} to {}", name_of(other), name_of(self_)).as_bytes(),
            );
            return core::ptr::null_mut();
        };
        let size = bytearray_size(self_);
        if size > isize::MAX - vo.len {
            release_buffer(vo);
            crate::tstate::set_string(exceptions::overflow_error(), b"");
            return core::ptr::null_mut();
        }
        let result = if PyByteArray_Resize(self_, size + vo.len) == 0 {
            core::ptr::copy_nonoverlapping(
                vo.buf,
                bytearray_as_string(self_).add(size as usize).cast(),
                vo.len as usize,
            );
            incref(self_);
            self_
        } else {
            core::ptr::null_mut()
        };
        release_buffer(vo);
        result
    }
}

/// `bytearray_irepeat`: repeat in place.
unsafe extern "C" fn bytearray_irepeat(self_: *mut PyObject, count: isize) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe {
        if count == 1 {
            incref(self_);
            return self_;
        }
        let count = count.max(0);
        let mysize = bytearray_size(self_);
        if count > 0 && mysize > isize::MAX / count {
            crate::tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        let size = mysize * count;
        if PyByteArray_Resize(self_, size) < 0 {
            return core::ptr::null_mut();
        }
        if size > 0 {
            _PyBytes_Repeat(
                (*(self_ as *mut PyByteArrayObject)).ob_start,
                size,
                bytearray_as_string(self_),
                mysize,
            );
        }
        incref(self_);
        self_
    }
}

/// `bytearray_getitem` slot.
unsafe extern "C" fn bytearray_getitem(self_: *mut PyObject, index: isize) -> *mut PyObject {
    // SAFETY: the index is checked before the read.
    unsafe {
        if index < 0 || index >= bytearray_size(self_) {
            crate::tstate::set_string(exceptions::index_error(), b"bytearray index out of range");
            return core::ptr::null_mut();
        }
        let b = *bytearray_as_string(self_).add(index as usize) as u8;
        crate::types::long::PyLong_FromUnsignedLong(b as _)
    }
}

/// `bytearray_setitem` slot (`sq_ass_item`): replace or delete one byte.
unsafe extern "C" fn bytearray_setitem(
    self_: *mut PyObject,
    index: isize,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the object is a bytearray; the value is checked first.
    unsafe {
        let mut ival: i32 = -1;
        if !value.is_null() {
            let Some(parsed) = getbytevalue(value) else {
                return -1;
            };
            ival = parsed;
        }
        let mut index = index;
        if index < 0 {
            index += bytearray_size(self_);
        }
        if index < 0 || index >= bytearray_size(self_) {
            crate::tstate::set_string(exceptions::index_error(), b"bytearray index out of range");
            return -1;
        }
        if value.is_null() {
            // `del ba[i]`: shift the tail down one place.
            let size = bytearray_size(self_);
            let start = bytearray_as_string(self_).add(index as usize);
            core::ptr::copy(start.add(1), start, (size - index - 1) as usize);
            return PyByteArray_Resize(self_, size - 1);
        }
        *bytearray_as_string(self_).add(index as usize) = ival as c_char;
        0
    }
}

/// `_getbytevalue`: an exact small int in range, else the error.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn getbytevalue(arg: *mut PyObject) -> Option<i32> {
    // SAFETY: the int conversion validates its own operand.
    unsafe {
        if !crate::types::long::is_long(arg) {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!(
                    "'{}' object cannot be interpreted as an integer",
                    name_of(arg)
                )
                .as_bytes(),
            );
            return None;
        }
        let value = crate::types::long::PyLong_AsLongLong(arg);
        if !crate::tstate::occurred().is_null() {
            return None;
        }
        if !(0..256).contains(&value) {
            crate::tstate::set_string(exceptions::value_error(), b"byte must be in range(0, 256)");
            return None;
        }
        Some(value as i32)
    }
}

/// `bytearray_contains` slot.
unsafe extern "C" fn bytearray_contains(self_: *mut PyObject, arg: *mut PyObject) -> c_int {
    // SAFETY: both are valid objects.
    unsafe { bytes_like_contains(bytearray_bytes(self_), arg) }
}

/// `_Py_bytes_contains` over raw bytes.
///
/// # Safety
///
/// `arg` must be a valid object.
pub(crate) unsafe fn bytes_like_contains(hay: &[u8], arg: *mut PyObject) -> c_int {
    // SAFETY: all reads are within checked objects.
    unsafe {
        if check(arg) {
            let needle = bytes_of(arg);
            if needle.is_empty() {
                return 1;
            }
            return memfind(hay, needle, 0).is_some() as c_int;
        }
        if bytearray_check(arg) {
            let needle = bytearray_bytes(arg);
            if needle.is_empty() {
                return 1;
            }
            return memfind(hay, needle, 0).is_some() as c_int;
        }
        if crate::types::long::is_long(arg) {
            let value = crate::types::long::PyLong_AsLongLong(arg);
            if !crate::tstate::occurred().is_null() {
                return -1;
            }
            if !(0..256).contains(&value) {
                crate::tstate::set_string(
                    exceptions::value_error(),
                    b"byte must be in range(0, 256)",
                );
                return -1;
            }
            return hay.contains(&(value as u8)) as c_int;
        }
        crate::tstate::set_string(
            exceptions::type_error(),
            format!(
                "argument should be integer or bytes-like object, not '{}'",
                name_of(arg)
            )
            .as_bytes(),
        );
        -1
    }
}

/// `bytes_contains` slot.
unsafe extern "C" fn bytes_contains(self_: *mut PyObject, arg: *mut PyObject) -> c_int {
    // SAFETY: both are valid objects.
    unsafe { bytes_like_contains(bytes_of(self_), arg) }
}

/// `bytes_length` slot.
unsafe extern "C" fn bytes_length(self_: *mut PyObject) -> isize {
    // SAFETY: the object is bytes.
    unsafe { size(self_) }
}

/// `bytes_concat` slot wrapper.
unsafe extern "C" fn bytes_concat_slot(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe { concat(a, b) }
}

/// `bytes_repeat` slot: `n` copies.
unsafe extern "C" fn bytes_repeat(self_: *mut PyObject, count: isize) -> *mut PyObject {
    // SAFETY: the object is bytes.
    unsafe {
        let count = count.max(0);
        let mysize = size(self_);
        if count > 0 && mysize > isize::MAX / count {
            crate::tstate::set_string(exceptions::overflow_error(), b"repeated bytes are too long");
            return core::ptr::null_mut();
        }
        let total = mysize * count;
        let result = from_parts(None, total);
        if let Some(object) = result {
            _PyBytes_Repeat(as_string(object), total, as_string(self_), mysize);
        }
        result.unwrap_or(core::ptr::null_mut())
    }
}

/// `bytes_item` slot.
unsafe extern "C" fn bytes_item(self_: *mut PyObject, index: isize) -> *mut PyObject {
    // SAFETY: the index is checked before the read.
    unsafe {
        let mut index = index;
        if index < 0 {
            index += size(self_);
        }
        if index < 0 || index >= size(self_) {
            crate::tstate::set_string(exceptions::index_error(), b"index out of range");
            return core::ptr::null_mut();
        }
        crate::types::long::PyLong_FromLong(*as_string(self_).add(index as usize) as c_long)
    }
}

/// `bytes_subscript` slot: integer indexes only (slices need the slice
/// type, which arrives with the container runtime).
///
/// # Safety
///
/// Both arguments must be valid objects.
pub(crate) unsafe fn bytes_subscript(self_: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    // SAFETY: all reads are within valid objects.
    unsafe {
        let Some(index) = crate::types::unicode::long_index(key) else {
            if crate::tstate::occurred().is_null() {
                crate::tstate::set_string(
                    exceptions::type_error(),
                    format!("byte indices must be integers, not {}", name_of(key)).as_bytes(),
                );
            }
            return core::ptr::null_mut();
        };
        let length = size(self_);
        let index = if index < 0 { length + index } else { index };
        if index < 0 || index >= length {
            crate::tstate::set_string(exceptions::index_error(), b"index out of range");
            return core::ptr::null_mut();
        }
        crate::types::long::PyLong_FromLong(*as_string(self_).add(index as usize) as c_long)
    }
}

/// The `bytes` mapping-subscript slot wrapper.
unsafe extern "C" fn bytes_subscript_slot(
    self_: *mut PyObject,
    key: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe { bytes_subscript(self_, key) }
}

/// `bytearray_subscript` slot.
unsafe extern "C" fn bytearray_subscript(
    self_: *mut PyObject,
    key: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: all reads are within valid objects.
    unsafe {
        let Some(index) = crate::types::unicode::long_index(key) else {
            if crate::tstate::occurred().is_null() {
                crate::tstate::set_string(
                    exceptions::type_error(),
                    format!("bytearray indices must be integers, not {}", name_of(key)).as_bytes(),
                );
            }
            return core::ptr::null_mut();
        };
        let length = bytearray_size(self_);
        let index = if index < 0 { length + index } else { index };
        if index < 0 || index >= length {
            crate::tstate::set_string(exceptions::index_error(), b"bytearray index out of range");
            return core::ptr::null_mut();
        }
        let b = *bytearray_as_string(self_).add(index as usize) as u8;
        crate::types::long::PyLong_FromUnsignedLong(b as _)
    }
}

/// `bytearray_ass_subscript` slot (`mp_ass_subscript`).
unsafe extern "C" fn bytearray_ass_subscript(
    self_: *mut PyObject,
    key: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: all three are valid objects (value may be null for delete).
    unsafe {
        let Some(index) = crate::types::unicode::long_index(key) else {
            if crate::tstate::occurred().is_null() {
                crate::tstate::set_string(
                    exceptions::type_error(),
                    format!("bytearray indices must be integers, not {}", name_of(key)).as_bytes(),
                );
            }
            return -1;
        };
        bytearray_setitem(self_, index, value)
    }
}

/// `bytearray_mod` slot: `bytearray % obj`.
unsafe extern "C" fn bytearray_mod(self_: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe {
        if !bytearray_check(self_) {
            return singletons::not_implemented();
        }
        _PyBytes_FormatEx(bytearray_as_string(self_), bytearray_size(self_), arg, 1)
    }
}

/// `bytes_mod` slot: `bytes % obj`.
unsafe extern "C" fn bytes_mod(self_: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is bytes.
    unsafe {
        if !check(self_) {
            return singletons::not_implemented();
        }
        _PyBytes_FormatEx(as_string(self_), size(self_), arg, 0)
    }
}

/// `bytearray_repr` slot: `bytearray(b'...')`.
unsafe extern "C" fn bytearray_repr(self_: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe {
        let class_name = name_of(self_);
        let bytes = bytearray_bytes(self_);
        let mut out = Vec::with_capacity(class_name.len() + 3 + bytes.len() * 4);
        out.extend_from_slice(class_name.as_bytes());
        out.extend_from_slice(b"(b");
        out.extend_from_slice(&repr_bytes(bytes, true)[1..]);
        out.push(b')');
        match crate::types::unicode::PyUnicode_FromStringAndSize(
            out.as_ptr().cast(),
            out.len() as isize,
        ) {
            object if !object.is_null() => object,
            _ => core::ptr::null_mut(),
        }
    }
}

/// `bytearray_str` slot: the `BytesWarning` runtime flag is a
/// warnings-task item, so this is the repr.
unsafe extern "C" fn bytearray_str(self_: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe { bytearray_repr(self_) }
}

/// `bytearray_dealloc` slot.
unsafe extern "C" fn bytearray_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from this module's allocator.
    unsafe {
        let state = object as *mut PyByteArrayObject;
        if (*state).ob_exports > 0 {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"deallocated bytearray object has exported buffers",
            );
        }
        if !(*state).ob_bytes.is_null() {
            crate::alloc::free_block((*state).ob_bytes.cast());
        }
        crate::alloc::free_block(object.cast());
    }
}

/// `bytes_dealloc` slot. (The 3.12.10 reference tree leaves
/// `PyBytes_Type.tp_dealloc` null and relies on the interpreter's free;
/// heap objects must be released, as 3.12.15's build does.)
unsafe extern "C" fn bytes_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from this module's allocator.
    unsafe {
        crate::alloc::free_block(object.cast());
    }
}

// -------------------------------------------------------- richcompare slot

/// `bytes_richcompare` slot: non-bytes operands are `NotImplemented`
/// (matching CPython; the `BytesWarning` runtime flag is the warnings
/// task's).
unsafe extern "C" fn bytes_richcompare_slot(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: both are valid objects.
    unsafe {
        if !check(a) || !check(b) {
            return singletons::not_implemented();
        }
        if core::ptr::eq(a, b) {
            return match op {
                CMP_EQ | Py_LE | Py_GE => singletons::true_(),
                CMP_NE | Py_LT | Py_GT => singletons::false_(),
                _ => {
                    crate::tstate::set_string(exceptions::system_error(), b"");
                    return core::ptr::null_mut();
                }
            };
        }
        richcompare(a, b, op)
    }
}

/// `bytes_hash` slot.
unsafe extern "C" fn bytes_hash_slot(self_: *mut PyObject) -> isize {
    // SAFETY: the object is a valid bytes object.
    unsafe { hash(self_) }
}

// -------------------------------------------------------- buffer protocol

/// `PyBuffer_FillInfo`: fill a 1-D unsigned-byte view.
///
/// # Safety
///
/// `view` must be a writable view struct; `buf` must hold `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FillInfo(
    view: *mut Py_buffer,
    obj: *mut PyObject,
    buf: *mut c_void,
    len: isize,
    readonly: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: the view is a valid struct per the contract.
    unsafe {
        if view.is_null() {
            crate::tstate::set_string(
                exceptions::buffer_error(),
                b"PyBuffer_FillInfo: view==NULL argument is obsolete",
            );
            return -1;
        }
        if flags & coil_capi_sys::PyBUF_WRITABLE == coil_capi_sys::PyBUF_WRITABLE && readonly == 1 {
            crate::tstate::set_string(exceptions::buffer_error(), b"Object is not writable.");
            return -1;
        }
        (*view).obj = if obj.is_null() {
            core::ptr::null_mut()
        } else {
            incref(obj);
            obj
        };
        (*view).buf = buf;
        (*view).len = len;
        (*view).readonly = readonly;
        (*view).itemsize = 1;
        (*view).format = core::ptr::null_mut();
        if flags & coil_capi_sys::PyBUF_FORMAT == coil_capi_sys::PyBUF_FORMAT {
            (*view).format = c"B".as_ptr().cast_mut();
        }
        (*view).ndim = 1;
        (*view).shape = core::ptr::null_mut();
        if flags & coil_capi_sys::PyBUF_ND == coil_capi_sys::PyBUF_ND {
            (*view).shape = core::ptr::addr_of_mut!((*view).len);
        }
        (*view).strides = core::ptr::null_mut();
        if flags & coil_capi_sys::PyBUF_STRIDES == coil_capi_sys::PyBUF_STRIDES {
            (*view).strides = core::ptr::addr_of_mut!((*view).itemsize);
        }
        (*view).suboffsets = core::ptr::null_mut();
        (*view).internal = core::ptr::null_mut();
        0
    }
}

/// `PyBuffer_Release`.
///
/// # Safety
///
/// `view` must be a previously filled view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    // SAFETY: the view is a valid struct per the contract.
    unsafe {
        let obj = (*view).obj;
        if obj.is_null() {
            return;
        }
        let procs = (*(header::type_of(obj))).tp_as_buffer;
        if !procs.is_null()
            && let Some(release) = (*procs).bf_releasebuffer
        {
            release(obj, view);
        }
        (*view).obj = core::ptr::null_mut();
        decref(obj);
    }
}

/// `PyObject_GetBuffer`: walk `tp_as_buffer` and fill the view.
///
/// # Safety
///
/// `obj` must be a valid object; `view` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetBuffer(
    obj: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    // SAFETY: the type table read is within the object's type.
    unsafe {
        let procs = (*(header::type_of(obj))).tp_as_buffer;
        if procs.is_null() || (*procs).bf_getbuffer.is_none() {
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("a bytes-like object is required, not '{}'", name_of(obj)).as_bytes(),
            );
            return -1;
        }
        ((*procs).bf_getbuffer.unwrap())(obj, view, flags)
    }
}

/// The `get_buffer` half of `PyByteArray_Concat` and friends: a filled
/// view, or `None` with the error set.
///
/// # Safety
///
/// `obj` must be a valid object.
pub(crate) unsafe fn get_buffer(obj: *mut PyObject) -> Option<Py_buffer> {
    // SAFETY: the view is local; release happens at the caller.
    unsafe {
        let mut view: Py_buffer = core::mem::zeroed();
        if PyObject_GetBuffer(obj, &mut view, coil_capi_sys::PyBUF_SIMPLE) < 0 {
            return None;
        }
        Some(view)
    }
}

/// Release a view taken by `get_buffer`.
///
/// # Safety
///
/// `view` must be a previously filled view.
pub(crate) unsafe fn release_buffer(view: Py_buffer) {
    // SAFETY: the view was filled by get_buffer.
    unsafe { PyBuffer_Release(&view as *const Py_buffer as *mut Py_buffer) }
}

/// `bytes_buffer_getbuffer`.
unsafe extern "C" fn bytes_buffer_getbuffer(
    self_: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    // SAFETY: the object is bytes; the view is valid.
    unsafe {
        if view.is_null() {
            crate::tstate::set_string(
                exceptions::buffer_error(),
                b"bytes_buffer_getbuffer: view==NULL argument is obsolete",
            );
            return -1;
        }
        PyBuffer_FillInfo(view, self_, as_string(self_).cast(), size(self_), 1, flags)
    }
}

/// `bytearray_getbuffer`.
unsafe extern "C" fn bytearray_getbuffer(
    self_: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    // SAFETY: the object is a bytearray; the view is valid.
    unsafe {
        if view.is_null() {
            crate::tstate::set_string(
                exceptions::buffer_error(),
                b"bytearray_getbuffer: view==NULL argument is obsolete",
            );
            return -1;
        }
        let result = PyBuffer_FillInfo(
            view,
            self_,
            bytearray_as_string(self_).cast(),
            bytearray_size(self_),
            0,
            flags,
        );
        if result == 0 {
            (*(self_ as *mut PyByteArrayObject)).ob_exports += 1;
        }
        result
    }
}

/// `bytearray_releasebuffer`.
unsafe extern "C" fn bytearray_releasebuffer(self_: *mut PyObject, _view: *mut Py_buffer) {
    // SAFETY: the object is a bytearray with a live export.
    unsafe {
        (*(self_ as *mut PyByteArrayObject)).ob_exports -= 1;
    }
}

static mut BYTES_BUFFER_PROCS: coil_capi_sys::PyBufferProcs = coil_capi_sys::PyBufferProcs {
    bf_getbuffer: Some(bytes_buffer_getbuffer),
    bf_releasebuffer: None,
};

static mut BYTEARRAY_BUFFER_PROCS: coil_capi_sys::PyBufferProcs = coil_capi_sys::PyBufferProcs {
    bf_getbuffer: Some(bytearray_getbuffer),
    bf_releasebuffer: Some(bytearray_releasebuffer),
};

// ------------------------------------------------------------ type objects

/// A `PyNumberMethods` table whose only live slot is `nb_remainder`
/// (bytes' `%` operator).
const fn zero_number(
    remainder: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
) -> coil_capi_sys::PyNumberMethods {
    coil_capi_sys::PyNumberMethods {
        nb_add: None,
        nb_subtract: None,
        nb_multiply: None,
        nb_remainder: remainder,
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
    }
}

static mut BYTES_NUMBER: coil_capi_sys::PyNumberMethods = zero_number(Some(bytes_mod));

static mut BYTES_SEQUENCE: coil_capi_sys::PySequenceMethods = coil_capi_sys::PySequenceMethods {
    sq_length: Some(bytes_length),
    sq_concat: Some(bytes_concat_slot),
    sq_repeat: Some(bytes_repeat),
    sq_item: Some(bytes_item),
    was_sq_slice: core::ptr::null_mut(),
    sq_ass_item: None,
    was_sq_ass_slice: core::ptr::null_mut(),
    sq_contains: Some(bytes_contains),
    sq_inplace_concat: None,
    sq_inplace_repeat: None,
};

static mut BYTES_MAPPING: coil_capi_sys::PyMappingMethods = coil_capi_sys::PyMappingMethods {
    mp_length: Some(bytes_length),
    mp_subscript: Some(bytes_subscript_slot),
    mp_ass_subscript: None,
};

const fn bytes_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"bytes".as_ptr();
    t.tp_basicsize = (bytes_layout::OB_SVAL + OB_SVAL_TERMINATOR) as isize;
    t.tp_itemsize = 1;
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_BYTES_SUBCLASS
        | PY_TPFLAGS_MATCH_SELF;
    t.tp_dealloc = Some(bytes_dealloc);
    t.tp_repr = Some(bytes_repr_slot);
    t.tp_as_number = &raw mut BYTES_NUMBER;
    t.tp_as_sequence = &raw mut BYTES_SEQUENCE;
    t.tp_as_mapping = &raw mut BYTES_MAPPING;
    t.tp_hash = Some(bytes_hash_slot);
    t.tp_str = Some(bytes_str);
    t.tp_as_buffer = &raw mut BYTES_BUFFER_PROCS;
    t.tp_richcompare = Some(bytes_richcompare_slot);
    t.tp_iter = Some(bytes_iter);
    t
}

/// CPython's `PyBytes_Type`.
#[unsafe(no_mangle)]
pub static mut PyBytes_Type: PyTypeObject = bytes_type();

static mut BYTEARRAY_NUMBER: coil_capi_sys::PyNumberMethods = zero_number(Some(bytearray_mod));

static mut BYTEARRAY_SEQUENCE: coil_capi_sys::PySequenceMethods =
    coil_capi_sys::PySequenceMethods {
        sq_length: Some(bytearray_length_slot),
        sq_concat: Some(bytearray_concat_slot),
        sq_repeat: Some(bytearray_repeat),
        sq_item: Some(bytearray_getitem),
        was_sq_slice: core::ptr::null_mut(),
        sq_ass_item: Some(bytearray_setitem),
        was_sq_ass_slice: core::ptr::null_mut(),
        sq_contains: Some(bytearray_contains),
        sq_inplace_concat: Some(bytearray_iconcat),
        sq_inplace_repeat: Some(bytearray_irepeat),
    };

static mut BYTEARRAY_MAPPING: coil_capi_sys::PyMappingMethods = coil_capi_sys::PyMappingMethods {
    mp_length: Some(bytearray_length_slot),
    mp_subscript: Some(bytearray_subscript),
    mp_ass_subscript: Some(bytearray_ass_subscript),
};

const fn bytearray_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"bytearray".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyByteArrayObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | PY_TPFLAGS_MATCH_SELF;
    t.tp_dealloc = Some(bytearray_dealloc);
    t.tp_repr = Some(bytearray_repr);
    t.tp_as_number = &raw mut BYTEARRAY_NUMBER;
    t.tp_as_sequence = &raw mut BYTEARRAY_SEQUENCE;
    t.tp_as_mapping = &raw mut BYTEARRAY_MAPPING;
    t.tp_as_buffer = &raw mut BYTEARRAY_BUFFER_PROCS;
    t.tp_str = Some(bytearray_str);
    t.tp_richcompare = Some(bytes_richcompare_slot);
    t.tp_iter = Some(bytearray_iter);
    t
}

/// CPython's `PyByteArray_Type`.
#[unsafe(no_mangle)]
pub static mut PyByteArray_Type: PyTypeObject = bytearray_type();

/// `PyByteArrayIter_Type` (shares the bytes iterator state layout).
const fn bytearray_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"bytearray_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyBytesIterObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT;
    t.tp_dealloc = Some(bytes_iter_dealloc);
    t.tp_iternext = Some(bytearray_iter_next);
    t
}

/// CPython's `PyByteArrayIter_Type`.
#[unsafe(no_mangle)]
pub static mut PyByteArrayIter_Type: PyTypeObject = bytearray_iter_type();

/// `bytes_iterator`, not exported (CPython keeps it internal to the
/// type's `tp_iter`).
const fn bytes_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"bytes_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyBytesIterObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT;
    t.tp_dealloc = Some(bytes_iter_dealloc);
    t.tp_iternext = Some(bytes_iter_next);
    t
}

/// CPython's `PyBytesIter_Type`.
#[unsafe(no_mangle)]
pub static mut PyBytesIter_Type: PyTypeObject = bytes_iter_type();

/// `bytes_iter` slot.
unsafe extern "C" fn bytes_iter(seq: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is bytes or bytearray.
    unsafe {
        if !check(seq) && !bytearray_check(seq) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        iter(seq)
    }
}

/// `bytearray_iter` slot.
unsafe extern "C" fn bytearray_iter(seq: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a bytearray.
    unsafe {
        if !bytearray_check(seq) {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        let object = crate::alloc::alloc_object(core::mem::size_of::<PyBytesIterObject>());
        if object.is_null() {
            return object.cast();
        }
        header::c_init(object.cast(), &raw mut PyByteArrayIter_Type);
        let state = object as *mut PyBytesIterObject;
        (*state).it_index = 0;
        (*state).it_seq = seq;
        incref(seq);
        object.cast()
    }
}

//! `binascii`: CPython 3.12's `binascii.c` — the uuencode, base64,
//! hex and quoted-printable codecs plus the CRC-CCITT and CRC-32
//! checksums, with CPython's exact `binascii.Error` messages. Arguments
//! are bytes (the `ascii_buffer`/`Py_buffer` converters' only
//! buffer-capable type until more types grow buffer support), and the
//! keyword-only flags `backtick`, `strict_mode` and `newline` run at
//! their defaults for want of module objects, the spec 10.6 unicode
//! seam of module `mod.rs`.

use core::ffi::c_int;

use coil_capi_sys::{
    _PyCFunctionFast, _PyCFunctionFastWithKeywords, METH_FASTCALL, METH_KEYWORDS, METH_O,
    PyCFunction, PyMethodDef, PyObject, PyTypeObject,
};

use crate::modules::{
    keywords_arity_error, missing_argument_error, number_index, positional_error,
    positional_error_kw,
};
use crate::object::header;
use crate::object::typeobject;
use crate::tstate;
use crate::types::bytes;
use crate::types::exc as exceptions;
use crate::types::long;

/// `binascii.Error`, the module's `ValueError` subtype; the base is
/// linked in `init`, when the exception hierarchy exists.
static mut ERROR_TYPE: PyTypeObject =
    exceptions::module_exception_type(c"binascii.Error".as_ptr(), core::ptr::null_mut());

/// The `binascii.Error` type.
pub fn error() -> *mut PyObject {
    &raw mut ERROR_TYPE as *mut PyObject
}

/// Ready the module's exception type; `init_static_types` calls this
/// after the built-in exception hierarchy is readied.
pub(crate) fn init() -> c_int {
    // SAFETY: init runs once before any concurrent access.
    unsafe {
        ERROR_TYPE.tp_base = exceptions::value_error() as *mut PyTypeObject;
        if typeobject::PyType_Ready(&raw mut ERROR_TYPE) < 0 {
            return -1;
        }
    }
    0
}

fn error_message(message: &[u8]) {
    tstate::set_string(error(), message);
}

/// A bytes argument: the `ascii_buffer`/`Py_buffer` converters accept
/// bytes, other buffers and ASCII strings; only bytes exist yet.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn bytes_arg(arg: *mut PyObject) -> Option<&'static [u8]> {
    // SAFETY: the type check precedes the payload read.
    unsafe {
        if bytes::check_exact(arg) {
            Some(bytes::bytes_of(arg))
        } else {
            let name = crate::util::cstr((*header::type_of(arg)).tp_name);
            tstate::set_string(
                exceptions::type_error(),
                format!("argument should be bytes, buffer or ASCII string, not '{name}'")
                    .as_bytes(),
            );
            None
        }
    }
}

/// The plain `Py_buffer` converter family of `crc_hqx`/`crc32`
/// (getargs' bytes-like message, not binascii's own converter).
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn byteslike_arg(arg: *mut PyObject) -> Option<&'static [u8]> {
    // SAFETY: the type check precedes the payload read.
    unsafe {
        if bytes::check_exact(arg) {
            Some(bytes::bytes_of(arg))
        } else {
            let name = crate::util::cstr((*header::type_of(arg)).tp_name);
            tstate::set_string(
                exceptions::type_error(),
                format!("a bytes-like object is required, not '{name}'").as_bytes(),
            );
            None
        }
    }
}

/// `_PyLong_DigitValue`: digit values below 36, `0xff` above.
fn digit_value(ch: u8) -> u32 {
    match ch {
        b'0'..=b'9' => (ch - b'0') as u32,
        b'A'..=b'Z' => (ch - b'A') as u32 + 10,
        b'a'..=b'z' => (ch - b'a') as u32 + 10,
        _ => 0xff,
    }
}

fn bytes_from_vec(data: &[u8]) -> *mut PyObject {
    // SAFETY: the pointer/length pair describes `data`, which the call
    // copies into the new bytes object.
    unsafe { bytes::PyBytes_FromStringAndSize(data.as_ptr().cast(), data.len() as isize) }
}

// ------------------------------------------------------------------ uu

/// `binascii.a2b_uu`.
unsafe extern "C" fn binascii_a2b_uu(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    let Some(ascii_data) = (unsafe { bytes_arg(arg) }) else {
        return core::ptr::null_mut();
    };
    let mut ascii_len = ascii_data.len() as isize;
    // The C reads the first byte even of empty input, where a bytes
    // object still offers its terminating NUL, then advances past it.
    let bin_len = (ascii_data.first().copied().unwrap_or(0).wrapping_sub(b' ')) & 0x3f;
    ascii_len -= 1;
    let mut out = Vec::with_capacity(bin_len as usize);
    let mut leftbits = 0u32;
    let mut leftchar = 0u32;
    let mut bin_left = bin_len;
    let mut i = 1usize;
    while bin_left > 0 {
        let raw = if ascii_len > 0 { ascii_data[i] } else { 0 };
        let this_ch = if raw == b'\n' || raw == b'\r' || ascii_len <= 0 {
            0
        } else {
            // A few uuencodes use '`' as zero instead of space, hence
            // the 64 where 63 would do.
            if !(b' '..=b' ' + 64).contains(&raw) {
                error_message(b"Illegal char");
                return core::ptr::null_mut();
            }
            (raw.wrapping_sub(b' ')) & 0x3f
        };
        leftchar = (leftchar << 6) | this_ch as u32;
        leftbits += 6;
        if leftbits >= 8 {
            leftbits -= 8;
            out.push(((leftchar >> leftbits) & 0xff) as u8);
            leftchar &= (1 << leftbits) - 1;
            bin_left -= 1;
        }
        ascii_len -= 1;
        i += 1;
    }
    while ascii_len > 0 {
        let this_ch = ascii_data[i];
        i += 1;
        ascii_len -= 1;
        // Extra '`' may be written as padding in some cases.
        if this_ch != b' ' && this_ch != b' ' + 64 && this_ch != b'\n' && this_ch != b'\r' {
            error_message(b"Trailing garbage");
            return core::ptr::null_mut();
        }
    }
    bytes_from_vec(&out)
}

/// `binascii.b2a_uu`; `backtick` is keyword-only (default False).
unsafe extern "C" fn binascii_b2a_uu(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 1 {
        keywords_arity_error("b2a_uu", 1, 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        positional_error_kw("b2a_uu", 1, 1, 0);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the argument is a valid object.
    let Some(bin_data) = (unsafe { bytes_arg(args[0]) }) else {
        return core::ptr::null_mut();
    };
    let backtick = false;
    if bin_data.len() > 45 {
        // The 45 is a limit that appears in all uuencode's.
        error_message(b"At most 45 bytes at once");
        return core::ptr::null_mut();
    }
    let mut out = Vec::with_capacity(2 + bin_data.len().div_ceil(3) * 4);
    out.push(if backtick && bin_data.is_empty() {
        b'`'
    } else {
        b' ' + bin_data.len() as u8
    });
    let mut leftbits = 0u32;
    let mut leftchar = 0u32;
    let mut bin_len = bin_data.len() as isize;
    let mut i = 0usize;
    while bin_len > 0 || leftbits != 0 {
        leftchar = if bin_len > 0 {
            (leftchar << 8) | bin_data[i] as u32
        } else {
            leftchar << 8
        };
        leftbits += 8;
        while leftbits >= 6 {
            let this_ch = ((leftchar >> (leftbits - 6)) & 0x3f) as u8;
            leftbits -= 6;
            out.push(if backtick && this_ch == 0 {
                b'`'
            } else {
                this_ch + b' '
            });
        }
        bin_len -= 1;
        i += 1;
    }
    out.push(b'\n');
    bytes_from_vec(&out)
}

// --------------------------------------------------------------- base64

const BASE64_PAD: u8 = b'=';

/// `table_a2b_base64`; the pad entry maps to 0.
const fn a2b_base64_table() -> [u8; 256] {
    let mut table = [255u8; 256];
    let mut ch = b'A';
    while ch <= b'Z' {
        table[ch as usize] = ch - b'A';
        ch += 1;
    }
    let mut ch = b'a';
    while ch <= b'z' {
        table[ch as usize] = ch - b'a' + 26;
        ch += 1;
    }
    let mut ch = b'0';
    while ch <= b'9' {
        table[ch as usize] = ch - b'0' + 52;
        ch += 1;
    }
    table[b'+' as usize] = 62;
    table[b'/' as usize] = 63;
    table[BASE64_PAD as usize] = 0;
    table
}

static A2B_BASE64: [u8; 256] = a2b_base64_table();

const B2A_BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `binascii.a2b_base64`; `strict_mode` is keyword-only (default False).
unsafe extern "C" fn binascii_a2b_base64(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 1 {
        keywords_arity_error("a2b_base64", 1, 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        positional_error_kw("a2b_base64", 1, 1, 0);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the argument is a valid object.
    let Some(ascii_data) = (unsafe { bytes_arg(args[0]) }) else {
        return core::ptr::null_mut();
    };
    let strict_mode = false;
    let mut out = Vec::with_capacity(ascii_data.len().div_ceil(4) * 3);
    if strict_mode && !ascii_data.is_empty() && ascii_data[0] == BASE64_PAD {
        error_message(b"Leading padding not allowed");
        return core::ptr::null_mut();
    }
    let mut quad_pos = 0u8;
    let mut leftchar = 0u8;
    let mut pads = 0u32;
    let mut padding_started = false;
    // The pad sequence's `goto done` skips the padding check below.
    let mut padded_done = false;
    let mut i = 0usize;
    while i < ascii_data.len() {
        let this_ch = ascii_data[i];
        i += 1;
        if this_ch == BASE64_PAD {
            padding_started = true;
            if strict_mode && quad_pos == 0 {
                error_message(b"Excess padding not allowed");
                return core::ptr::null_mut();
            }
            if quad_pos >= 2
                && quad_pos as u32 + {
                    pads += 1;
                    pads
                } >= 4
            {
                // A pad sequence means we should not parse more input.
                if strict_mode && i < ascii_data.len() {
                    error_message(b"Excess data after padding");
                    return core::ptr::null_mut();
                }
                padded_done = true;
                break;
            }
            continue;
        }
        let this_ch = A2B_BASE64[this_ch as usize];
        if this_ch >= 64 {
            if strict_mode {
                error_message(b"Only base64 data is allowed");
                return core::ptr::null_mut();
            }
            continue;
        }
        if strict_mode && padding_started {
            error_message(b"Discontinuous padding not allowed");
            return core::ptr::null_mut();
        }
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = this_ch;
            }
            1 => {
                quad_pos = 2;
                out.push((leftchar << 2) | (this_ch >> 4));
                leftchar = this_ch & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((leftchar << 4) | (this_ch >> 2));
                leftchar = this_ch & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((leftchar << 6) | this_ch);
                leftchar = 0;
            }
        }
    }
    if !padded_done && quad_pos != 0 {
        if quad_pos == 1 {
            // Exactly one extra data character: no input could encode
            // into such a base64 string.
            error_message(
                format!(
                    "Invalid base64-encoded string: number of data characters ({}) cannot be 1 \
                     more than a multiple of 4",
                    out.len() / 3 * 4 + 1
                )
                .as_bytes(),
            );
        } else {
            error_message(b"Incorrect padding");
        }
        return core::ptr::null_mut();
    }
    bytes_from_vec(&out)
}

/// `binascii.b2a_base64`; `newline` is keyword-only (default True).
unsafe extern "C" fn binascii_b2a_base64(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 1 {
        keywords_arity_error("b2a_base64", 1, 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        positional_error_kw("b2a_base64", 1, 1, 0);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the argument is a valid object.
    let Some(bin_data) = (unsafe { bytes_arg(args[0]) }) else {
        return core::ptr::null_mut();
    };
    let newline = true;
    // `(PY_SSIZE_T_MAX - 3) / 2` in C: unreachable for real allocations,
    // kept so the boundary matches.
    const BASE64_MAXBIN: usize = (isize::MAX as usize - 3) / 2;
    if bin_data.len() > BASE64_MAXBIN {
        error_message(b"Too much data for base64 line");
        return core::ptr::null_mut();
    }
    let mut out = Vec::with_capacity(bin_data.len() * 2 + 2 + usize::from(newline));
    let mut leftbits = 0u32;
    let mut leftchar = 0u32;
    for &b in bin_data {
        leftchar = (leftchar << 8) | b as u32;
        leftbits += 8;
        while leftbits >= 6 {
            let this_ch = ((leftchar >> (leftbits - 6)) & 0x3f) as usize;
            leftbits -= 6;
            out.push(B2A_BASE64[this_ch]);
        }
    }
    if leftbits == 2 {
        out.push(B2A_BASE64[((leftchar & 3) << 4) as usize]);
        out.push(BASE64_PAD);
        out.push(BASE64_PAD);
    } else if leftbits == 4 {
        out.push(B2A_BASE64[((leftchar & 0xf) << 2) as usize]);
        out.push(BASE64_PAD);
    }
    if newline {
        out.push(b'\n');
    }
    bytes_from_vec(&out)
}

// ------------------------------------------------------------------ crc

/// The CRC-CCITT table (poly 0x1021, MSB first).
const fn crctab_hqx() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = (i << 8) as u16;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRCTAB_HQX: [u16; 256] = crctab_hqx();

/// The CRC-32 table (the reflected 0xEDB88320 poly).
const fn crc_32_tab() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC_32_TAB: [u32; 256] = crc_32_tab();

/// `internal_crc32`: Jim Ahlstrom's table-driven CRC-32, the same
/// polynomial zlib's `crc32` computes.
fn internal_crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc = CRC_32_TAB[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The `unsigned int(bitwise=True)` conversion: `__index__`, then the
/// low 32 bits.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn unsigned_int_bitwise(arg: *mut PyObject) -> Option<u32> {
    // SAFETY: the index protocol follows the number slots.
    let index = unsafe { number_index(arg) };
    if index.is_null() {
        return None;
    }
    // SAFETY: the call takes a valid int object.
    let value = unsafe { long::PyLong_AsUnsignedLongMask(index) };
    // SAFETY: the int came from the index protocol with our reference.
    unsafe { exceptions::release_object(index) };
    Some(value as u32)
}

/// `binascii.crc_hqx`.
unsafe extern "C" fn binascii_crc_hqx(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("crc_hqx", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe {
        let Some(data) = byteslike_arg(args[0]) else {
            return core::ptr::null_mut();
        };
        let Some(crc) = unsigned_int_bitwise(args[1]) else {
            return core::ptr::null_mut();
        };
        let mut crc = crc & 0xffff;
        for &b in data {
            crc = ((crc << 8) & 0xff00)
                ^ CRCTAB_HQX[(((crc >> 8) ^ b as u32) & 0xff) as usize] as u32;
        }
        long::PyLong_FromUnsignedLong(crc)
    }
}

/// `binascii.crc32`.
unsafe extern "C" fn binascii_crc32(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if !(1..=2).contains(&nargs) {
        positional_error("crc32", 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe {
        let Some(data) = byteslike_arg(args[0]) else {
            return core::ptr::null_mut();
        };
        let crc = if nargs == 2 {
            let Some(crc) = unsigned_int_bitwise(args[1]) else {
                return core::ptr::null_mut();
            };
            crc
        } else {
            0
        };
        long::PyLong_FromUnsignedLong(internal_crc32(data, crc))
    }
}

// ------------------------------------------------------------------ hex

/// `PyObject_Length` reduced to this runtime: `mp_length`, then
/// `sq_length`, then "object of type 'X' has no len()".
///
/// # Safety
///
/// `obj` must be a valid object.
unsafe fn object_length(obj: *mut PyObject) -> isize {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(obj) };
    // SAFETY: the slots, when present, take the object.
    unsafe {
        if let Some(table) = (*ty).tp_as_mapping.as_ref()
            && let Some(mp_length) = table.mp_length
        {
            return mp_length(obj);
        }
        if let Some(table) = (*ty).tp_as_sequence.as_ref()
            && let Some(sq_length) = table.sq_length
        {
            return sq_length(obj);
        }
    }
    let name = unsafe { crate::util::cstr((*ty).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!("object of type '{name}' has no len()").as_bytes(),
    );
    -1
}

/// `_Py_strhex_impl` with `return_bytes` set.
fn strhex_bytes(data: &[u8], sep: *mut PyObject, bytes_per_group: i32) -> *mut PyObject {
    let hexdigits = b"0123456789abcdef";
    let mut bytes_per_group = bytes_per_group;
    let sep_char = if !sep.is_null() {
        // SAFETY: the length protocol takes any valid object.
        let seplen = unsafe { object_length(sep) };
        if seplen < 0 {
            return core::ptr::null_mut();
        }
        if seplen != 1 {
            tstate::set_string(exceptions::value_error(), b"sep must be length 1.");
            return core::ptr::null_mut();
        }
        // SAFETY: the type check precedes the payload read.
        if !unsafe { bytes::check_exact(sep) } {
            tstate::set_string(exceptions::type_error(), b"sep must be str or bytes.");
            return core::ptr::null_mut();
        }
        unsafe { bytes::bytes_of(sep)[0] }
    } else {
        bytes_per_group = 0;
        0
    };
    let arglen = data.len() as isize;
    let mut abs_bytes_per_sep = bytes_per_group.unsigned_abs() as isize;
    if abs_bytes_per_sep >= arglen {
        bytes_per_group = 0;
        abs_bytes_per_sep = 0;
    }
    let mut resultlen = arglen * 2;
    if bytes_per_group != 0 && arglen > 0 {
        resultlen += (arglen - 1) / abs_bytes_per_sep;
    }
    let mut retbuf = vec![0u8; resultlen as usize];
    if bytes_per_group == 0 {
        let mut j = 0usize;
        for &c in data {
            retbuf[j] = hexdigits[(c >> 4) as usize];
            retbuf[j + 1] = hexdigits[(c & 0x0f) as usize];
            j += 2;
        }
    } else {
        // The number of complete chunk+sep periods.
        let chunks = (arglen - 1) / abs_bytes_per_sep;
        if bytes_per_group < 0 {
            let mut i = 0usize;
            let mut j = 0usize;
            for _ in 0..chunks {
                for _ in 0..abs_bytes_per_sep {
                    let c = data[i];
                    i += 1;
                    retbuf[j] = hexdigits[(c >> 4) as usize];
                    retbuf[j + 1] = hexdigits[(c & 0x0f) as usize];
                    j += 2;
                }
                retbuf[j] = sep_char;
                j += 1;
            }
            while i < data.len() {
                let c = data[i];
                i += 1;
                retbuf[j] = hexdigits[(c >> 4) as usize];
                retbuf[j + 1] = hexdigits[(c & 0x0f) as usize];
                j += 2;
            }
        } else {
            let mut i = arglen - 1;
            let mut j = resultlen - 1;
            for _ in 0..chunks {
                for _ in 0..abs_bytes_per_sep {
                    let c = data[i as usize];
                    i -= 1;
                    retbuf[j as usize] = hexdigits[(c & 0x0f) as usize];
                    j -= 1;
                    retbuf[j as usize] = hexdigits[(c >> 4) as usize];
                    j -= 1;
                }
                retbuf[j as usize] = sep_char;
                j -= 1;
            }
            while i >= 0 {
                let c = data[i as usize];
                i -= 1;
                retbuf[j as usize] = hexdigits[(c & 0x0f) as usize];
                j -= 1;
                retbuf[j as usize] = hexdigits[(c >> 4) as usize];
                j -= 1;
            }
        }
    }
    bytes_from_vec(&retbuf)
}

/// The shared body of `b2a_hex` and `hexlify`: `sep` and
/// `bytes_per_sep` are positional-or-keyword, so both arrive
/// positionally here.
///
/// # Safety
///
/// `args` must hold 1 to 3 valid objects.
unsafe fn b2a_hex_body(args: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: the arguments are valid objects.
    unsafe {
        let Some(data) = bytes_arg(args[0]) else {
            return core::ptr::null_mut();
        };
        // CPython passes `sep` through raw: `None` is not a separator
        // here and trips the length protocol, unlike `bytes.hex`.
        let sep = if args.len() >= 2 {
            args[1]
        } else {
            core::ptr::null_mut()
        };
        let bytes_per_sep = if args.len() >= 3 {
            let index = number_index(args[2]);
            if index.is_null() {
                return core::ptr::null_mut();
            }
            let value = long::PyLong_AsLongLong(index);
            // SAFETY: the int came from the index protocol with our reference.
            exceptions::release_object(index);
            // The clinic's int converter reports its own C int overflow
            // message even where a wider conversion would succeed.
            if value == -1 && !tstate::occurred().is_null() {
                tstate::clear();
                tstate::set_string(
                    exceptions::overflow_error(),
                    b"Python int too large to convert to C int",
                );
                return core::ptr::null_mut();
            }
            let Ok(value) = i32::try_from(value) else {
                tstate::set_string(
                    exceptions::overflow_error(),
                    b"Python int too large to convert to C int",
                );
                return core::ptr::null_mut();
            };
            value
        } else {
            1
        };
        strhex_bytes(data, sep, bytes_per_sep)
    }
}

/// `binascii.b2a_hex`.
unsafe extern "C" fn binascii_b2a_hex(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 3 {
        keywords_arity_error("b2a_hex", 1, 3, 3, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        missing_argument_error("b2a_hex", "data", 1);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the body checks each argument.
    unsafe { b2a_hex_body(args) }
}

/// `binascii.hexlify`, CPython's alias of `b2a_hex` under its own name.
unsafe extern "C" fn binascii_hexlify(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 3 {
        keywords_arity_error("hexlify", 1, 3, 3, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        missing_argument_error("hexlify", "data", 1);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the body checks each argument.
    unsafe { b2a_hex_body(args) }
}

/// `binascii.a2b_hex`'s body; `unhexlify` shares it.
fn a2b_hex_body(hexstr: &[u8]) -> *mut PyObject {
    if !hexstr.len().is_multiple_of(2) {
        error_message(b"Odd-length string");
        return core::ptr::null_mut();
    }
    let mut out = Vec::with_capacity(hexstr.len() / 2);
    let mut i = 0usize;
    while i < hexstr.len() {
        let top = digit_value(hexstr[i]);
        let bot = digit_value(hexstr[i + 1]);
        if top >= 16 || bot >= 16 {
            error_message(b"Non-hexadecimal digit found");
            return core::ptr::null_mut();
        }
        out.push(((top << 4) + bot) as u8);
        i += 2;
    }
    bytes_from_vec(&out)
}

/// `binascii.a2b_hex`.
unsafe extern "C" fn binascii_a2b_hex(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    let Some(hexstr) = (unsafe { bytes_arg(arg) }) else {
        return core::ptr::null_mut();
    };
    a2b_hex_body(hexstr)
}

/// `binascii.unhexlify`.
unsafe extern "C" fn binascii_unhexlify(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    let Some(hexstr) = (unsafe { bytes_arg(arg) }) else {
        return core::ptr::null_mut();
    };
    a2b_hex_body(hexstr)
}

// ------------------------------------------------------------------ qp

const MAXLINESIZE: u32 = 76;

/// `binascii.a2b_qp`; `header` is positional-or-keyword (default False).
unsafe extern "C" fn binascii_a2b_qp(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 2 {
        keywords_arity_error("a2b_qp", 1, 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        // a2b_qp's params are positional-or-keyword, so the shortfall
        // reports the parameter by name.
        missing_argument_error("a2b_qp", "data", 1);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe {
        let Some(ascii_data) = bytes_arg(args[0]) else {
            return core::ptr::null_mut();
        };
        let header = if nargs == 2 { is_true(args[1]) } else { false };
        let datalen = ascii_data.len();
        let mut odata = Vec::with_capacity(datalen);
        let mut input = 0usize;
        while input < datalen {
            if ascii_data[input] == b'=' {
                input += 1;
                if input >= datalen {
                    break;
                }
                // Soft line breaks.
                if ascii_data[input] == b'\n' || ascii_data[input] == b'\r' {
                    if ascii_data[input] != b'\n' {
                        while input < datalen && ascii_data[input] != b'\n' {
                            input += 1;
                        }
                    }
                    if input < datalen {
                        input += 1;
                    }
                } else if ascii_data[input] == b'=' {
                    // broken case from broken python qp
                    odata.push(b'=');
                    input += 1;
                } else if input + 1 < datalen
                    && ascii_data[input].is_ascii_hexdigit()
                    && ascii_data[input + 1].is_ascii_hexdigit()
                {
                    let ch = (digit_value(ascii_data[input]) << 4) as u8;
                    input += 1;
                    let ch = ch | digit_value(ascii_data[input]) as u8;
                    input += 1;
                    odata.push(ch);
                } else {
                    odata.push(b'=');
                }
            } else if header && ascii_data[input] == b'_' {
                odata.push(b' ');
                input += 1;
            } else {
                odata.push(ascii_data[input]);
                input += 1;
            }
        }
        bytes_from_vec(&odata)
    }
}

/// `PyObject_IsTrue` for the flag arguments; a slot error keeps the
/// exception and reads as false, as CPython's `if (quotetabs)` does.
///
/// # Safety
///
/// `obj` must be a valid object.
unsafe fn is_true(obj: *mut PyObject) -> bool {
    // SAFETY: the call follows the truth protocol.
    unsafe { crate::modules::object_is_true(obj) > 0 }
}

/// `to_hex`: the uppercase two-digit form.
fn to_hex(ch: u8) -> [u8; 2] {
    let hexdigits = b"0123456789ABCDEF";
    [
        hexdigits[(ch >> 4) as usize],
        hexdigits[(ch & 0x0f) as usize],
    ]
}

/// `binascii.b2a_qp`; `quotetabs` (False), `istext` (True) and
/// `header` (False) are positional-or-keyword.
unsafe extern "C" fn binascii_b2a_qp(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 4 {
        keywords_arity_error("b2a_qp", 1, 4, 4, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { crate::modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        missing_argument_error("b2a_qp", "data", 1);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe {
        let Some(databuf) = bytes_arg(args[0]) else {
            return core::ptr::null_mut();
        };
        let quotetabs = if nargs >= 2 { is_true(args[1]) } else { false };
        let istext = if nargs >= 3 { is_true(args[2]) } else { true };
        let header = if nargs >= 4 { is_true(args[3]) } else { false };
        b2a_qp_body(databuf, quotetabs, istext, header)
    }
}

/// The two-pass `b2a_qp` algorithm: one scan sizes the output (with
/// the line-length bookkeeping), the second fills it.
fn b2a_qp_body(databuf: &[u8], quotetabs: bool, istext: bool, header: bool) -> *mut PyObject {
    let datalen = databuf.len();
    // See if this string is using CRLF line ends; the pass below
    // converts every end of line to the same form.
    let crlf = matches!(databuf.iter().position(|&b| b == b'\n'), Some(at) if at > 0 && databuf[at - 1] == b'\r');

    let mut odatalen = 0usize;
    {
        let mut linelen = 0u32;
        let mut input = 0usize;
        while input < datalen {
            let mut delta = 0usize;
            if databuf[input] > 126
                || databuf[input] == b'='
                || (header && databuf[input] == b'_')
                || (databuf[input] == b'.'
                    && linelen == 0
                    && (input + 1 == datalen
                        || databuf[input + 1] == b'\n'
                        || databuf[input + 1] == b'\r'
                        || databuf[input + 1] == 0))
                || (!istext && (databuf[input] == b'\r' || databuf[input] == b'\n'))
                || ((databuf[input] == b'\t' || databuf[input] == b' ') && input + 1 == datalen)
                || (databuf[input] < 33
                    && databuf[input] != b'\r'
                    && databuf[input] != b'\n'
                    && (quotetabs || (databuf[input] != b'\t' && databuf[input] != b' ')))
            {
                if linelen + 3 >= MAXLINESIZE {
                    linelen = 0;
                    delta += if crlf { 3 } else { 2 };
                }
                linelen += 3;
                delta += 3;
                input += 1;
            } else if istext
                && (databuf[input] == b'\n'
                    || (input + 1 < datalen
                        && databuf[input] == b'\r'
                        && databuf[input + 1] == b'\n'))
            {
                linelen = 0;
                // Protect against whitespace on end of line.
                if input > 0 && (databuf[input - 1] == b' ' || databuf[input - 1] == b'\t') {
                    delta += 2;
                }
                delta += if crlf { 2 } else { 1 };
                if databuf[input] == b'\r' {
                    input += 2;
                } else {
                    input += 1;
                }
            } else {
                if input + 1 != datalen && databuf[input + 1] != b'\n' && linelen + 1 >= MAXLINESIZE
                {
                    linelen = 0;
                    delta += if crlf { 3 } else { 2 };
                }
                linelen += 1;
                delta += 1;
                input += 1;
            }
            odatalen += delta;
        }
    }

    let mut odata = Vec::with_capacity(odatalen);
    {
        let mut linelen = 0u32;
        let mut input = 0usize;
        while input < datalen {
            if databuf[input] > 126
                || databuf[input] == b'='
                || (header && databuf[input] == b'_')
                || (databuf[input] == b'.'
                    && linelen == 0
                    && (input + 1 == datalen
                        || databuf[input + 1] == b'\n'
                        || databuf[input + 1] == b'\r'
                        || databuf[input + 1] == 0))
                || (!istext && (databuf[input] == b'\r' || databuf[input] == b'\n'))
                || ((databuf[input] == b'\t' || databuf[input] == b' ') && input + 1 == datalen)
                || (databuf[input] < 33
                    && databuf[input] != b'\r'
                    && databuf[input] != b'\n'
                    && (quotetabs || (databuf[input] != b'\t' && databuf[input] != b' ')))
            {
                if linelen + 3 >= MAXLINESIZE {
                    odata.push(b'=');
                    if crlf {
                        odata.push(b'\r');
                    }
                    odata.push(b'\n');
                    linelen = 0;
                }
                odata.push(b'=');
                odata.extend_from_slice(&to_hex(databuf[input]));
                input += 1;
                linelen += 3;
            } else if istext
                && (databuf[input] == b'\n'
                    || (input + 1 < datalen
                        && databuf[input] == b'\r'
                        && databuf[input + 1] == b'\n'))
            {
                linelen = 0;
                // Protect against whitespace on end of line.
                if !odata.is_empty()
                    && (odata.last() == Some(&b' ') || odata.last() == Some(&b'\t'))
                {
                    let ch = *odata.last().unwrap();
                    let last = odata.len() - 1;
                    odata[last] = b'=';
                    odata.extend_from_slice(&to_hex(ch));
                }
                if crlf {
                    odata.push(b'\r');
                }
                odata.push(b'\n');
                if databuf[input] == b'\r' {
                    input += 2;
                } else {
                    input += 1;
                }
            } else if input + 1 != datalen
                && databuf[input + 1] != b'\n'
                && linelen + 1 >= MAXLINESIZE
            {
                odata.push(b'=');
                if crlf {
                    odata.push(b'\r');
                }
                odata.push(b'\n');
                linelen = 0;
                linelen += 1;
                if header && databuf[input] == b' ' {
                    odata.push(b'_');
                    input += 1;
                } else {
                    odata.push(databuf[input]);
                    input += 1;
                }
            } else {
                linelen += 1;
                if header && databuf[input] == b' ' {
                    odata.push(b'_');
                } else {
                    odata.push(databuf[input]);
                }
                input += 1;
            }
        }
    }
    bytes_from_vec(&odata)
}

macro_rules! method_def {
    ($name:literal, $func:ident, $flags:expr) => {{
        let func: PyCFunction = $func;
        PyMethodDef {
            ml_name: concat!($name, "\0").as_ptr() as *const core::ffi::c_char,
            ml_meth: Some(func),
            ml_flags: $flags,
            ml_doc: core::ptr::null(),
        }
    }};
    ($name:literal, $func:ident, $ty:ty, $flags:expr) => {{
        let func: $ty = $func;
        PyMethodDef {
            ml_name: concat!($name, "\0").as_ptr() as *const core::ffi::c_char,
            ml_meth: Some(unsafe {
                // SAFETY: the dispatch in pycfunction.rs reads this
                // field back through exactly the $ty signature chosen
                // here, so every call happens at the function's real
                // type.
                core::mem::transmute::<$ty, PyCFunction>(func)
            }),
            ml_flags: $flags,
            ml_doc: core::ptr::null(),
        }
    }};
}

/// The `binascii` functions as CPython orders them.
pub static BINASCII_METHODS: crate::modules::MethodTable = crate::modules::MethodTable(&[
    method_def!("a2b_uu", binascii_a2b_uu, METH_O),
    method_def!(
        "b2a_uu",
        binascii_b2a_uu,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!(
        "a2b_base64",
        binascii_a2b_base64,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!(
        "b2a_base64",
        binascii_b2a_base64,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("a2b_hex", binascii_a2b_hex, METH_O),
    method_def!(
        "b2a_hex",
        binascii_b2a_hex,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!(
        "hexlify",
        binascii_hexlify,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("unhexlify", binascii_unhexlify, METH_O),
    method_def!("crc_hqx", binascii_crc_hqx, _PyCFunctionFast, METH_FASTCALL),
    method_def!("crc32", binascii_crc32, _PyCFunctionFast, METH_FASTCALL),
    method_def!(
        "a2b_qp",
        binascii_a2b_qp,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!(
        "b2a_qp",
        binascii_b2a_qp,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
]);

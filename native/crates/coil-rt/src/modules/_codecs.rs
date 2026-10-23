//! The `_codecs` module (spec section 10.6): the codec registry, the
//! error-handler registry with CPython's built-in handlers, and the
//! codec-function layer `Lib/encodings` builds every built-in codec
//! from. The registry's Python-facing half — CodecInfo objects, calls
//! into registered search functions, calling registered error handlers
//! — needs the container object model; those paths raise typed errors
//! naming what is missing while the codec work itself runs natively.

use std::sync::{Mutex, Once, OnceLock};

use core::ffi::{c_char, c_int};

use coil_capi_sys::PyObject;

use crate::object::header;
use crate::object::singletons;
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::unicode;
use crate::types::{bytes as rt_bytes, long, tuple};
use crate::util;

// ------------------------------------------------------------- the registry

/// A stored object reference; the registry owns it across threads the
/// way CPython's interpreter state does.
struct RegistryRef(*mut PyObject);

// SAFETY: the reference is opaque storage; every access happens under
// the registry's lock, like the GIL it replaces.
unsafe impl Send for RegistryRef {}

fn search_path() -> &'static Mutex<Vec<RegistryRef>> {
    static PATH: OnceLock<Mutex<Vec<RegistryRef>>> = OnceLock::new();
    PATH.get_or_init(|| Mutex::new(Vec::new()))
}

fn error_registry() -> &'static Mutex<Vec<(String, RegistryRef)>> {
    static REGISTRY: OnceLock<Mutex<Vec<(String, RegistryRef)>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// The codec error paths' handler-name lookup (the `ErrHandler::Named`
/// resolution): `namereplace` is one of CPython's built-ins but needs
/// the unicodedata name tables, and a user-registered handler needs the
/// call protocol.
fn handler_lookup(name: &str) -> Option<&'static str> {
    if name == "namereplace" {
        return Some("coil: the 'namereplace' error handler needs the unicodedata name tables");
    }
    if error_registry()
        .lock()
        .expect("error handler registry poisoned")
        .iter()
        .any(|(registered, _)| registered == name)
    {
        return Some("coil: calling a registered error handler needs the container runtime");
    }
    None
}

/// Wire the registry into the codec error paths; every entry point runs
/// this once.
fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| unicode::install_handler_lookup(handler_lookup));
}

/// The canonical codec-function prefix of a built-in encoding name
/// (already normalized): the `encodings.<name>` module the CPython
/// registry loads dispatches to these, and the type errors its wrappers
/// surface name them.
fn codec_function_prefix(name: &str) -> Option<&'static str> {
    Some(match name {
        "utf8" | "u8" | "utf_8" => "utf_8",
        "utf7" | "u7" | "utf_7" => "utf_7",
        "utf16" | "u16" | "utf_16" => "utf_16",
        "utf_16le" | "utf_16_le" => "utf_16_le",
        "utf_16be" | "utf_16_be" => "utf_16_be",
        "utf32" | "u32" | "utf_32" => "utf_32",
        "utf_32le" | "utf_32_le" => "utf_32_le",
        "utf_32be" | "utf_32_be" => "utf_32_be",
        "ascii" | "us_ascii" | "646" | "ansi_x34_1968" | "ansi_x3_4_1968" => "ascii",
        "latin" | "latin1" | "l1" | "8859" | "cp819" | "iso8859_1" | "iso_8859_1" | "latin_1" => {
            "latin_1"
        }
        "unicode_escape" => "unicode_escape",
        "raw_unicode_escape" => "raw_unicode_escape",
        "charmap" => "charmap",
        "mbcs" => "mbcs",
        "oem" => "oem",
        _ => return None,
    })
}

/// `_PyCodec_Lookup`'s failure over a normalized name, the one every
/// registry entry point shares. A name the native codec table resolves
/// gets the typed error for what is actually missing — the CodecInfo
/// object — rather than a LookupError the runtime's own `encode` and
/// `decode` would contradict; a name beyond the table falls to
/// CPython's messages.
fn raise_lookup_failure(name: &str) {
    if codec_function_prefix(name).is_some() {
        tstate::set_string(
            exceptions::system_error(),
            b"coil: building CodecInfo objects needs the container runtime",
        );
        return;
    }
    if search_path()
        .lock()
        .expect("codec search path poisoned")
        .is_empty()
    {
        tstate::set_string(
            exceptions::lookup_error(),
            b"no codec search functions registered: can't find encoding",
        );
    } else {
        tstate::set_string(
            exceptions::system_error(),
            b"coil: calling codec search functions needs the container runtime",
        );
    }
}

/// The encoding-name argument: normalized, with CPython's BadArgument
/// for a null pointer.
///
/// # Safety
///
/// `encoding` must be null or a valid C string.
unsafe fn encoding_name(encoding: *const c_char) -> Option<String> {
    // SAFETY: the caller owns the C string for the call's duration.
    unsafe {
        if encoding.is_null() {
            tstate::set_string(
                exceptions::type_error(),
                b"bad argument type for built-in operation",
            );
            return None;
        }
        // CPython's registry normalizes into a 100-byte buffer.
        Some(unicode::normalize_encoding(util::cstr(encoding), 100))
    }
}

/// `PyCodec_Register`.
///
/// # Safety
///
/// `search_function` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Register(search_function: *mut PyObject) -> c_int {
    // SAFETY: header reads stay within the object.
    unsafe {
        install();
        if search_function.is_null() {
            tstate::set_string(
                exceptions::type_error(),
                b"bad argument type for built-in operation",
            );
            return -1;
        }
        if (*header::type_of(search_function)).tp_call.is_none() {
            tstate::set_string(exceptions::type_error(), b"argument must be callable");
            return -1;
        }
        exceptions::retain_object(search_function);
        search_path()
            .lock()
            .expect("codec search path poisoned")
            .push(RegistryRef(search_function));
        0
    }
}

/// `PyCodec_Unregister`.
///
/// # Safety
///
/// `search_function` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Unregister(search_function: *mut PyObject) -> c_int {
    // SAFETY: the registry owns every stored reference.
    unsafe {
        install();
        let mut path = search_path().lock().expect("codec search path poisoned");
        if let Some(index) = path
            .iter()
            .position(|item| core::ptr::eq(item.0, search_function))
        {
            let removed = path.remove(index).0;
            drop(path);
            exceptions::release_object(removed);
        }
        0
    }
}

/// `_PyCodec_Lookup`.
///
/// # Safety
///
/// `encoding` must be null or a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCodec_Lookup(encoding: *const c_char) -> *mut PyObject {
    // SAFETY: the name read stays within the C string.
    unsafe {
        install();
        let Some(name) = encoding_name(encoding) else {
            return core::ptr::null_mut();
        };
        raise_lookup_failure(&name);
        core::ptr::null_mut()
    }
}

/// `PyCodec_KnownEncoding`: the native codec table is the known set,
/// since `PyCodec_Encode` and `PyCodec_Decode` resolve through it.
///
/// # Safety
///
/// `encoding` must be null or a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_KnownEncoding(encoding: *const c_char) -> c_int {
    // SAFETY: the name read stays within the C string.
    unsafe {
        install();
        let Some(name) = encoding_name(encoding) else {
            return -1;
        };
        if codec_function_prefix(&name).is_some() {
            1
        } else {
            0
        }
    }
}

/// The buffer argument of the registry's decode path: bytes contents,
/// and for the three escape codecs a str's UTF-8 bytes, the way their
/// clinic `Py_buffer(accept={str, buffer})` argument converts it.
///
/// # Safety
///
/// `object` must be a valid object.
unsafe fn decode_input(object: *mut PyObject, prefix: &str) -> Option<Vec<u8>> {
    // SAFETY: type reads and contents stay within the object.
    unsafe {
        if rt_bytes::check(object) || rt_bytes::bytearray_check(object) {
            let (size, contents) = rt_bytes::as_string_parts(object);
            return Some(
                core::slice::from_raw_parts(contents.cast::<u8>(), size.max(0) as usize).to_vec(),
            );
        }
        if unicode::check(object)
            && matches!(prefix, "escape" | "unicode_escape" | "raw_unicode_escape")
        {
            let chars = unicode::as_chars(object)?;
            return unicode::encode_utf8_bytes(&chars.as_vec(), unicode::ErrHandler::Strict);
        }
        let name = util::cstr((*header::type_of(object)).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("a bytes-like object is required, not '{name}'").as_bytes(),
        );
        None
    }
}

/// `PyCodec_Decode`: the native codec table applied to bytes-like input.
///
/// # Safety
///
/// `object` must be a valid object; the C strings valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Decode(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C API contract.
    unsafe {
        install();
        let Some(name) = encoding_name(encoding) else {
            return core::ptr::null_mut();
        };
        let Some(prefix) = codec_function_prefix(&name) else {
            raise_lookup_failure(&name);
            return core::ptr::null_mut();
        };
        let Some(data) = decode_input(object, prefix) else {
            return core::ptr::null_mut();
        };
        if prefix == "mbcs" || prefix == "oem" {
            // The encodings wrappers decode with `final=True`.
            return decode_windows_codec(prefix, &data, errors);
        }
        let handler = unicode::error_handler(unicode::errors_arg(errors));
        let Some(chars) = unicode::decode_by_name(Some(&name), &data, handler) else {
            return core::ptr::null_mut();
        };
        unicode::chars_to_object(&chars, 0)
    }
}

/// `PyCodec_Encode`: the native codec table applied to a str.
///
/// # Safety
///
/// `object` must be a valid object; the C strings valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Encode(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C API contract.
    unsafe {
        install();
        let Some(name) = encoding_name(encoding) else {
            return core::ptr::null_mut();
        };
        let Some(prefix) = codec_function_prefix(&name) else {
            raise_lookup_failure(&name);
            return core::ptr::null_mut();
        };
        if !unicode::check(object) {
            let tp = util::cstr((*header::type_of(object)).tp_name);
            tstate::set_string(
                exceptions::type_error(),
                format!("{prefix}_encode() argument 1 must be str, not {tp}").as_bytes(),
            );
            return core::ptr::null_mut();
        }
        if prefix == "mbcs" || prefix == "oem" {
            return encode_windows_codec(object, prefix, errors);
        }
        let Some(chars) = unicode::as_chars(object) else {
            return core::ptr::null_mut();
        };
        let handler = unicode::error_handler(unicode::errors_arg(errors));
        let Some(out) = unicode::encode_by_name(&name, &chars.as_vec(), handler) else {
            return core::ptr::null_mut();
        };
        match rt_bytes::from_vec(Some(out)) {
            Some(object) => object,
            None => core::ptr::null_mut(),
        }
    }
}

/// The `mbcs`/`oem` codecs: Windows code pages on Windows, and off it
/// CPython does not build them at all, so the typed error names the
/// platform.
fn decode_windows_codec(prefix: &str, data: &[u8], errors: *const c_char) -> *mut PyObject {
    #[cfg(target_os = "windows")]
    {
        let code_page = if prefix == "mbcs" { CP_ACP } else { CP_OEMCP };
        // SAFETY: the C string is the caller's contract.
        let errors = unsafe { unicode::errors_arg(errors) };
        let decoded = decode_stateful(code_page as i32, data, errors, None);
        // `PyCodec_Decode` returns the decoded value, not the module's
        // tuple.
        match decoded {
            Some(chars) => unicode::chars_to_object(&chars, 0),
            None => core::ptr::null_mut(),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (data, errors);
        tstate::set_string(
            exceptions::system_error(),
            format!("coil: the '{prefix}' codec needs Windows code pages").as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// The encode side of `mbcs`/`oem` (see `decode_windows_codec`).
///
/// # Safety
///
/// `object` must be a str object.
unsafe fn encode_windows_codec(
    object: *mut PyObject,
    prefix: &str,
    errors: *const c_char,
) -> *mut PyObject {
    #[cfg(target_os = "windows")]
    {
        // SAFETY: the str contents read stays within the object.
        unsafe {
            let code_page = if prefix == "mbcs" { CP_ACP } else { CP_OEMCP };
            let Some(chars) = unicode::as_chars(object) else {
                return core::ptr::null_mut();
            };
            let out = encode(code_page as i32, &chars.as_vec(), unicode::errors_arg(errors));
            match out {
                Some(out) => match rt_bytes::from_vec(Some(out)) {
                    Some(object) => object,
                    None => core::ptr::null_mut(),
                },
                None => core::ptr::null_mut(),
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (object, errors);
        tstate::set_string(
            exceptions::system_error(),
            format!("coil: the '{prefix}' codec needs Windows code pages").as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// The codec-object entry points: each needs the CodecInfo tuple's
/// items or calls into them, so after the lookup failure they have
/// nothing to add.
macro_rules! codec_object_entry {
    ($(fn $name:ident($encoding:ident: $encoding_ty:ty $(, $arg:ident: $ty:ty)*)
        ;)*) => {
        $(
            #[doc = concat!("`", stringify!($name), "`.")]
            ///
            /// # Safety
            ///
            /// The pointer arguments must be valid objects or null C strings.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name(
                $encoding: $encoding_ty $(, $arg: $ty)*
            ) -> *mut PyObject {
                // SAFETY: the name read stays within the C string; the
                // other arguments never reach use.
                unsafe {
                    let _ = ($($arg),*);
                    install();
                    let Some(name) = encoding_name($encoding) else {
                        return core::ptr::null_mut();
                    };
                    raise_lookup_failure(&name);
                    core::ptr::null_mut()
                }
            }
        )*
    };
}

codec_object_entry! {
    fn PyCodec_Encoder(encoding: *const c_char);
    fn PyCodec_Decoder(encoding: *const c_char);
    fn PyCodec_IncrementalEncoder(encoding: *const c_char, errors: *const c_char);
    fn PyCodec_IncrementalDecoder(encoding: *const c_char, errors: *const c_char);
    fn PyCodec_StreamReader(encoding: *const c_char, stream: *mut PyObject, errors: *const c_char);
    fn PyCodec_StreamWriter(encoding: *const c_char, stream: *mut PyObject, errors: *const c_char);
    fn _PyCodec_LookupTextEncoding(encoding: *const c_char, alternate_command: *const c_char);
}

/// `_PyCodec_EncodeText`: `_codecs.encode`'s text path.
///
/// # Safety
///
/// `object` must be a valid object; the C strings valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCodec_EncodeText(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the name read stays within the C string; the other
    // arguments never reach use.
    unsafe {
        let _ = (object, errors);
        install();
        let Some(name) = encoding_name(encoding) else {
            return core::ptr::null_mut();
        };
        raise_lookup_failure(&name);
        core::ptr::null_mut()
    }
}

/// `_PyCodec_DecodeText`: `_codecs.decode`'s text path.
///
/// # Safety
///
/// `object` must be a valid object; the C strings valid or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCodec_DecodeText(
    object: *mut PyObject,
    encoding: *const c_char,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: as `_PyCodec_EncodeText`.
    unsafe { _PyCodec_EncodeText(object, encoding, errors) }
}

/// `_PyCodecInfo_GetIncrementalDecoder`: the codec_info object this
/// reads attributes from exists only in the container runtime.
///
/// # Safety
///
/// `codec_info` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCodecInfo_GetIncrementalDecoder(
    codec_info: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    let _ = (codec_info, errors);
    tstate::set_string(
        exceptions::system_error(),
        b"coil: CodecInfo attribute access needs the container runtime",
    );
    core::ptr::null_mut()
}

/// `_PyCodecInfo_GetIncrementalEncoder`.
///
/// # Safety
///
/// `codec_info` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCodecInfo_GetIncrementalEncoder(
    codec_info: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: as the decoder form.
    unsafe { _PyCodecInfo_GetIncrementalDecoder(codec_info, errors) }
}

/// `PyCodec_RegisterError`.
///
/// # Safety
///
/// `error` must be a valid object; `name` a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_RegisterError(name: *const c_char, error: *mut PyObject) -> c_int {
    // SAFETY: the C string and object follow the C API contract.
    unsafe {
        install();
        if name.is_null() {
            tstate::set_string(
                exceptions::type_error(),
                b"bad argument type for built-in operation",
            );
            return -1;
        }
        if error.is_null() || (*header::type_of(error)).tp_call.is_none() {
            tstate::set_string(exceptions::type_error(), b"handler must be callable");
            return -1;
        }
        let key = util::cstr(name);
        let mut registry = error_registry()
            .lock()
            .expect("error handler registry poisoned");
        match registry
            .iter()
            .position(|(registered, _)| registered == key)
        {
            Some(index) => {
                let replaced = core::mem::replace(&mut registry[index].1, RegistryRef(error)).0;
                drop(registry);
                exceptions::release_object(replaced);
            }
            None => {
                exceptions::retain_object(error);
                registry.push((key.to_string(), RegistryRef(error)));
            }
        }
        0
    }
}

/// `PyCodec_LookupError`. The built-ins this runtime implements are
/// answered by the codec error paths themselves; a callable handler
/// object needs the call protocol, and `NULL` names `strict` the way
/// CPython does.
///
/// # Safety
///
/// `name` must be null or a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_LookupError(name: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is the caller's contract.
    unsafe {
        install();
        if name.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"coil: returning the strict error handler as an object needs the container runtime",
            );
            return core::ptr::null_mut();
        }
        let key = util::cstr(name);
        let present = matches!(
            key,
            "strict"
                | "ignore"
                | "replace"
                | "backslashreplace"
                | "xmlcharrefreplace"
                | "surrogateescape"
                | "surrogatepass"
                | "namereplace"
        ) || error_registry()
            .lock()
            .expect("error handler registry poisoned")
            .iter()
            .any(|(registered, _)| registered == key);
        if !present {
            tstate::set_string(
                exceptions::lookup_error(),
                format!("unknown error handler name '{key}'").as_bytes(),
            );
        } else {
            tstate::set_string(
                exceptions::system_error(),
                b"coil: returning an error handler object needs the container runtime",
            );
        }
        core::ptr::null_mut()
    }
}

// ------------------------------------------------- the error handler exports

/// Which of the three Unicode error types an exception instance is, for
/// the handlers' field reads.
///
/// # Safety
///
/// `exc` must be a valid object.
unsafe fn error_kind(exc: *mut PyObject) -> Option<ErrorKind> {
    // SAFETY: the type chain read stays within static types.
    unsafe {
        if unicode::is_exception_instance(exc, exceptions::unicode_encode_error()) {
            Some(ErrorKind::Encode)
        } else if unicode::is_exception_instance(exc, exceptions::unicode_decode_error()) {
            Some(ErrorKind::Decode)
        } else if unicode::is_exception_instance(exc, exceptions::unicode_translate_error()) {
            Some(ErrorKind::Translate)
        } else {
            None
        }
    }
}

enum ErrorKind {
    Encode,
    Decode,
    Translate,
}

/// `wrong_exception_type`'s TypeError.
unsafe fn wrong_exception_type(exc: *mut PyObject) {
    // SAFETY: the type read stays within the object.
    unsafe {
        let name = util::cstr((*header::type_of(exc)).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("don't know how to handle {name} in error callback").as_bytes(),
        );
    }
}

/// The `(replacement, end)` tuple the handlers return.
fn handler_tuple(replacement: *mut PyObject, end: isize) -> *mut PyObject {
    let end_object = long::PyLong_FromSsize_t(end);
    if end_object.is_null() {
        // SAFETY: our reference, created a moment ago.
        unsafe { exceptions::release_object(replacement) };
        return core::ptr::null_mut();
    }
    tuple::from_array(&[replacement, end_object])
}

/// The handler result's `(start, end)`; -1 with the error pending.
///
/// # Safety
///
/// `exc` must be one of the three Unicode error instances.
unsafe fn error_range(exc: *mut PyObject, kind: &ErrorKind) -> Option<(isize, isize)> {
    // SAFETY: the accessor's instance check is `kind`'s.
    unsafe {
        let mut start = 0isize;
        let mut end = 0isize;
        let ok = match kind {
            ErrorKind::Encode => {
                unicode::PyUnicodeEncodeError_GetStart(exc, &mut start) == 0
                    && unicode::PyUnicodeEncodeError_GetEnd(exc, &mut end) == 0
            }
            ErrorKind::Decode => {
                unicode::PyUnicodeDecodeError_GetStart(exc, &mut start) == 0
                    && unicode::PyUnicodeDecodeError_GetEnd(exc, &mut end) == 0
            }
            ErrorKind::Translate => {
                unicode::PyUnicodeTranslateError_GetStart(exc, &mut start) == 0
                    && unicode::PyUnicodeTranslateError_GetEnd(exc, &mut end) == 0
            }
        };
        if ok { Some((start, end)) } else { None }
    }
}

/// The handler result's object field (a str for encode/translate, bytes
/// for decode), released before returning.
///
/// # Safety
///
/// `exc` must be one of the three Unicode error instances.
unsafe fn error_object(exc: *mut PyObject, kind: &ErrorKind) -> Option<*mut PyObject> {
    // SAFETY: as `error_range`.
    unsafe {
        let object = match kind {
            ErrorKind::Encode => unicode::PyUnicodeEncodeError_GetObject(exc),
            ErrorKind::Decode => unicode::PyUnicodeDecodeError_GetObject(exc),
            ErrorKind::Translate => unicode::PyUnicodeTranslateError_GetObject(exc),
        };
        if object.is_null() { None } else { Some(object) }
    }
}

/// `PyCodec_StrictErrors`: re-raise the exception.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_StrictErrors(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the instance check and re-raise stay within the object.
    unsafe {
        if unicode::is_exception_instance(exc, exceptions::base_exception()) {
            let class = header::type_of(exc).cast::<PyObject>();
            // `PyErr_SetObject` borrows the value.
            exceptions::PyErr_SetObject(class, exc);
        } else {
            tstate::set_string(
                exceptions::type_error(),
                b"codec must pass exception instance",
            );
        }
        core::ptr::null_mut()
    }
}

/// `PyCodec_IgnoreErrors`: an empty replacement and the error's end.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_IgnoreErrors(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field reads stay within the exception instance.
    unsafe {
        let Some(kind) = error_kind(exc) else {
            wrong_exception_type(exc);
            return core::ptr::null_mut();
        };
        let Some((_, end)) = error_range(exc, &kind) else {
            return core::ptr::null_mut();
        };
        handler_tuple(unicode::empty(), end)
    }
}

/// `PyCodec_ReplaceErrors`.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_ReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field reads stay within the exception instance.
    unsafe {
        let Some(kind) = error_kind(exc) else {
            wrong_exception_type(exc);
            return core::ptr::null_mut();
        };
        let Some((start, end)) = error_range(exc, &kind) else {
            return core::ptr::null_mut();
        };
        let replacement = match kind {
            ErrorKind::Encode | ErrorKind::Translate => {
                let chars = vec![b'?' as u32; (end - start).max(0) as usize];
                unicode::chars_to_object(&chars, b'?' as u32)
            }
            // `Py_BuildValue("(Cn)", 0xFFFD, end)`: one character.
            ErrorKind::Decode => unicode::chars_to_object(&[0xFFFD], 0xFFFD),
        };
        if replacement.is_null() {
            return core::ptr::null_mut();
        }
        handler_tuple(replacement, end)
    }
}

/// `PyCodec_XMLCharRefReplaceErrors`: `&#N;` per character; decode
/// errors cannot take it.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_XMLCharRefReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field reads stay within the exception instance.
    unsafe {
        let Some(kind) = error_kind(exc) else {
            wrong_exception_type(exc);
            return core::ptr::null_mut();
        };
        if !matches!(kind, ErrorKind::Encode) {
            wrong_exception_type(exc);
            return core::ptr::null_mut();
        }
        let Some((start, end)) = error_range(exc, &kind) else {
            return core::ptr::null_mut();
        };
        let Some(object) = error_object(exc, &kind) else {
            return core::ptr::null_mut();
        };
        let length = unicode::PyUnicode_GetLength(object);
        let mut chars = Vec::new();
        let mut i = start.max(0);
        while i < end.min(length) {
            chars.push(unicode::PyUnicode_ReadChar(object, i));
            i += 1;
        }
        exceptions::release_object(object);
        let mut out = String::new();
        for ch in chars {
            out.push_str(&format!("&#{};", ch));
        }
        let replacement = unicode::str_from_utf8(&out);
        if replacement.is_null() {
            return core::ptr::null_mut();
        }
        handler_tuple(replacement, end)
    }
}

/// `PyCodec_BackslashReplaceErrors`.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_BackslashReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field reads stay within the exception instance.
    unsafe {
        let Some(kind) = error_kind(exc) else {
            wrong_exception_type(exc);
            return core::ptr::null_mut();
        };
        let Some((start, end)) = error_range(exc, &kind) else {
            return core::ptr::null_mut();
        };
        let Some(object) = error_object(exc, &kind) else {
            return core::ptr::null_mut();
        };
        let mut out = Vec::new();
        if let ErrorKind::Decode = kind {
            // `object` is the bytes the decoder failed on.
            let (size, data) = rt_bytes::as_string_parts(object);
            let bytes = core::slice::from_raw_parts(data.cast::<u8>(), size.max(0) as usize);
            let mut i = start.max(0) as usize;
            while i < end.max(0) as usize && i < bytes.len() {
                out.push(b'\\');
                out.push(b'x');
                out.push(unicode::HEX_DIGITS[(bytes[i] >> 4) as usize] as u8);
                out.push(unicode::HEX_DIGITS[(bytes[i] & 0xf) as usize] as u8);
                i += 1;
            }
        } else {
            let length = unicode::PyUnicode_GetLength(object);
            let mut i = start.max(0);
            while i < end.min(length) {
                let ch = unicode::PyUnicode_ReadChar(object, i);
                out.push(b'\\');
                if ch >= 0x10000 {
                    out.push(b'U');
                    out.push(unicode::HEX_DIGITS[((ch >> 28) & 0xf) as usize]);
                    out.push(unicode::HEX_DIGITS[((ch >> 24) & 0xf) as usize]);
                    out.push(unicode::HEX_DIGITS[((ch >> 20) & 0xf) as usize]);
                    out.push(unicode::HEX_DIGITS[((ch >> 16) & 0xf) as usize]);
                    out.push(unicode::HEX_DIGITS[((ch >> 12) & 0xf) as usize]);
                } else if ch >= 0x100 {
                    out.push(b'u');
                    out.push(unicode::HEX_DIGITS[((ch >> 12) & 0xf) as usize]);
                } else {
                    out.push(b'x');
                }
                out.push(unicode::HEX_DIGITS[((ch >> 4) & 0xf) as usize]);
                out.push(unicode::HEX_DIGITS[(ch & 0xf) as usize]);
                i += 1;
            }
        }
        exceptions::release_object(object);
        let replacement = match core::str::from_utf8(&out) {
            Ok(text) => unicode::str_from_utf8(text),
            Err(_) => {
                tstate::set_string(
                    exceptions::system_error(),
                    b"coil: the escape text is not UTF-8",
                );
                return core::ptr::null_mut();
            }
        };
        if replacement.is_null() {
            return core::ptr::null_mut();
        }
        handler_tuple(replacement, end)
    }
}

/// `PyCodec_NameReplaceErrors`: needs the unicodedata name tables the
/// runtime resolves through `unicodedata.pyd`.
///
/// # Safety
///
/// `exc` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_NameReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    let _ = exc;
    tstate::set_string(
        exceptions::system_error(),
        b"coil: the 'namereplace' error handler needs the unicodedata name tables",
    );
    core::ptr::null_mut()
}

// ------------------------------------------------------ the module surface

/// `codec_tuple`: the `(result, length)` pair every module codec
/// function returns.
fn codec_tuple(decoded: *mut PyObject, len: isize) -> *mut PyObject {
    if decoded.is_null() {
        return core::ptr::null_mut();
    }
    let length = long::PyLong_FromSsize_t(len);
    if length.is_null() {
        // SAFETY: our reference, created a moment ago.
        unsafe { exceptions::release_object(decoded) };
        return core::ptr::null_mut();
    }
    tuple::from_array(&[decoded, length])
}

/// A decoder's shared body: run the decode, then the module tuple with
/// the consumed length the stateful form reports.
fn decode_tuple(
    decoded: Option<Vec<u32>>,
    consumed: Option<&mut isize>,
    len: isize,
) -> *mut PyObject {
    let Some(chars) = decoded else {
        return core::ptr::null_mut();
    };
    let reported = match consumed {
        Some(value) => *value,
        None => len,
    };
    let object = unicode::chars_to_object(&chars, 0);
    codec_tuple(object, reported)
}

/// An encoder's shared body: the module tuple with the input length.
fn encode_tuple(encoded: Option<Vec<u8>>, len: isize) -> *mut PyObject {
    let Some(out) = encoded else {
        return core::ptr::null_mut();
    };
    let object = match rt_bytes::from_vec(Some(out)) {
        Some(object) => object,
        None => return core::ptr::null_mut(),
    };
    codec_tuple(object, len)
}

/// `_codecs.escape_decode`: bytes-literal escape processing over a
/// buffer or a str's UTF-8 bytes.
pub fn escape_decode(data: &[u8], errors: Option<&'static str>) -> *mut PyObject {
    let decoded = rt_bytes::decode_escape(data, errors);
    let Some(out) = decoded else {
        return core::ptr::null_mut();
    };
    let object = match rt_bytes::from_vec(Some(out)) {
        Some(object) => object,
        None => return core::ptr::null_mut(),
    };
    codec_tuple(object, data.len() as isize)
}

/// `_codecs.escape_encode`: the bytes repr-style escaping.
pub fn escape_encode(data: &[u8], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    // CPython's OverflowError for absurd sizes.
    if data.len() as isize > isize::MAX / 4 {
        tstate::set_string(
            exceptions::overflow_error(),
            b"string is too large to encode",
        );
        return core::ptr::null_mut();
    }
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        match c {
            b'\'' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x00..=0x1F | 0x7F..=0xFF => {
                out.push(b'\\');
                out.push(b'x');
                out.push(unicode::HEX_DIGITS[(c >> 4) as usize]);
                out.push(unicode::HEX_DIGITS[(c & 0xf) as usize]);
            }
            _ => out.push(c),
        }
    }
    encode_tuple(Some(out), data.len() as isize)
}

/// `_codecs.readbuffer_encode`: a copy of the buffer.
pub fn readbuffer_encode(data: &[u8], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    encode_tuple(Some(data.to_vec()), data.len() as isize)
}

/// `_codecs.utf_7_decode`.
pub fn utf_7_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_utf7_bytes(
        data,
        unicode::error_handler(errors),
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.utf_8_decode`.
pub fn utf_8_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_utf8_bytes(
        data,
        unicode::error_handler(errors),
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// The `_codecs.utf_16_decode` family's shared body.
fn utf_16_decode_with(
    data: &[u8],
    errors: Option<&'static str>,
    byteorder: i32,
    final_: bool,
) -> Option<(Vec<u32>, isize, i32)> {
    let mut order = byteorder;
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_utf16_bytes(
        data,
        unicode::error_handler(errors),
        Some(&mut order),
        if final_ { None } else { Some(&mut consumed) },
    );
    decoded.map(|chars| (chars, consumed, order))
}

/// `_codecs.utf_16_decode`.
pub fn utf_16_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_16_decode_with(data, errors, 0, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_16_le_decode`.
pub fn utf_16_le_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_16_decode_with(data, errors, -1, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_16_be_decode`.
pub fn utf_16_be_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_16_decode_with(data, errors, 1, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_16_ex_decode`: the 3-tuple with the byte order.
pub fn utf_16_ex_decode(
    data: &[u8],
    errors: Option<&'static str>,
    byteorder: i32,
    final_: bool,
) -> *mut PyObject {
    let Some((chars, consumed, order)) = utf_16_decode_with(data, errors, byteorder, final_) else {
        return core::ptr::null_mut();
    };
    let reported = if final_ {
        data.len() as isize
    } else {
        consumed
    };
    let decoded = unicode::chars_to_object(&chars, 0);
    if decoded.is_null() {
        return core::ptr::null_mut();
    }
    let length = long::PyLong_FromSsize_t(reported);
    let order_object = long::PyLong_FromLong(order);
    if length.is_null() || order_object.is_null() {
        // SAFETY: our references, created a moment ago.
        unsafe {
            exceptions::release_object(decoded);
            exceptions::release_object(length);
            exceptions::release_object(order_object);
        }
        return core::ptr::null_mut();
    }
    tuple::from_array(&[decoded, length, order_object])
}

/// The `_codecs.utf_32_decode` family's shared body.
fn utf_32_decode_with(
    data: &[u8],
    errors: Option<&'static str>,
    byteorder: i32,
    final_: bool,
) -> Option<(Vec<u32>, isize, i32)> {
    let mut order = byteorder;
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_utf32_bytes(
        data,
        unicode::error_handler(errors),
        &mut order,
        if final_ { None } else { Some(&mut consumed) },
    );
    decoded.map(|chars| (chars, consumed, order))
}

/// `_codecs.utf_32_decode`.
pub fn utf_32_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_32_decode_with(data, errors, 0, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_32_le_decode`.
pub fn utf_32_le_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_32_decode_with(data, errors, -1, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_32_be_decode`.
pub fn utf_32_be_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    match utf_32_decode_with(data, errors, 1, final_) {
        Some((chars, consumed, _)) => {
            let reported = if final_ {
                data.len() as isize
            } else {
                consumed
            };
            codec_tuple(unicode::chars_to_object(&chars, 0), reported)
        }
        None => core::ptr::null_mut(),
    }
}

/// `_codecs.utf_32_ex_decode`: the 3-tuple with the byte order.
pub fn utf_32_ex_decode(
    data: &[u8],
    errors: Option<&'static str>,
    byteorder: i32,
    final_: bool,
) -> *mut PyObject {
    let Some((chars, consumed, order)) = utf_32_decode_with(data, errors, byteorder, final_) else {
        return core::ptr::null_mut();
    };
    let reported = if final_ {
        data.len() as isize
    } else {
        consumed
    };
    let decoded = unicode::chars_to_object(&chars, 0);
    if decoded.is_null() {
        return core::ptr::null_mut();
    }
    let length = long::PyLong_FromSsize_t(reported);
    let order_object = long::PyLong_FromLong(order);
    if length.is_null() || order_object.is_null() {
        // SAFETY: our references, created a moment ago.
        unsafe {
            exceptions::release_object(decoded);
            exceptions::release_object(length);
            exceptions::release_object(order_object);
        }
        return core::ptr::null_mut();
    }
    tuple::from_array(&[decoded, length, order_object])
}

/// `_codecs.unicode_escape_decode`.
pub fn unicode_escape_decode(
    data: &[u8],
    errors: Option<&'static str>,
    final_: bool,
) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_unicode_escape(
        data,
        unicode::error_handler(errors),
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.raw_unicode_escape_decode`.
pub fn raw_unicode_escape_decode(
    data: &[u8],
    errors: Option<&'static str>,
    final_: bool,
) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = unicode::decode_raw_unicode_escape(
        data,
        unicode::error_handler(errors),
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.latin_1_decode`.
pub fn latin_1_decode(data: &[u8], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    let chars: Vec<u32> = data.iter().map(|b| *b as u32).collect();
    codec_tuple(unicode::chars_to_object(&chars, 0), data.len() as isize)
}

/// `_codecs.ascii_decode`.
pub fn ascii_decode(data: &[u8], errors: Option<&'static str>) -> *mut PyObject {
    let chars = unicode::decode_ascii_bytes(data, unicode::error_handler(errors));
    decode_tuple(chars, None, data.len() as isize)
}

/// `_codecs.charmap_decode`.
pub fn charmap_decode(
    data: &[u8],
    errors: Option<&'static str>,
    mapping: *mut PyObject,
) -> *mut PyObject {
    let mapping = if core::ptr::eq(mapping, singletons::none()) {
        core::ptr::null_mut()
    } else {
        mapping
    };
    let chars = unicode::decode_charmap_bytes(data, mapping, unicode::error_handler(errors));
    decode_tuple(chars, None, data.len() as isize)
}

/// `_codecs.charmap_build`.
///
/// # Safety
///
/// `map` must be a valid object.
pub unsafe fn charmap_build(map: *mut PyObject) -> *mut PyObject {
    // SAFETY: the runtime's C export checks its own argument.
    unsafe { unicode::PyUnicode_BuildEncodingMap(map) }
}

/// `_codecs.utf_7_encode`.
pub fn utf_7_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    let out = unicode::encode_utf7_bytes(chars, false, false);
    encode_tuple(Some(out), chars.len() as isize)
}

/// `_codecs.utf_8_encode`.
pub fn utf_8_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let out = unicode::encode_utf8_bytes(chars, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.utf_16_encode` with the byteorder argument.
pub fn utf_16_encode(chars: &[u32], errors: Option<&'static str>, byteorder: i32) -> *mut PyObject {
    let out = unicode::encode_utf16_bytes(chars, byteorder, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.utf_16_le_encode`.
pub fn utf_16_le_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    utf_16_encode(chars, errors, -1)
}

/// `_codecs.utf_16_be_encode`.
pub fn utf_16_be_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    utf_16_encode(chars, errors, 1)
}

/// `_codecs.utf_32_encode` with the byteorder argument.
pub fn utf_32_encode(chars: &[u32], errors: Option<&'static str>, byteorder: i32) -> *mut PyObject {
    let out = unicode::encode_utf32_bytes(chars, byteorder, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.utf_32_le_encode`.
pub fn utf_32_le_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    utf_32_encode(chars, errors, -1)
}

/// `_codecs.utf_32_be_encode`.
pub fn utf_32_be_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    utf_32_encode(chars, errors, 1)
}

/// `_codecs.unicode_escape_encode`.
pub fn unicode_escape_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    let out = unicode::encode_by_name("unicode_escape", chars, unicode::ErrHandler::Strict);
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.raw_unicode_escape_encode`.
pub fn raw_unicode_escape_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let _ = errors;
    let out = unicode::encode_by_name("raw_unicode_escape", chars, unicode::ErrHandler::Strict);
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.latin_1_encode`.
pub fn latin_1_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let out = unicode::encode_ucs1(chars, 0x100, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.ascii_encode`.
pub fn ascii_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let out = unicode::encode_ucs1(chars, 0x80, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.charmap_encode`.
pub fn charmap_encode(
    chars: &[u32],
    errors: Option<&'static str>,
    mapping: *mut PyObject,
) -> *mut PyObject {
    let mapping = if core::ptr::eq(mapping, singletons::none()) {
        core::ptr::null_mut()
    } else {
        mapping
    };
    let out = unicode::encode_charmap_bytes(chars, mapping, unicode::error_handler(errors));
    encode_tuple(out, chars.len() as isize)
}

// -------------------------------------------------- Windows code page codecs

/// The `mbcs`, `oem` and explicit code-page codecs: a port of
/// `unicodeobject.c`'s `decode_code_page_stateful`/`encode_code_page`
/// over the `kernel32` conversions. CPython builds these only on
/// Windows, and so does this runtime.
#[cfg(target_os = "windows")]
mod code_page {
    use super::*;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            in_ptr: *const u8,
            in_len: i32,
            out_ptr: *mut u16,
            out_len: i32,
        ) -> i32;
        fn WideCharToMultiByte(
            code_page: u32,
            flags: u32,
            in_ptr: *const u16,
            in_len: i32,
            out_ptr: *mut u8,
            out_len: i32,
            default_char: *const u8,
            used_default_char: *mut i32,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    const CP_ACP: u32 = 0;
    const CP_UTF7: u32 = 65000;
    const CP_UTF8: u32 = 65001;
    const MB_ERR_INVALID_CHARS: u32 = 0x0000_0008;
    const WC_ERR_INVALID_CHARS: u32 = 0x0000_0080;
    const WC_NO_BEST_FIT_CHARS: u32 = 0x0000_0400;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_INVALID_FLAGS: u32 = 1004;
    const ERROR_NO_UNICODE_TRANSLATION: u32 = 1116;
    /// CPython works in `INT_MAX/4` chunks to stay inside the
    /// conversions' int lengths.
    const CHUNK_SIZE: isize = (i32::MAX / 4) as isize;

    /// `code_page_name`: the name a codec exception reports.
    fn code_page_name(code_page: u32) -> String {
        if code_page == CP_ACP {
            "mbcs".to_string()
        } else if code_page == CP_UTF7 {
            "CP_UTF7".to_string()
        } else if code_page == CP_UTF8 {
            "CP_UTF8".to_string()
        } else {
            format!("cp{code_page}")
        }
    }

    /// The sidecar message for CPython's `PyErr_SetFromWindowsErr(0)`;
    /// `FormatMessage` text needs the locale machinery.
    fn os_error() {
        tstate::set_string(
            exceptions::os_error(),
            b"codec failed: the Windows conversion reported an error",
        );
    }

    /// `widechar_resize` on the input side is unnecessary here: the
    /// sizing call returns the exact length before the real one.
    /// One strict conversion over a chunk: `Err(())` is the
    /// `ERROR_NO_UNICODE_TRANSLATION` the error-handler pass handles,
    /// `Err(())` with an OSError raised is any other failure.
    fn decode_chunk_strict(
        code_page: u32,
        out: &mut Vec<u32>,
        input: &[u8],
    ) -> Result<usize, DecodeFailure> {
        let mut flags = MB_ERR_INVALID_CHARS;
        // SAFETY: a null out pointer only sizes the conversion.
        let mut size = unsafe {
            MultiByteToWideChar(
                code_page,
                flags,
                input.as_ptr(),
                input.len() as i32,
                core::ptr::null_mut(),
                0,
            )
        };
        while size <= 0 {
            // SAFETY: the failure code is thread-local kernel32 state.
            let err = unsafe { GetLastError() };
            if flags == 0 || err != ERROR_INVALID_FLAGS {
                return Err(match err {
                    ERROR_NO_UNICODE_TRANSLATION => DecodeFailure::Translation,
                    _ => {
                        os_error();
                        DecodeFailure::Os
                    }
                });
            }
            // Some code pages (e.g. UTF-7) accept only flags 0.
            flags = 0;
            // SAFETY: as the sizing call.
            size = unsafe {
                MultiByteToWideChar(
                    code_page,
                    flags,
                    input.as_ptr(),
                    input.len() as i32,
                    core::ptr::null_mut(),
                    0,
                )
            };
        }
        let mut units = vec![0u16; size as usize];
        // SAFETY: the buffer holds `size` units.
        let written = unsafe {
            MultiByteToWideChar(
                code_page,
                flags,
                input.as_ptr(),
                input.len() as i32,
                units.as_mut_ptr(),
                size,
            )
        };
        if written <= 0 {
            // SAFETY: as the conversion.
            let err = unsafe { GetLastError() };
            return Err(match err {
                ERROR_NO_UNICODE_TRANSLATION => DecodeFailure::Translation,
                _ => {
                    os_error();
                    DecodeFailure::Os
                }
            });
        }
        units.truncate(written as usize);
        out.extend(units.iter().map(|&u| u as u32));
        Ok(input.len())
    }

    enum DecodeFailure {
        Translation,
        Os,
    }

    /// `decode_code_page_errors`: byte-at-a-time conversion with the
    /// error handler; `Ok(consumed)` or the pending error as `Err`.
    fn decode_chunk_errors(
        code_page: u32,
        out: &mut Vec<u32>,
        input: &[u8],
        errors: Option<&'static str>,
        final_: bool,
    ) -> Result<usize, ()> {
        let encoding = code_page_name(code_page);
        let reason = "No mapping for the Unicode character exists in the target code page.";
        let handler = unicode::error_handler(errors);
        // CPython's strict handler raises over the whole buffer with an
        // empty position before the byte loop runs.
        if matches!(handler, unicode::ErrHandler::Strict) && final_ {
            unicode::raise_decode_error(&encoding, input, 0, 0, reason);
            return Err(());
        }
        let mut pos = 0usize;
        while pos < input.len() {
            // A character may be a surrogate pair, and the try lengths
            // run to UTF-8's four bytes.
            let mut flags = MB_ERR_INVALID_CHARS;
            let mut insize = 1usize;
            let mut buffer = [0u16; 2];
            let mut written;
            loop {
                // SAFETY: `insize` bytes from `pos` stay within the chunk.
                written = unsafe {
                    MultiByteToWideChar(
                        code_page,
                        flags,
                        input.as_ptr().add(pos),
                        insize as i32,
                        buffer.as_mut_ptr(),
                        2,
                    )
                };
                if written > 0 {
                    break;
                }
                // SAFETY: the failure code is thread-local kernel32 state.
                let err = unsafe { GetLastError() };
                if err == ERROR_INVALID_FLAGS && flags != 0 {
                    flags = 0;
                    continue;
                }
                if err != ERROR_NO_UNICODE_TRANSLATION && err != ERROR_INSUFFICIENT_BUFFER {
                    os_error();
                    return Err(());
                }
                insize += 1;
                if insize > 4 || pos + insize > input.len() {
                    break;
                }
            }
            if written > 0 {
                for &unit in &buffer[..written as usize] {
                    out.push(unit as u32);
                }
                pos += insize;
                continue;
            }
            // An undecodable byte. A trailing partial sequence holds for
            // the next chunk unless this is the final one.
            if pos + insize >= input.len() && !final_ {
                break;
            }
            match unicode::generic_decode_error(
                out,
                input,
                pos as isize,
                pos as isize + 1,
                handler,
                &encoding,
                reason,
            ) {
                Some(newpos) => pos = newpos.max(0) as usize,
                None => return Err(()),
            }
        }
        Ok(pos)
    }

    /// `decode_code_page_stateful`.
    pub fn decode_stateful(
        code_page: i32,
        data: &[u8],
        errors: Option<&'static str>,
        mut consumed: Option<&mut isize>,
    ) -> Option<Vec<u32>> {
        if code_page < 0 {
            tstate::set_string(exceptions::value_error(), b"invalid code page number");
            return None;
        }
        let code_page = code_page as u32;
        let mut out = Vec::new();
        let mut remaining = data;
        let final_decode = consumed.is_none();
        if let Some(slot) = consumed.as_deref_mut() {
            *slot = 0;
        }
        loop {
            let chunk_len = remaining.len().min(CHUNK_SIZE as usize);
            let chunk = &remaining[..chunk_len];
            let final_chunk = final_decode && chunk_len == remaining.len();
            if chunk.is_empty() {
                break;
            }
            let converted = match decode_chunk_strict(code_page, &mut out, chunk) {
                Ok(converted) => converted,
                Err(DecodeFailure::Os) => return None,
                Err(DecodeFailure::Translation) => {
                    match decode_chunk_errors(code_page, &mut out, chunk, errors, final_chunk) {
                        Ok(converted) => converted,
                        Err(()) => return None,
                    }
                }
            };
            if let Some(slot) = consumed.as_deref_mut() {
                *slot += converted as isize;
            }
            remaining = &remaining[converted..];
            if remaining.is_empty() {
                break;
            }
        }
        Some(out)
    }

    /// `encode_code_page_flags`.
    fn encode_flags(code_page: u32, errors: Option<&str>) -> u32 {
        if code_page == CP_UTF8 {
            WC_ERR_INVALID_CHARS
        } else if code_page == CP_UTF7 || errors == Some("replace") {
            0
        } else {
            WC_NO_BEST_FIT_CHARS
        }
    }

    /// `encode_code_page_strict` over the whole input: `Err(())` is the
    /// unmappable-character failure the handler pass takes, an OSError
    /// is already raised.
    fn encode_strict(
        code_page: u32,
        out: &mut Vec<u8>,
        units: &[u16],
    ) -> Result<(), EncodeFailure> {
        let flags = encode_flags(code_page, None);
        let tracks_default = code_page != CP_UTF8 && code_page != CP_UTF7;
        let mut used_default = 0i32;
        // SAFETY: a null out pointer only sizes the conversion.
        let size = unsafe {
            WideCharToMultiByte(
                code_page,
                flags,
                units.as_ptr(),
                units.len() as i32,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
                if tracks_default {
                    &raw mut used_default
                } else {
                    core::ptr::null_mut()
                },
            )
        };
        if size <= 0 {
            return Err(encode_failure());
        }
        if used_default != 0 {
            return Err(EncodeFailure::Unmappable);
        }
        let mut bytes = vec![0u8; size as usize];
        // SAFETY: the buffer holds `size` bytes.
        let written = unsafe {
            WideCharToMultiByte(
                code_page,
                flags,
                units.as_ptr(),
                units.len() as i32,
                bytes.as_mut_ptr(),
                size,
                core::ptr::null(),
                if tracks_default {
                    &raw mut used_default
                } else {
                    core::ptr::null_mut()
                },
            )
        };
        if written <= 0 {
            return Err(encode_failure());
        }
        if used_default != 0 {
            return Err(EncodeFailure::Unmappable);
        }
        bytes.truncate(written as usize);
        out.extend_from_slice(&bytes);
        Ok(())
    }

    enum EncodeFailure {
        Unmappable,
        Os,
    }

    /// The `Err` for a failed conversion: the translation error the
    /// handler pass handles, or the OSError already raised.
    fn encode_failure() -> EncodeFailure {
        // SAFETY: the failure code is thread-local kernel32 state.
        let err = unsafe { GetLastError() };
        if err == ERROR_NO_UNICODE_TRANSLATION {
            EncodeFailure::Unmappable
        } else {
            os_error();
            EncodeFailure::Os
        }
    }

    /// The handler pass of `encode_code_page_errors`: one character at
    /// a time, the replacement's bytes appended raw and its characters
    /// required to be ASCII.
    fn encode_errors(
        code_page: u32,
        out: &mut Vec<u8>,
        chars: &[u32],
        errors: Option<&'static str>,
    ) -> Result<(), ()> {
        let encoding = code_page_name(code_page);
        let reason = "invalid character";
        let handler = unicode::error_handler(errors);
        let flags = encode_flags(code_page, errors);
        let tracks_default = code_page != CP_UTF8 && code_page != CP_UTF7;
        let mut pos = 0usize;
        while pos < chars.len() {
            let ch = chars[pos];
            let units: [u16; 2] = if ch < 0x10000 {
                [ch as u16, 0]
            } else {
                [
                    (0xD800 + ((ch - 0x10000) >> 10)) as u16,
                    (0xDC00 + ((ch - 0x10000) & 0x3FF)) as u16,
                ]
            };
            let unit_len = if ch < 0x10000 { 1 } else { 2 };
            let mut buffer = [0u8; 4];
            let mut used_default = 0i32;
            // SAFETY: `units` holds one character and `buffer` four
            // bytes, the maximum one character converts to.
            let written = unsafe {
                WideCharToMultiByte(
                    code_page,
                    flags,
                    units.as_ptr(),
                    unit_len,
                    buffer.as_mut_ptr(),
                    4,
                    core::ptr::null(),
                    if tracks_default {
                        &raw mut used_default
                    } else {
                        core::ptr::null_mut()
                    },
                )
            };
            if written > 0 && !(tracks_default && used_default != 0) {
                out.extend_from_slice(&buffer[..written as usize]);
                pos += 1;
                continue;
            }
            if written <= 0 {
                // SAFETY: the failure code is thread-local kernel32 state.
                let err = unsafe { GetLastError() };
                if err != ERROR_NO_UNICODE_TRANSLATION {
                    os_error();
                    return Err(());
                }
            }
            // The character failed: one handler step over it.
            match handler {
                unicode::ErrHandler::Strict => {
                    // CPython's strict encode error reports the whole
                    // string with an empty position.
                    unicode::raise_encode_error(&encoding, chars, 0, 0, reason);
                    return Err(());
                }
                unicode::ErrHandler::SurrogatePass => {
                    // Not a standard encoding name, so `surrogatepass`
                    // re-raises over the character.
                    unicode::raise_encode_error(
                        &encoding,
                        chars,
                        pos as isize,
                        pos as isize + 1,
                        reason,
                    );
                    return Err(());
                }
                unicode::ErrHandler::Ignore => pos += 1,
                unicode::ErrHandler::Replace => {
                    out.push(b'?');
                    pos += 1;
                }
                unicode::ErrHandler::BackslashReplace => {
                    let escape = if ch < 0x100 {
                        format!("\\x{ch:02x}")
                    } else if ch < 0x10000 {
                        format!("\\u{ch:04x}")
                    } else {
                        format!("\\U{ch:08x}")
                    };
                    out.extend_from_slice(escape.as_bytes());
                    pos += 1;
                }
                unicode::ErrHandler::XmlCharRefReplace => {
                    out.extend_from_slice(format!("&#{};", ch).as_bytes());
                    pos += 1;
                }
                unicode::ErrHandler::SurrogateEscape => {
                    if !(0xDC80..=0xDCFF).contains(&ch) {
                        unicode::raise_encode_error(
                            &encoding,
                            chars,
                            pos as isize,
                            pos as isize + 1,
                            reason,
                        );
                        return Err(());
                    }
                    out.push((ch - 0xDC00) as u8);
                    pos += 1;
                }
                unicode::ErrHandler::Named(name) => {
                    unicode::named_handler_error(name);
                    return Err(());
                }
            }
        }
        Ok(())
    }

    /// `encode_code_page`.
    pub fn encode(code_page: i32, chars: &[u32], errors: Option<&'static str>) -> Option<Vec<u8>> {
        if code_page < 0 {
            tstate::set_string(exceptions::value_error(), b"invalid code page number");
            return None;
        }
        if chars.is_empty() {
            return Some(Vec::new());
        }
        let code_page = code_page as u32;
        let mut out = Vec::new();
        let mut offset = 0usize;
        while offset < chars.len() {
            let chunk = &chars[offset..(offset + chars.len() - offset).min(CHUNK_SIZE as usize)];
            let units: Vec<u16> = chunk
                .iter()
                .flat_map(|&ch| {
                    if ch < 0x10000 {
                        vec![ch as u16]
                    } else {
                        vec![
                            (0xD800 + ((ch - 0x10000) >> 10)) as u16,
                            (0xDC00 + ((ch - 0x10000) & 0x3FF)) as u16,
                        ]
                    }
                })
                .collect();
            match encode_strict(code_page, &mut out, &units) {
                Ok(()) => {}
                Err(EncodeFailure::Os) => return None,
                Err(EncodeFailure::Unmappable) => {
                    encode_errors(code_page, &mut out, chunk, errors).ok()?;
                }
            }
            offset += chunk.len();
        }
        Some(out)
    }
}

#[cfg(target_os = "windows")]
use code_page::{decode_stateful, encode};

/// `PyUnicode_DecodeCodePageStateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeCodePageStateful(
    code_page: c_int,
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: the byte range and C string are the caller's contract.
    unsafe {
        install();
        let data = core::slice::from_raw_parts(s.cast::<u8>(), size.max(0) as usize);
        let errors = unicode::errors_arg(errors);
        let decoded = if consumed.is_null() {
            decode_stateful(code_page, data, errors, None)
        } else {
            decode_stateful(code_page, data, errors, Some(&mut *consumed))
        };
        match decoded {
            Some(chars) => unicode::chars_to_object(&chars, 0),
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_DecodeMBCSStateful`.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeMBCSStateful(
    s: *const c_char,
    size: isize,
    errors: *const c_char,
    consumed: *mut isize,
) -> *mut PyObject {
    // SAFETY: as `PyUnicode_DecodeCodePageStateful`.
    unsafe { PyUnicode_DecodeCodePageStateful(CP_ACP as c_int, s, size, errors, consumed) }
}

/// `PyUnicode_DecodeMBCS`: the final (non-stateful) form.
///
/// # Safety
///
/// `s` must point to `size` bytes.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeMBCS(s: *const c_char, size: isize) -> *mut PyObject {
    // SAFETY: as `PyUnicode_DecodeCodePageStateful`.
    unsafe { PyUnicode_DecodeMBCSStateful(s, size, core::ptr::null(), core::ptr::null_mut()) }
}

/// `PyUnicode_EncodeCodePage`.
///
/// # Safety
///
/// `unicode` must be a str object.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeCodePage(
    code_page: c_int,
    unicode: *mut PyObject,
    errors: *const c_char,
) -> *mut PyObject {
    // SAFETY: the str contents read stays within the object.
    unsafe {
        install();
        if !unicode::check(unicode) {
            tstate::set_string(
                exceptions::type_error(),
                b"bad argument type for built-in operation",
            );
            return core::ptr::null_mut();
        }
        let Some(chars) = unicode::as_chars(unicode) else {
            return core::ptr::null_mut();
        };
        let out = encode(code_page, &chars.as_vec(), unicode::errors_arg(errors));
        match out {
            Some(out) => match rt_bytes::from_vec(Some(out)) {
                Some(object) => object,
                None => core::ptr::null_mut(),
            },
            None => core::ptr::null_mut(),
        }
    }
}

/// `PyUnicode_AsMBCSString`.
///
/// # Safety
///
/// `unicode` must be a str object.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsMBCSString(unicode: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `PyUnicode_EncodeCodePage`.
    unsafe { PyUnicode_EncodeCodePage(CP_ACP as c_int, unicode, core::ptr::null()) }
}

/// The code page constants the module functions need; `CP_OEMCP` has
/// no numeric value exposed to user code.
#[cfg(target_os = "windows")]
const CP_ACP: u32 = 0;
#[cfg(target_os = "windows")]
const CP_OEMCP: u32 = 1;

/// `_codecs.mbcs_decode`.
#[cfg(target_os = "windows")]
pub fn mbcs_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = decode_stateful(
        CP_ACP as i32,
        data,
        errors,
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.oem_decode`.
#[cfg(target_os = "windows")]
pub fn oem_decode(data: &[u8], errors: Option<&'static str>, final_: bool) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = decode_stateful(
        CP_OEMCP as i32,
        data,
        errors,
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.code_page_decode`.
#[cfg(target_os = "windows")]
pub fn code_page_decode(
    code_page: i32,
    data: &[u8],
    errors: Option<&'static str>,
    final_: bool,
) -> *mut PyObject {
    let mut consumed = data.len() as isize;
    let decoded = decode_stateful(
        code_page,
        data,
        errors,
        if final_ { None } else { Some(&mut consumed) },
    );
    decode_tuple(
        decoded,
        if final_ { None } else { Some(&mut consumed) },
        data.len() as isize,
    )
}

/// `_codecs.mbcs_encode`.
#[cfg(target_os = "windows")]
pub fn mbcs_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let out = encode(CP_ACP as i32, chars, errors);
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.oem_encode`.
#[cfg(target_os = "windows")]
pub fn oem_encode(chars: &[u32], errors: Option<&'static str>) -> *mut PyObject {
    let out = encode(CP_OEMCP as i32, chars, errors);
    encode_tuple(out, chars.len() as isize)
}

/// `_codecs.code_page_encode`.
#[cfg(target_os = "windows")]
pub fn code_page_encode(
    code_page: i32,
    chars: &[u32],
    errors: Option<&'static str>,
) -> *mut PyObject {
    let out = encode(code_page, chars, errors);
    encode_tuple(out, chars.len() as isize)
}

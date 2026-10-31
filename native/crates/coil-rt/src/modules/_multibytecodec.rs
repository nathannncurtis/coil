//! The `_multibytecodec` module (spec section 10.6): the stateful
//! multibyte engine CPython's `Modules/cjkcodecs/multibytecodec.c`
//! implements, over the codec descriptors the `_codecs_*` locale
//! modules below it define. This module owns the encode/decode drivers
//! and the error protocol; `_multibytecodec.MultibyteIncrementalEncoder`
//! and friends are static types whose method bodies are the `pub unsafe
//! fn`s here, built by Rust constructors until compiled code can
//! subclass through `Lib/encodings`.
//!
//! The engine follows CPython's structure with two deliberate shape
//! changes, both invisible to the observable protocol: the output
//! buffer is an unbounded `Vec` (CPython's `MBERR_TOOSMALL` dance only
//! ever grows its own buffer and never escapes), and the per-locale
//! tables are referenced directly rather than imported through
//! `__map_*` capsules (those exist to share data across separate C
//! extensions; the Rust crate is one object).

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_DISALLOW_INSTANTIATION,
    Py_TPFLAGS_IMMUTABLETYPE, PyObject, PyTypeObject,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};
use crate::tstate;
use crate::types::exc::{self as exceptions};
use crate::types::{bytes as rt_bytes, long, tuple, unicode};
use unicode::ErrHandler;

/// `MBERR_TOOFEW`: the input ends mid-sequence.
pub const MBERR_TOOFEW: i64 = -2;
/// `MBERR_INTERNAL`: the codec hit an invariant violation.
pub const MBERR_INTERNAL: i64 = -3;
/// `MBERR_EXCEPTION`: the step function raised and returned.
pub const MBERR_EXCEPTION: i64 = -4;

/// `MAXENCPENDING`: the incremental encoder's pending character cap.
pub const MAXENCPENDING: usize = 2;
/// `MAXDECPENDING`: the incremental decoder's pending byte cap.
pub const MAXDECPENDING: usize = 8;

/// `MBENC_FLUSH`: encode everything, incomplete sequences included.
pub const MBENC_FLUSH: u32 = 0x0001;
/// `MBENC_RESET`: run the encoder's reset after the session.
pub const MBENC_RESET: u32 = 0x0002;

/// `MultibyteCodec_State`: the per-codec state bytes, reinterpreted by
/// the stateful codecs (hz, iso2022) and opaque to the engine.
#[derive(Clone, Copy, Default)]
pub struct MbState {
    pub c: [u8; 8],
}

impl MbState {
    pub const fn new() -> MbState {
        MbState { c: [0; 8] }
    }
}

/// An encoder step (`multibytecodec.h`'s `encode`): consumes characters
/// from `chars[*inpos..]`, appending output bytes and advancing
/// `*inpos`. Returns 0 when it stops cleanly, a positive count as the
/// width of an illegal sequence at `*inpos`, or `MBERR_TOOFEW` when it
/// needs characters past the end.
pub type EncodeStep =
    fn(&mut MbState, &'static MbCodec, &[u32], &mut usize, &mut Vec<u8>, u32) -> i64;

/// A decoder step (`decode`): consumes bytes from `data[*inpos..]`,
/// appending characters and advancing `*inpos`, with the same return
/// contract over bytes.
pub type DecodeStep = fn(&mut MbState, &'static MbCodec, &[u8], &mut usize, &mut Vec<u32>) -> i64;

/// An `encinit`/`decinit` step: nonzero means the init failed.
pub type InitStep = fn(&mut MbState, &'static MbCodec) -> i64;

/// An `encreset` step: appends the end-of-session bytes (an iso2022
/// escape back to ASCII, for example).
pub type EncResetStep = fn(&mut MbState, &'static MbCodec, &mut Vec<u8>) -> i64;

/// A `decreset` step.
pub type DecResetStep = fn(&mut MbState, &'static MbCodec) -> i64;

/// `struct _multibyte_codec`: one locale codec's step functions. The
/// `config` field carries the per-encoding variations the step
/// functions themselves switch on (the JIS X 0213:2000 emulation
/// flags).
pub struct MbCodec {
    pub encoding: &'static str,
    pub config: u32,
    pub encode: EncodeStep,
    pub encinit: Option<InitStep>,
    pub encreset: Option<EncResetStep>,
    pub decode: DecodeStep,
    pub decinit: Option<InitStep>,
    pub decreset: Option<DecResetStep>,
}

// -------------------------------------------------------------- drivers

/// `multibytecodec_encode`: the one-shot encode driver. `None` returns
/// with the error set. `*inpos` reports how far `chars` was consumed.
pub fn encode_run(
    codec: &'static MbCodec,
    state: &mut MbState,
    chars: &[u32],
    inpos: &mut usize,
    handler: ErrHandler,
    flags: u32,
) -> Option<Vec<u8>> {
    if chars.is_empty() && flags & MBENC_RESET == 0 {
        return Some(Vec::new());
    }
    let mut out = Vec::with_capacity(chars.len() * 2 + 16);
    while *inpos < chars.len() {
        let r = (codec.encode)(state, codec, chars, inpos, &mut out, flags);
        if r == 0 || (r == MBERR_TOOFEW && flags & MBENC_FLUSH == 0) {
            break;
        } else if !encerror(codec, state, chars, inpos, &mut out, handler, r) {
            return None;
        } else if r == MBERR_TOOFEW {
            break;
        }
    }
    if let Some(encreset) = codec.encreset
        && flags & MBENC_RESET != 0
    {
        loop {
            let r = encreset(state, codec, &mut out);
            if r == 0 {
                break;
            } else if !encerror(codec, state, chars, inpos, &mut out, handler, r) {
                return None;
            }
        }
    }
    Some(out)
}

/// `multibytecodec_decode`: the one-shot decode driver. `*inpos`
/// reports how far `data` was consumed; the one-shot surface always
/// reports the full length, but the incremental decoder reads it.
pub fn decode_run(
    codec: &'static MbCodec,
    state: &mut MbState,
    data: &[u8],
    inpos: &mut usize,
    handler: ErrHandler,
) -> Option<Vec<u32>> {
    let mut out: Vec<u32> = Vec::with_capacity(data.len());
    while *inpos < data.len() {
        let r = (codec.decode)(state, codec, data, inpos, &mut out);
        if r == 0 {
            break;
        } else if !decerror(codec, data, inpos, &mut out, handler, r) {
            return None;
        }
    }
    Some(out)
}

/// `multibytecodec_encerror`: the error protocol over a failing
/// encoder step. `false` returns with the error set.
fn encerror(
    codec: &'static MbCodec,
    state: &mut MbState,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    handler: ErrHandler,
    e: i64,
) -> bool {
    let (reason, esize) = if e > 0 {
        ("illegal multibyte sequence", e as usize)
    } else if e == MBERR_TOOFEW {
        ("incomplete multibyte sequence", *inpos)
    } else if e == MBERR_INTERNAL {
        tstate::set_string(exceptions::runtime_error(), b"internal codec error");
        return false;
    } else {
        tstate::set_string(exceptions::runtime_error(), b"unknown runtime error");
        return false;
    };

    if handler == ErrHandler::Replace {
        // `?` goes through the codec itself; a codec that cannot
        // encode it gets the literal byte.
        let mut subpos = 0;
        let r = (codec.encode)(state, codec, &[b'?' as u32], &mut subpos, out, 0);
        if r != 0 {
            out.push(b'?');
        }
    }
    if matches!(handler, ErrHandler::Ignore | ErrHandler::Replace) {
        *inpos += esize;
        return true;
    }

    let start = *inpos;
    let end = start + esize;
    match handler {
        ErrHandler::Strict => {
            unicode::raise_encode_error(
                codec.encoding,
                chars,
                start as isize,
                end as isize,
                reason,
            );
            false
        }
        ErrHandler::BackslashReplace | ErrHandler::XmlCharRefReplace => {
            // The registry handlers' replacement text, re-encoded
            // through this codec under strict errors exactly as
            // CPython's callback path does.
            let mut text = Vec::new();
            for &ch in &chars[start..end.min(chars.len())] {
                if handler == ErrHandler::BackslashReplace {
                    if ch >= 0x10000 {
                        text.extend(format!("\\U{ch:08x}").chars().map(|c| c as u32));
                    } else if ch >= 0x100 {
                        text.extend(format!("\\u{ch:04x}").chars().map(|c| c as u32));
                    } else {
                        text.extend(format!("\\x{ch:02x}").chars().map(|c| c as u32));
                    }
                } else {
                    text.extend(format!("&#{};", ch).chars().map(|c| c as u32));
                }
            }
            let mut subpos = 0;
            match encode_run(
                codec,
                state,
                &text,
                &mut subpos,
                ErrHandler::Strict,
                MBENC_FLUSH,
            ) {
                Some(replacement) => {
                    out.extend_from_slice(&replacement);
                    *inpos = end;
                    true
                }
                None => false,
            }
        }
        ErrHandler::SurrogateEscape => {
            // Every character of the span must be one of the
            // escape-range surrogates, else the original error
            // re-raises (`PyCodec_SurrogateEscapeErrors`).
            let span = &chars[start..end.min(chars.len())];
            if span.iter().all(|&ch| (0xDC80..=0xDCFF).contains(&ch)) && !span.is_empty() {
                out.extend(span.iter().map(|&ch| ch as u8));
                *inpos = end;
                true
            } else {
                unicode::raise_encode_error(
                    codec.encoding,
                    chars,
                    start as isize,
                    end as isize,
                    reason,
                );
                false
            }
        }
        ErrHandler::SurrogatePass => {
            surrogatepass_type_error(true);
            false
        }
        ErrHandler::Named(name) => {
            unicode::named_handler_error(name);
            false
        }
        // Ignore and Replace never reach the callback path.
        ErrHandler::Ignore | ErrHandler::Replace => unreachable!(),
    }
}

/// `multibytecodec_decerror`: the error protocol over a failing decoder
/// step. `false` returns with the error set.
fn decerror(
    codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
    handler: ErrHandler,
    e: i64,
) -> bool {
    let (reason, esize) = if e > 0 {
        ("illegal multibyte sequence", e as usize)
    } else if e == MBERR_TOOFEW {
        ("incomplete multibyte sequence", data.len() - *inpos)
    } else if e == MBERR_INTERNAL {
        tstate::set_string(exceptions::runtime_error(), b"internal codec error");
        return false;
    } else if e == MBERR_EXCEPTION {
        return false;
    } else {
        tstate::set_string(exceptions::runtime_error(), b"unknown runtime error");
        return false;
    };

    if matches!(handler, ErrHandler::Ignore | ErrHandler::Replace) {
        if handler == ErrHandler::Replace {
            out.push(0xFFFD);
        }
        *inpos += esize;
        return true;
    }

    let start = *inpos;
    let end = start + esize;
    match handler {
        ErrHandler::Strict => {
            unicode::raise_decode_error(codec.encoding, data, start as isize, end as isize, reason);
            false
        }
        // The registry handlers CPython's callbacks would run,
        // including the built-in named ones the interim contract
        // covers inline: backslashreplace's escapes, surrogateescape's
        // byte surrogates, surrogatepass's and xmlcharrefreplace's
        // refusals.
        ErrHandler::BackslashReplace
        | ErrHandler::SurrogateEscape
        | ErrHandler::SurrogatePass
        | ErrHandler::XmlCharRefReplace
        | ErrHandler::Named(_) => {
            match unicode::generic_decode_error(
                out,
                data,
                start as isize,
                end as isize,
                handler,
                codec.encoding,
                reason,
            ) {
                Some(newpos) => {
                    *inpos = newpos.max(0) as usize;
                    true
                }
                None => false,
            }
        }
        ErrHandler::Ignore | ErrHandler::Replace => unreachable!(),
    }
}

/// The surrogatepass refusal for an encoding it does not know:
/// `wrong_exception_type`'s TypeError naming the exception class.
fn surrogatepass_type_error(encode: bool) {
    let name = if encode {
        "UnicodeEncodeError"
    } else {
        "UnicodeDecodeError"
    };
    tstate::set_string(
        exceptions::type_error(),
        format!("don't know how to handle {name} in error callback").as_bytes(),
    );
}

// --------------------------------------------------------------- objects

/// CPython 3.12's `MultibyteCodecObject`: the object `getcodec`
/// returns, holding the codec descriptor the stateful constructors and
/// the encode/decode methods run. The `cjk_module` reference keeps the
/// defining module's tables alive in CPython; this runtime's tables are
/// statics, so the object carries the descriptor alone.
#[repr(C)]
pub struct MultibyteCodecObject {
    pub ob_base: PyObject,
    pub codec: &'static MbCodec,
}

/// CPython 3.12's `MultibyteStatefulEncoderContext`: the fields every
/// stateful encoder shares.
#[repr(C)]
pub struct MultibyteIncrementalEncoderObject {
    pub ob_base: PyObject,
    pub codec: &'static MbCodec,
    pub errors: ErrHandler,
    pub state: MbState,
    pub pending: Option<Vec<u32>>,
}

/// CPython 3.12's `MultibyteStatefulDecoderContext` plus the pending
/// buffer `MultibyteIncrementalDecoderObject` adds.
#[repr(C)]
pub struct MultibyteIncrementalDecoderObject {
    pub ob_base: PyObject,
    pub codec: &'static MbCodec,
    pub errors: ErrHandler,
    pub state: MbState,
    pub pending: [u8; MAXDECPENDING],
    pub pendingsize: usize,
}

const fn codec_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"_multibytecodec.MultibyteCodec".as_ptr();
    t.tp_basicsize = core::mem::size_of::<MultibyteCodecObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn encoder_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"_multibytecodec.MultibyteIncrementalEncoder".as_ptr();
    t.tp_basicsize = core::mem::size_of::<MultibyteIncrementalEncoderObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_IMMUTABLETYPE
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn decoder_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"_multibytecodec.MultibyteIncrementalDecoder".as_ptr();
    t.tp_basicsize = core::mem::size_of::<MultibyteIncrementalDecoderObject>() as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_IMMUTABLETYPE
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_DISALLOW_INSTANTIATION;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `_multibytecodec.MultibyteCodec` type object.
pub static mut MultibyteCodec_Type: PyTypeObject = codec_type();
/// The `_multibytecodec.MultibyteIncrementalEncoder` type object.
pub static mut MultibyteIncrementalEncoder_Type: PyTypeObject = encoder_type();
/// The `_multibytecodec.MultibyteIncrementalDecoder` type object.
pub static mut MultibyteIncrementalDecoder_Type: PyTypeObject = decoder_type();

/// The three static types, for `init_static_types`. CPython's module
/// also exposes `MultibyteStreamReader` and `MultibyteStreamWriter`;
/// every one of their methods reads or writes through a
/// caller-supplied stream object, so they arrive with the container
/// call protocol rather than as shapes whose methods cannot run.
pub fn static_types() -> [*mut PyTypeObject; 3] {
    [
        &raw mut MultibyteCodec_Type,
        &raw mut MultibyteIncrementalEncoder_Type,
        &raw mut MultibyteIncrementalDecoder_Type,
    ]
}

/// Build a `MultibyteCodecObject` for `codec` (CPython's `__create_codec`
/// over a `getcodec` hit).
///
/// # Safety
///
/// `init_static_types` must have run.
pub unsafe fn create_codec(codec: &'static MbCodec) -> *mut PyObject {
    // SAFETY: the type is readied and the object freshly allocated.
    unsafe {
        let object = PyType_GenericAlloc(&raw mut MultibyteCodec_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        (*object.cast::<MultibyteCodecObject>()).codec = codec;
        object
    }
}

/// The codec descriptor a `MultibyteCodecObject` holds; `None` with a
/// `TypeError` set for other objects.
///
/// # Safety
///
/// `object` must be a valid object.
pub unsafe fn codec_of(object: *mut PyObject) -> Option<&'static MbCodec> {
    // SAFETY: the header read is within the object.
    unsafe {
        if header::type_of(object) == &raw mut MultibyteCodec_Type {
            Some((*object.cast::<MultibyteCodecObject>()).codec)
        } else {
            tstate::set_string(exceptions::type_error(), b"codec is unexpected type");
            None
        }
    }
}

/// `_codecs_*.getcodec`'s shared body: the codec named `encoding` in
/// `codecs`, as a `MultibyteCodecObject`; CPython's `LookupError`
/// "no such codec is supported." for a miss.
pub fn lookup_codec(codecs: &[&'static MbCodec], encoding: &str) -> *mut PyObject {
    for codec in codecs {
        if codec.encoding == encoding {
            // SAFETY: the caller initialises static types before any
            // module surface runs.
            return unsafe { create_codec(codec) };
        }
    }
    tstate::set_string(exceptions::lookup_error(), b"no such codec is supported.");
    core::ptr::null_mut()
}

/// Build the `(object, consumed)` tuple the multibyte codec functions
/// return; `None` with the error set.
fn codec_tuple(object: *mut PyObject, consumed: isize) -> *mut PyObject {
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let length = long::PyLong_FromSsize_t(consumed);
    if length.is_null() {
        // SAFETY: our reference, created a moment ago.
        unsafe { exceptions::release_object(object) };
        return core::ptr::null_mut();
    }
    tuple::from_array(&[object, length])
}

// ------------------------------------------------ the MultibyteCodec methods

/// `MultibyteCodec.encode(input, errors)`: a one-shot encode returning
/// `(bytes, consumed_chars)`.
///
/// # Safety
///
/// `codec_object` must be a `MultibyteCodecObject`.
pub unsafe fn mbcodec_encode(
    codec_object: *mut PyObject,
    chars: &[u32],
    errors: Option<&'static str>,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let codec = match unsafe { codec_of(codec_object) } {
        Some(codec) => codec,
        None => return core::ptr::null_mut(),
    };
    let mut state = MbState::new();
    let handler = unicode::error_handler(errors);
    if let Some(encinit) = codec.encinit
        && encinit(&mut state, codec) != 0
    {
        return core::ptr::null_mut();
    }
    let mut inpos = 0;
    let Some(encoded) = encode_run(
        codec,
        &mut state,
        chars,
        &mut inpos,
        handler,
        MBENC_FLUSH | MBENC_RESET,
    ) else {
        return core::ptr::null_mut();
    };
    let Some(object) = rt_bytes::from_vec(Some(encoded)) else {
        return core::ptr::null_mut();
    };
    codec_tuple(object, chars.len() as isize)
}

/// `MultibyteCodec.decode(input, errors)`: a one-shot decode returning
/// `(str, consumed_bytes)`.
///
/// # Safety
///
/// `codec_object` must be a `MultibyteCodecObject`.
pub unsafe fn mbcodec_decode(
    codec_object: *mut PyObject,
    data: &[u8],
    errors: Option<&'static str>,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let codec = match unsafe { codec_of(codec_object) } {
        Some(codec) => codec,
        None => return core::ptr::null_mut(),
    };
    let handler = unicode::error_handler(errors);
    if data.is_empty() {
        let empty = unicode::chars_to_object(&[], 0);
        return codec_tuple(empty, 0);
    }
    let mut state = MbState::new();
    if let Some(decinit) = codec.decinit
        && decinit(&mut state, codec) != 0
    {
        return core::ptr::null_mut();
    }
    let mut inpos = 0;
    let Some(decoded) = decode_run(codec, &mut state, data, &mut inpos, handler) else {
        return core::ptr::null_mut();
    };
    let object = unicode::chars_to_object(&decoded, 0);
    codec_tuple(object, data.len() as isize)
}

// ------------------------------------------- the incremental encoder surface

/// `MultibyteIncrementalEncoder(codec, errors)`: CPython's type reads
/// the codec off its `Lib/encodings` subclass; the Rust constructor
/// takes the descriptor directly.
///
/// # Safety
///
/// `init_static_types` must have run.
pub unsafe fn incremental_encoder_new(
    codec: &'static MbCodec,
    errors: Option<&'static str>,
) -> *mut PyObject {
    // SAFETY: the type is readied and the object freshly allocated.
    unsafe {
        let object = PyType_GenericAlloc(&raw mut MultibyteIncrementalEncoder_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let ctx = &mut *object.cast::<MultibyteIncrementalEncoderObject>();
        ctx.codec = codec;
        ctx.errors = unicode::error_handler(errors);
        ctx.state = MbState::new();
        ctx.pending = None;
        if let Some(encinit) = codec.encinit
            && encinit(&mut ctx.state, codec) != 0
        {
            exceptions::release_object(object);
            return core::ptr::null_mut();
        }
        object
    }
}

/// `encoder_encode_stateful`: `MultibyteIncrementalEncoder.encode(input,
/// final=False)`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalEncoderObject`.
pub unsafe fn incremental_encoder_encode(
    object: *mut PyObject,
    chars: &[u32],
    final_: bool,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalEncoderObject>() };
    let mut inbuf: Vec<u32> = match &ctx.pending {
        Some(pending) => {
            let mut joined = pending.clone();
            joined.extend_from_slice(chars);
            joined
        }
        None => chars.to_vec(),
    };
    let mut inpos = 0;
    let flags = if final_ { MBENC_FLUSH | MBENC_RESET } else { 0 };
    let encoded = match encode_run(
        ctx.codec,
        &mut ctx.state,
        &inbuf,
        &mut inpos,
        ctx.errors,
        flags,
    ) {
        Some(encoded) => encoded,
        None => return core::ptr::null_mut(),
    };
    if inpos < inbuf.len() {
        if inbuf.len() - inpos > MAXENCPENDING {
            // Normal codecs never reach here.
            tstate::set_string(exceptions::unicode_error(), b"pending buffer overflow");
            return core::ptr::null_mut();
        }
        inbuf.drain(..inpos);
        ctx.pending = Some(inbuf);
    } else {
        ctx.pending = None;
    }
    match rt_bytes::from_vec(Some(encoded)) {
        Some(object) => object,
        None => core::ptr::null_mut(),
    }
}

/// `MultibyteIncrementalEncoder.getstate()`: the pending buffer and
/// state packed into a little-endian byte integer.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalEncoderObject`.
pub unsafe fn incremental_encoder_getstate(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &*object.cast::<MultibyteIncrementalEncoderObject>() };
    let mut statebytes = [0u8; 1 + MAXENCPENDING * 4 + 8];
    let mut statesize = 1;
    if let Some(pending) = &ctx.pending {
        let bytes = match unicode::encode_utf8_bytes(pending, ErrHandler::Strict) {
            Some(bytes) => bytes,
            None => return core::ptr::null_mut(),
        };
        if bytes.len() > MAXENCPENDING * 4 {
            tstate::set_string(exceptions::unicode_error(), b"pending buffer too large");
            return core::ptr::null_mut();
        }
        statebytes[0] = bytes.len() as u8;
        statebytes[1..1 + bytes.len()].copy_from_slice(&bytes);
        statesize = 1 + bytes.len();
    }
    statebytes[statesize..statesize + 8].copy_from_slice(&ctx.state.c);
    statesize += 8;
    long_from_bytes_le(&statebytes[..statesize])
}

/// `MultibyteIncrementalEncoder.setstate(state)`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalEncoderObject`; `state` a
/// valid int object.
pub unsafe fn incremental_encoder_setstate(
    object: *mut PyObject,
    state: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalEncoderObject>() };
    let mut statebytes = [0u8; 1 + MAXENCPENDING * 4 + 8];
    if !unsafe { long_as_bytes_le(state, &mut statebytes) } {
        return core::ptr::null_mut();
    }
    if statebytes[0] as usize > MAXENCPENDING * 4 {
        tstate::set_string(exceptions::unicode_error(), b"pending buffer too large");
        return core::ptr::null_mut();
    }
    let pendingsize = statebytes[0] as usize;
    let pending =
        match unicode::decode_utf8_bytes(&statebytes[1..1 + pendingsize], ErrHandler::Strict, None)
        {
            Some(chars) => chars,
            None => return core::ptr::null_mut(),
        };
    ctx.pending = Some(pending);
    ctx.state
        .c
        .copy_from_slice(&statebytes[1 + pendingsize..1 + pendingsize + 8]);
    singletons::none()
}

/// `MultibyteIncrementalEncoder.reset()`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalEncoderObject`.
pub unsafe fn incremental_encoder_reset(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalEncoderObject>() };
    if let Some(encreset) = ctx.codec.encreset {
        let mut out = Vec::new();
        if encreset(&mut ctx.state, ctx.codec, &mut out) != 0 {
            return core::ptr::null_mut();
        }
    }
    ctx.pending = None;
    singletons::none()
}

// ------------------------------------------- the incremental decoder surface

/// `MultibyteIncrementalDecoder(codec, errors)`.
///
/// # Safety
///
/// `init_static_types` must have run.
pub unsafe fn incremental_decoder_new(
    codec: &'static MbCodec,
    errors: Option<&'static str>,
) -> *mut PyObject {
    // SAFETY: the type is readied and the object freshly allocated.
    unsafe {
        let object = PyType_GenericAlloc(&raw mut MultibyteIncrementalDecoder_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let ctx = &mut *object.cast::<MultibyteIncrementalDecoderObject>();
        ctx.codec = codec;
        ctx.errors = unicode::error_handler(errors);
        ctx.state = MbState::new();
        ctx.pendingsize = 0;
        if let Some(decinit) = codec.decinit
            && decinit(&mut ctx.state, codec) != 0
        {
            exceptions::release_object(object);
            return core::ptr::null_mut();
        }
        object
    }
}

/// `MultibyteIncrementalDecoder.decode(input, final=False)`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalDecoderObject`.
pub unsafe fn incremental_decoder_decode(
    object: *mut PyObject,
    data: &[u8],
    final_: bool,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalDecoderObject>() };
    let origpending = ctx.pendingsize;
    if ctx.pendingsize == 0 {
        decoder_feed(ctx, data, final_, origpending)
    } else {
        let mut wdata = ctx.pending[..ctx.pendingsize].to_vec();
        wdata.extend_from_slice(data);
        ctx.pendingsize = 0;
        decoder_feed(ctx, &wdata, final_, origpending)
    }
}

/// The feed loop over one input buffer (`decoder_feed_buffer` plus the
/// final-flush and pending handling around it). `origpending` is the
/// pending byte count to restore if the pass fails.
fn decoder_feed(
    ctx: &mut MultibyteIncrementalDecoderObject,
    buffer: &[u8],
    final_: bool,
    origpending: usize,
) -> *mut PyObject {
    let mut inpos = 0;
    let mut out: Vec<u32> = Vec::with_capacity(buffer.len());
    while inpos < buffer.len() {
        let r = (ctx.codec.decode)(&mut ctx.state, ctx.codec, buffer, &mut inpos, &mut out);
        if r == 0 || r == MBERR_TOOFEW {
            break;
        } else if !decerror(ctx.codec, buffer, &mut inpos, &mut out, ctx.errors, r) {
            // Recover the original pending buffer.
            ctx.pending[..origpending].copy_from_slice(&buffer[..origpending]);
            ctx.pendingsize = origpending;
            return core::ptr::null_mut();
        }
    }
    if final_
        && inpos < buffer.len()
        && !decerror(
            ctx.codec,
            buffer,
            &mut inpos,
            &mut out,
            ctx.errors,
            MBERR_TOOFEW,
        )
    {
        ctx.pending[..origpending].copy_from_slice(&buffer[..origpending]);
        ctx.pendingsize = origpending;
        return core::ptr::null_mut();
    }
    if inpos < buffer.len() {
        let npendings = buffer.len() - inpos;
        if npendings + ctx.pendingsize > MAXDECPENDING {
            tstate::set_string(exceptions::unicode_error(), b"pending buffer overflow");
            return core::ptr::null_mut();
        }
        ctx.pending[ctx.pendingsize..ctx.pendingsize + npendings].copy_from_slice(&buffer[inpos..]);
        ctx.pendingsize += npendings;
    }
    unicode::chars_to_object(&out, 0)
}

/// `MultibyteIncrementalDecoder.getstate()`: `(pending_bytes, state)`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalDecoderObject`.
pub unsafe fn incremental_decoder_getstate(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &*object.cast::<MultibyteIncrementalDecoderObject>() };
    let buffer = match rt_bytes::from_vec(Some(ctx.pending[..ctx.pendingsize].to_vec())) {
        Some(buffer) => buffer,
        None => return core::ptr::null_mut(),
    };
    let statelong = long_from_bytes_le(&ctx.state.c);
    if statelong.is_null() {
        // SAFETY: our reference, created a moment ago.
        unsafe { exceptions::release_object(buffer) };
        return core::ptr::null_mut();
    }
    tuple::from_array(&[buffer, statelong])
}

/// `MultibyteIncrementalDecoder.setstate((buffer, state))`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalDecoderObject`; `state` a
/// valid object.
pub unsafe fn incremental_decoder_setstate(
    object: *mut PyObject,
    state: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalDecoderObject>() };
    let illegal = || {
        tstate::set_string(
            exceptions::type_error(),
            b"setstate(): illegal state argument",
        );
        core::ptr::null_mut()
    };
    // SAFETY: the reads below stay within the tuple the checks bound.
    unsafe {
        if !tuple::is_tuple(state) || tuple::PyTuple_Size(state) != 2 {
            return illegal();
        }
        let buffer = tuple::PyTuple_GetItem(state, 0);
        let statelong = tuple::PyTuple_GetItem(state, 1);
        let mut statebytes = [0u8; 8];
        if !rt_bytes::check(buffer) || !long::is_long(statelong) {
            return illegal();
        }
        if !long_as_bytes_le(statelong, &mut statebytes) {
            return core::ptr::null_mut();
        }
        let buffersize = rt_bytes::size(buffer) as usize;
        if buffersize > MAXDECPENDING {
            tstate::set_string(exceptions::unicode_error(), b"pending buffer too large");
            return core::ptr::null_mut();
        }
        ctx.pending[..buffersize].copy_from_slice(rt_bytes::bytes_of(buffer));
        ctx.pendingsize = buffersize;
        ctx.state.c = statebytes;
    }
    singletons::none()
}

/// `MultibyteIncrementalDecoder.reset()`.
///
/// # Safety
///
/// `object` must be a `MultibyteIncrementalDecoderObject`.
pub unsafe fn incremental_decoder_reset(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller guarantees the object's type.
    let ctx = unsafe { &mut *object.cast::<MultibyteIncrementalDecoderObject>() };
    if let Some(decreset) = ctx.codec.decreset
        && decreset(&mut ctx.state, ctx.codec) != 0
    {
        return core::ptr::null_mut();
    }
    ctx.pendingsize = 0;
    singletons::none()
}

// ------------------------------------------------------------ long packing

/// `_PyLong_FromByteArray(bytes, little_endian=1, is_signed=0)`: a
/// little-endian byte array as one non-negative int. `from_mag`'s
/// digits are base 2^30, so the bytes fold in eight bits at a time.
fn long_from_bytes_le(bytes: &[u8]) -> *mut PyObject {
    let mut mag: Vec<u32> = Vec::new();
    let mut acc: u64 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc |= (b as u64) << bits;
        bits += 8;
        if bits >= 30 {
            mag.push((acc & long::MASK as u64) as u32);
            acc >>= 30;
            bits -= 30;
        }
    }
    if bits > 0 || acc != 0 {
        mag.push((acc & long::MASK as u64) as u32);
    }
    // SAFETY: every entry pushed is below 2^30.
    unsafe { long::from_mag(1, &mag) }
}

/// `_PyLong_AsByteArray(v, out, little_endian=1, is_signed=0)`: a
/// non-negative int back into little-endian bytes. Negative values
/// raise CPython's OverflowError; values too wide for `out` raise the
/// OverflowError `int too big to convert`.
///
/// # Safety
///
/// `value` must be a valid int object.
unsafe fn long_as_bytes_le(value: *mut PyObject, out: &mut [u8]) -> bool {
    // SAFETY: the caller guarantees the int object.
    unsafe {
        if long::is_negative(value.cast()) {
            tstate::set_string(
                exceptions::overflow_error(),
                b"can't convert negative value to unsigned int",
            );
            return false;
        }
        let digits = long::digits(value.cast());
        // The highest nonzero digit bounds the value; anything set past
        // `out`'s width is too big.
        let top = digits
            .iter()
            .rposition(|&d| d != 0)
            .map(|i| i * 30 + (32 - digits[i].leading_zeros()) as usize)
            .map(|bits| bits.div_ceil(8))
            .unwrap_or(0);
        if top > out.len() {
            tstate::set_string(exceptions::overflow_error(), b"int too big to convert");
            return false;
        }
        for (i, byte) in out.iter_mut().enumerate() {
            let lo = 8 * i;
            let k = lo / 30;
            let shift = lo % 30;
            // The window spans two 30-bit digits, so it folds in u64;
            // digits past `top` are zero and read back as zero.
            let digit = |k: usize| digits.get(k).copied().unwrap_or(0);
            let mut window = (digit(k) >> shift) as u64;
            if shift > 0 {
                window |= (digit(k + 1) as u64) << (30 - shift);
            }
            *byte = (window & 0xFF) as u8;
        }
    }
    true
}

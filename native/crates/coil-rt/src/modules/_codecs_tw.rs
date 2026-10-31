//! The `_codecs_tw` module (spec section 10.6): big5 and cp950, a
//! direct port of `Modules/cjkcodecs/_codecs_tw.c` over the generated
//! tables.

use super::_multibytecodec::{MbCodec, MbState, lookup_codec};
use super::cjk_data::{trymap_dec, trymap_enc, tw};

fn big5_encode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    _flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c < 0x80 {
            out.push(c as u8);
            *inpos += 1;
            continue;
        }
        if c > 0xFFFF {
            return 1;
        }
        let Some(code) = trymap_enc(&tw::BIG5_ENC, c) else {
            return 1;
        };
        out.push((code >> 8) as u8);
        out.push((code & 0xFF) as u8);
        *inpos += 1;
    }
    0
}

fn big5_decode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];
        if c < 0x80 {
            out.push(c as u32);
            *inpos += 1;
            continue;
        }
        if data.len() - *inpos < 2 {
            return super::_multibytecodec::MBERR_TOOFEW;
        }
        match trymap_dec(&tw::BIG5_DEC, c, data[*inpos + 1]) {
            Some(decoded) => {
                out.push(decoded);
                *inpos += 2;
            }
            None => return 1,
        }
    }
    0
}

fn cp950_encode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    _flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c < 0x80 {
            out.push(c as u8);
            *inpos += 1;
            continue;
        }
        if c > 0xFFFF {
            return 1;
        }
        let code = match trymap_enc(&tw::CP950EXT_ENC, c) {
            Some(code) => code,
            None => match trymap_enc(&tw::BIG5_ENC, c) {
                Some(code) => code,
                None => return 1,
            },
        };
        out.push((code >> 8) as u8);
        out.push((code & 0xFF) as u8);
        *inpos += 1;
    }
    0
}

fn cp950_decode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];
        if c < 0x80 {
            out.push(c as u32);
            *inpos += 1;
            continue;
        }
        if data.len() - *inpos < 2 {
            return super::_multibytecodec::MBERR_TOOFEW;
        }
        let c2 = data[*inpos + 1];
        let decoded = match trymap_dec(&tw::CP950EXT_DEC, c, c2) {
            Some(decoded) => decoded,
            None => match trymap_dec(&tw::BIG5_DEC, c, c2) {
                Some(decoded) => decoded,
                None => return 1,
            },
        };
        out.push(decoded);
        *inpos += 2;
    }
    0
}

/// The module's codec table.
static CODECS: [&MbCodec; 2] = [
    &MbCodec {
        encoding: "big5",
        config: 0,
        encode: big5_encode,
        encinit: None,
        encreset: None,
        decode: big5_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "cp950",
        config: 0,
        encode: cp950_encode,
        encinit: None,
        encreset: None,
        decode: cp950_decode,
        decinit: None,
        decreset: None,
    },
];

/// `_codecs_tw.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

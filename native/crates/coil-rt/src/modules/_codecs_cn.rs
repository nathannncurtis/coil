//! The `_codecs_cn` module (spec section 10.6): the Mainland Chinese
//! codecs — gb2312, gbk, gb18030 and the stateful hz — as step
//! functions over the generated tables, a direct port of
//! `Modules/cjkcodecs/_codecs_cn.c`. The `getcodec` surface is the
//! engine's.

use super::_multibytecodec::{MbCodec, MbState, lookup_codec};
use super::cjk_data::{cn, trymap_dec, trymap_enc};

/// GBK and GB2312 differ in a few code points (`_codecs_cn.c`'s
/// `GBK_DECODE`): the literal mappings first, then gb2312 with the
/// high bits stripped, then the GBK extensions.
fn gbk_decode(dc1: u8, dc2: u8, out: &mut Vec<u32>) -> bool {
    match (dc1, dc2) {
        (0xa1, 0xaa) => out.push(0x2014),
        (0xa8, 0x44) => out.push(0x2015),
        (0xa1, 0xa4) => out.push(0x00b7),
        _ => {
            if let Some(decoded) = trymap_dec(&cn::GB2312_DEC, dc1 ^ 0x80, dc2 ^ 0x80) {
                out.push(decoded);
            } else if let Some(decoded) = trymap_dec(&cn::GBKEXT_DEC, dc1, dc2) {
                out.push(decoded);
            } else {
                return false;
            }
        }
    }
    true
}

/// `GBK_ENCODE`: the same differences on the way up, with U+30FB
/// excluded from the common table.
fn gbk_encode(c: u32) -> Option<u16> {
    match c {
        0x2014 => Some(0xa1aa),
        0x2015 => Some(0xa844),
        0x00b7 => Some(0xa1a4),
        0x30fb => None,
        _ => trymap_enc(&cn::GBCOMMON_ENC, c),
    }
}

/// The codecs in this file use the first state byte for hz's mode.
const CN_STATE_OFFSET: usize = 0;

fn gb2312_encode(
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
        let Some(code) = trymap_enc(&cn::GBCOMMON_ENC, c) else {
            return 1;
        };
        if code & 0x8000 != 0 {
            // MSB set: GBK.
            return 1;
        }
        out.push(((code >> 8) | 0x80) as u8);
        out.push(((code & 0xFF) | 0x80) as u8);
        *inpos += 1;
    }
    0
}

fn gb2312_decode(
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
        match trymap_dec(&cn::GB2312_DEC, c ^ 0x80, data[*inpos + 1] ^ 0x80) {
            Some(decoded) => {
                out.push(decoded);
                *inpos += 2;
            }
            None => return 1,
        }
    }
    0
}

fn gbk_encode_step(
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
        let Some(code) = gbk_encode(c) else {
            return 1;
        };
        out.push(((code >> 8) | 0x80) as u8);
        if code & 0x8000 != 0 {
            // MSB set: GBK.
            out.push((code & 0xFF) as u8);
        } else {
            // MSB unset: GB2312.
            out.push(((code & 0xFF) | 0x80) as u8);
        }
        *inpos += 1;
    }
    0
}

fn gbk_decode_step(
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
        if !gbk_decode(c, data[*inpos + 1], out) {
            return 1;
        }
        *inpos += 2;
    }
    0
}

fn gb18030_encode_step(
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
        if c >= 0x10000 {
            // The four-byte form over the whole linear range past the
            // BMP: the C encoder fills the byte slots back to front.
            let mut tc = c - 0x10000;
            let b4 = (tc % 10) as u8 + 0x30;
            tc /= 10;
            let b3 = (tc % 126) as u8 + 0x81;
            tc /= 126;
            let b2 = (tc % 10) as u8 + 0x30;
            tc /= 10;
            let b1 = tc as u8 + 0x90;
            out.extend_from_slice(&[b1, b2, b3, b4]);
            *inpos += 1;
            continue;
        }
        let code = match gbk_encode(c) {
            Some(code) => code,
            None => match trymap_enc(&cn::GB18030EXT_ENC, c) {
                Some(code) => code,
                None => {
                    let ranges = cn::GB18030_RANGES;
                    let mut hit = None;
                    for &(first, last, base) in ranges {
                        if first == 0 {
                            break;
                        }
                        if first <= c && c <= last {
                            let mut tc = c - first + base as u32;
                            let b4 = (tc % 10) as u8 + 0x30;
                            tc /= 10;
                            let b3 = (tc % 126) as u8 + 0x81;
                            tc /= 126;
                            let b2 = (tc % 10) as u8 + 0x30;
                            tc /= 10;
                            let b1 = tc as u8 + 0x81;
                            out.extend_from_slice(&[b1, b2, b3, b4]);
                            hit = Some(());
                            break;
                        }
                    }
                    if hit.is_none() {
                        return 1;
                    }
                    *inpos += 1;
                    continue;
                }
            },
        };
        out.push(((code >> 8) | 0x80) as u8);
        if code & 0x8000 != 0 {
            // MSB set: GBK or gb18030 extensions.
            out.push((code & 0xFF) as u8);
        } else {
            // MSB unset: GB2312.
            out.push(((code & 0xFF) | 0x80) as u8);
        }
        *inpos += 1;
    }
    0
}

fn gb18030_decode_step(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    let ranges = cn::GB18030_RANGES;
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
        if (0x30..=0x39).contains(&c2) {
            // A four-byte sequence.
            if data.len() - *inpos < 4 {
                return super::_multibytecodec::MBERR_TOOFEW;
            }
            let c3 = data[*inpos + 2];
            let c4 = data[*inpos + 3];
            if !(0x81..=0xFE).contains(&c)
                || !(0x81..=0xFE).contains(&c3)
                || !(0x30..=0x39).contains(&c4)
            {
                return 1;
            }
            let c = c - 0x81;
            let c2 = c2 - 0x30;
            let c3 = c3 - 0x81;
            let c4 = c4 - 0x30;
            if c < 4 {
                // U+0080 - U+FFFF: linear over the unassigned ranges.
                let lseq = (c as u32 * 10 + c2 as u32) * 1260 + c3 as u32 * 10 + c4 as u32;
                if lseq < 39420 {
                    // Walk until the next range's base passes lseq.
                    let mut index = 0;
                    while lseq >= ranges[index + 1].2 as u32 {
                        index += 1;
                    }
                    out.push(ranges[index].0 - ranges[index].2 as u32 + lseq);
                    *inpos += 4;
                    continue;
                }
            } else if c >= 15 {
                // U+10000 - U+10FFFF.
                let lseq = 0x10000
                    + ((c as u32 - 15) * 10 + c2 as u32) * 1260
                    + c3 as u32 * 10
                    + c4 as u32;
                if lseq <= 0x10FFFF {
                    out.push(lseq);
                    *inpos += 4;
                    continue;
                }
            }
            return 1;
        }
        if !gbk_decode(c, c2, out) {
            match trymap_dec(&cn::GB18030EXT_DEC, c, c2) {
                Some(decoded) => out.push(decoded),
                None => return 1,
            }
        }
        *inpos += 2;
    }
    0
}

fn hz_encinit(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[CN_STATE_OFFSET] = 0;
    0
}

fn hz_encreset(state: &mut MbState, _codec: &'static MbCodec, out: &mut Vec<u8>) -> i64 {
    if state.c[CN_STATE_OFFSET] != 0 {
        out.extend_from_slice(b"~}");
        state.c[CN_STATE_OFFSET] = 0;
    }
    0
}

fn hz_encode_step(
    state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    _flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c < 0x80 {
            if state.c[CN_STATE_OFFSET] != 0 {
                out.extend_from_slice(b"~}");
                state.c[CN_STATE_OFFSET] = 0;
            }
            out.push(c as u8);
            *inpos += 1;
            if c == b'~' as u32 {
                out.push(b'~');
            }
            continue;
        }
        if c > 0xFFFF {
            return 1;
        }
        let Some(code) = trymap_enc(&cn::GBCOMMON_ENC, c) else {
            return 1;
        };
        if code & 0x8000 != 0 {
            // MSB set: GBK.
            return 1;
        }
        if state.c[CN_STATE_OFFSET] == 0 {
            out.extend_from_slice(&[b'~', b'{', (code >> 8) as u8, (code & 0xff) as u8]);
            state.c[CN_STATE_OFFSET] = 1;
        } else {
            out.push((code >> 8) as u8);
            out.push((code & 0xff) as u8);
        }
        *inpos += 1;
    }
    0
}

fn hz_decinit(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[CN_STATE_OFFSET] = 0;
    0
}

fn hz_decreset(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[CN_STATE_OFFSET] = 0;
    0
}

fn hz_decode_step(
    state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];
        if c == b'~' {
            if data.len() - *inpos < 2 {
                return super::_multibytecodec::MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            let mode = state.c[CN_STATE_OFFSET];
            match (c2, mode) {
                (b'~', 0) => out.push(b'~' as u32),
                (b'{', 0) => state.c[CN_STATE_OFFSET] = 1,
                (b'\n', 0) => {}
                (b'}', 1) => state.c[CN_STATE_OFFSET] = 0,
                _ => return 1,
            }
            *inpos += 2;
            continue;
        }
        if c & 0x80 != 0 {
            return 1;
        }
        if state.c[CN_STATE_OFFSET] == 0 {
            // ASCII mode.
            out.push(c as u32);
            *inpos += 1;
        } else {
            // GB mode.
            if data.len() - *inpos < 2 {
                return super::_multibytecodec::MBERR_TOOFEW;
            }
            match trymap_dec(&cn::GB2312_DEC, c, data[*inpos + 1]) {
                Some(decoded) => {
                    out.push(decoded);
                    *inpos += 2;
                }
                None => return 1,
            }
        }
    }
    0
}

/// The module's codec table (`BEGIN_CODECS_LIST`).
static CODECS: [&MbCodec; 4] = [
    &MbCodec {
        encoding: "gb2312",
        config: 0,
        encode: gb2312_encode,
        encinit: None,
        encreset: None,
        decode: gb2312_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "gbk",
        config: 0,
        encode: gbk_encode_step,
        encinit: None,
        encreset: None,
        decode: gbk_decode_step,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "gb18030",
        config: 0,
        encode: gb18030_encode_step,
        encinit: None,
        encreset: None,
        decode: gb18030_decode_step,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "hz",
        config: 0,
        encode: hz_encode_step,
        encinit: Some(hz_encinit),
        encreset: Some(hz_encreset),
        decode: hz_decode_step,
        decinit: Some(hz_decinit),
        decreset: Some(hz_decreset),
    },
];

/// `_codecs_cn.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

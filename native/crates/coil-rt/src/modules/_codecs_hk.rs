//! The `_codecs_hk` module (spec section 10.6): big5hkscs, a direct
//! port of `Modules/cjkcodecs/_codecs_hk.c` over the generated tables.
//! CPython imports big5 from `_codecs_tw` through a capsule at codec
//! init; the tables here are statics, so the step functions reference
//! `_codecs_tw`'s directly.

use super::_multibytecodec::{MBENC_FLUSH, MbCodec, MbState, lookup_codec};
use super::cjk_data::{MULTIC, hk, trymap_dec, trymap_enc, tw};

/// The four HKSCS 2004 composition sequences, hand-written in CPython
/// rather than table-mapped (see `_codecs_hk.c`).
const BIG5HKSCS_PAIRENC_TABLE: [u16; 4] = [0x8862, 0x8864, 0x88a3, 0x88a5];

fn big5hkscs_encode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c < 0x80 {
            out.push(c as u8);
            *inpos += 1;
            continue;
        }
        let mut insize: usize = 1;
        let code;
        if c < 0x10000 {
            if let Some(mapped) = trymap_enc(&hk::BIG5HKSCS_BMP_ENC, c) {
                if mapped == MULTIC {
                    let c2 = if chars.len() - *inpos >= 2 {
                        chars[*inpos + 1]
                    } else {
                        0
                    };
                    if chars.len() - *inpos >= 2
                        && (c & 0xffdf) == 0x00ca
                        && (c2 & 0xfff7) == 0x0304
                    {
                        code = BIG5HKSCS_PAIRENC_TABLE[((c >> 4) | (c2 >> 3)) as usize & 3];
                        insize = 2;
                    } else if chars.len() - *inpos < 2 && flags & MBENC_FLUSH == 0 {
                        return super::_multibytecodec::MBERR_TOOFEW;
                    } else if c == 0xca {
                        code = 0x8866;
                    } else {
                        // c == 0xea
                        code = 0x88a7;
                    }
                } else {
                    code = mapped;
                }
            } else if let Some(mapped) = trymap_enc(&tw::BIG5_ENC, c) {
                code = mapped;
            } else {
                return 1;
            }
        } else if c < 0x20000 {
            return insize as i64;
        } else if c < 0x30000 {
            match trymap_enc(&hk::BIG5HKSCS_NONBMP_ENC, c & 0xffff) {
                Some(mapped) => code = mapped,
                None => return insize as i64,
            }
        } else {
            return insize as i64;
        }
        out.push((code >> 8) as u8);
        out.push((code & 0xFF) as u8);
        *inpos += insize;
    }
    0
}

/// `BH2S`: the linear code of a big5hkscs byte pair over the
/// private-use plane-hint ranges.
fn bh2s(c1: u8, c2: u8) -> i32 {
    (c1 as i32 - 0x87) * (0xfe - 0x40 + 1) + (c2 as i32 - 0x40)
}

fn big5hkscs_decode(
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
        if (!(0xc6..=0xc8).contains(&c) || (c < 0xc7 && c2 < 0xa1))
            && let Some(decoded) = trymap_dec(&tw::BIG5_DEC, c, c2)
        {
            out.push(decoded);
            *inpos += 2;
            continue;
        }
        if let Some(decoded) = trymap_dec(&hk::BIG5HKSCS_DEC, c, c2) {
            let mut s = bh2s(c, c2);
            let hintbase = if bh2s(0x87, 0x40) <= s && s <= bh2s(0xa0, 0xfe) {
                s -= bh2s(0x87, 0x40);
                &hk::BIG5HKSCS_PHINT_0
            } else if bh2s(0xc6, 0xa1) <= s && s <= bh2s(0xc8, 0xfe) {
                s -= bh2s(0xc6, 0xa1);
                &hk::BIG5HKSCS_PHINT_12130
            } else if bh2s(0xf9, 0xd6) <= s && s <= bh2s(0xfe, 0xfe) {
                s -= bh2s(0xf9, 0xd6);
                &hk::BIG5HKSCS_PHINT_21924
            } else {
                return super::_multibytecodec::MBERR_INTERNAL;
            };
            if hintbase[(s >> 3) as usize] & (1 << (s & 7)) != 0 {
                out.push(decoded | 0x20000);
            } else {
                out.push(decoded);
            }
            *inpos += 2;
            continue;
        }
        match ((c as u16) << 8) | c2 as u16 {
            0x8862 => {
                out.push(0x00ca);
                out.push(0x0304);
            }
            0x8864 => {
                out.push(0x00ca);
                out.push(0x030c);
            }
            0x88a3 => {
                out.push(0x00ea);
                out.push(0x0304);
            }
            0x88a5 => {
                out.push(0x00ea);
                out.push(0x030c);
            }
            _ => return 1,
        }
        *inpos += 2;
    }
    0
}

/// The module's codec table.
static CODECS: [&MbCodec; 1] = [&MbCodec {
    encoding: "big5hkscs",
    config: 0,
    encode: big5hkscs_encode,
    encinit: None,
    encreset: None,
    decode: big5hkscs_decode,
    decinit: None,
    decreset: None,
}];

/// `_codecs_hk.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

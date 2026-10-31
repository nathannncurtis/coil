//! The `_codecs_jp` module (spec section 10.6): the seven Japanese
//! codecs, a direct port of `Modules/cjkcodecs/_codecs_jp.c` over the
//! generated tables, including the JIS X 0213:2000 emulation the
//! `euc_jisx0213` and `shift_jisx0213` spellings carry through their
//! `config` (2000).

use super::_multibytecodec::{MBENC_FLUSH, MBERR_TOOFEW, MbCodec, MbState, lookup_codec};
use super::cjk_data::{
    MULTIC, NOCHAR, find_pairencmap, jp, trymap_dec, trymap_enc, trymap_wide_dec,
};

/// `EMPBASE`: the JIS X 0213 extended plane base.
const EMPBASE: u32 = 0x20000;

/// The JIS X 0213:2000 emulation of U+9B1D (plane 2, 7D3B).
const EMULATED_2000_9B1D: u16 = 0x8000 | 0x7d3b;

/// `EMULATE_JISX0213_2000_ENCODE_BMP`'s invalid set.
pub(crate) fn is_2000_invalid_bmp(c: u32) -> bool {
    matches!(
        c,
        0x9B1C | 0x4FF1 | 0x525D | 0x541E | 0x5653 | 0x59F8 | 0x5C5B | 0x5E77 | 0x7626 | 0x7E6B
    )
}

/// `EMULATE_JISX0213_2000_DECODE_PLANE1`'s invalid pairs.
pub(crate) fn is_2000_invalid_plane1(c1: u8, c2: u8) -> bool {
    matches!(
        (c1, c2),
        (0x2E, 0x21)
            | (0x2F, 0x7E)
            | (0x4F, 0x54)
            | (0x4F, 0x7E)
            | (0x74, 0x27)
            | (0x7E, 0x7A)
            | (0x7E, 0x7B)
            | (0x7E, 0x7C)
            | (0x7E, 0x7D)
            | (0x7E, 0x7E)
    )
}

/// `JISX0201_R_ENCODE`: the Roman set, where 0x5c and 0x7e are yen and
/// overline rather than themselves.
pub(crate) fn jisx0201_r_encode(c: u32) -> Option<u32> {
    if c < 0x80 && c != 0x5c && c != 0x7e {
        Some(c)
    } else if c == 0x00a5 {
        Some(0x5c)
    } else if c == 0x203e {
        Some(0x7e)
    } else {
        None
    }
}

/// `JISX0201_K_ENCODE`: half-width katakana.
pub(crate) fn jisx0201_k_encode(c: u32) -> Option<u32> {
    if (0xff61..=0xff9f).contains(&c) {
        Some(c - 0xfec0)
    } else {
        None
    }
}

/// `JISX0201_ENCODE`: Roman first, then katakana.
fn jisx0201_encode(c: u32) -> Option<u32> {
    jisx0201_r_encode(c).or_else(|| jisx0201_k_encode(c))
}

/// `JISX0201_R_DECODE`: the Roman half of the mirror over bytes.
pub(crate) fn jisx0201_r_decode(c: u8) -> Option<u32> {
    if c < 0x5c {
        Some(c as u32)
    } else if c == 0x5c {
        Some(0x00a5)
    } else if c < 0x7e {
        Some(c as u32)
    } else if c == 0x7e {
        Some(0x203e)
    } else if c == 0x7f {
        Some(0x7f)
    } else {
        None
    }
}

/// `JISX0201_DECODE`: the mirror of [`jisx0201_encode`] over bytes.
fn jisx0201_decode(c: u8) -> Option<u32> {
    jisx0201_r_decode(c).or_else(|| jisx0201_k_decode(c))
}

/// `JISX0201_K_DECODE`: the katakana half.
fn jisx0201_k_decode(c: u8) -> Option<u32> {
    if (0xa1..=0xdf).contains(&c) {
        Some(0xfec0 + c as u32)
    } else {
        None
    }
}

// ------------------------------------------------------------- cp932

fn cp932_encode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    _flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c <= 0x80 {
            out.push(c as u8);
            *inpos += 1;
            continue;
        } else if (0xff61..=0xff9f).contains(&c) {
            out.push((c - 0xfec0) as u8);
            *inpos += 1;
            continue;
        } else if (0xf8f0..=0xf8f3).contains(&c) {
            // Windows compatibility area
            if c == 0xf8f0 {
                out.push(0xa0);
            } else {
                out.push((c - 0xf8f1 + 0xfd) as u8);
            }
            *inpos += 1;
            continue;
        }

        if c > 0xFFFF {
            return 1;
        }

        let code = if let Some(mapped) = trymap_enc(&jp::CP932EXT_ENC, c) {
            mapped
        } else if let Some(mapped) = trymap_enc(&jp::JISXCOMMON_ENC, c) {
            if mapped & 0x8000 != 0 {
                // MSB set: JIS X 0212
                return 1;
            }
            // JIS X 0208, folded to the Shift JIS pointer
            let c1 = (mapped >> 8) as u32;
            let c2 = (mapped & 0xff) as u32;
            let c2 = (if (c1 - 0x21) & 1 != 0 { 0x5e } else { 0 }) + (c2 - 0x21);
            let c1 = (c1 - 0x21) >> 1;
            let b1 = if c1 < 0x1f { c1 + 0x81 } else { c1 + 0xc1 };
            let b2 = if c2 < 0x3f { c2 + 0x40 } else { c2 + 0x41 };
            ((b1 << 8) | b2) as u16
        } else if (0xe000..0xe758).contains(&c) {
            // User-defined area
            let c1 = (c - 0xe000) / 188;
            let c2 = (c - 0xe000) % 188;
            let b2 = if c2 < 0x3f { c2 + 0x40 } else { c2 + 0x41 };
            (((c1 + 0xf0) << 8) | b2) as u16
        } else {
            return 1;
        };
        out.push((code >> 8) as u8);
        out.push((code & 0xff) as u8);
        *inpos += 1;
    }
    0
}

fn cp932_decode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];
        if c <= 0x80 {
            out.push(c as u32);
            *inpos += 1;
            continue;
        } else if (0xa0..=0xdf).contains(&c) {
            if c == 0xa0 {
                out.push(0xf8f0);
            } else {
                out.push(0xfec0 + c as u32);
            }
            *inpos += 1;
            continue;
        } else if c >= 0xfd {
            // Windows compatibility area
            out.push(0xf8f1 - 0xfd + c as u32);
            *inpos += 1;
            continue;
        }

        if data.len() - *inpos < 2 {
            return MBERR_TOOFEW;
        }
        let c2 = data[*inpos + 1];

        if let Some(decoded) = trymap_dec(&jp::CP932EXT_DEC, c, c2) {
            out.push(decoded);
        } else if (0x81..=0x9f).contains(&c) || (0xe0..=0xea).contains(&c) {
            if c2 < 0x40 || (c2 > 0x7e && c2 < 0x80) || c2 > 0xfc {
                return 1;
            }
            let c1 = if c < 0xe0 { c - 0x81 } else { c - 0xc1 };
            let c2f = if c2 < 0x80 { c2 - 0x40 } else { c2 - 0x41 };
            let c1 = 2 * c1 as u32 + if c2f < 0x5e { 0 } else { 1 } + 0x21;
            let c2f = (if c2f < 0x5e { c2f } else { c2f - 0x5e }) + 0x21;
            match trymap_dec(&jp::JISX0208_DEC, c1 as u8, c2f) {
                Some(decoded) => out.push(decoded),
                None => return 1,
            }
        } else if (0xf0..=0xf9).contains(&c) {
            if (0x40..=0x7e).contains(&c2) || (0x80..=0xfc).contains(&c2) {
                let c2f = if c2 < 0x80 { c2 - 0x40 } else { c2 - 0x41 };
                out.push(0xe000 + 188 * (c - 0xf0) as u32 + c2f as u32);
            } else {
                return 1;
            }
        } else {
            return 1;
        }
        *inpos += 2;
    }
    0
}

// ----------------------------------------------------- euc_jis_2004

fn euc_jis_2004_encode(
    _state: &mut MbState,
    codec: &'static MbCodec,
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
        let code: u16;
        if c <= 0xFFFF {
            if codec.config == 2000 && is_2000_invalid_bmp(c) {
                return 1;
            } else if codec.config == 2000 && c == 0x9B1D {
                code = EMULATED_2000_9B1D;
            } else if let Some(mapped) = trymap_enc(&jp::JISX0213_BMP_ENC, c) {
                if mapped == MULTIC {
                    if chars.len() - *inpos < 2 {
                        if flags & MBENC_FLUSH != 0 {
                            match find_pairencmap(c, 0, &jp::JISX0213_PAIR_ENC) {
                                Some(pair) => code = pair,
                                None => return 1,
                            }
                        } else {
                            return MBERR_TOOFEW;
                        }
                    } else {
                        let c2 = chars[*inpos + 1];
                        match find_pairencmap(c, c2, &jp::JISX0213_PAIR_ENC) {
                            Some(pair) => {
                                code = pair;
                                insize = 2;
                            }
                            None => match find_pairencmap(c, 0, &jp::JISX0213_PAIR_ENC) {
                                Some(pair) => code = pair,
                                None => return 1,
                            },
                        }
                    }
                } else {
                    code = mapped;
                }
            } else if let Some(mapped) = trymap_enc(&jp::JISXCOMMON_ENC, c) {
                code = mapped;
            } else if (0xff61..=0xff9f).contains(&c) {
                // JIS X 0201 half-width katakana
                out.push(0x8e);
                out.push((c - 0xfec0) as u8);
                *inpos += 1;
                continue;
            } else if c == 0xff3c {
                // F/W REVERSE SOLIDUS (see NOTES in _codecs_jp.c)
                code = 0x2140;
            } else if c == 0xff5e {
                // F/W TILDE (see NOTES in _codecs_jp.c)
                code = 0x2232;
            } else {
                return 1;
            }
        } else if c >> 16 == EMPBASE >> 16 {
            if codec.config == 2000 && c == 0x20B9F {
                return 1;
            }
            match trymap_enc(&jp::JISX0213_EMP_ENC, c & 0xffff) {
                Some(mapped) => code = mapped,
                None => return insize as i64,
            }
        } else {
            return insize as i64;
        }

        if code & 0x8000 != 0 {
            // Codeset 2
            out.push(0x8f);
            out.push((code >> 8) as u8);
            out.push(((code & 0xFF) | 0x80) as u8);
        } else {
            // Codeset 1
            out.push(((code >> 8) | 0x80) as u8);
            out.push(((code & 0xFF) | 0x80) as u8);
        }
        *inpos += insize;
    }
    0
}

fn euc_jis_2004_decode(
    _state: &mut MbState,
    codec: &'static MbCodec,
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

        if c == 0x8e {
            // JIS X 0201 half-width katakana
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            if (0xa1..=0xdf).contains(&c2) {
                out.push(0xfec0 + c2 as u32);
                *inpos += 2;
            } else {
                return 1;
            }
        } else if c == 0x8f {
            if data.len() - *inpos < 3 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1] ^ 0x80;
            let c3 = data[*inpos + 2] ^ 0x80;

            // JIS X 0213 Plane 2 or JIS X 0212 (see NOTES in _codecs_jp.c)
            if codec.config == 2000 && c2 == 0x7d && c3 == 0x3b {
                out.push(0x9B1D);
            } else if let Some(decoded) = trymap_dec(&jp::JISX0213_2_BMP_DEC, c2, c3) {
                out.push(decoded);
            } else if let Some(code) = trymap_dec(&jp::JISX0213_2_EMP_DEC, c2, c3) {
                out.push(EMPBASE | code);
                *inpos += 3;
                continue;
            } else if let Some(decoded) = trymap_dec(&jp::JISX0212_DEC, c2, c3) {
                out.push(decoded);
            } else {
                return 1;
            }
            *inpos += 3;
        } else {
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c1 = c ^ 0x80;
            let c2 = data[*inpos + 1] ^ 0x80;

            // JIS X 0213 Plane 1
            if codec.config == 2000 && is_2000_invalid_plane1(c1, c2) {
                return 2;
            } else if c1 == 0x21 && c2 == 0x40 {
                out.push(0xff3c);
            } else if c1 == 0x22 && c2 == 0x32 {
                out.push(0xff5e);
            } else if let Some(decoded) = trymap_dec(&jp::JISX0208_DEC, c1, c2) {
                out.push(decoded);
            } else if let Some(decoded) = trymap_dec(&jp::JISX0213_1_BMP_DEC, c1, c2) {
                out.push(decoded);
            } else if let Some(code) = trymap_dec(&jp::JISX0213_1_EMP_DEC, c1, c2) {
                out.push(EMPBASE | code);
                *inpos += 2;
                continue;
            } else if let Some(code) = trymap_wide_dec(&jp::JISX0213_PAIR_DEC, c1, c2) {
                out.push(code >> 16);
                out.push(code & 0xffff);
                *inpos += 2;
                continue;
            } else {
                return 1;
            }
            *inpos += 2;
        }
    }
    0
}

// ------------------------------------------------------------- euc_jp

fn euc_jp_encode(
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

        let code = if let Some(mapped) = trymap_enc(&jp::JISXCOMMON_ENC, c) {
            mapped
        } else if (0xff61..=0xff9f).contains(&c) {
            // JIS X 0201 half-width katakana
            out.push(0x8e);
            out.push((c - 0xfec0) as u8);
            *inpos += 1;
            continue;
        } else if c == 0xff3c {
            // FULL-WIDTH REVERSE SOLIDUS
            0x2140
        } else if c == 0xa5 {
            // YEN SIGN
            out.push(0x5c);
            *inpos += 1;
            continue;
        } else if c == 0x203e {
            // OVERLINE
            out.push(0x7e);
            *inpos += 1;
            continue;
        } else {
            return 1;
        };

        if code & 0x8000 != 0 {
            // JIS X 0212
            out.push(0x8f);
            out.push((code >> 8) as u8);
            out.push(((code & 0xFF) | 0x80) as u8);
        } else {
            // JIS X 0208
            out.push(((code >> 8) | 0x80) as u8);
            out.push(((code & 0xFF) | 0x80) as u8);
        }
        *inpos += 1;
    }
    0
}

fn euc_jp_decode(
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

        if c == 0x8e {
            // JIS X 0201 half-width katakana
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            if (0xa1..=0xdf).contains(&c2) {
                out.push(0xfec0 + c2 as u32);
                *inpos += 2;
            } else {
                return 1;
            }
        } else if c == 0x8f {
            if data.len() - *inpos < 3 {
                return MBERR_TOOFEW;
            }
            // JIS X 0212
            match trymap_dec(
                &jp::JISX0212_DEC,
                data[*inpos + 1] ^ 0x80,
                data[*inpos + 2] ^ 0x80,
            ) {
                Some(decoded) => out.push(decoded),
                None => return 1,
            }
            *inpos += 3;
        } else {
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            // JIS X 0208
            if c == 0xa1 && c2 == 0xc0 {
                // FULL-WIDTH REVERSE SOLIDUS
                out.push(0xff3c);
            } else if let Some(decoded) = trymap_dec(&jp::JISX0208_DEC, c ^ 0x80, c2 ^ 0x80) {
                out.push(decoded);
            } else {
                return 1;
            }
            *inpos += 2;
        }
    }
    0
}

// ---------------------------------------------------------- shift_jis

fn shift_jis_encode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    _flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];

        // The non-strict build passes 0x5c and 0x7e through; the yen
        // sign and overline take their places.
        let mut code = if c < 0x80 {
            Some(c)
        } else if c == 0x00a5 {
            Some(0x5c)
        } else if c == 0x203e {
            Some(0x7e)
        } else {
            jisx0201_k_encode(c)
        };

        if c > 0xFFFF {
            return 1;
        }
        if code.is_none() {
            code = Some(NOCHAR as u32);
        }
        let code = code.unwrap() as u16;

        if (code as u32) < 0x80 || (0xa1..=0xdf).contains(&code) {
            out.push(code as u8);
            *inpos += 1;
            continue;
        }

        let code = if code == NOCHAR {
            match trymap_enc(&jp::JISXCOMMON_ENC, c) {
                Some(mapped) => {
                    if mapped & 0x8000 != 0 {
                        // MSB set: JIS X 0212
                        return 1;
                    }
                    mapped
                }
                None => {
                    if c == 0xff3c {
                        // FULL-WIDTH REVERSE SOLIDUS
                        0x2140
                    } else {
                        return 1;
                    }
                }
            }
        } else {
            code
        };

        let c1 = (code >> 8) as u32;
        let c2 = (code & 0xff) as u32;
        let c2 = (if (c1 - 0x21) & 1 != 0 { 0x5e } else { 0 }) + (c2 - 0x21);
        let c1 = (c1 - 0x21) >> 1;
        out.push((if c1 < 0x1f { c1 + 0x81 } else { c1 + 0xc1 }) as u8);
        out.push((if c2 < 0x3f { c2 + 0x40 } else { c2 + 0x41 }) as u8);
        *inpos += 1;
    }
    0
}

fn shift_jis_decode(
    _state: &mut MbState,
    _codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];

        // The non-strict build passes 0x5c and 0x7e through.
        if c < 0x80 {
            out.push(c as u32);
            *inpos += 1;
            continue;
        } else if (0xa1..=0xdf).contains(&c) {
            out.push(0xfec0 + c as u32);
            *inpos += 1;
            continue;
        } else if (0x81..=0x9f).contains(&c) || (0xe0..=0xea).contains(&c) {
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            if c2 < 0x40 || (c2 > 0x7e && c2 < 0x80) || c2 > 0xfc {
                return 1;
            }

            let c1 = if c < 0xe0 { c - 0x81 } else { c - 0xc1 };
            let c2f = if c2 < 0x80 { c2 - 0x40 } else { c2 - 0x41 };
            let c1 = 2 * c1 as u32 + if c2f < 0x5e { 0 } else { 1 } + 0x21;
            let c2f = (if c2f < 0x5e { c2f } else { c2f - 0x5e }) + 0x21;

            if c1 == 0x21 && c2f == 0x40 {
                // FULL-WIDTH REVERSE SOLIDUS
                out.push(0xff3c);
                *inpos += 2;
                continue;
            }
            match trymap_dec(&jp::JISX0208_DEC, c1 as u8, c2f) {
                Some(decoded) => out.push(decoded),
                None => return 1,
            }
            *inpos += 2;
        } else {
            return 1;
        }
    }
    0
}

// ---------------------------------------------------- shift_jis_2004

fn shift_jis_2004_encode(
    _state: &mut MbState,
    codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    flags: u32,
) -> i64 {
    while *inpos < chars.len() {
        let c = chars[*inpos];
        let mut code = jisx0201_encode(c).unwrap_or(NOCHAR as u32) as u16;

        if (code as u32) < 0x80 || (0xa1..=0xdf).contains(&code) {
            out.push(code as u8);
            *inpos += 1;
            continue;
        }

        let mut insize: usize = 1;

        if code == NOCHAR {
            if c <= 0xffff {
                if codec.config == 2000 && is_2000_invalid_bmp(c) {
                    return 1;
                } else if codec.config == 2000 && c == 0x9B1D {
                    code = EMULATED_2000_9B1D;
                } else if let Some(mapped) = trymap_enc(&jp::JISX0213_BMP_ENC, c) {
                    if mapped == MULTIC {
                        if chars.len() - *inpos < 2 {
                            if flags & MBENC_FLUSH != 0 {
                                match find_pairencmap(c, 0, &jp::JISX0213_PAIR_ENC) {
                                    Some(pair) => code = pair,
                                    None => return 1,
                                }
                            } else {
                                return MBERR_TOOFEW;
                            }
                        } else {
                            let ch2 = chars[*inpos + 1];
                            match find_pairencmap(c, ch2, &jp::JISX0213_PAIR_ENC) {
                                Some(pair) => {
                                    code = pair;
                                    insize = 2;
                                }
                                None => match find_pairencmap(c, 0, &jp::JISX0213_PAIR_ENC) {
                                    Some(pair) => code = pair,
                                    None => return 1,
                                },
                            }
                        }
                    } else {
                        code = mapped;
                    }
                } else if let Some(mapped) = trymap_enc(&jp::JISXCOMMON_ENC, c) {
                    // abandon JIS X 0212 codes
                    if mapped & 0x8000 != 0 {
                        return 1;
                    }
                    code = mapped;
                } else {
                    return 1;
                }
            } else if c >> 16 == EMPBASE >> 16 {
                if codec.config == 2000 && c == 0x20B9F {
                    return 1;
                }
                match trymap_enc(&jp::JISX0213_EMP_ENC, c & 0xffff) {
                    Some(mapped) => code = mapped,
                    None => return insize as i64,
                }
            } else {
                return insize as i64;
            }
        }

        let mut c1 = (code >> 8) as i32;
        let mut c2 = (code & 0xff) as i32 - 0x21;

        if c1 & 0x80 != 0 {
            // Plane 2
            if c1 >= 0xee {
                c1 -= 0x87;
            } else if c1 >= 0xac || c1 == 0xa8 {
                c1 -= 0x49;
            } else {
                c1 -= 0x43;
            }
        } else {
            // Plane 1
            c1 -= 0x21;
        }

        if c1 & 1 != 0 {
            c2 += 0x5e;
        }
        c1 >>= 1;
        out.push((c1 + if c1 < 0x1f { 0x81 } else { 0xc1 }) as u8);
        out.push((c2 + if c2 < 0x3f { 0x40 } else { 0x41 }) as u8);

        *inpos += insize;
    }
    0
}

fn shift_jis_2004_decode(
    _state: &mut MbState,
    codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    while *inpos < data.len() {
        let c = data[*inpos];

        if let Some(decoded) = jisx0201_decode(c) {
            out.push(decoded);
            *inpos += 1;
            continue;
        } else if (0x81..=0x9f).contains(&c) || (0xe0..=0xfc).contains(&c) {
            if data.len() - *inpos < 2 {
                return MBERR_TOOFEW;
            }
            let c2 = data[*inpos + 1];
            if c2 < 0x40 || (c2 > 0x7e && c2 < 0x80) || c2 > 0xfc {
                return 1;
            }

            let mut c1 = (if c < 0xe0 { c - 0x81 } else { c - 0xc1 }) as i32;
            let mut c2 = (if c2 < 0x80 { c2 - 0x40 } else { c2 - 0x41 }) as i32;
            c1 = 2 * c1 + if c2 < 0x5e { 0 } else { 1 };
            c2 = (if c2 < 0x5e { c2 } else { c2 - 0x5e }) + 0x21;

            if c1 < 0x5e {
                // Plane 1
                c1 += 0x21;
                if codec.config == 2000 && is_2000_invalid_plane1(c1 as u8, c2 as u8) {
                    return 2;
                } else if let Some(decoded) = trymap_dec(&jp::JISX0208_DEC, c1 as u8, c2 as u8) {
                    out.push(decoded);
                } else if let Some(decoded) =
                    trymap_dec(&jp::JISX0213_1_BMP_DEC, c1 as u8, c2 as u8)
                {
                    out.push(decoded);
                } else if let Some(code) = trymap_dec(&jp::JISX0213_1_EMP_DEC, c1 as u8, c2 as u8) {
                    out.push(EMPBASE | code);
                } else if let Some(code) =
                    trymap_wide_dec(&jp::JISX0213_PAIR_DEC, c1 as u8, c2 as u8)
                {
                    out.push(code >> 16);
                    out.push(code & 0xffff);
                } else {
                    return 1;
                }
                *inpos += 2;
            } else {
                // Plane 2
                if c1 >= 0x67 {
                    c1 += 0x07;
                } else if c1 >= 0x63 || c1 == 0x5f {
                    c1 -= 0x37;
                } else {
                    c1 -= 0x3d;
                }

                if codec.config == 2000 && c1 == 0x7d && c2 == 0x3b {
                    out.push(0x9B1D);
                } else if let Some(decoded) =
                    trymap_dec(&jp::JISX0213_2_BMP_DEC, c1 as u8, c2 as u8)
                {
                    out.push(decoded);
                } else if let Some(code) = trymap_dec(&jp::JISX0213_2_EMP_DEC, c1 as u8, c2 as u8) {
                    out.push(EMPBASE | code);
                    *inpos += 2;
                    continue;
                } else {
                    return 1;
                }
                *inpos += 2;
            }
            continue;
        } else {
            return 1;
        }
    }
    0
}

/// The module's codec table; the `jisx0213` spellings are the 2004
/// steps under the 2000 emulation config.
static CODECS: [&MbCodec; 7] = [
    &MbCodec {
        encoding: "shift_jis",
        config: 0,
        encode: shift_jis_encode,
        encinit: None,
        encreset: None,
        decode: shift_jis_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "cp932",
        config: 0,
        encode: cp932_encode,
        encinit: None,
        encreset: None,
        decode: cp932_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "euc_jp",
        config: 0,
        encode: euc_jp_encode,
        encinit: None,
        encreset: None,
        decode: euc_jp_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "shift_jis_2004",
        config: 0,
        encode: shift_jis_2004_encode,
        encinit: None,
        encreset: None,
        decode: shift_jis_2004_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "euc_jis_2004",
        config: 0,
        encode: euc_jis_2004_encode,
        encinit: None,
        encreset: None,
        decode: euc_jis_2004_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "euc_jisx0213",
        config: 2000,
        encode: euc_jis_2004_encode,
        encinit: None,
        encreset: None,
        decode: euc_jis_2004_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "shift_jisx0213",
        config: 2000,
        encode: shift_jis_2004_encode,
        encinit: None,
        encreset: None,
        decode: shift_jis_2004_decode,
        decinit: None,
        decreset: None,
    },
];

/// `_codecs_jp.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

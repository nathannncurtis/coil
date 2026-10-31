//! The `_codecs_iso2022` module (spec section 10.6): the seven
//! ISO-2022 codecs, a direct port of
//! `Modules/cjkcodecs/_codecs_iso2022.c` over the generated tables of
//! the other locale modules. CPython threads the shared maps in
//! through `IMPORT_MAP` capsules at codec init; here they are statics,
//! so the per-charset accessors reference them directly and the codec
//! init pass disappears.
//!
//! `MbCodec::config` indexes [`CONFIGS`]; the JIS X 0213:2000
//! emulation is carried by the encoder and decoder functions the
//! registry rows pick.

use super::_codecs_jp::{
    is_2000_invalid_bmp, is_2000_invalid_plane1, jisx0201_k_encode, jisx0201_r_decode,
    jisx0201_r_encode,
};
use super::_multibytecodec::{
    MBENC_FLUSH, MBERR_INTERNAL, MBERR_TOOFEW, MbCodec, MbState, lookup_codec,
};
use super::cjk_data::{
    MULTIC, cn, find_pairencmap, jp, kr, trymap_dec, trymap_enc, trymap_wide_dec,
};

const ESC: u8 = 0x1B;
const SO: u8 = 0x0E;
const SI: u8 = 0x0F;
const LF: u8 = 0x0A;

/// `MAX_ESCSEQLEN`: the longest escape sequence `iso2022processesc`
/// scans for.
const MAX_ESCSEQLEN: usize = 16;

const CHARSET_ISO8859_1: u8 = b'A';
const CHARSET_ASCII: u8 = b'B';
const CHARSET_ISO8859_7: u8 = b'F';
const CHARSET_JISX0201_K: u8 = b'I';
const CHARSET_JISX0201_R: u8 = b'J';

const CHARSET_GB2312: u8 = b'A' | CHARSET_DBCS;
const CHARSET_JISX0208: u8 = b'B' | CHARSET_DBCS;
const CHARSET_KSX1001: u8 = b'C' | CHARSET_DBCS;
const CHARSET_JISX0212: u8 = b'D' | CHARSET_DBCS;
const CHARSET_JISX0213_2000_1: u8 = b'O' | CHARSET_DBCS;
const CHARSET_JISX0213_2: u8 = b'P' | CHARSET_DBCS;
const CHARSET_JISX0213_2004_1: u8 = b'Q' | CHARSET_DBCS;
const CHARSET_JISX0208_O: u8 = b'@' | CHARSET_DBCS;

const CHARSET_DBCS: u8 = 0x80;

/// `MAP_UNMAPPABLE`: a designation accessor's miss.
const MAP_UNMAPPABLE: u16 = 0xFFFF;
/// `MAP_MULTIPLE_AVAIL`: the first of a JIS X 0213 pair.
const MAP_MULTIPLE_AVAIL: u16 = 0xFFFE;

/// `F_SHIFTED`: G1 is invoked (SO seen, no SI yet).
const F_SHIFTED: u8 = 0x01;
/// `F_ESCTHROUGHOUT`: passing an unknown escape sequence through.
const F_ESCTHROUGHOUT: u8 = 0x02;

// iso2022_config.flags
const NO_SHIFT: u32 = 0x01;
const USE_G2: u32 = 0x02;
const USE_JISX0208_EXT: u32 = 0x04;

/// `IS_ESCEND`.
fn is_escend(c: u8) -> bool {
    c.is_ascii_uppercase() || c == b'@'
}

/// `IS_ISO2022ESC`: enough of the escape headers for the CJK instances.
fn is_iso2022esc(c2: u8) -> bool {
    matches!(c2, b'(' | b')' | b'$' | b'.' | b'&')
}

/// A designation's decoder: the character of `data[0..width]`, or
/// `MAP_UNMAPPABLE`.
type IsoDecodeFn = fn(&[u8]) -> u32;

/// A designation's encoder: the code of `data`, where `*length` is the
/// character count on the way in and the consumed count on the way out
/// (`-1` flushes an unterminated pair).
type IsoEncodeFn = fn(&[u32], &mut isize) -> u16;

/// `struct iso2022_designation`.
struct IsoDesignation {
    mark: u8,
    plane: u8,
    width: u8,
    decoder: IsoDecodeFn,
    encoder: IsoEncodeFn,
}

/// `struct iso2022_config`.
struct Iso2022Config {
    flags: u32,
    designations: &'static [IsoDesignation],
}

/// The codec's config, indexed by `MbCodec::config`.
static CONFIGS: [Iso2022Config; 7] = [
    Iso2022Config {
        flags: 0,
        designations: &DESIGNATIONS_KR,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP_1,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_G2 | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP_2,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP_2004,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP_3,
    },
    Iso2022Config {
        flags: NO_SHIFT | USE_JISX0208_EXT,
        designations: &DESIGNATIONS_JP_EXT,
    },
];

fn config_of(codec: &'static MbCodec) -> &'static Iso2022Config {
    &CONFIGS[codec.config as usize]
}

// ------------------------------------------------------------ step fns

fn iso2022_encinit(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[4] = 0;
    state.c[0] = CHARSET_ASCII;
    state.c[1] = CHARSET_ASCII;
    0
}

fn iso2022_encreset(state: &mut MbState, _codec: &'static MbCodec, out: &mut Vec<u8>) -> i64 {
    if state.c[4] & F_SHIFTED != 0 {
        out.push(SI);
        state.c[4] &= !F_SHIFTED;
    }
    if state.c[0] != CHARSET_ASCII {
        out.push(ESC);
        out.push(b'(');
        out.push(b'B');
        state.c[0] = CHARSET_ASCII;
    }
    0
}

fn iso2022_encode(
    state: &mut MbState,
    codec: &'static MbCodec,
    chars: &[u32],
    inpos: &mut usize,
    out: &mut Vec<u8>,
    flags: u32,
) -> i64 {
    let config = config_of(codec);
    while *inpos < chars.len() {
        let c = chars[*inpos];
        if c < 0x80 {
            if state.c[0] != CHARSET_ASCII {
                out.push(ESC);
                out.push(b'(');
                out.push(b'B');
                state.c[0] = CHARSET_ASCII;
            }
            if state.c[4] & F_SHIFTED != 0 {
                out.push(SI);
                state.c[4] &= !F_SHIFTED;
            }
            out.push(c as u8);
            *inpos += 1;
            continue;
        }

        let mut insize: usize = 1;
        let mut found: Option<&'static IsoDesignation> = None;
        let mut encoded = MAP_UNMAPPABLE;
        for dsg in config.designations {
            let mut buf = [c, 0];
            let mut length: isize = 1;
            encoded = (dsg.encoder)(&buf, &mut length);
            if encoded == MAP_MULTIPLE_AVAIL {
                // this implementation won't work for a pair of
                // non-BMP characters
                if chars.len() - *inpos < 2 {
                    if flags & MBENC_FLUSH == 0 {
                        return MBERR_TOOFEW;
                    }
                    length = -1;
                } else {
                    buf[1] = chars[*inpos + 1];
                    length = 2;
                }
                encoded = (dsg.encoder)(&buf, &mut length);
                if encoded != MAP_UNMAPPABLE {
                    insize = length as usize;
                    found = Some(dsg);
                    break;
                }
            } else if encoded != MAP_UNMAPPABLE {
                found = Some(dsg);
                break;
            }
        }

        let Some(dsg) = found else {
            return 1;
        };

        match dsg.plane {
            0 => {
                // G0
                if state.c[4] & F_SHIFTED != 0 {
                    out.push(SI);
                    state.c[4] &= !F_SHIFTED;
                }
                if state.c[0] != dsg.mark {
                    if dsg.width == 1 {
                        out.push(ESC);
                        out.push(b'(');
                        out.push(dsg.mark & 0x7f);
                        state.c[0] = dsg.mark;
                    } else if dsg.mark == CHARSET_JISX0208 {
                        out.push(ESC);
                        out.push(b'$');
                        out.push(dsg.mark & 0x7f);
                        state.c[0] = dsg.mark;
                    } else {
                        out.push(ESC);
                        out.push(b'$');
                        out.push(b'(');
                        out.push(dsg.mark & 0x7f);
                        state.c[0] = dsg.mark;
                    }
                }
            }
            1 => {
                // G1
                if state.c[1] != dsg.mark {
                    if dsg.width == 1 {
                        out.push(ESC);
                        out.push(b')');
                        out.push(dsg.mark & 0x7f);
                        state.c[1] = dsg.mark;
                    } else {
                        out.push(ESC);
                        out.push(b'$');
                        out.push(b')');
                        out.push(dsg.mark & 0x7f);
                        state.c[1] = dsg.mark;
                    }
                }
                if state.c[4] & F_SHIFTED == 0 {
                    out.push(SO);
                    state.c[4] |= F_SHIFTED;
                }
            }
            // G2 and G3 are not supported: no encoding in CJKCodecs
            // uses them
            _ => return MBERR_INTERNAL,
        }

        if dsg.width == 1 {
            out.push(encoded as u8);
        } else {
            out.push((encoded >> 8) as u8);
            out.push((encoded & 0xff) as u8);
        }
        *inpos += insize;
    }
    0
}

/// `iso2022processesc`: the escape sequence at `data[*inpos..]`, or the
/// error (a positive width is an illegal sequence).
fn iso2022processesc(
    codec: &'static MbCodec,
    state: &mut MbState,
    data: &[u8],
    inpos: &mut usize,
) -> i64 {
    let config = config_of(codec);
    let inleft = data.len() - *inpos;
    let mut esclen = 0usize;
    let mut i = 1usize;
    while i < MAX_ESCSEQLEN {
        if i >= inleft {
            return MBERR_TOOFEW;
        }
        if is_escend(data[*inpos + i]) {
            esclen = i + 1;
            break;
        } else if config.flags & USE_JISX0208_EXT != 0
            && i + 1 < inleft
            && data[*inpos + i] == b'&'
            && data[*inpos + i + 1] == b'@'
        {
            i += 2;
        }
        i += 1;
    }

    let charset: u8;
    let designation: usize;
    match esclen {
        0 => return 1, // unterminated escape sequence
        3 => {
            if data[*inpos + 1] == b'$' {
                charset = data[*inpos + 2] | CHARSET_DBCS;
                designation = 0;
            } else {
                charset = data[*inpos + 2];
                designation = match data[*inpos + 1] {
                    b'(' => 0,
                    b')' => 1,
                    c2 if config.flags & USE_G2 != 0 && c2 == b'.' => 2,
                    _ => return 3,
                };
            }
        }
        4 => {
            if data[*inpos + 1] != b'$' {
                return 4;
            }
            charset = data[*inpos + 3] | CHARSET_DBCS;
            designation = match data[*inpos + 2] {
                b'(' => 0,
                b')' => 1,
                _ => return 4,
            };
        }
        6 => {
            // designation with prefix
            if config.flags & USE_JISX0208_EXT != 0
                && data[*inpos + 3] == ESC
                && data[*inpos + 4] == b'$'
                && data[*inpos + 5] == b'B'
            {
                charset = b'B' | CHARSET_DBCS;
                designation = 0;
            } else {
                return 6;
            }
        }
        other => return other as i64,
    }

    // raise an error when the charset is not designated for this
    // encoding
    if charset != CHARSET_ASCII && !config.designations.iter().any(|dsg| dsg.mark == charset) {
        return esclen as i64;
    }

    state.c[designation] = charset;
    *inpos += esclen;
    0
}

/// `ISO8859_7_DECODE`.
fn iso8859_7_decode(c: u8) -> Option<u32> {
    if c < 0xa0 || (c < 0xc0 && (0x288f_3bc9u32 & (1 << (c - 0xa0))) != 0) {
        Some(c as u32)
    } else if (0xb4..=0xfe).contains(&c) && (c >= 0xd4 || (0xbfff_fd77u32 & (1 << (c - 0xb4))) != 0)
    {
        Some(0x02d0 + c as u32)
    } else if c == 0xa1 {
        Some(0x2018)
    } else if c == 0xa2 {
        Some(0x2019)
    } else if c == 0xaf {
        Some(0x2015)
    } else {
        None
    }
}

/// `iso2022processg2`: the SS2 single shift at `data[*inpos..]`.
fn iso2022processg2(
    _codec: &'static MbCodec,
    state: &mut MbState,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    let c3 = data[*inpos + 2];
    if state.c[2] == CHARSET_ISO8859_1 {
        if c3 < 0x80 {
            out.push(c3 as u32 + 0x80);
        } else {
            return 3;
        }
    } else if state.c[2] == CHARSET_ISO8859_7 {
        match iso8859_7_decode(c3 ^ 0x80) {
            Some(decoded) => out.push(decoded),
            None => return 3,
        }
    } else if state.c[2] == CHARSET_ASCII {
        if c3 & 0x80 != 0 {
            return 3;
        }
        out.push(c3 as u32);
    } else {
        return MBERR_INTERNAL;
    }
    *inpos += 3;
    0
}

fn iso2022_decinit(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[4] = 0;
    state.c[0] = CHARSET_ASCII;
    state.c[1] = CHARSET_ASCII;
    state.c[2] = CHARSET_ASCII;
    0
}

fn iso2022_decreset(state: &mut MbState, _codec: &'static MbCodec) -> i64 {
    state.c[0] = CHARSET_ASCII;
    state.c[4] &= !F_SHIFTED;
    0
}

fn iso2022_decode(
    state: &mut MbState,
    codec: &'static MbCodec,
    data: &[u8],
    inpos: &mut usize,
    out: &mut Vec<u32>,
) -> i64 {
    let config = config_of(codec);
    while *inpos < data.len() {
        let c = data[*inpos];

        if state.c[4] & F_ESCTHROUGHOUT != 0 {
            // ESC throughout mode: for non-iso2022 escape sequences
            out.push(c as u32); // assume as ISO-8859-1
            *inpos += 1;
            if is_escend(c) {
                state.c[4] &= !F_ESCTHROUGHOUT;
            }
            continue;
        }

        match c {
            ESC => {
                if data.len() - *inpos < 2 {
                    return MBERR_TOOFEW;
                }
                if is_iso2022esc(data[*inpos + 1]) {
                    let err = iso2022processesc(codec, state, data, inpos);
                    if err != 0 {
                        return err;
                    }
                } else if config.flags & USE_G2 != 0 && data[*inpos + 1] == b'N' {
                    // SS2
                    if data.len() - *inpos < 3 {
                        return MBERR_TOOFEW;
                    }
                    let err = iso2022processg2(codec, state, data, inpos, out);
                    if err != 0 {
                        return err;
                    }
                } else {
                    out.push(ESC as u32);
                    state.c[4] |= F_ESCTHROUGHOUT;
                    *inpos += 1;
                }
            }
            SI => {
                if config.flags & NO_SHIFT != 0 {
                    out.push(c as u32);
                    *inpos += 1;
                } else {
                    state.c[4] &= !F_SHIFTED;
                    *inpos += 1;
                }
            }
            SO => {
                if config.flags & NO_SHIFT != 0 {
                    out.push(c as u32);
                    *inpos += 1;
                } else {
                    state.c[4] |= F_SHIFTED;
                    *inpos += 1;
                }
            }
            LF => {
                state.c[4] &= !F_SHIFTED;
                out.push(LF as u32);
                *inpos += 1;
            }
            _ => {
                if c < 0x20 {
                    // C0
                    out.push(c as u32);
                    *inpos += 1;
                } else if c >= 0x80 {
                    return 1;
                } else {
                    let charset = if state.c[4] & F_SHIFTED != 0 {
                        state.c[1]
                    } else {
                        state.c[0]
                    };

                    if charset == CHARSET_ASCII {
                        out.push(c as u32);
                        *inpos += 1;
                        continue;
                    }

                    let dsg = config.designations.iter().find(|dsg| dsg.mark == charset);
                    let Some(dsg) = dsg else {
                        return MBERR_INTERNAL;
                    };

                    if data.len() - *inpos < dsg.width as usize {
                        return MBERR_TOOFEW;
                    }
                    let decoded = (dsg.decoder)(&data[*inpos..]);
                    if decoded == MAP_UNMAPPABLE as u32 {
                        return dsg.width as i64;
                    }

                    if decoded < 0x30000 {
                        out.push(decoded);
                    } else {
                        // JIS X 0213 pairs
                        out.push(decoded >> 16);
                        out.push(decoded & 0xffff);
                    }
                    *inpos += dsg.width as usize;
                }
            }
        }
    }
    0
}

// ------------------------------------------------- mapping accessors

fn ksx1001_decoder(data: &[u8]) -> u32 {
    trymap_dec(&kr::KSX1001_DEC, data[0], data[1]).unwrap_or(MAP_UNMAPPABLE as u32)
}

fn ksx1001_encoder(data: &[u32], _length: &mut isize) -> u16 {
    if data[0] < 0x10000
        && let Some(coded) = trymap_enc(&kr::CP949_ENC, data[0])
        && coded & 0x8000 == 0
    {
        return coded;
    }
    MAP_UNMAPPABLE
}

fn jisx0208_decoder(data: &[u8]) -> u32 {
    if data[0] == 0x21 && data[1] == 0x40 {
        // F/W REVERSE SOLIDUS
        0xff3c
    } else {
        trymap_dec(&jp::JISX0208_DEC, data[0], data[1]).unwrap_or(MAP_UNMAPPABLE as u32)
    }
}

fn jisx0208_encoder(data: &[u32], _length: &mut isize) -> u16 {
    if data[0] < 0x10000 {
        if data[0] == 0xff3c {
            // F/W REVERSE SOLIDUS
            return 0x2140;
        }
        if let Some(coded) = trymap_enc(&jp::JISXCOMMON_ENC, data[0])
            && coded & 0x8000 == 0
        {
            return coded;
        }
    }
    MAP_UNMAPPABLE
}

fn jisx0212_decoder(data: &[u8]) -> u32 {
    trymap_dec(&jp::JISX0212_DEC, data[0], data[1]).unwrap_or(MAP_UNMAPPABLE as u32)
}

fn jisx0212_encoder(data: &[u32], _length: &mut isize) -> u16 {
    if data[0] < 0x10000
        && let Some(coded) = trymap_enc(&jp::JISXCOMMON_ENC, data[0])
        && coded & 0x8000 != 0
    {
        return coded & 0x7fff;
    }
    MAP_UNMAPPABLE
}

/// The JIS X 0213 plane 2:2000 decoder's emulation macro is a plain
/// `if` whose assignment the table chain then overwrites or discards,
/// so the 2004 table value is what surfaces; kept that way to match
/// CPython observable behavior.
fn jisx0213_2000_2_decoder(data: &[u8]) -> u32 {
    if let Some(u) = trymap_dec(&jp::JISX0213_2_BMP_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_2_EMP_DEC, data[0], data[1]) {
        return u | 0x20000;
    }
    MAP_UNMAPPABLE as u32
}

fn jisx0213_2000_1_decoder(data: &[u8]) -> u32 {
    if is_2000_invalid_plane1(data[0], data[1]) {
        return MAP_UNMAPPABLE as u32;
    }
    if data[0] == 0x21 && data[1] == 0x40 {
        // F/W REVERSE SOLIDUS
        return 0xff3c;
    }
    if let Some(u) = trymap_dec(&jp::JISX0208_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_1_BMP_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_1_EMP_DEC, data[0], data[1]) {
        return u | 0x20000;
    }
    if let Some(u) = trymap_wide_dec(&jp::JISX0213_PAIR_DEC, data[0], data[1]) {
        return u;
    }
    MAP_UNMAPPABLE as u32
}

fn jisx0213_2004_1_decoder(data: &[u8]) -> u32 {
    if data[0] == 0x21 && data[1] == 0x40 {
        // F/W REVERSE SOLIDUS
        return 0xff3c;
    }
    if let Some(u) = trymap_dec(&jp::JISX0208_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_1_BMP_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_1_EMP_DEC, data[0], data[1]) {
        return u | 0x20000;
    }
    if let Some(u) = trymap_wide_dec(&jp::JISX0213_PAIR_DEC, data[0], data[1]) {
        return u;
    }
    MAP_UNMAPPABLE as u32
}

fn jisx0213_2004_2_decoder(data: &[u8]) -> u32 {
    if let Some(u) = trymap_dec(&jp::JISX0213_2_BMP_DEC, data[0], data[1]) {
        return u;
    }
    if let Some(u) = trymap_dec(&jp::JISX0213_2_EMP_DEC, data[0], data[1]) {
        return u | 0x20000;
    }
    MAP_UNMAPPABLE as u32
}

/// `jisx0213_encoder`: the shared JIS X 0213 body, where `emu_2000`
/// is the `config == 2000` the emulation macros guard on.
fn jisx0213_encode_raw(data: &[u32], length: &mut isize, emu_2000: bool) -> u16 {
    match *length {
        1 => {
            // first character
            let c = data[0];
            if c >= 0x10000 {
                if c >> 16 == 0x20000 >> 16 {
                    if emu_2000 && c == 0x20B9F {
                        return MAP_UNMAPPABLE;
                    }
                    if let Some(coded) = trymap_enc(&jp::JISX0213_EMP_ENC, c & 0xffff) {
                        return coded;
                    }
                }
                return MAP_UNMAPPABLE;
            }

            if emu_2000 && is_2000_invalid_bmp(c) {
                MAP_UNMAPPABLE
            } else if emu_2000 && c == 0x9B1D {
                0x8000 | 0x7d3b
            } else if let Some(coded) = trymap_enc(&jp::JISX0213_BMP_ENC, c) {
                if coded == MULTIC {
                    MAP_MULTIPLE_AVAIL
                } else {
                    coded
                }
            } else if let Some(coded) = trymap_enc(&jp::JISXCOMMON_ENC, c) {
                if coded & 0x8000 != 0 {
                    MAP_UNMAPPABLE
                } else {
                    coded
                }
            } else {
                MAP_UNMAPPABLE
            }
        }
        2 => {
            // second character of a unicode pair
            if let Some(coded) = find_pairencmap(data[0], data[1], &jp::JISX0213_PAIR_ENC) {
                return coded;
            }
            // flush the unterminated pair
            *length = 1;
            find_pairencmap(data[0], 0, &jp::JISX0213_PAIR_ENC).unwrap_or(MAP_UNMAPPABLE)
        }
        -1 => {
            // flush unterminated
            *length = 1;
            find_pairencmap(data[0], 0, &jp::JISX0213_PAIR_ENC).unwrap_or(MAP_UNMAPPABLE)
        }
        _ => MAP_UNMAPPABLE,
    }
}

fn jisx0213_2000_1_encoder(data: &[u32], length: &mut isize) -> u16 {
    let coded = jisx0213_encode_raw(data, length, true);
    if coded == MAP_UNMAPPABLE || coded == MAP_MULTIPLE_AVAIL {
        coded
    } else if coded & 0x8000 != 0 {
        MAP_UNMAPPABLE
    } else {
        coded
    }
}

fn jisx0213_2000_1_encoder_paironly(data: &[u32], length: &mut isize) -> u16 {
    let ilength = *length;
    let coded = jisx0213_encode_raw(data, length, true);
    match ilength {
        1 => {
            if coded == MAP_MULTIPLE_AVAIL {
                MAP_MULTIPLE_AVAIL
            } else {
                MAP_UNMAPPABLE
            }
        }
        2 => {
            if *length != 2 {
                MAP_UNMAPPABLE
            } else {
                coded
            }
        }
        _ => MAP_UNMAPPABLE,
    }
}

fn jisx0213_2000_2_encoder(data: &[u32], length: &mut isize) -> u16 {
    let coded = jisx0213_encode_raw(data, length, true);
    if coded == MAP_UNMAPPABLE || coded == MAP_MULTIPLE_AVAIL {
        coded
    } else if coded & 0x8000 != 0 {
        coded & 0x7fff
    } else {
        MAP_UNMAPPABLE
    }
}

fn jisx0213_2004_1_encoder(data: &[u32], length: &mut isize) -> u16 {
    let coded = jisx0213_encode_raw(data, length, false);
    if coded == MAP_UNMAPPABLE || coded == MAP_MULTIPLE_AVAIL {
        coded
    } else if coded & 0x8000 != 0 {
        MAP_UNMAPPABLE
    } else {
        coded
    }
}

fn jisx0213_2004_1_encoder_paironly(data: &[u32], length: &mut isize) -> u16 {
    let ilength = *length;
    let coded = jisx0213_encode_raw(data, length, false);
    match ilength {
        1 => {
            if coded == MAP_MULTIPLE_AVAIL {
                MAP_MULTIPLE_AVAIL
            } else {
                MAP_UNMAPPABLE
            }
        }
        2 => {
            if *length != 2 {
                MAP_UNMAPPABLE
            } else {
                coded
            }
        }
        _ => MAP_UNMAPPABLE,
    }
}

fn jisx0213_2004_2_encoder(data: &[u32], length: &mut isize) -> u16 {
    let coded = jisx0213_encode_raw(data, length, false);
    if coded == MAP_UNMAPPABLE || coded == MAP_MULTIPLE_AVAIL {
        coded
    } else if coded & 0x8000 != 0 {
        coded & 0x7fff
    } else {
        MAP_UNMAPPABLE
    }
}

fn jisx0201_r_iso2022_decoder(data: &[u8]) -> u32 {
    jisx0201_r_decode(data[0]).unwrap_or(MAP_UNMAPPABLE as u32)
}

fn jisx0201_r_iso2022_encoder(data: &[u32], _length: &mut isize) -> u16 {
    jisx0201_r_encode(data[0]).unwrap_or(MAP_UNMAPPABLE as u32) as u16
}

fn jisx0201_k_decoder(data: &[u8]) -> u32 {
    let c = data[0] ^ 0x80;
    if (0xa1..=0xdf).contains(&c) {
        0xfec0 + c as u32
    } else {
        MAP_UNMAPPABLE as u32
    }
}

fn jisx0201_k_encoder(data: &[u32], _length: &mut isize) -> u16 {
    match jisx0201_k_encode(data[0]) {
        Some(coded) => (coded - 0x80) as u16,
        None => MAP_UNMAPPABLE,
    }
}

fn gb2312_decoder(data: &[u8]) -> u32 {
    trymap_dec(&cn::GB2312_DEC, data[0], data[1]).unwrap_or(MAP_UNMAPPABLE as u32)
}

fn gb2312_encoder(data: &[u32], _length: &mut isize) -> u16 {
    if data[0] < 0x10000
        && let Some(coded) = trymap_enc(&cn::GBCOMMON_ENC, data[0])
        && coded & 0x8000 == 0
    {
        return coded;
    }
    MAP_UNMAPPABLE
}

fn dummy_decoder(_data: &[u8]) -> u32 {
    MAP_UNMAPPABLE as u32
}

fn dummy_encoder(_data: &[u32], _length: &mut isize) -> u16 {
    MAP_UNMAPPABLE
}

// -------------------------------------------------------- registries

static DESIGNATIONS_KR: [IsoDesignation; 1] = [IsoDesignation {
    mark: CHARSET_KSX1001,
    plane: 1,
    width: 2,
    decoder: ksx1001_decoder,
    encoder: ksx1001_encoder,
}];

static DESIGNATIONS_JP: [IsoDesignation; 3] = [
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0201_R,
        plane: 0,
        width: 1,
        decoder: jisx0201_r_iso2022_decoder,
        encoder: jisx0201_r_iso2022_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208_O,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
];

static DESIGNATIONS_JP_1: [IsoDesignation; 4] = [
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0212,
        plane: 0,
        width: 2,
        decoder: jisx0212_decoder,
        encoder: jisx0212_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0201_R,
        plane: 0,
        width: 1,
        decoder: jisx0201_r_iso2022_decoder,
        encoder: jisx0201_r_iso2022_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208_O,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
];

static DESIGNATIONS_JP_2: [IsoDesignation; 8] = [
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0212,
        plane: 0,
        width: 2,
        decoder: jisx0212_decoder,
        encoder: jisx0212_encoder,
    },
    IsoDesignation {
        mark: CHARSET_KSX1001,
        plane: 0,
        width: 2,
        decoder: ksx1001_decoder,
        encoder: ksx1001_encoder,
    },
    IsoDesignation {
        mark: CHARSET_GB2312,
        plane: 0,
        width: 2,
        decoder: gb2312_decoder,
        encoder: gb2312_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0201_R,
        plane: 0,
        width: 1,
        decoder: jisx0201_r_iso2022_decoder,
        encoder: jisx0201_r_iso2022_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208_O,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_ISO8859_1,
        plane: 2,
        width: 1,
        decoder: dummy_decoder,
        encoder: dummy_encoder,
    },
    IsoDesignation {
        mark: CHARSET_ISO8859_7,
        plane: 2,
        width: 1,
        decoder: dummy_decoder,
        encoder: dummy_encoder,
    },
];

static DESIGNATIONS_JP_2004: [IsoDesignation; 4] = [
    IsoDesignation {
        mark: CHARSET_JISX0213_2004_1,
        plane: 0,
        width: 2,
        decoder: jisx0213_2004_1_decoder,
        encoder: jisx0213_2004_1_encoder_paironly,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0213_2004_1,
        plane: 0,
        width: 2,
        decoder: jisx0213_2004_1_decoder,
        encoder: jisx0213_2004_1_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0213_2,
        plane: 0,
        width: 2,
        decoder: jisx0213_2004_2_decoder,
        encoder: jisx0213_2004_2_encoder,
    },
];

static DESIGNATIONS_JP_3: [IsoDesignation; 4] = [
    IsoDesignation {
        mark: CHARSET_JISX0213_2000_1,
        plane: 0,
        width: 2,
        decoder: jisx0213_2000_1_decoder,
        encoder: jisx0213_2000_1_encoder_paironly,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0213_2000_1,
        plane: 0,
        width: 2,
        decoder: jisx0213_2000_1_decoder,
        encoder: jisx0213_2000_1_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0213_2,
        plane: 0,
        width: 2,
        decoder: jisx0213_2000_2_decoder,
        encoder: jisx0213_2000_2_encoder,
    },
];

static DESIGNATIONS_JP_EXT: [IsoDesignation; 5] = [
    IsoDesignation {
        mark: CHARSET_JISX0208,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0212,
        plane: 0,
        width: 2,
        decoder: jisx0212_decoder,
        encoder: jisx0212_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0201_R,
        plane: 0,
        width: 1,
        decoder: jisx0201_r_iso2022_decoder,
        encoder: jisx0201_r_iso2022_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0201_K,
        plane: 0,
        width: 1,
        decoder: jisx0201_k_decoder,
        encoder: jisx0201_k_encoder,
    },
    IsoDesignation {
        mark: CHARSET_JISX0208_O,
        plane: 0,
        width: 2,
        decoder: jisx0208_decoder,
        encoder: jisx0208_encoder,
    },
];

/// The module's codec table; `config` indexes [`CONFIGS`].
static CODECS: [&MbCodec; 7] = [
    &MbCodec {
        encoding: "iso2022_kr",
        config: 0,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp",
        config: 1,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp_1",
        config: 2,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp_2",
        config: 3,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp_2004",
        config: 4,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp_3",
        config: 5,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
    &MbCodec {
        encoding: "iso2022_jp_ext",
        config: 6,
        encode: iso2022_encode,
        encinit: Some(iso2022_encinit),
        encreset: Some(iso2022_encreset),
        decode: iso2022_decode,
        decinit: Some(iso2022_decinit),
        decreset: Some(iso2022_decreset),
    },
];

/// `_codecs_iso2022.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

//! The `_codecs_kr` module (spec section 10.6): euc_kr, cp949 and
//! johab, a direct port of `Modules/cjkcodecs/_codecs_kr.c` over the
//! generated tables, including the KS X 1001:1998 make-up jamo
//! sequences.

use super::_multibytecodec::{MbCodec, MbState, lookup_codec};
use super::cjk_data::{kr, trymap_dec, trymap_enc};

const EUCKR_JAMO_FIRSTBYTE: u8 = 0xA4;
const EUCKR_JAMO_FILLER: u8 = 0xD4;

const NONE: u8 = 127;
const JOHAB_NONE: u8 = 0xff;
const JOHAB_FILL: u8 = 0xfd;

static U2CGK_CHOSEONG: [u8; 19] = [
    0xa1, 0xa2, 0xa4, 0xa7, 0xa8, 0xa9, 0xb1, 0xb2, 0xb3, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbb,
    0xbc, 0xbd, 0xbe,
];
static U2CGK_JUNGSEONG: [u8; 21] = [
    0xbf, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xcb, 0xcc, 0xcd, 0xce,
    0xcf, 0xd0, 0xd1, 0xd2, 0xd3,
];
static U2CGK_JONGSEONG: [u8; 28] = [
    0xd4, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf, 0xb0,
    0xb1, 0xb2, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xba, 0xbb, 0xbc, 0xbd, 0xbe,
];

fn euc_kr_encode(
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
        let Some(code) = trymap_enc(&kr::CP949_ENC, c) else {
            return 1;
        };
        if code & 0x8000 == 0 {
            // KS X 1001 coded character.
            out.push(((code >> 8) | 0x80) as u8);
            out.push(((code & 0xFF) | 0x80) as u8);
        } else {
            // Found in the CP949 extension, but encoded in KS
            // X 1001:1998 as a make-up sequence.
            out.push(EUCKR_JAMO_FIRSTBYTE);
            out.push(EUCKR_JAMO_FILLER);
            let c = c - 0xac00;
            out.push(EUCKR_JAMO_FIRSTBYTE);
            out.push(U2CGK_CHOSEONG[(c / 588) as usize]);
            out.push(EUCKR_JAMO_FIRSTBYTE);
            out.push(U2CGK_JUNGSEONG[((c / 28) % 21) as usize]);
            out.push(EUCKR_JAMO_FIRSTBYTE);
            out.push(U2CGK_JONGSEONG[(c % 28) as usize]);
        }
        *inpos += 1;
    }
    0
}

static CGK2U_CHOSEONG: [u8; 30] = [
    0, 1, NONE, 2, NONE, NONE, 3, 4, 5, NONE, NONE, NONE, NONE, NONE, NONE, NONE, 6, 7, 8, NONE, 9,
    10, 11, 12, 13, 14, 15, 16, 17, 18,
];
static CGK2U_JONGSEONG: [u8; 30] = [
    1, 2, 3, 4, 5, 6, 7, NONE, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, NONE, 18, 19, 20, 21, 22,
    NONE, 23, 24, 25, 26, 27,
];

fn euc_kr_decode(
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
        if c == EUCKR_JAMO_FIRSTBYTE && data[*inpos + 1] == EUCKR_JAMO_FILLER {
            // KS X 1001:1998 make-up sequence.
            if data.len() - *inpos < 8 {
                return super::_multibytecodec::MBERR_TOOFEW;
            }
            if data[*inpos + 2] != EUCKR_JAMO_FIRSTBYTE
                || data[*inpos + 4] != EUCKR_JAMO_FIRSTBYTE
                || data[*inpos + 6] != EUCKR_JAMO_FIRSTBYTE
            {
                return 1;
            }
            let c = data[*inpos + 3];
            let cho = if (0xa1..=0xbe).contains(&c) {
                CGK2U_CHOSEONG[(c - 0xa1) as usize]
            } else {
                NONE
            };
            let c = data[*inpos + 5];
            let jung = if (0xbf..=0xd3).contains(&c) {
                c - 0xbf
            } else {
                NONE
            };
            let c = data[*inpos + 7];
            let jong = if c == EUCKR_JAMO_FILLER {
                0
            } else if (0xa1..=0xbe).contains(&c) {
                CGK2U_JONGSEONG[(c - 0xa1) as usize]
            } else {
                NONE
            };
            if cho == NONE || jung == NONE || jong == NONE {
                return 1;
            }
            out.push(0xac00 + cho as u32 * 588 + jung as u32 * 28 + jong as u32);
            *inpos += 8;
        } else if let Some(decoded) =
            trymap_dec(&kr::KSX1001_DEC, c ^ 0x80, data[*inpos + 1] ^ 0x80)
        {
            out.push(decoded);
            *inpos += 2;
        } else {
            return 1;
        }
    }
    0
}

fn cp949_encode(
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
        let Some(code) = trymap_enc(&kr::CP949_ENC, c) else {
            return 1;
        };
        out.push(((code >> 8) | 0x80) as u8);
        if code & 0x8000 != 0 {
            // MSB set: CP949.
            out.push((code & 0xFF) as u8);
        } else {
            // MSB unset: KS X 1001.
            out.push(((code & 0xFF) | 0x80) as u8);
        }
        *inpos += 1;
    }
    0
}

fn cp949_decode(
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
        let decoded = match trymap_dec(&kr::KSX1001_DEC, c ^ 0x80, c2 ^ 0x80) {
            Some(decoded) => decoded,
            None => match trymap_dec(&kr::CP949EXT_DEC, c, c2) {
                Some(decoded) => decoded,
                None => return 1,
            },
        };
        out.push(decoded);
        *inpos += 2;
    }
    0
}

static U2JOHABIDX_CHOSEONG: [u8; 19] = [
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
    0x12, 0x13, 0x14,
];
static U2JOHABIDX_JUNGSEONG: [u8; 21] = [
    0x03, 0x04, 0x05, 0x06, 0x07, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x17, 0x1a, 0x1b, 0x1c, 0x1d,
];
static U2JOHABIDX_JONGSEONG: [u8; 28] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    0x11, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
];
static U2JOHABJAMO: [u16; 51] = [
    0x8841, 0x8c41, 0x8444, 0x9041, 0x8446, 0x8447, 0x9441, 0x9841, 0x9c41, 0x844a, 0x844b, 0x844c,
    0x844d, 0x844e, 0x844f, 0x8450, 0xa041, 0xa441, 0xa841, 0x8454, 0xac41, 0xb041, 0xb441, 0xb841,
    0xbc41, 0xc041, 0xc441, 0xc841, 0xcc41, 0xd041, 0x8461, 0x8481, 0x84a1, 0x84c1, 0x84e1, 0x8541,
    0x8561, 0x8581, 0x85a1, 0x85c1, 0x85e1, 0x8641, 0x8661, 0x8681, 0x86a1, 0x86c1, 0x86e1, 0x8741,
    0x8761, 0x8781, 0x87a1,
];

fn johab_encode(
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
        let code;
        if (0xac00..=0xd7a3).contains(&c) {
            let c = c - 0xac00;
            code = 0x8000
                | ((U2JOHABIDX_CHOSEONG[(c / 588) as usize] as u16) << 10)
                | ((U2JOHABIDX_JUNGSEONG[((c / 28) % 21) as usize] as u16) << 5)
                | U2JOHABIDX_JONGSEONG[(c % 28) as usize] as u16;
        } else if (0x3131..=0x3163).contains(&c) {
            code = U2JOHABJAMO[(c - 0x3131) as usize];
        } else {
            let Some(mapped) = trymap_enc(&kr::CP949_ENC, c) else {
                return 1;
            };
            // KS X 1001 code points only; the CP949 extension has no
            // johab form.
            let c1 = (mapped >> 8) as u8;
            let c2 = (mapped & 0xff) as u8;
            if ((0x21..=0x2c).contains(&c1) || (0x4a..=0x7d).contains(&c1))
                && (0x21..=0x7e).contains(&c2)
            {
                let t1: u16 = if c1 < 0x4a {
                    c1 as u16 - 0x21 + 0x1b2
                } else {
                    c1 as u16 - 0x21 + 0x197
                };
                let t2 = (if t1 & 1 != 0 { 0x5e } else { 0 }) + c2 - 0x21;
                out.push((t1 >> 1) as u8);
                out.push(if t2 < 0x4e { t2 + 0x31 } else { t2 + 0x43 });
                *inpos += 1;
                continue;
            } else {
                return 1;
            }
        }
        out.push((code >> 8) as u8);
        out.push((code & 0xff) as u8);
        *inpos += 1;
    }
    0
}

static JOHABIDX_CHOSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_FILL, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
    0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE,
    JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE,
];
static JOHABIDX_JUNGSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_NONE, JOHAB_FILL, 0x00, 0x01, 0x02, 0x03, 0x04, JOHAB_NONE, JOHAB_NONE, 0x05,
    0x06, 0x07, 0x08, 0x09, 0x0a, JOHAB_NONE, JOHAB_NONE, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    JOHAB_NONE, JOHAB_NONE, 0x11, 0x12, 0x13, 0x14, JOHAB_NONE, JOHAB_NONE,
];
static JOHABIDX_JONGSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_FILL, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
    0x0d, 0x0e, 0x0f, 0x10, JOHAB_NONE, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a,
    0x1b, JOHAB_NONE, JOHAB_NONE,
];

static JOHABJAMO_CHOSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_FILL, 0x31, 0x32, 0x34, 0x37, 0x38, 0x39, 0x41, 0x42, 0x43, 0x45, 0x46, 0x47,
    0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE,
    JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE, JOHAB_NONE,
];
static JOHABJAMO_JUNGSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_NONE, JOHAB_FILL, 0x4f, 0x50, 0x51, 0x52, 0x53, JOHAB_NONE, JOHAB_NONE, 0x54,
    0x55, 0x56, 0x57, 0x58, 0x59, JOHAB_NONE, JOHAB_NONE, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
    JOHAB_NONE, JOHAB_NONE, 0x60, 0x61, 0x62, 0x63, JOHAB_NONE, JOHAB_NONE,
];
static JOHABJAMO_JONGSEONG: [u8; 32] = [
    JOHAB_NONE, JOHAB_FILL, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x39, 0x3a, 0x3b, 0x3c, 0x3d,
    0x3e, 0x3f, 0x40, 0x41, JOHAB_NONE, 0x42, 0x44, 0x45, 0x46, 0x47, 0x48, 0x4a, 0x4b, 0x4c, 0x4d,
    0x4e, JOHAB_NONE, JOHAB_NONE,
];

fn johab_decode(
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
        if c < 0xd8 {
            // johab hangul.
            let c_cho = (c >> 2) & 0x1f;
            let c_jung = ((c << 3) | c2 >> 5) & 0x1f;
            let c_jong = c2 & 0x1f;

            let i_cho = JOHABIDX_CHOSEONG[c_cho as usize];
            let i_jung = JOHABIDX_JUNGSEONG[c_jung as usize];
            let i_jong = JOHABIDX_JONGSEONG[c_jong as usize];

            if i_cho == JOHAB_NONE || i_jung == JOHAB_NONE || i_jong == JOHAB_NONE {
                return 1;
            }
            // U+1100 hangul jamo are not used.
            if i_cho == JOHAB_FILL {
                if i_jung == JOHAB_FILL {
                    if i_jong == JOHAB_FILL {
                        out.push(0x3000);
                    } else {
                        out.push(0x3100 | JOHABJAMO_JONGSEONG[c_jong as usize] as u32);
                    }
                } else if i_jong == JOHAB_FILL {
                    out.push(0x3100 | JOHABJAMO_JUNGSEONG[c_jung as usize] as u32);
                } else {
                    return 1;
                }
            } else if i_jung == JOHAB_FILL {
                if i_jong == JOHAB_FILL {
                    out.push(0x3100 | JOHABJAMO_CHOSEONG[c_cho as usize] as u32);
                } else {
                    return 1;
                }
            } else {
                let jong = if i_jong == JOHAB_FILL {
                    0
                } else {
                    i_jong as u32
                };
                out.push(0xac00 + i_cho as u32 * 588 + i_jung as u32 * 28 + jong);
            }
            *inpos += 2;
        } else {
            // KS X 1001 except hangul jamos and syllables.
            if c == 0xdf
                || c > 0xf9
                || c2 < 0x31
                || (0x80..0x91).contains(&c2)
                || c2 & 0x7f == 0x7f
                || (c == 0xda && (0xa1..=0xd3).contains(&c2))
            {
                return 1;
            }
            let mut t1 = if c < 0xe0 {
                2 * (c as u16 - 0xd9)
            } else {
                2 * c as u16 - 0x197
            };
            let mut t2 = if c2 < 0x91 { c2 - 0x31 } else { c2 - 0x43 };
            t1 += (if t2 < 0x5e { 0 } else { 1 }) + 0x21;
            t2 = (if t2 < 0x5e { t2 } else { t2 - 0x5e }) + 0x21;
            match trymap_dec(&kr::KSX1001_DEC, t1 as u8, t2) {
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

/// The module's codec table.
static CODECS: [&MbCodec; 3] = [
    &MbCodec {
        encoding: "euc_kr",
        config: 0,
        encode: euc_kr_encode,
        encinit: None,
        encreset: None,
        decode: euc_kr_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "cp949",
        config: 0,
        encode: cp949_encode,
        encinit: None,
        encreset: None,
        decode: cp949_decode,
        decinit: None,
        decreset: None,
    },
    &MbCodec {
        encoding: "johab",
        config: 0,
        encode: johab_encode,
        encinit: None,
        encreset: None,
        decode: johab_decode,
        decinit: None,
        decreset: None,
    },
];

/// `_codecs_kr.getcodec(encoding)`.
pub fn getcodec(encoding: &str) -> *mut coil_capi_sys::PyObject {
    lookup_codec(&CODECS, encoding)
}

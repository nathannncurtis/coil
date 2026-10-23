//! NFKC normalization for identifiers (UAX #15, Unicode 15.0.0).
//!
//! CPython applies NFKC to non-ASCII identifiers in
//! `_PyPegen_new_identifier`; the tables in `unicode_norm.rs` come from the
//! same Unicode version as the pinned CPython.

use super::unicode_norm::{COMBINING, COMPOSITIONS, DECOMPOSITIONS};

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

fn ccc(ch: u32) -> u8 {
    COMBINING
        .binary_search_by_key(&ch, |&(cp, _)| cp)
        .map_or(0, |i| COMBINING[i].1)
}

fn decomposition(ch: u32, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&ch) {
        let s_index = ch - S_BASE;
        let l = L_BASE + s_index / N_COUNT;
        let v = V_BASE + (s_index % N_COUNT) / T_COUNT;
        let t = s_index % T_COUNT;
        decomposition(l, out);
        decomposition(v, out);
        if t != 0 {
            decomposition(T_BASE + t, out);
        }
        return;
    }
    match DECOMPOSITIONS.binary_search_by_key(&ch, |&(cp, _)| cp) {
        Ok(i) => {
            for &c in DECOMPOSITIONS[i].1 {
                decomposition(c, out);
            }
        }
        Err(_) => out.push(ch),
    }
}

fn compose(starter: u32, second: u32) -> Option<u32> {
    COMPOSITIONS
        .binary_search_by(|&(first, sec, _)| (first, sec).cmp(&(starter, second)))
        .ok()
        .map(|i| COMPOSITIONS[i].2)
}

pub fn nfkc(s: &[u32]) -> Vec<u32> {
    // NFKD: full decomposition.
    let mut decomposed = Vec::with_capacity(s.len());
    for &ch in s {
        decomposition(ch, &mut decomposed);
    }

    // Canonical ordering: stable sort of each non-starter run by class.
    let mut chars: Vec<(u32, u8)> = decomposed.into_iter().map(|c| (c, ccc(c))).collect();
    let mut i = 1;
    while i < chars.len() {
        if chars[i].1 == 0 {
            i += 1;
            continue;
        }
        let mut j = i;
        while j > 0 && chars[j - 1].1 > chars[j].1 {
            chars.swap(j - 1, j);
            j -= 1;
        }
        i += 1;
    }

    // Canonical composition.
    let mut out: Vec<u32> = Vec::with_capacity(chars.len());
    let mut last_starter = usize::MAX;
    let mut last_cc = 0u8;
    for &(ch, class) in &chars {
        if last_starter != usize::MAX && (last_cc < class || last_cc == 0) {
            let starter = out[last_starter];
            let hangul = if (L_BASE..L_BASE + L_COUNT).contains(&starter)
                && (V_BASE..V_BASE + V_COUNT).contains(&ch)
            {
                let l = starter - L_BASE;
                let v = ch - V_BASE;
                Some(S_BASE + (l * N_COUNT + v * T_COUNT))
            } else if (S_BASE..S_BASE + S_COUNT).contains(&starter)
                && (starter - S_BASE).is_multiple_of(T_COUNT)
                && (T_BASE + 1..T_BASE + T_COUNT).contains(&ch)
            {
                Some(starter + (ch - T_BASE))
            } else {
                None
            };
            if let Some(c) = hangul.or_else(|| compose(starter, ch)) {
                out[last_starter] = c;
                continue;
            }
        }
        if class == 0 {
            last_starter = out.len();
        }
        last_cc = class;
        out.push(ch);
    }
    out
}

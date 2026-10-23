//! Interner for AST identifiers and strings, which ASDL represents as code
//! point sequences because Python strings may contain lone surrogates.

use std::collections::HashMap;

/// An interned string. Equality is index equality within one [`Interner`];
/// symbols from different interners never compare meaningfully.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sym(u32);

#[derive(Default)]
pub struct Interner {
    strings: Vec<Box<[u32]>>,
    lookup: HashMap<Box<[u32]>, u32>,
}

impl Interner {
    pub fn new() -> Interner {
        Interner::default()
    }

    pub fn intern(&mut self, text: &str) -> Sym {
        self.intern_code_points(text.chars().map(u32::from))
    }

    pub fn intern_code_points(&mut self, code_points: impl IntoIterator<Item = u32>) -> Sym {
        let code_points: Box<[u32]> = code_points.into_iter().collect();
        for &code_point in &code_points {
            assert!(
                code_point <= 0x10FFFF,
                "code point {code_point:#x} is not a scalar value"
            );
        }
        match self.lookup.get(code_points.as_ref()) {
            Some(&index) => Sym(index),
            None => {
                let index = self.strings.len() as u32;
                self.strings.push(code_points.clone());
                self.lookup.insert(code_points, index);
                Sym(index)
            }
        }
    }

    pub fn resolve(&self, sym: Sym) -> &[u32] {
        &self.strings[sym.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_equal_text_once() {
        let mut interner = Interner::new();
        let first = interner.intern("value");
        let second = interner.intern("value");
        assert_eq!(first, second);
        assert_eq!(interner.resolve(first), &[118, 97, 108, 117, 101]);
    }

    #[test]
    fn interns_lone_surrogates_and_astral_code_points() {
        let mut interner = Interner::new();
        let sym = interner.intern_code_points([0xD800, 0x1F600]);
        assert_eq!(interner.resolve(sym), &[0xD800, 0x1F600]);
    }

    #[test]
    #[should_panic]
    fn rejects_non_scalar_code_points() {
        Interner::new().intern_code_points([0x110000]);
    }
}

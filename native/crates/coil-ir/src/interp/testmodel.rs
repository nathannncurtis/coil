//! The test object model: a self-contained Python-value semantics sufficient
//! for the behavioural corpus (spec section 5). `Val` is `Rc<Obj>`; integers
//! are arbitrary-precision; sets replicate CPython's table layout so their
//! iteration order matches; exceptions carry tracebacks of line numbers.
//!
//! This is a test double for build step 1's runtime, not a runtime: modules
//! are `collections.abc` and `typing` stubs, and the builtin set is what the
//! corpus uses.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::interp::model::{ErrorTag, IrFunction, ObjectModel, UnboxError};
use crate::interp::{Frame, Interp, Resume};
use crate::ir::{BinOp, CmpOp, UnaryOp};
use crate::types::{FnId, Type};

pub type Val = Rc<Obj>;

/// A native function: `(args, kwargs)` with the receiver first when bound.
pub type NativeFn =
    fn(&mut TestModel, &mut Interp<'_, TestModel>, &[Val], &[(&str, Val)]) -> Result<Val, Val>;

pub enum Obj {
    None_,
    Bool(bool),
    Int(Rc<BigInt>),
    Float(f64),
    Ellipsis,
    NotImplemented,
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    Tuple(Rc<Vec<Val>>),
    List(RefCell<Vec<Val>>),
    Dict(RefCell<Dict>),
    Set(RefCell<SetTable>),
    FrozenSet(RefCell<SetTable>),
    Slice {
        start: Option<Val>,
        stop: Option<Val>,
        step: Option<Val>,
    },
    Range {
        start: i128,
        stop: i128,
        step: i128,
    },
    Cell(RefCell<Option<Val>>),
    Function(Rc<FuncObj>),
    Native(&'static str, NativeFn),
    BoundMethod {
        recv: Val,
        func: Val,
    },
    Class(Rc<ClassObj>),
    Instance(Rc<InstObj>),
    Property {
        fget: Option<Val>,
        fset: Option<Val>,
        fdel: Option<Val>,
    },
    StaticMethod(Val),
    ClassMethod(Val),
    Super(Rc<SuperObj>),
    Module(Rc<ModuleObj>),
    Generator(Rc<GenState>),
    // iterators
    IterList {
        list: Val,
        pos: Cell<usize>,
    },
    IterTuple {
        items: Rc<Vec<Val>>,
        pos: Cell<usize>,
    },
    IterRange {
        cur: Cell<i128>,
        step: i128,
        stop: i128,
    },
    IterStr {
        s: Rc<str>,
        pos: Cell<usize>,
    },
    IterBytes {
        b: Rc<[u8]>,
        pos: Cell<usize>,
    },
    IterDict {
        dict: Val,
        pos: Cell<usize>,
        len: usize,
    },
    IterSet {
        set: Val,
        pos: Cell<usize>,
    },
    /// An object whose `__next__` is called each step (from `__iter__`).
    IterObj {
        obj: Val,
    },
    /// The `__getitem__` sequence protocol (legacy iterable).
    IterSeq {
        obj: Val,
        pos: Cell<i128>,
    },
    IterMap {
        f: Val,
        it: Val,
        // multi-iterator map: each turn's zip tuple is spread as arguments
        spread: bool,
    },
    IterFilter {
        f: Option<Val>,
        it: Val,
    },
    IterZip {
        its: Vec<Val>,
    },
    IterEnum {
        it: Val,
        count: Cell<i128>,
    },
    IterRevGetItem {
        obj: Val,
        pos: Cell<i128>,
    },
    /// `agen.asend(v)` / `agen.__anext__()`: a lazy awaitable that forwards
    /// each send into the async generator and yields what it yields;
    /// the generator's `StopAsyncIteration` surfaces as the awaiter's
    /// exception, and other errors pass through. `pending` holds the value
    /// `asend` was called with for the first drive.
    AsendAwaiter {
        agen: Val,
        pending: Cell<Option<Val>>,
    },
    IterRevList {
        list: Val,
        pos: Cell<usize>,
    },
    Traceback,
    // typing-module values: opaque to the corpus beyond identity
    TypeVar(Rc<str>),
    TypeVarTuple(Rc<str>),
    ParamSpec(Rc<str>),
    GenericAlias(Val),
    TypeAlias(Rc<str>, Val),
}

pub struct FuncObj {
    pub ir: Option<IrFunction<Val>>,
    pub native: Option<(&'static str, NativeFn)>,
    pub name: RefCell<String>,
    pub qualname: RefCell<String>,
    pub attrs: RefCell<AttrMap>,
}

pub struct ClassObj {
    pub name: String,
    pub bases: Vec<Val>,
    pub mro: Vec<Val>,
    pub dict: RefCell<AttrMap>,
    pub kind: ClassKind,
    /// The predicate an ABC uses in `isinstance` (sequence/mapping).
    pub abc: Option<AbcKind>,
    pub module: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassKind {
    Object,
    Type,
    /// A builtin type whose instances are `Obj` variants.
    Builtin(BuiltinType),
    /// A builtin or user exception class.
    Exception {
        base: bool,
    },
    User,
    Abc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuiltinType {
    None,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    List,
    Tuple,
    Dict,
    Set,
    FrozenSet,
    Slice,
    Range,
    Function,
    Module,
    Generator,
    Traceback,
    Super,
    Property,
    Ellipsis,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AbcKind {
    Sequence,
    Mapping,
    Iterable,
    Callable,
}

pub struct InstObj {
    pub class: Val,
    /// Instance attributes, held as a live dict so `obj.__dict__` shares
    /// storage with the instance.
    pub attrs: Val,
    pub exc: RefCell<Option<ExcData>>,
}

pub struct ExcData {
    pub args: Vec<Val>,
    pub tb: Vec<(String, u32)>,
    pub cause: Option<Val>,
    pub context: Option<Val>,
    pub suppress_context: bool,
    /// `Some` for exception groups.
    pub group: Option<GroupData>,
}

pub struct GroupData {
    pub message: Val,
    pub children: Vec<Val>,
    /// `Some(root)` marks a group the interpreter's `except*` machinery
    /// derived from `root` (a `split` result or projection): such groups
    /// share the root's `__traceback__`/`__cause__`/`__context__`, which is
    /// how `_PyExc_PrepReraiseStar` tells a reraised group from a raised
    /// one. Constructor-made groups carry `None`.
    pub meta_root: Option<Val>,
}

pub struct SuperObj {
    pub typ: Val,
    pub obj: Option<Val>,
}

pub struct ModuleObj {
    pub name: String,
    pub dict: RefCell<AttrMap>,
}

/// A generator, coroutine or async generator: a suspended frame.
pub struct GenState {
    pub frame: RefCell<Option<Frame<TestModel>>>,
    pub running: Cell<bool>,
    pub started: Cell<bool>,
    pub kind: GenKind,
    pub ret: RefCell<Option<Val>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenKind {
    Generator,
    Coroutine,
    AsyncGen,
}

/// An attribute map: class dicts and instance dicts, small and ordered.
#[derive(Default)]
pub struct AttrMap {
    pub entries: Vec<(String, Val)>,
}

impl AttrMap {
    pub fn get(&self, name: &str) -> Option<&Val> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
    pub fn set(&mut self, name: &str, v: Val) {
        for (n, slot) in self.entries.iter_mut() {
            if n == name {
                *slot = v;
                return;
            }
        }
        self.entries.push((name.to_owned(), v));
    }
    pub fn del(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(n, _)| n != name);
        self.entries.len() != before
    }
}

// ---------------------------------------------------------------------------
// Arbitrary-precision integers: sign-magnitude, little-endian u32 limbs.
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BigInt {
    neg: bool,
    /// Little-endian magnitude, no trailing zero limbs; empty is zero.
    mag: Vec<u32>,
}

impl BigInt {
    pub fn zero() -> Self {
        BigInt {
            neg: false,
            mag: Vec::new(),
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v == 0 {
            return Self::zero();
        }
        let neg = v < 0;
        let u = v.unsigned_abs();
        let mut mag = vec![u as u32, (u >> 32) as u32];
        while mag.last() == Some(&0) {
            mag.pop();
        }
        BigInt { neg, mag }
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.neg && !self.mag.is_empty()
    }

    pub fn to_i64(&self) -> Option<i64> {
        match (self.mag.len(), self.neg) {
            (0, _) => Some(0),
            (1, false) => Some(self.mag[0] as i64),
            (1, true) => Some(-(self.mag[0] as i64)),
            (2, false) if (self.mag[1] as i64) < 0 && self.mag[1] >> 31 == 0 => None,
            (2, false) => {
                let v = self.mag[0] as u64 | ((self.mag[1] as u64) << 32);
                i64::try_from(v).ok()
            }
            (2, true) => {
                let v = self.mag[0] as u64 | ((self.mag[1] as u64) << 32);
                if v == 1 << 63 {
                    Some(i64::MIN)
                } else if v > i64::MAX as u64 {
                    None
                } else {
                    Some(-(v as i64))
                }
            }
            _ => None,
        }
    }

    pub fn to_i128(&self) -> Option<i128> {
        if self.mag.len() > 4 {
            return None;
        }
        let mut mag: u128 = 0;
        for limb in self.mag.iter().rev() {
            mag = (mag << 32) | (*limb as u128);
        }
        if self.neg {
            if mag > (1u128 << 127) {
                return None;
            }
            Some(0u128.wrapping_sub(mag) as i128)
        } else if mag > i128::MAX as u128 {
            None
        } else {
            Some(mag as i128)
        }
    }

    fn trim(mut self) -> Self {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
        self
    }

    fn cmp_mag(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for i in (0..a.len()).rev() {
            match a[i].cmp(&b[i]) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }

    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self.is_negative(), other.is_negative()) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => Self::cmp_mag(&self.mag, &other.mag),
            (true, true) => Self::cmp_mag(&other.mag, &self.mag),
        }
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
        let mut out = Vec::with_capacity(long.len() + 1);
        let mut carry = 0u64;
        for (i, l) in long.iter().enumerate() {
            let s = *l as u64 + short.get(i).copied().unwrap_or(0) as u64 + carry;
            out.push(s as u32);
            carry = s >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        out
    }

    /// `a - b`, requiring `a >= b` in magnitude.
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len());
        let mut borrow = 0i64;
        for (i, x) in a.iter().enumerate() {
            let d = *x as i64 - b.get(i).copied().unwrap_or(0) as i64 - borrow;
            if d < 0 {
                out.push((d + (1i64 << 32)) as u32);
                borrow = 1;
            } else {
                out.push(d as u32);
                borrow = 0;
            }
        }
        out
    }

    pub fn add(&self, other: &Self) -> Self {
        use std::cmp::Ordering;
        if self.neg == other.neg {
            BigInt {
                neg: self.neg,
                mag: Self::add_mag(&self.mag, &other.mag),
            }
            .trim()
        } else {
            match Self::cmp_mag(&self.mag, &other.mag) {
                Ordering::Equal => Self::zero(),
                Ordering::Greater => BigInt {
                    neg: self.neg,
                    mag: Self::sub_mag(&self.mag, &other.mag),
                }
                .trim(),
                Ordering::Less => BigInt {
                    neg: other.neg,
                    mag: Self::sub_mag(&other.mag, &self.mag),
                }
                .trim(),
            }
        }
    }

    pub fn neg(&self) -> Self {
        if self.is_zero() {
            Self::zero()
        } else {
            BigInt {
                neg: !self.neg,
                mag: self.mag.clone(),
            }
        }
    }

    pub fn abs(&self) -> Self {
        BigInt {
            neg: false,
            mag: self.mag.clone(),
        }
    }

    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.neg())
    }

    pub fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let mut out = vec![0u32; self.mag.len() + other.mag.len()];
        for (i, &a) in self.mag.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in other.mag.iter().enumerate() {
                let t = out[i + j] as u64 + a as u64 * b as u64 + carry;
                out[i + j] = t as u32;
                carry = t >> 32;
            }
            let mut k = i + other.mag.len();
            while carry != 0 {
                let t = out[k] as u64 + carry;
                out[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        BigInt {
            neg: self.neg != other.neg,
            mag: out,
        }
        .trim()
    }

    fn mul_small(&self, m: u32) -> Self {
        if m == 0 || self.is_zero() {
            return Self::zero();
        }
        let mut out = Vec::with_capacity(self.mag.len() + 1);
        let mut carry = 0u64;
        for &l in &self.mag {
            let t = l as u64 * m as u64 + carry;
            out.push(t as u32);
            carry = t >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        BigInt {
            neg: self.neg,
            mag: out,
        }
        .trim()
    }

    fn add_small(&self, m: u32) -> Self {
        self.add(&BigInt {
            neg: false,
            mag: vec![m],
        })
    }

    /// Truncated division by a small positive divisor: `(quotient, rest)`.
    fn divmod_small(&self, m: u32) -> (Self, u32) {
        let mut q = vec![0u32; self.mag.len()];
        let mut rem = 0u64;
        for i in (0..self.mag.len()).rev() {
            let cur = (rem << 32) | self.mag[i] as u64;
            q[i] = (cur / m as u64) as u32;
            rem = cur % m as u64;
        }
        (
            BigInt {
                neg: self.neg,
                mag: q,
            }
            .trim(),
            rem as u32,
        )
    }

    /// Knuth algorithm D over magnitudes: `num` little-endian.
    fn knuth_div(mut num: Vec<u32>, den: &[u32]) -> (Vec<u32>, Vec<u32>) {
        let n = den.len();
        if n == 1 {
            let d = den[0] as u64;
            let mut q = vec![0u32; num.len()];
            let mut rem = 0u64;
            for i in (0..num.len()).rev() {
                let cur = (rem << 32) | num[i] as u64;
                q[i] = (cur / d) as u32;
                rem = cur % d;
            }
            let r = vec![rem as u32];
            return (q, r);
        }
        // normalize: shift so the divisor's top limb has its high bit set
        let shift = den[n - 1].leading_zeros();
        let den: Vec<u32> = shl_limbs(den, shift);
        num = shl_limbs(&num, shift);
        if num.len() == den.len() {
            num.push(0);
        } else if num.len() < den.len() {
            num.resize(den.len() + 1, 0);
        }
        let m = num.len() - n - 1;
        let mut q = vec![0u32; m + 1];
        let top = den[n - 1] as u64;
        let second = den[n - 2] as u64;
        for j in (0..=m).rev() {
            let hi = ((num[j + n] as u64) << 32) | num[j + n - 1] as u64;
            let mut qhat = hi / top;
            let mut rhat = hi % top;
            while qhat >= 1 << 32 || qhat * second > (rhat << 32) + num[j + n - 2] as u64 {
                qhat -= 1;
                rhat += top;
                if rhat >= 1 << 32 {
                    break;
                }
            }
            // multiply and subtract
            let mut borrow: i64 = 0;
            let mut carry: u64 = 0;
            for i in 0..n {
                let p = qhat * den[i] as u64 + carry;
                carry = p >> 32;
                let d = (num[i + j] as i64) - ((p & 0xffff_ffff) as i64) + borrow;
                num[i + j] = d as u32;
                borrow = d >> 32;
            }
            let d = (num[j + n] as i64) - (carry as i64) + borrow;
            num[j + n] = d as u32;
            borrow = d >> 32;
            if borrow != 0 {
                // qhat was one too large: add the divisor back
                qhat -= 1;
                let mut carry2 = 0u64;
                for i in 0..n {
                    let t = num[i + j] as u64 + den[i] as u64 + carry2;
                    num[i + j] = t as u32;
                    carry2 = t >> 32;
                }
                num[j + n] = (num[j + n] as u64 + carry2) as u32;
            }
            q[j] = qhat as u32;
        }
        let mut rem = num[..n].to_vec();
        if shift > 0 {
            rem = shr_limbs(&rem, shift);
        }
        (q, rem)
    }

    /// Floor division and modulo; `None` when `other` is zero.
    pub fn divmod(&self, other: &Self) -> Option<(Self, Self)> {
        use std::cmp::Ordering;
        if other.is_zero() {
            return None;
        }
        // Floor division: adjust quotient and remainder toward negative
        // infinity whenever the signs differ, including the |self| < |other|
        // case (Python: -8 % 26 == 18, -8 // 26 == -1).
        let (mut quo, mut rem) = if Self::cmp_mag(&self.mag, &other.mag) == Ordering::Less {
            (Self::zero(), self.clone())
        } else {
            let (q, r) = Self::knuth_div(self.mag.clone(), &other.mag);
            (
                BigInt {
                    neg: self.neg != other.neg,
                    mag: q,
                }
                .trim(),
                BigInt {
                    neg: self.neg,
                    mag: r,
                }
                .trim(),
            )
        };
        if !rem.is_zero() && (rem.is_negative() != other.is_negative()) {
            rem = rem.add(other);
            quo = quo.sub(&BigInt::from_i64(1));
        }
        Some((quo, rem))
    }

    pub fn pow(&self, e: u64) -> Self {
        let mut result = BigInt::from_i64(1);
        let mut base = self.clone();
        let mut exp = e;
        while exp > 0 {
            if exp & 1 == 1 {
                result = result.mul(&base);
            }
            exp >>= 1;
            if exp > 0 {
                base = base.mul(&base);
            }
        }
        result
    }

    pub fn bit_len(&self) -> usize {
        match self.mag.last() {
            None => 0,
            Some(&top) => self.mag.len() * 32 - top.leading_zeros() as usize,
        }
    }

    /// `self >> n`: logical shift of the magnitude.
    fn shr_pos(&self, n: usize) -> Self {
        let limb = n / 32;
        if limb >= self.mag.len() {
            return Self::zero();
        }
        let mut out = self.mag[limb..].to_vec();
        let bit = n % 32;
        if bit > 0 {
            for i in 0..out.len() {
                let hi = if i + 1 < out.len() { out[i + 1] } else { 0 };
                out[i] = (out[i] >> bit) | (hi << (32 - bit));
            }
        }
        BigInt {
            neg: false,
            mag: out,
        }
        .trim()
    }

    /// `self >> n` arithmetic (floor) for non-negative `n`.
    pub fn shr(&self, n: usize) -> Self {
        let magnitude = self.abs().shr_pos(n);
        if !self.is_negative() {
            magnitude
        } else {
            let floor_exact = magnitude.shl(n).eq_mag(&self.abs());
            if floor_exact {
                magnitude.neg()
            } else {
                magnitude.add(&BigInt::from_i64(1)).neg()
            }
        }
    }

    /// `self << n` for non-negative `n`.
    pub fn shl(&self, n: usize) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        let mag = shl_limbs(&self.mag, (n % 32) as u32);
        let mut out = vec![0u32; n / 32];
        out.extend(mag);
        BigInt {
            neg: self.neg,
            mag: out,
        }
        .trim()
    }

    fn eq_mag(&self, other: &Self) -> bool {
        self.mag == other.mag
    }

    /// Two's-complement limbs of `n` bits (little-endian).
    fn to_twos(&self, bits: usize) -> Vec<u32> {
        let limbs = bits.div_ceil(32);
        let mut v = vec![0u32; limbs];
        for (i, &l) in self.mag.iter().enumerate() {
            if i < limbs {
                v[i] = l;
            }
        }
        if self.is_negative() {
            // negate: invert and add one
            for l in v.iter_mut() {
                *l = !*l;
            }
            let mut carry = 1u64;
            for l in v.iter_mut() {
                let t = *l as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
                if carry == 0 {
                    break;
                }
            }
        }
        v
    }

    fn from_twos(v: &[u32]) -> Self {
        let top_set = v.last().map(|&l| l >> 31 == 1).unwrap_or(false);
        if top_set {
            // negative: invert and add one, then negate
            let mut inv: Vec<u32> = v.iter().map(|&l| !l).collect();
            let mut carry = 1u64;
            for l in inv.iter_mut() {
                let t = *l as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
                if carry == 0 {
                    break;
                }
            }
            BigInt {
                neg: true,
                mag: inv,
            }
            .trim()
        } else {
            BigInt {
                neg: false,
                mag: v.to_vec(),
            }
            .trim()
        }
    }

    pub fn bitop(&self, other: &Self, op: u8) -> Self {
        // op: 0 = and, 1 = or, 2 = xor
        let bits = self.bit_len().max(other.bit_len()) + 1;
        let a = self.to_twos(bits);
        let b = other.to_twos(bits);
        let mut out = Vec::with_capacity(a.len());
        for (i, x) in a.iter().enumerate() {
            let x = *x;
            let y = b
                .get(i)
                .copied()
                .unwrap_or_else(|| if other.is_negative() { u32::MAX } else { 0 });
            out.push(match op {
                0 => x & y,
                1 => x | y,
                _ => x ^ y,
            });
        }
        // sign-extend b was already sized; a likewise
        Self::from_twos(&out)
    }

    pub fn invert(&self) -> Self {
        self.neg().sub(&BigInt::from_i64(1))
    }

    pub fn parse(s: &str) -> Option<Self> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut v = BigInt::zero();
        let bytes = digits.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let take = (bytes.len() - i).min(9);
            let chunk: u32 = digits[i..i + take].parse().ok()?;
            v = v.mul_small(10u32.pow(take as u32)).add_small(chunk);
            i += take;
        }
        v.neg = neg && !v.is_zero();
        Some(v)
    }

    pub fn to_string_radix(&self, radix: u32) -> String {
        let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut n = self.abs();
        if n.is_zero() {
            return "0".to_string();
        }
        let mut out = Vec::new();
        while !n.is_zero() {
            let (q, r) = n.divmod_small(radix);
            out.push(digits[r as usize]);
            n = q;
        }
        out.reverse();
        String::from_utf8(out).unwrap_or_default()
    }

    fn to_decimal(&self) -> String {
        if self.is_zero() {
            return "0".to_owned();
        }
        let mut chunks: Vec<u32> = Vec::new();
        let mut cur = self.abs();
        while !cur.is_zero() {
            let (q, r) = cur.divmod_small(1_000_000_000);
            chunks.push(r);
            cur = q;
        }
        let mut s = String::new();
        if self.is_negative() {
            s.push('-');
        }
        for (i, c) in chunks.iter().rev().enumerate() {
            if i == 0 {
                s.push_str(&c.to_string());
            } else {
                s.push_str(&format!("{c:09}"));
            }
        }
        s
    }

    /// CPython's `int.__hash__`: `|x| mod (2^61 - 1)`, negated, `-1 -> -2`.
    pub fn py_hash(&self) -> i64 {
        const MOD: u128 = (1u128 << 61) - 1;
        let mut acc: u128 = 0;
        for &l in self.mag.iter().rev() {
            acc = ((acc << 32) + l as u128) % MOD;
        }
        let mut h = acc as i64;
        if self.is_negative() {
            h = -h;
        }
        if h == -1 {
            h = -2;
        }
        h
    }

    /// Correctly-rounded conversion; `None` over the float range.
    pub fn to_f64(&self) -> Option<f64> {
        if self.is_zero() {
            return Some(0.0);
        }
        let bits = self.bit_len();
        if bits as i64 > 1024 {
            return None;
        }
        // top 54 bits for round-to-nearest-even at 53
        const WANT: i64 = 54;
        let shift = bits as i64 - WANT;
        let top = if shift > 0 {
            self.abs().shr(shift as usize)
        } else {
            self.abs().shl((-shift) as usize)
        };
        let m = top.to_i64()? as u64;
        let sticky = if shift > 0 {
            let back = top.shl(shift as usize);
            !back.eq_mag(&self.abs())
        } else {
            false
        };
        let mut mant = m >> 1;
        let round_bit = m & 1;
        if round_bit == 1 && (sticky || mant & 1 == 1) {
            mant += 1;
        }
        let e = shift + 1;
        let v = (mant as f64) * two_pow(e);
        Some(if self.is_negative() { -v } else { v })
    }
}

impl std::fmt::Display for BigInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_decimal())
    }
}

fn two_pow(e: i64) -> f64 {
    if e >= 0 {
        (1u64 << e.min(62)) as f64
            * if e > 62 {
                2f64.powi((e - 62) as i32)
            } else {
                1.0
            }
    } else {
        2f64.powi(e as i32)
    }
}

trait VecTrim {
    fn trim(self) -> Self;
}

impl VecTrim for Vec<u32> {
    fn trim(mut self) -> Self {
        while self.last() == Some(&0) {
            self.pop();
        }
        self
    }
}

fn shl_limbs(v: &[u32], shift: u32) -> Vec<u32> {
    if shift == 0 {
        return v.to_vec();
    }
    let mut out = Vec::with_capacity(v.len() + 1);
    let mut carry = 0u32;
    for &l in v {
        out.push((l << shift) | carry);
        carry = l >> (32 - shift);
    }
    if carry != 0 {
        out.push(carry);
    }
    out
}

fn shr_limbs(v: &[u32], shift: u32) -> Vec<u32> {
    if shift == 0 {
        return v.to_vec();
    }
    let mut out = vec![0u32; v.len()];
    for i in 0..v.len() {
        let hi = if i + 1 < v.len() { v[i + 1] } else { 0 };
        out[i] = (v[i] >> shift) | (hi << (32 - shift));
    }
    out.trim()
}

// ---------------------------------------------------------------------------
// CPython-compatible hashing and float formatting
// ---------------------------------------------------------------------------

/// CPython's float hash: for `v = m * 2^e` with `m` an odd integer,
/// `(m mod p) * 2^e mod p` over `p = 2^61 - 1`, negated, `-1 -> -2`.
pub fn float_hash(v: f64) -> i64 {
    const P: u128 = (1u128 << 61) - 1;
    if !v.is_finite() {
        return 0; // NaN hashes per-object in CPython; corpus keeps NaNs out of sets
    }
    if v == 0.0 {
        return 0;
    }
    let bits = v.to_bits();
    let sign_neg = bits >> 63 == 1;
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if biased == 0 {
        (frac, -1074i64)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    while m & 1 == 0 {
        m >>= 1;
        e += 1;
    }
    let mut acc: u128 = 0;
    let mut shifted = m;
    while shifted != 0 {
        acc = ((acc << 32) + (shifted & 0xffff_ffff) as u128) % P;
        shifted >>= 32;
    }
    // 2^61 = 2 (mod p), so the exponent folds modulo 61
    let mut pow = 1u128;
    let mut base = 2u128;
    let mut k = e.rem_euclid(61) as u32;
    while k > 0 {
        if k & 1 == 1 {
            pow = pow * base % P;
        }
        base = base * base % P;
        k >>= 1;
    }
    let mut h = (acc * pow % P) as i64;
    if sign_neg {
        h = -h;
    }
    if h == -1 {
        h = -2;
    }
    h
}

/// CPython's tuple hash (xxHash-based mixing).
pub fn tuple_hash(hashes: &[i64]) -> i64 {
    const P1: u64 = 11400714785074694791;
    const P2: u64 = 14029467366897019727;
    const P3: u64 = 1609587929392839161;
    const P5: u64 = 2870177450012600261;
    let mut acc: u64 = P5;
    for &h in hashes {
        let lane = h as u64;
        acc = acc.wrapping_add(lane.wrapping_mul(P2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(P1);
        acc = acc.wrapping_add(lane.wrapping_mul(P3));
    }
    acc = acc.wrapping_add((hashes.len() as u64) ^ (P5 ^ 3527539));
    if acc == u64::MAX {
        return 1546275796;
    }
    acc as i64
}

/// CPython's `repr` of a float: shortest digits, scientific far from 1.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    let neg = f < 0.0 || (f == 0.0 && f.is_sign_negative());
    let a = f.abs();
    // Rust's {:e} gives the shortest round-trip digits in scientific form
    let sci = format!("{a:e}");
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..16).contains(&exp) {
        out.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if exp < 0 {
            out.push('-');
        } else {
            out.push('+');
        }
        out.push_str(&format!("{:02}", exp.abs()));
    } else if exp >= 0 {
        let point = exp as usize + 1;
        if digits.len() <= point {
            out.push_str(digits);
            for _ in 0..point - digits.len() {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..point]);
            out.push('.');
            out.push_str(&digits[point..]);
        }
    } else {
        out.push_str("0.");
        for _ in 0..-exp - 1 {
            out.push('0');
        }
        out.push_str(digits);
    }
    out
}

// ---------------------------------------------------------------------------
// Ordered dicts
// ---------------------------------------------------------------------------

/// A hash-consistent key wrapper: values equal under Python's `==` (across
/// int/bool/float) hash equally.
#[derive(Clone)]
struct DictKey(Val);

impl PartialEq for DictKey {
    fn eq(&self, other: &Self) -> bool {
        eq_novm(&self.0, &other.0)
    }
}

impl Eq for DictKey {}

impl std::hash::Hash for DictKey {
    // The Python hash is stored alongside the key; this wrapper only
    // supplies equality.
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

/// Insertion-ordered dict, CPython's layout semantics (order survives
/// deletes; re-set of a live key keeps position).
#[derive(Default)]
pub struct Dict {
    entries: Vec<Option<(Val, Val)>>,
    index: HashMap<(i64, DictKey), usize>,
}

impl Dict {
    fn idx(&self, key: &Val, h: i64) -> Option<usize> {
        self.index.get(&(h, DictKey(key.clone()))).copied()
    }
    pub fn get_hash(&self, key: &Val, h: i64) -> Option<Val> {
        self.idx(key, h)
            .and_then(|i| self.entries[i].as_ref().map(|(_, v)| v.clone()))
    }
    pub fn set_hash(&mut self, key: Val, h: i64, value: Val) {
        if let Some(i) = self.idx(&key, h) {
            self.entries[i] = Some((key, value));
            return;
        }
        self.entries.push(Some((key.clone(), value)));
        self.index.insert((h, DictKey(key)), self.entries.len() - 1);
    }
    pub fn del_hash(&mut self, key: &Val, h: i64) -> bool {
        let Some(i) = self.idx(key, h) else {
            return false;
        };
        if self.entries[i].is_some() {
            self.entries[i] = None;
            self.index.remove(&(h, DictKey(key.clone())));
            true
        } else {
            false
        }
    }
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| e.is_none())
    }
    pub fn items(&self) -> Vec<(Val, Val)> {
        self.entries.iter().filter_map(|e| e.clone()).collect()
    }
    pub fn keys(&self) -> Vec<Val> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().map(|(k, _)| k.clone()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Sets: CPython's open-addressing table, replicated for iteration order
// ---------------------------------------------------------------------------

const SET_MINSIZE: usize = 8;
const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;

#[derive(Clone)]
enum SSlot {
    Empty,
    Dummy,
    Full(i64, Val),
}

#[derive(Default)]
pub struct SetTable {
    slots: Vec<SSlot>,
    fill: usize,
    used: usize,
}

impl SetTable {
    pub fn new() -> Self {
        SetTable {
            slots: vec![SSlot::Empty; SET_MINSIZE],
            fill: 0,
            used: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    fn mask(&self) -> usize {
        self.slots.len() - 1
    }

    /// CPython's `set_add_entry` probe sequence. Returns `Ok((slot, true))`
    /// when the value is new (slot to fill), `Err(slot)` when present.
    fn probe(&self, hash: i64, v: &Val, eq: &dyn Fn(&Val) -> bool) -> Result<(usize, bool), usize> {
        let mask = self.mask();
        let mut i = (hash as usize) & mask;
        let mut freeslot: Option<usize> = None;
        let mut perturb = hash as u64;
        loop {
            match &self.slots[i] {
                SSlot::Empty => {
                    return Ok((freeslot.unwrap_or(i), freeslot.is_some()));
                }
                SSlot::Dummy => {
                    freeslot.get_or_insert(i);
                }
                SSlot::Full(h, e) if *h == hash && (is_identical(e, v) || eq(e)) => {
                    return Err(i);
                }
                SSlot::Full(..) => {}
            }
            if i + LINEAR_PROBES <= mask {
                for j in i + 1..i + LINEAR_PROBES {
                    match &self.slots[j] {
                        SSlot::Empty => {
                            return Ok((freeslot.unwrap_or(j), freeslot.is_some()));
                        }
                        SSlot::Dummy => {
                            freeslot.get_or_insert(j);
                        }
                        SSlot::Full(h, e) if *h == hash && (is_identical(e, v) || eq(e)) => {
                            return Err(j);
                        }
                        SSlot::Full(..) => {}
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = ((i << 2) + i + perturb as usize + 1) & mask;
        }
    }

    pub fn add(&mut self, v: Val, hash: i64, eq: &dyn Fn(&Val) -> bool) -> bool {
        match self.probe(hash, &v, eq) {
            Err(_) => false,
            Ok((slot, reused_dummy)) => {
                self.slots[slot] = SSlot::Full(hash, v);
                self.used += 1;
                if !reused_dummy {
                    self.fill += 1;
                }
                if self.fill * 5 >= (self.mask() + 1) * 3 {
                    let minused = if self.used > 50000 {
                        self.used * 2
                    } else {
                        self.used * 4
                    };
                    self.resize(minused);
                }
                true
            }
        }
    }

    pub fn contains(&self, v: &Val, hash: i64, eq: &dyn Fn(&Val) -> bool) -> bool {
        self.probe(hash, v, eq).is_err()
    }

    pub fn remove(&mut self, v: &Val, hash: i64, eq: &dyn Fn(&Val) -> bool) -> bool {
        if let Err(slot) = self.probe(hash, v, eq) {
            self.slots[slot] = SSlot::Dummy;
            self.used -= 1;
            true
        } else {
            false
        }
    }

    fn resize(&mut self, minused: usize) {
        let mut newsize = SET_MINSIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        let old = std::mem::replace(&mut self.slots, vec![SSlot::Empty; newsize]);
        self.fill = self.used;
        for slot in old {
            if let SSlot::Full(h, v) = slot {
                // re-insert in old slot order, no dummy reuse, no resize
                let mask = newsize - 1;
                let mut i = (h as usize) & mask;
                let mut perturb = h as u64;
                loop {
                    if matches!(self.slots[i], SSlot::Empty) {
                        self.slots[i] = SSlot::Full(h, v);
                        break;
                    }
                    perturb >>= PERTURB_SHIFT;
                    i = ((i << 2) + i + perturb as usize + 1) & mask;
                }
            }
        }
    }

    pub fn values(&self) -> Vec<Val> {
        self.slots
            .iter()
            .filter_map(|s| match s {
                SSlot::Full(_, v) => Some(v.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn from_iter(
        items: Vec<Val>,
        hash: &dyn Fn(&Val) -> i64,
        eq: &dyn Fn(&Val, &Val) -> bool,
    ) -> Self {
        let mut t = SetTable::new();
        for v in items {
            let h = hash(&v);
            let key = v.clone();
            t.add(v, h, &|o| eq(o, &key));
        }
        t
    }
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

pub struct TestModel {
    builtins: HashMap<String, Val>,
    /// When set, `print` appends here instead of writing to stdout, so
    /// embedders (tests) can compare program output.
    pub captured: RefCell<Option<String>>,
    pub class_object: Val,
    class_type: Val,
    class_int: Val,
    class_float: Val,
    class_bool: Val,
    class_str: Val,
    class_bytes: Val,
    class_list: Val,
    class_tuple: Val,
    class_dict: Val,
    class_set: Val,
    class_frozenset: Val,
    class_slice: Val,
    class_range: Val,
    class_function: Val,
    class_module: Val,
    class_generator: Val,
    class_coroutine: Val,
    class_asyncgen: Val,
    class_traceback: Val,
    class_none: Val,
    class_ellipsis: Val,
    class_property: Val,
    class_staticmethod: Val,
    class_classmethod: Val,
    class_super: Val,
    exc_base: Val,
    exc_root: Val,
    exc_stop_iteration: Val,
    exc_generator_exit: Val,
    exc_by_tag: HashMap<ErrorTag, Val>,
    exc_group: Val,
    exc_base_group: Val,
    typing_module: Val,
    collections_module: Val,
    abc_module: Val,
    exc_stop_async: Val,
}

fn v_int(v: i64) -> Val {
    Rc::new(Obj::Int(Rc::new(BigInt::from_i64(v))))
}

fn v_bool(v: bool) -> Val {
    Rc::new(Obj::Bool(v))
}

fn v_none() -> Val {
    Rc::new(Obj::None_)
}

fn v_str(s: &str) -> Val {
    Rc::new(Obj::Str(Rc::from(s)))
}

fn new_class(name: &str, bases: &[Val], kind: ClassKind) -> Val {
    let mro = if bases.is_empty() {
        Vec::new()
    } else {
        c3(name, bases)
    };
    Rc::new(Obj::Class(Rc::new(ClassObj {
        name: name.to_owned(),
        bases: bases.to_vec(),
        mro,
        dict: RefCell::new(AttrMap::default()),
        kind,
        abc: None,
        module: "builtins",
    })))
}

/// The C3 linearization of `bases`: the new class's ancestors in
/// method-resolution order, excluding the class itself. A base's stored
/// `mro` lists its own ancestors, so the base's full order is itself
/// followed by its `mro`.
fn c3(name: &str, bases: &[Val]) -> Vec<Val> {
    let mut seqs: Vec<Vec<Val>> = bases
        .iter()
        .map(|b| {
            let tail = match &**b {
                Obj::Class(c) => c.mro.clone(),
                _ => Vec::new(),
            };
            let mut s = Vec::with_capacity(tail.len() + 1);
            s.push(b.clone());
            s.extend(tail);
            s
        })
        .collect();
    seqs.push(bases.to_vec());
    let mut result: Vec<Val> = Vec::new();
    while !seqs.is_empty() {
        seqs.retain(|s| !s.is_empty());
        if seqs.is_empty() {
            break;
        }
        let mut chosen: Option<Val> = None;
        'outer: for s in &seqs {
            let head = &s[0];
            for t in &seqs {
                if t[1..].iter().any(|x| Rc::ptr_eq(x, head)) {
                    continue 'outer;
                }
            }
            chosen = Some(head.clone());
            break;
        }
        let Some(head) = chosen else {
            panic!("inconsistent MRO for class {name}");
        };
        result.push(head.clone());
        for s in seqs.iter_mut() {
            s.retain(|x| !Rc::ptr_eq(x, &head));
        }
    }
    result
}

fn class_name(v: &Val) -> String {
    match &**v {
        Obj::Class(c) => c.name.clone(),
        _ => "?".to_owned(),
    }
}

/// Whether `c` is the class `c2` or a subclass (MRO membership).
fn mro_has(c: &Val, c2: &Val) -> bool {
    match &**c {
        Obj::Class(cls) => cls.mro.iter().any(|x| Rc::ptr_eq(x, c2)) || Rc::ptr_eq(c, c2),
        _ => false,
    }
}

fn as_class(v: &Val) -> Option<Rc<ClassObj>> {
    match &**v {
        Obj::Class(c) => Some(c.clone()),
        _ => None,
    }
}

fn is_identical(a: &Val, b: &Val) -> bool {
    match (&**a, &**b) {
        (Obj::None_, Obj::None_) => true,
        (Obj::Bool(x), Obj::Bool(y)) => x == y,
        (Obj::Ellipsis, Obj::Ellipsis) => true,
        (Obj::Int(x), Obj::Int(y)) => {
            Rc::ptr_eq(x_as_rc(a), x_as_rc(b)) || x == y && x.mag.len() <= 1
        }
        _ => Rc::ptr_eq(a, b),
    }
}

fn x_as_rc(v: &Val) -> &Rc<BigInt> {
    match &**v {
        Obj::Int(i) => i,
        _ => unreachable!(),
    }
}

/// Structural equality without running user code (dict keys and set
/// membership on builtin keys).
fn eq_novm(a: &Val, b: &Val) -> bool {
    match (&**a, &**b) {
        (Obj::None_, Obj::None_) | (Obj::Ellipsis, Obj::Ellipsis) => true,
        (Obj::Bool(x), Obj::Bool(y)) => x == y,
        (Obj::Int(x), Obj::Int(y)) => x == y,
        (Obj::Float(x), Obj::Float(y)) => x == y,
        (Obj::Int(x), Obj::Float(y)) | (Obj::Float(y), Obj::Int(x)) => {
            big_eq_float(x, *y).unwrap_or(false)
        }
        (Obj::Bool(x), Obj::Int(y)) | (Obj::Int(y), Obj::Bool(x)) => {
            BigInt::from_i64(i64::from(*x)) == **y
        }
        (Obj::Bool(x), Obj::Float(y)) | (Obj::Float(y), Obj::Bool(x)) => f64::from(*x) == *y,
        (Obj::Str(x), Obj::Str(y)) => x == y,
        (Obj::Bytes(x), Obj::Bytes(y)) => x == y,
        (Obj::Tuple(x), Obj::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| eq_novm(a, b))
        }
        _ => Rc::ptr_eq(a, b),
    }
}

/// Exact BigInt vs float equality.
fn big_eq_float(b: &BigInt, f: f64) -> Option<bool> {
    let ord = cmp_big_float(b, f)?;
    Some(ord == std::cmp::Ordering::Equal)
}

/// Exact ordering of a BigInt against a finite float.
fn cmp_big_float(b: &BigInt, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if !f.is_finite() {
        return if f.is_nan() {
            None
        } else if f > 0.0 {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        };
    }
    let bits = f.to_bits();
    let neg = bits >> 63 == 1;
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if biased == 0 {
        (frac, -1074i64)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    // |b| vs m * 2^e  (m > 0 when f != 0)
    let babs = b.abs();
    let ord = if f == 0.0 {
        Ordering::Greater
    } else if e >= 0 {
        let fbig = BigInt {
            neg: false,
            mag: vec![m as u32, (m >> 32) as u32],
        }
        .trim()
        .shl(e as usize);
        BigInt::cmp_mag(&babs.mag, &fbig.mag)
    } else {
        // scale b left by -e
        let bscaled = babs.shl((-e) as usize);
        let fmag = BigInt {
            neg: false,
            mag: vec![m as u32, (m >> 32) as u32],
        }
        .trim();
        BigInt::cmp_mag(&bscaled.mag, &fmag.mag)
    };
    Some(if neg { ord.reverse() } else { ord })
}

/// Python `hash` of a value, for dict/set tables.
pub fn py_hash(v: &Val) -> Result<i64, String> {
    Ok(match &**v {
        Obj::None_ => (0xFCA8_6420_7C5D_8197u64 as i64) % (1 << 30),
        Obj::Bool(b) => i64::from(*b),
        Obj::Int(i) => i.py_hash(),
        Obj::Float(f) => float_hash(*f),
        Obj::Str(s) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            h.finish() as i64
        }
        Obj::Bytes(b) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            b.hash(&mut h);
            h.finish() as i64
        }
        Obj::Tuple(t) => {
            let hs: Result<Vec<i64>, String> = t.iter().map(py_hash).collect();
            tuple_hash(&hs?)
        }
        Obj::Ellipsis => (0x5962_3AEE_2F9C_8845u64 as i64) % (1 << 30),
        Obj::Instance(i) => {
            let is_exc = i.exc.borrow().is_some();
            if is_exc {
                return Err(exc_type_name(v));
            }
            Rc::as_ptr(i) as i64
        }
        other => {
            return Err(type_name_of(other).to_owned());
        }
    })
}

fn exc_type_name(v: &Val) -> String {
    match &**v {
        Obj::Instance(i) => class_name(&i.class),
        _ => type_name_of(v).to_owned(),
    }
}

/// The builtin type name of a raw value (no user classes).
fn type_name_of(v: &Obj) -> &'static str {
    match v {
        Obj::None_ => "NoneType",
        Obj::Bool(_) => "bool",
        Obj::Int(_) => "int",
        Obj::Float(_) => "float",
        Obj::Ellipsis => "ellipsis",
        Obj::NotImplemented => "NotImplementedType",
        Obj::Str(_) => "str",
        Obj::Bytes(_) => "bytes",
        Obj::Tuple(_) => "tuple",
        Obj::List(_) => "list",
        Obj::Dict(_) => "dict",
        Obj::Set(_) => "set",
        Obj::FrozenSet(_) => "frozenset",
        Obj::Slice { .. } => "slice",
        Obj::Range { .. } => "range",
        Obj::Cell(_) => "cell",
        Obj::Function(_) => "function",
        Obj::Native(..) => "builtin_function_or_method",
        Obj::BoundMethod { .. } => "method",
        Obj::Class(_) => "type",
        Obj::Instance(_) => "instance",
        Obj::Property { .. } => "property",
        Obj::StaticMethod(_) => "staticmethod",
        Obj::ClassMethod(_) => "classmethod",
        Obj::Super(_) => "super",
        Obj::Module(_) => "module",
        Obj::Generator(g) => match g.kind {
            GenKind::Generator => "generator",
            GenKind::Coroutine => "coroutine",
            GenKind::AsyncGen => "async_generator",
        },
        Obj::IterList { .. }
        | Obj::IterTuple { .. }
        | Obj::IterRange { .. }
        | Obj::IterStr { .. }
        | Obj::IterBytes { .. }
        | Obj::IterDict { .. }
        | Obj::IterSet { .. }
        | Obj::IterObj { .. }
        | Obj::IterSeq { .. }
        | Obj::IterMap { .. }
        | Obj::IterFilter { .. }
        | Obj::IterZip { .. }
        | Obj::IterEnum { .. }
        | Obj::IterRevList { .. }
        | Obj::IterRevGetItem { .. } => "iterator",
        Obj::AsendAwaiter { .. } => "coroutine",
        Obj::Traceback => "traceback",
        Obj::TypeVar(_) => "TypeVar",
        Obj::TypeVarTuple(_) => "TypeVarTuple",
        Obj::ParamSpec(_) => "ParamSpec",
        Obj::GenericAlias(_) => "_GenericAlias",
        Obj::TypeAlias(..) => "TypeAliasType",
    }
}

/// CPython `repr` of a str literal, with quote preference and escapes.
pub fn str_repr(s: &str) -> String {
    let has_sq = s.contains('\'');
    let has_dq = s.contains('"');
    let quote = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `ascii()`: like repr but with non-ASCII escaped.
pub fn str_ascii(s: &str) -> String {
    let r = str_repr(s);
    let mut out = String::with_capacity(r.len());
    let chars = r.chars().peekable();
    let mut in_escape = false;
    for c in chars {
        if in_escape {
            in_escape = false;
            out.push(c);
            continue;
        }
        if c == '\\' {
            in_escape = true;
            out.push(c);
        } else if (c as u32) >= 0x80 {
            let cp = c as u32;
            if cp <= 0xff {
                out.push_str(&format!("\\x{cp:02x}"));
            } else if cp <= 0xffff {
                out.push_str(&format!("\\u{cp:04x}"));
            } else {
                out.push_str(&format!("\\U{cp:08x}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn bytes_repr(b: &[u8]) -> String {
    let mut out = String::from("b'");
    for &x in b {
        match x {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x27 => out.push_str("\\'"),
            x if (0x20..0x7f).contains(&x) => out.push(x as char),
            x => out.push_str(&format!("\\x{x:02x}")),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Class-of, isinstance, exception factory
// ---------------------------------------------------------------------------

fn err(m: &TestModel, tag: ErrorTag, msg: &str) -> Val {
    let class = m
        .exc_by_tag
        .get(&tag)
        .or_else(|| m.exc_by_tag.get(&ErrorTag::RuntimeError))
        .expect("exception class")
        .clone();
    new_instance(&class, vec![v_str(msg)], None)
}

fn new_instance(class: &Val, args: Vec<Val>, group: Option<GroupData>) -> Val {
    // only exception-kind classes carry exception data; a plain instance of
    // a user class must not read as a BaseException
    let exc_kind = as_class(class)
        .map(|c| matches!(c.kind, ClassKind::Exception { .. }))
        .unwrap_or(false);
    Rc::new(Obj::Instance(Rc::new(InstObj {
        class: class.clone(),
        attrs: new_dict(),
        exc: RefCell::new(if exc_kind {
            Some(ExcData {
                args,
                tb: Vec::new(),
                cause: None,
                context: None,
                suppress_context: false,
                group,
            })
        } else {
            None
        }),
    })))
}

fn m_class_of(m: &TestModel, v: &Val) -> Val {
    match &**v {
        Obj::None_ => m.class_none.clone(),
        Obj::Bool(_) => m.class_bool.clone(),
        Obj::Int(_) => m.class_int.clone(),
        Obj::Float(_) => m.class_float.clone(),
        Obj::Ellipsis => m.class_ellipsis.clone(),
        Obj::Str(_) => m.class_str.clone(),
        Obj::Bytes(_) => m.class_bytes.clone(),
        Obj::Tuple(_) => m.class_tuple.clone(),
        Obj::List(_) => m.class_list.clone(),
        Obj::Dict(_) => m.class_dict.clone(),
        Obj::Set(_) => m.class_set.clone(),
        Obj::FrozenSet(_) => m.class_frozenset.clone(),
        Obj::Slice { .. } => m.class_slice.clone(),
        Obj::Range { .. } => m.class_range.clone(),
        Obj::Function(_) | Obj::Native(..) => m.class_function.clone(),
        Obj::BoundMethod { .. } => m.class_function.clone(),
        Obj::Class(_) => m.class_type.clone(),
        Obj::Instance(i) => i.class.clone(),
        Obj::Property { .. } => m.class_property.clone(),
        Obj::StaticMethod(_) => m.class_staticmethod.clone(),
        Obj::ClassMethod(_) => m.class_classmethod.clone(),
        Obj::Super(_) => m.class_super.clone(),
        Obj::Module(_) => m.class_module.clone(),
        Obj::Generator(g) => match g.kind {
            GenKind::Generator => m.class_generator.clone(),
            GenKind::Coroutine => m.class_coroutine.clone(),
            GenKind::AsyncGen => m.class_asyncgen.clone(),
        },
        Obj::Traceback => m.class_traceback.clone(),
        _ => m.class_object.clone(),
    }
}

/// `isinstance` including tuple types and ABC predicates.
fn isinstance_val(m: &TestModel, v: &Val, cls: &Val) -> bool {
    match &**cls {
        Obj::Tuple(t) => t.iter().any(|c| isinstance_val(m, v, c)),
        Obj::Class(c) => {
            if let Some(abc) = c.abc {
                return abc_matches(m, v, abc);
            }
            match c.kind {
                ClassKind::Builtin(bt) => builtin_isinstance(v, bt),
                _ => {
                    let vclass = m_class_of(m, v);
                    mro_has(&vclass, cls)
                }
            }
        }
        _ => false,
    }
}

fn abc_matches(_m: &TestModel, v: &Val, abc: AbcKind) -> bool {
    match abc {
        AbcKind::Sequence => matches!(
            &**v,
            Obj::List(_) | Obj::Tuple(_) | Obj::Range { .. } | Obj::Str(_) | Obj::Bytes(_)
        ),
        AbcKind::Mapping => matches!(&**v, Obj::Dict(_)),
        AbcKind::Iterable => !matches!(
            &**v,
            Obj::None_
                | Obj::Bool(_)
                | Obj::Int(_)
                | Obj::Float(_)
                | Obj::Ellipsis
                | Obj::Cell(_)
                | Obj::Traceback
        ),
        AbcKind::Callable => matches!(
            &**v,
            Obj::Function(_)
                | Obj::Native(..)
                | Obj::BoundMethod { .. }
                | Obj::Class(_)
                | Obj::Property { .. }
                | Obj::StaticMethod(_)
                | Obj::ClassMethod(_)
        ),
    }
}

fn builtin_isinstance(v: &Val, bt: BuiltinType) -> bool {
    let tn = type_name_of(v);
    matches!(
        (bt, tn),
        (BuiltinType::None, "NoneType")
            | (BuiltinType::Bool, "bool")
            | (BuiltinType::Int, "int")
            | (BuiltinType::Int, "bool")
            | (BuiltinType::Float, "float")
            | (BuiltinType::Str, "str")
            | (BuiltinType::Bytes, "bytes")
            | (BuiltinType::List, "list")
            | (BuiltinType::Tuple, "tuple")
            | (BuiltinType::Dict, "dict")
            | (BuiltinType::Set, "set")
            | (BuiltinType::FrozenSet, "frozenset")
            | (BuiltinType::Set, "frozenset")
            | (BuiltinType::Slice, "slice")
            | (BuiltinType::Range, "range")
            | (BuiltinType::Function, "function")
            | (BuiltinType::Function, "builtin_function_or_method")
            | (BuiltinType::Function, "method")
            | (BuiltinType::Module, "module")
            | (BuiltinType::Traceback, "traceback")
            | (BuiltinType::Generator, "generator")
            | (BuiltinType::Ellipsis, "ellipsis")
    )
}

/// `issubclass(cls, of)`: MRO membership, tuple types.
fn issubclass_val(cls: &Val, of: &Val) -> Result<bool, Val> {
    match &**of {
        Obj::Tuple(t) => {
            for c in t.iter() {
                if issubclass_val(cls, c)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Obj::Class(_) => Ok(mro_has(cls, of)),
        _ => Err(v_str(
            "issubclass() arg 2 must be a class or tuple of classes",
        )),
    }
}

/// Whether `v` is a `BaseException` instance (builtin classes and groups).
fn is_exc_instance(m: &TestModel, v: &Val) -> bool {
    match &**v {
        Obj::Instance(i) => {
            if i.exc.borrow().is_some() {
                return true;
            }
            let root = m.exc_root.clone();
            mro_has(&i.class, &root)
        }
        _ => false,
    }
}

fn is_exc_class(m: &TestModel, v: &Val) -> bool {
    match &**v {
        Obj::Class(c) => {
            let root = m.exc_root.clone();
            Rc::ptr_eq(v, &root) || c.mro.iter().any(|x| Rc::ptr_eq(x, &root))
        }
        _ => false,
    }
}

/// `PyErr_GivenExceptionMatches`: `exc` is an instance or a class.
fn exc_matches_val(m: &TestModel, exc: &Val, types: &Val) -> bool {
    let target = match &**exc {
        Obj::Class(_) => exc.clone(),
        Obj::Instance(i) => i.class.clone(),
        _ => return false,
    };
    match &**types {
        Obj::Tuple(t) => t.iter().any(|c| exc_matches_val(m, &target, c)),
        Obj::Class(_) => {
            if !is_exc_class(m, types) && as_class(types).is_some() {
                return false;
            }
            mro_has(&target, types)
        }
        _ => false,
    }
}

/// The unhandled-`except*` match-type check: no group classes.
fn check_star_types(m: &TestModel, types: &Val) -> Result<(), Val> {
    let group_root = m.exc_base_group.clone();
    match &**types {
        Obj::Tuple(t) => {
            for c in t.iter() {
                check_star_types(m, c)?;
            }
            Ok(())
        }
        Obj::Class(c) => {
            if Rc::ptr_eq(types, &group_root) || c.mro.iter().any(|x| Rc::ptr_eq(x, &group_root)) {
                Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "catching {} with except* is not allowed. Use except instead.",
                        c.name
                    ),
                ))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Slices
// ---------------------------------------------------------------------------

fn slice_parts(m: &TestModel, v: &Val) -> Option<(Option<Val>, Option<Val>, Option<Val>)> {
    match &**v {
        Obj::Slice { start, stop, step } => Some((start.clone(), stop.clone(), step.clone())),
        _ => {
            let _ = m;
            None
        }
    }
}

/// `PySlice_Unpack`: adjusts negative and clamped bounds; `None` fills.
fn slice_indices(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    slic: &Val,
    len: i64,
) -> Result<(i64, i64, i64), Val> {
    let (start, stop, step) = slice_parts(m, slic).ok_or_else(|| {
        err(
            m,
            ErrorTag::TypeError,
            "slice indices must be integers or None",
        )
    })?;
    let step = match step {
        None => 1,
        Some(s) if matches!(&*s, Obj::None_) => 1,
        Some(s) => {
            let v = py_index(m, vm, &s)?;
            if v == 0 {
                return Err(err(m, ErrorTag::ValueError, "slice step cannot be zero"));
            }
            v.clamp(-i64::MAX, i64::MAX)
        }
    };
    let is_none = |v: &Option<Val>| v.as_ref().is_none_or(|s| matches!(&**s, Obj::None_));
    let start = if is_none(&start) {
        if step < 0 { len - 1 } else { 0 }
    } else {
        let s = start.unwrap();
        let v = py_index(m, vm, &s)?;
        let v = if v < 0 { v + len } else { v };
        if step < 0 {
            if v < 0 { -1 } else { v.min(len - 1) }
        } else if v < 0 {
            0
        } else {
            v.min(len)
        }
    };
    let stop = if is_none(&stop) {
        if step < 0 { -1 } else { len }
    } else {
        let s = stop.unwrap();
        let v = py_index(m, vm, &s)?;
        let v = if v < 0 { v + len } else { v };
        if step < 0 { v.max(-1) } else { v.clamp(0, len) }
    };
    Ok((start, stop, step))
}

/// The index list a slice denotes.
fn slice_range(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    slic: &Val,
    len: i64,
) -> Result<Vec<usize>, Val> {
    let (start, stop, step) = slice_indices(m, vm, slic, len)?;
    let mut out = Vec::new();
    if step > 0 {
        let mut i = start;
        while i < stop {
            out.push(i as usize);
            i += step;
        }
    } else {
        let mut i = start;
        while i > stop {
            if i >= 0 && i < len {
                out.push(i as usize);
            }
            i += step;
        }
    }
    Ok(out)
}

/// `operator.index` over int/bool/`__index__`.
fn py_index(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<i64, Val> {
    match &**v {
        Obj::Int(i) => i.to_i64().ok_or_else(|| {
            err(
                m,
                ErrorTag::OverflowError,
                "cannot fit 'int' into an index-sized integer",
            )
        }),
        Obj::Bool(b) => Ok(i64::from(*b)),
        Obj::Instance(_) => {
            let dunder = v_str("__index__");
            match getattr_val(m, vm, v, "__index__") {
                Ok(f) => {
                    let r = call_val(m, vm, &f, &[], &[])?;
                    match &*r {
                        Obj::Int(i) => i.to_i64().ok_or_else(|| {
                            err(
                                m,
                                ErrorTag::OverflowError,
                                "cannot fit 'int' into an index-sized integer",
                            )
                        }),
                        Obj::Bool(b) => Ok(i64::from(*b)),
                        _ => {
                            let _ = dunder;
                            Err(err(
                                m,
                                ErrorTag::TypeError,
                                &format!(
                                    "'{}' object cannot be interpreted as an integer",
                                    type_name_of(&r)
                                ),
                            ))
                        }
                    }
                }
                Err(_) => Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "'{}' object cannot be interpreted as an integer",
                        val_type_name(m, v)
                    ),
                )),
            }
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' object cannot be interpreted as an integer",
                val_type_name(m, v)
            ),
        )),
    }
}

/// The user-visible type name of a value (its class's name).
fn val_type_name(_m: &TestModel, v: &Val) -> String {
    match &**v {
        Obj::Instance(i) => class_name(&i.class),
        Obj::Class(c) => c.name.clone(),
        _ => type_name_of(v).to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Attribute protocol
// ---------------------------------------------------------------------------

/// `type(v)`'s MRO lookup of `name`: `v` a class (its own MRO) or an
/// instance (its class's MRO), `_PyObject_LookupSpecial`'s rule. Returns the
/// raw entry; descriptor binding happens at the call site.
fn mro_attr(v: &Val, name: &str) -> Option<Val> {
    let class = match &**v {
        Obj::Class(_) => v.clone(),
        Obj::Instance(i) => i.class.clone(),
        _ => return None,
    };
    let c = as_class(&class)?;
    if let Some(hit) = c.dict.borrow().get(name) {
        return Some(hit.clone());
    }
    for b in &c.mro {
        let bc = as_class(b)?;
        if let Some(hit) = bc.dict.borrow().get(name) {
            return Some(hit.clone());
        }
    }
    None
}

fn is_function_like(v: &Val) -> bool {
    matches!(
        &**v,
        Obj::Function(_) | Obj::Native(..) | Obj::BoundMethod { .. }
    )
}

/// Whether `v` acts as a data descriptor (property-like).
fn is_data_descriptor(v: &Val) -> bool {
    match &**v {
        Obj::Property { .. } => true,
        Obj::Instance(i) => {
            mro_attr(&i.class, "__set__").is_some() || mro_attr(&i.class, "__delete__").is_some()
        }
        _ => false,
    }
}

/// Whether `v` is any descriptor (data or non-data: has `__get__`).
fn bind(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    v: &Val,
    owner: &Val,
    inst: Option<Val>,
) -> Result<Val, Val> {
    match &**v {
        Obj::Function(_) | Obj::Native(..) => Ok(match inst {
            Some(recv) => Rc::new(Obj::BoundMethod {
                recv,
                func: v.clone(),
            }),
            None => v.clone(),
        }),
        Obj::StaticMethod(f) => Ok(f.clone()),
        Obj::ClassMethod(f) => {
            // a classmethod called on an instance binds the instance's class
            let target = match &inst {
                Some(r) => m_class_of(m, r),
                None => owner.clone(),
            };
            Ok(Rc::new(Obj::BoundMethod {
                recv: target,
                func: f.clone(),
            }))
        }
        Obj::Property { fget, .. } => {
            let Some(f) = fget else {
                return Err(err(m, ErrorTag::AttributeError, "unreadable attribute"));
            };
            let Some(recv) = inst else {
                return Ok(v.clone());
            };
            call_val(m, vm, f, &[recv], &[])
        }
        Obj::Instance(i) => {
            let g = mro_attr(&i.class, "__get__");
            if let Some(g) = g {
                let owner_v = owner.clone();
                let inst_v = inst.clone().unwrap_or_else(v_none);
                return call_val(m, vm, &g, &[v.clone(), inst_v, owner_v], &[]);
            }
            Ok(v.clone())
        }
        _ => Ok(v.clone()),
    }
}

/// The full `getattr` protocol.
fn instance_plain_getattr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    name: &str,
) -> Result<Val, Val> {
    match &**obj {
        Obj::Instance(i) => {
            // data descriptors first
            if let Some(a) = mro_attr(&i.class, name)
                && is_data_descriptor(&a)
            {
                return bind(m, vm, &a, &i.class, Some(obj.clone()));
            }
            if let Some(v) = dict_get_str(&i.attrs, name) {
                return Ok(v);
            }
            if let Some(a) = mro_attr(&i.class, name) {
                return bind(m, vm, &a, &i.class, Some(obj.clone()));
            }
            // exception attributes
            if i.exc.borrow().is_some() {
                match name {
                    "args" => {
                        let args: Vec<Val> = i
                            .exc
                            .borrow()
                            .as_ref()
                            .map(|e| e.args.clone())
                            .unwrap_or_default();
                        return Ok(Rc::new(Obj::Tuple(Rc::new(args))));
                    }
                    "value" if is_exc_matches(m, obj, "StopIteration") => {
                        let v = i
                            .exc
                            .borrow()
                            .as_ref()
                            .and_then(|e| e.args.first().cloned())
                            .unwrap_or_else(v_none);
                        return Ok(v);
                    }
                    "exceptions" => {
                        let children = i
                            .exc
                            .borrow()
                            .as_ref()
                            .and_then(|e| e.group.as_ref().map(|g| g.children.clone()))
                            .unwrap_or_default();
                        return Ok(Rc::new(Obj::Tuple(Rc::new(children))));
                    }
                    "message" => {
                        let msg = i
                            .exc
                            .borrow()
                            .as_ref()
                            .and_then(|e| e.group.as_ref().map(|g| g.message.clone()))
                            .unwrap_or_else(v_none);
                        return Ok(msg);
                    }
                    "__cause__" | "__context__" => {
                        let v = i
                            .exc
                            .borrow()
                            .as_ref()
                            .and_then(|e| {
                                if name == "__cause__" {
                                    e.cause.clone()
                                } else {
                                    e.context.clone()
                                }
                            })
                            .unwrap_or_else(v_none);
                        return Ok(v);
                    }
                    "__suppress_context__" => {
                        let v = i
                            .exc
                            .borrow()
                            .as_ref()
                            .map(|e| e.suppress_context)
                            .unwrap_or(false);
                        return Ok(v_bool(v));
                    }
                    "__traceback__" => {
                        return Ok(v_none());
                    }
                    _ => {}
                }
            }
            if name == "__class__" {
                return Ok(i.class.clone());
            }
            if name == "__dict__" {
                return Ok(i.attrs.clone());
            }
            Err(attr_error(m, obj, name))
        }
        _ => unreachable!("instance"),
    }
}

fn getattr_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    name: &str,
) -> Result<Val, Val> {
    match &**obj {
        Obj::Instance(_) => {
            // A user `__getattribute__` wraps the whole lookup;
            // AttributeError from it falls through to `__getattr__`.
            let hook = mro_attr(obj, "__getattribute__")
                .filter(|f| is_function_like(f) && !matches!(&**f, Obj::Native(..)));
            match hook {
                Some(f) => {
                    if let Obj::Instance(i) = &**obj {
                        let bound = bind(m, vm, &f, &i.class, Some(obj.clone()))?;
                        match call_val(m, vm, &bound, &[v_str(name)], &[]) {
                            Ok(v) => return Ok(v),
                            Err(e) if is_exc_matches(m, &e, "AttributeError") => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                None => match instance_plain_getattr(m, vm, obj, name) {
                    Ok(v) => return Ok(v),
                    Err(e) if is_exc_matches(m, &e, "AttributeError") => {}
                    Err(e) => return Err(e),
                },
            }
            // `__getattr__` runs only after ordinary lookup fails
            if let Some(f) = mro_attr(obj, "__getattr__")
                && is_function_like(&f)
                && let Obj::Instance(i) = &**obj
            {
                let bound = bind(m, vm, &f, &i.class, Some(obj.clone()))?;
                return call_val(m, vm, &bound, &[v_str(name)], &[]);
            }
            Err(attr_error(m, obj, name))
        }
        Obj::Class(c) => {
            if name == "__name__" {
                return Ok(v_str(&c.name));
            }
            if name == "__mro__" {
                let mut mro = vec![obj.clone()];
                mro.extend(c.mro.iter().cloned());
                return Ok(Rc::new(Obj::Tuple(Rc::new(mro))));
            }
            if name == "__bases__" {
                return Ok(Rc::new(Obj::Tuple(Rc::new(c.bases.clone()))));
            }
            if name == "__class__" {
                return Ok(m.class_type.clone());
            }
            if let Some(v) = c.dict.borrow().get(name) {
                return bind(m, vm, v, obj, None);
            }
            for b in &c.mro {
                let bc = as_class(b).expect("mro class");
                if let Some(v) = bc.dict.borrow().get(name) {
                    return bind(m, vm, v, obj, None);
                }
            }
            // `str.upper` and friends: an unbound native taking the
            // receiver as its first argument.
            if let ClassKind::Builtin(bt) = c.kind
                && let Some(proto) = builtin_prototype(bt)
                && let Some(bound) = builtin_method(m, &proto, name)
                && let Obj::BoundMethod { func, .. } = &*bound
            {
                return Ok(func.clone());
            }
            // type's own attrs
            if let Some(v) = builtin_method(m, obj, name) {
                return Ok(v);
            }
            Err(attr_error(m, obj, name))
        }
        Obj::Module(md) => {
            if name == "__name__" {
                return Ok(v_str(&md.name));
            }
            if let Some(v) = md.dict.borrow().get(name) {
                return Ok(v.clone());
            }
            Err(err(
                m,
                ErrorTag::AttributeError,
                &format!("module '{}' has no attribute '{}'", md.name, name),
            ))
        }
        Obj::Generator(g) => {
            let _ = g;
            if let Some(v) = builtin_method(m, obj, name) {
                return Ok(v);
            }
            Err(attr_error(m, obj, name))
        }
        Obj::Super(s) => {
            // Search `obj`'s MRO strictly after `typ`; a class's `mro`
            // excludes itself, so rebuild the full chain first. An unbound
            // super (no obj) searches typ's own MRO after typ.
            let start = s
                .obj
                .clone()
                .map(|o| match &*o {
                    Obj::Instance(i) => i.class.clone(),
                    _ => o.clone(),
                })
                .unwrap_or_else(|| s.typ.clone());
            let mut chain: Vec<Val> = vec![start.clone()];
            if let Obj::Class(c) = &*start {
                chain.extend(c.mro.iter().cloned());
            }
            let pos = chain
                .iter()
                .position(|x| Rc::ptr_eq(x, &s.typ))
                .map(|p| p + 1)
                .unwrap_or(chain.len());
            for b in &chain[pos.min(chain.len())..] {
                let Some(bc) = as_class(b) else {
                    continue;
                };
                if let Some(v) = bc.dict.borrow().get(name) {
                    return bind(m, vm, v, &s.typ, s.obj.clone());
                }
            }
            if let Some(v) = builtin_method(m, obj, name) {
                return Ok(v);
            }
            Err(err(
                m,
                ErrorTag::AttributeError,
                &format!("'super' object has no attribute '{name}'"),
            ))
        }
        Obj::Function(f) => match name {
            "__name__" => Ok(v_str(&f.name.borrow())),
            "__qualname__" => Ok(v_str(&f.qualname.borrow())),
            _ => match f.attrs.borrow().get(name) {
                Some(v) => Ok(v.clone()),
                None => Err(attr_error(m, obj, name)),
            },
        },
        _ => {
            if let Some(v) = builtin_method(m, obj, name) {
                return Ok(v);
            }
            if name == "__class__" {
                return Ok(m_class_of(m, obj));
            }
            if let Some(v) = builtin_attr(m, obj, name) {
                return Ok(v);
            }
            Err(attr_error(m, obj, name))
        }
    }
}

/// Non-callable attributes of builtin values: `(1).real` and friends.
fn builtin_attr(_m: &TestModel, obj: &Val, name: &str) -> Option<Val> {
    let num = |v: f64| Rc::new(Obj::Float(v));
    match (&**obj, name) {
        (Obj::Int(i), "real") => Some(Rc::new(Obj::Int(i.clone()))),
        (Obj::Int(_), "imag") => Some(v_int(0)),
        (Obj::Int(i), "numerator") => Some(Rc::new(Obj::Int(i.clone()))),
        (Obj::Int(_), "denominator") => Some(v_int(1)),
        (Obj::Float(f), "real") => Some(num(*f)),
        (Obj::Float(_), "imag") => Some(num(0.0)),
        (Obj::Bool(b), "real") => Some(v_int(*b as i64)),
        (Obj::Bool(_), "imag") => Some(v_int(0)),
        (Obj::Slice { start, .. }, "start") => Some(start.clone().unwrap_or_else(v_none)),
        (Obj::Slice { stop, .. }, "stop") => Some(stop.clone().unwrap_or_else(v_none)),
        (Obj::Slice { step, .. }, "step") => Some(step.clone().unwrap_or_else(v_none)),
        _ => None,
    }
}

fn attr_error(m: &TestModel, obj: &Val, name: &str) -> Val {
    err(
        m,
        ErrorTag::AttributeError,
        &format!(
            "'{}' object has no attribute '{}'",
            val_type_name(m, obj),
            name
        ),
    )
}

fn setattr_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    name: &str,
    value: &Val,
) -> Result<(), Val> {
    match &**obj {
        Obj::Instance(i) => {
            if let Some(f) = mro_attr(&i.class, "__setattr__")
                .filter(|f| is_function_like(f) && !matches!(&**f, Obj::Native(..)))
            {
                let bound = bind(m, vm, &f, &i.class, Some(obj.clone()))?;
                call_val(m, vm, &bound, &[v_str(name), value.clone()], &[])?;
                return Ok(());
            }
            if let Some(a) = mro_attr(&i.class, name) {
                if let Obj::Property { fset: Some(f), .. } = &*a {
                    call_val(m, vm, f, &[obj.clone(), value.clone()], &[])?;
                    return Ok(());
                }
                if let Obj::Property { fset: None, .. } = &*a {
                    return Err(err(
                        m,
                        ErrorTag::AttributeError,
                        &format!(
                            "property '{}' of '{}' object has no setter",
                            name,
                            class_name(&i.class)
                        ),
                    ));
                }
                // A non-property data descriptor with `__set__`.
                if let Some(setter) = mro_attr(&a, "__set__").filter(is_function_like) {
                    let descr = a.clone();
                    call_val(m, vm, &setter, &[descr, obj.clone(), value.clone()], &[])?;
                    return Ok(());
                }
            }
            dict_set_str(&i.attrs, name, value.clone());
            Ok(())
        }
        Obj::Class(c) => {
            c.dict.borrow_mut().set(name, value.clone());
            Ok(())
        }
        Obj::Module(md) => {
            md.dict.borrow_mut().set(name, value.clone());
            Ok(())
        }
        Obj::Function(f) => {
            f.attrs.borrow_mut().set(name, value.clone());
            if (name == "__name__" || name == "__qualname__")
                && let Obj::Str(s) = &**value
            {
                *f.name.borrow_mut() = s.to_string();
            }
            Ok(())
        }
        _ => Err(err(
            m,
            ErrorTag::AttributeError,
            &format!(
                "'{}' object has no attribute '{}'",
                val_type_name(m, obj),
                name
            ),
        )),
    }
}

fn delattr_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    name: &str,
) -> Result<(), Val> {
    match &**obj {
        Obj::Instance(i) => {
            if dict_del_str(&i.attrs, name) {
                return Ok(());
            }
            if let Some(a) = mro_attr(&i.class, name)
                && let Obj::Property { fdel: Some(f), .. } = &*a
            {
                call_val(m, vm, f, std::slice::from_ref(obj), &[])?;
                return Ok(());
            }
            Err(attr_error(m, obj, name))
        }
        Obj::Class(c) => {
            if c.dict.borrow_mut().del(name) {
                Ok(())
            } else {
                Err(attr_error(m, obj, name))
            }
        }
        Obj::Module(md) => {
            if md.dict.borrow_mut().del(name) {
                Ok(())
            } else {
                Err(attr_error(m, obj, name))
            }
        }
        _ => Err(attr_error(m, obj, name)),
    }
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

fn call_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    callee: &Val,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &**callee {
        Obj::Function(f) => {
            let ir = f.ir.clone().expect("ir function");
            let flags = vm.flags_of(ir.func);
            let g = crate::ir::FnFlags::GENERATOR;
            let c = crate::ir::FnFlags::COROUTINE;
            let ag = crate::ir::FnFlags::ASYNC_GEN;
            if flags.contains(g) || flags.contains(c) || flags.contains(ag) {
                let frame = vm.new_frame(
                    m,
                    ir.func,
                    ir.cells.clone(),
                    args,
                    kwargs,
                    &ir.defaults.clone(),
                    &ir.kwdefaults.clone(),
                    None,
                )?;
                let kind = if flags.contains(g) {
                    GenKind::Generator
                } else if flags.contains(c) {
                    GenKind::Coroutine
                } else {
                    GenKind::AsyncGen
                };
                return Ok(Rc::new(Obj::Generator(Rc::new(GenState {
                    frame: RefCell::new(Some(frame)),
                    running: Cell::new(false),
                    started: Cell::new(false),
                    kind,
                    ret: RefCell::new(None),
                }))));
            }
            vm.call_ir_function(m, &ir, args, kwargs, None)
        }
        Obj::Native(_, f) => f(m, vm, args, kwargs),
        Obj::BoundMethod { recv, func } => {
            let mut full = Vec::with_capacity(args.len() + 1);
            full.push(recv.clone());
            full.extend(args.iter().cloned());
            call_val(m, vm, func, &full, kwargs)
        }
        Obj::Class(_) => call_class(m, vm, callee, args, kwargs),
        Obj::StaticMethod(f) => call_val(m, vm, f, args, kwargs),
        Obj::Instance(_) => {
            let dunder = getattr_val(m, vm, callee, "__call__")?;
            call_val(m, vm, &dunder, args, kwargs)
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("'{}' object is not callable", val_type_name(m, callee)),
        )),
    }
}

fn call_class(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    class: &Val,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let c = as_class(class).expect("class");
    match c.kind {
        ClassKind::Builtin(bt) => builtin_ctor(m, vm, bt, args, kwargs),
        ClassKind::Exception { .. } => {
            if Rc::ptr_eq(class, &m.exc_group) || Rc::ptr_eq(class, &m.exc_base_group) {
                return make_exception_group(m, class, args);
            }
            let inst = new_instance(class, args.to_vec(), None);
            // a user-defined __init__ (anything beyond BaseException's)
            if let Some(init) = mro_attr(class, "__init__")
                && is_function_like(&init)
                && !matches!(&*init, Obj::Native(..))
            {
                let bound = bind(m, vm, &init, class, Some(inst.clone()))?;
                call_val(m, vm, &bound, args, kwargs)?;
            }
            Ok(inst)
        }
        ClassKind::User => {
            let user_new = mro_attr(class, "__new__")
                .filter(|f| is_function_like(f) && !matches!(&**f, Obj::Native(..)));
            let inst = match user_new {
                // `__new__` is an implicit static method: called on the
                // class with `cls` as the first argument.
                Some(new) => {
                    let mut full = Vec::with_capacity(args.len() + 1);
                    full.push(class.clone());
                    full.extend(args.iter().cloned());
                    call_val(m, vm, &new, &full, kwargs)?
                }
                None => new_instance(class, Vec::new(), None),
            };
            // `__init__` runs only when `__new__` returned an instance of
            // this class (CPython's type_call).
            if !isinstance_val(m, &inst, class) {
                return Ok(inst);
            }
            match mro_attr(class, "__init__") {
                Some(init) if is_function_like(&init) => {
                    let bound = bind(m, vm, &init, class, Some(inst.clone()))?;
                    call_val(m, vm, &bound, args, kwargs)?;
                }
                _ => {
                    if !args.is_empty() || !kwargs.is_empty() {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            &format!("{}() takes no arguments", c.name),
                        ));
                    }
                }
            }
            Ok(inst)
        }
        ClassKind::Object => {
            if !args.is_empty() {
                return Err(err(m, ErrorTag::TypeError, "object() takes no arguments"));
            }
            Ok(new_instance(class, Vec::new(), None))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("'{}' object is not callable", c.name),
        )),
    }
}

fn make_exception_group(m: &mut TestModel, class: &Val, args: &[Val]) -> Result<Val, Val> {
    if args.len() != 2 {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "BaseExceptionGroup.__new__() takes exactly 2 arguments",
        ));
    }
    let (msg, seq) = (&args[0], &args[1]);
    if !matches!(&**msg, Obj::Str(_)) {
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "BaseExceptionGroup.__new__() argument 1 must be str, not {}",
                val_type_name(m, msg)
            ),
        ));
    }
    let items: Vec<Val> = match &**seq {
        Obj::Tuple(t) => t.iter().cloned().collect(),
        Obj::List(l) => l.borrow().clone(),
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "second argument (exceptions) must be a sequence",
            ));
        }
    };
    if items.is_empty() {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "second argument (exceptions) must be a non-empty sequence",
        ));
    }
    let is_exc_group_class = Rc::ptr_eq(class, &m.exc_group);
    for (i, item) in items.iter().enumerate() {
        if !is_exc_instance(m, item) {
            return Err(err(
                m,
                ErrorTag::ValueError,
                &format!("Item {i} of second argument (exceptions) is not an exception"),
            ));
        }
        if is_exc_group_class && !exc_matches_val(m, item, &m.exc_base.clone()) {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "Cannot nest BaseExceptions in an ExceptionGroup",
            ));
        }
    }
    Ok(new_instance(
        class,
        args.to_vec(),
        Some(GroupData {
            message: msg.clone(),
            children: items,
            meta_root: None,
        }),
    ))
}

fn builtin_ctor(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    bt: BuiltinType,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match bt {
        BuiltinType::Int => ctor_int(m, args),
        BuiltinType::Float => ctor_float(m, args),
        BuiltinType::Bool => {
            if args.is_empty() {
                return Ok(v_bool(false));
            }
            truth_val(m, vm, &args[0]).map(v_bool)
        }
        BuiltinType::Str => {
            if args.is_empty() {
                return Ok(v_str(""));
            }
            str_val(m, vm, &args[0]).map(|s| Rc::new(Obj::Str(Rc::from(s.as_str()))))
        }
        BuiltinType::Bytes => ctor_bytes(m, args),
        BuiltinType::List => {
            if args.is_empty() {
                return Ok(Rc::new(Obj::List(RefCell::new(Vec::new()))));
            }
            let it = iter_val(m, vm, &args[0])?;
            let mut out = Vec::new();
            loop {
                match next_val(m, vm, &it) {
                    Ok(v) => out.push(v),
                    Err(e) if is_exc_matches(m, &e, "StopIteration") => break,
                    Err(e) => return Err(e),
                }
            }
            Ok(Rc::new(Obj::List(RefCell::new(out))))
        }
        BuiltinType::Tuple => {
            if args.is_empty() {
                return Ok(Rc::new(Obj::Tuple(Rc::new(Vec::new()))));
            }
            let it = iter_val(m, vm, &args[0])?;
            let mut out = Vec::new();
            loop {
                match next_val(m, vm, &it) {
                    Ok(v) => out.push(v),
                    Err(e) if is_exc_matches(m, &e, "StopIteration") => break,
                    Err(e) => return Err(e),
                }
            }
            Ok(Rc::new(Obj::Tuple(Rc::new(out))))
        }
        BuiltinType::Set => {
            let items = if args.is_empty() {
                Vec::new()
            } else {
                let it = iter_val(m, vm, &args[0])?;
                let mut out = Vec::new();
                loop {
                    match next_val(m, vm, &it) {
                        Ok(v) => out.push(v),
                        Err(e) if is_exc_matches(m, &e, "StopIteration") => break,
                        Err(e) => return Err(e),
                    }
                }
                out
            };
            let table = set_from_items(m, items)?;
            Ok(Rc::new(Obj::Set(RefCell::new(table))))
        }
        BuiltinType::FrozenSet => {
            let items = if args.is_empty() {
                Vec::new()
            } else {
                let it = iter_val(m, vm, &args[0])?;
                let mut out = Vec::new();
                loop {
                    match next_val(m, vm, &it) {
                        Ok(v) => out.push(v),
                        Err(e) if is_exc_matches(m, &e, "StopIteration") => break,
                        Err(e) => return Err(e),
                    }
                }
                out
            };
            let table = set_from_items(m, items)?;
            Ok(Rc::new(Obj::FrozenSet(RefCell::new(table))))
        }
        BuiltinType::Dict => ctor_dict(m, vm, args, kwargs),
        BuiltinType::Range => ctor_range(m, args),
        BuiltinType::Slice => match args.len() {
            1 => Ok(Rc::new(Obj::Slice {
                start: None,
                stop: Some(args[0].clone()),
                step: None,
            })),
            2 => Ok(Rc::new(Obj::Slice {
                start: Some(args[0].clone()),
                stop: Some(args[1].clone()),
                step: None,
            })),
            3 => Ok(Rc::new(Obj::Slice {
                start: Some(args[0].clone()),
                stop: Some(args[1].clone()),
                step: Some(args[2].clone()),
            })),
            n => Err(err(
                m,
                ErrorTag::TypeError,
                &format!("slice expected at most 3 arguments, got {n}"),
            )),
        },
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("cannot create '{}'", type_name_of_bt(bt)),
        )),
    }
}

fn set_from_items(m: &mut TestModel, items: Vec<Val>) -> Result<SetTable, Val> {
    let mut t = SetTable::new();
    for v in items {
        let h = py_hash(&v).map_err(|t| unhashable(m, &t))?;
        let key = v.clone();
        t.add(v, h, &|o| eq_novm(o, &key));
    }
    Ok(t)
}

fn type_name_of_bt(bt: BuiltinType) -> &'static str {
    match bt {
        BuiltinType::None => "NoneType",
        BuiltinType::Bool => "bool",
        BuiltinType::Int => "int",
        BuiltinType::Float => "float",
        BuiltinType::Str => "str",
        BuiltinType::Bytes => "bytes",
        BuiltinType::List => "list",
        BuiltinType::Tuple => "tuple",
        BuiltinType::Dict => "dict",
        BuiltinType::Set => "set",
        BuiltinType::FrozenSet => "frozenset",
        BuiltinType::Slice => "slice",
        BuiltinType::Range => "range",
        BuiltinType::Function => "function",
        BuiltinType::Module => "module",
        BuiltinType::Generator => "generator",
        BuiltinType::Traceback => "traceback",
        BuiltinType::Super => "super",
        BuiltinType::Property => "property",
        BuiltinType::Ellipsis => "ellipsis",
    }
}

fn ctor_int(m: &mut TestModel, args: &[Val]) -> Result<Val, Val> {
    if args.is_empty() {
        return Ok(v_int(0));
    }
    let v = &args[0];
    match &**v {
        Obj::Int(i) => Ok(Rc::new(Obj::Int(i.clone()))),
        Obj::Bool(b) => Ok(v_int(i64::from(*b))),
        Obj::Float(f) => {
            if !f.is_finite() {
                return Err(err(
                    m,
                    ErrorTag::OverflowError,
                    "cannot convert float infinity to integer",
                ));
            }
            let t = f.trunc();
            if t.abs() < 9.223372036854776e18 {
                Ok(Rc::new(Obj::Int(Rc::new(BigInt::from_i64(t as i64)))))
            } else {
                let b = float_to_bigint(t)
                    .ok_or_else(|| err(m, ErrorTag::OverflowError, "int too large"))?;
                Ok(Rc::new(Obj::Int(Rc::new(b))))
            }
        }
        Obj::Str(s) => {
            let t = s.trim();
            let (radix, digits) =
                if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    (16, rest)
                } else if let Some(rest) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
                    (8, rest)
                } else if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
                    (2, rest)
                } else {
                    (10, t)
                };
            if args.len() > 1
                && let Obj::Int(base) = &*args[1]
            {
                return parse_int_radix(m, s.trim(), base.to_i64().unwrap_or(10) as u32);
            }
            parse_int_radix(m, digits, radix).or_else(|_| parse_int_radix(m, t, 10))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                val_type_name(m, v)
            ),
        )),
    }
}

fn float_to_bigint(f: f64) -> Option<BigInt> {
    if !f.is_finite() {
        return None;
    }
    let bits = f.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if biased == 0 {
        (frac, -1074i64)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    let mut b = BigInt {
        neg: f < 0.0,
        mag: vec![m as u32, (m >> 32) as u32],
    };
    b = b.trim();
    if e >= 0 {
        Some(b.shl(e as usize))
    } else if e >= -52 {
        // exactly representable only if the low bits vanish; int() truncates
        Some(b.shr((-e) as usize))
    } else {
        Some(b.shr((-e) as usize))
    }
}

fn parse_int_radix(m: &mut TestModel, s: &str, radix: u32) -> Result<Val, Val> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() {
        return Err(int_parse_error(m, s));
    }
    let mut v = BigInt::zero();
    for c in digits.chars() {
        let d = c.to_digit(radix).ok_or_else(|| int_parse_error(m, s))?;
        v = v.mul_small(radix).add_small(d);
    }
    v.neg = neg && !v.is_zero();
    Ok(Rc::new(Obj::Int(Rc::new(v))))
}

fn int_parse_error(m: &TestModel, s: &str) -> Val {
    err(
        m,
        ErrorTag::ValueError,
        &format!("invalid literal for int() with base 10: '{s}'"),
    )
}

fn ctor_float(m: &mut TestModel, args: &[Val]) -> Result<Val, Val> {
    if args.is_empty() {
        return Ok(Rc::new(Obj::Float(0.0)));
    }
    let v = &args[0];
    match &**v {
        Obj::Int(i) => i.to_f64().map(|f| Rc::new(Obj::Float(f))).ok_or_else(|| {
            err(
                m,
                ErrorTag::OverflowError,
                "int too large to convert to float",
            )
        }),
        Obj::Bool(b) => Ok(Rc::new(Obj::Float(f64::from(*b)))),
        Obj::Float(f) => Ok(Rc::new(Obj::Float(*f))),
        Obj::Str(s) => {
            let t = s.trim();
            match t {
                "inf" | "+inf" | "Infinity" | "+Infinity" => Ok(Rc::new(Obj::Float(f64::INFINITY))),
                "-inf" | "-Infinity" => Ok(Rc::new(Obj::Float(f64::NEG_INFINITY))),
                "nan" | "+nan" | "-nan" => Ok(Rc::new(Obj::Float(f64::NAN))),
                _ => t
                    .parse::<f64>()
                    .map(|f| Rc::new(Obj::Float(f)))
                    .map_err(|_| {
                        err(
                            m,
                            ErrorTag::ValueError,
                            &format!("could not convert string to float: '{t}'"),
                        )
                    }),
            }
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "float() argument must be a string or a real number, not '{}'",
                val_type_name(m, v)
            ),
        )),
    }
}

fn ctor_bytes(m: &mut TestModel, args: &[Val]) -> Result<Val, Val> {
    if args.is_empty() {
        return Ok(Rc::new(Obj::Bytes(Rc::from(&[][..]))));
    }
    match &*args[0] {
        Obj::Int(i) => {
            let n = i.to_i64().unwrap_or(-1);
            if n < 0 {
                return Err(err(m, ErrorTag::ValueError, "negative count"));
            }
            Ok(Rc::new(Obj::Bytes(Rc::from(
                vec![0u8; n as usize].as_slice(),
            ))))
        }
        Obj::Str(s) => Ok(Rc::new(Obj::Bytes(Rc::from(s.as_bytes())))),
        Obj::Bytes(b) => Ok(Rc::new(Obj::Bytes(b.clone()))),
        Obj::List(l) => {
            let mut out = Vec::new();
            for item in l.borrow().iter() {
                match &**item {
                    Obj::Int(i) => {
                        let v = i.to_i64().unwrap_or(256);
                        if !(0..=255).contains(&v) {
                            return Err(err(
                                m,
                                ErrorTag::ValueError,
                                "bytes must be in range(0, 256)",
                            ));
                        }
                        out.push(v as u8);
                    }
                    _ => {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            &format!(
                                "'{}' type can't be converted to bytes",
                                val_type_name(m, item)
                            ),
                        ));
                    }
                }
            }
            Ok(Rc::new(Obj::Bytes(Rc::from(out.as_slice()))))
        }
        other => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("cannot convert '{}' to bytes", type_name_of(other)),
        )),
    }
}

fn ctor_range(m: &mut TestModel, args: &[Val]) -> Result<Val, Val> {
    let ints: Vec<i128> = args
        .iter()
        .map(|a| match &**a {
            Obj::Int(i) => i
                .to_i64()
                .map(i128::from)
                .ok_or_else(|| err(m, ErrorTag::OverflowError, "range() arg too large")),
            Obj::Bool(b) => Ok(i128::from(i64::from(*b))),
            _ => Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "'{}' object cannot be interpreted as an integer",
                    val_type_name(m, a)
                ),
            )),
        })
        .collect::<Result<_, _>>()?;
    let (start, stop, step) = match ints.len() {
        1 => (0, ints[0], 1),
        2 => (ints[0], ints[1], 1),
        3 => (ints[0], ints[1], ints[2]),
        n => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!("range expected at most 3 arguments, got {n}"),
            ));
        }
    };
    if step == 0 {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "range() arg 3 must not be zero",
        ));
    }
    Ok(Rc::new(Obj::Range { start, stop, step }))
}

fn ctor_dict(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let mut d = Dict::default();
    if let Some(src) = args.first() {
        match &**src {
            Obj::Dict(src_d) => {
                for (k, v) in src_d.borrow().items() {
                    let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
                    d.set_hash(k, h, v);
                }
            }
            _ => {
                let it = iter_val(m, vm, src)?;
                loop {
                    let pair = match next_val(m, vm, &it) {
                        Ok(v) => v,
                        Err(e) if is_exc_matches(m, &e, "StopIteration") => break,
                        Err(e) => return Err(e),
                    };
                    let items = match &*pair {
                        Obj::Tuple(t) => t.iter().cloned().collect::<Vec<_>>(),
                        Obj::List(l) => l.borrow().clone(),
                        _ => {
                            return Err(err(
                                m,
                                ErrorTag::TypeError,
                                "cannot convert dictionary update sequence element #0 to a sequence",
                            ));
                        }
                    };
                    if items.len() != 2 {
                        return Err(err(
                            m,
                            ErrorTag::ValueError,
                            &format!(
                                "dictionary update sequence element #0 has length {}; 2 is required",
                                items.len()
                            ),
                        ));
                    }
                    let h = py_hash(&items[0]).map_err(|t| unhashable(m, &t))?;
                    d.set_hash(items[0].clone(), h, items[1].clone());
                }
            }
        }
    }
    for (k, v) in kwargs {
        let h = py_hash(&v_str(k)).map_err(|t| unhashable(m, &t))?;
        d.set_hash(v_str(k), h, v.clone());
    }
    Ok(Rc::new(Obj::Dict(RefCell::new(d))))
}

fn is_exc_matches(_m: &TestModel, e: &Val, class_name: &str) -> bool {
    match &**e {
        Obj::Instance(i) => mro_names_contains(&i.class, class_name),
        _ => false,
    }
}

fn mro_names_contains(class: &Val, name: &str) -> bool {
    as_class(class).is_some_and(|c| {
        c.name == name
            || c.mro
                .iter()
                .any(|b| as_class(b).is_some_and(|x| x.name == name))
    })
}

// ---------------------------------------------------------------------------
// repr / str / truth / len
// ---------------------------------------------------------------------------

fn repr_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<String, Val> {
    Ok(match &**v {
        Obj::None_ => "None".to_string(),
        Obj::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Obj::Int(i) => i.to_string(),
        Obj::Float(f) => float_repr(*f),
        Obj::Ellipsis => "Ellipsis".to_string(),
        Obj::Str(s) => str_repr(s),
        Obj::Bytes(b) => bytes_repr(b),
        Obj::Tuple(t) => {
            let mut out = String::from("(");
            for (i, item) in t.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&repr_val(m, vm, item)?);
            }
            if t.len() == 1 {
                out.push(',');
            }
            out.push(')');
            out
        }
        Obj::List(l) => {
            let items = l.borrow().clone();
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&repr_val(m, vm, item)?);
            }
            out.push(']');
            out
        }
        Obj::Dict(d) => {
            let mut out = String::from("{");
            for (i, (k, val)) in d.borrow().items().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&repr_val(m, vm, k)?);
                out.push_str(": ");
                out.push_str(&repr_val(m, vm, val)?);
            }
            out.push('}');
            out
        }
        Obj::Set(t) => {
            let items = t.borrow().values();
            if items.is_empty() {
                "set()".to_string()
            } else {
                let mut out = String::from("{");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&repr_val(m, vm, item)?);
                }
                out.push('}');
                out
            }
        }
        Obj::FrozenSet(t) => {
            let items = t.borrow().values();
            if items.is_empty() {
                "frozenset()".to_string()
            } else {
                let mut out = String::from("frozenset({");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&repr_val(m, vm, item)?);
                }
                out.push_str("})");
                out
            }
        }
        Obj::Slice { start, stop, step } => {
            let start_s = match start {
                Some(x) => repr_val(m, vm, x)?,
                None => "None".to_string(),
            };
            let stop_s = match stop {
                Some(x) => repr_val(m, vm, x)?,
                None => "None".to_string(),
            };
            match step {
                Some(s) => format!("slice({}, {}, {})", start_s, stop_s, repr_val(m, vm, s)?),
                None => format!("slice({}, {})", start_s, stop_s),
            }
        }
        Obj::Range { start, stop, step } => {
            if *step == 1 {
                format!("range({start}, {stop})")
            } else {
                format!("range({start}, {stop}, {step})")
            }
        }
        Obj::Function(f) => format!("<function {}>", f.qualname.borrow()),
        Obj::BoundMethod { recv, func } => {
            let name = match &**func {
                Obj::Function(f) => f.qualname.borrow().clone(),
                Obj::Native(name, _) => (*name).to_string(),
                _ => "?".to_string(),
            };
            format!("<bound method {} of {}>", name, repr_val(m, vm, recv)?)
        }
        Obj::Class(c) => {
            // Builtin classes print bare; user classes print the qualified
            // name the class body stored (`Outer.Inner`).
            if c.module == "builtins" {
                format!("<class '{}'>", c.name)
            } else {
                let q = c
                    .dict
                    .borrow()
                    .get("__qualname__")
                    .and_then(|v| str_of(v).map(str::to_owned))
                    .unwrap_or_else(|| c.name.clone());
                format!("<class '{}.{}'>", c.module, q)
            }
        }
        Obj::Module(modu) => format!("<module '{}'>", modu.name),
        Obj::TypeVar(n) => format!("~{n}"),
        Obj::TypeVarTuple(n) => format!("~{n}"),
        Obj::ParamSpec(n) => format!("**{n}"),
        Obj::GenericAlias(g) => generic_alias_repr(m, vm, g)?.to_string(),
        Obj::TypeAlias(n, v) => {
            format!("TypeAliasType('{}', {})", n, repr_val(m, vm, v)?)
        }
        Obj::Instance(_) => repr_instance(m, vm, v)?,
        Obj::Generator(g) => match g.kind {
            GenKind::Generator => "<generator object>".to_string(),
            GenKind::Coroutine => "<coroutine object>".to_string(),
            GenKind::AsyncGen => "<async generator object>".to_string(),
        },
        _ => format!("<{} object>", val_type_name(m, v)),
    })
}

fn generic_alias_repr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    g: &Val,
) -> Result<String, Val> {
    match &**g {
        Obj::Class(c) => Ok(c.name.clone()),
        Obj::TypeVar(n) | Obj::TypeVarTuple(n) => Ok(n.to_string()),
        Obj::ParamSpec(n) => Ok(n.to_string()),
        Obj::GenericAlias(orig) => generic_alias_repr(m, vm, orig),
        _ => repr_val(m, vm, g),
    }
}

fn repr_instance(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    v: &Val,
) -> Result<String, Val> {
    let inst = match &**v {
        Obj::Instance(i) => i,
        _ => unreachable!(),
    };
    if let Some(r) = mro_attr(v, "__repr__")
        && is_function_like(&r)
    {
        let bound = bind(m, vm, &r, v, Some(v.clone()))?;
        let out = call_val(m, vm, &bound, &[], &[])?;
        return match &*out {
            Obj::Str(s) => Ok(s.to_string()),
            _ => Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "__repr__ returned non-string (type {})",
                    val_type_name(m, &out)
                ),
            )),
        };
    }
    if let Some(exc) = inst.exc.borrow().as_ref() {
        // exception repr: Type(args...) / Type()
        let tname = class_name(&inst.class);
        if let Some(g) = &exc.group {
            // BaseExceptionGroup.__repr__: message repr, children list repr
            let msg = repr_val(m, vm, &g.message)?;
            let parts: Vec<String> = g
                .children
                .iter()
                .map(|c| repr_val(m, vm, c))
                .collect::<Result<_, _>>()?;
            return Ok(format!("{tname}({msg}, [{}])", parts.join(", ")));
        }
        if exc.args.is_empty() {
            return Ok(format!("{tname}()"));
        }
        let parts: Vec<String> = exc
            .args
            .iter()
            .map(|a| repr_val(m, vm, a))
            .collect::<Result<_, _>>()?;
        return Ok(format!("{tname}({})", parts.join(", ")));
    }
    let module = as_class(&inst.class)
        .map(|c| c.module)
        .unwrap_or("builtins");
    Ok(format!("<{}.{} object>", module, class_name(&inst.class)))
}

/// `BaseException.__str__`: the single argument, or the repr tuple.
fn exc_str(m: &TestModel, args: &[Val], key_error: bool) -> String {
    match args.len() {
        0 => String::new(),
        1 => match &*args[0] {
            Obj::Str(s) if key_error => format!("'{s}'"),
            Obj::Str(s) => s.to_string(),
            _ => repr_static(m, &args[0]).unwrap_or_else(|| "?".to_string()),
        },
        _ => {
            // Multiple args: str(e) is the repr of the args tuple.
            let inner: Vec<String> = args
                .iter()
                .map(|a| repr_static(m, a).unwrap_or_else(|| "?".to_string()))
                .collect();
            format!("({})", inner.join(", "))
        }
    }
}

fn str_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<String, Val> {
    match &**v {
        Obj::Str(s) => Ok(s.to_string()),
        Obj::Instance(_) => {
            if let Some(s) = mro_attr(v, "__str__")
                && is_function_like(&s)
            {
                let bound = bind(m, vm, &s, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[], &[])?;
                return match &*out {
                    Obj::Str(s) => Ok(s.to_string()),
                    _ => Err(err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "__str__ returned non-string (type {})",
                            val_type_name(m, &out)
                        ),
                    )),
                };
            }
            if let Obj::Instance(i) = &**v
                && let Some(d) = i.exc.borrow().as_ref()
            {
                if let Some(g) = &d.group {
                    // BaseExceptionGroup.__str__
                    return Ok(format!(
                        "{} ({} sub-exception{})",
                        str_of(&g.message).unwrap_or(""),
                        g.children.len(),
                        if g.children.len() == 1 { "" } else { "s" }
                    ));
                }
                let args = d.args.clone();
                let key_error = is_exc_matches(m, v, "KeyError");
                return Ok(exc_str(m, &args, key_error));
            }
            repr_val(m, vm, v)
        }
        _ => repr_val(m, vm, v),
    }
}

fn truth_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<bool, Val> {
    Ok(match &**v {
        Obj::None_ => false,
        Obj::Bool(b) => *b,
        Obj::Int(i) => !i.is_zero(),
        Obj::Float(f) => *f != 0.0,
        Obj::Str(s) => !s.is_empty(),
        Obj::Bytes(b) => !b.is_empty(),
        Obj::Tuple(t) => !t.is_empty(),
        Obj::List(l) => !l.borrow().is_empty(),
        Obj::Dict(d) => !d.borrow().entries.is_empty(),
        Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().used > 0,
        Obj::Range { start, stop, step } => range_len(*start, *stop, *step) > 0,
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__bool__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[], &[])?;
                return match &*out {
                    Obj::Bool(b) => Ok(*b),
                    _ => Err(err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "__bool__ should return bool, returned {}",
                            val_type_name(m, &out)
                        ),
                    )),
                };
            }
            if let Some(f) = mro_attr(v, "__len__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[], &[])?;
                let n = match &*out {
                    Obj::Int(i) => i.to_i64().unwrap_or(0),
                    _ => {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            &format!(
                                "'{}' object cannot be interpreted as an integer",
                                val_type_name(m, &out)
                            ),
                        ));
                    }
                };
                if n < 0 {
                    return Err(err(m, ErrorTag::ValueError, "__len__() should return >= 0"));
                }
                return Ok(n != 0);
            }
            true
        }
        _ => true,
    })
}

pub fn range_len(start: i128, stop: i128, step: i128) -> i128 {
    if step > 0 {
        if stop > start {
            (stop - start + step - 1) / step
        } else {
            0
        }
    } else if stop < start {
        (start - stop - step - 1) / -step
    } else {
        0
    }
}

fn len_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<i64, Val> {
    Ok(match &**v {
        Obj::Str(s) => s.chars().count() as i64,
        Obj::Bytes(b) => b.len() as i64,
        Obj::Tuple(t) => t.len() as i64,
        Obj::List(l) => l.borrow().len() as i64,
        Obj::Dict(d) => d.borrow().entries.iter().filter(|e| e.is_some()).count() as i64,
        Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().used as i64,
        Obj::Range { start, stop, step } => range_len(*start, *stop, *step) as i64,
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__len__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[], &[])?;
                let n = match &*out {
                    Obj::Int(i) => i.to_i64(),
                    _ => None,
                };
                return match n {
                    Some(n) if n >= 0 => Ok(n),
                    Some(_) => Err(err(m, ErrorTag::ValueError, "__len__() should return >= 0")),
                    None => Err(err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "'{}' object cannot be interpreted as an integer",
                            val_type_name(m, &out)
                        ),
                    )),
                };
            }
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!("object of type '{}' has no len()", val_type_name(m, v)),
            ));
        }
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!("object of type '{}' has no len()", val_type_name(m, v)),
            ));
        }
    })
}

// ---------------------------------------------------------------------------
// Iteration
// ---------------------------------------------------------------------------

fn iter_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<Val, Val> {
    Ok(match &**v {
        Obj::List(_) => Rc::new(Obj::IterList {
            list: v.clone(),
            pos: Cell::new(0),
        }),
        Obj::Tuple(t) => Rc::new(Obj::IterTuple {
            items: t.clone(),
            pos: Cell::new(0),
        }),
        Obj::Range { start, stop, step } => Rc::new(Obj::IterRange {
            cur: Cell::new(*start),
            stop: *stop,
            step: *step,
        }),
        Obj::Str(t) => Rc::new(Obj::IterStr {
            s: t.clone(),
            pos: Cell::new(0),
        }),
        Obj::Bytes(t) => Rc::new(Obj::IterBytes {
            b: t.clone(),
            pos: Cell::new(0),
        }),
        Obj::Dict(_) => {
            let len = match &**v {
                Obj::Dict(d) => d.borrow().entries.iter().filter(|e| e.is_some()).count(),
                _ => 0,
            };
            Rc::new(Obj::IterDict {
                dict: v.clone(),
                pos: Cell::new(0),
                len,
            })
        }
        Obj::Set(_) | Obj::FrozenSet(_) => Rc::new(Obj::IterSet {
            set: v.clone(),
            pos: Cell::new(0),
        }),
        Obj::Generator(_) => v.clone(),
        Obj::IterList { .. }
        | Obj::IterTuple { .. }
        | Obj::IterRange { .. }
        | Obj::IterStr { .. }
        | Obj::IterBytes { .. }
        | Obj::IterDict { .. }
        | Obj::IterSet { .. }
        | Obj::IterObj { .. }
        | Obj::IterSeq { .. }
        | Obj::IterMap { .. }
        | Obj::IterFilter { .. }
        | Obj::IterZip { .. }
        | Obj::IterEnum { .. }
        | Obj::IterRevList { .. }
        | Obj::IterRevGetItem { .. } => v.clone(),
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__iter__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[], &[])?;
                // `__iter__` returning `self` hands back the instance;
                // wrap anything without a native iterator representation
                // so `__next__` dispatches through IterObj.
                return Ok(match &*out {
                    Obj::Generator(_)
                    | Obj::IterList { .. }
                    | Obj::IterTuple { .. }
                    | Obj::IterRange { .. }
                    | Obj::IterStr { .. }
                    | Obj::IterBytes { .. }
                    | Obj::IterDict { .. }
                    | Obj::IterSet { .. }
                    | Obj::IterObj { .. }
                    | Obj::IterSeq { .. }
                    | Obj::IterMap { .. }
                    | Obj::IterFilter { .. }
                    | Obj::IterZip { .. }
                    | Obj::IterEnum { .. }
                    | Obj::IterRevList { .. }
                    | Obj::IterRevGetItem { .. } => out,
                    _ => Rc::new(Obj::IterObj { obj: out }),
                });
            }
            if mro_attr(v, "__getitem__").is_some() {
                return Ok(Rc::new(Obj::IterSeq {
                    obj: v.clone(),
                    pos: Cell::new(0),
                }));
            }
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!("'{}' object is not iterable", val_type_name(m, v)),
            ));
        }
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!("'{}' object is not iterable", val_type_name(m, v)),
            ));
        }
    })
}

fn stop_iteration_value(m: &TestModel, value: Val) -> Val {
    new_instance(&m.exc_stop_iteration, vec![value], None)
}

fn next_val(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, it: &Val) -> Result<Val, Val> {
    match &**it {
        Obj::IterList { list, pos } => {
            let (n, v) = match &**list {
                Obj::List(l) => {
                    let l = l.borrow();
                    (
                        l.len(),
                        if pos.get() < l.len() {
                            Some(l[pos.get()].clone())
                        } else {
                            None
                        },
                    )
                }
                _ => (0, None),
            };
            let _ = n;
            match v {
                Some(v) => {
                    pos.set(pos.get() + 1);
                    Ok(v)
                }
                None => Err(stop_iteration_value(m, v_none())),
            }
        }
        Obj::IterTuple { items, pos } => {
            if pos.get() < items.len() {
                let v = items[pos.get()].clone();
                pos.set(pos.get() + 1);
                Ok(v)
            } else {
                Err(stop_iteration_value(m, v_none()))
            }
        }
        Obj::IterRange { cur, stop, step } => {
            let v = cur.get();
            let ok = if *step > 0 { v < *stop } else { v > *stop };
            if ok {
                cur.set(v + *step);
                Ok(Rc::new(Obj::Int(Rc::new(BigInt::from_i64(v as i64)))))
            } else {
                Err(stop_iteration_value(m, v_none()))
            }
        }
        Obj::IterStr { s, pos } => {
            let chars: Vec<char> = s.chars().collect();
            if pos.get() < chars.len() {
                let c = chars[pos.get()];
                pos.set(pos.get() + 1);
                Ok(v_str(&c.to_string()))
            } else {
                Err(stop_iteration_value(m, v_none()))
            }
        }
        Obj::IterBytes { b, pos } => {
            if pos.get() < b.len() {
                let byte = b[pos.get()];
                pos.set(pos.get() + 1);
                Ok(v_int(byte as i64))
            } else {
                Err(stop_iteration_value(m, v_none()))
            }
        }
        Obj::IterDict { dict, pos, len } => {
            let cur_len = match &**dict {
                Obj::Dict(d) => d.borrow().entries.iter().filter(|e| e.is_some()).count(),
                _ => 0,
            };
            if cur_len != *len {
                return Err(err(
                    m,
                    ErrorTag::RuntimeError,
                    "dictionary changed size during iteration",
                ));
            }
            let entries_len = match &**dict {
                Obj::Dict(d) => d.borrow().entries.len(),
                _ => 0,
            };
            let mut idx = pos.get();
            while idx < entries_len {
                let slot = match &**dict {
                    Obj::Dict(d) => d.borrow().entries[idx].clone(),
                    _ => unreachable!(),
                };
                idx += 1;
                if let Some((k, _)) = slot {
                    pos.set(idx);
                    return Ok(k);
                }
            }
            Err(stop_iteration_value(m, v_none()))
        }
        Obj::IterSet { set, pos } => {
            let slots = match &**set {
                Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().slots.clone(),
                _ => unreachable!(),
            };
            let mut i = pos.get();
            while i < slots.len() {
                let item = match &slots[i] {
                    SSlot::Full(_, v) => Some(v.clone()),
                    _ => None,
                };
                i += 1;
                if let Some(v) = item {
                    pos.set(i);
                    return Ok(v);
                }
            }
            Err(stop_iteration_value(m, v_none()))
        }
        Obj::IterObj { obj } => {
            let dunder = getattr_val(m, vm, obj, "__next__")?;
            call_val(m, vm, &dunder, &[], &[])
        }
        Obj::IterSeq { obj, pos } => {
            let i = pos.get();
            pos.set(i + 1);
            let getter = getattr_val(m, vm, obj, "__getitem__")?;
            let index = if i >= i64::MIN as i128 && i <= i64::MAX as i128 {
                Rc::new(Obj::Int(Rc::new(BigInt::from_i64(i as i64))))
            } else {
                return Err(stop_iteration_value(m, v_none()));
            };
            match call_val(m, vm, &getter, &[index], &[]) {
                Ok(v) => Ok(v),
                Err(e) if is_exc_matches(m, &e, "IndexError") => {
                    Err(stop_iteration_value(m, v_none()))
                }
                Err(e) => Err(e),
            }
        }
        Obj::IterMap { it, f, spread } => {
            let item = next_val(m, vm, it)?;
            if *spread && let Obj::Tuple(t) = &*item {
                let a: Vec<Val> = t.iter().cloned().collect();
                call_val(m, vm, f, &a, &[])
            } else {
                call_val(m, vm, f, &[item], &[])
            }
        }
        Obj::IterFilter { it, f } => loop {
            let item = next_val(m, vm, it)?;
            match f {
                None => {
                    if truth_val(m, vm, &item)? {
                        return Ok(item);
                    }
                }
                Some(f) => {
                    let keep = call_val(m, vm, f, std::slice::from_ref(&item), &[])?;
                    if truth_val(m, vm, &keep)? {
                        return Ok(item);
                    }
                }
            }
        },
        Obj::IterZip { its } => {
            let mut out = Vec::with_capacity(its.len());
            for it in its {
                out.push(next_val(m, vm, it)?);
            }
            Ok(Rc::new(Obj::Tuple(Rc::new(out))))
        }
        Obj::IterEnum { it, count } => {
            let item = next_val(m, vm, it)?;
            let c = count.get();
            count.set(c + 1);
            Ok(Rc::new(Obj::Tuple(Rc::new(vec![v_int(c as i64), item]))))
        }
        Obj::IterRevGetItem { obj, pos } => {
            let i = match pos.get().checked_sub(1) {
                Some(i) if i >= 0 => i,
                _ => return Err(stop_iteration_value(m, v_none())),
            };
            pos.set(i);
            let idx = v_int(i as i64);
            match getitem_val(m, vm, obj, &idx) {
                Ok(v) => Ok(v),
                Err(e) if is_exc_matches(m, &e, "IndexError") => {
                    Err(stop_iteration_value(m, v_none()))
                }
                Err(e) => Err(e),
            }
        }
        Obj::IterRevList { list, pos } => {
            // pos is the next index to yield, walking down from len
            let v = match &**list {
                Obj::List(l) => {
                    let l = l.borrow();
                    let i = pos.get().checked_sub(1).filter(|i| *i < l.len());
                    i.map(|i| l[i].clone())
                }
                _ => None,
            };
            match v {
                Some(v) => {
                    pos.set(pos.get() - 1);
                    Ok(v)
                }
                None => Err(stop_iteration_value(m, v_none())),
            }
        }
        Obj::Generator(g) => gen_send(m, vm, g, &v_none()),
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("'{}' object is not an iterator", val_type_name(m, it)),
        )),
    }
}

// ---------------------------------------------------------------------------
// Binary / unary / comparison operators
// ---------------------------------------------------------------------------

fn to_big(v: &Val) -> Option<Rc<BigInt>> {
    match &**v {
        Obj::Int(i) => Some(i.clone()),
        Obj::Bool(b) => Some(Rc::new(BigInt::from_i64(i64::from(*b)))),
        _ => None,
    }
}

fn to_float(m: &TestModel, v: &Val) -> Result<f64, Val> {
    match &**v {
        Obj::Int(i) => i.to_f64().ok_or_else(|| {
            err(
                m,
                ErrorTag::OverflowError,
                "int too large to convert to float",
            )
        }),
        Obj::Bool(b) => Ok(f64::from(*b)),
        Obj::Float(f) => Ok(*f),
        _ => unreachable!(),
    }
}

fn fmod_py(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && (r < 0.0) != (b < 0.0) {
        r + b
    } else {
        r
    }
}

fn floordiv_py(a: f64, b: f64) -> f64 {
    (a / b).floor()
}

fn binop_name(op: crate::ir::BinOp) -> &'static str {
    use crate::ir::BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        FloorDiv => "//",
        Mod => "%",
        Pow => "**",
        LShift => "<<",
        RShift => ">>",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        MatMul => "@",
    }
}

fn dunder_for(op: crate::ir::BinOp) -> (&'static str, &'static str) {
    use crate::ir::BinOp::*;
    match op {
        Add => ("__add__", "__radd__"),
        Sub => ("__sub__", "__rsub__"),
        Mul => ("__mul__", "__rmul__"),
        Div => ("__truediv__", "__rtruediv__"),
        FloorDiv => ("__floordiv__", "__rfloordiv__"),
        Mod => ("__mod__", "__rmod__"),
        Pow => ("__pow__", "__rpow__"),
        LShift => ("__lshift__", "__rlshift__"),
        RShift => ("__rshift__", "__rrshift__"),
        BitAnd => ("__and__", "__rand__"),
        BitOr => ("__or__", "__ror__"),
        BitXor => ("__xor__", "__rxor__"),
        MatMul => ("__matmul__", "__rmatmul__"),
    }
}

fn binop_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    op: crate::ir::BinOp,
    a: &Val,
    b: &Val,
) -> Result<Val, Val> {
    use crate::ir::BinOp;
    // instance dunder dispatch
    let a_inst = matches!(&**a, Obj::Instance(_));
    let b_inst = matches!(&**b, Obj::Instance(_));
    if a_inst || b_inst {
        let (l, r) = dunder_for(op);
        if a_inst
            && mro_attr(a, l).is_some()
            && let Some(v) = try_dunder(m, vm, a, l, b)?
        {
            return Ok(v);
        }
        if b_inst
            && mro_attr(b, r).is_some()
            && let Some(v) = try_dunder(m, vm, b, r, a)?
        {
            return Ok(v);
        }
        if a_inst || b_inst {
            return Err(unsupported(m, op, a, b));
        }
    }
    // numeric tower
    if let (Some(x), Some(y)) = (to_big(a), to_big(b)) {
        return int_binop(m, op, &x, &y);
    }
    let a_num = matches!(&**a, Obj::Int(_) | Obj::Bool(_) | Obj::Float(_));
    let b_num = matches!(&**b, Obj::Int(_) | Obj::Bool(_) | Obj::Float(_));
    if a_num && b_num {
        return float_binop(m, op, &to_float(m, a)?, &to_float(m, b)?);
    }
    match (op, &**a, &**b) {
        (BinOp::Add, Obj::Str(x), Obj::Str(y)) => {
            let mut out = String::with_capacity(x.len() + y.len());
            out.push_str(x);
            out.push_str(y);
            Ok(v_str(&out))
        }
        (BinOp::Add, Obj::List(x), Obj::List(y)) => {
            let mut out = x.borrow().clone();
            out.extend(y.borrow().iter().cloned());
            Ok(Rc::new(Obj::List(RefCell::new(out))))
        }
        (BinOp::Add, Obj::Tuple(x), Obj::Tuple(y)) => {
            let mut out = x.iter().cloned().collect::<Vec<_>>();
            out.extend(y.iter().cloned());
            Ok(Rc::new(Obj::Tuple(Rc::new(out))))
        }
        (BinOp::Add, Obj::Bytes(x), Obj::Bytes(y)) => {
            let mut out: Vec<u8> = Vec::with_capacity(x.len() + y.len());
            out.extend_from_slice(x);
            out.extend_from_slice(y);
            Ok(Rc::new(Obj::Bytes(Rc::from(out.as_slice()))))
        }
        (BinOp::Add, Obj::Bytes(_), _) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("can't concat {} to bytes", val_type_name(m, b)),
        )),
        (BinOp::Add, Obj::Tuple(_), _) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "can only concatenate tuple (not \"{}\") to tuple",
                val_type_name(m, b)
            ),
        )),
        (BinOp::Add, Obj::List(_), _) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "can only concatenate list (not \"{}\") to list",
                val_type_name(m, b)
            ),
        )),
        (BinOp::Add, Obj::Str(_), _) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "can only concatenate str (not \"{}\") to str",
                val_type_name(m, b)
            ),
        )),
        (BinOp::Mul, Obj::Str(x), _) | (BinOp::Mul, _, Obj::Str(x)) => {
            let (s, n) = if matches!(&**a, Obj::Str(_)) {
                (x, b)
            } else {
                (x, a)
            };
            let n = to_big(n).ok_or_else(|| {
                err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "can't multiply sequence by non-int of type '{}'",
                        val_type_name(m, if matches!(&**a, Obj::Str(_)) { b } else { a })
                    ),
                )
            })?;
            let n = n.to_i64().filter(|n| *n >= 0).ok_or_else(|| {
                err(
                    m,
                    ErrorTag::OverflowError,
                    "cannot fit 'int' into an index-sized integer",
                )
            })?;
            if n.checked_mul(s.chars().count() as i64)
                .is_none_or(|t| t > 1_000_000)
            {
                return Err(err(
                    m,
                    ErrorTag::OverflowError,
                    "repeated string is too long",
                ));
            }
            let mut out = String::with_capacity((s.len() * n as usize).min(2_000_000));
            for _ in 0..n {
                out.push_str(s);
            }
            Ok(v_str(&out))
        }
        (BinOp::Mul, Obj::Bytes(x), _) | (BinOp::Mul, _, Obj::Bytes(x)) => {
            let (bs, n) = if matches!(&**a, Obj::Bytes(_)) {
                (x, b)
            } else {
                (x, a)
            };
            let n = to_big(n)
                .and_then(|n| n.to_i64())
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "can't multiply sequence by non-int of type '{}'",
                            val_type_name(m, if matches!(&**a, Obj::Bytes(_)) { b } else { a })
                        ),
                    )
                })?;
            let mut out: Vec<u8> = Vec::with_capacity(bs.len().saturating_mul(n as usize));
            for _ in 0..n {
                out.extend_from_slice(bs);
            }
            Ok(Rc::new(Obj::Bytes(Rc::from(out.as_slice()))))
        }
        (BinOp::Mul, Obj::List(x), _) | (BinOp::Mul, _, Obj::List(x)) => {
            let (l, n) = if matches!(&**a, Obj::List(_)) {
                (x, b)
            } else {
                (x, a)
            };
            let n = to_big(n)
                .and_then(|n| n.to_i64())
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "can't multiply sequence by non-int of type '{}'",
                            val_type_name(m, if matches!(&**a, Obj::List(_)) { b } else { a })
                        ),
                    )
                })?;
            let src = l.borrow().clone();
            let mut out = Vec::with_capacity(src.len() * n as usize);
            for _ in 0..n {
                out.extend(src.iter().cloned());
            }
            Ok(Rc::new(Obj::List(RefCell::new(out))))
        }
        (BinOp::Mul, Obj::Tuple(x), _) | (BinOp::Mul, _, Obj::Tuple(x)) => {
            let (t, n) = if matches!(&**a, Obj::Tuple(_)) {
                (x, b)
            } else {
                (x, a)
            };
            let n = to_big(n)
                .and_then(|n| n.to_i64())
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "can't multiply sequence by non-int of type '{}'",
                            val_type_name(m, if matches!(&**a, Obj::Tuple(_)) { b } else { a })
                        ),
                    )
                })?;
            let mut out = Vec::with_capacity(t.len() * n as usize);
            for _ in 0..n {
                out.extend(t.iter().cloned());
            }
            Ok(Rc::new(Obj::Tuple(Rc::new(out))))
        }
        (BinOp::BitOr, Obj::Set(_), Obj::Set(_))
        | (BinOp::BitOr, Obj::Set(_), Obj::FrozenSet(_))
        | (BinOp::BitOr, Obj::FrozenSet(_), Obj::Set(_))
        | (BinOp::BitOr, Obj::FrozenSet(_), Obj::FrozenSet(_))
        | (BinOp::BitAnd, Obj::Set(_), Obj::Set(_))
        | (BinOp::BitAnd, Obj::Set(_), Obj::FrozenSet(_))
        | (BinOp::BitAnd, Obj::FrozenSet(_), Obj::Set(_))
        | (BinOp::BitAnd, Obj::FrozenSet(_), Obj::FrozenSet(_))
        | (BinOp::Sub, Obj::Set(_), Obj::Set(_))
        | (BinOp::Sub, Obj::Set(_), Obj::FrozenSet(_))
        | (BinOp::Sub, Obj::FrozenSet(_), Obj::Set(_))
        | (BinOp::Sub, Obj::FrozenSet(_), Obj::FrozenSet(_))
        | (BinOp::BitXor, Obj::Set(_), Obj::Set(_))
        | (BinOp::BitXor, Obj::Set(_), Obj::FrozenSet(_))
        | (BinOp::BitXor, Obj::FrozenSet(_), Obj::Set(_))
        | (BinOp::BitXor, Obj::FrozenSet(_), Obj::FrozenSet(_)) => {
            // result follows the left operand's concrete type, per CPython
            let frozen = match (&**a, &**b) {
                (Obj::FrozenSet(_), _) => true,
                (Obj::Set(_), Obj::FrozenSet(_)) => false,
                (Obj::Set(_), _) => false,
                _ => true,
            };
            set_binop(m, op, a, b, frozen)
        }
        _ => Err(unsupported(m, op, a, b)),
    }
}

fn try_dunder(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    recv: &Val,
    name: &str,
    other: &Val,
) -> Result<Option<Val>, Val> {
    let f = mro_attr(recv, name).expect("dunder present");
    let bound = bind(m, vm, &f, recv, Some(recv.clone()))?;
    let out = call_val(m, vm, &bound, std::slice::from_ref(other), &[])?;
    if matches!(&*out, Obj::NotImplemented) {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn unsupported(m: &TestModel, op: crate::ir::BinOp, a: &Val, b: &Val) -> Val {
    err(
        m,
        ErrorTag::TypeError,
        &format!(
            "unsupported operand type(s) for {}: '{}' and '{}'",
            binop_name(op),
            val_type_name(m, a),
            val_type_name(m, b)
        ),
    )
}

fn set_binop(
    m: &mut TestModel,
    op: crate::ir::BinOp,
    a: &Val,
    b: &Val,
    frozen: bool,
) -> Result<Val, Val> {
    let xa = match &**a {
        Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().values(),
        _ => unreachable!(),
    };
    let xb = match &**b {
        Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().values(),
        _ => unreachable!(),
    };
    let items = match op {
        crate::ir::BinOp::BitOr => {
            let mut v = xa.clone();
            v.extend(xb.iter().cloned());
            v
        }
        crate::ir::BinOp::BitAnd => xa
            .iter()
            .filter(|x| xb.iter().any(|y| eq_novm(x, y)))
            .cloned()
            .collect(),
        crate::ir::BinOp::Sub => xa
            .iter()
            .filter(|x| !xb.iter().any(|y| eq_novm(x, y)))
            .cloned()
            .collect(),
        crate::ir::BinOp::BitXor => {
            let mut v: Vec<Val> = xa
                .iter()
                .filter(|x| !xb.iter().any(|y| eq_novm(x, y)))
                .cloned()
                .collect();
            v.extend(
                xb.iter()
                    .filter(|y| !xa.iter().any(|x| eq_novm(x, y)))
                    .cloned(),
            );
            v
        }
        _ => unreachable!(),
    };
    let table = set_from_items(m, items)?;
    Ok(if frozen {
        Rc::new(Obj::FrozenSet(RefCell::new(table)))
    } else {
        Rc::new(Obj::Set(RefCell::new(table)))
    })
}

fn int_binop(m: &mut TestModel, op: crate::ir::BinOp, x: &BigInt, y: &BigInt) -> Result<Val, Val> {
    use crate::ir::BinOp;
    let int = |b: BigInt| Rc::new(Obj::Int(Rc::new(b)));
    Ok(match op {
        BinOp::Add => int(x.add(y)),
        BinOp::Sub => int(x.sub(y)),
        BinOp::Mul => int(x.mul(y)),
        BinOp::Div => {
            if y.is_zero() {
                return Err(err(m, ErrorTag::ZeroDivisionError, "division by zero"));
            }
            let fx = x.to_f64().ok_or_else(|| {
                err(
                    m,
                    ErrorTag::OverflowError,
                    "integer division result too large for a float",
                )
            })?;
            let fy = y.to_f64().ok_or_else(|| {
                err(
                    m,
                    ErrorTag::OverflowError,
                    "integer division result too large for a float",
                )
            })?;
            Rc::new(Obj::Float(fx / fy))
        }
        BinOp::FloorDiv => {
            if y.is_zero() {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "integer division or modulo by zero",
                ));
            }
            let (q, _) = x.divmod(y).expect("nonzero");
            int(q)
        }
        BinOp::Mod => {
            if y.is_zero() {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "integer modulo by zero",
                ));
            }
            let (_, r) = x.divmod(y).expect("nonzero");
            int(r)
        }
        BinOp::Pow => {
            if y.is_negative() {
                if x.is_zero() {
                    return Err(err(
                        m,
                        ErrorTag::ZeroDivisionError,
                        "0.0 cannot be raised to a negative power",
                    ));
                }
                let fx = x.to_f64().ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::OverflowError,
                        "int too large to convert to float",
                    )
                })?;
                let ey = y
                    .abs()
                    .to_i64()
                    .ok_or_else(|| err(m, ErrorTag::OverflowError, "exponent too large"))?;
                let mag = fx.powi(ey.min(i32::MAX as i64) as i32);
                return Ok(Rc::new(Obj::Float(1.0 / mag)));
            }
            let e = y
                .to_i64()
                .ok_or_else(|| err(m, ErrorTag::OverflowError, "exponent too large"))?;
            if x.bit_len() as i64 * e > 4_000_000 {
                return Err(err(m, ErrorTag::OverflowError, "exponent too large"));
            }
            int(x.pow(e as u64))
        }
        BinOp::LShift => {
            if y.is_negative() {
                return Err(err(m, ErrorTag::ValueError, "negative shift count"));
            }
            let n = y
                .to_i64()
                .ok_or_else(|| err(m, ErrorTag::OverflowError, "shift count too large"))?;
            if n > 1_000_000 {
                return Err(err(m, ErrorTag::OverflowError, "shift count too large"));
            }
            int(x.shl(n as usize))
        }
        BinOp::RShift => {
            if y.is_negative() {
                return Err(err(m, ErrorTag::ValueError, "negative shift count"));
            }
            let n = y
                .to_i64()
                .ok_or_else(|| err(m, ErrorTag::OverflowError, "shift count too large"))?;
            int(x.shr(n.min(1_000_000) as usize))
        }
        BinOp::BitAnd => int(x.bitop(y, 0)),
        BinOp::BitOr => int(x.bitop(y, 1)),
        BinOp::BitXor => int(x.bitop(y, 2)),
        BinOp::MatMul => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "unsupported operand type(s) for @: 'int' and 'int'",
            ));
        }
    })
}

fn float_binop(m: &mut TestModel, op: crate::ir::BinOp, x: &f64, y: &f64) -> Result<Val, Val> {
    use crate::ir::BinOp;
    let f = |v: f64| Rc::new(Obj::Float(v));
    Ok(match op {
        BinOp::Add => f(x + y),
        BinOp::Sub => f(x - y),
        BinOp::Mul => f(x * y),
        BinOp::Div => {
            if *y == 0.0 {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "float division by zero",
                ));
            }
            f(x / y)
        }
        BinOp::FloorDiv => {
            if *y == 0.0 {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "float floor division by zero",
                ));
            }
            f(floordiv_py(*x, *y))
        }
        BinOp::Mod => {
            if *y == 0.0 {
                return Err(err(m, ErrorTag::ZeroDivisionError, "float modulo"));
            }
            f(fmod_py(*x, *y))
        }
        BinOp::Pow => {
            if *x == 0.0 && *y < 0.0 {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "0.0 cannot be raised to a negative power",
                ));
            }
            let r = x.powf(*y);
            if r.is_infinite() && x.is_finite() && y.is_finite() && x.abs() != 1.0 {
                return Err(err(m, ErrorTag::OverflowError, "(34, 'Result too large')"));
            }
            f(r)
        }
        BinOp::LShift
        | BinOp::RShift
        | BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::MatMul => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "unsupported operand type(s) for {}: 'float' and 'float'",
                    binop_name(op)
                ),
            ));
        }
    })
}

fn unary_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    op: crate::ir::UnaryOp,
    v: &Val,
) -> Result<Val, Val> {
    use crate::ir::UnaryOp;
    Ok(match op {
        UnaryOp::Not => {
            let t = truth_val(m, vm, v)?;
            v_bool(!t)
        }
        UnaryOp::Neg => match &**v {
            Obj::Int(i) => Rc::new(Obj::Int(Rc::new(i.neg()))),
            Obj::Bool(b) => Rc::new(Obj::Int(Rc::new(BigInt::from_i64(-i64::from(*b))))),
            Obj::Float(f) => Rc::new(Obj::Float(-f)),
            Obj::Instance(_) => {
                let f = mro_attr(v, "__neg__").ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!("bad operand type for unary -: '{}'", val_type_name(m, v)),
                    )
                })?;
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                call_val(m, vm, &bound, &[], &[])?
            }
            _ => {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!("bad operand type for unary -: '{}'", val_type_name(m, v)),
                ));
            }
        },
        UnaryOp::Pos => match &**v {
            Obj::Int(_) => v.clone(),
            Obj::Bool(b) => v_int(i64::from(*b)),
            Obj::Float(_) => v.clone(),
            Obj::Instance(_) => {
                let f = mro_attr(v, "__pos__").ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!("bad operand type for unary +: '{}'", val_type_name(m, v)),
                    )
                })?;
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                call_val(m, vm, &bound, &[], &[])?
            }
            _ => {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!("bad operand type for unary +: '{}'", val_type_name(m, v)),
                ));
            }
        },
        UnaryOp::Invert => match &**v {
            Obj::Int(i) => Rc::new(Obj::Int(Rc::new(i.invert()))),
            Obj::Bool(b) => Rc::new(Obj::Int(Rc::new(BigInt::from_i64(-i64::from(*b) - 1)))),
            Obj::Instance(_) => {
                let f = mro_attr(v, "__invert__").ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::TypeError,
                        &format!("bad operand type for unary ~: '{}'", val_type_name(m, v)),
                    )
                })?;
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                call_val(m, vm, &bound, &[], &[])?
            }
            _ => {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!("bad operand type for unary ~: '{}'", val_type_name(m, v)),
                ));
            }
        },
    })
}

// ---------------------------------------------------------------------------
// Comparisons
// ---------------------------------------------------------------------------

fn cmp_op_name(op: crate::ir::CmpOp) -> &'static str {
    use crate::ir::CmpOp::*;
    match op {
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        Is => "is",
        IsNot => "is not",
        In => "in",
        NotIn => "not in",
    }
}

fn cmp_dunder(op: crate::ir::CmpOp) -> (&'static str, &'static str) {
    use crate::ir::CmpOp::*;
    match op {
        Eq => ("__eq__", "__eq__"),
        Ne => ("__ne__", "__ne__"),
        Lt => ("__lt__", "__gt__"),
        Le => ("__le__", "__ge__"),
        Gt => ("__gt__", "__lt__"),
        Ge => ("__ge__", "__le__"),
        _ => ("", ""),
    }
}

fn compare_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    op: crate::ir::CmpOp,
    a: &Val,
    b: &Val,
) -> Result<bool, Val> {
    use crate::ir::CmpOp;
    match op {
        CmpOp::Is => return Ok(is_identical(a, b)),
        CmpOp::IsNot => return Ok(!is_identical(a, b)),
        CmpOp::In => return contains_val(m, vm, b, a),
        CmpOp::NotIn => return Ok(!contains_val(m, vm, b, a)?),
        _ => {}
    }
    // instance dunder dispatch
    let a_inst = matches!(&**a, Obj::Instance(_));
    let b_inst = matches!(&**b, Obj::Instance(_));
    if a_inst || b_inst {
        let (l, r) = cmp_dunder(op);
        if a_inst
            && mro_attr(a, l).is_some()
            && let Some(v) = try_cmp_dunder(m, vm, a, l, b)?
        {
            return coerce_cmp(m, op, v);
        }
        if b_inst
            && mro_attr(b, r).is_some()
            && !Rc::ptr_eq(a, b)
            && let Some(v) = try_cmp_dunder(m, vm, b, r, a)?
        {
            return coerce_cmp(m, flip_cmp(op), v);
        }
        if op == CmpOp::Eq {
            return Ok(is_identical(a, b));
        }
        if op == CmpOp::Ne {
            return Ok(!is_identical(a, b));
        }
        return Err(ordering_unsupported(m, op, a, b));
    }
    // numbers
    if let (Some(x), Some(y)) = (to_big(a), to_big(b)) {
        let ord = x.compare(&y);
        return Ok(apply_ord(op, ord));
    }
    if matches!(&**a, Obj::Int(_) | Obj::Bool(_) | Obj::Float(_))
        && matches!(&**b, Obj::Int(_) | Obj::Bool(_) | Obj::Float(_))
    {
        let x = to_float(m, a)?;
        let y = to_float(m, b)?;
        let ord = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Greater);
        return Ok(apply_ord(op, ord));
    }
    match (&**a, &**b) {
        (Obj::Str(x), Obj::Str(y)) => Ok(apply_ord(op, x.cmp(y))),
        (Obj::Bytes(x), Obj::Bytes(y)) => Ok(apply_ord(op, x.cmp(y))),
        (Obj::None_, Obj::None_) | (Obj::Ellipsis, Obj::Ellipsis) => {
            Ok(op == CmpOp::Eq || op == CmpOp::Le || op == CmpOp::Ge)
        }
        (Obj::Tuple(x), Obj::Tuple(y)) => {
            let ord = seq_cmp(m, vm, x.iter(), y.iter())?;
            Ok(apply_ord(op, ord))
        }
        (Obj::List(x), Obj::List(y)) => {
            let xs = x.borrow().clone();
            let ys = y.borrow().clone();
            let ord = seq_cmp(m, vm, xs.iter(), ys.iter())?;
            Ok(apply_ord(op, ord))
        }
        (Obj::Dict(x), Obj::Dict(y)) => {
            if op == CmpOp::Eq {
                let xd = x.borrow();
                let yd = y.borrow();
                if xd.len() != yd.len() {
                    return Ok(false);
                }
                for (k, v) in xd.items() {
                    let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
                    match yd.get_hash(&k, h) {
                        Some(v2) => {
                            if !compare_val(m, vm, CmpOp::Eq, &v, &v2)? {
                                return Ok(false);
                            }
                        }
                        None => return Ok(false),
                    }
                }
                Ok(true)
            } else if op == CmpOp::Ne {
                Ok(!compare_val(m, vm, CmpOp::Eq, a, b)?)
            } else {
                Err(ordering_unsupported(m, op, a, b))
            }
        }
        (Obj::Set(_) | Obj::FrozenSet(_), Obj::Set(_) | Obj::FrozenSet(_)) => {
            let xa = match &**a {
                Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().values(),
                _ => unreachable!(),
            };
            let xb = match &**b {
                Obj::Set(t) | Obj::FrozenSet(t) => t.borrow().values(),
                _ => unreachable!(),
            };
            let sub = |x: &[Val], y: &[Val]| x.iter().all(|e| y.iter().any(|f| eq_novm(e, f)));
            Ok(match op {
                CmpOp::Eq => xa.len() == xb.len() && sub(&xa, &xb),
                CmpOp::Ne => !compare_val(m, vm, CmpOp::Eq, a, b)?,
                CmpOp::Le => sub(&xa, &xb),
                CmpOp::Lt => sub(&xa, &xb) && xa.len() < xb.len(),
                CmpOp::Ge => sub(&xb, &xa),
                CmpOp::Gt => sub(&xb, &xa) && xb.len() < xa.len(),
                _ => unreachable!(),
            })
        }
        (Obj::Slice { .. }, Obj::Slice { .. }) => {
            if matches!(op, CmpOp::Eq | CmpOp::Ne) {
                let (sa, sb, ss) = slice_parts(m, a).expect("slice");
                let (ta, tb, ts) = slice_parts(m, b).expect("slice");
                // `slice(None)` stores an explicit None; an omitted bound is
                // the same value to Python.
                let norm =
                    |x: &Option<Val>| x.as_ref().filter(|v| !matches!(&***v, Obj::None_)).cloned();
                let sa = norm(&sa);
                let sb = norm(&sb);
                let ss = norm(&ss);
                let ta = norm(&ta);
                let tb = norm(&tb);
                let ts = norm(&ts);
                let mut part = |x: &Option<Val>, y: &Option<Val>| -> Result<bool, Val> {
                    match (x, y) {
                        (None, None) => Ok(true),
                        (Some(x), Some(y)) => compare_val(m, vm, CmpOp::Eq, x, y),
                        _ => Ok(false),
                    }
                };
                let eq = part(&sa, &ta)? && part(&sb, &tb)? && part(&ss, &ts)?;
                Ok(if op == CmpOp::Eq { eq } else { !eq })
            } else {
                Err(ordering_unsupported(m, op, a, b))
            }
        }
        (Obj::Bool(x), Obj::Bool(y)) => Ok(apply_ord(op, x.cmp(y))),
        _ => {
            // Identity is the default equality for objects without a
            // structural comparison (classes, functions, ...).
            if op == CmpOp::Eq {
                return Ok(Rc::ptr_eq(a, b));
            }
            if op == CmpOp::Ne {
                return Ok(!Rc::ptr_eq(a, b));
            }
            Err(ordering_unsupported(m, op, a, b))
        }
    }
}

fn seq_cmp<'a>(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    xs: impl Iterator<Item = &'a Val>,
    ys: impl Iterator<Item = &'a Val>,
) -> Result<std::cmp::Ordering, Val> {
    let xs: Vec<&Val> = xs.collect();
    let ys: Vec<&Val> = ys.collect();
    for (x, y) in xs.iter().zip(ys.iter()) {
        if compare_val(m, vm, crate::ir::CmpOp::Eq, x, y)? {
            continue;
        }
        if compare_val(m, vm, crate::ir::CmpOp::Lt, x, y)? {
            return Ok(std::cmp::Ordering::Less);
        }
        if compare_val(m, vm, crate::ir::CmpOp::Gt, x, y)? {
            return Ok(std::cmp::Ordering::Greater);
        }
        return Err(ordering_unsupported(m, crate::ir::CmpOp::Lt, x, y));
    }
    Ok(xs.len().cmp(&ys.len()))
}

fn apply_ord(op: crate::ir::CmpOp, ord: std::cmp::Ordering) -> bool {
    use crate::ir::CmpOp;
    use std::cmp::Ordering::*;
    match (op, ord) {
        (CmpOp::Eq, _) | (CmpOp::Le, _) | (CmpOp::Ge, _) if ord == Equal => true,
        (CmpOp::Ne, _) if ord != Equal => true,
        (CmpOp::Lt, Less) | (CmpOp::Le, Less) => true,
        (CmpOp::Gt, Greater) | (CmpOp::Ge, Greater) => true,
        _ => false,
    }
}

fn flip_cmp(op: crate::ir::CmpOp) -> crate::ir::CmpOp {
    use crate::ir::CmpOp::*;
    match op {
        Eq => Eq,
        Ne => Ne,
        Lt => Gt,
        Le => Ge,
        Gt => Lt,
        Ge => Le,
        other => other,
    }
}

fn coerce_cmp(m: &TestModel, _op: crate::ir::CmpOp, v: Val) -> Result<bool, Val> {
    match &*v {
        Obj::Bool(b) => Ok(*b),
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' cannot be interpreted as a boolean",
                val_type_name(m, &v)
            ),
        )),
    }
}

fn try_cmp_dunder(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    recv: &Val,
    name: &str,
    other: &Val,
) -> Result<Option<Val>, Val> {
    let f = mro_attr(recv, name).expect("dunder present");
    let bound = bind(m, vm, &f, recv, Some(recv.clone()))?;
    let out = call_val(m, vm, &bound, std::slice::from_ref(other), &[])?;
    if matches!(&*out, Obj::NotImplemented) {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn ordering_unsupported(m: &TestModel, op: crate::ir::CmpOp, a: &Val, b: &Val) -> Val {
    err(
        m,
        ErrorTag::TypeError,
        &format!(
            "'{}' not supported between instances of '{}' and '{}'",
            cmp_op_name(op),
            val_type_name(m, a),
            val_type_name(m, b)
        ),
    )
}

fn contains_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    hay: &Val,
    needle: &Val,
) -> Result<bool, Val> {
    match &**hay {
        Obj::Dict(d) => Ok(dict_lookup(m, vm, &d.borrow(), needle)?.is_some()),
        Obj::Set(t) | Obj::FrozenSet(t) => {
            let h = py_hash(needle).map_err(|t| unhashable(m, &t))?;
            let eq = |x: &Val| eq_novm(x, needle);
            Ok(t.borrow().contains(needle, h, &eq))
        }
        Obj::List(l) => {
            let items = l.borrow().clone();
            for item in &items {
                if compare_val(m, vm, crate::ir::CmpOp::Eq, item, needle)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Obj::Tuple(t) => {
            for item in t.iter() {
                if compare_val(m, vm, crate::ir::CmpOp::Eq, item, needle)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Obj::Str(s) => match &**needle {
            Obj::Str(n) => Ok(s.contains(n.as_ref())),
            _ => Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "'in <string>' requires string as left operand, not {}",
                    val_type_name(m, needle)
                ),
            )),
        },
        Obj::Bytes(b) => match &**needle {
            Obj::Bytes(n) => Ok(knuth_find(b, n).is_some()),
            Obj::Int(i) => {
                let v = i.to_i64().unwrap_or(-1);
                Ok((0..=255).contains(&v) && b.contains(&(v as u8)))
            }
            _ => Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "a bytes-like object is required, not '{}'",
                    val_type_name(m, needle)
                ),
            )),
        },
        Obj::Range { start, stop, step } => {
            let n = match to_big(needle) {
                Some(n) => n,
                None => return Ok(false),
            };
            let n = match n.to_i128() {
                Some(n) => n,
                None => return Ok(false),
            };
            if *step > 0 {
                if n < *start || n >= *stop {
                    return Ok(false);
                }
            } else if n > *start || n <= *stop {
                return Ok(false);
            }
            Ok((n - start) % step == 0)
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(hay, "__contains__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, hay, Some(hay.clone()))?;
                let out = call_val(m, vm, &bound, std::slice::from_ref(needle), &[])?;
                return truth_val(m, vm, &out);
            }
            let it = iter_val(m, vm, hay)?;
            loop {
                match next_val(m, vm, &it) {
                    Ok(item) => {
                        if compare_val(m, vm, crate::ir::CmpOp::Eq, &item, needle)? {
                            return Ok(true);
                        }
                    }
                    Err(e) if is_exc_matches(m, &e, "StopIteration") => return Ok(false),
                    Err(e) => return Err(e),
                }
            }
        }
        _ => {
            // Only base types reach here; `x in y` words the failure
            // differently from iter(y).
            let it = iter_val(m, vm, hay).map_err(|_| {
                err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "argument of type '{}' is not iterable",
                        val_type_name(m, hay)
                    ),
                )
            })?;
            loop {
                match next_val(m, vm, &it) {
                    Ok(item) => {
                        if compare_val(m, vm, crate::ir::CmpOp::Eq, &item, needle)? {
                            return Ok(true);
                        }
                    }
                    Err(e) if is_exc_matches(m, &e, "StopIteration") => return Ok(false),
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

fn knuth_find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    'outer: for i in 0..hay.len().saturating_sub(needle.len() - 1) {
        for j in 0..needle.len() {
            if hay[i + j] != needle[j] {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn unhashable(m: &TestModel, type_name: &str) -> Val {
    err(
        m,
        ErrorTag::TypeError,
        &format!("unhashable type: '{type_name}'"),
    )
}

/// Hash honouring a user `__hash__` (calling back into the interpreter).
/// A class body defining `__eq__` without `__hash__` implicitly sets
/// `__hash__ = None`, making instances unhashable.
fn hash_vm(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<i64, Val> {
    if let Obj::Instance(i) = &**v {
        let mut classes = vec![i.class.clone()];
        if let Some(c) = as_class(&i.class) {
            classes.extend(c.mro.iter().cloned());
        }
        for owner in &classes {
            let Some(c) = as_class(owner) else { continue };
            let d = c.dict.borrow();
            if let Some(h) = d.get("__hash__") {
                if matches!(&**h, Obj::None_) {
                    return Err(unhashable(m, &class_name(&i.class)));
                }
                if is_function_like(h) && !matches!(&**h, Obj::Native(..)) {
                    let h = h.clone();
                    drop(d);
                    let bound = bind(m, vm, &h, owner, Some(v.clone()))?;
                    let out = call_val(m, vm, &bound, &[], &[])?;
                    let Some(big) = to_big(&out) else {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            "__hash__ method should return an integer",
                        ));
                    };
                    return Ok(big.py_hash());
                }
                break;
            }
            if d.get("__eq__").is_some() {
                return Err(unhashable(m, &class_name(&i.class)));
            }
        }
    }
    py_hash(v).map_err(|t| unhashable(m, &t))
}

/// Does either side define `__eq__` the hash index cannot evaluate?
fn custom_eq_key(a: &Val, keys: &[Val]) -> bool {
    let inst = |v: &Val| matches!(&**v, Obj::Instance(_));
    inst(a) || keys.iter().any(inst)
}

fn dict_lookup(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    d: &Dict,
    key: &Val,
) -> Result<Option<Val>, Val> {
    let h = hash_vm(m, vm, key)?;
    if let Some(v) = d.get_hash(key, h) {
        return Ok(Some(v));
    }
    if custom_eq_key(key, &d.keys()) {
        for (k, v) in d.items() {
            if !Rc::ptr_eq(&k, key) && compare_val(m, vm, crate::ir::CmpOp::Eq, &k, key)? {
                return Ok(Some(v));
            }
        }
    }
    Ok(None)
}

fn dict_insert(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    d: &mut Dict,
    key: &Val,
    value: &Val,
) -> Result<(), Val> {
    let h = hash_vm(m, vm, key)?;
    if d.get_hash(key, h).is_none() && custom_eq_key(key, &d.keys()) {
        let items = d.items();
        for (k, _) in items {
            if !Rc::ptr_eq(&k, key) && compare_val(m, vm, crate::ir::CmpOp::Eq, &k, key)? {
                d.del_hash(&k, h);
                break;
            }
        }
    }
    d.set_hash(key.clone(), h, value.clone());
    Ok(())
}

fn dict_del(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    d: &mut Dict,
    key: &Val,
) -> Result<bool, Val> {
    let h = hash_vm(m, vm, key)?;
    if d.get_hash(key, h).is_some() {
        d.del_hash(key, h);
        return Ok(true);
    }
    if custom_eq_key(key, &d.keys()) {
        let items = d.items();
        for (k, _) in items {
            if !Rc::ptr_eq(&k, key) && compare_val(m, vm, crate::ir::CmpOp::Eq, &k, key)? {
                d.del_hash(&k, h);
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// `a, b = 5` words a non-iterable as unpack failure, not iteration.
/// Only base types reach this: instance `__iter__` errors pass through.
fn noniterable_unpack(m: &TestModel, src: &Val, e: Val) -> Val {
    if matches!(&**src, Obj::Instance(_)) {
        return e;
    }
    err(
        m,
        ErrorTag::TypeError,
        &format!(
            "cannot unpack non-iterable {} object",
            val_type_name(m, src)
        ),
    )
}

fn new_dict() -> Val {
    Rc::new(Obj::Dict(RefCell::new(Dict::default())))
}

/// String-keyed access into a dict value (instance attribute storage).
fn dict_get_str(d: &Val, name: &str) -> Option<Val> {
    let Obj::Dict(dd) = &**d else {
        return None;
    };
    let key = v_str(name);
    let h = py_hash(&key).ok()?;
    dd.borrow().get_hash(&key, h)
}

fn dict_set_str(d: &Val, name: &str, v: Val) {
    if let Obj::Dict(dd) = &**d {
        let key = v_str(name);
        if let Ok(h) = py_hash(&key) {
            dd.borrow_mut().set_hash(key, h, v);
        }
    }
}

fn dict_del_str(d: &Val, name: &str) -> bool {
    let Obj::Dict(dd) = &**d else {
        return false;
    };
    let key = v_str(name);
    match py_hash(&key) {
        Ok(h) => dd.borrow_mut().del_hash(&key, h),
        Err(_) => false,
    }
}

fn new_dict_from(entries: Vec<(String, Val)>) -> Val {
    let mut d = Dict::default();
    for (k, v) in entries {
        let key = v_str(&k);
        if let Ok(h) = py_hash(&key) {
            d.set_hash(key, h, v);
        }
    }
    Rc::new(Obj::Dict(RefCell::new(d)))
}

// ---------------------------------------------------------------------------
// Subscripting
// ---------------------------------------------------------------------------

fn index_or_error(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    _seq: &Val,
    key: &Val,
    ty: &str,
) -> Result<i64, Val> {
    match py_index(m, vm, key) {
        Ok(i) => Ok(i),
        Err(_) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "{ty} indices must be integers or slices, not {}",
                val_type_name(m, key)
            ),
        )),
    }
}

fn norm_index(i: i64, len: usize) -> Option<usize> {
    let len = len as i64;
    let i = if i < 0 { i + len } else { i };
    if i >= 0 && i < len {
        Some(i as usize)
    } else {
        None
    }
}

fn getitem_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    key: &Val,
) -> Result<Val, Val> {
    match &**obj {
        Obj::List(l) => {
            let items = l.borrow().clone();
            if matches!(&**key, Obj::Slice { .. }) {
                let idxs = slice_range(m, vm, key, items.len() as i64)?;
                let out: Vec<Val> = idxs.into_iter().map(|i| items[i].clone()).collect();
                return Ok(Rc::new(Obj::List(RefCell::new(out))));
            }
            let i = index_or_error(m, vm, obj, key, "list")?;
            norm_index(i, items.len())
                .map(|i| items[i].clone())
                .ok_or_else(|| err(m, ErrorTag::IndexError, "list index out of range"))
        }
        Obj::Tuple(t) => {
            if matches!(&**key, Obj::Slice { .. }) {
                let idxs = slice_range(m, vm, key, t.len() as i64)?;
                let out: Vec<Val> = idxs.into_iter().map(|i| t[i].clone()).collect();
                return Ok(Rc::new(Obj::Tuple(Rc::new(out))));
            }
            let i = index_or_error(m, vm, obj, key, "tuple")?;
            norm_index(i, t.len())
                .map(|i| t[i].clone())
                .ok_or_else(|| err(m, ErrorTag::IndexError, "tuple index out of range"))
        }
        Obj::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            if matches!(&**key, Obj::Slice { .. }) {
                let idxs = slice_range(m, vm, key, chars.len() as i64)?;
                let out: String = idxs.into_iter().map(|i| chars[i]).collect();
                return Ok(v_str(&out));
            }
            let i = index_or_error(m, vm, obj, key, "string")?;
            norm_index(i, chars.len())
                .map(|i| v_str(chars[i].to_string().as_str()))
                .ok_or_else(|| err(m, ErrorTag::IndexError, "string index out of range"))
        }
        Obj::Bytes(b) => {
            if matches!(&**key, Obj::Slice { .. }) {
                let idxs = slice_range(m, vm, key, b.len() as i64)?;
                let out: Vec<u8> = idxs.into_iter().map(|i| b[i]).collect();
                return Ok(Rc::new(Obj::Bytes(Rc::from(out.as_slice()))));
            }
            let i = index_or_error(m, vm, obj, key, "index")?;
            norm_index(i, b.len())
                .map(|i| v_int(b[i] as i64))
                .ok_or_else(|| err(m, ErrorTag::IndexError, "index out of range"))
        }
        Obj::Dict(d) => {
            let found = dict_lookup(m, vm, &d.borrow(), key)?;
            found.ok_or_else(|| key_error(m, key))
        }
        Obj::Range { start, stop, step } => {
            let len = range_len(*start, *stop, *step) as i64;
            if matches!(&**key, Obj::Slice { .. }) {
                let idxs = slice_range(m, vm, key, len)?;
                let out: Vec<Val> = idxs
                    .into_iter()
                    .map(|i| {
                        Rc::new(Obj::Int(Rc::new(BigInt::from_i64(
                            (start + (i as i128) * step) as i64,
                        ))))
                    })
                    .collect();
                return Ok(Rc::new(Obj::Tuple(Rc::new(out))));
            }
            let i = match to_big(key) {
                Some(n) => n.to_i64().ok_or_else(|| {
                    err(
                        m,
                        ErrorTag::OverflowError,
                        "cannot fit 'int' into an index-sized integer",
                    )
                })?,
                None => {
                    return Err(err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "range indices must be integers or slices, not {}",
                            val_type_name(m, key)
                        ),
                    ));
                }
            };
            norm_index(i, len as usize)
                .map(|i| {
                    Rc::new(Obj::Int(Rc::new(BigInt::from_i64(
                        (start + i as i128 * step) as i64,
                    ))))
                })
                .ok_or_else(|| err(m, ErrorTag::IndexError, "range object index out of range"))
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(obj, "__getitem__") {
                let bound = bind(m, vm, &f, obj, Some(obj.clone()))?;
                return call_val(m, vm, &bound, std::slice::from_ref(key), &[]);
            }
            Err(not_subscriptable(m, obj))
        }
        _ => Err(not_subscriptable(m, obj)),
    }
}

fn not_subscriptable(m: &TestModel, obj: &Val) -> Val {
    err(
        m,
        ErrorTag::TypeError,
        &format!("'{}' object is not subscriptable", val_type_name(m, obj)),
    )
}

/// `KeyError` carries the missing key itself as `args[0]`; `str()` is what
/// quotes a string key.
fn key_error(m: &TestModel, key: &Val) -> Val {
    let class = m
        .exc_by_tag
        .get(&ErrorTag::KeyError)
        .or_else(|| m.exc_by_tag.get(&ErrorTag::RuntimeError))
        .expect("KeyError class")
        .clone();
    new_instance(&class, vec![key.clone()], None)
}

/// repr of builtin values without running user code (KeyError messages).
fn repr_static(m: &TestModel, v: &Val) -> Option<String> {
    Some(match &**v {
        Obj::None_ => "None".to_string(),
        Obj::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Obj::Int(i) => i.to_string(),
        Obj::Float(f) => float_repr(*f),
        Obj::Str(s) => str_repr(s),
        Obj::Bytes(b) => bytes_repr(b),
        Obj::Tuple(t) => {
            let inner: Vec<String> = t.iter().map(|x| repr_static(m, x)).collect::<Option<_>>()?;
            if inner.len() == 1 {
                format!("({},)", inner[0])
            } else {
                format!("({})", inner.join(", "))
            }
        }
        _ => {
            let _ = m;
            return None;
        }
    })
}

fn setitem_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    key: &Val,
    value: &Val,
) -> Result<(), Val> {
    match &**obj {
        Obj::List(l) => {
            if matches!(&**key, Obj::Slice { .. }) {
                let items = l.borrow().clone();
                let idxs = slice_range(m, vm, key, items.len() as i64)?;
                let src: Vec<Val> = match &**value {
                    Obj::List(v) => v.borrow().clone(),
                    Obj::Tuple(t) => t.iter().cloned().collect(),
                    _ => {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            &format!(
                                "can only assign an iterable to a slice, not '{}'",
                                val_type_name(m, value)
                            ),
                        ));
                    }
                };
                let step = match slice_indices(m, vm, key, items.len() as i64) {
                    Ok((_, _, st)) => st,
                    Err(e) => return Err(e),
                };
                if step != 1 {
                    if idxs.len() != src.len() {
                        return Err(err(
                            m,
                            ErrorTag::ValueError,
                            &format!(
                                "attempt to assign sequence of size {} to extended slice of size {}",
                                src.len(),
                                idxs.len()
                            ),
                        ));
                    }
                    for (pos, v) in idxs.into_iter().zip(src) {
                        l.borrow_mut()[pos] = v;
                    }
                    return Ok(());
                }
                let (lo, hi) = slice_replace_span(m, vm, key, items.len() as i64)?;
                let mut out: Vec<Val> = Vec::new();
                out.extend_from_slice(&items[..lo]);
                out.extend(src.iter().cloned());
                out.extend_from_slice(&items[hi.min(items.len())..]);
                *l.borrow_mut() = out;
                return Ok(());
            }
            let i = index_or_error(m, vm, obj, key, "list")?;
            let len = l.borrow().len();
            match norm_index(i, len) {
                Some(i) => {
                    l.borrow_mut()[i] = value.clone();
                    Ok(())
                }
                None => Err(err(
                    m,
                    ErrorTag::IndexError,
                    "list assignment index out of range",
                )),
            }
        }
        Obj::Dict(d) => {
            dict_insert(m, vm, &mut d.borrow_mut(), key, value)?;
            Ok(())
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(obj, "__setitem__") {
                let bound = bind(m, vm, &f, obj, Some(obj.clone()))?;
                call_val(m, vm, &bound, &[key.clone(), value.clone()], &[])?;
                return Ok(());
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "'{}' object does not support item assignment",
                    val_type_name(m, obj)
                ),
            ))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' object does not support item assignment",
                val_type_name(m, obj)
            ),
        )),
    }
}

/// The contiguous [lo, hi) span a step-1 slice replaces.
fn slice_replace_span(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    key: &Val,
    len: i64,
) -> Result<(usize, usize), Val> {
    let (start, stop, step) = slice_indices(m, vm, key, len)?;
    debug_assert_eq!(step, 1);
    Ok((start.max(0) as usize, stop.max(0) as usize))
}

fn delitem_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    obj: &Val,
    key: &Val,
) -> Result<(), Val> {
    match &**obj {
        Obj::List(l) => {
            if matches!(&**key, Obj::Slice { .. }) {
                let len = l.borrow().len() as i64;
                let idxs = slice_range(m, vm, key, len)?;
                for pos in idxs.into_iter().rev() {
                    l.borrow_mut().remove(pos);
                }
                return Ok(());
            }
            let i = index_or_error(m, vm, obj, key, "list")?;
            let len = l.borrow().len();
            match norm_index(i, len) {
                Some(i) => {
                    l.borrow_mut().remove(i);
                    Ok(())
                }
                None => Err(err(
                    m,
                    ErrorTag::IndexError,
                    "list assignment index out of range",
                )),
            }
        }
        Obj::Dict(d) => {
            if dict_del(m, vm, &mut d.borrow_mut(), key)? {
                Ok(())
            } else {
                Err(key_error(m, key))
            }
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(obj, "__delitem__") {
                let bound = bind(m, vm, &f, obj, Some(obj.clone()))?;
                call_val(m, vm, &bound, std::slice::from_ref(key), &[])?;
                return Ok(());
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "'{}' object does not support item deletion",
                    val_type_name(m, obj)
                ),
            ))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' object does not support item deletion",
                val_type_name(m, obj)
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// Generators / coroutines
// ---------------------------------------------------------------------------

fn gen_already_running(m: &TestModel, kind: GenKind) -> Val {
    let what = match kind {
        GenKind::Generator => "generator",
        GenKind::Coroutine => "coroutine",
        GenKind::AsyncGen => "async generator",
    };
    err(
        m,
        ErrorTag::ValueError,
        &format!("{what} already executing"),
    )
}

fn gen_exhausted(m: &TestModel, g: &GenState) -> Val {
    let value = g.ret.borrow().clone().unwrap_or_else(v_none);
    match g.kind {
        GenKind::AsyncGen => new_instance(&m.exc_stop_async, vec![value], None),
        _ => stop_iteration_value(m, value),
    }
}

fn gen_send(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    g: &Rc<GenState>,
    v: &Val,
) -> Result<Val, Val> {
    if g.running.get() {
        return Err(gen_already_running(m, g.kind));
    }
    let Some(mut frame) = g.frame.borrow_mut().take() else {
        return Err(gen_exhausted(m, g));
    };
    let fresh = !g.started.get();
    if fresh && !matches!(&**v, Obj::None_) {
        let what = match g.kind {
            GenKind::AsyncGen => "async generator",
            _ => "generator",
        };
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!("can't send non-None value to a just-started {what}"),
        ));
    }
    g.started.set(true);
    g.running.set(true);
    let action = if fresh {
        crate::interp::Resume::Start
    } else {
        crate::interp::Resume::Send(v.clone())
    };
    let result = vm.resume_frame(m, &mut frame, action);
    g.running.set(false);
    match result {
        Ok(crate::interp::Flow::Done(v)) => {
            *g.ret.borrow_mut() = Some(v.clone());
            Err(gen_exhausted(m, g))
        }
        Ok(crate::interp::Flow::Suspended(y)) => {
            *g.frame.borrow_mut() = Some(frame);
            Ok(y)
        }
        Err(e) => Err(e),
    }
}

fn gen_throw(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    g: &Rc<GenState>,
    exc: &Val,
) -> Result<Val, Val> {
    if g.running.get() {
        return Err(gen_already_running(m, g.kind));
    }
    let exc = instantiate_exc(m, vm, exc)?;
    let Some(mut frame) = g.frame.borrow_mut().take() else {
        return Err(exc);
    };
    let fresh = !g.started.get();
    g.started.set(true);
    g.running.set(true);
    let result = if fresh {
        Err(exc)
    } else {
        vm.resume_frame(m, &mut frame, crate::interp::Resume::Throw(exc))
    };
    g.running.set(false);
    match result {
        Ok(crate::interp::Flow::Done(v)) => {
            *g.ret.borrow_mut() = Some(v.clone());
            Err(gen_exhausted(m, g))
        }
        Ok(crate::interp::Flow::Suspended(y)) => {
            *g.frame.borrow_mut() = Some(frame);
            Ok(y)
        }
        Err(e) => Err(e),
    }
}

fn gen_close(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    g: &Rc<GenState>,
) -> Result<Val, Val> {
    if !g.started.get() {
        g.started.set(true);
        *g.frame.borrow_mut() = None;
        return Ok(v_none());
    }
    if g.frame.borrow().is_none() {
        return Ok(v_none());
    }
    let exit = new_instance(&m.exc_generator_exit, Vec::new(), None);
    match gen_throw(m, vm, g, &exit) {
        Err(e)
            if is_exc_matches(m, &e, "StopIteration") || is_exc_matches(m, &e, "GeneratorExit") =>
        {
            Ok(v_none())
        }
        Err(e) => Err(e),
        Ok(_) => Err(err(
            m,
            ErrorTag::RuntimeError,
            "generator ignored GeneratorExit",
        )),
    }
}

/// `raise X` accepts a class or an instance.
fn instantiate_exc(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<Val, Val> {
    if is_exc_instance(m, v) {
        return Ok(v.clone());
    }
    if is_exc_class(m, v) {
        return call_val(m, vm, v, &[], &[]);
    }
    Err(err(
        m,
        ErrorTag::TypeError,
        "exceptions must derive from BaseException",
    ))
}

// ---------------------------------------------------------------------------
// format(value, spec)
// ---------------------------------------------------------------------------

struct Spec {
    fill: char,
    align: Option<char>,
    sign: Option<char>,
    alternate: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    ty: Option<char>,
}

fn parse_spec(spec: &str) -> Result<Spec, String> {
    let cs: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let (fill, align) = if cs.len() >= 2 && matches!(cs[1], '<' | '>' | '=' | '^') {
        let f = cs[0];
        i = 2;
        (f, Some(cs[1]))
    } else if !cs.is_empty() && matches!(cs[0], '<' | '>' | '=' | '^') {
        i = 1;
        (' ', Some(cs[0]))
    } else {
        (' ', None)
    };
    let mut sign = None;
    if i < cs.len() && matches!(cs[i], '+' | '-' | ' ') {
        sign = Some(cs[i]);
        i += 1;
    }
    let mut alternate = false;
    if i < cs.len() && cs[i] == '#' {
        alternate = true;
        i += 1;
    }
    let mut zero = false;
    if i < cs.len() && cs[i] == '0' {
        zero = true;
        i += 1;
    }
    let mut width = None;
    let start = i;
    while i < cs.len() && cs[i].is_ascii_digit() {
        i += 1;
    }
    if i > start {
        let digits: String = cs[start..i].iter().collect();
        width = Some(digits.parse().map_err(|_| "Invalid format specifier")?);
    }
    let mut grouping = None;
    if i < cs.len() && (cs[i] == ',' || cs[i] == '_') {
        grouping = Some(cs[i]);
        i += 1;
    }
    let mut precision = None;
    if i < cs.len() && cs[i] == '.' {
        i += 1;
        let start = i;
        while i < cs.len() && cs[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return Err("Format specifier missing precision".to_string());
        }
        let digits: String = cs[start..i].iter().collect();
        precision = Some(digits.parse().map_err(|_| "Invalid format specifier")?);
    }
    let ty = if i < cs.len() {
        let t = cs[i];
        i += 1;
        Some(t)
    } else {
        None
    };
    if i != cs.len() {
        return Err("Invalid format specifier".to_string());
    }
    Ok(Spec {
        fill,
        align,
        sign,
        alternate,
        zero,
        width,
        grouping,
        precision,
        ty,
    })
}

fn pad_numeric(
    out: &mut String,
    fill: char,
    align: Option<char>,
    zero: bool,
    width: Option<usize>,
    body: &str,
) {
    // `out` is empty on entry: this appends `body` padded to `width`.
    let w = width.unwrap_or(0);
    let pad = w.saturating_sub(body.chars().count());
    let align = align.or(if zero { Some('=') } else { None }).unwrap_or('>');
    match align {
        '<' => {
            out.push_str(body);
            for _ in 0..pad {
                out.push(fill);
            }
        }
        '^' => {
            let l = pad / 2;
            for _ in 0..l {
                out.push(fill);
            }
            out.push_str(body);
            for _ in 0..pad - l {
                out.push(fill);
            }
        }
        '=' | '>' => {
            if zero && align == '=' {
                let (sign_part, digits) = split_sign(body);
                out.push_str(sign_part);
                for _ in 0..pad {
                    out.push('0');
                }
                out.push_str(digits);
            } else {
                for _ in 0..pad {
                    out.push(fill);
                }
                out.push_str(body);
            }
        }
        _ => {
            out.push_str(body);
            for _ in 0..pad {
                out.push(fill);
            }
        }
    }
}

fn split_sign(body: &str) -> (&str, &str) {
    if let Some(rest) = body.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = body.strip_prefix('+') {
        ("+", rest)
    } else {
        ("", body)
    }
}

fn group_digits(digits: &str, sep: char, group: usize) -> String {
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let first = bytes.len() % group;
    if first > 0 {
        out.push_str(&digits[..first]);
    }
    for (n, chunk) in digits.as_bytes()[first.min(bytes.len())..]
        .chunks(group)
        .enumerate()
    {
        if n > 0 || first > 0 {
            out.push(sep);
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    out
}

fn apply_sign(neg: bool, sign: Option<char>, digits: &str) -> String {
    if neg {
        format!("-{digits}")
    } else {
        match sign {
            Some('+') => format!("+{digits}"),
            Some(' ') => format!(" {digits}"),
            _ => digits.to_string(),
        }
    }
}

fn fmt_int_spec(m: &mut TestModel, v: &BigInt, spec: &str) -> Result<String, Val> {
    let s = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
    if let Some(t) = s.ty
        && !"dobxXnceEfgGn%".contains(t)
    {
        return Err(err(
            m,
            ErrorTag::ValueError,
            &format!("Unknown format code '{t}' for object of type 'int'"),
        ));
    }
    if s.precision.is_some() {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "Precision not allowed in integer format specifier",
        ));
    }
    if s.ty == Some('n') {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "Format code 'n' is not supported by the test model",
        ));
    }
    let neg = v.is_negative();
    let mag = v.abs();
    if matches!(
        s.ty,
        Some('e') | Some('E') | Some('f') | Some('F') | Some('g') | Some('G') | Some('%')
    ) {
        let f = mag.to_f64().ok_or_else(|| {
            err(
                m,
                ErrorTag::OverflowError,
                "int too large to convert to float",
            )
        })?;
        let signed = if neg { -f } else { f };
        return fmt_float_parts(m, signed, spec, s.ty);
    }
    let mut body = match s.ty {
        Some('c') => {
            if neg {
                return Err(err(
                    m,
                    ErrorTag::ValueError,
                    "Negative format counts not allowed in character format specifier",
                ));
            }
            let code = mag
                .to_i64()
                .ok_or_else(|| err(m, ErrorTag::OverflowError, "%c arg not in range(0x110000)"))?;
            let c = char::from_u32(u32::try_from(code).unwrap_or(u32::MAX))
                .ok_or_else(|| err(m, ErrorTag::ValueError, "%c arg not in range(0x110000)"))?;
            let mut out = String::new();
            pad_numeric(&mut out, s.fill, s.align, s.zero, s.width, &c.to_string());
            return Ok(out);
        }
        Some('b') => mag.to_string_radix(2),
        Some('o') => mag.to_string_radix(8),
        Some('x') => mag.to_string_radix(16),
        Some('X') => mag.to_string_radix(16).to_uppercase(),
        _ => mag.to_string(),
    };
    let base_is_int = !matches!(s.ty, Some('b') | Some('o') | Some('x') | Some('X'));
    if let Some(g) = s.grouping {
        if !base_is_int && g == ',' {
            let t = s.ty.unwrap_or('d');
            return Err(err(
                m,
                ErrorTag::ValueError,
                &format!("Cannot specify ',' with '{t}'."),
            ));
        }
        let group = if base_is_int { 3 } else { 4 };
        body = group_digits(&body, g, group);
    }
    let prefix = if s.alternate {
        match s.ty {
            Some('b') => "0b",
            Some('o') => "0o",
            Some('x') => "0x",
            Some('X') => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let sign_str = if neg {
        "-"
    } else {
        match s.sign {
            Some('+') => "+",
            Some(' ') => " ",
            _ => "",
        }
    };
    let full = format!("{sign_str}{prefix}{body}");
    let mut out = String::new();
    pad_numeric(&mut out, s.fill, s.align, s.zero, s.width, &full);
    Ok(out)
}

fn fmt_float_spec(m: &mut TestModel, v: f64, spec: &str) -> Result<String, Val> {
    let s = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
    if let Some(t) = s.ty
        && !"eEfFgGn%".contains(t)
    {
        return Err(err(
            m,
            ErrorTag::ValueError,
            &format!("Unknown format code '{t}' for object of type 'float'"),
        ));
    }
    if s.ty == Some('n') {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "Format code 'n' is not supported by the test model",
        ));
    }
    fmt_float_parts(m, v, spec, s.ty)
}

fn fmt_float_parts(m: &mut TestModel, v: f64, spec: &str, ty: Option<char>) -> Result<String, Val> {
    let s = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
    let precision = s.precision.unwrap_or(6);
    let neg = v.is_sign_negative() && !v.is_nan();
    let mag = v.abs();
    if !v.is_finite() {
        let core = if v.is_nan() {
            "nan".to_string()
        } else if neg {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
        let mut out = String::new();
        pad_numeric(&mut out, s.fill, s.align, s.zero, s.width, &core);
        return Ok(out);
    }
    let body = match ty {
        Some('f') | Some('F') => {
            let d = format!("{:.*}", precision, mag);
            let grouped = apply_group(&d, s.grouping);
            apply_sign(neg, s.sign, &grouped)
        }
        Some('%') => {
            let d = format!("{:.*}", precision, mag * 100.0);
            let grouped = apply_group(&d, s.grouping);
            format!("{}%", apply_sign(neg, s.sign, &grouped))
        }
        Some('e') | Some('E') => {
            let raw = format!("{:.*e}", precision, mag);
            let mut d = fix_exp(&raw);
            if ty == Some('E') {
                d = d.to_uppercase();
            }
            let grouped = apply_group(&d, s.grouping);
            apply_sign(neg, s.sign, &grouped)
        }
        Some('g') | Some('G') | None => {
            // No type and no precision means str(): the shortest
            // round-trip repr, not 'g' with 6 digits.
            if ty.is_none() && s.precision.is_none() {
                let d = float_repr(v);
                let mut out = String::new();
                pad_numeric(&mut out, s.fill, s.align, s.zero, s.width, &d);
                return Ok(out);
            }
            let exp = decimal_exp(mag);
            let p = if precision == 0 { 1 } else { precision };
            let mut d = if exp < -4 || exp >= p as i32 {
                let raw = format!("{:.*e}", { p - 1 }, mag);
                fix_exp(&raw)
            } else {
                let frac = (p as i32 - 1 - exp).max(0) as usize;
                let frac = frac.min(17);
                format!("{:.*}", frac, mag)
            };
            if !s.alternate {
                if d.contains('.') {
                    while d.ends_with('0') {
                        d.pop();
                    }
                    if d.ends_with('.') {
                        d.pop();
                    }
                } else if (ty == Some('g') || ty == Some('G') || ty.is_none()) && d.contains('e') {
                    // exponent form keeps its digits as-is
                }
            }
            if ty == Some('G') {
                d = d.to_uppercase();
            }
            let grouped = apply_group(&d, s.grouping);
            apply_sign(neg, s.sign, &grouped)
        }
        _ => unreachable!(),
    };
    let mut out = String::new();
    pad_numeric(&mut out, s.fill, s.align, s.zero, s.width, &body);
    Ok(out)
}

/// The decimal exponent of a float (the position of its leading digit).
fn decimal_exp(v: f64) -> i32 {
    if v == 0.0 {
        return 0;
    }
    let repr = format!("{v:e}");
    let exp: i32 = repr.rsplit('e').next().unwrap_or("0").parse().unwrap_or(0);
    exp
}

/// Rust `1.5e2` → Python `1.5e+02`.
fn fix_exp(raw: &str) -> String {
    let (mant, exp) = match raw.split_once('e') {
        Some((m, e)) => (m, e),
        None => return raw.to_string(),
    };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp.strip_prefix('+').unwrap_or(exp)),
    };
    if digits.len() < 2 {
        format!("{mant}e{sign}0{digits}")
    } else {
        format!("{mant}e{sign}{digits}")
    }
}

fn apply_group(digits: &str, grouping: Option<char>) -> String {
    match grouping {
        Some(g) => {
            let (int_part, frac) = match digits.split_once('.') {
                Some((i, f)) => (i, Some(f)),
                None => (digits, None),
            };
            let grouped = group_digits(int_part, g, 3);
            match frac {
                Some(f) => format!("{grouped}.{f}"),
                None => grouped,
            }
        }
        None => digits.to_string(),
    }
}

fn format_val(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    v: &Val,
    spec: &Val,
) -> Result<Val, Val> {
    let spec = match &**spec {
        Obj::Str(s) => s.to_string(),
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "unsupported format string passed to {}.__format__",
                    val_type_name(m, v)
                ),
            ));
        }
    };
    let out = format_str(m, vm, v, &spec)?;
    Ok(v_str(&out))
}

fn format_str(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    v: &Val,
    spec: &str,
) -> Result<String, Val> {
    match &**v {
        Obj::Int(i) => fmt_int_spec(m, i, spec),
        Obj::Bool(b) => {
            // bool formats as its name unless a numeric type is asked for.
            let has_num_ty = spec
                .chars()
                .last()
                .is_some_and(|c| "bodxXeEfFgGn%".contains(c));
            if has_num_ty {
                fmt_int_spec(m, &BigInt::from_i64(i64::from(*b)), spec)
            } else {
                let sp = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
                let body = if *b { "True" } else { "False" };
                let mut out = String::new();
                let align = sp.align.or(Some('<'));
                pad_numeric(&mut out, sp.fill, align, false, sp.width, body);
                Ok(out)
            }
        }
        Obj::None_ => {
            let sp = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
            let mut out = String::new();
            let align = sp.align.or(Some('<'));
            pad_numeric(&mut out, sp.fill, align, false, sp.width, "None");
            Ok(out)
        }
        Obj::Float(f) => fmt_float_spec(m, *f, spec),
        Obj::Str(s) => {
            let sp = parse_spec(spec).map_err(|msg| err(m, ErrorTag::ValueError, &msg))?;
            if let Some(t) = sp.ty
                && t != 's'
            {
                return Err(err(
                    m,
                    ErrorTag::ValueError,
                    &format!("Unknown format code '{t}' for object of type 'str'"),
                ));
            }
            if sp.sign.is_some() || sp.alternate || sp.zero || sp.grouping.is_some() {
                return Err(err(
                    m,
                    ErrorTag::ValueError,
                    "Sign not allowed in string format specifier",
                ));
            }
            let mut text = s.to_string();
            if let Some(p) = sp.precision {
                text = text.chars().take(p).collect();
            }
            let mut out = String::new();
            let align = sp.align.or(Some('<'));
            pad_numeric(&mut out, sp.fill, align, false, sp.width, &text);
            Ok(out)
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__format__") {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                let out = call_val(m, vm, &bound, &[v_str(spec)], &[])?;
                return match &*out {
                    Obj::Str(s) => Ok(s.to_string()),
                    _ => Err(err(
                        m,
                        ErrorTag::TypeError,
                        &format!(
                            "__format__ must return a str, not {}",
                            val_type_name(m, &out)
                        ),
                    )),
                };
            }
            if spec.is_empty() {
                return str_val(m, vm, v);
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "unsupported format string passed to {}.__format__",
                    val_type_name(m, v)
                ),
            ))
        }
        _ => {
            if spec.is_empty() {
                return str_val(m, vm, v);
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "unsupported format string passed to {}.__format__",
                    val_type_name(m, v)
                ),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Builtin methods: `obj.method` for the model's builtin types. Each is a
// `BoundMethod` whose receiver is `args[0]` at call time.
// ---------------------------------------------------------------------------

fn bmethod(recv: Val, name: &'static str, f: NativeFn) -> Val {
    Rc::new(Obj::BoundMethod {
        recv,
        func: Rc::new(Obj::Native(name, f)),
    })
}

/// A placeholder value of a builtin type, used to look up that type's
/// methods on the class itself (`str.upper`).
fn builtin_prototype(bt: BuiltinType) -> Option<Val> {
    Some(match bt {
        BuiltinType::Str => v_str(""),
        BuiltinType::Bytes => Rc::new(Obj::Bytes(Rc::from(&b""[..]))),
        BuiltinType::Int => v_int(0),
        BuiltinType::Float => Rc::new(Obj::Float(0.0)),
        BuiltinType::Bool => v_bool(false),
        BuiltinType::List => Rc::new(Obj::List(RefCell::new(Vec::new()))),
        BuiltinType::Tuple => Rc::new(Obj::Tuple(Rc::new(Vec::new()))),
        BuiltinType::Dict => Rc::new(Obj::Dict(RefCell::new(Dict::default()))),
        BuiltinType::Set => Rc::new(Obj::Set(RefCell::new(SetTable::default()))),
        BuiltinType::FrozenSet => Rc::new(Obj::FrozenSet(RefCell::new(SetTable::default()))),
        BuiltinType::Slice => Rc::new(Obj::Slice {
            start: None,
            stop: None,
            step: None,
        }),
        _ => return None,
    })
}

/// Drain an iterable into a `Vec`, stopping at `StopIteration`.
fn drain_iter(m: &mut TestModel, vm: &mut Interp<'_, TestModel>, v: &Val) -> Result<Vec<Val>, Val> {
    let it = iter_val(m, vm, v)?;
    let mut out = Vec::new();
    loop {
        match next_val(m, vm, &it) {
            Ok(x) => out.push(x),
            Err(e) => {
                if is_exc_matches(m, &e, "StopIteration") {
                    return Ok(out);
                }
                return Err(e);
            }
        }
    }
}

/// Python `==` for two values, via the full comparison machinery.
fn val_eq(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    a: &Val,
    b: &Val,
) -> Result<bool, Val> {
    compare_val(m, vm, CmpOp::Eq, a, b)
}

/// Total ordering for `sort`/`sorted`; a comparison error sorts as equal.
fn cmp_total(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    a: &Val,
    b: &Val,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let Ok(true) = compare_val(m, vm, CmpOp::Lt, a, b) {
        return Ordering::Less;
    }
    if let Ok(true) = compare_val(m, vm, CmpOp::Gt, a, b) {
        return Ordering::Greater;
    }
    Ordering::Equal
}

fn sort_items(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    items: Vec<Val>,
    keyf: &Option<Val>,
    reverse: bool,
) -> Result<Vec<Val>, Val> {
    let mut keyed: Vec<(Val, Val)> = Vec::with_capacity(items.len());
    for x in &items {
        let k = match keyf {
            Some(f) => call_val(m, vm, f, std::slice::from_ref(x), &[])?,
            None => x.clone(),
        };
        keyed.push((k, x.clone()));
    }
    // Inverting the comparator (rather than reversing afterwards) keeps
    // the sort stable under reverse=True, as CPython's is.
    keyed.sort_by(|a, b| {
        if reverse {
            cmp_total(m, vm, &b.0, &a.0)
        } else {
            cmp_total(m, vm, &a.0, &b.0)
        }
    });
    Ok(keyed.into_iter().map(|(_, x)| x).collect())
}

fn kwarg_bool(kwargs: &[(&str, Val)], name: &str) -> bool {
    kwargs
        .iter()
        .find(|(k, _)| *k == name)
        .is_some_and(|(_, v)| matches!(&**v, Obj::Bool(true)))
}

fn kwarg_val(kwargs: &[(&str, Val)], name: &str) -> Option<Val> {
    kwargs
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| (*v).clone())
}

/// `prop.setter(f)`: a copy of the property with `fset` replaced.
/// `getter`/`deleter` follow the same shape with their slot.
fn prop_setter(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (fget, fset, fdel) = match &*args[0] {
        Obj::Property { fget, fset, fdel } => (fget.clone(), fset.clone(), fdel.clone()),
        _ => (None, None, None),
    };
    let fset = args
        .get(1)
        .filter(|f| !matches!(&***f, Obj::None_))
        .cloned()
        .or(fset);
    Ok(Rc::new(Obj::Property { fget, fset, fdel }))
}

fn prop_getter(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (fget, fset, fdel) = match &*args[0] {
        Obj::Property { fget, fset, fdel } => (fget.clone(), fset.clone(), fdel.clone()),
        _ => (None, None, None),
    };
    let fget = args
        .get(1)
        .filter(|f| !matches!(&***f, Obj::None_))
        .cloned()
        .or(fget);
    Ok(Rc::new(Obj::Property { fget, fset, fdel }))
}

fn prop_deleter(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (fget, fset, fdel) = match &*args[0] {
        Obj::Property { fget, fset, fdel } => (fget.clone(), fset.clone(), fdel.clone()),
        _ => (None, None, None),
    };
    let fdel = args
        .get(1)
        .filter(|f| !matches!(&***f, Obj::None_))
        .cloned()
        .or(fdel);
    Ok(Rc::new(Obj::Property { fget, fset, fdel }))
}

fn builtin_method(_m: &mut TestModel, obj: &Val, name: &str) -> Option<Val> {
    let recv = obj.clone();
    match &**obj {
        Obj::List(_) => match name {
            "append" => Some(bmethod(recv, "append", list_append)),
            "extend" => Some(bmethod(recv, "extend", list_extend)),
            "insert" => Some(bmethod(recv, "insert", list_insert)),
            "remove" => Some(bmethod(recv, "remove", list_remove)),
            "pop" => Some(bmethod(recv, "pop", list_pop)),
            "clear" => Some(bmethod(recv, "clear", list_clear)),
            "index" => Some(bmethod(recv, "index", list_index)),
            "count" => Some(bmethod(recv, "count", list_count)),
            "sort" => Some(bmethod(recv, "sort", list_sort)),
            "reverse" => Some(bmethod(recv, "reverse", list_reverse)),
            "copy" => Some(bmethod(recv, "copy", list_copy)),
            "__len__" => Some(bmethod(recv, "__len__", list_dunder_len)),
            _ => None,
        },
        Obj::Tuple(_) => match name {
            "count" => Some(bmethod(recv, "count", tuple_count)),
            "index" => Some(bmethod(recv, "index", tuple_index)),
            "__len__" => Some(bmethod(recv, "__len__", tuple_dunder_len)),
            _ => None,
        },
        Obj::Dict(_) => match name {
            "get" => Some(bmethod(recv, "get", dict_get)),
            "keys" => Some(bmethod(recv, "keys", dict_keys)),
            "values" => Some(bmethod(recv, "values", dict_values)),
            "items" => Some(bmethod(recv, "items", dict_items)),
            "update" => Some(bmethod(recv, "update", dict_update)),
            "setdefault" => Some(bmethod(recv, "setdefault", dict_setdefault)),
            "pop" => Some(bmethod(recv, "pop", dict_pop)),
            "popitem" => Some(bmethod(recv, "popitem", dict_popitem)),
            "__len__" => Some(bmethod(recv, "__len__", dict_dunder_len)),
            "clear" => Some(bmethod(recv, "clear", dict_clear)),
            "copy" => Some(bmethod(recv, "copy", dict_copy)),
            _ => None,
        },
        Obj::Set(_) => match name {
            "add" => Some(bmethod(recv, "add", set_add)),
            "remove" => Some(bmethod(recv, "remove", set_remove)),
            "discard" => Some(bmethod(recv, "discard", set_discard)),
            "update" => Some(bmethod(recv, "update", set_update)),
            "difference" => Some(bmethod(recv, "difference", set_difference)),
            "intersection" => Some(bmethod(recv, "intersection", set_intersection)),
            "union" => Some(bmethod(recv, "union", set_union)),
            "issubset" => Some(bmethod(recv, "issubset", set_issubset)),
            "issuperset" => Some(bmethod(recv, "issuperset", set_issuperset)),
            "copy" => Some(bmethod(recv, "copy", set_copy)),
            "clear" => Some(bmethod(recv, "clear", set_clear)),
            _ => None,
        },
        Obj::FrozenSet(_) => match name {
            "difference" => Some(bmethod(recv, "difference", set_difference)),
            "intersection" => Some(bmethod(recv, "intersection", set_intersection)),
            "union" => Some(bmethod(recv, "union", set_union)),
            "issubset" => Some(bmethod(recv, "issubset", set_issubset)),
            "issuperset" => Some(bmethod(recv, "issuperset", set_issuperset)),
            "copy" => Some(bmethod(recv, "copy", set_copy)),
            "__len__" => Some(bmethod(recv, "__len__", set_dunder_len)),
            _ => None,
        },
        Obj::Str(_) => match name {
            "upper" => Some(bmethod(recv, "upper", str_upper)),
            "lower" => Some(bmethod(recv, "lower", str_lower)),
            "strip" => Some(bmethod(recv, "strip", str_strip)),
            "lstrip" => Some(bmethod(recv, "lstrip", str_lstrip)),
            "rstrip" => Some(bmethod(recv, "rstrip", str_rstrip)),
            "split" => Some(bmethod(recv, "split", str_split)),
            "rsplit" => Some(bmethod(recv, "rsplit", str_rsplit)),
            "join" => Some(bmethod(recv, "join", str_join)),
            "replace" => Some(bmethod(recv, "replace", str_replace)),
            "count" => Some(bmethod(recv, "count", str_count)),
            "find" => Some(bmethod(recv, "find", str_find)),
            "rfind" => Some(bmethod(recv, "rfind", str_rfind)),
            "startswith" => Some(bmethod(recv, "startswith", str_startswith)),
            "endswith" => Some(bmethod(recv, "endswith", str_endswith)),
            "splitlines" => Some(bmethod(recv, "splitlines", str_splitlines)),
            "isdigit" => Some(bmethod(recv, "isdigit", str_isdigit)),
            "isalpha" => Some(bmethod(recv, "isalpha", str_isalpha)),
            "isalnum" => Some(bmethod(recv, "isalnum", str_isalnum)),
            "title" => Some(bmethod(recv, "title", str_title)),
            "capitalize" => Some(bmethod(recv, "capitalize", str_capitalize)),
            "swapcase" => Some(bmethod(recv, "swapcase", str_swapcase)),
            "zfill" => Some(bmethod(recv, "zfill", str_zfill)),
            "center" => Some(bmethod(recv, "center", str_center)),
            "ljust" => Some(bmethod(recv, "ljust", str_ljust)),
            "rjust" => Some(bmethod(recv, "rjust", str_rjust)),
            "encode" => Some(bmethod(recv, "encode", str_encode)),
            "expandtabs" => Some(bmethod(recv, "expandtabs", str_expandtabs)),
            "partition" => Some(bmethod(recv, "partition", str_partition)),
            "rpartition" => Some(bmethod(recv, "rpartition", str_rpartition)),
            "index" => Some(bmethod(recv, "index", str_index)),
            "rindex" => Some(bmethod(recv, "rindex", str_rindex)),
            "islower" => Some(bmethod(recv, "islower", str_islower)),
            "isupper" => Some(bmethod(recv, "isupper", str_isupper)),
            "isspace" => Some(bmethod(recv, "isspace", str_isspace)),
            "istitle" => Some(bmethod(recv, "istitle", str_istitle)),
            "casefold" => Some(bmethod(recv, "casefold", str_lower)),
            "removeprefix" => Some(bmethod(recv, "removeprefix", str_removeprefix)),
            "removesuffix" => Some(bmethod(recv, "removesuffix", str_removesuffix)),
            "__len__" => Some(bmethod(recv, "__len__", str_dunder_len)),
            _ => None,
        },
        Obj::Bytes(_) => match name {
            "decode" => Some(bmethod(recv, "decode", bytes_decode)),
            "upper" => Some(bmethod(recv, "upper", bytes_upper)),
            "lower" => Some(bmethod(recv, "lower", bytes_lower)),
            "strip" => Some(bmethod(recv, "strip", bytes_strip)),
            "split" => Some(bmethod(recv, "split", bytes_split)),
            "replace" => Some(bmethod(recv, "replace", bytes_replace)),
            "count" => Some(bmethod(recv, "count", bytes_count)),
            "find" => Some(bmethod(recv, "find", bytes_find)),
            "startswith" => Some(bmethod(recv, "startswith", bytes_startswith)),
            "endswith" => Some(bmethod(recv, "endswith", bytes_endswith)),
            "hex" => Some(bmethod(recv, "hex", bytes_hex)),
            "join" => Some(bmethod(recv, "join", bytes_join)),
            _ => None,
        },
        Obj::Int(_) => match name {
            "bit_length" => Some(bmethod(recv, "bit_length", int_bit_length)),
            _ => None,
        },
        Obj::Float(_) => match name {
            "is_integer" => Some(bmethod(recv, "is_integer", float_is_integer)),
            "hex" => Some(bmethod(recv, "hex", float_hex)),
            _ => None,
        },
        Obj::Property { .. } => match name {
            "setter" => Some(bmethod(recv, "setter", prop_setter)),
            "getter" => Some(bmethod(recv, "getter", prop_getter)),
            "deleter" => Some(bmethod(recv, "deleter", prop_deleter)),
            _ => None,
        },
        Obj::Slice { .. } => match name {
            "indices" => Some(bmethod(recv, "indices", slice_indices_method)),
            _ => None,
        },
        Obj::Generator(g) => match (name, g.kind) {
            ("send", _) => Some(bmethod(recv, "send", gen_send_m)),
            ("throw", _) => Some(bmethod(recv, "throw", gen_throw_m)),
            ("close", _) => Some(bmethod(recv, "close", gen_close_m)),
            ("__iter__", _) => Some(recv.clone()),
            ("__next__", GenKind::Generator) => Some(bmethod(recv, "__next__", gen_next_m)),
            // `await coro`: the coroutine is its own awaiter.
            ("__await__", GenKind::Coroutine) => Some(bmethod(recv, "__await__", gen_self_m)),
            ("__aiter__", GenKind::AsyncGen) => Some(bmethod(recv, "__aiter__", gen_self_m)),
            ("__anext__", GenKind::AsyncGen) => Some(bmethod(recv, "__anext__", gen_anext_m)),
            ("asend", GenKind::AsyncGen) => Some(bmethod(recv, "asend", gen_asend_m)),
            ("aclose", GenKind::AsyncGen) => Some(bmethod(recv, "aclose", gen_aclose_m)),
            _ => None,
        },
        Obj::AsendAwaiter { .. } => match name {
            "send" => Some(bmethod(recv, "send", asend_send_m)),
            "throw" => Some(bmethod(recv, "throw", asend_throw_m)),
            "close" => Some(bmethod(recv, "close", asend_close_m)),
            "__await__" => Some(bmethod(recv, "__await__", gen_self_m)),
            _ => None,
        },
        _ => None,
    }
}

// ---- list -----------------------------------------------------------------

fn list_append(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(item) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.append() takes exactly one argument",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        l.borrow_mut().push(item.clone());
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "append() receiver is not a list",
        ))
    }
}

fn list_extend(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(iter) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.extend() takes exactly one argument",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        let items = drain_iter(m, vm, iter)?;
        l.borrow_mut().extend(items);
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "extend() receiver is not a list",
        ))
    }
}

fn list_insert(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(i), Some(x)) = (args.get(1), args.get(2)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.insert() takes exactly two arguments",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        let len = l.borrow().len() as i64;
        let idx = py_index(m, vm, i)?;
        let pos = idx.clamp(0, len) as usize;
        l.borrow_mut().insert(pos, x.clone());
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "insert() receiver is not a list",
        ))
    }
}

fn list_remove(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.remove() takes exactly one argument",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        let len = l.borrow().len();
        for i in 0..len {
            if val_eq(m, vm, &l.borrow()[i], x)? {
                l.borrow_mut().remove(i);
                return Ok(v_none());
            }
        }
        Err(err(
            m,
            ErrorTag::ValueError,
            "list.remove(x): x not in list",
        ))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "remove() receiver is not a list",
        ))
    }
}

fn list_pop(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::List(l) = &*args[0] {
        let len = l.borrow().len();
        if len == 0 {
            return Err(err(m, ErrorTag::IndexError, "pop from empty list"));
        }
        let idx = match args.get(1) {
            Some(i) => py_index(m, vm, i)?,
            None => -1,
        };
        let Some(pos) = norm_index(idx, len) else {
            return Err(err(m, ErrorTag::IndexError, "pop index out of range"));
        };
        Ok(l.borrow_mut().remove(pos))
    } else {
        Err(err(m, ErrorTag::TypeError, "pop() receiver is not a list"))
    }
}

fn list_clear(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::List(l) = &*args[0] {
        l.borrow_mut().clear();
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "clear() receiver is not a list",
        ))
    }
}

fn list_index(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.index() takes at least one argument",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        let l = l.borrow();
        for (i, v) in l.iter().enumerate() {
            if val_eq(m, vm, v, x)? {
                return Ok(v_int(i as i64));
            }
        }
        Err(err(
            m,
            ErrorTag::ValueError,
            &format!("{} is not in list", repr_static(m, x).unwrap_or_default()),
        ))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "index() receiver is not a list",
        ))
    }
}

fn list_count(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "list.count() takes exactly one argument",
        ));
    };
    if let Obj::List(l) = &*args[0] {
        let l = l.borrow();
        let mut n = 0i64;
        for v in l.iter() {
            if val_eq(m, vm, v, x)? {
                n += 1;
            }
        }
        Ok(v_int(n))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "count() receiver is not a list",
        ))
    }
}

fn list_sort(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::List(l) = &*args[0] {
        let items = l.borrow().clone();
        let keyf = kwarg_val(kwargs, "key");
        let reverse = kwarg_bool(kwargs, "reverse");
        *l.borrow_mut() = sort_items(m, vm, items, &keyf, reverse)?;
        Ok(v_none())
    } else {
        Err(err(m, ErrorTag::TypeError, "sort() receiver is not a list"))
    }
}

fn list_reverse(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::List(l) = &*args[0] {
        l.borrow_mut().reverse();
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "reverse() receiver is not a list",
        ))
    }
}

fn list_copy(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::List(l) = &*args[0] {
        Ok(Rc::new(Obj::List(RefCell::new(l.borrow().clone()))))
    } else {
        Err(err(m, ErrorTag::TypeError, "copy() receiver is not a list"))
    }
}

// ---- tuple ----------------------------------------------------------------

fn tuple_count(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "tuple.count() takes exactly one argument",
        ));
    };
    if let Obj::Tuple(t) = &*args[0] {
        let mut n = 0i64;
        for v in t.iter() {
            if val_eq(m, vm, v, x)? {
                n += 1;
            }
        }
        Ok(v_int(n))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "count() receiver is not a tuple",
        ))
    }
}

fn tuple_index(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "tuple.index() takes at least one argument",
        ));
    };
    if let Obj::Tuple(t) = &*args[0] {
        for (i, v) in t.iter().enumerate() {
            if val_eq(m, vm, v, x)? {
                return Ok(v_int(i as i64));
            }
        }
        Err(err(
            m,
            ErrorTag::ValueError,
            "tuple.index(x): x not in tuple",
        ))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "index() receiver is not a tuple",
        ))
    }
}

// ---- dict -----------------------------------------------------------------

fn dict_get(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(key) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "dict.get() takes at least one argument",
        ));
    };
    if let Obj::Dict(d) = &*args[0] {
        let h = py_hash(key).map_err(|t| unhashable(m, &t))?;
        let d = d.borrow();
        Ok(d.get_hash(key, h)
            .or_else(|| args.get(2).cloned())
            .unwrap_or_else(v_none))
    } else {
        Err(err(m, ErrorTag::TypeError, "get() receiver is not a dict"))
    }
}

fn dict_keys(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        let keys = d.borrow().keys();
        Ok(Rc::new(Obj::List(RefCell::new(keys))))
    } else {
        Err(err(m, ErrorTag::TypeError, "keys() receiver is not a dict"))
    }
}

fn dict_values(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        let vals = d.borrow().items().into_iter().map(|(_, v)| v).collect();
        Ok(Rc::new(Obj::List(RefCell::new(vals))))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "values() receiver is not a dict",
        ))
    }
}

fn dict_items(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        let items: Vec<Val> = d
            .borrow()
            .items()
            .into_iter()
            .map(|(k, v)| Rc::new(Obj::Tuple(Rc::new(vec![k, v]))))
            .collect();
        Ok(Rc::new(Obj::List(RefCell::new(items))))
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "items() receiver is not a dict",
        ))
    }
}

fn dict_update(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        if let Some(src) = args.get(1) {
            match &**src {
                Obj::Dict(src_d) => {
                    for (k, v) in src_d.borrow().items() {
                        let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
                        d.borrow_mut().set_hash(k, h, v);
                    }
                }
                Obj::List(items) => {
                    for pair in items.borrow().iter() {
                        let (k, v) = match &**pair {
                            Obj::Tuple(t) if t.len() == 2 => (t[0].clone(), t[1].clone()),
                            _ => {
                                return Err(err(
                                    m,
                                    ErrorTag::ValueError,
                                    "dictionary update sequence element has length 1; 2 is required",
                                ));
                            }
                        };
                        let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
                        d.borrow_mut().set_hash(k, h, v);
                    }
                }
                _ => {
                    let keys = drain_iter(m, vm, &mro_attr(src, "keys").unwrap_or_else(v_none))?;
                    for k in keys {
                        let v = getitem_val(m, vm, src, &k)?;
                        let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
                        d.borrow_mut().set_hash(k, h, v);
                    }
                }
            }
        }
        for (k, v) in kwargs {
            let h = py_hash(&v_str(k)).map_err(|t| unhashable(m, &t))?;
            d.borrow_mut().set_hash(v_str(k), h, v.clone());
        }
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "update() receiver is not a dict",
        ))
    }
}

fn dict_setdefault(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(key) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "dict.setdefault() takes at least one argument",
        ));
    };
    if let Obj::Dict(d) = &*args[0] {
        let h = py_hash(key).map_err(|t| unhashable(m, &t))?;
        let mut d = d.borrow_mut();
        if let Some(v) = d.get_hash(key, h) {
            Ok(v)
        } else {
            let v = args.get(2).cloned().unwrap_or_else(v_none);
            d.set_hash(key.clone(), h, v.clone());
            Ok(v)
        }
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "setdefault() receiver is not a dict",
        ))
    }
}

fn dict_pop(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(key) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "dict.pop() takes at least one argument",
        ));
    };
    if let Obj::Dict(d) = &*args[0] {
        let h = py_hash(key).map_err(|t| unhashable(m, &t))?;
        let mut d = d.borrow_mut();
        match d.get_hash(key, h) {
            Some(v) => {
                d.del_hash(key, h);
                Ok(v)
            }
            None => match args.get(2) {
                Some(dflt) => Ok(dflt.clone()),
                None => Err(key_error(m, key)),
            },
        }
    } else {
        Err(err(m, ErrorTag::TypeError, "pop() receiver is not a dict"))
    }
}

fn dict_popitem(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        let mut d = d.borrow_mut();
        let items = d.items();
        match items.last() {
            Some((k, v)) => {
                let h = py_hash(k).map_err(|t| unhashable(m, &t))?;
                d.del_hash(k, h);
                Ok(Rc::new(Obj::Tuple(Rc::new(vec![k.clone(), v.clone()]))))
            }
            None => Err(err(m, ErrorTag::KeyError, "popitem(): dictionary is empty")),
        }
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "popitem() receiver is not a dict",
        ))
    }
}

fn dict_clear(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        *d.borrow_mut() = Dict::default();
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "clear() receiver is not a dict",
        ))
    }
}

fn dict_copy(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Dict(d) = &*args[0] {
        let mut c = Dict::default();
        for (k, v) in d.borrow().items() {
            let h = py_hash(&k).map_err(|t| unhashable(m, &t))?;
            c.set_hash(k, h, v);
        }
        Ok(Rc::new(Obj::Dict(RefCell::new(c))))
    } else {
        Err(err(m, ErrorTag::TypeError, "copy() receiver is not a dict"))
    }
}

// ---- set ------------------------------------------------------------------

fn set_add(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.add() takes exactly one argument",
        ));
    };
    if let Obj::Set(s) = &*args[0] {
        let h = py_hash(x).map_err(|t| unhashable(m, &t))?;
        s.borrow_mut().add(x.clone(), h, &|o| eq_novm(o, x));
        Ok(v_none())
    } else {
        Err(err(m, ErrorTag::TypeError, "add() receiver is not a set"))
    }
}

fn set_remove(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.remove() takes exactly one argument",
        ));
    };
    if let Obj::Set(s) = &*args[0] {
        let h = py_hash(x).map_err(|t| unhashable(m, &t))?;
        if s.borrow_mut().remove(x, h, &|o| eq_novm(o, x)) {
            Ok(v_none())
        } else {
            Err(key_error(m, x))
        }
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "remove() receiver is not a set",
        ))
    }
}

fn set_discard(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(x) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.discard() takes exactly one argument",
        ));
    };
    if let Obj::Set(s) = &*args[0] {
        let h = py_hash(x).map_err(|t| unhashable(m, &t))?;
        s.borrow_mut().remove(x, h, &|o| eq_novm(o, x));
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "discard() receiver is not a set",
        ))
    }
}

fn set_update(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(iter) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.update() takes exactly one argument",
        ));
    };
    if let Obj::Set(s) = &*args[0] {
        for x in drain_iter(m, vm, iter)? {
            let h = py_hash(&x).map_err(|t| unhashable(m, &t))?;
            let eq = x.clone();
            s.borrow_mut().add(x, h, &|o| eq_novm(o, &eq));
        }
        Ok(v_none())
    } else {
        Err(err(
            m,
            ErrorTag::TypeError,
            "update() receiver is not a set",
        ))
    }
}

fn set_difference(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(other) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.difference() takes at least one argument",
        ));
    };
    let (recv, is_frozen) = match &*args[0] {
        Obj::Set(_) => (args[0].clone(), false),
        Obj::FrozenSet(_) => (args[0].clone(), true),
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "difference() receiver is not a set",
            ));
        }
    };
    let base = set_values(&recv)?;
    let other_vals = set_values(other)?;
    let out: Vec<Val> = base
        .into_iter()
        .filter(|v| !other_vals.iter().any(|o| eq_novm(v, o)))
        .collect();
    let t = set_from_items(m, out)?;
    Ok(if is_frozen {
        Rc::new(Obj::FrozenSet(RefCell::new(t)))
    } else {
        Rc::new(Obj::Set(RefCell::new(t)))
    })
}

fn set_intersection(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(other) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.intersection() takes at least one argument",
        ));
    };
    let (recv, is_frozen) = match &*args[0] {
        Obj::Set(_) => (args[0].clone(), false),
        Obj::FrozenSet(_) => (args[0].clone(), true),
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "intersection() receiver is not a set",
            ));
        }
    };
    let base = set_values(&recv)?;
    let other_vals = set_values(other)?;
    let out: Vec<Val> = base
        .into_iter()
        .filter(|v| other_vals.iter().any(|o| eq_novm(v, o)))
        .collect();
    let t = set_from_items(m, out)?;
    Ok(if is_frozen {
        Rc::new(Obj::FrozenSet(RefCell::new(t)))
    } else {
        Rc::new(Obj::Set(RefCell::new(t)))
    })
}

fn set_union(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(other) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.union() takes at least one argument",
        ));
    };
    let (recv, is_frozen) = match &*args[0] {
        Obj::Set(_) => (args[0].clone(), false),
        Obj::FrozenSet(_) => (args[0].clone(), true),
        _ => return Err(err(m, ErrorTag::TypeError, "union() receiver is not a set")),
    };
    let mut out = set_values(&recv)?;
    for v in set_values(other)? {
        if !out.iter().any(|o| eq_novm(o, &v)) {
            out.push(v);
        }
    }
    let t = set_from_items(m, out)?;
    Ok(if is_frozen {
        Rc::new(Obj::FrozenSet(RefCell::new(t)))
    } else {
        Rc::new(Obj::Set(RefCell::new(t)))
    })
}

fn set_issubset(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(other) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.issubset() takes exactly one argument",
        ));
    };
    let base = set_values(&args[0])?;
    let other_vals = set_values(other)?;
    let r = base
        .iter()
        .all(|v| other_vals.iter().any(|o| eq_novm(v, o)));
    Ok(v_bool(r))
}

fn set_issuperset(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(other) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "set.issuperset() takes exactly one argument",
        ));
    };
    let base = set_values(&args[0])?;
    let other_vals = set_values(other)?;
    let r = other_vals
        .iter()
        .all(|v| base.iter().any(|o| eq_novm(v, o)));
    Ok(v_bool(r))
}

fn set_copy(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &*args[0] {
        Obj::Set(s) => {
            let t = set_from_items(m, s.borrow().values())?;
            Ok(Rc::new(Obj::Set(RefCell::new(t))))
        }
        Obj::FrozenSet(s) => {
            let t = set_from_items(m, s.borrow().values())?;
            Ok(Rc::new(Obj::FrozenSet(RefCell::new(t))))
        }
        _ => Err(err(m, ErrorTag::TypeError, "copy() receiver is not a set")),
    }
}

fn set_clear(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Obj::Set(s) = &*args[0] {
        *s.borrow_mut() = SetTable::new();
        Ok(v_none())
    } else {
        Err(err(m, ErrorTag::TypeError, "clear() receiver is not a set"))
    }
}

fn set_values(v: &Val) -> Result<Vec<Val>, Val> {
    match &**v {
        Obj::Set(s) => Ok(s.borrow().values()),
        Obj::FrozenSet(s) => Ok(s.borrow().values()),
        _ => Ok(drain_iter_no_vm(v)),
    }
}

fn drain_iter_no_vm(v: &Val) -> Vec<Val> {
    match &**v {
        Obj::List(l) => l.borrow().clone(),
        Obj::Tuple(t) => t.iter().cloned().collect(),
        Obj::Str(s) => s.chars().map(|c| v_str(&c.to_string())).collect(),
        Obj::Range { start, stop, step } => {
            let mut out = Vec::new();
            let mut i = *start;
            if *step > 0 {
                while i < *stop {
                    out.push(v_int(i as i64));
                    i += *step;
                }
            } else {
                while i > *stop {
                    out.push(v_int(i as i64));
                    i += *step;
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

// ---- str ------------------------------------------------------------------

fn str_of(v: &Val) -> Option<&str> {
    match &**v {
        Obj::Str(s) => Some(s.as_ref()),
        _ => None,
    }
}

fn str_upper(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_str(&s.to_uppercase()))
}

fn str_lower(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_str(&s.to_lowercase()))
}

fn str_strip(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let chars: Option<Vec<char>> = args.get(1).and_then(str_of).map(|c| c.chars().collect());
    let out = match chars {
        Some(c) => s.trim_matches(|ch| c.contains(&ch)),
        None => s.trim(),
    };
    Ok(v_str(out))
}

fn str_lstrip(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let chars: Option<Vec<char>> = args.get(1).and_then(str_of).map(|c| c.chars().collect());
    let out = match chars {
        Some(c) => s.trim_start_matches(|ch| c.contains(&ch)),
        None => s.trim_start(),
    };
    Ok(v_str(out))
}

fn str_rstrip(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let chars: Option<Vec<char>> = args.get(1).and_then(str_of).map(|c| c.chars().collect());
    let out = match chars {
        Some(c) => s.trim_end_matches(|ch| c.contains(&ch)),
        None => s.trim_end(),
    };
    Ok(v_str(out))
}

fn py_split(s: &str, sep: Option<&str>, mut maxsplit: i64) -> Vec<String> {
    match sep {
        None => {
            let words: Vec<&str> = s.split_whitespace().collect();
            if maxsplit >= 0 {
                let n = (maxsplit as usize) + 1;
                if words.len() <= n {
                    words.into_iter().map(|w| w.to_string()).collect()
                } else {
                    let mut out: Vec<String> = words[..n].iter().map(|w| w.to_string()).collect();
                    out.push(words[n..].join(" "));
                    out
                }
            } else {
                words.into_iter().map(|w| w.to_string()).collect()
            }
        }
        Some(sep) => {
            let mut out: Vec<String> = Vec::new();
            let mut rest = s;
            loop {
                if maxsplit == 0 {
                    out.push(rest.to_string());
                    break;
                }
                match rest.find(sep) {
                    Some(pos) => {
                        out.push(rest[..pos].to_string());
                        rest = &rest[pos + sep.len()..];
                        if maxsplit > 0 {
                            maxsplit -= 1;
                        }
                    }
                    None => {
                        out.push(rest.to_string());
                        break;
                    }
                }
            }
            out
        }
    }
}

fn str_split(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let sep = args.get(1).and_then(str_of);
    let maxsplit = args
        .get(2)
        .cloned()
        .or_else(|| kwarg_val(kwargs, "maxsplit"))
        .and_then(|v| to_big(&v))
        .map(|b| b.to_i64().unwrap_or(-1))
        .unwrap_or(-1);
    let parts = py_split(s, sep, maxsplit);
    Ok(Rc::new(Obj::List(RefCell::new(
        parts.into_iter().map(|p| v_str(&p)).collect(),
    ))))
}

fn py_rsplit(s: &str, sep: Option<&str>, maxsplit: i64) -> Vec<String> {
    match sep {
        Some(sep) => {
            let full: Vec<&str> = s.split(sep).collect();
            let n = maxsplit as usize;
            if maxsplit < 0 || n >= full.len().saturating_sub(1) {
                full.into_iter().map(|p| p.to_string()).collect()
            } else {
                let fold = full.len() - 1 - n;
                let mut head = full[0].to_string();
                for part in full.iter().take(fold + 1).skip(1) {
                    head.push_str(sep);
                    head.push_str(part);
                }
                let mut out = vec![head];
                out.extend(full[fold + 1..].iter().map(|p| p.to_string()));
                out
            }
        }
        None => {
            let bytes = s.as_bytes();
            let mut spans: Vec<(usize, usize)> = Vec::new();
            let mut i = 0usize;
            while i < bytes.len() {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i > start {
                    spans.push((start, i));
                }
            }
            let n = maxsplit as usize;
            if maxsplit < 0 || n >= spans.len().saturating_sub(1) {
                spans.iter().map(|&(a, b)| s[a..b].to_string()).collect()
            } else {
                let fold_end = spans[spans.len() - 1 - n].1;
                let mut out = vec![s[..fold_end].to_string()];
                out.extend(
                    spans[spans.len() - n..]
                        .iter()
                        .map(|&(a, b)| s[a..b].to_string()),
                );
                out
            }
        }
    }
}

fn str_rsplit(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let sep = args.get(1).and_then(str_of);
    let maxsplit = args
        .get(2)
        .cloned()
        .or_else(|| kwarg_val(kwargs, "maxsplit"))
        .and_then(|v| to_big(&v))
        .map(|b| b.to_i64().unwrap_or(-1))
        .unwrap_or(-1);
    let parts = py_rsplit(s, sep, maxsplit);
    Ok(Rc::new(Obj::List(RefCell::new(
        parts.into_iter().map(|p| v_str(&p)).collect(),
    ))))
}

fn str_join(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let sep = str_of(&args[0]).unwrap_or("");
    let Some(items) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.join() takes exactly one argument",
        ));
    };
    let drained = drain_iter(m, vm, items)?;
    let mut out = String::new();
    for (i, it) in drained.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        match &**it {
            Obj::Str(s) => out.push_str(s),
            _ => {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "sequence item {i}: expected str instance, {} found",
                        val_type_name(m, it)
                    ),
                ));
            }
        }
    }
    Ok(v_str(&out))
}

fn str_replace(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let (Some(old), Some(new)) = (args.get(1).and_then(str_of), args.get(2).and_then(str_of))
    else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.replace() takes at least two arguments",
        ));
    };
    let count = args
        .get(3)
        .and_then(to_big)
        .map(|b| b.to_i64().unwrap_or(-1))
        .unwrap_or(-1);
    let out = if count >= 0 {
        s.replacen(old, new, count as usize)
    } else {
        s.replace(old, new)
    };
    Ok(v_str(&out))
}

fn str_count(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(sub) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.count() takes at least one argument",
        ));
    };
    let (start, end) = str_span(m, s, args, 2)?;
    let n = s[start..end].matches(sub).count();
    Ok(v_int(n as i64))
}

fn str_find(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(sub) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.find() takes at least one argument",
        ));
    };
    Ok(v_int(match s.find(sub) {
        Some(p) => p as i64,
        None => -1,
    }))
}

fn str_rfind(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(sub) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.rfind() takes at least one argument",
        ));
    };
    Ok(v_int(match s.rfind(sub) {
        Some(p) => p as i64,
        None => -1,
    }))
}

fn str_startswith(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(p) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.startswith() takes at least one argument",
        ));
    };
    let (start, end) = str_span(m, s, args, 2)?;
    Ok(v_bool(s[start..end].starts_with(p)))
}

fn str_endswith(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(p) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.endswith() takes at least one argument",
        ));
    };
    let (start, end) = str_span(m, s, args, 2)?;
    Ok(v_bool(s[start..end].ends_with(p)))
}

/// `sl.indices(n)`: the (start, stop, step) triple a slice denotes for a
/// length-`n` sequence.
fn slice_indices_method(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(n) = args.get(1).and_then(to_big).and_then(|b| b.to_i64()) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "slice.indices() takes one argument",
        ));
    };
    if n < 0 {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "length should not be negative",
        ));
    }
    let (start, stop, step) = slice_indices(m, vm, &args[0], n)?;
    Ok(Rc::new(Obj::Tuple(Rc::new(vec![
        v_int(start),
        v_int(stop),
        v_int(step),
    ]))))
}

/// The optional `start`/`end` operand pair shared by several string
/// methods, as byte indices into `s`.
fn str_span(m: &TestModel, s: &str, args: &[Val], at: usize) -> Result<(usize, usize), Val> {
    let n = s.len();
    let clamp = |v: &Val| -> Option<i64> {
        match &**v {
            Obj::Int(i) => i.to_i64(),
            Obj::Bool(b) => Some(*b as i64),
            _ => None,
        }
    };
    let mut start = 0usize;
    let mut end = n;
    if let Some(a) = args.get(at).and_then(clamp) {
        start = if a < 0 {
            n.saturating_sub(a.unsigned_abs() as usize)
        } else {
            (a as usize).min(n)
        };
    }
    if let Some(b) = args.get(at + 1).and_then(clamp) {
        end = if b < 0 {
            n.saturating_sub(b.unsigned_abs() as usize)
        } else {
            (b as usize).min(n)
        };
    }
    if start > end {
        start = end;
    }
    let _ = m;
    Ok((start, end))
}

fn str_splitlines(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let mut parts: Vec<Val> = s
        .split('\n')
        .map(|l| v_str(l.trim_end_matches('\r')))
        .collect();
    // A trailing boundary does not open a new line.
    if s.ends_with('\n') || s.ends_with('\r') {
        parts.pop();
    }
    Ok(Rc::new(Obj::List(RefCell::new(parts))))
}

fn str_isdigit(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()),
    ))
}

fn str_isalpha(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        !s.is_empty() && s.chars().all(|c| c.is_alphabetic()),
    ))
}

fn str_isalnum(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        !s.is_empty() && s.chars().all(|c| c.is_alphanumeric()),
    ))
}

fn str_index(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = str_find(m, vm, args, &[])?;
    match &*v {
        Obj::Int(i) if i.to_i64() == Some(-1) => {
            Err(err(m, ErrorTag::ValueError, "substring not found"))
        }
        _ => Ok(v),
    }
}

fn str_rindex(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = str_rfind(m, vm, args, &[])?;
    match &*v {
        Obj::Int(i) if i.to_i64() == Some(-1) => {
            Err(err(m, ErrorTag::ValueError, "substring not found"))
        }
        _ => Ok(v),
    }
}

fn str_islower(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        s.chars().any(|c| c.is_lowercase()) && !s.chars().any(|c| c.is_uppercase()),
    ))
}

fn str_isupper(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        s.chars().any(|c| c.is_uppercase()) && !s.chars().any(|c| c.is_lowercase()),
    ))
}

fn str_isspace(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(v_bool(
        !s.is_empty() && s.chars().all(|c| c.is_whitespace()),
    ))
}

fn str_istitle(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    // an upper-case letter after any non-cased character, no lower-case
    // letter following a cased one
    let s = str_of(&args[0]).unwrap_or("");
    let mut cased = false;
    let mut valid = !s.is_empty();
    let mut prev_cased = false;
    for c in s.chars() {
        if c.is_uppercase() {
            if prev_cased {
                valid = false;
            }
            cased = true;
            prev_cased = true;
        } else if c.is_lowercase() {
            if !prev_cased {
                valid = false;
            }
            cased = true;
            prev_cased = true;
        } else {
            prev_cased = false;
        }
    }
    Ok(v_bool(valid && cased))
}

fn str_removeprefix(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let p = args.get(1).and_then(str_of).unwrap_or("");
    Ok(v_str(s.strip_prefix(p).unwrap_or(s)))
}

fn str_removesuffix(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let p = args.get(1).and_then(str_of).unwrap_or("");
    Ok(v_str(s.strip_suffix(p).unwrap_or(s)))
}

fn str_title(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    // a cased character starts a word: uppercase it, lowercase the rest,
    // leave separators in place
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if c.is_lowercase() || c.is_uppercase() {
            if prev_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_cased = true;
        } else {
            out.push(c);
            prev_cased = false;
        }
    }
    Ok(v_str(&out))
}

fn str_capitalize(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let mut ch = s.chars();
    let first = ch
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let rest = ch.as_str().to_lowercase();
    Ok(v_str(&format!("{first}{rest}")))
}

fn str_swapcase(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_uppercase() {
                c.to_lowercase().collect()
            } else if c.is_lowercase() {
                c.to_uppercase().collect()
            } else {
                c.to_string()
            }
        })
        .collect();
    Ok(v_str(&out))
}

fn str_zfill(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(w) = args.get(1).and_then(to_big) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.zfill() takes exactly one argument",
        ));
    };
    let w = w.to_i64().unwrap_or(0) as usize;
    if s.len() >= w {
        return Ok(v_str(s));
    }
    let pad = w - s.len();
    let (sign, body) = match s.chars().next() {
        Some('+') => ("+", &s[1..]),
        Some('-') => ("-", &s[1..]),
        _ => ("", s),
    };
    Ok(v_str(&format!("{sign}{:0>width$}{body}", "", width = pad)))
}

/// The fill character argument shared by ljust/rjust/center: a single
/// character string, defaulting to a space.
fn fill_char(m: &TestModel, args: &[Val]) -> Result<char, Val> {
    match args.get(2) {
        None => Ok(' '),
        Some(v) => {
            let Some(s) = str_of(v) else {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    "The fill character must be exactly one character long",
                ));
            };
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(c),
                _ => Err(err(
                    m,
                    ErrorTag::TypeError,
                    "The fill character must be exactly one character long",
                )),
            }
        }
    }
}

fn str_center(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(w) = args.get(1).and_then(to_big) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.center() takes at least one argument",
        ));
    };
    let fill = fill_char(m, args)?;
    let w = w.to_i64().unwrap_or(0) as usize;
    let n = s.chars().count();
    if n >= w {
        return Ok(v_str(s));
    }
    let total = w - n;
    let left = total / 2;
    let right = total - left;
    let mut out = String::new();
    out.extend(std::iter::repeat_n(fill, left));
    out.push_str(s);
    out.extend(std::iter::repeat_n(fill, right));
    Ok(v_str(&out))
}

fn str_ljust(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(w) = args.get(1).and_then(to_big) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.ljust() takes at least one argument",
        ));
    };
    let fill = fill_char(m, args)?;
    let w = w.to_i64().unwrap_or(0) as usize;
    let n = s.chars().count();
    if n >= w {
        return Ok(v_str(s));
    }
    let mut out = String::from(s);
    out.extend(std::iter::repeat_n(fill, w - n));
    Ok(v_str(&out))
}

fn str_rjust(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(w) = args.get(1).and_then(to_big) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.rjust() takes at least one argument",
        ));
    };
    let fill = fill_char(m, args)?;
    let w = w.to_i64().unwrap_or(0) as usize;
    let n = s.chars().count();
    if n >= w {
        return Ok(v_str(s));
    }
    let mut out: String = std::iter::repeat_n(fill, w - n).collect();
    out.push_str(s);
    Ok(v_str(&out))
}

fn str_encode(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    Ok(Rc::new(Obj::Bytes(Rc::from(s.as_bytes()))))
}

fn str_expandtabs(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let tab = args
        .get(1)
        .and_then(to_big)
        .map(|b| b.to_i64().unwrap_or(8))
        .unwrap_or(8) as usize;
    let mut out = String::new();
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let next = (col / tab + 1) * tab;
            out.push_str(&" ".repeat(next - col));
            col = next;
        } else {
            out.push(c);
            col += 1;
        }
    }
    Ok(v_str(&out))
}

fn str_partition(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(sep) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.partition() takes exactly one argument",
        ));
    };
    match s.find(sep) {
        Some(p) => Ok(Rc::new(Obj::Tuple(Rc::new(vec![
            v_str(&s[..p]),
            v_str(sep),
            v_str(&s[p + sep.len()..]),
        ])))),
        None => Ok(Rc::new(Obj::Tuple(Rc::new(vec![
            v_str(s),
            v_str(""),
            v_str(""),
        ])))),
    }
}

fn str_rpartition(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let Some(sep) = args.get(1).and_then(str_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "str.rpartition() takes exactly one argument",
        ));
    };
    match s.rfind(sep) {
        Some(p) => Ok(Rc::new(Obj::Tuple(Rc::new(vec![
            v_str(&s[..p]),
            v_str(sep),
            v_str(&s[p + sep.len()..]),
        ])))),
        None => Ok(Rc::new(Obj::Tuple(Rc::new(vec![
            v_str(""),
            v_str(""),
            v_str(s),
        ])))),
    }
}

// ---- bytes ----------------------------------------------------------------

fn bytes_of(v: &Val) -> Option<&[u8]> {
    match &**v {
        Obj::Bytes(b) => Some(b.as_ref()),
        _ => None,
    }
}

fn bytes_decode(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    match std::str::from_utf8(b) {
        Ok(s) => Ok(v_str(s)),
        Err(_) => Err(err(m, ErrorTag::ValueError, "invalid utf-8")),
    }
}

fn bytes_upper(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let out: Vec<u8> = b.iter().map(|c| c.to_ascii_uppercase()).collect();
    Ok(Rc::new(Obj::Bytes(Rc::from(out))))
}

fn bytes_lower(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let out: Vec<u8> = b.iter().map(|c| c.to_ascii_lowercase()).collect();
    Ok(Rc::new(Obj::Bytes(Rc::from(out))))
}

fn bytes_strip(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let out = b.trim_ascii();
    Ok(Rc::new(Obj::Bytes(Rc::from(out.to_vec()))))
}

fn bytes_split(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let sep = args.get(1).and_then(bytes_of);
    let parts: Vec<Val> = match sep {
        Some(s) if !s.is_empty() => {
            let mut out = Vec::new();
            let mut start = 0usize;
            while let Some(pos) = b[start..].windows(s.len()).position(|w| w == s) {
                out.push(Rc::new(Obj::Bytes(Rc::from(
                    b[start..start + pos].to_vec(),
                ))));
                start += pos + s.len();
            }
            out.push(Rc::new(Obj::Bytes(Rc::from(b[start..].to_vec()))));
            out
        }
        _ => b
            .split(|c| c.is_ascii_whitespace())
            .filter(|p| !p.is_empty())
            .map(|p| Rc::new(Obj::Bytes(Rc::from(p.to_vec()))))
            .collect(),
    };
    Ok(Rc::new(Obj::List(RefCell::new(parts))))
}

fn bytes_replace(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let (Some(old), Some(new)) = (
        args.get(1).and_then(bytes_of),
        args.get(2).and_then(bytes_of),
    ) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.replace() takes at least two arguments",
        ));
    };
    let count = args
        .get(3)
        .and_then(to_big)
        .map(|b| b.to_i64().unwrap_or(-1))
        .unwrap_or(-1);
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut rest = b;
    let mut replaced = 0i64;
    while let Some(pos) = rest.windows(old.len()).position(|w| w == old) {
        if count >= 0 && replaced >= count {
            break;
        }
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(new);
        rest = &rest[pos + old.len()..];
        replaced += 1;
    }
    out.extend_from_slice(rest);
    Ok(Rc::new(Obj::Bytes(Rc::from(out))))
}

fn bytes_count(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let Some(sub) = args.get(1).and_then(bytes_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.count() takes at least one argument",
        ));
    };
    let n = b.windows(sub.len()).filter(|w| *w == sub).count();
    Ok(v_int(n as i64))
}

fn bytes_startswith(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let Some(p) = args.get(1).and_then(bytes_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.startswith() takes at least one argument",
        ));
    };
    Ok(v_bool(b.starts_with(p)))
}

fn bytes_endswith(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let Some(p) = args.get(1).and_then(bytes_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.endswith() takes at least one argument",
        ));
    };
    Ok(v_bool(b.ends_with(p)))
}

fn bytes_find(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let Some(sub) = args.get(1).and_then(bytes_of) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.find() takes at least one argument",
        ));
    };
    Ok(v_int(match b.windows(sub.len()).position(|w| w == sub) {
        Some(p) => p as i64,
        None => -1,
    }))
}

fn bytes_hex(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let b = bytes_of(&args[0]).unwrap_or(&[]);
    let out: String = b.iter().map(|c| format!("{c:02x}")).collect();
    Ok(v_str(&out))
}

fn bytes_join(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let sep = bytes_of(&args[0]).unwrap_or(&[]);
    let Some(items) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "bytes.join() takes exactly one argument",
        ));
    };
    let drained = drain_iter(m, vm, items)?;
    let mut out = Vec::new();
    for (i, it) in drained.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        match &**it {
            Obj::Bytes(b) => out.extend_from_slice(b),
            _ => {
                return Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "sequence item {i}: expected bytes instance, {} found",
                        val_type_name(m, it)
                    ),
                ));
            }
        }
    }
    Ok(Rc::new(Obj::Bytes(Rc::from(out))))
}

// ---- int / float ----------------------------------------------------------

fn int_bit_length(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let i = to_big(&args[0]).unwrap_or_else(|| Rc::new(BigInt::zero()));
    let bits = match i.mag.last() {
        None => 0,
        // bits used by the top limb, plus every full limb below it
        Some(top) => (i.mag.len() - 1) as i64 * 32 + (32 - top.leading_zeros() as i64),
    };
    Ok(v_int(bits))
}

fn float_is_integer(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let f = match &*args[0] {
        Obj::Float(f) => *f,
        _ => 0.0,
    };
    Ok(v_bool(f.is_finite() && f.fract() == 0.0))
}

fn float_hex(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let f = match &*args[0] {
        Obj::Float(f) => *f,
        _ => 0.0,
    };
    if !f.is_finite() {
        return Err(err(
            _m,
            ErrorTag::OverflowError,
            "cannot convert float to hexadecimal",
        ));
    }
    Ok(v_str(&float_hex_str(f)))
}

/// `float.hex()`'s format: `[-]0x1.<13 hex digits>p<+/-exp>`, always 13
/// mantissa digits.
fn float_hex_str(f: f64) -> String {
    if f == 0.0 {
        return format!(
            "0x0.0000000000000p{}0",
            if f.is_sign_negative() { '-' } else { '+' }
        );
    }
    let neg = f.is_sign_negative();
    let bits = f.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & 0xf_ffff_ffff_ffff;
    let (fraction, e) = if exp == 0 {
        (frac << 1, -1022i64)
    } else {
        (frac, exp - 1023)
    };
    let hex = format!("{fraction:013x}");
    format!("{}0x1.{hex}p{e:+}", if neg { "-" } else { "" })
}

fn str_dunder_len(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let s = str_of(&args[0]).unwrap_or("");
    let _ = m;
    Ok(v_int(s.chars().count() as i64))
}

fn list_dunder_len(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &*args[0] {
        Obj::List(l) => Ok(v_int(l.borrow().len() as i64)),
        _ => Ok(v_int(0)),
    }
}

fn tuple_dunder_len(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &*args[0] {
        Obj::Tuple(t) => Ok(v_int(t.len() as i64)),
        _ => Ok(v_int(0)),
    }
}

fn dict_dunder_len(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &*args[0] {
        Obj::Dict(d) => Ok(v_int(d.borrow().len() as i64)),
        _ => Ok(v_int(0)),
    }
}

fn set_dunder_len(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match &*args[0] {
        Obj::Set(t) | Obj::FrozenSet(t) => Ok(v_int(t.borrow().values().len() as i64)),
        _ => Ok(v_int(0)),
    }
}

// ---- generator ------------------------------------------------------------

fn gen_self_m(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(args.first().cloned().unwrap_or_else(v_none))
}

fn gen_next_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let none = v_none();
    let recv = args.first().cloned().unwrap_or_else(v_none);
    gen_send_m(m, vm, &[recv, none], &[])
}

/// A finished awaitable: driving it raises `StopIteration(value)`, which is
/// exactly what a `YieldFrom` over it resolves to.
fn finished_awaiter(value: Val) -> Val {
    Rc::new(Obj::Generator(Rc::new(GenState {
        frame: RefCell::new(None),
        running: Cell::new(false),
        started: Cell::new(true),
        kind: GenKind::Coroutine,
        ret: RefCell::new(Some(value)),
    })))
}

/// `agen.__anext__()` / `agen.asend(v)`: the awaiter is lazy; the
/// interpreter's `await` machinery drives it, forwarding yields outward.
fn gen_anext_m(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(Rc::new(Obj::AsendAwaiter {
        agen: args[0].clone(),
        pending: Cell::new(None),
    }))
}

fn gen_asend_m(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(Rc::new(Obj::AsendAwaiter {
        agen: args[0].clone(),
        pending: Cell::new(Some(args.get(1).cloned().unwrap_or_else(v_none))),
    }))
}

fn gen_aclose_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    gen_close_m(m, vm, args, &[])?;
    Ok(finished_awaiter(v_none()))
}

fn asend_agen(args: &[Val]) -> Option<(Val, Option<Val>)> {
    match args.first().map(|v| &**v) {
        Some(Obj::AsendAwaiter { agen, pending }) => Some((agen.clone(), pending.take())),
        _ => None,
    }
}

/// Drive an async generator on behalf of an `asend()`/`__anext__()`
/// awaiter. A yield the generator forwards from its own `await` passes
/// outward; the generator's own `yield` is the awaiter's return value.
fn asend_step(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    g: &Rc<GenState>,
    v: &Val,
) -> Result<Val, Val> {
    match gen_send(m, vm, g, v) {
        Ok(y) => {
            let forwarded = g.frame.borrow().as_ref().is_some_and(|f| f.is_delegating());
            if forwarded {
                Ok(y)
            } else {
                Err(stop_iteration_value(m, y))
            }
        }
        Err(e) => Err(e),
    }
}

fn asend_send_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some((agen, pending)) = asend_agen(args) else {
        return Err(err(m, ErrorTag::TypeError, "send() on a bad awaiter"));
    };
    let v = pending
        .or_else(|| args.get(1).cloned())
        .unwrap_or_else(v_none);
    let Obj::Generator(g) = &*agen else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "asend() awaiter without an async generator",
        ));
    };
    asend_step(m, vm, g, &v)
}

fn asend_throw_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some((agen, _)) = asend_agen(args) else {
        return Err(err(m, ErrorTag::TypeError, "throw() on a bad awaiter"));
    };
    let exc = args
        .get(1)
        .cloned()
        .ok_or_else(|| err(m, ErrorTag::TypeError, "throw() requires an exception"))?;
    let Obj::Generator(g) = &*agen else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "asend() awaiter without an async generator",
        ));
    };
    let out = gen_throw(m, vm, g, &exc)?;
    Ok(out)
}

fn asend_close_m(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    _args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(v_none())
}

fn gen_send_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(v) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "generator.send() takes exactly one argument",
        ));
    };
    let Obj::Generator(g) = &*args[0] else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "send() receiver is not a generator",
        ));
    };
    gen_send(m, vm, g, v)
}

fn gen_throw_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(e) = args.get(1) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "generator.throw() takes at least one argument",
        ));
    };
    let Obj::Generator(g) = &*args[0] else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "throw() receiver is not a generator",
        ));
    };
    gen_throw(m, vm, g, e)
}

fn gen_close_m(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Obj::Generator(g) = &*args[0] else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "close() receiver is not a generator",
        ));
    };
    gen_close(m, vm, g)
}

// ---------------------------------------------------------------------------
// Module-level builtins
// ---------------------------------------------------------------------------

fn need<'a>(m: &TestModel, name: &str, args: &'a [Val]) -> Result<&'a Val, Val> {
    args.first().ok_or_else(|| {
        err(
            m,
            ErrorTag::TypeError,
            &format!("{name}() takes exactly one argument ({} given)", args.len()),
        )
    })
}

fn tuple_items_of(v: &Val) -> Option<Vec<Val>> {
    match &**v {
        Obj::Tuple(t) => Some(t.iter().cloned().collect()),
        _ => None,
    }
}

fn opt_str_kwarg(
    m: &TestModel,
    kwargs: &[(&str, Val)],
    name: &str,
    default: &str,
) -> Result<String, Val> {
    match kwarg_val(kwargs, name) {
        None => Ok(default.to_string()),
        Some(v) => match &*v {
            Obj::None_ => Ok(default.to_string()),
            Obj::Str(s) => Ok(s.to_string()),
            _ => Err(err(
                m,
                ErrorTag::TypeError,
                &format!("{name} must be None or str, not {}", val_type_name(m, &v)),
            )),
        },
    }
}

fn bi_print(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Some(f) = kwarg_val(kwargs, "file")
        && !matches!(&*f, Obj::None_)
    {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "print() file argument is not supported by the test model",
        ));
    }
    let sep = opt_str_kwarg(m, kwargs, "sep", " ")?;
    let end = opt_str_kwarg(m, kwargs, "end", "\n")?;
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(&sep);
        }
        out.push_str(&str_val(m, vm, a)?);
    }
    out.push_str(&end);
    if m.captured.borrow().is_some() {
        m.captured.borrow_mut().as_mut().unwrap().push_str(&out);
        return Ok(v_none());
    }
    let mut stdout = std::io::stdout().lock();
    let written = stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush());
    if let Err(e) = written {
        return Err(err(
            m,
            ErrorTag::RuntimeError,
            &format!("print failed: {e}"),
        ));
    }
    Ok(v_none())
}

fn bi_len(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(v_int(len_val(m, vm, need(m, "len", args)?)?))
}

fn bi_isinstance(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(v), Some(cls)) = (args.first(), args.get(1)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "isinstance expected 2 arguments",
        ));
    };
    let ok = match &**cls {
        Obj::Class(_) | Obj::Tuple(_) => isinstance_val(m, v, cls),
        _ => {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "isinstance() arg 2 must be a type, a tuple of types, or a union, not {}",
                    val_type_name(m, cls)
                ),
            ));
        }
    };
    Ok(v_bool(ok))
}

fn bi_issubclass(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(cls), Some(of)) = (args.first(), args.get(1)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "issubclass expected 2 arguments",
        ));
    };
    if !matches!(&**cls, Obj::Class(_)) {
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "issubclass() arg 1 must be a class, not {}",
                val_type_name(m, cls)
            ),
        ));
    }
    match issubclass_val(cls, of) {
        Ok(b) => Ok(v_bool(b)),
        Err(_) => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "issubclass() arg 2 must be a class or tuple of classes, not {}",
                val_type_name(m, of)
            ),
        )),
    }
}

fn bi_repr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let r = repr_val(m, vm, need(m, "repr", args)?)?;
    Ok(v_str(&r))
}

fn bi_ascii(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let r = repr_val(m, vm, need(m, "ascii", args)?)?;
    Ok(v_str(&str_ascii(&r)))
}

fn bi_abs(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "abs", args)?;
    match &**v {
        Obj::Int(i) => Ok(Rc::new(Obj::Int(Rc::new(i.abs())))),
        Obj::Bool(b) => Ok(v_int(i64::from(*b))),
        Obj::Float(f) => Ok(Rc::new(Obj::Float(f.abs()))),
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__abs__")
                && is_function_like(&f)
            {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                return call_val(m, vm, &bound, &[], &[]);
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!("bad operand type for abs(): '{}'", val_type_name(m, v)),
            ))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("bad operand type for abs(): '{}'", val_type_name(m, v)),
        )),
    }
}

fn apply_key(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    key: &Option<Val>,
    v: &Val,
) -> Result<Val, Val> {
    match key {
        Some(f) if !matches!(&**f, Obj::None_) => call_val(m, vm, f, std::slice::from_ref(v), &[]),
        _ => Ok(v.clone()),
    }
}

fn extremum(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    name: &str,
    args: &[Val],
    kwargs: &[(&str, Val)],
    op: CmpOp,
) -> Result<Val, Val> {
    let key = kwarg_val(kwargs, "key");
    let items: Vec<Val> = if args.len() == 1 {
        drain_iter(m, vm, &args[0])?
    } else {
        args.to_vec()
    };
    let mut it = items.into_iter();
    let Some(first) = it.next() else {
        return match kwarg_val(kwargs, "default") {
            Some(d) => Ok(d),
            None => Err(err(
                m,
                ErrorTag::ValueError,
                &format!("{name}() arg is an empty sequence"),
            )),
        };
    };
    let mut best = first;
    let mut best_k = apply_key(m, vm, &key, &best)?;
    for x in it {
        let k = apply_key(m, vm, &key, &x)?;
        if compare_val(m, vm, op, &k, &best_k)? {
            best = x;
            best_k = k;
        }
    }
    Ok(best)
}

fn bi_min(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    extremum(m, vm, "min", args, kwargs, CmpOp::Lt)
}

fn bi_max(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    extremum(m, vm, "max", args, kwargs, CmpOp::Gt)
}

fn bi_sum(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let it = need(m, "sum", args)?;
    let start = match args.get(1).cloned().or_else(|| kwarg_val(kwargs, "start")) {
        Some(s) => s,
        None => v_int(0),
    };
    if matches!(&*start, Obj::Str(_) | Obj::Bytes(_)) {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "sum() can't sum strings [use ''.join(seq) instead]",
        ));
    }
    let mut acc = start;
    for x in drain_iter(m, vm, it)? {
        acc = binop_val(m, vm, crate::ir::BinOp::Add, &acc, &x)?;
    }
    Ok(acc)
}

fn bi_sorted(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let key = match kwarg_val(kwargs, "key") {
        Some(k) if !matches!(&*k, Obj::None_) => Some(k),
        _ => None,
    };
    let reverse = kwarg_bool(kwargs, "reverse");
    let items = drain_iter(m, vm, need(m, "sorted", args)?)?;
    let sorted = sort_items(m, vm, items, &key, reverse)?;
    Ok(Rc::new(Obj::List(RefCell::new(sorted))))
}

fn bi_reversed(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "reversed", args)?;
    match &**v {
        Obj::List(l) => Ok(Rc::new(Obj::IterRevList {
            list: v.clone(),
            pos: Cell::new(l.borrow().len()),
        })),
        Obj::Tuple(_) | Obj::Str(_) | Obj::Bytes(_) | Obj::Range { .. } => {
            // Drain in order, reverse, then iterate the result forward.
            let mut items = drain_iter(m, vm, v)?;
            items.reverse();
            Ok(Rc::new(Obj::IterList {
                list: Rc::new(Obj::List(RefCell::new(items))),
                pos: Cell::new(0),
            }))
        }
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__reversed__") {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                return call_val(m, vm, &bound, &[], &[]);
            }
            // Sequence protocol fallback: __len__ + __getitem__.
            if mro_attr(v, "__len__").is_some() && mro_attr(v, "__getitem__").is_some() {
                let n = len_val(m, vm, v)?;
                return Ok(Rc::new(Obj::IterRevGetItem {
                    obj: v.clone(),
                    pos: Cell::new(n as i128),
                }));
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                "argument to reversed() must be a sequence",
            ))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            "argument to reversed() must be a sequence",
        )),
    }
}

fn bi_enumerate(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let it = iter_val(m, vm, need(m, "enumerate", args)?)?;
    let start = match args.get(1).cloned().or_else(|| kwarg_val(kwargs, "start")) {
        Some(v) => py_index(m, vm, &v)?,
        None => 0,
    };
    Ok(Rc::new(Obj::IterEnum {
        it,
        count: Cell::new(start as i128),
    }))
}

fn bi_zip(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let its: Vec<Val> = {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(iter_val(m, vm, a)?);
        }
        out
    };
    if !kwarg_bool(kwargs, "strict") {
        return Ok(Rc::new(Obj::IterZip { its }));
    }
    // strict: equal lengths, checked by draining fully
    let drained: Vec<Vec<Val>> = {
        let mut out = Vec::with_capacity(its.len());
        for it in &its {
            out.push(drain_iter(m, vm, it)?);
        }
        out
    };
    for (i, d) in drained.iter().enumerate().skip(1) {
        if d.len() != drained[0].len() {
            let shorter = if d.len() < drained[0].len() { i } else { 0 };
            return Err(err(
                m,
                ErrorTag::ValueError,
                &format!(
                    "zip() argument {shorter} is shorter than argument {}",
                    1 + (shorter == 0) as usize * (i - 1)
                ),
            ));
        }
    }
    let n = drained.first().map(|d| d.len()).unwrap_or(0);
    let rows: Vec<Val> = (0..n)
        .map(|i| {
            Rc::new(Obj::Tuple(Rc::new(
                drained.iter().map(|d| d[i].clone()).collect(),
            )))
        })
        .collect();
    Ok(Rc::new(Obj::IterList {
        list: Rc::new(Obj::List(RefCell::new(rows))),
        pos: Cell::new(0),
    }))
}

fn bi_map(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some((f, rest)) = args.split_first() else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "map() must have at least two arguments.",
        ));
    };
    let its: Vec<Val> = {
        let mut out = Vec::with_capacity(rest.len());
        for a in rest {
            out.push(iter_val(m, vm, a)?);
        }
        out
    };
    let (it, spread) = if its.len() == 1 {
        (its[0].clone(), false)
    } else {
        (Rc::new(Obj::IterZip { its }), true)
    };
    Ok(Rc::new(Obj::IterMap {
        f: f.clone(),
        it,
        spread,
    }))
}

fn bi_filter(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some((f, rest)) = args.split_first() else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "filter() must have at least two arguments.",
        ));
    };
    let it = iter_val(
        m,
        vm,
        rest.first().ok_or_else(|| {
            err(
                m,
                ErrorTag::TypeError,
                "filter() must have at least two arguments.",
            )
        })?,
    )?;
    let f = if matches!(&**f, Obj::None_) {
        None
    } else {
        Some(f.clone())
    };
    Ok(Rc::new(Obj::IterFilter { f, it }))
}

fn bi_any(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    for x in drain_iter(m, vm, need(m, "any", args)?)? {
        if truth_val(m, vm, &x)? {
            return Ok(v_bool(true));
        }
    }
    Ok(v_bool(false))
}

fn bi_all(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    for x in drain_iter(m, vm, need(m, "all", args)?)? {
        if !truth_val(m, vm, &x)? {
            return Ok(v_bool(false));
        }
    }
    Ok(v_bool(true))
}

fn bi_iter(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if args.len() != 1 {
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!("iter expected at most 1 argument, got {}", args.len()),
        ));
    }
    iter_val(m, vm, &args[0])
}

fn bi_next(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let it = need(m, "next", args)?;
    match next_val(m, vm, it) {
        Ok(v) => Ok(v),
        Err(e) => match args.get(1) {
            Some(d) if is_exc_matches(m, &e, "StopIteration") => Ok(d.clone()),
            _ => Err(e),
        },
    }
}

fn bi_hash(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "hash", args)?;
    match hash_vm(m, vm, v) {
        Ok(h) => Ok(v_int(h)),
        Err(e) => Err(e),
    }
}

fn bi_id(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "id", args)?;
    let addr = Rc::as_ptr(v) as *const () as usize;
    Ok(v_int(addr as i64))
}

fn bi_ord(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "ord", args)?;
    match &**v {
        Obj::Str(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(v_int(c as i64)),
                _ => Err(err(
                    m,
                    ErrorTag::TypeError,
                    &format!(
                        "ord() expected a character, but string of length {} found",
                        s.chars().count()
                    ),
                )),
            }
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "ord() expected string of length 1, but {} found",
                val_type_name(m, v)
            ),
        )),
    }
}

fn bi_chr(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "chr", args)?;
    let Some(code) = to_big(v).and_then(|b| b.to_i64()) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' object cannot be interpreted as an integer",
                val_type_name(m, v)
            ),
        ));
    };
    match u32::try_from(code).ok().and_then(char::from_u32) {
        Some(c) => Ok(v_str(&c.to_string())),
        None => Err(err(
            m,
            ErrorTag::ValueError,
            "chr() arg not in range(0x110000)",
        )),
    }
}

fn radix_str(
    m: &mut TestModel,
    name: &str,
    args: &[Val],
    radix: u32,
    prefix: &str,
) -> Result<Val, Val> {
    let v = need(m, name, args)?;
    let Some(b) = to_big(v) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "'{}' object cannot be interpreted as an integer",
                val_type_name(m, v)
            ),
        ));
    };
    let digits = b.abs().to_string_radix(radix);
    let sign = if b.is_negative() { "-" } else { "" };
    Ok(v_str(&format!("{sign}{prefix}{digits}")))
}

fn bi_hex(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    radix_str(m, "hex", args, 16, "0x")
}

fn bi_oct(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    radix_str(m, "oct", args, 8, "0o")
}

fn bi_bin(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    radix_str(m, "bin", args, 2, "0b")
}

fn bi_divmod(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return Err(err(m, ErrorTag::TypeError, "divmod expected 2 arguments"));
    };
    match (to_big(a), to_big(b)) {
        (Some(x), Some(y)) => {
            if y.is_zero() {
                return Err(err(
                    m,
                    ErrorTag::ZeroDivisionError,
                    "integer division or modulo by zero",
                ));
            }
            let (q, r) = x.divmod(&y).expect("nonzero");
            Ok(Rc::new(Obj::Tuple(Rc::new(vec![
                Rc::new(Obj::Int(Rc::new(q))),
                Rc::new(Obj::Int(Rc::new(r))),
            ]))))
        }
        _ => {
            let (x, y) = (to_float(m, a)?, to_float(m, b)?);
            if y == 0.0 {
                return Err(err(m, ErrorTag::ZeroDivisionError, "float divmod()"));
            }
            Ok(Rc::new(Obj::Tuple(Rc::new(vec![
                Rc::new(Obj::Float(floordiv_py(x, y))),
                Rc::new(Obj::Float(fmod_py(x, y))),
            ]))))
        }
    }
}

fn bi_round(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "round", args)?;
    let ndigits = match args
        .get(1)
        .cloned()
        .or_else(|| kwarg_val(kwargs, "ndigits"))
    {
        Some(d) => Some(py_index(m, vm, &d)?),
        None => None,
    };
    match &**v {
        Obj::Int(_) | Obj::Bool(_) => match ndigits {
            None | Some(0) => Ok(v.clone()),
            Some(n) if n > 0 => Ok(v.clone()),
            Some(n) => {
                let iv = match &**v {
                    Obj::Int(i) => (**i).clone(),
                    Obj::Bool(b) => BigInt::from_i64(i64::from(*b)),
                    _ => unreachable!("matched above"),
                };
                // Round half to even to a multiple of 10^k (k = -n).
                let k = -(n as i128) as u64;
                if k > iv.bit_len() as u64 + 8 {
                    return Ok(Rc::new(Obj::Int(Rc::new(BigInt::zero()))));
                }
                let unit = BigInt::from_i64(10).pow(k);
                let neg = iv.is_negative();
                let mag = if neg { BigInt::zero().sub(&iv) } else { iv };
                let (mut q, r) = mag.divmod(&unit).expect("unit nonzero");
                let twice = r.add(&r);
                let diff = twice.sub(&unit);
                let q_even = q
                    .divmod(&BigInt::from_i64(2))
                    .is_some_and(|(_, rem)| rem.is_zero());
                let round_up = if diff.is_zero() {
                    !q_even
                } else {
                    !diff.is_negative()
                };
                if round_up {
                    q = q.add(&BigInt::from_i64(1));
                }
                let mut res = q.mul(&unit);
                if neg && !res.is_zero() {
                    res = BigInt::zero().sub(&res);
                }
                Ok(Rc::new(Obj::Int(Rc::new(res))))
            }
        },
        Obj::Float(f) => match ndigits {
            None => {
                let s = format!("{:.0}", f);
                let b = BigInt::parse(&s).unwrap_or_else(BigInt::zero);
                Ok(Rc::new(Obj::Int(Rc::new(b))))
            }
            Some(n) if n < 0 => {
                let scale = 10f64.powi(-(n as i32).clamp(i32::MIN, 30_000));
                let r = format!("{:.0}", f / scale).parse::<f64>().unwrap_or(*f) * scale;
                Ok(Rc::new(Obj::Float(r)))
            }
            Some(n) => {
                let n = (n as usize).min(340);
                let r = format!("{:.*}", n, f).parse::<f64>().unwrap_or(*f);
                Ok(Rc::new(Obj::Float(r)))
            }
        },
        Obj::Instance(_) => {
            if let Some(f) = mro_attr(v, "__round__") {
                let bound = bind(m, vm, &f, v, Some(v.clone()))?;
                return call_val(m, vm, &bound, &args[1..], &[]);
            }
            Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "type {} doesn't define __round__ method",
                    val_type_name(m, v)
                ),
            ))
        }
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            &format!(
                "type {} doesn't define __round__ method",
                val_type_name(m, v)
            ),
        )),
    }
}

fn bi_pow(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if args.len() <= 2 {
        let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
            return Err(err(
                m,
                ErrorTag::TypeError,
                "pow expected at least 2 arguments",
            ));
        };
        return binop_val(m, vm, crate::ir::BinOp::Pow, a, b);
    }
    let (a, b, c) = (&args[0], &args[1], &args[2]);
    let (Some(x), Some(y), Some(z)) = (to_big(a), to_big(b), to_big(c)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "pow() 3rd argument not allowed unless all arguments are integers",
        ));
    };
    if y.is_negative() {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "pow() 2nd argument cannot be negative when 3rd argument specified",
        ));
    }
    if z.is_zero() {
        return Err(err(
            m,
            ErrorTag::ValueError,
            "pow() 3rd argument cannot be 0",
        ));
    }
    let e = y
        .to_i64()
        .ok_or_else(|| err(m, ErrorTag::OverflowError, "exponent too large"))?;
    if x.bit_len() as i64 * e > 4_000_000 {
        return Err(err(m, ErrorTag::OverflowError, "exponent too large"));
    }
    let p = x.pow(e as u64);
    let (_, r) = p.divmod(&z).expect("nonzero");
    Ok(Rc::new(Obj::Int(Rc::new(r))))
}

fn bi_getattr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v)) = (args.first(), args.get(1)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "getattr expected at least 2 arguments",
        ));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    match getattr_val(m, vm, obj, name) {
        Ok(v) => Ok(v),
        Err(e) => match args.get(2) {
            Some(d) if is_exc_matches(m, &e, "AttributeError") => Ok(d.clone()),
            _ => Err(e),
        },
    }
}

fn bi_setattr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v), Some(value)) = (args.first(), args.get(1), args.get(2)) else {
        return Err(err(m, ErrorTag::TypeError, "setattr expected 3 arguments"));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    setattr_val(m, vm, obj, name, value)?;
    Ok(v_none())
}

fn bi_delattr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v)) = (args.first(), args.get(1)) else {
        return Err(err(m, ErrorTag::TypeError, "delattr expected 2 arguments"));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    delattr_val(m, vm, obj, name)?;
    Ok(v_none())
}

fn bi_hasattr(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v)) = (args.first(), args.get(1)) else {
        return Err(err(m, ErrorTag::TypeError, "hasattr expected 2 arguments"));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    match getattr_val(m, vm, obj, name) {
        Ok(_) => Ok(v_bool(true)),
        Err(e) if is_exc_matches(m, &e, "AttributeError") => Ok(v_bool(false)),
        Err(e) => Err(e),
    }
}

fn bi_callable(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "callable", args)?;
    let ok = matches!(
        &**v,
        Obj::Function(_)
            | Obj::Native(..)
            | Obj::BoundMethod { .. }
            | Obj::Class(_)
            | Obj::StaticMethod(_)
            | Obj::ClassMethod(_)
            | Obj::Property { .. }
    ) || matches!(&**v, Obj::Instance(i) if mro_attr(&i.class, "__call__").is_some());
    Ok(v_bool(ok))
}

fn bi_type(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match args.len() {
        1 => Ok(m_class_of(m, &args[0])),
        2 | 3 => {
            let Obj::Str(name) = &*args[0] else {
                return Err(err(m, ErrorTag::TypeError, "type() argument 1 must be str"));
            };
            let bases: Vec<Val> = match args.get(1).and_then(tuple_items_of) {
                Some(items) if !items.is_empty() => items,
                _ => vec![m.class_object.clone()],
            };
            let entries: Vec<(String, Val)> = match args.get(2) {
                Some(d) => match &**d {
                    Obj::Dict(t) => t
                        .borrow()
                        .items()
                        .into_iter()
                        .filter_map(|(k, v)| str_of(&k).map(|s| (s.to_string(), v)))
                        .collect(),
                    _ => {
                        return Err(err(
                            m,
                            ErrorTag::TypeError,
                            "type() argument 3 must be dict",
                        ));
                    }
                },
                None => Vec::new(),
            };
            make_class_value(m, vm, name, &bases, entries, &v_none(), &[])
        }
        n => Err(err(
            m,
            ErrorTag::TypeError,
            &format!("type() takes 1 or 3 arguments ({n} given)"),
        )),
    }
}

fn bi_super(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(typ), obj) = (args.first(), args.get(1)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "super() expected at least 1 argument",
        ));
    };
    if !matches!(&**typ, Obj::Class(_)) {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "super() argument 1 must be a type",
        ));
    }
    let obj = obj.cloned();
    Ok(Rc::new(Obj::Super(Rc::new(SuperObj {
        typ: typ.clone(),
        obj,
    }))))
}

fn bi_property(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if !args.is_empty() && !is_function_like(&args[0]) && !matches!(&*args[0], Obj::None_) {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "property() fget must be callable",
        ));
    }
    let fget = match args.first() {
        Some(f) if !matches!(&**f, Obj::None_) => Some(f.clone()),
        _ => kwarg_val(kwargs, "fget").filter(|f| !matches!(&**f, Obj::None_)),
    };
    let fset = match args.get(1) {
        Some(f) if !matches!(&**f, Obj::None_) => Some(f.clone()),
        _ => kwarg_val(kwargs, "fset").filter(|f| !matches!(&**f, Obj::None_)),
    };
    let fdel = match args.get(2) {
        Some(f) if !matches!(&**f, Obj::None_) => Some(f.clone()),
        _ => kwarg_val(kwargs, "fdel").filter(|f| !matches!(&**f, Obj::None_)),
    };
    Ok(Rc::new(Obj::Property { fget, fset, fdel }))
}

fn bi_staticmethod(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(f) = args.first() else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "staticmethod expected 1 argument",
        ));
    };
    Ok(Rc::new(Obj::StaticMethod(f.clone())))
}

fn bi_classmethod(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(f) = args.first() else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "classmethod expected 1 argument",
        ));
    };
    Ok(Rc::new(Obj::ClassMethod(f.clone())))
}

fn bi_globals(
    _m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    _args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(vm.globals.clone())
}

fn bi_vars(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(v) = args.first() else {
        return Ok(vm.globals.clone());
    };
    match &**v {
        Obj::Module(md) => Ok(new_dict_from(md.dict.borrow().entries.clone())),
        Obj::Instance(i) => Ok(i.attrs.clone()),
        Obj::Class(c) => Ok(new_dict_from(c.dict.borrow().entries.clone())),
        _ => Err(err(
            m,
            ErrorTag::TypeError,
            "vars() argument must have __dict__ attribute",
        )),
    }
}

fn bi_format(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let v = need(m, "format", args)?;
    let spec = match args.get(1) {
        Some(s) => s.clone(),
        None => v_str(""),
    };
    let Obj::Str(_) = &*spec else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "format() argument 2 must be str",
        ));
    };
    format_val(m, vm, v, &spec)
}

fn bi_typevar(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(v) = args.first() else {
        return Err(err(m, ErrorTag::TypeError, "TypeVar() requires a name"));
    };
    let Obj::Str(s) = &**v else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "TypeVar() name must be a string",
        ));
    };
    Ok(Rc::new(Obj::TypeVar(Rc::from(s.as_ref()))))
}

fn bi_paramspec(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(v) = args.first() else {
        return Err(err(m, ErrorTag::TypeError, "ParamSpec() requires a name"));
    };
    let Obj::Str(s) = &**v else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "ParamSpec() name must be a string",
        ));
    };
    Ok(Rc::new(Obj::ParamSpec(Rc::from(s.as_ref()))))
}

fn bi_typevartuple(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(v) = args.first() else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "TypeVarTuple() requires a name",
        ));
    };
    let Obj::Str(s) = &**v else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "TypeVarTuple() name must be a string",
        ));
    };
    Ok(Rc::new(Obj::TypeVarTuple(Rc::from(s.as_ref()))))
}

fn bi_cast(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    match args.get(1) {
        Some(v) => Ok(v.clone()),
        None => Ok(v_none()),
    }
}

// ---------------------------------------------------------------------------
// The `except*` machinery: exception_group_match, split and PrepReraiseStar
// ---------------------------------------------------------------------------

/// A `BaseExceptionGroup` instance (or subclass).
fn is_group_inst(m: &TestModel, v: &Val) -> bool {
    match &**v {
        Obj::Instance(i) => mro_has(&i.class, &m.exc_base_group),
        _ => false,
    }
}

fn group_children(v: &Val) -> Option<Vec<Val>> {
    match &**v {
        Obj::Instance(i) => i
            .exc
            .borrow()
            .as_ref()
            .and_then(|e| e.group.as_ref().map(|g| g.children.clone())),
        _ => None,
    }
}

/// The chain root this group was derived from (`None` for constructor-made
/// groups), standing in for CPython's shared `__traceback__`/`__cause__`/
/// `__context__` pointers.
fn meta_root_of(v: &Val) -> Option<Val> {
    match &**v {
        Obj::Instance(i) => i
            .exc
            .borrow()
            .as_ref()
            .and_then(|e| e.group.as_ref().and_then(|g| g.meta_root.clone())),
        _ => None,
    }
}

/// `is_same_exception_metadata`: a reraised group shares the original's
/// exception metadata (the split chain copies it), a raised one does not.
fn same_meta(m: &TestModel, e: &Val, orig: &Val) -> bool {
    if Rc::ptr_eq(e, orig) {
        return true;
    }
    match meta_root_of(e) {
        Some(root) => Rc::ptr_eq(&root, orig),
        None => {
            let _ = m;
            false
        }
    }
}

/// `BaseExceptionGroup.__new__`'s class pick: all `Exception`s make an
/// `ExceptionGroup`, any bare `BaseException` forces `BaseExceptionGroup`.
fn group_class_for(m: &TestModel, children: &[Val]) -> Val {
    let base = m.exc_base.clone();
    let all_exc = children.iter().all(|c| match &**c {
        Obj::Instance(i) => mro_has(&i.class, &base),
        _ => false,
    });
    if all_exc {
        m.exc_group.clone()
    } else {
        m.exc_base_group.clone()
    }
}

/// `derive`: a fresh group of `src`'s message over `children`, in the
/// exception-group chain rooted at `root`.
fn derive_group(m: &TestModel, src: &Val, children: Vec<Val>, root: &Val) -> Val {
    let (message, src_tb) = match &**src {
        Obj::Instance(i) => {
            let d = i.exc.borrow();
            let msg = d
                .as_ref()
                .and_then(|e| e.group.as_ref().map(|g| g.message.clone()))
                .unwrap_or_else(v_none);
            let tb = d.as_ref().map(|e| e.tb.clone()).unwrap_or_default();
            (msg, tb)
        }
        _ => (v_none(), Vec::new()),
    };
    let class = group_class_for(m, &children);
    let args = vec![
        message.clone(),
        Rc::new(Obj::Tuple(Rc::new(children.clone()))),
    ];
    let inst = new_instance(
        &class,
        args,
        Some(GroupData {
            message,
            children,
            meta_root: Some(root.clone()),
        }),
    );
    // the split chain shares the parent's traceback
    if let Obj::Instance(i) = &*inst
        && let Some(d) = i.exc.borrow_mut().as_mut()
    {
        d.tb = src_tb;
    }
    inst
}

/// `exceptiongroup_split_recursive` with the by-type matcher and a rest
/// result: the subgroup of `exc` matching `types` and its complement, each
/// `None` when empty. A group whose own class matches is kept whole.
fn split_group_recursive(
    m: &TestModel,
    exc: &Val,
    types: &Val,
    root: &Val,
) -> (Option<Val>, Option<Val>) {
    if exc_matches_val(m, exc, types) {
        return (Some(exc.clone()), None);
    }
    if !is_group_inst(m, exc) {
        return (None, Some(exc.clone()));
    }
    let Some(children) = group_children(exc) else {
        return (None, Some(exc.clone()));
    };
    let mut matched = Vec::new();
    let mut rest = Vec::new();
    for c in &children {
        let (mm, rr) = split_group_recursive(m, c, types, root);
        if let Some(x) = mm {
            matched.push(x);
        }
        if let Some(x) = rr {
            rest.push(x);
        }
    }
    let mo = (!matched.is_empty()).then(|| derive_group(m, exc, matched, root));
    let ro = (!rest.is_empty()).then(|| derive_group(m, exc, rest, root));
    (mo, ro)
}

fn collect_leaf_ids(v: &Val, ids: &mut Vec<usize>) {
    if let Obj::Instance(i) = &**v {
        let d = i.exc.borrow();
        if let Some(e) = d.as_ref()
            && let Some(g) = &e.group
        {
            for c in &g.children {
                collect_leaf_ids(c, ids);
            }
            return;
        }
    }
    ids.push(Rc::as_ptr(v) as usize);
}

/// `exception_group_projection`: the subgroup of `orig` containing exactly
/// the leaves whose ids `keep` collects, `None` when it is empty.
fn projection_recursive(m: &TestModel, exc: &Val, ids: &[usize], root: &Val) -> Option<Val> {
    let children = group_children(exc)?;
    let mut kept = Vec::new();
    for c in &children {
        if is_group_inst(m, c) {
            if let Some(sub) = projection_recursive(m, c, ids, root) {
                kept.push(sub);
            }
        } else if ids.contains(&(Rc::as_ptr(c) as usize)) {
            kept.push(c.clone());
        }
    }
    if kept.is_empty() {
        None
    } else {
        Some(derive_group(m, exc, kept, root))
    }
}

// ---------------------------------------------------------------------------
// Class creation (`__build_class__`)
// ---------------------------------------------------------------------------

fn native(name: &'static str, f: NativeFn) -> Val {
    Rc::new(Obj::Native(name, f))
}

/// Creates the class object over a finished class-body namespace: C3 MRO,
/// the `__classcell__` binding, `__set_name__` and `__init_subclass__`.
fn make_class_value(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    name: &str,
    bases: &[Val],
    entries: Vec<(String, Val)>,
    classcell: &Val,
    kwds: &[(String, Val)],
) -> Result<Val, Val> {
    for b in bases {
        if !matches!(&**b, Obj::Class(_)) {
            return Err(err(
                m,
                ErrorTag::TypeError,
                &format!(
                    "__build_class__: base is not a class (got {})",
                    val_type_name(m, b)
                ),
            ));
        }
    }
    let bases: Vec<Val> = if bases.is_empty() {
        vec![m.class_object.clone()]
    } else {
        bases.to_vec()
    };
    let all_exc = bases.iter().all(|b| is_exc_class(m, b));
    let kind = if all_exc {
        ClassKind::Exception { base: false }
    } else {
        ClassKind::User
    };
    let cls = Rc::new(Obj::Class(Rc::new(ClassObj {
        name: name.to_owned(),
        bases: bases.clone(),
        mro: c3(name, &bases),
        dict: RefCell::new(AttrMap::default()),
        kind,
        abc: None,
        module: "__main__",
    })));
    {
        let mut dict = cls_dict(&cls).borrow_mut();
        for (n, v) in entries {
            if n == "__classcell__" {
                continue;
            }
            dict.set(&n, v);
        }
    }
    // __classcell__: zero-argument super() and __class__ in the body
    if matches!(&**classcell, Obj::Cell(_)) {
        m.cell_set(classcell, cls.clone());
    }
    // __set_name__(owner, name) on every descriptor that defines it
    let entries_now: Vec<(String, Val)> = cls_dict(&cls).borrow().entries.clone();
    for (n, v) in entries_now {
        if let Obj::Instance(i) = &*v
            && let Some(f) = mro_attr(&i.class, "__set_name__")
        {
            let owner = cls.clone();
            let name_v = v_str(&n);
            call_val(m, vm, &f, &[v.clone(), owner, name_v], &[])?;
        }
    }
    // __init_subclass__ of the nearest base that defines it (beyond object),
    // with the class statement's keyword arguments
    let mut consumed_kwds = false;
    for b in &bases {
        let mut defined = false;
        if let Some(f) = mro_attr(b, "__init_subclass__")
            && is_function_like(&f)
        {
            let owner_cls = as_class(b).expect("base class");
            let is_object = matches!(owner_cls.kind, ClassKind::Object);
            if !is_object {
                defined = true;
                let bound = bind(m, vm, &f, b, None)?;
                let kw: Vec<(&str, Val)> =
                    kwds.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                call_val(m, vm, &bound, std::slice::from_ref(&cls), &kw)?;
                consumed_kwds = true;
            }
        }
        if defined {
            break;
        }
    }
    if !consumed_kwds && !kwds.is_empty() {
        // No base consumed them: object.__init_subclass__ rejects kwargs.
        return Err(err(
            m,
            ErrorTag::TypeError,
            &format!("{name}() takes no keyword arguments"),
        ));
    }
    Ok(cls)
}

fn cls_dict(cls: &Val) -> &RefCell<AttrMap> {
    match &**cls {
        Obj::Class(c) => &c.dict,
        _ => unreachable!("class dict"),
    }
}

// ---------------------------------------------------------------------------
// Construction of the model: the class registry, exception hierarchy and
// builtin namespace
// ---------------------------------------------------------------------------

fn bt_class(name: &str, bases: &[Val], bt: BuiltinType) -> Val {
    new_class(name, bases, ClassKind::Builtin(bt))
}

/// `BaseException.__init__`: replace the instance's args.
fn exc_init(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    if let Some(inst) = args.first()
        && let Obj::Instance(i) = &**inst
    {
        i.exc
            .borrow_mut()
            .as_mut()
            .expect("exception instance")
            .args = args[1..].to_vec();
    }
    Ok(v_none())
}

fn exc_class(name: &str, bases: &[Val], base: bool) -> Val {
    new_class(name, bases, ClassKind::Exception { base })
}

fn new_abc_class(name: &str, bases: &[Val], abc: AbcKind) -> Val {
    Rc::new(Obj::Class(Rc::new(ClassObj {
        name: name.to_owned(),
        bases: bases.to_vec(),
        mro: if bases.is_empty() {
            Vec::new()
        } else {
            c3(name, bases)
        },
        dict: RefCell::new(AttrMap::default()),
        kind: ClassKind::Abc,
        abc: Some(abc),
        module: "collections.abc",
    })))
}

fn new_module(name: &str) -> Val {
    Rc::new(Obj::Module(Rc::new(ModuleObj {
        name: name.to_owned(),
        dict: RefCell::new(AttrMap::default()),
    })))
}

fn mod_set(module: &Val, name: &str, v: Val) {
    if let Obj::Module(md) = &**module {
        md.dict.borrow_mut().set(name, v);
    }
}

/// `object.__getattribute__(self, name)`: the hook-free lookup.
fn obj_getattribute(
    m: &mut TestModel,
    vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v)) = (args.first(), args.get(1)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "__getattribute__ expected 2 arguments",
        ));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    instance_plain_getattr(m, vm, obj, name)
}

/// `object.__setattr__(self, name, value)`: the instance-dict write.
fn obj_setattribute(
    m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let (Some(obj), Some(name_v), Some(value)) = (args.first(), args.get(1), args.get(2)) else {
        return Err(err(
            m,
            ErrorTag::TypeError,
            "__setattr__ expected 3 arguments",
        ));
    };
    let Obj::Str(name) = &**name_v else {
        return Err(err(m, ErrorTag::TypeError, "attribute name must be string"));
    };
    if let Obj::Instance(i) = &**obj {
        dict_set_str(&i.attrs, name, value.clone());
        Ok(v_none())
    } else {
        Err(attr_error(m, obj, name))
    }
}

fn native_noop(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    _args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    Ok(Rc::new(Obj::None_))
}

/// `object.__new__(cls)`: a fresh instance of `cls`'s own kind.
fn obj_new(
    _m: &mut TestModel,
    _vm: &mut Interp<'_, TestModel>,
    args: &[Val],
    _kwargs: &[(&str, Val)],
) -> Result<Val, Val> {
    let Some(cls) = args.first() else {
        return Err(err(
            _m,
            ErrorTag::TypeError,
            "object.__new__(): not enough arguments",
        ));
    };
    if !matches!(&**cls, Obj::Class(_)) {
        return Err(err(
            _m,
            ErrorTag::TypeError,
            "object.__new__(X): X is not a type object",
        ));
    }
    Ok(new_instance(cls, Vec::new(), None))
}

impl TestModel {
    pub fn new() -> TestModel {
        let object = new_class("object", &[], ClassKind::Object);
        // `object.__init__`/`__init_subclass__` exist on every instance and
        // `super()` lookup: no-ops accepting (and ignoring) any arguments.
        if let Obj::Class(c) = &*object {
            c.dict
                .borrow_mut()
                .set("__init__", Rc::new(Obj::Native("__init__", native_noop)));
            c.dict.borrow_mut().set(
                "__init_subclass__",
                Rc::new(Obj::Native("__init_subclass__", native_noop)),
            );
            // `object.__getattribute__`/`__setattr__`: the plain lookup
            // users reach for from their own hooks.
            c.dict.borrow_mut().set(
                "__getattribute__",
                Rc::new(Obj::Native("__getattribute__", obj_getattribute)),
            );
            c.dict.borrow_mut().set(
                "__setattr__",
                Rc::new(Obj::Native("__setattr__", obj_setattribute)),
            );
            c.dict
                .borrow_mut()
                .set("__new__", Rc::new(Obj::Native("__new__", obj_new)));
        }
        let type_cls = new_class("type", std::slice::from_ref(&object), ClassKind::Type);
        let int = bt_class("int", std::slice::from_ref(&object), BuiltinType::Int);
        let bool_cls = bt_class("bool", std::slice::from_ref(&int), BuiltinType::Bool);
        let float = bt_class("float", std::slice::from_ref(&object), BuiltinType::Float);
        let str_cls = bt_class("str", std::slice::from_ref(&object), BuiltinType::Str);
        let bytes = bt_class("bytes", std::slice::from_ref(&object), BuiltinType::Bytes);
        let list = bt_class("list", std::slice::from_ref(&object), BuiltinType::List);
        let tuple = bt_class("tuple", std::slice::from_ref(&object), BuiltinType::Tuple);
        let dict = bt_class("dict", std::slice::from_ref(&object), BuiltinType::Dict);
        let set = bt_class("set", std::slice::from_ref(&object), BuiltinType::Set);
        let frozenset = bt_class(
            "frozenset",
            std::slice::from_ref(&object),
            BuiltinType::FrozenSet,
        );
        let slice = bt_class("slice", std::slice::from_ref(&object), BuiltinType::Slice);
        let range = bt_class("range", std::slice::from_ref(&object), BuiltinType::Range);
        let function = bt_class(
            "function",
            std::slice::from_ref(&object),
            BuiltinType::Function,
        );
        let module_cls = bt_class("module", std::slice::from_ref(&object), BuiltinType::Module);
        let generator = bt_class(
            "generator",
            std::slice::from_ref(&object),
            BuiltinType::Generator,
        );
        let coroutine = bt_class(
            "coroutine",
            std::slice::from_ref(&object),
            BuiltinType::Generator,
        );
        let asyncgen = bt_class(
            "asyncgenerator",
            std::slice::from_ref(&object),
            BuiltinType::Generator,
        );
        let traceback = bt_class(
            "traceback",
            std::slice::from_ref(&object),
            BuiltinType::Traceback,
        );
        let none_type = bt_class("NoneType", std::slice::from_ref(&object), BuiltinType::None);
        let ellipsis = bt_class(
            "ellipsis",
            std::slice::from_ref(&object),
            BuiltinType::Ellipsis,
        );
        let property = bt_class(
            "property",
            std::slice::from_ref(&object),
            BuiltinType::Property,
        );
        let staticmethod = bt_class(
            "staticmethod",
            std::slice::from_ref(&object),
            BuiltinType::Function,
        );
        let classmethod = bt_class(
            "classmethod",
            std::slice::from_ref(&object),
            BuiltinType::Function,
        );
        let super_cls = bt_class("super", std::slice::from_ref(&object), BuiltinType::Super);

        // the exception hierarchy
        let root = exc_class("BaseException", std::slice::from_ref(&object), true);
        // `super().__init__(...)` from a user __init__ resets args.
        if let Obj::Class(c) = &*root {
            c.dict
                .borrow_mut()
                .set("__init__", Rc::new(Obj::Native("__init__", exc_init)));
        }
        let ki = exc_class("KeyboardInterrupt", std::slice::from_ref(&root), false);
        let se = exc_class("SystemExit", std::slice::from_ref(&root), false);
        let gen_exit = exc_class("GeneratorExit", std::slice::from_ref(&root), false);
        let exc = exc_class("Exception", std::slice::from_ref(&root), false);
        let stop_iteration = exc_class("StopIteration", std::slice::from_ref(&exc), false);
        let stop_async = exc_class("StopAsyncIteration", std::slice::from_ref(&exc), false);
        let arithmetic = exc_class("ArithmeticError", std::slice::from_ref(&exc), false);
        let zero_div = exc_class(
            "ZeroDivisionError",
            std::slice::from_ref(&arithmetic),
            false,
        );
        let overflow = exc_class("OverflowError", std::slice::from_ref(&arithmetic), false);
        let lookup = exc_class("LookupError", std::slice::from_ref(&exc), false);
        let index_err = exc_class("IndexError", std::slice::from_ref(&lookup), false);
        let key_err = exc_class("KeyError", std::slice::from_ref(&lookup), false);
        let name_err = exc_class("NameError", std::slice::from_ref(&exc), false);
        let unbound = exc_class("UnboundLocalError", std::slice::from_ref(&name_err), false);
        let runtime = exc_class("RuntimeError", std::slice::from_ref(&exc), false);
        let not_impl = exc_class("NotImplementedError", std::slice::from_ref(&runtime), false);
        let recursion = exc_class("RecursionError", std::slice::from_ref(&runtime), false);
        let type_err = exc_class("TypeError", std::slice::from_ref(&exc), false);
        let value_err = exc_class("ValueError", std::slice::from_ref(&exc), false);
        let import_err = exc_class("ImportError", std::slice::from_ref(&exc), false);
        let module_err = exc_class(
            "ModuleNotFoundError",
            std::slice::from_ref(&import_err),
            false,
        );
        let attr_err = exc_class("AttributeError", std::slice::from_ref(&exc), false);
        let assertion = exc_class("AssertionError", std::slice::from_ref(&exc), false);
        let base_group = exc_class("BaseExceptionGroup", std::slice::from_ref(&root), false);
        let group = exc_class("ExceptionGroup", &[exc.clone(), base_group.clone()], false);

        let mut exc_by_tag = HashMap::new();
        exc_by_tag.insert(ErrorTag::TypeError, type_err.clone());
        exc_by_tag.insert(ErrorTag::ValueError, value_err.clone());
        exc_by_tag.insert(ErrorTag::NameError, name_err.clone());
        exc_by_tag.insert(ErrorTag::UnboundLocalError, unbound.clone());
        exc_by_tag.insert(ErrorTag::AttributeError, attr_err.clone());
        exc_by_tag.insert(ErrorTag::KeyError, key_err.clone());
        exc_by_tag.insert(ErrorTag::IndexError, index_err.clone());
        exc_by_tag.insert(ErrorTag::StopIteration, stop_iteration.clone());
        exc_by_tag.insert(ErrorTag::ZeroDivisionError, zero_div.clone());
        exc_by_tag.insert(ErrorTag::OverflowError, overflow.clone());
        exc_by_tag.insert(ErrorTag::RuntimeError, runtime.clone());
        exc_by_tag.insert(ErrorTag::RecursionError, recursion.clone());
        exc_by_tag.insert(ErrorTag::GeneratorExit, gen_exit.clone());
        exc_by_tag.insert(ErrorTag::AssertionError, assertion.clone());
        exc_by_tag.insert(ErrorTag::NotImplementedError, not_impl.clone());
        exc_by_tag.insert(ErrorTag::ImportError, import_err.clone());
        exc_by_tag.insert(ErrorTag::ModuleNotFoundError, module_err.clone());

        // the stub modules the corpus may import
        let typing_mod = new_module("typing");
        mod_set(&typing_mod, "TypeVar", native("TypeVar", bi_typevar));
        mod_set(&typing_mod, "ParamSpec", native("ParamSpec", bi_paramspec));
        mod_set(
            &typing_mod,
            "TypeVarTuple",
            native("TypeVarTuple", bi_typevartuple),
        );
        mod_set(&typing_mod, "cast", native("cast", bi_cast));
        mod_set(&typing_mod, "List", native("List", bi_cast));
        mod_set(&typing_mod, "TYPE_CHECKING", v_bool(false));

        let abc_iterable =
            new_abc_class("Iterable", std::slice::from_ref(&object), AbcKind::Iterable);
        let abc_iterator = new_abc_class(
            "Iterator",
            std::slice::from_ref(&abc_iterable),
            AbcKind::Iterable,
        );
        let abc_callable =
            new_abc_class("Callable", std::slice::from_ref(&object), AbcKind::Callable);
        let abc_sequence = new_abc_class(
            "Sequence",
            std::slice::from_ref(&abc_iterable),
            AbcKind::Sequence,
        );
        let abc_mut_seq = new_abc_class(
            "MutableSequence",
            std::slice::from_ref(&abc_sequence),
            AbcKind::Sequence,
        );
        let abc_mapping = new_abc_class(
            "Mapping",
            std::slice::from_ref(&abc_iterable),
            AbcKind::Mapping,
        );
        let abc_mut_map = new_abc_class(
            "MutableMapping",
            std::slice::from_ref(&abc_mapping),
            AbcKind::Mapping,
        );
        let abc_mod = new_module("collections.abc");
        mod_set(&abc_mod, "Iterable", abc_iterable.clone());
        mod_set(&abc_mod, "Iterator", abc_iterator.clone());
        mod_set(&abc_mod, "Callable", abc_callable);
        mod_set(&abc_mod, "Sequence", abc_sequence);
        mod_set(&abc_mod, "MutableSequence", abc_mut_seq);
        mod_set(&abc_mod, "Mapping", abc_mapping);
        mod_set(&abc_mod, "MutableMapping", abc_mut_map);

        let collections_mod = new_module("collections");
        mod_set(&collections_mod, "abc", abc_mod.clone());

        let mut builtins: HashMap<String, Val> = HashMap::new();
        builtins.insert("__name__".into(), v_str("builtins"));
        builtins.insert("None".into(), v_none());
        builtins.insert("True".into(), v_bool(true));
        builtins.insert("False".into(), v_bool(false));
        builtins.insert("Ellipsis".into(), Rc::new(Obj::Ellipsis));
        builtins.insert("NotImplemented".into(), Rc::new(Obj::NotImplemented));
        let classes: Vec<(&str, Val)> = vec![
            ("object", object.clone()),
            ("type", type_cls.clone()),
            ("int", int.clone()),
            ("float", float.clone()),
            ("bool", bool_cls.clone()),
            ("str", str_cls.clone()),
            ("bytes", bytes.clone()),
            (
                "bytearray",
                bt_class(
                    "bytearray",
                    std::slice::from_ref(&object),
                    BuiltinType::Bytes,
                ),
            ),
            ("list", list.clone()),
            ("tuple", tuple.clone()),
            ("dict", dict.clone()),
            ("set", set.clone()),
            ("frozenset", frozenset.clone()),
            ("range", range.clone()),
            ("slice", slice.clone()),
        ];
        for (n, c) in classes {
            builtins.insert(n.into(), c);
        }
        let natives: Vec<(&str, NativeFn)> = vec![
            ("print", bi_print),
            ("len", bi_len),
            ("isinstance", bi_isinstance),
            ("issubclass", bi_issubclass),
            ("repr", bi_repr),
            ("ascii", bi_ascii),
            ("abs", bi_abs),
            ("min", bi_min),
            ("max", bi_max),
            ("sum", bi_sum),
            ("sorted", bi_sorted),
            ("reversed", bi_reversed),
            ("enumerate", bi_enumerate),
            ("zip", bi_zip),
            ("map", bi_map),
            ("filter", bi_filter),
            ("any", bi_any),
            ("all", bi_all),
            ("iter", bi_iter),
            ("next", bi_next),
            ("hash", bi_hash),
            ("id", bi_id),
            ("ord", bi_ord),
            ("chr", bi_chr),
            ("hex", bi_hex),
            ("oct", bi_oct),
            ("bin", bi_bin),
            ("divmod", bi_divmod),
            ("round", bi_round),
            ("pow", bi_pow),
            ("getattr", bi_getattr),
            ("setattr", bi_setattr),
            ("delattr", bi_delattr),
            ("hasattr", bi_hasattr),
            ("callable", bi_callable),
            ("super", bi_super),
            ("type", bi_type),
            ("globals", bi_globals),
            ("vars", bi_vars),
            ("format", bi_format),
            ("property", bi_property),
            ("staticmethod", bi_staticmethod),
            ("classmethod", bi_classmethod),
        ];
        for (n, f) in natives {
            builtins.insert(n.into(), native(n, f));
        }
        let excs: Vec<(&str, Val)> = vec![
            ("BaseException", root.clone()),
            ("KeyboardInterrupt", ki.clone()),
            ("SystemExit", se.clone()),
            ("GeneratorExit", gen_exit.clone()),
            ("Exception", exc.clone()),
            ("StopIteration", stop_iteration.clone()),
            ("StopAsyncIteration", stop_async.clone()),
            ("ArithmeticError", arithmetic.clone()),
            ("ZeroDivisionError", zero_div.clone()),
            ("OverflowError", overflow.clone()),
            ("LookupError", lookup.clone()),
            ("IndexError", index_err.clone()),
            ("KeyError", key_err.clone()),
            ("NameError", name_err.clone()),
            ("UnboundLocalError", unbound.clone()),
            ("RuntimeError", runtime.clone()),
            ("NotImplementedError", not_impl.clone()),
            ("RecursionError", recursion.clone()),
            ("TypeError", type_err.clone()),
            ("ValueError", value_err.clone()),
            ("ImportError", import_err.clone()),
            ("ModuleNotFoundError", module_err.clone()),
            ("AttributeError", attr_err.clone()),
            ("AssertionError", assertion.clone()),
            ("BaseExceptionGroup", base_group.clone()),
            ("ExceptionGroup", group.clone()),
        ];
        for (n, c) in excs {
            builtins.insert(n.into(), c);
        }
        builtins.insert("abc".into(), abc_mod.clone());

        TestModel {
            builtins,
            class_object: object,
            class_type: type_cls,
            class_int: int,
            class_float: float,
            class_bool: bool_cls,
            class_str: str_cls,
            class_bytes: bytes,
            class_list: list,
            class_tuple: tuple,
            class_dict: dict,
            class_set: set,
            class_frozenset: frozenset,
            class_slice: slice,
            class_range: range,
            class_function: function,
            class_module: module_cls,
            class_generator: generator,
            class_coroutine: coroutine,
            class_asyncgen: asyncgen,
            class_traceback: traceback,
            class_none: none_type,
            class_ellipsis: ellipsis,
            class_property: property,
            class_staticmethod: staticmethod,
            class_classmethod: classmethod,
            class_super: super_cls,
            exc_base: exc,
            exc_root: root,
            exc_stop_iteration: stop_iteration,
            exc_generator_exit: gen_exit,
            exc_by_tag,
            exc_group: group,
            exc_base_group: base_group,
            typing_module: typing_mod,
            collections_module: collections_mod,
            abc_module: abc_mod,
            exc_stop_async: stop_async,
            captured: RefCell::new(None),
        }
    }

    /// Redirect `print` into an internal buffer; returns the buffer's
    /// contents and restores direct stdout output.
    pub fn take_output(&mut self) -> String {
        self.captured.borrow_mut().take().unwrap_or_default()
    }

    pub fn capture_output(&mut self) {
        *self.captured.borrow_mut() = Some(String::new());
    }
}

impl Default for TestModel {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ObjectModel
// ---------------------------------------------------------------------------

impl ObjectModel for TestModel {
    type Val = Val;

    fn none(&self) -> Val {
        v_none()
    }

    fn bool(&self, v: bool) -> Val {
        v_bool(v)
    }

    fn int(&self, v: i64) -> Val {
        v_int(v)
    }

    fn bigint(&self, decimal: &str) -> Val {
        Rc::new(Obj::Int(Rc::new(
            BigInt::parse(decimal).expect("decimal literal"),
        )))
    }

    fn float(&self, v: f64) -> Val {
        Rc::new(Obj::Float(v))
    }

    fn str(&self, v: &str) -> Val {
        v_str(v)
    }

    fn bytes(&self, v: &[u8]) -> Val {
        Rc::new(Obj::Bytes(Rc::from(v)))
    }

    fn ellipsis(&self) -> Val {
        Rc::new(Obj::Ellipsis)
    }

    fn tuple(&self, items: Vec<Val>) -> Val {
        Rc::new(Obj::Tuple(Rc::new(items)))
    }

    fn is_none(&self, v: &Val) -> bool {
        matches!(&**v, Obj::None_)
    }

    fn is_same(&self, a: &Val, b: &Val) -> bool {
        is_identical(a, b)
    }

    fn unbox_int(&self, v: &Val) -> Result<i64, UnboxError> {
        match &**v {
            Obj::Bool(b) => Ok(i64::from(*b)),
            Obj::Int(i) => i.to_i64().ok_or(UnboxError::Overflow),
            _ => Err(UnboxError::Type),
        }
    }

    fn unbox_float(&self, v: &Val) -> Result<f64, UnboxError> {
        match &**v {
            Obj::Bool(b) => Ok(f64::from(*b)),
            Obj::Int(i) => i.to_f64().ok_or(UnboxError::Overflow),
            Obj::Float(f) => Ok(*f),
            _ => Err(UnboxError::Type),
        }
    }

    fn unbox_bool(&self, v: &Val) -> Result<bool, UnboxError> {
        match &**v {
            Obj::Bool(b) => Ok(*b),
            _ => Err(UnboxError::Type),
        }
    }

    fn truth(&mut self, vm: &mut Interp<'_, Self>, v: &Val) -> Result<bool, Val> {
        truth_val(self, vm, v)
    }

    fn binop(
        &mut self,
        vm: &mut Interp<'_, Self>,
        op: BinOp,
        lhs: &Val,
        rhs: &Val,
    ) -> Result<Val, Val> {
        binop_val(self, vm, op, lhs, rhs)
    }

    fn unary(&mut self, vm: &mut Interp<'_, Self>, op: UnaryOp, v: &Val) -> Result<Val, Val> {
        unary_val(self, vm, op, v)
    }

    fn compare(
        &mut self,
        vm: &mut Interp<'_, Self>,
        op: CmpOp,
        lhs: &Val,
        rhs: &Val,
    ) -> Result<Val, Val> {
        compare_val(self, vm, op, lhs, rhs).map(v_bool)
    }

    fn contains(
        &mut self,
        vm: &mut Interp<'_, Self>,
        haystack: &Val,
        needle: &Val,
    ) -> Result<bool, Val> {
        contains_val(self, vm, haystack, needle)
    }

    fn getattr(&mut self, vm: &mut Interp<'_, Self>, obj: &Val, name: &str) -> Result<Val, Val> {
        getattr_val(self, vm, obj, name)
    }

    fn setattr(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Val,
        name: &str,
        value: &Val,
    ) -> Result<(), Val> {
        setattr_val(self, vm, obj, name, value)
    }

    fn delattr(&mut self, vm: &mut Interp<'_, Self>, obj: &Val, name: &str) -> Result<(), Val> {
        delattr_val(self, vm, obj, name)
    }

    fn getitem(&mut self, vm: &mut Interp<'_, Self>, obj: &Val, key: &Val) -> Result<Val, Val> {
        getitem_val(self, vm, obj, key)
    }

    fn setitem(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Val,
        key: &Val,
        value: &Val,
    ) -> Result<(), Val> {
        setitem_val(self, vm, obj, key, value)
    }

    fn delitem(&mut self, vm: &mut Interp<'_, Self>, obj: &Val, key: &Val) -> Result<(), Val> {
        delitem_val(self, vm, obj, key)
    }

    fn get_iter(&mut self, vm: &mut Interp<'_, Self>, obj: &Val) -> Result<Val, Val> {
        iter_val(self, vm, obj)
    }

    fn iter_next(&mut self, vm: &mut Interp<'_, Self>, iter: &Val) -> Result<Val, Val> {
        next_val(self, vm, iter)
    }

    fn iter_send(
        &mut self,
        vm: &mut Interp<'_, Self>,
        iter: &Val,
        value: Option<Val>,
    ) -> Result<Val, Val> {
        match &**iter {
            Obj::Generator(g) => gen_send(self, vm, g, &value.unwrap_or_else(v_none)),
            Obj::AsendAwaiter { agen, pending } => {
                let v = pending.take().or(value).unwrap_or_else(v_none);
                let Obj::Generator(g) = &**agen else {
                    return Err(err(
                        self,
                        ErrorTag::TypeError,
                        "asend() awaiter without an async generator",
                    ));
                };
                asend_step(self, vm, g, &v)
            }
            _ => next_val(self, vm, iter),
        }
    }

    fn iter_throw(&mut self, vm: &mut Interp<'_, Self>, iter: &Val, exc: Val) -> Result<Val, Val> {
        match &**iter {
            Obj::Generator(g) => gen_throw(self, vm, g, &exc),
            Obj::Instance(i) => {
                if let Some(t) = mro_attr(&i.class, "throw") {
                    let bound = bind(self, vm, &t, iter, Some(iter.clone()))?;
                    return call_val(self, vm, &bound, &[exc], &[]);
                }
                Err(exc)
            }
            _ => Err(exc),
        }
    }

    fn await_iter(&mut self, vm: &mut Interp<'_, Self>, v: &Val) -> Result<Val, Val> {
        match &**v {
            Obj::Generator(g) if g.kind == GenKind::Coroutine => Ok(v.clone()),
            Obj::AsendAwaiter { .. } => Ok(v.clone()),
            _ => {
                let aw = getattr_val(self, vm, v, "__await__")?;
                call_val(self, vm, &aw, &[], &[])
            }
        }
    }

    fn call(
        &mut self,
        vm: &mut Interp<'_, Self>,
        callee: &Val,
        args: &[Val],
        kwargs: &[(&str, Val)],
    ) -> Result<Val, Val> {
        call_val(self, vm, callee, args, kwargs)
    }

    fn call_ex(
        &mut self,
        vm: &mut Interp<'_, Self>,
        callee: &Val,
        args: &Val,
        kwargs: &Val,
    ) -> Result<Val, Val> {
        let args_v: Vec<Val> = match &**args {
            Obj::Tuple(t) => t.iter().cloned().collect(),
            _ => {
                return Err(err(
                    self,
                    ErrorTag::TypeError,
                    "argument after * must be an iterable, not a tuple-like value",
                ));
            }
        };
        let mut kw_v: Vec<(String, Val)> = Vec::new();
        if !matches!(&**kwargs, Obj::None_) {
            let items = match &**kwargs {
                Obj::Dict(d) => d.borrow().items(),
                _ => {
                    return Err(err(
                        self,
                        ErrorTag::TypeError,
                        "argument after ** must be a mapping, not a dict-like value",
                    ));
                }
            };
            for (k, v) in items {
                let Obj::Str(s) = &*k else {
                    return Err(err(self, ErrorTag::TypeError, "keywords must be strings"));
                };
                kw_v.push((s.to_string(), v));
            }
        }
        let kwargs_slice: Vec<(&str, Val)> =
            kw_v.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        call_val(self, vm, callee, &args_v, &kwargs_slice)
    }

    fn len(&mut self, vm: &mut Interp<'_, Self>, v: &Val) -> Result<i64, Val> {
        len_val(self, vm, v)
    }

    fn format(&mut self, vm: &mut Interp<'_, Self>, value: &Val, spec: &Val) -> Result<Val, Val> {
        format_val(self, vm, value, spec)
    }

    fn build_list(&self, items: Vec<Val>) -> Val {
        Rc::new(Obj::List(RefCell::new(items)))
    }

    fn build_tuple(&self, items: Vec<Val>) -> Val {
        Rc::new(Obj::Tuple(Rc::new(items)))
    }

    fn build_dict(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        items: Vec<(Val, Val)>,
    ) -> Result<Val, Val> {
        let mut d = Dict::default();
        for (k, v) in items {
            let h = py_hash(&k).map_err(|t| unhashable(self, &t))?;
            d.set_hash(k, h, v);
        }
        Ok(Rc::new(Obj::Dict(RefCell::new(d))))
    }

    fn build_set(&mut self, vm: &mut Interp<'_, Self>, items: Vec<Val>) -> Result<Val, Val> {
        let table = set_from_items(self, items)?;
        let _ = vm;
        Ok(Rc::new(Obj::Set(RefCell::new(table))))
    }

    fn build_str(&self, parts: Vec<Val>) -> Val {
        let mut out = String::new();
        for p in parts {
            match &*p {
                Obj::Str(s) => out.push_str(s),
                // the front end only feeds Str pieces; anything else is a
                // contract violation, not a value to coerce
                _ => panic!("BuildStr fed a non-string part"),
            }
        }
        v_str(&out)
    }

    fn build_slice(&self, start: Option<Val>, stop: Val, step: Option<Val>) -> Val {
        Rc::new(Obj::Slice {
            start,
            stop: Some(stop),
            step,
        })
    }

    fn unpack(
        &mut self,
        vm: &mut Interp<'_, Self>,
        src: &Val,
        n: u32,
        star: Option<u32>,
    ) -> Result<Vec<Val>, Val> {
        match star {
            None => {
                // `a, b = 5` words the failure as unpack, not iteration.
                let items =
                    drain_iter(self, vm, src).map_err(|e| noniterable_unpack(self, src, e))?;
                if (items.len() as u32) < n {
                    return Err(err(
                        self,
                        ErrorTag::ValueError,
                        &format!(
                            "not enough values to unpack (expected {}, got {})",
                            n,
                            items.len()
                        ),
                    ));
                }
                if (items.len() as u32) > n {
                    return Err(err(
                        self,
                        ErrorTag::ValueError,
                        &format!("too many values to unpack (expected {n})"),
                    ));
                }
                Ok(items)
            }
            Some(s) => {
                let before = s;
                let after = n - 1 - s;
                let it = iter_val(self, vm, src).map_err(|e| noniterable_unpack(self, src, e))?;
                let mut out = Vec::new();
                let mut got = 0u32;
                for _ in 0..before {
                    match next_val(self, vm, &it) {
                        Ok(v) => {
                            out.push(v);
                            got += 1;
                        }
                        Err(e) if is_exc_matches(self, &e, "StopIteration") => {
                            return Err(err(
                                self,
                                ErrorTag::ValueError,
                                &format!(
                                    "not enough values to unpack (expected at least {}, got {got})",
                                    n - 1
                                ),
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                }
                let mut mid: Vec<Val> = Vec::new();
                loop {
                    match next_val(self, vm, &it) {
                        Ok(v) => mid.push(v),
                        Err(e) if is_exc_matches(self, &e, "StopIteration") => break,
                        Err(e) => return Err(e),
                    }
                }
                got += mid.len() as u32;
                if (mid.len() as u32) < after {
                    return Err(err(
                        self,
                        ErrorTag::ValueError,
                        &format!(
                            "not enough values to unpack (expected at least {}, got {got})",
                            n - 1
                        ),
                    ));
                }
                let tail: Vec<Val> = if after > 0 {
                    mid.split_off(mid.len() - after as usize)
                } else {
                    Vec::new()
                };
                out.push(Rc::new(Obj::List(RefCell::new(mid))));
                out.extend(tail);
                Ok(out)
            }
        }
    }

    fn import(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        name: &str,
        fromlist: &Val,
        level: &Val,
    ) -> Result<Val, Val> {
        let level_n = match &**level {
            Obj::Int(i) => i.to_i64().unwrap_or(0),
            _ => 0,
        };
        if level_n > 0 {
            return Err(err(
                self,
                ErrorTag::ImportError,
                "attempted relative import with no known parent package",
            ));
        }
        let wants_sub = match &**fromlist {
            Obj::Tuple(t) => !t.is_empty(),
            Obj::List(l) => !l.borrow().is_empty(),
            _ => false,
        };
        let target = if wants_sub {
            name.to_owned()
        } else {
            name.split('.').next().unwrap_or(name).to_owned()
        };
        match target.as_str() {
            "typing" => Ok(self.typing_module.clone()),
            "collections" => Ok(self.collections_module.clone()),
            "collections.abc" => Ok(self.abc_module.clone()),
            _ => Err(err(
                self,
                ErrorTag::ModuleNotFoundError,
                &format!("No module named '{name}'"),
            )),
        }
    }

    fn star_names(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        module: &Val,
    ) -> Result<Vec<(String, Val)>, Val> {
        let Obj::Module(md) = &**module else {
            return Err(err(
                self,
                ErrorTag::TypeError,
                "import * only valid at module level",
            ));
        };
        let dict = md.dict.borrow();
        if let Some(all) = dict.get("__all__") {
            let items: Vec<Val> = match &**all {
                Obj::Tuple(t) => t.iter().cloned().collect(),
                Obj::List(l) => l.borrow().clone(),
                _ => Vec::new(),
            };
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let Obj::Str(s) = &*it else {
                    return Err(err(
                        self,
                        ErrorTag::TypeError,
                        "Item in __all__ must be str",
                    ));
                };
                let name = s.to_string();
                let v = dict.get(&name).cloned().unwrap_or_else(v_none);
                out.push((name, v));
            }
            return Ok(out);
        }
        Ok(dict
            .entries
            .iter()
            .filter(|(n, _)| !n.starts_with('_'))
            .cloned()
            .collect())
    }

    fn new_dict(&self) -> Val {
        Rc::new(Obj::Dict(RefCell::new(Dict::default())))
    }

    fn dict_get(&self, dict: &Val, key: &str) -> Option<Val> {
        match &**dict {
            Obj::Dict(d) => d.borrow().items().into_iter().find_map(|(k, v)| {
                if str_of(&k) == Some(key) {
                    Some(v)
                } else {
                    None
                }
            }),
            _ => None,
        }
    }

    fn dict_get_val(&self, dict: &Val, key: &Val) -> Option<Val> {
        match &**dict {
            Obj::Dict(d) => {
                let h = py_hash(key).ok()?;
                d.borrow().get_hash(key, h)
            }
            _ => None,
        }
    }

    fn dict_set(&mut self, dict: &Val, key: &str, value: Val) {
        if let Obj::Dict(d) = &**dict {
            let k = v_str(key);
            let h = py_hash(&k).unwrap_or(0);
            d.borrow_mut().set_hash(k, h, value);
        }
    }

    fn dict_del(&mut self, dict: &Val, key: &str) -> bool {
        dict_del_str(dict, key)
    }

    fn builtin(&self, name: &str) -> Option<Val> {
        self.builtins.get(name).cloned()
    }

    fn make_function(
        &mut self,
        vm: &mut Interp<'_, Self>,
        func: FnId,
        cells: Vec<Val>,
        defaults: Val,
        kwdefaults: Val,
    ) -> Val {
        let module = vm.module();
        let qualname = module
            .consts
            .sym_str(module.functions[func.0 as usize].qualname)
            .to_owned();
        let name = qualname.rsplit('.').next().unwrap_or(&qualname).to_owned();
        Rc::new(Obj::Function(Rc::new(FuncObj {
            ir: Some(IrFunction {
                func,
                cells,
                defaults,
                kwdefaults,
            }),
            native: None,
            name: RefCell::new(name),
            qualname: RefCell::new(qualname),
            attrs: RefCell::new(AttrMap::default()),
        })))
    }

    fn make_class(
        &mut self,
        vm: &mut Interp<'_, Self>,
        name: &str,
        body: &Val,
        bases: &[Val],
        kwds: &Val,
    ) -> Result<Val, Val> {
        let class_kwds: Vec<(String, Val)> = match &**kwds {
            Obj::Dict(d) if !d.borrow().is_empty() => d
                .borrow()
                .items()
                .into_iter()
                .filter_map(|(k, v)| str_of(&k).map(|s| (s.to_string(), v)))
                .collect(),
            _ => Vec::new(),
        };
        let Obj::Function(f) = &**body else {
            return Err(err(
                self,
                ErrorTag::TypeError,
                "__build_class__: func must be a function",
            ));
        };
        let Some(ir) = f.ir.clone() else {
            return Err(err(
                self,
                ErrorTag::TypeError,
                "__build_class__: func must be a function",
            ));
        };
        let ns = self.new_dict();
        let defaults = ir.defaults.clone();
        let kwdefaults = ir.kwdefaults.clone();
        let mut frame = vm.new_frame(
            self,
            ir.func,
            ir.cells.clone(),
            &[],
            &[],
            &defaults,
            &kwdefaults,
            Some(ns.clone()),
        )?;
        let ret = match vm.resume_frame(self, &mut frame, Resume::Start) {
            Ok(crate::interp::Flow::Done(v)) => v,
            Ok(crate::interp::Flow::Suspended(_)) => {
                return Err(err(
                    self,
                    ErrorTag::TypeError,
                    "__build_class__ body cannot be a generator",
                ));
            }
            Err(e) => return Err(e),
        };
        let entries: Vec<(String, Val)> = match &*ns {
            Obj::Dict(d) => d
                .borrow()
                .items()
                .into_iter()
                .filter_map(|(k, v)| str_of(&k).map(|s| (s.to_string(), v)))
                .collect(),
            _ => Vec::new(),
        };
        make_class_value(self, vm, name, bases, entries, &ret, &class_kwds)
    }

    fn new_cell(&self, initial: Option<Val>) -> Val {
        Rc::new(Obj::Cell(RefCell::new(initial)))
    }

    fn cell_get(&self, cell: &Val) -> Option<Val> {
        match &**cell {
            Obj::Cell(c) => c.borrow().clone(),
            _ => None,
        }
    }

    fn cell_set(&mut self, cell: &Val, value: Val) {
        if let Obj::Cell(c) = &**cell {
            *c.borrow_mut() = Some(value);
        }
    }

    fn is_cell(&self, v: &Val) -> bool {
        matches!(&**v, Obj::Cell(_))
    }

    fn tuple_items(&self, v: &Val) -> Option<Vec<Val>> {
        match &**v {
            Obj::Tuple(t) => Some(t.iter().cloned().collect()),
            _ => None,
        }
    }

    fn error(&self, tag: ErrorTag, msg: &str) -> Val {
        let class = self.exc_by_tag.get(&tag).cloned().unwrap_or_else(|| {
            self.exc_by_tag
                .get(&ErrorTag::RuntimeError)
                .cloned()
                .expect("RuntimeError registered")
        });
        new_instance(&class, vec![v_str(msg)], None)
    }

    fn stop_iteration(&self, value: Val) -> Val {
        stop_iteration_value(self, value)
    }

    fn is_class(&self, v: &Val) -> bool {
        matches!(&**v, Obj::Class(_))
    }

    fn is_exception_instance(&self, v: &Val) -> bool {
        is_exc_instance(self, v)
    }

    fn exc_matches(&self, exc: &Val, types: &Val) -> bool {
        exc_matches_val(self, exc, types)
    }

    fn set_exc_context(&mut self, exc: &Val, context: Option<Val>) {
        if let Obj::Instance(i) = &**exc
            && let Some(d) = i.exc.borrow_mut().as_mut()
        {
            d.context = context;
        }
    }

    fn set_exc_cause(&mut self, exc: &Val, cause: Option<Val>) {
        // Only called for an explicit `from` clause, which suppresses the
        // implicit context whether the cause is an exception or None.
        if let Obj::Instance(i) = &**exc
            && let Some(d) = i.exc.borrow_mut().as_mut()
        {
            d.cause = cause.clone();
            d.suppress_context = true;
        }
    }

    fn catch_star_split(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        src: &Val,
        types: &Val,
    ) -> Result<(bool, Val, Val), Val> {
        check_star_types(self, types)?;
        if matches!(&**src, Obj::None_) {
            return Ok((false, v_none(), v_none()));
        }
        let none = v_none();
        if exc_matches_val(self, src, types) {
            if is_group_inst(self, src) {
                return Ok((true, src.clone(), none));
            }
            // a naked exception: wrapped in a synthetic single-member group
            let wrapper = new_instance(
                &group_class_for(self, std::slice::from_ref(src)),
                vec![v_str(""), Rc::new(Obj::Tuple(Rc::new(vec![src.clone()])))],
                Some(GroupData {
                    message: v_str(""),
                    children: vec![src.clone()],
                    meta_root: Some(src.clone()),
                }),
            );
            return Ok((true, wrapper, none));
        }
        if is_group_inst(self, src) {
            let root = meta_root_of(src).unwrap_or_else(|| src.clone());
            let (matched, rest) = split_group_recursive(self, src, types, &root);
            let hit = matched.is_some();
            return Ok((
                hit,
                matched.unwrap_or_else(v_none),
                rest.unwrap_or_else(v_none),
            ));
        }
        Ok((false, none, src.clone()))
    }

    fn prep_reraise_star(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        orig: &Val,
        excs: &Val,
    ) -> Result<Val, Val> {
        let items: Vec<Val> = match &**excs {
            Obj::List(l) => l.borrow().clone(),
            _ => {
                return Err(err(
                    self,
                    ErrorTag::TypeError,
                    "PrepReraiseStar expects a list",
                ));
            }
        };
        let live: Vec<&Val> = items
            .iter()
            .filter(|e| !matches!(&***e, Obj::None_))
            .collect();
        if live.is_empty() {
            return Ok(v_none());
        }
        if !is_group_inst(self, orig) {
            // a naked exception was caught: at most one handler ran, so at
            // most one exception is left to raise
            return Ok((*live[0]).clone());
        }
        let mut raised: Vec<Val> = Vec::new();
        let mut reraised: Vec<Val> = Vec::new();
        for e in &live {
            if same_meta(self, e, orig) {
                reraised.push((*e).clone());
            } else {
                raised.push((*e).clone());
            }
        }
        let mut ids: Vec<usize> = Vec::new();
        for e in &reraised {
            collect_leaf_ids(e, &mut ids);
        }
        let root = meta_root_of(orig).unwrap_or_else(|| orig.clone());
        let reraised_eg = projection_recursive(self, orig, &ids, &root);
        if raised.is_empty() {
            return Ok(reraised_eg.unwrap_or_else(v_none));
        }
        let mut children = raised;
        if let Some(rg) = reraised_eg {
            children.push(rg);
        }
        if children.len() > 1 {
            let class = group_class_for(self, &children);
            return Ok(new_instance(
                &class,
                vec![v_str(""), Rc::new(Obj::Tuple(Rc::new(children.clone())))],
                Some(GroupData {
                    message: v_str(""),
                    children,
                    meta_root: None,
                }),
            ));
        }
        Ok(children.remove(0))
    }

    fn add_traceback(&mut self, exc: &Val, file: &str, line: u32, _qualname: &str) {
        if let Obj::Instance(i) = &**exc
            && let Some(d) = i.exc.borrow_mut().as_mut()
        {
            d.tb.push((file.to_owned(), line));
        }
    }

    fn format_uncaught(&self, exc: &Val) -> String {
        // the full chain, innermost cause first, as CPython's traceback
        // module prints it (without the source lines)
        let mut chain: Vec<Val> = Vec::new();
        let mut cur = exc.clone();
        while let Obj::Instance(i) = &*cur {
            let next = {
                let d = i.exc.borrow();
                let d = d.as_ref();
                if let Some(cause) = d.and_then(|d| d.cause.as_ref()) {
                    cause.clone()
                } else if d.map(|d| !d.suppress_context).unwrap_or(true)
                    && let Some(ctx) = d.and_then(|d| d.context.as_ref())
                {
                    ctx.clone()
                } else {
                    break;
                }
            };
            chain.push(next.clone());
            cur = next;
        }
        chain.reverse();
        chain.push(exc.clone());
        let mut out = String::new();
        for (i, e) in chain.iter().enumerate() {
            if i > 0 {
                out.push('\n');
                out.push_str("\nThe above exception was the cause of the following exception:\n");
                out.push('\n');
            }
            out.push_str("Traceback (most recent call last):\n");
            if let Obj::Instance(inst) = &**e
                && let Some(d) = inst.exc.borrow().as_ref()
            {
                for (file, line) in &d.tb {
                    out.push_str(&format!("  File \"{file}\", line {line}\n"));
                }
            }
            let name = match &**e {
                Obj::Instance(inst) => class_name(&inst.class),
                _ => "exception".to_string(),
            };
            let msg = match &**e {
                Obj::Instance(inst) => inst
                    .exc
                    .borrow()
                    .as_ref()
                    .map(|d| {
                        if let Some(g) = &d.group {
                            format!(
                                "{} ({} sub-exception{})",
                                str_of(&g.message).unwrap_or(""),
                                g.children.len(),
                                if g.children.len() == 1 { "" } else { "s" }
                            )
                        } else if d.args.is_empty() {
                            String::new()
                        } else {
                            let key_error = is_exc_matches(self, e, "KeyError");
                            exc_str(self, &d.args, key_error)
                        }
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            };
            if msg.is_empty() {
                out.push_str(&name);
            } else {
                out.push_str(&format!("{name}: {msg}"));
            }
        }
        out
    }

    fn alloc_instance(
        &mut self,
        _vm: &mut Interp<'_, Self>,
        cls: &Val,
        slots: u32,
    ) -> Result<Val, Val> {
        if !matches!(&**cls, Obj::Class(_)) {
            return Err(err(
                self,
                ErrorTag::TypeError,
                &format!(
                    "AllocInst class must be a class, not {}",
                    val_type_name(self, cls)
                ),
            ));
        }
        let inst = new_instance(cls, Vec::new(), None);
        if let Obj::Instance(i) = &*inst {
            for s in 0..slots {
                dict_set_str(&i.attrs, &format!("__slot{s}"), v_none());
            }
        }
        Ok(inst)
    }

    fn field_get(&self, obj: &Val, slot: u32) -> Result<Val, Val> {
        let name = format!("__slot{slot}");
        match &**obj {
            Obj::Instance(i) => {
                dict_get_str(&i.attrs, &name).ok_or_else(|| attr_error_static(self, obj, &name))
            }
            _ => Err(attr_error_static(self, obj, &name)),
        }
    }

    fn field_set(&mut self, obj: &Val, slot: u32, value: &Val) -> Result<(), Val> {
        let name = format!("__slot{slot}");
        match &**obj {
            Obj::Instance(i) => {
                dict_set_str(&i.attrs, &name, value.clone());
                Ok(())
            }
            _ => Err(attr_error_static(self, obj, &name)),
        }
    }

    fn type_matches(&self, vm: &Interp<'_, Self>, v: &Val, ty: &Type) -> Result<bool, UnboxError> {
        Ok(match ty {
            Type::Top | Type::Ext(_) | Type::Union(_) => true,
            Type::Bottom => false,
            Type::Int64 => matches!(&**v, Obj::Int(_) | Obj::Bool(_)),
            Type::BigInt => matches!(&**v, Obj::Int(_)),
            Type::Float64 => matches!(&**v, Obj::Float(_)),
            Type::Bool => matches!(&**v, Obj::Bool(_)),
            Type::None => matches!(&**v, Obj::None_),
            Type::Str => matches!(&**v, Obj::Str(_)),
            Type::Bytes => matches!(&**v, Obj::Bytes(_)),
            Type::Tuple(_) => matches!(&**v, Obj::Tuple(_)),
            Type::List => matches!(&**v, Obj::List(_)),
            Type::Dict => matches!(&**v, Obj::Dict(_)),
            Type::Set => matches!(&**v, Obj::Set(_) | Obj::FrozenSet(_)),
            Type::Func(_) => matches!(
                &**v,
                Obj::Function(_)
                    | Obj::Native(..)
                    | Obj::BoundMethod { .. }
                    | Obj::Class(_)
                    | Obj::StaticMethod(_)
                    | Obj::ClassMethod(_)
            ),
            Type::Module(_) => matches!(&**v, Obj::Module(_)),
            Type::Inst { cls, exact } => match vm.class_value(*cls) {
                Some(c) => {
                    if *exact {
                        match &**v {
                            Obj::Instance(i) => Rc::ptr_eq(&i.class, c),
                            _ => false,
                        }
                    } else {
                        match &**v {
                            Obj::Instance(i) => mro_has(&i.class, c),
                            _ => false,
                        }
                    }
                }
                None => false,
            },
            Type::Class(id) => match vm.class_value(*id) {
                Some(c) => match &**v {
                    Obj::Class(_) => self.is_same(v, c),
                    _ => false,
                },
                None => false,
            },
        })
    }
}

fn attr_error_static(m: &TestModel, obj: &Val, name: &str) -> Val {
    let class = m
        .exc_by_tag
        .get(&ErrorTag::AttributeError)
        .cloned()
        .expect("AttributeError registered");
    let tname = val_type_name(m, obj);
    new_instance(
        &class,
        vec![v_str(&format!(
            "'{tname}' object has no attribute '{name}'"
        ))],
        None,
    )
}

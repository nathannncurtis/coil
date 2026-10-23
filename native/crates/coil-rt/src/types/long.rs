//! `int`: CPython 3.12's `PyLongObject` — `lv_tag` plus 30-bit digits —
//! with arbitrary-precision arithmetic, int/string conversion under the
//! 3.12 digit limit, the modular numeric hash, and the `PyLong_*` C API
//! entry points (spec sections 8.2 and 8.5).
//!
//! The algorithms are CPython's (`longobject.c`): compact fast paths,
//! schoolbook add/sub/mul, Knuth-D division (`x_divrem`), floor/mod from
//! truncating divrem, the correctly rounded int-to-double paths
//! (`_PyLong_Frexp`, `long_true_divide`), and `_PyLong_Format`'s
//! conversions. Formatting produces bytes, not `str` objects: the `tp_repr`
//! slot and every str-returning entry stay unset until the unicode runtime
//! exists, and the `nb_divmod` slot until tuples do.

use core::cmp::Ordering;
use core::ffi::{c_char, c_int, c_longlong, c_ulonglong, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_LONG_SUBCLASS, PyNumberMethods, PyObject,
    PyTypeObject, c_long, c_ulong,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{
    _Py_TPFLAGS_MATCH_SELF, Py_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE, zero_type,
};
use crate::types::exc as exceptions;

/// Bits per digit (`PyLong_SHIFT`).
pub const SHIFT: usize = 30;
/// The digit base (`PyLong_BASE`).
pub const BASE: u64 = 1 << SHIFT;
/// The digit mask (`PyLong_MASK`).
pub const MASK: u32 = (BASE as u32) - 1;
/// Digits per decimal group (`_PyLong_DECIMAL_SHIFT`).
pub const DECIMAL_SHIFT: usize = 9;
/// The decimal group base (`_PyLong_DECIMAL_BASE`).
pub const DECIMAL_BASE: u32 = 1_000_000_000;

/// The tag's sign bits: negative=2, zero=1, positive=0 (`SIGN_MASK`).
const SIGN_MASK: usize = 3;
/// The zero sign encoding (`SIGN_ZERO`).
const SIGN_ZERO: usize = 1;
/// The negative sign encoding (`SIGN_NEGATIVE`).
const SIGN_NEGATIVE: usize = 2;
/// Tag bits below the digit count (`_PyLong_NON_SIZE_BITS`); bit 2 is
/// reserved for the immortality flag.
const NON_SIZE_BITS: usize = 3;

/// `PyLong_SHIFT` under its C name, for the exported `PyLong_GetInfo`.
pub const PyLong_SHIFT: usize = SHIFT;

/// The int/str conversion limit's guaranteed floor
/// (`_PY_LONG_MAX_STR_DIGITS_THRESHOLD`).
pub const MAX_STR_DIGITS_THRESHOLD: usize = 640;

/// The interpreter's int/str conversion limit
/// (`_PY_LONG_DEFAULT_MAX_STR_DIGITS`); zero disables the limit. The `sys`
/// module's `set_int_max_str_digits` writes here when it lands.
static MAX_STR_DIGITS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(4300);

/// The current int/str conversion limit (see `MAX_STR_DIGITS`).
pub fn max_str_digits() -> usize {
    MAX_STR_DIGITS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Set the int/str conversion limit, rejecting values CPython rejects:
/// anything but zero or at least `_PY_LONG_MAX_STR_DIGITS_THRESHOLD`.
/// Returns false with `ValueError` set on rejection.
pub fn set_max_str_digits(value: usize) -> bool {
    if value != 0 && value < MAX_STR_DIGITS_THRESHOLD {
        crate::tstate::set_string(
            exceptions::value_error(),
            b"maxstrdigits must be 0 or larger than 640",
        );
        return false;
    }
    MAX_STR_DIGITS.store(value, core::sync::atomic::Ordering::Relaxed);
    true
}

/// CPython 3.12's `PyLongObject`: the header, the sign-and-size tag, then
/// `ob_digit` — the digits of the magnitude, least significant first, each
/// `SHIFT` bits (Cython reads this layout directly; spec section 8.2).
#[repr(C)]
pub struct PyLongObject {
    pub ob_base: PyObject,
    pub lv_tag: usize,
    pub ob_digit: [u32; 1],
}

/// `_PyLong_TAG_FROM_SIGN_AND_SIZE`: the tag for `sign` (-1, 0, 1) and
/// `size` digits.
pub const fn tag_from_sign_and_size(sign: i32, size: usize) -> usize {
    (1 - sign) as usize | (size << NON_SIZE_BITS)
}

/// `_PyLong_IsCompact`: the value fits one digit with room to spare, so
/// add/subtract can work on the value directly.
///
/// # Safety
///
/// `v` must point to a `PyLongObject`.
pub unsafe fn is_compact(v: *const PyLongObject) -> bool {
    // SAFETY: the tag is at the object's fixed offset.
    unsafe { (*v).lv_tag < (2 << NON_SIZE_BITS) }
}

/// The number of significant digits (`_PyLong_DigitCount`).
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
pub unsafe fn digit_count(v: *const PyLongObject) -> usize {
    // SAFETY: the tag is at the object's fixed offset.
    unsafe { (*v).lv_tag >> NON_SIZE_BITS }
}

/// The value's sign: -1, 0 or 1 (`_PyLong_Sign`).
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
pub unsafe fn sign(v: *const PyLongObject) -> i32 {
    // SAFETY: the tag is at the object's fixed offset.
    unsafe { 1 - ((*v).lv_tag & SIGN_MASK) as i32 }
}

/// Whether the value is zero (`_PyLong_IsZero`).
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
pub unsafe fn is_zero(v: *const PyLongObject) -> bool {
    // SAFETY: the tag is at the object's fixed offset.
    unsafe { (*v).lv_tag & SIGN_MASK == SIGN_ZERO }
}

/// Whether the value is negative (`_PyLong_IsNegative`).
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
pub unsafe fn is_negative(v: *const PyLongObject) -> bool {
    // SAFETY: the tag is at the object's fixed offset.
    unsafe { (*v).lv_tag & SIGN_MASK == SIGN_NEGATIVE }
}

/// The digits of the magnitude, least significant first.
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
pub unsafe fn digits(v: *const PyLongObject) -> &'static [u32] {
    // SAFETY: `digit_count` entries of `ob_digit` are within the object.
    unsafe { core::slice::from_raw_parts((*v).ob_digit.as_ptr(), digit_count(v)) }
}

/// `medium_value`: the value of a compact int as a 64-bit integer.
///
/// # Safety
///
/// `v` must point to a compact `PyLongObject`.
pub unsafe fn compact_value(v: *const PyLongObject) -> i64 {
    // SAFETY: `_PyLong_IsNonNegativeCompact` checks the tag; the single
    // digit is within the object.
    unsafe {
        let sign = if (*v).lv_tag <= (1 << NON_SIZE_BITS) {
            1i64
        } else {
            -1i64
        };
        sign * (*v).ob_digit[0] as i64
    }
}

/// Whether `op` is an `int` (or subclass): `PyLong_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub unsafe fn is_long(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: the flag read is within the type object.
    !ty.is_null() && unsafe { (*ty).tp_flags } & Py_TPFLAGS_LONG_SUBCLASS != 0
}

/// Whether `x` fits a single digit (`is_medium_int`), compared unsigned the
/// way CPython does to include the negative boundary.
fn is_medium_int(x: i64) -> bool {
    let x_plus_mask = (x as u64).wrapping_add(MASK as u64);
    x_plus_mask < MASK as u64 + BASE
}

/// `_PyLong_New`: allocate an int with `size` digit slots, zero-filled,
/// sign pre-set for the caller to refine. Null with `MemoryError` set.
fn long_alloc(size: usize) -> *mut PyLongObject {
    let max_digits =
        (isize::MAX as usize - coil_rt_abi::cp312::long::OB_DIGIT) / core::mem::size_of::<u32>();
    if size > max_digits {
        crate::tstate::set_string(exceptions::overflow_error(), b"too many digits in integer");
        return core::ptr::null_mut();
    }
    let ndigits = size.max(1);
    let block = crate::alloc::alloc_block(coil_rt_abi::cp312::long::OB_DIGIT + 4 * ndigits);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    let v = block.cast::<PyLongObject>();
    // SAFETY: the fresh block is zeroed and exclusively ours.
    unsafe {
        (*v).lv_tag = tag_from_sign_and_size(if size != 0 { 1 } else { 0 }, size);
        header::c_init(&raw mut (*v).ob_base, &raw mut PyLong_Type);
    }
    v
}

/// Build an int from sign and magnitude digits, any order-normalised the
/// way `long_normalize` does: zero collapses to the zero singleton and
/// compact values go through `maybe_small_long`. Null with an error set.
///
/// # Safety
///
/// Every entry of `mag` must be below `BASE`.
unsafe fn from_mag(sign: i32, mag: &[u32]) -> *mut PyObject {
    // SAFETY: the digit writes below stay within the allocation.
    unsafe {
        let mut size = mag.len();
        while size > 0 && mag[size - 1] == 0 {
            size -= 1;
        }
        if size == 0 {
            return singletons::get_small_int(0);
        }
        if size == 1 {
            let value = mag[0] as i64 * sign as i64;
            if (singletons::SMALL_INT_MIN..=singletons::SMALL_INT_MAX).contains(&value) {
                return singletons::get_small_int(value);
            }
        }
        let v = long_alloc(size);
        if v.is_null() {
            return core::ptr::null_mut();
        }
        (*v).lv_tag = tag_from_sign_and_size(sign, size);
        core::ptr::copy_nonoverlapping(mag.as_ptr(), (*v).ob_digit.as_mut_ptr(), size);
        v.cast()
    }
}

/// `_PyLong_FromSTwoDigits`: a value that fits two digits.
fn from_stwo_digits(x: i64) -> *mut PyObject {
    if (singletons::SMALL_INT_MIN..=singletons::SMALL_INT_MAX).contains(&x) {
        return singletons::get_small_int(x);
    }
    if is_medium_int(x) {
        return from_medium(x);
    }
    from_i64(x)
}

/// `_PyLong_FromMedium`: a one-digit non-small value.
fn from_medium(x: i64) -> *mut PyObject {
    // SAFETY: a single in-range digit.
    unsafe { from_mag(if x < 0 { -1 } else { 1 }, &[x.unsigned_abs() as u32]) }
}

/// Split a full-width signed value into digits.
fn from_i64(x: i64) -> *mut PyObject {
    let sign = if x < 0 { -1 } else { 1 };
    let mut value = x.unsigned_abs() as u128;
    let mut digits = Vec::new();
    while value > 0 {
        digits.push((value & MASK as u128) as u32);
        value >>= SHIFT;
    }
    // SAFETY: every digit is masked to `MASK`.
    unsafe { from_mag(sign, &digits) }
}

// ------------------------------------------------------- magnitude helpers

/// Compare magnitudes (`long_compare`'s digit part).
pub(crate) fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

/// Add magnitudes (`x_add`).
fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (longer, shorter) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut z = Vec::with_capacity(longer.len() + 1);
    let mut carry: u64 = 0;
    for i in 0..shorter.len() {
        carry += longer[i] as u64 + shorter[i] as u64;
        z.push((carry & MASK as u64) as u32);
        carry >>= SHIFT;
    }
    for &d in &longer[shorter.len()..] {
        carry += d as u64;
        z.push((carry & MASK as u64) as u32);
        carry >>= SHIFT;
    }
    if carry != 0 {
        z.push(carry as u32);
    }
    z
}

/// Subtract magnitudes (`x_sub`); `a` must be at least `b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    debug_assert!(mag_cmp(a, b) != Ordering::Less);
    let mut z = Vec::with_capacity(a.len());
    let mut borrow: u32 = 0;
    for i in 0..b.len() {
        let diff = (a[i] as u64).wrapping_sub(b[i] as u64 + borrow as u64);
        z.push((diff & MASK as u64) as u32);
        borrow = ((diff >> SHIFT) & 1) as u32;
    }
    for &d in &a[b.len()..] {
        let diff = (d as u64).wrapping_sub(borrow as u64);
        z.push((diff & MASK as u64) as u32);
        borrow = ((diff >> SHIFT) & 1) as u32;
    }
    while let Some(&0) = z.last() {
        z.pop();
    }
    z
}

/// Multiply magnitudes (`x_mul`'s schoolbook path, which CPython uses below
/// the Karatsuba cutoff; the cutoff is a speed choice, not semantics).
fn mag_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut z = vec![0u32; a.len() + b.len()];
    for (i, &f) in a.iter().enumerate() {
        if f == 0 {
            continue;
        }
        let mut carry: u64 = 0;
        for (j, &g) in b.iter().enumerate() {
            carry += z[i + j] as u64 + f as u64 * g as u64;
            z[i + j] = (carry & MASK as u64) as u32;
            carry >>= SHIFT;
        }
        let mut k = i + b.len();
        while carry != 0 {
            carry += z[k] as u64;
            z[k] = (carry & MASK as u64) as u32;
            carry >>= SHIFT;
            k += 1;
        }
    }
    while let Some(&0) = z.last() {
        z.pop();
    }
    z
}

/// Shift digits left by `d` bits (`v_lshift`), `d < SHIFT`; returns the
/// bits shifted out of the top.
fn mag_lshift(z: &mut [u32], a: &[u32], d: usize) -> u32 {
    debug_assert!(d < SHIFT);
    let mut carry: u32 = 0;
    for (i, &x) in a.iter().enumerate() {
        let acc = ((x as u64) << d) | carry as u64;
        z[i] = (acc & MASK as u64) as u32;
        carry = (acc >> SHIFT) as u32;
    }
    carry
}

/// Shift digits right by `d` bits (`v_rshift`), `d < SHIFT`; returns the
/// bits shifted out of the bottom.
fn mag_rshift(z: &mut [u32], a: &[u32], d: usize) -> u32 {
    debug_assert!(d < SHIFT);
    let mask = (1u32 << d) - 1;
    let mut carry: u32 = 0;
    for i in (0..a.len()).rev() {
        let acc = ((carry as u64) << SHIFT) | a[i] as u64;
        carry = (acc as u32) & mask;
        z[i] = (acc >> d) as u32;
    }
    carry
}

/// The number of significant bits of a digit (`bit_length_digit`).
fn bit_length_digit(x: u32) -> usize {
    32 - x.leading_zeros() as usize
}

/// Divide a magnitude by a single digit (`inplace_divrem1`); returns the
/// remainder. `n` must be nonzero.
fn mag_divrem1(pout: &mut [u32], pin: &[u32], n: u32) -> u32 {
    debug_assert!(n > 0 && n <= MASK);
    let mut remainder: u64 = 0;
    for i in (0..pin.len()).rev() {
        let dividend = (remainder << SHIFT) | pin[i] as u64;
        pout[i] = (dividend / n as u64) as u32;
        remainder = dividend % n as u64;
    }
    remainder as u32
}

/// Unsigned division with remainder (`x_divrem`, Knuth's Algorithm D) for
/// `2 <= w.len() <= v.len()`. Returns `(quotient, remainder)`.
fn mag_divrem(v: &[u32], w: &[u32]) -> (Vec<u32>, Vec<u32>) {
    debug_assert!(w.len() >= 2 && v.len() >= w.len());
    // Normalize: shift w left so its top digit is >= BASE/2, and v by the
    // same amount.
    let d = SHIFT - bit_length_digit(w[w.len() - 1]);
    let size_w = w.len();
    let mut wn = vec![0u32; size_w];
    debug_assert!(mag_lshift(&mut wn, w, d) == 0);

    let mut vn = vec![0u32; v.len() + 1];
    let carry = mag_lshift(&mut vn[..v.len()], v, d);
    let mut size_v = v.len();
    if carry != 0 || vn[size_v - 1] >= wn[size_w - 1] {
        vn[size_v] = carry;
        size_v += 1;
    }

    // The quotient has at most k digits.
    let k = size_v - size_w;
    let mut a = vec![0u32; k];
    let wm1 = wn[size_w - 1] as u64;
    let wm2 = wn[size_w - 2] as u64;
    for pos in (0..k).rev() {
        // Divide vk[0..=size_w] by wn[0..size_w]: one quotient digit and
        // the remainder in place.
        let vk = &mut vn[pos..pos + size_w + 1];
        let vtop = vk[size_w] as u64;
        debug_assert!(vtop <= wm1);
        let vv = (vtop << SHIFT) | vk[size_w - 1] as u64;
        // The estimate may overestimate by one (rarely).
        let mut q = (vv / wm1) as u64;
        let mut r = (vv % wm1) as u64;
        while wm2 * q > ((r as u64) << SHIFT | vk[size_w - 2] as u64) {
            q -= 1;
            r += wm1;
            if r >= BASE {
                break;
            }
        }
        debug_assert!(q <= BASE);

        // Subtract q*wn from vk[0..=size_w].
        let mut zhi: i64 = 0;
        for i in 0..size_w {
            let z = vk[i] as i64 + zhi - (q as i64 * wn[i] as i64);
            vk[i] = (z as u64 & MASK as u64) as u32;
            zhi = z >> SHIFT;
        }
        // Add wn back if q was too large (rare).
        if vtop as i64 + zhi < 0 {
            let mut carry: u64 = 0;
            for i in 0..size_w {
                carry += vk[i] as u64 + wn[i] as u64;
                vk[i] = (carry & MASK as u64) as u32;
                carry >>= SHIFT;
            }
            q -= 1;
        }
        debug_assert!(q < BASE);
        a[pos] = q as u32;
    }

    // Unshift the remainder.
    let mut rem = vec![0u32; size_w];
    debug_assert!(mag_rshift(&mut rem, &vn[..size_w], d) == 0);
    while let Some(&0) = a.last() {
        a.pop();
    }
    while let Some(&0) = rem.last() {
        rem.pop();
    }
    (a, rem)
}

// ------------------------------------------------------ value operations

/// The operands of a slot call must both be ints, or the slot reports
/// `NotImplemented` for the other type's reflected slot.
///
/// # Safety
///
/// `a` and `b` must point to valid `PyObject`s.
unsafe fn check_binop(a: *mut PyObject, b: *mut PyObject) -> bool {
    // SAFETY: `PyLong_Check` only reads type flags.
    unsafe { is_long(a) && is_long(b) }
}

/// `long_add`/`_PyLong_Add`: signs decide between `mag_add` and `mag_sub`.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn add(a: *const PyLongObject, b: *const PyLongObject) -> *mut PyObject {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let (sa, sb) = (sign(a), sign(b));
        if sa == 0 {
            return from_mag(sb, digits(b));
        }
        if sb == 0 {
            return from_mag(sa, digits(a));
        }
        if (sa < 0) == (sb < 0) {
            let mag = mag_add(digits(a), digits(b));
            return from_mag(sa, &mag);
        }
        match mag_cmp(digits(a), digits(b)) {
            Ordering::Equal => singletons::get_small_int(0),
            Ordering::Greater => from_mag(sa, &mag_sub(digits(a), digits(b))),
            Ordering::Less => from_mag(sb, &mag_sub(digits(b), digits(a))),
        }
    }
}

/// `long_sub`/`_PyLong_Subtract`.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn sub(a: *const PyLongObject, b: *const PyLongObject) -> *mut PyObject {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let (sa, sb) = (sign(a), sign(b));
        if sa == 0 {
            return from_mag(-sb, digits(b));
        }
        if sb == 0 {
            return from_mag(sa, digits(a));
        }
        if (sa < 0) == (sb < 0) {
            match mag_cmp(digits(a), digits(b)) {
                Ordering::Equal => singletons::get_small_int(0),
                Ordering::Greater => from_mag(sa, &mag_sub(digits(a), digits(b))),
                Ordering::Less => from_mag(-sa, &mag_sub(digits(b), digits(a))),
            }
        } else {
            let mag = mag_add(digits(a), digits(b));
            from_mag(sa, &mag)
        }
    }
}

/// `long_mul`/`x_mul`, signs included.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn mul(a: *const PyLongObject, b: *const PyLongObject) -> *mut PyObject {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let sign = sign(a) * sign(b);
        let mag = mag_mul(digits(a), digits(b));
        from_mag(sign, &mag)
    }
}

/// Negate in place when the object is unshared; CPython's `_PyLong_Negate`.
///
/// `long_neg`.
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
unsafe fn neg(v: *const PyLongObject) -> *mut PyObject {
    // SAFETY: compact values fit `from_stwo_digits`; the rest are copied
    // with the sign bit flipped.
    unsafe {
        if is_compact(v) {
            return from_stwo_digits(-compact_value(v));
        }
        from_mag(-sign(v), digits(v))
    }
}

/// Truncating division of magnitudes (`long_divrem`): quotient sign is
/// `sign(a)*sign(b)`, remainder sign is `sign(a)`. Null with
/// `ZeroDivisionError` set.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn divrem(
    a: *const PyLongObject,
    b: *const PyLongObject,
) -> Option<(*mut PyObject, *mut PyObject)> {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let (da, db) = (digits(a), digits(b));
        if db.is_empty() {
            crate::tstate::set_string(
                exceptions::zero_division_error(),
                b"integer division or modulo by zero",
            );
            return None;
        }
        let (div_mag, rem_mag) = match mag_cmp(da, db) {
            Ordering::Less => (Vec::new(), da.to_vec()),
            Ordering::Equal => (vec![1], Vec::new()),
            Ordering::Greater => {
                if db.len() == 1 {
                    let mut q = vec![0u32; da.len()];
                    let r = mag_divrem1(&mut q, da, db[0]);
                    (q, if r == 0 { Vec::new() } else { vec![r] })
                } else {
                    let (q, r) = mag_divrem(da, db);
                    (q, r)
                }
            }
        };
        let div = from_mag(sign(a) * sign(b), &div_mag);
        let rem = from_mag(sign(a), &rem_mag);
        Some((div, rem))
    }
}

/// Floor division and modulo (`l_divmod`): adjust the truncating divrem so
/// the modulo takes the divisor's sign.
///
/// # Safety
///
/// `v` and `w` must point to normalized `PyLongObject`s.
unsafe fn l_divmod(
    v: *const PyLongObject,
    w: *const PyLongObject,
) -> Option<(*mut PyObject, *mut PyObject)> {
    // SAFETY: digit reads are within the objects.
    unsafe {
        // The single-digit fast paths (`fast_floor_div`, `fast_mod`) share
        // this sign arithmetic through the general path.
        let (div, rem) = divrem(v, w)?;
        if (is_negative(rem.cast()) && !is_negative(w.cast()))
            || (!is_negative(rem.cast()) && !is_zero(rem.cast()) && is_negative(w.cast()))
        {
            let adjusted = add(rem.cast(), w);
            if adjusted.is_null() {
                return None;
            }
            let one = singletons::get_small_int(1).cast::<PyLongObject>();
            let div_adj = sub(div.cast::<PyLongObject>(), one);
            if div_adj.is_null() {
                return None;
            }
            return Some((div_adj, adjusted));
        }
        Some((div, rem))
    }
}

/// Python's `%` (`l_mod`).
///
/// # Safety
///
/// `v` and `w` must point to normalized `PyLongObject`s.
unsafe fn l_mod(v: *const PyLongObject, w: *const PyLongObject) -> Option<*mut PyObject> {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let (da, db) = (digits(v), digits(w));
        if db.is_empty() {
            crate::tstate::set_string(exceptions::zero_division_error(), b"integer modulo by zero");
            return None;
        }
        let rem_mag = match mag_cmp(da, db) {
            Ordering::Less => da.to_vec(),
            Ordering::Equal => Vec::new(),
            Ordering::Greater => {
                if db.len() == 1 {
                    let mut q = vec![0u32; da.len()];
                    let r = mag_divrem1(&mut q, da, db[0]);
                    if r == 0 { Vec::new() } else { vec![r] }
                } else {
                    mag_divrem(da, db).1
                }
            }
        };
        let mut rem = from_mag(sign(v), &rem_mag);
        if is_negative(rem.cast()) && !is_zero(rem.cast()) && !is_negative(w.cast())
            || !is_negative(rem.cast()) && !is_zero(rem.cast()) && is_negative(w.cast())
        {
            let adjusted = add(rem.cast(), w);
            if adjusted.is_null() {
                return None;
            }
            rem = adjusted;
        }
        Some(rem)
    }
}

/// Shift a magnitude left by `SHIFT*wordshift + remshift` bits
/// (`long_lshift1`); the caller guarantees the shift count is nonnegative
/// and within range. Null with an error set.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`.
unsafe fn lshift1(a: *const PyLongObject, wordshift: usize, remshift: usize) -> *mut PyObject {
    // SAFETY: digit reads and writes stay within the two objects.
    unsafe {
        let da = digits(a);
        if wordshift == 0 && is_compact(a) {
            let m = compact_value(a);
            let x = if m < 0 {
                -((-m as i128) << remshift)
            } else {
                (m as i128) << remshift
            };
            return from_i128(
                x,
                if x < 0 {
                    -1
                } else if x > 0 {
                    1
                } else {
                    0
                },
            );
        }
        // `long_lshift1` allocates the result object before touching digits:
        // the digit cap raises OverflowError and the failed allocation a
        // MemoryError (`_PyLong_New`), where building a magnitude buffer
        // first would abort the process on the allocation instead.
        let newsize = wordshift + da.len() + usize::from(remshift != 0);
        let z = long_alloc(newsize);
        if z.is_null() {
            return core::ptr::null_mut();
        }
        {
            let dest = core::slice::from_raw_parts_mut((*z).ob_digit.as_mut_ptr(), newsize);
            // The `wordshift` low digits are the allocator's zeroes.
            let carry = mag_lshift(&mut dest[wordshift..wordshift + da.len()], da, remshift);
            if remshift != 0 {
                dest[wordshift + da.len()] = carry;
            }
        }
        // `long_normalize` plus `maybe_small_long`: trim high zeroes, then
        // collapse a small single digit onto the cache. A nonzero value
        // shifted left stays nonzero. The digits are addressed through the
        // pointer — `ob_digit` is declared as a one-element array.
        let sign_a = sign(a);
        let out_digits = (*z).ob_digit.as_ptr();
        let mut size = newsize;
        while size > 0 && *out_digits.add(size - 1) == 0 {
            size -= 1;
        }
        if size == 1 {
            let value = *out_digits as i64 * sign_a as i64;
            if (singletons::SMALL_INT_MIN..=singletons::SMALL_INT_MAX).contains(&value) {
                crate::alloc::free_block(z.cast());
                return singletons::get_small_int(value);
            }
        }
        (*z).lv_tag = tag_from_sign_and_size(sign_a, size);
        z.cast()
    }
}

/// Build an int from a value wider than two digits.
fn from_i128(x: i128, sign: i32) -> *mut PyObject {
    let mut mag = x.unsigned_abs();
    let mut digits = Vec::new();
    while mag > 0 {
        digits.push((mag & MASK as u128) as u32);
        mag >>= SHIFT;
    }
    // SAFETY: every digit is masked to `MASK`.
    unsafe { from_mag(sign, &digits) }
}

/// Shift right by `SHIFT*wordshift + remshift` bits (`long_rshift1`),
/// floor semantics for negative values.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`; `wordshift` and
/// `remshift` within `divmod_shift`'s bounds.
unsafe fn rshift1(a: *const PyLongObject, wordshift: usize, remshift: usize) -> *mut PyObject {
    // SAFETY: digit reads are within the object.
    unsafe {
        if is_compact(a) {
            let m = compact_value(a);
            let shift = if wordshift == 0 { remshift } else { SHIFT };
            let x = if m < 0 { !(!m >> shift) } else { m >> shift };
            return from_stwo_digits(x);
        }
        let a_negative = is_negative(a);
        let size_a = digit_count(a);
        let mut remshift = remshift;
        let mut wordshift = wordshift;
        if a_negative && remshift == 0 {
            if wordshift == 0 {
                let mag = digits(a).to_vec();
                return from_mag(sign(a), &mag);
            }
            remshift = SHIFT;
            wordshift -= 1;
        }
        let newsize = size_a as isize - wordshift as isize;
        if newsize <= 0 {
            return singletons::get_small_int(-(a_negative as i64));
        }
        let newsize = newsize as usize;
        let da = digits(a);
        let hishift = SHIFT - remshift;
        let mut accum: u64 = da[wordshift] as u64;
        if a_negative {
            // (-a) >> shift == -((a + 2**shift - 1) >> shift): the addend's
            // low digits are all MASK, so a carry appears exactly when one
            // of the shifted-out digits is nonzero.
            let sticky = da[..wordshift].iter().any(|&d| d != 0);
            accum += (MASK >> hishift) as u64 + sticky as u64;
        }
        accum >>= remshift;
        let mut z = vec![0u32; newsize];
        for i in 0..newsize - 1 {
            accum += (da[wordshift + 1 + i] as u64) << hishift;
            z[i] = (accum & MASK as u64) as u32;
            accum >>= SHIFT;
        }
        z[newsize - 1] = accum as u32;
        from_mag(if a_negative { -1 } else { 1 }, &z)
    }
}

/// The two's complement of a magnitude (`v_complement`).
fn mag_complement(z: &mut [u32], a: &[u32]) {
    let mut carry: u64 = 1;
    for i in 0..a.len() {
        carry += (a[i] ^ MASK) as u64;
        z[i] = (carry & MASK as u64) as u32;
        carry >>= SHIFT;
    }
}

/// `long_bitwise`: and/or/xor over two's-complement forms.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn bitwise(a: *const PyLongObject, op: u8, b: *const PyLongObject) -> *mut PyObject {
    // SAFETY: digit reads are within the objects.
    unsafe {
        let nega = is_negative(a);
        let negb = is_negative(b);
        let mut ta = digits(a).to_vec();
        let mut tb = digits(b).to_vec();
        if nega {
            let mut tc = vec![0u32; ta.len()];
            mag_complement(&mut tc, &ta);
            ta = tc;
        }
        if negb {
            let mut tc = vec![0u32; tb.len()];
            mag_complement(&mut tc, &tb);
            tb = tc;
        }
        // Keep `a` the longer operand, signs travelling with the digits.
        let (ta, tb, nega, negb) = if ta.len() < tb.len() {
            (tb, ta, negb, nega)
        } else {
            (ta, tb, nega, negb)
        };
        let size_a = ta.len();
        let size_b = tb.len();
        let (negz, size_z) = match op {
            b'^' => (nega ^ negb, size_a),
            b'&' => (nega & negb, if negb { size_a } else { size_b }),
            _ => (nega | negb, if negb { size_b } else { size_a }),
        };
        let mut tz = vec![0u32; size_z + negz as usize];
        for i in 0..size_b {
            tz[i] = match op {
                b'&' => ta[i] & tb[i],
                b'|' => ta[i] | tb[i],
                _ => ta[i] ^ tb[i],
            };
        }
        // The digits of `a` beyond `b` copy through, complemented for xor
        // when `b` is negative (its two's complement is all ones up there).
        for i in size_b..size_z {
            tz[i] = if op == b'^' && negb {
                ta[i] ^ MASK
            } else {
                ta[i]
            };
        }
        if negz {
            // The extra digit keeps the final complement from overflowing.
            tz[size_z] = MASK;
            let mut tc = vec![0u32; size_z + 1];
            mag_complement(&mut tc, &tz);
            tz = tc;
        }
        from_mag(if negz { -1 } else { 1 }, &tz)
    }
}

/// `long_compare`: negative when `a < b`.
///
/// # Safety
///
/// `a` and `b` must point to normalized `PyLongObject`s.
unsafe fn compare(a: *const PyLongObject, b: *const PyLongObject) -> Ordering {
    // SAFETY: digit reads are within the objects.
    unsafe {
        if is_compact(a) && is_compact(b) {
            return compact_value(a).cmp(&compact_value(b));
        }
        let sa = sign(a) as i64 * digit_count(a) as i64;
        let sb = sign(b) as i64 * digit_count(b) as i64;
        if sa != sb {
            return sa.cmp(&sb);
        }
        let diff = mag_cmp(digits(a), digits(b));
        if sign(a) < 0 { diff.reverse() } else { diff }
    }
}

/// `long_hash`: the compact value itself, or the rotation-based modular
/// hash over `2**61 - 1` (spec section 8.5).
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
unsafe extern "C" fn long_hash(v: *mut PyObject) -> isize {
    // SAFETY: digit reads are within the object.
    unsafe {
        let v = v.cast::<PyLongObject>();
        const MODULUS: u64 = (1 << 61) - 1;
        const BITS: u32 = 61;
        if is_compact(v) {
            let x = compact_value(v) as u64;
            return if x == u64::MAX { -2 } else { x as isize };
        }
        let mut x: u64 = 0;
        for &d in digits(v).iter().rev() {
            x = (x << SHIFT & MODULUS) | (x >> (BITS as usize - SHIFT));
            x += d as u64;
            if x >= MODULUS {
                x -= MODULUS;
            }
        }
        x = x.wrapping_mul(sign(v) as u64);
        if x == u64::MAX {
            x = u64::MAX - 1;
        }
        x as isize
    }
}

// ------------------------------------------------------------- conversions

/// `long_to_decimal_string_internal`: decimal digits grouped by
/// `DECIMAL_BASE` (Knuth TAOCP 4.4 Method 1b), with both 3.12 digit-limit
/// checks. Returns `None` with an error set.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`.
unsafe fn to_decimal_string(a: *const PyLongObject) -> Option<String> {
    // SAFETY: digit reads are within the object.
    unsafe {
        let size_a = digit_count(a);
        let negative = is_negative(a);

        // The quick pre-check of PR 96537: 10/3 >= log2(10).
        if size_a >= 10 * MAX_STR_DIGITS_THRESHOLD / (3 * SHIFT) + 2 {
            let max_str_digits = max_str_digits();
            if max_str_digits > 0 && max_str_digits / (3 * SHIFT) <= (size_a - 11) / 10 {
                limit_error_to_str(max_str_digits);
                return None;
            }
        }

        let mut pout: Vec<u32> = Vec::new();
        for &hi in digits(a).iter().rev() {
            let mut hi = hi as u64;
            for slot in pout.iter_mut() {
                let z = (*slot as u64) << SHIFT | hi;
                hi = z / DECIMAL_BASE as u64;
                *slot = (z - hi * DECIMAL_BASE as u64) as u32;
            }
            while hi != 0 {
                pout.push((hi % DECIMAL_BASE as u64) as u32);
                hi /= DECIMAL_BASE as u64;
            }
        }
        if pout.is_empty() {
            pout.push(0);
        }

        // Exact output length, and the exact digit-limit check.
        let mut strlen = usize::from(negative) + 1 + (pout.len() - 1) * DECIMAL_SHIFT;
        let mut tenpow = 10u32;
        let rem = pout[pout.len() - 1];
        while rem >= tenpow {
            tenpow *= 10;
            strlen += 1;
        }
        if strlen > MAX_STR_DIGITS_THRESHOLD {
            let max_str_digits = max_str_digits();
            let strlen_nosign = strlen - usize::from(negative);
            if max_str_digits > 0 && strlen_nosign > max_str_digits {
                limit_error_to_str(max_str_digits);
                return None;
            }
        }

        let mut out = Vec::with_capacity(strlen);
        for &top in &pout[..pout.len() - 1] {
            let mut rem = top;
            for _ in 0..DECIMAL_SHIFT {
                out.push(b'0' + (rem % 10) as u8);
                rem /= 10;
            }
        }
        let mut rem = pout[pout.len() - 1];
        loop {
            out.push(b'0' + (rem % 10) as u8);
            rem /= 10;
            if rem == 0 {
                break;
            }
        }
        if negative {
            out.push(b'-');
        }
        out.reverse();
        Some(String::from_utf8(out).expect("decimal digits are ASCII"))
    }
}

/// The to-string digit-limit error.
fn limit_error_to_str(max_str_digits: usize) {
    let message = format!(
        "Exceeds the limit ({max_str_digits} digits) for integer string conversion; \
use sys.set_int_max_str_digits() to increase the limit"
    );
    crate::tstate::set_string(exceptions::value_error(), message.as_bytes());
}

/// `long_format_binary`: bases 2, 8 and 16, digit accumulation in `accum`.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`.
unsafe fn format_binary(a: *const PyLongObject, base: u32, alternate: bool) -> Option<String> {
    // SAFETY: digit reads are within the object.
    unsafe {
        let bits = match base {
            16 => 4,
            8 => 3,
            2 => 1,
            _ => {
                crate::tstate::set_string(
                    exceptions::system_error(),
                    b"bad argument to internal function",
                );
                return None;
            }
        };
        let negative = is_negative(a);
        let size_a = digit_count(a);
        let mut out: Vec<u8> = Vec::new();
        let mut accum: u64 = 0;
        let mut accumbits: i32 = 0;
        for (i, &d) in digits(a).iter().enumerate() {
            accum |= (d as u64) << accumbits;
            accumbits += SHIFT as i32;
            loop {
                let cdigit = (accum & (base as u64 - 1)) as u8;
                out.push(if cdigit < 10 {
                    b'0' + cdigit
                } else {
                    b'a' + cdigit - 10
                });
                accumbits -= bits;
                accum >>= bits;
                if i < size_a - 1 {
                    if accumbits < bits {
                        break;
                    }
                } else if accum == 0 {
                    break;
                }
            }
        }
        if out.is_empty() {
            out.push(b'0');
        }
        if alternate {
            out.push(match base {
                16 => b'x',
                8 => b'o',
                _ => b'b',
            });
            out.push(b'0');
        }
        if negative {
            out.push(b'-');
        }
        out.reverse();
        Some(String::from_utf8(out).expect("binary digits are ASCII"))
    }
}

/// `_PyLong_Format`: decimal, or binary with the `0b`/`0o`/`0x` prefix.
/// Returns `None` with an error set.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`.
pub unsafe fn format(a: *const PyLongObject, base: u32, alternate: bool) -> Option<String> {
    // SAFETY: the converters only read the object's digits.
    unsafe {
        if base == 10 {
            to_decimal_string(a)
        } else {
            format_binary(a, base, alternate)
        }
    }
}

/// `_PyLong_DigitValue`: digit values of the 128 ASCII codes, 37 for
/// non-digits.
static DIGIT_VALUE: [u8; 128] = {
    let mut table = [37u8; 128];
    let mut c = b'0';
    while c <= b'9' {
        table[c as usize] = c - b'0';
        c += 1;
    }
    c = b'a';
    while c <= b'z' {
        table[c as usize] = c - b'a' + 10;
        c += 1;
    }
    c = b'A';
    while c <= b'Z' {
        table[c as usize] = c - b'A' + 10;
        c += 1;
    }
    table
};

/// The digit value of `c` when it is ASCII, else 37 (invalid).
fn digit_value(c: u8) -> u32 {
    if c < 128 {
        DIGIT_VALUE[c as usize] as u32
    } else {
        37
    }
}

/// `long_from_binary_base`: linear conversion for power-of-two bases over
/// `digits` digit characters (underscores already allowed for).
fn from_binary_base(digits: &[u8], base: u32) -> Vec<u32> {
    // Bits per character: floor(log2(base)) for a power-of-two base.
    let bits_per_char = (32 - base.leading_zeros() - 1) as usize;
    let mut z: Vec<u32> = Vec::new();
    let mut accum: u64 = 0;
    let mut bits_in_accum: usize = 0;
    for &c in digits.iter().rev() {
        if c == b'_' {
            continue;
        }
        let k = digit_value(c);
        accum |= (k as u64) << bits_in_accum;
        bits_in_accum += bits_per_char;
        if bits_in_accum >= SHIFT {
            z.push((accum & MASK as u64) as u32);
            accum >>= SHIFT;
            bits_in_accum -= SHIFT;
        }
    }
    if bits_in_accum != 0 {
        z.push(accum as u32);
    }
    z
}

/// `long_from_non_binary_base`: the quadratic conversion, `convwidth`
/// characters at a time.
fn from_non_binary_base(start: &[u8], base: u32) -> Option<Vec<u32>> {
    // The largest i with base**i <= BASE, and base**i itself.
    let mut convmax = base as u64;
    let mut convwidth = 1usize;
    loop {
        let next = convmax * base as u64;
        if next > BASE {
            break;
        }
        convmax = next;
        convwidth += 1;
    }
    let mut z: Vec<u32> = Vec::new();
    let mut p = 0usize;
    while p < start.len() {
        if start[p] == b'_' {
            p += 1;
            continue;
        }
        let mut c = digit_value(start[p]) as u64;
        p += 1;
        let mut i = 1usize;
        while i < convwidth && p < start.len() {
            if start[p] == b'_' {
                p += 1;
                continue;
            }
            i += 1;
            c = c * base as u64 + digit_value(start[p]) as u64;
            p += 1;
        }
        let mut convmult = convmax;
        if i != convwidth {
            convmult = base as u64;
            let mut j = i;
            while j > 1 {
                convmult *= base as u64;
                j -= 1;
            }
        }
        // z = z*convmult + c, digit by digit.
        for slot in z.iter_mut() {
            c += *slot as u64 * convmult;
            *slot = (c & MASK as u64) as u32;
            c >>= SHIFT;
        }
        if c != 0 {
            z.push(c as u32);
        }
    }
    while let Some(&0) = z.last() {
        z.pop();
    }
    Some(z)
}

/// `long_from_string_base` over a validated digit run: returns the
/// unsigned magnitude, or `None` with an error set.
fn from_string_base(digits: &[u8], base: u32, digit_count: usize) -> Option<Vec<u32>> {
    if base.count_ones() == 1 {
        return Some(from_binary_base(digits, base));
    }
    if digit_count > MAX_STR_DIGITS_THRESHOLD {
        let max_str_digits = max_str_digits();
        if max_str_digits > 0 && digit_count > max_str_digits {
            let message = format!(
                "Exceeds the limit ({max_str_digits} digits) for integer string \
conversion: value has {digit_count} digits; use sys.set_int_max_str_digits() \
to increase the limit"
            );
            crate::tstate::set_string(exceptions::value_error(), message.as_bytes());
            return None;
        }
    }
    from_non_binary_base(digits, base)
}

/// `PyLong_FromString`'s grammar over a NUL-terminated C string: optional
/// sign, optional base prefix, digits with single inner underscores,
/// trailing whitespace. Returns the int and, on request, the end pointer.
/// Null with an error set on failure.
///
/// # Safety
///
/// `str` must point to a valid null-terminated C string; `pend`, when not
/// null, is writable.
pub unsafe fn from_string_bytes(
    bytes: &[u8],
    base: c_int,
    consumed: Option<&mut usize>,
) -> *mut PyObject {
    // SAFETY: the parse only reads `bytes`; every error path sets the
    // pending exception.
    unsafe {
        if (base != 0 && base < 2) || base > 36 {
            crate::tstate::set_string(
                exceptions::value_error(),
                b"int() arg 2 must be >= 2 and <= 36",
            );
            return core::ptr::null_mut();
        }
        let mut base = base as u32;
        let mut sign: i32 = 1;
        let mut error_if_nonzero = false;
        let mut i = 0;
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'+' {
            i += 1;
        } else if i < bytes.len() && bytes[i] == b'-' {
            i += 1;
            sign = -1;
        }
        if base == 0 {
            let c0 = bytes.get(i).copied().unwrap_or(0);
            let c1 = bytes.get(i + 1).copied().unwrap_or(0);
            if c0 != b'0' {
                base = 10;
            } else if c1 == b'x' || c1 == b'X' {
                base = 16;
            } else if c1 == b'o' || c1 == b'O' {
                base = 8;
            } else if c1 == b'b' || c1 == b'B' {
                base = 2;
            } else {
                // A C-style octal literal: still zero, or an error.
                error_if_nonzero = true;
                base = 10;
            }
        }
        if bytes.get(i).copied().unwrap_or(b'\0') == b'0' {
            let c1 = bytes.get(i + 1).copied().unwrap_or(0);
            let prefixed = (base == 16 && (c1 == b'x' || c1 == b'X'))
                || (base == 8 && (c1 == b'o' || c1 == b'O'))
                || (base == 2 && (c1 == b'b' || c1 == b'B'));
            if prefixed {
                i += 2;
                // A single underscore is allowed right after the prefix.
                if bytes.get(i).copied() == Some(b'_') {
                    i += 1;
                }
            }
        }

        // The digit run, validated: digits and single inner underscores,
        // then trailing whitespace to the end. A leading underscore is a
        // syntax error (`long_from_string_base`'s first check).
        let start = i;
        if bytes.get(start) == Some(&b'_') {
            invalid_literal(bytes, base);
            if let Some(slot) = consumed {
                *slot = start;
            }
            return core::ptr::null_mut();
        }
        let mut prev = 0u8;
        let mut digits = 0usize;
        while i < bytes.len() && (digit_value(bytes[i]) < base || bytes[i] == b'_') {
            if bytes[i] == b'_' {
                if prev == b'_' {
                    break;
                }
            } else {
                digits += 1;
            }
            prev = bytes[i];
            i += 1;
        }
        let end = i;
        if prev == b'_' || start == end {
            invalid_literal(bytes, base);
            if let Some(slot) = consumed {
                *slot = end;
            }
            return core::ptr::null_mut();
        }
        let mut p = end;
        while p < bytes.len() && (bytes[p] as char).is_ascii_whitespace() {
            p += 1;
        }
        if p != bytes.len() {
            invalid_literal(bytes, base);
            if let Some(slot) = consumed {
                *slot = end;
            }
            return core::ptr::null_mut();
        }

        let mag = match from_string_base(&bytes[start..end], base, digits) {
            Some(mag) => mag,
            None => return core::ptr::null_mut(),
        };
        if error_if_nonzero && !mag.is_empty() {
            invalid_literal(bytes, 0);
            if let Some(slot) = consumed {
                *slot = end;
            }
            return core::ptr::null_mut();
        }
        if let Some(slot) = consumed {
            *slot = p;
        }
        from_mag(sign, &mag)
    }
}

/// The invalid-literal error over the raw input.
fn invalid_literal(bytes: &[u8], base: u32) {
    let mut shown: Vec<u8> = bytes.to_vec();
    if shown.len() > 200 {
        shown.truncate(200);
    }
    let message = format!(
        "invalid literal for int() with base {}: '{}'",
        base,
        String::from_utf8_lossy(&shown)
    );
    crate::tstate::set_string(exceptions::value_error(), message.as_bytes());
}

// ------------------------------------------------- float conversions, pow

/// `frexp` from the bit pattern: `x == m * 2**e` with 0.5 <= |m| < 1,
/// exact for finite doubles.
pub(crate) fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    if biased == 0 {
        // Subnormal: scaling by 2**54 makes it normal.
        let (m, e) = frexp(x.abs() * 18014398509481984.0);
        return (m * sign, e - 54);
    }
    // The mantissa keeps `x`'s sign bit; only the exponent is rebased.
    let mantissa = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (mantissa, biased - 1022)
}

/// `_PyLong_Frexp`: `a` as `x * 2**e` with 0.5 <= |x| < 1, `x` rounded to
/// 53 significant bits, ties to even. Sets `OverflowError` and returns
/// -1.0 for a bit count beyond `isize`.
///
/// # Safety
///
/// `a` must point to a normalized `PyLongObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_Frexp(a: *mut PyObject, e: *mut isize) -> f64 {
    // SAFETY: digit reads are within the object.
    unsafe {
        const MANT_DIG: usize = 53;
        const CORRECTION: [i32; 8] = [0, -1, -2, 1, 0, -1, 2, 1];
        let a = a.cast::<PyLongObject>();
        let a_size = digit_count(a);
        if a_size == 0 {
            *e = 0;
            return 0.0;
        }
        let mut a_bits = (a_size - 1) * SHIFT + bit_length_digit(digits(a)[a_size - 1]);
        if a_bits > isize::MAX as usize {
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"huge integer: number of bits overflows a Py_ssize_t",
            );
            *e = 0;
            return -1.0;
        }

        // The first MANT_DIG+2 bits of `a`, shifted into `x_digits`.
        let mut x_digits = [0u32; 2 + (MANT_DIG + 1) / SHIFT];
        let mut x_size;
        let da = digits(a);
        if a_bits <= MANT_DIG + 2 {
            let shift_digits = (MANT_DIG + 2 - a_bits) / SHIFT;
            let shift_bits = (MANT_DIG + 2 - a_bits) % SHIFT;
            let rem = mag_lshift(
                &mut x_digits[shift_digits..shift_digits + da.len()],
                da,
                shift_bits,
            );
            x_size = shift_digits + da.len();
            x_digits[x_size] = rem;
            x_size += 1;
        } else {
            let shift_digits = (a_bits - MANT_DIG - 2) / SHIFT;
            let shift_bits = (a_bits - MANT_DIG - 2) % SHIFT;
            let rem = mag_rshift(
                &mut x_digits[..da.len() - shift_digits],
                &da[shift_digits..],
                shift_bits,
            );
            x_size = da.len() - shift_digits;
            // Sticky: any shifted-out bit forces the low bit on.
            if rem != 0 || da[..shift_digits].iter().any(|&d| d != 0) {
                x_digits[0] |= 1;
            }
        }

        // Round half to even: the low digit to a multiple of 4, ties to 8.
        x_digits[0] = (x_digits[0] as i32 + CORRECTION[(x_digits[0] & 7) as usize]) as u32;
        let mut dx = x_digits[x_size - 1] as f64;
        let mut i = x_size - 1;
        while i > 0 {
            i -= 1;
            dx = dx * BASE as f64 + x_digits[i] as f64;
        }
        dx /= 4.0 * 9007199254740992.0;
        if dx == 1.0 {
            dx = 0.5;
            a_bits += 1;
        }
        *e = a_bits as isize;
        if is_negative(a) { -dx } else { dx }
    }
}

/// `PyLong_AsDouble`: the nearest double, half-to-even.
///
/// # Safety
///
/// `v` must point to a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsDouble(v: *mut PyObject) -> f64 {
    // SAFETY: the conversion only reads the object's digits.
    unsafe {
        if v.is_null() || !is_long(v) {
            crate::tstate::set_string(exceptions::type_error(), b"an integer is required");
            return -1.0;
        }
        let v = v.cast::<PyLongObject>();
        if is_compact(v) {
            return compact_value(v) as f64;
        }
        let mut exponent: isize = 0;
        let x = _PyLong_Frexp(v.cast(), &mut exponent);
        if (x == -1.0 && !crate::tstate::occurred().is_null()) || exponent > 1024 {
            crate::tstate::set_string(
                exceptions::overflow_error(),
                b"int too large to convert to float",
            );
            return -1.0;
        }
        ldexp(x, exponent as i32)
    }
}

/// `ldexp`, exact for in-range results. The exponent is applied in two
/// steps: one power-of-two multiply can overflow to infinity while the
/// true result is still finite (frexp hands out exponents up to
/// `DBL_MAX_EXP`).
pub(crate) fn ldexp(x: f64, e: i32) -> f64 {
    let half = e / 2;
    x * pow2(half) * pow2(e - half)
}

/// 2**e as a double, exact while representable.
fn pow2(e: i32) -> f64 {
    let mut result = 1.0f64;
    let mut base = 2.0f64;
    let mut n = e.unsigned_abs();
    while n > 0 {
        if n & 1 == 1 {
            result *= base;
        }
        base *= base;
        n >>= 1;
    }
    if e < 0 { 1.0 / result } else { result }
}

/// `long_true_divide`: correctly rounded int/int as a double, the integer
/// algorithm of 3.12 (`a / b`).
///
/// # Safety
///
/// `v` and `w` must point to normalized `PyLongObject`s.
unsafe fn true_divide(v: *const PyLongObject, w: *const PyLongObject) -> *mut PyObject {
    // SAFETY: digit reads are within the objects.
    unsafe {
        const MANT_DIG_DIGITS: usize = 53 / SHIFT;
        const MANT_DIG_BITS: usize = 53 % SHIFT;
        let (a_size, b_size) = (digit_count(v), digit_count(w));
        let negate = is_negative(v) != is_negative(w);
        if b_size == 0 {
            crate::tstate::set_string(exceptions::zero_division_error(), b"division by zero");
            return core::ptr::null_mut();
        }
        if a_size == 0 {
            return crate::types::float_::from_double(if negate { -0.0 } else { 0.0 });
        }
        let da = digits(v);
        let db = digits(w);
        let small = |d: &[u32]| {
            d.len() <= MANT_DIG_DIGITS
                || (d.len() == MANT_DIG_DIGITS + 1 && d[MANT_DIG_DIGITS] >> MANT_DIG_BITS == 0)
        };
        if small(da) && small(db) {
            // Both exactly representable: hardware division is correctly
            // rounded.
            let mut aa = da[da.len() - 1] as f64;
            for &d in da[..da.len() - 1].iter().rev() {
                aa = aa * BASE as f64 + d as f64;
            }
            let mut bb = db[db.len() - 1] as f64;
            for &d in db[..db.len() - 1].iter().rev() {
                bb = bb * BASE as f64 + d as f64;
            }
            let result = aa / bb;
            return crate::types::float_::from_double(if negate { -result } else { result });
        }

        // a_bits - b_bits decides the obvious overflow and underflow.
        let diff = (a_size as isize - b_size as isize) * SHIFT as isize
            + bit_length_digit(da[a_size - 1]) as isize
            - bit_length_digit(db[b_size - 1]) as isize;
        if diff > 1024 {
            overflow_true_divide();
            return core::ptr::null_mut();
        }
        if diff < -1021 - 53 - 1 {
            return crate::types::float_::from_double(if negate { -0.0 } else { 0.0 });
        }

        let shift = diff.max(-1021) - 53 - 2;
        let mut inexact = false;

        // x = abs(a * 2**-shift).
        let mut x: Vec<u32>;
        if shift <= 0 {
            let shift_digits = (-shift / SHIFT as isize) as usize;
            if a_size >= isize::MAX as usize - 1 - shift_digits {
                crate::tstate::set_string(
                    exceptions::overflow_error(),
                    b"intermediate overflow during division",
                );
                return core::ptr::null_mut();
            }
            let rem_bits = (-shift % SHIFT as isize) as usize;
            let mut z = vec![0u32; shift_digits + a_size + 1];
            let rem = mag_lshift(&mut z[shift_digits..shift_digits + a_size], da, rem_bits);
            z[shift_digits + a_size] = rem;
            x = z;
        } else {
            let shift_digits = (shift / SHIFT as isize) as usize;
            let rem_bits = (shift % SHIFT as isize) as usize;
            let mut z = vec![0u32; a_size - shift_digits];
            let rem = mag_rshift(&mut z, &da[shift_digits..], rem_bits);
            if rem != 0 {
                inexact = true;
            }
            if !inexact && da[..shift_digits].iter().any(|&d| d != 0) {
                inexact = true;
            }
            x = z;
        }
        while let Some(&0) = x.last() {
            x.pop();
        }

        // x //= b, tracking exactness.
        if db.len() == 1 {
            let mut q = vec![0u32; x.len()];
            let rem = mag_divrem1(&mut q, &x, db[0]);
            x = q;
            while let Some(&0) = x.last() {
                x.pop();
            }
            if rem != 0 {
                inexact = true;
            }
        } else {
            let (q, r) = mag_divrem(&x, db);
            if !r.is_empty() {
                inexact = true;
            }
            x = q;
        }
        let x_size = x.len();
        let x_bits = (x_size - 1) * SHIFT + bit_length_digit(x[x_size - 1]);

        // Round away the extra two or three bits, half to even.
        let bound = (-1021isize - shift).max(0) as usize;
        let extra_bits = x_bits.max(bound) - 53;
        debug_assert!(extra_bits == 2 || extra_bits == 3);
        let mask = 1u32 << (extra_bits - 1);
        let mut low = x[0] | inexact as u32;
        if (low & mask != 0) && (low & (3 * mask - 1)) != 0 {
            low += mask;
        }
        x[0] = low & !(2 * mask - 1);

        let mut dx = x[x_size - 1] as f64;
        for &d in x[..x_size - 1].iter().rev() {
            dx = dx * BASE as f64 + d as f64;
        }

        // Overflow check, then scale.
        if shift + x_bits as isize >= 1024
            && (shift + x_bits as isize > 1024 || dx == pow2(x_bits as i32))
        {
            overflow_true_divide();
            return core::ptr::null_mut();
        }
        let result = ldexp(dx, shift as i32);
        crate::types::float_::from_double(if negate { -result } else { result })
    }
}

/// The true-division overflow error.
fn overflow_true_divide() {
    crate::tstate::set_string(
        exceptions::overflow_error(),
        b"integer division result too large for a float",
    );
}

/// A signed magnitude value, for algorithms CPython expresses through
/// fresh `PyLongObject`s (`long_invmod`, `long_pow`).
#[derive(Clone)]
struct Signed {
    sign: i32,
    mag: Vec<u32>,
}

impl Signed {
    fn new(sign: i32, mut mag: Vec<u32>) -> Signed {
        while mag.last() == Some(&0) {
            mag.pop();
        }
        if mag.is_empty() {
            return Signed { sign: 0, mag };
        }
        Signed { sign, mag }
    }

    fn zero() -> Signed {
        Signed {
            sign: 0,
            mag: Vec::new(),
        }
    }

    fn one() -> Signed {
        Signed {
            sign: 1,
            mag: vec![1],
        }
    }

    fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    fn is_one(&self) -> bool {
        self.sign > 0 && self.mag.len() == 1 && self.mag[0] == 1
    }

    fn neg(&self) -> Signed {
        Signed {
            sign: -self.sign,
            mag: self.mag.clone(),
        }
    }

    fn add(&self, other: &Signed) -> Signed {
        if self.sign == 0 {
            return other.clone();
        }
        if other.sign == 0 {
            return self.clone();
        }
        if self.sign == other.sign {
            return Signed::new(self.sign, mag_add(&self.mag, &other.mag));
        }
        match mag_cmp(&self.mag, &other.mag) {
            Ordering::Equal => Signed::zero(),
            Ordering::Greater => Signed::new(self.sign, mag_sub(&self.mag, &other.mag)),
            Ordering::Less => Signed::new(other.sign, mag_sub(&other.mag, &self.mag)),
        }
    }

    fn sub(&self, other: &Signed) -> Signed {
        self.add(&other.neg())
    }

    fn mul(&self, other: &Signed) -> Signed {
        Signed::new(self.sign * other.sign, mag_mul(&self.mag, &other.mag))
    }

    /// Truncating division; `None` on a zero divisor.
    fn divrem(&self, other: &Signed) -> Option<(Signed, Signed)> {
        if other.is_zero() {
            return None;
        }
        let (q, r) = match mag_cmp(&self.mag, &other.mag) {
            Ordering::Less => (Vec::new(), self.mag.clone()),
            Ordering::Equal => (vec![1], Vec::new()),
            Ordering::Greater => {
                if other.mag.len() == 1 {
                    let mut q = vec![0u32; self.mag.len()];
                    let r = mag_divrem1(&mut q, &self.mag, other.mag[0]);
                    (q, if r == 0 { Vec::new() } else { vec![r] })
                } else {
                    mag_divrem(&self.mag, &other.mag)
                }
            }
        };
        Some((
            Signed::new(self.sign * other.sign, q),
            Signed::new(self.sign, r),
        ))
    }

    /// Python's `%`: the remainder with the divisor's sign.
    fn floor_mod(&self, other: &Signed) -> Option<Signed> {
        let (_, r) = self.divrem(other)?;
        if !r.is_zero() && r.sign != other.sign {
            Some(r.add(other))
        } else {
            Some(r)
        }
    }

    /// The object form.
    fn to_object(&self) -> *mut PyObject {
        // SAFETY: the digits came from the magnitude helpers, all < BASE.
        unsafe { from_mag(self.sign, &self.mag) }
    }
}

/// `long_invmod`: the inverse of `a` modulo positive `n`, or `None` with
/// `ValueError` set.
fn invmod(a: &Signed, n: &Signed) -> Option<Signed> {
    let mut a = a.clone();
    let mut n = n.clone();
    let mut b = Signed::one();
    let mut c = Signed::zero();
    while !n.is_zero() {
        let (q, r) = a.divrem(&n)?;
        a = n;
        n = r;
        let new_c = b.sub(&q.mul(&c));
        b = c;
        c = new_c;
    }
    if a.is_one() {
        Some(b)
    } else {
        crate::tstate::set_string(
            exceptions::value_error(),
            b"base is not invertible for the given modulus",
        );
        None
    }
}

/// `long_pow`: two-argument power (float result for a negative exponent)
/// and the three-argument modular power, including the negative-modulus
/// and modular-inverse conventions of CPython 3.12.
///
/// # Safety
///
/// `v`, `w` and `x` must point to valid `PyObject`s.
unsafe fn pow(v: *mut PyObject, w: *mut PyObject, x: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type checks.
    unsafe {
        if !check_binop(v, w) {
            return singletons::not_implemented();
        }
        let base = Signed::new(sign(v.cast()), digits(v.cast()).to_vec());
        let mut exponent = Signed::new(sign(w.cast()), digits(w.cast()).to_vec());
        let mut modulus = if is_long(x) {
            Some(Signed::new(sign(x.cast()), digits(x.cast()).to_vec()))
        } else if core::ptr::eq(x, singletons::none()) || x.is_null() {
            None
        } else {
            return singletons::not_implemented();
        };

        if exponent.sign < 0 && modulus.is_none() {
            // float_pow converts both operands to double.
            return crate::types::float_::float_pow(v, w, x);
        }
        let mut negative_output = false;
        if let Some(m) = modulus.as_mut() {
            if m.is_zero() {
                crate::tstate::set_string(
                    exceptions::value_error(),
                    b"pow() 3rd argument cannot be 0",
                );
                return core::ptr::null_mut();
            }
            if m.sign < 0 {
                negative_output = true;
                *m = m.neg();
            }
            if m.is_one() {
                return singletons::get_small_int(0);
            }
        }
        let mut base = base;
        if exponent.sign < 0 {
            exponent = exponent.neg();
            // SAFETY: `modulus` is present whenever the exponent was negative.
            let modulus = modulus.as_ref().unwrap_unchecked();
            match invmod(&base, modulus) {
                Some(inverse) => base = inverse,
                None => return core::ptr::null_mut(),
            }
        }
        if let Some(m) = modulus.as_ref()
            && (base.sign < 0 || mag_cmp(&base.mag, &m.mag) == Ordering::Greater)
        {
            match base.floor_mod(m) {
                Some(reduced) => base = reduced,
                None => return core::ptr::null_mut(),
            }
        }

        // Left-to-right binary exponentiation (HAC 14.79).
        let mut z = Signed::one();
        let mut bitlen = exponent.mag.len() * SHIFT;
        if !exponent.mag.is_empty() {
            bitlen -= SHIFT - bit_length_digit(*exponent.mag.last().unwrap());
        }
        for bit in (0..bitlen).rev() {
            z = z.mul(&z);
            if let Some(m) = modulus.as_ref() {
                match z.floor_mod(m) {
                    Some(reduced) => z = reduced,
                    None => return core::ptr::null_mut(),
                }
            }
            if exponent.mag[bit / SHIFT] >> (bit % SHIFT) & 1 == 1 {
                z = z.mul(&base);
                if let Some(m) = modulus.as_ref() {
                    match z.floor_mod(m) {
                        Some(reduced) => z = reduced,
                        None => return core::ptr::null_mut(),
                    }
                }
            }
        }
        if negative_output && !z.is_zero() {
            z = z.sub(modulus.as_ref().unwrap());
        }
        z.to_object()
    }
}

// ------------------------------------------------------------------ slots

macro_rules! long_slot_binop {
    ($name:ident, $op:expr) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            // SAFETY: the objects are read as ints after the type check.
            unsafe {
                if !check_binop(a, b) {
                    return singletons::not_implemented();
                }
                $op(a.cast(), b.cast())
            }
        }
    };
}

long_slot_binop!(long_add, add);
long_slot_binop!(long_sub, sub);
long_slot_binop!(long_mul, mul);

unsafe extern "C" fn long_neg(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is read as an int.
    unsafe { neg(v.cast()) }
}

unsafe extern "C" fn long_abs(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is read as an int; the absolute value keeps the
    // magnitude and drops the sign.
    unsafe {
        if is_negative(v.cast()) {
            neg(v.cast())
        } else {
            from_mag(1, digits(v.cast()))
        }
    }
}

unsafe extern "C" fn long_invert(v: *mut PyObject) -> *mut PyObject {
    // ~x is -(x+1).
    // SAFETY: the object is read as an int.
    unsafe {
        let v = v.cast::<PyLongObject>();
        if is_compact(v) {
            return from_stwo_digits(!compact_value(v));
        }
        let one = singletons::get_small_int(1).cast::<PyLongObject>();
        let x = add(v, one);
        if x.is_null() {
            return x;
        }
        neg(x.cast())
    }
}

unsafe extern "C" fn long_bool(v: *mut PyObject) -> c_int {
    // SAFETY: the tag read is within the object.
    unsafe { !is_zero(v.cast()) as c_int }
}

unsafe extern "C" fn long_div(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        l_divmod(a.cast(), b.cast())
            .map(|(div, _)| div)
            .unwrap_or(core::ptr::null_mut())
    }
}

unsafe extern "C" fn long_mod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        l_mod(a.cast(), b.cast()).unwrap_or(core::ptr::null_mut())
    }
}

unsafe extern "C" fn long_true_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        true_divide(a.cast(), b.cast())
    }
}

unsafe extern "C" fn long_pow(
    v: *mut PyObject,
    w: *mut PyObject,
    x: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type checks inside.
    unsafe { pow(v, w, x) }
}

/// `divmod_shift`: split a nonnegative shift count into whole digits and
/// leftover bits. A shift whose digit count reaches `isize::MAX/4` clips
/// there, as in CPython: a right shift of that much is 0 or -1 anyway, and
/// the left shift's allocation check raises.
///
/// # Safety
///
/// `shiftby` must point to a normalized nonnegative `PyLongObject`.
unsafe fn divmod_shift(shiftby: *const PyLongObject) -> (usize, usize) {
    // SAFETY: at most three digits are read.
    unsafe {
        let clip = isize::MAX as usize / core::mem::size_of::<u32>();
        if digit_count(shiftby) > 3 {
            return (clip, 0);
        }
        let mut total: u128 = 0;
        for &d in digits(shiftby).iter().rev() {
            total = total << SHIFT | d as u128;
        }
        if total <= isize::MAX as u128 {
            let total = total as usize;
            return (total / SHIFT, total % SHIFT);
        }
        let wordshift = total / SHIFT as u128;
        if wordshift < clip as u128 {
            (wordshift as usize, (total % SHIFT as u128) as usize)
        } else {
            (clip, 0)
        }
    }
}

unsafe extern "C" fn long_lshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        if is_negative(b.cast()) {
            crate::tstate::set_string(exceptions::value_error(), b"negative shift count");
            return core::ptr::null_mut();
        }
        if is_zero(a.cast()) {
            return singletons::get_small_int(0);
        }
        let (wordshift, remshift) = divmod_shift(b.cast());
        lshift1(a.cast(), wordshift, remshift)
    }
}

unsafe extern "C" fn long_rshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        if is_negative(b.cast()) {
            crate::tstate::set_string(exceptions::value_error(), b"negative shift count");
            return core::ptr::null_mut();
        }
        if is_zero(a.cast()) {
            return singletons::get_small_int(0);
        }
        let (wordshift, remshift) = divmod_shift(b.cast());
        rshift1(a.cast(), wordshift, remshift)
    }
}

unsafe extern "C" fn long_bitwise_slot(
    a: *mut PyObject,
    b: *mut PyObject,
    op: u8,
) -> *mut PyObject {
    // SAFETY: compact operands take the value fast path; the rest go
    // through the two's-complement form.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        let (x, y) = (a.cast::<PyLongObject>(), b.cast::<PyLongObject>());
        if is_compact(x) && is_compact(y) {
            let (m, n) = (compact_value(x), compact_value(y));
            let value = match op {
                b'&' => m & n,
                b'|' => m | n,
                _ => m ^ n,
            };
            return from_stwo_digits(value);
        }
        bitwise(x, op, y)
    }
}

unsafe extern "C" fn long_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: see `long_bitwise_slot`.
    unsafe { long_bitwise_slot(a, b, b'&') }
}

unsafe extern "C" fn long_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: see `long_bitwise_slot`.
    unsafe { long_bitwise_slot(a, b, b'^') }
}

unsafe extern "C" fn long_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: see `long_bitwise_slot`.
    unsafe { long_bitwise_slot(a, b, b'|') }
}

unsafe extern "C" fn long_float(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: `PyLong_AsDouble` only reads the object's digits.
    unsafe {
        let x = PyLong_AsDouble(v);
        if x == -1.0 && !crate::tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        crate::types::float_::from_double(x)
    }
}

unsafe extern "C" fn long_long_op(v: *mut PyObject) -> *mut PyObject {
    // `int.__int__`: an exact int is itself; a subclass copies its value.
    // SAFETY: the digits are read as the int they are.
    unsafe { from_mag(sign(v.cast()), digits(v.cast())) }
}

unsafe extern "C" fn long_richcompare(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: the objects are read as ints after the type check.
    unsafe {
        if !check_binop(a, b) {
            return singletons::not_implemented();
        }
        let result = if core::ptr::eq(a, b) {
            Ordering::Equal
        } else {
            compare(a.cast(), b.cast())
        };
        let truth = match op {
            Py_LT => result == Ordering::Less,
            Py_LE => result != Ordering::Greater,
            Py_EQ => result == Ordering::Equal,
            Py_NE => result != Ordering::Equal,
            Py_GT => result == Ordering::Greater,
            Py_GE => result != Ordering::Less,
            _ => return singletons::not_implemented(),
        };
        if truth {
            singletons::true_()
        } else {
            singletons::false_()
        }
    }
}

unsafe extern "C" fn long_dealloc(object: *mut PyObject) {
    // A stray decref on a small int resets its immortal count; every other
    // int frees through the type's `tp_free`.
    // SAFETY: the header and tag reads are within the object.
    unsafe {
        let v = object.cast::<PyLongObject>();
        if is_compact(v) {
            let ival = compact_value(v);
            if (singletons::SMALL_INT_MIN..=singletons::SMALL_INT_MAX).contains(&ival)
                && core::ptr::eq(object, singletons::get_small_int(ival))
            {
                (*object).ob_refcnt = header::IMMORTAL_REFCNT;
                return;
            }
        }
        // SAFETY: the object came from the allocator behind
        // `PyObject_Malloc`, so `PyObject_Free`'s contract holds.
        crate::alloc::pymem::PyObject_Free(object.cast());
    }
}

// ------------------------------------------------------------ type object

const fn long_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"int".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::long::OB_DIGIT as isize;
    t.tp_itemsize = core::mem::size_of::<u32>() as isize;
    t.tp_dealloc = Some(long_dealloc);
    t.tp_as_number = &raw mut LONG_AS_NUMBER;
    t.tp_hash = Some(long_hash);
    t.tp_richcompare = Some(long_richcompare);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_LONG_SUBCLASS
        | _Py_TPFLAGS_MATCH_SELF;
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// `int`'s number slots: everything CPython's `long_as_number` defines
/// except `nb_divmod`, which needs tuples and arrives with them.
static mut LONG_AS_NUMBER: PyNumberMethods = PyNumberMethods {
    nb_add: Some(long_add),
    nb_subtract: Some(long_sub),
    nb_multiply: Some(long_mul),
    nb_remainder: Some(long_mod),
    nb_divmod: None,
    nb_power: Some(long_pow),
    nb_negative: Some(long_neg),
    nb_positive: Some(long_long_op),
    nb_absolute: Some(long_abs),
    nb_bool: Some(long_bool),
    nb_invert: Some(long_invert),
    nb_lshift: Some(long_lshift),
    nb_rshift: Some(long_rshift),
    nb_and: Some(long_and),
    nb_xor: Some(long_xor),
    nb_or: Some(long_or),
    nb_int: Some(long_long_op),
    nb_reserved: core::ptr::null_mut(),
    nb_float: Some(long_float),
    nb_inplace_add: None,
    nb_inplace_subtract: None,
    nb_inplace_multiply: None,
    nb_inplace_remainder: None,
    nb_inplace_power: None,
    nb_inplace_lshift: None,
    nb_inplace_rshift: None,
    nb_inplace_and: None,
    nb_inplace_xor: None,
    nb_inplace_or: None,
    nb_floor_divide: Some(long_div),
    nb_true_divide: Some(long_true_divide),
    nb_inplace_floor_divide: None,
    nb_inplace_true_divide: None,
    nb_index: Some(long_long_op),
    nb_matrix_multiply: None,
    nb_inplace_matrix_multiply: None,
};

/// The `int` type object (CPython's `PyLong_Type`). `tp_repr` and `tp_new`
/// stay unset until `str` objects and argument parsing exist; readying
/// marks the type non-instantiable, which the str task lifts.
#[unsafe(no_mangle)]
pub static mut PyLong_Type: PyTypeObject = long_type();

// ----------------------------------------------------------- C API entry

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromLong(ival: c_long) -> *mut PyObject {
    from_i64(ival as i64)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromLongLong(ival: c_longlong) -> *mut PyObject {
    from_i64(ival)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromUnsignedLong(ival: c_ulong) -> *mut PyObject {
    from_u64(ival as u64)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromUnsignedLongLong(ival: c_ulonglong) -> *mut PyObject {
    from_u64(ival)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromSsize_t(ival: isize) -> *mut PyObject {
    from_i64(ival as i64)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromSize_t(ival: usize) -> *mut PyObject {
    from_u64(ival as u64)
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` is passed through as a bit pattern; the value is not dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromVoidPtr(p: *const c_void) -> *mut PyObject {
    from_u64(p as usize as u64)
}

fn from_u64(ival: u64) -> *mut PyObject {
    let mut value = ival as u128;
    let mut digits = Vec::new();
    while value > 0 {
        digits.push((value & MASK as u128) as u32);
        value >>= SHIFT;
    }
    // SAFETY: every digit is masked to `MASK`.
    unsafe { from_mag(1, &digits) }
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_FromDouble(dval: f64) -> *mut PyObject {
    // The fast path needs `long`'s 32-bit Windows width: anything within
    // ±(2**31) truncates through `FromLong`.
    if (-2147483648.0..2147483648.0).contains(&dval) {
        return PyLong_FromLong(dval as c_long);
    }
    if dval.is_infinite() {
        crate::tstate::set_string(
            exceptions::overflow_error(),
            b"cannot convert float infinity to integer",
        );
        return core::ptr::null_mut();
    }
    if dval.is_nan() {
        crate::tstate::set_string(
            exceptions::value_error(),
            b"cannot convert float NaN to integer",
        );
        return core::ptr::null_mut();
    }
    let neg = dval < 0.0;
    let dval = dval.abs();
    let (frac, expo) = frexp(dval);
    let ndig = ((expo - 1) / SHIFT as i32) as usize + 1;
    let mut digits = vec![0u32; ndig];
    // dval = frac*2**expo; scale frac so each step yields one digit.
    let mut frac = frac * pow2((expo - 1) % SHIFT as i32 + 1);
    for slot in digits.iter_mut().rev() {
        let bits = frac as u32;
        *slot = bits;
        frac -= bits as f64;
        frac = ldexp(frac, SHIFT as i32);
    }
    // SAFETY: the digits are the integer parts of exact powers of two.
    unsafe { from_mag(if neg { -1 } else { 1 }, &digits) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `str` must point to a valid null-terminated C string; `pend`, when not
/// null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromString(
    str_: *const c_char,
    pend: *mut *mut c_char,
    base: c_int,
) -> *mut PyObject {
    if str_.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the caller guarantees a null-terminated C string.
    let len = unsafe { core::ffi::CStr::from_ptr(str_).to_bytes().len() };
    // SAFETY: `len` bytes are readable per the C string contract.
    let bytes = unsafe { core::slice::from_raw_parts(str_.cast::<u8>(), len) };
    let mut consumed = 0usize;
    // SAFETY: the parse reads only `bytes` and reports errors through the
    // pending exception.
    let result = unsafe { from_string_bytes(bytes, base, Some(&mut consumed)) };
    if !pend.is_null() {
        // SAFETY: the caller guarantees a writable pointer.
        unsafe {
            *pend = str_.cast::<u8>().add(consumed) as *mut c_char;
        }
    }
    result
}

/// The magnitude as a `u128`, for the extractor range checks. `None` when
/// the value needs more than 128 bits.
///
/// # Safety
///
/// `v` must point to a normalized `PyLongObject`.
unsafe fn magnitude_u128(v: *const PyLongObject) -> Option<u128> {
    // SAFETY: digit reads are within the object.
    unsafe {
        let mut x: u128 = 0;
        for &d in digits(v).iter().rev() {
            x = x.checked_shl(SHIFT as u32)?.checked_add(d as u128)?;
        }
        Some(x)
    }
}

/// TypeError for an extractor handed a non-int.
fn not_an_integer() {
    crate::tstate::set_string(exceptions::type_error(), b"an integer is required");
}

/// `PyLong_AsLongAndOverflow`'s shared body over a 64-bit range.
///
/// # Safety
///
/// `vv` must point to a valid `PyObject`.
unsafe fn as_i64_and_overflow(vv: *mut PyObject, overflow: *mut c_int) -> i64 {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        *overflow = 0;
        if !is_long(vv) {
            type_error_cannot_interpret(vv);
            return -1;
        }
        let v = vv.cast::<PyLongObject>();
        if is_compact(v) {
            return compact_value(v);
        }
        let sign = sign(v) as i64;
        let Some(mag) = magnitude_u128(v) else {
            *overflow = sign as c_int;
            return -1;
        };
        if mag > i64::MAX as u128 && !(sign < 0 && mag == 1u128 << 63) {
            *overflow = sign as c_int;
            return -1;
        }
        (mag as i64).wrapping_mul(sign)
    }
}

/// The "'X' object cannot be interpreted as an integer" error.
///
/// # Safety
///
/// `vv` must point to a valid `PyObject`.
unsafe fn type_error_cannot_interpret(vv: *mut PyObject) {
    // SAFETY: only the type's name is read.
    unsafe {
        let ty = header::type_of(vv);
        let name = if ty.is_null() {
            "?".to_string()
        } else {
            format!("'{}'", crate::util::cstr((*ty).tp_name))
        };
        let message = format!("{name} object cannot be interpreted as an integer");
        crate::tstate::set_string(exceptions::type_error(), message.as_bytes());
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`; `overflow`, when not null, must point to writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLongAndOverflow(
    vv: *mut PyObject,
    overflow: *mut c_int,
) -> c_long {
    // SAFETY: the shared body reads the object as an int.
    let result = unsafe { as_i64_and_overflow(vv, overflow) };
    // SAFETY: `overflow` is the caller's out-pointer.
    let (overflow_flag, fits) =
        unsafe { (*overflow != 0, (-2147483648..=2147483647).contains(&result)) };
    if !fits {
        if !overflow_flag {
            // The value fits 64 bits but not the 32-bit C long.
            // SAFETY: `overflow` is the caller's out-pointer.
            unsafe {
                *overflow = if result > 0 { 1 } else { -1 };
            }
        }
        return -1;
    }
    result as c_long
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLong(obj: *mut PyObject) -> c_long {
    let mut overflow = 0;
    // SAFETY: the shared body reads the object as an int.
    let result = unsafe { PyLong_AsLongAndOverflow(obj, &mut overflow) };
    if overflow != 0 {
        crate::tstate::set_string(
            exceptions::overflow_error(),
            b"Python int too large to convert to C long",
        );
    }
    result
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`; `overflow`, when not null, must point to writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLongLongAndOverflow(
    vv: *mut PyObject,
    overflow: *mut c_int,
) -> c_longlong {
    // SAFETY: the shared body reads the object as an int.
    unsafe { as_i64_and_overflow(vv, overflow) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLongLong(obj: *mut PyObject) -> c_longlong {
    let mut overflow = 0;
    // SAFETY: the shared body reads the object as an int.
    let result = unsafe { PyLong_AsLongLongAndOverflow(obj, &mut overflow) };
    if overflow != 0 {
        crate::tstate::set_string(
            exceptions::overflow_error(),
            b"Python int too large to convert to C long long",
        );
    }
    result
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsSsize_t(vv: *mut PyObject) -> isize {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        if vv.is_null() || !is_long(vv) {
            not_an_integer();
            return -1;
        }
        let v = vv.cast::<PyLongObject>();
        if is_compact(v) {
            return compact_value(v) as isize;
        }
        let Some(mag) = magnitude_u128(v) else {
            overflow_error_c("Python int too large to convert to C ssize_t");
            return -1;
        };
        if mag <= isize::MAX as u128 {
            (mag as i64 * sign(v) as i64) as isize
        } else if sign(v) < 0 && mag == 1u128 << 63 {
            isize::MIN
        } else {
            overflow_error_c("Python int too large to convert to C ssize_t");
            -1
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsSize_t(vv: *mut PyObject) -> usize {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        if vv.is_null() || !is_long(vv) {
            not_an_integer();
            return usize::MAX;
        }
        let v = vv.cast::<PyLongObject>();
        if is_compact(v) {
            if is_negative(v) {
                overflow_error_c("can't convert negative value to size_t");
                return usize::MAX;
            }
            return compact_value(v) as usize;
        }
        if is_negative(v) {
            overflow_error_c("can't convert negative value to size_t");
            return usize::MAX;
        }
        match magnitude_u128(v) {
            Some(mag) if mag <= usize::MAX as u128 => mag as usize,
            _ => {
                overflow_error_c("Python int too large to convert to C size_t");
                usize::MAX
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsUnsignedLong(vv: *mut PyObject) -> c_ulong {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        if vv.is_null() || !is_long(vv) {
            not_an_integer();
            return c_ulong::MAX;
        }
        let v = vv.cast::<PyLongObject>();
        if is_negative(v) {
            overflow_error_c("can't convert negative value to unsigned int");
            return c_ulong::MAX;
        }
        match magnitude_u128(v) {
            Some(mag) if mag <= u32::MAX as u128 => mag as c_ulong,
            _ => {
                overflow_error_c("Python int too large to convert to C unsigned long");
                c_ulong::MAX
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsUnsignedLongLong(vv: *mut PyObject) -> c_ulonglong {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        if vv.is_null() || !is_long(vv) {
            not_an_integer();
            return c_ulonglong::MAX;
        }
        let v = vv.cast::<PyLongObject>();
        if is_negative(v) {
            overflow_error_c("can't convert negative value to unsigned long long");
            return c_ulonglong::MAX;
        }
        match magnitude_u128(v) {
            Some(mag) if mag <= u64::MAX as u128 => mag as c_ulonglong,
            _ => {
                overflow_error_c("Python int too large to convert to C unsigned long long");
                c_ulonglong::MAX
            }
        }
    }
}

/// The shared truncating mask body.
///
/// # Safety
///
/// `vv` must point to a valid `PyObject`.
unsafe fn as_mask(vv: *mut PyObject, bits: u32) -> u128 {
    // SAFETY: the object is read as an int after the type check.
    unsafe {
        if vv.is_null() || !is_long(vv) {
            not_an_integer();
            return (1u128 << bits) - 1;
        }
        let v = vv.cast::<PyLongObject>();
        if is_compact(v) {
            return compact_value(v) as u128 & ((1u128 << bits) - 1);
        }
        // The digits fold into a 2**bits register, most significant first
        // the way CPython's loop runs (`_PyLong_AsUnsignedLongMask`); the
        // per-step mask is the register's bits shifting out.
        let mut x: u128 = 0;
        for &d in digits(v).iter().rev() {
            x = x << SHIFT | d as u128;
            x &= (1u128 << bits) - 1;
        }
        if sign(v) < 0 {
            x = x.wrapping_neg() & ((1u128 << bits) - 1);
        }
        x
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsUnsignedLongMask(op: *mut PyObject) -> c_ulong {
    // SAFETY: the shared body reads the object as an int.
    unsafe { as_mask(op, 32) as c_ulong }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsUnsignedLongLongMask(op: *mut PyObject) -> c_ulonglong {
    // SAFETY: the shared body reads the object as an int.
    unsafe { as_mask(op, 64) as c_ulonglong }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `vv` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsVoidPtr(vv: *mut PyObject) -> *mut c_void {
    // SAFETY: `PyLong_AsSize_t` only reads the object's digits.
    let address = unsafe { PyLong_AsSize_t(vv) };
    if address == usize::MAX && !crate::tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    address as *mut c_void
}

/// An `OverflowError` with a static message.
fn overflow_error_c(message: &str) {
    crate::tstate::set_string(exceptions::overflow_error(), message.as_bytes());
}

/// See the CPython 3.12 contract for this entry point. The 3.12 signature
/// takes a `str` object; until the unicode runtime exists this reports
/// `SystemError` naming the entry point (spec section 10.1).
///
/// # Safety
///
/// `unicode` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUnicodeObject(
    _unicode: *mut PyObject,
    _base: c_int,
) -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyLong_FromUnicodeObject is not implemented",
    );
    core::ptr::null_mut()
}

/// See the CPython 3.12 contract for this entry point. The 3.12 signature
/// returns a `structseq` describing the int layout; until struct sequence
/// objects exist this reports `SystemError` naming the entry point (spec
/// section 10.1).
#[unsafe(no_mangle)]
pub extern "C" fn PyLong_GetInfo() -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyLong_GetInfo is not implemented",
    );
    core::ptr::null_mut()
}

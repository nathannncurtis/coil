//! The C API object-protocol families of spec section 10.1 against
//! results CPython 3.12 pins: the number, sequence, mapping, iterator
//! and callable protocols, buffers and memoryview, capsules, the call
//! and vectorcall family, and the GC/allocator entry points.

mod common;

use std::ffi::{CString, c_char, c_int, c_void};
use std::fs;
use std::path::Path;
use std::sync::Once;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use coil_capi_sys::{
    METH_FASTCALL, METH_KEYWORDS, METH_NOARGS, METH_O, METH_VARARGS, Py_buffer, PyBUF_FORMAT,
    PyBUF_READ, PyBUF_SIMPLE, PyBUF_STRIDES, PyBUF_WRITABLE, PyBUF_WRITE, PyMethodDef, PyObject,
    PyVarObject,
};
use python312::capi;
use python312::capi::r#abstract as pabs;
use python312::gc::counted;
use python312::object::{singletons, typeobject};
use python312::tstate;
use python312::types::exc;
use python312::types::{bool_, bytes, dict, float_, list, long, pycfunction, tuple, unicode};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

fn int(value: i32) -> *mut PyObject {
    long::PyLong_FromLong(value)
}

fn big(text: &str) -> *mut PyObject {
    common::parse_int(text)
}

fn text(value: &str) -> *mut PyObject {
    let c = CString::new(value).unwrap();
    // SAFETY: `c` is a NUL-terminated string the call only reads.
    unsafe { unicode::PyUnicode_FromString(c.as_ptr()) }
}

fn utf8(object: *mut PyObject) -> String {
    // SAFETY: the object is a runtime string.
    String::from_utf8_lossy(&unsafe { unicode::as_utf8_vec(object) }).into_owned()
}

fn as_i64(object: *mut PyObject) -> i64 {
    // SAFETY: the object is an int or bool.
    unsafe { long::PyLong_AsLongLong(object) }
}

fn assert_int(object: *mut PyObject, expected: i64) {
    assert!(!object.is_null(), "expected an int result");
    assert_eq!(as_i64(object), expected);
}

fn assert_text(object: *mut PyObject, expected: &str) {
    assert!(!object.is_null(), "expected a str result");
    assert_eq!(utf8(object), expected);
}

fn drop(object: *mut PyObject) {
    // SAFETY: the reference is one a constructor returned.
    unsafe { counted::Py_DecRef(object) };
}

fn list_of(values: &[i32]) -> *mut PyObject {
    // SAFETY: the calls follow the list constructor's contract; SetItem
    // takes the reference `int` produced.
    unsafe {
        let object = list::PyList_New(values.len() as isize);
        assert!(!object.is_null());
        for (i, value) in values.iter().enumerate() {
            assert_eq!(list::PyList_SetItem(object, i as isize, int(*value)), 0);
        }
        object
    }
}

fn tuple_of(values: &[i32]) -> *mut PyObject {
    // SAFETY: the calls follow the tuple constructor's contract; SetItem
    // takes the reference `int` produced.
    unsafe {
        let object = tuple::PyTuple_New(values.len() as isize);
        assert!(!object.is_null());
        for (i, value) in values.iter().enumerate() {
            assert_eq!(tuple::PyTuple_SetItem(object, i as isize, int(*value)), 0);
        }
        object
    }
}

fn dict_of(pairs: &[(&str, i32)]) -> *mut PyObject {
    // SAFETY: the calls follow the dict constructor's contract.
    unsafe {
        let object = dict::PyDict_New();
        assert!(!object.is_null());
        for (key, value) in pairs {
            let key_object = text(key);
            assert!(!key_object.is_null());
            assert_eq!(dict::PyDict_SetItem(object, key_object, int(*value)), 0);
            drop(key_object);
        }
        object
    }
}

fn bytes_of(value: &[u8]) -> *mut PyObject {
    // SAFETY: the pointer/length pair describes `value`, which the call
    // copies into the new bytes object.
    unsafe { bytes::PyBytes_FromStringAndSize(value.as_ptr().cast(), value.len() as isize) }
}

fn blank_view() -> Py_buffer {
    Py_buffer {
        buf: core::ptr::null_mut(),
        obj: core::ptr::null_mut(),
        len: 0,
        itemsize: 0,
        readonly: 0,
        ndim: 0,
        format: core::ptr::null_mut(),
        shape: core::ptr::null_mut(),
        strides: core::ptr::null_mut(),
        suboffsets: core::ptr::null_mut(),
        internal: core::ptr::null_mut(),
    }
}

/// The pending error must be `expected`; returns its message after
/// clearing it.
fn take_error(expected: *mut PyObject) -> String {
    let pending = tstate::occurred();
    assert!(!pending.is_null(), "an error must be pending");
    assert!(
        core::ptr::eq(pending, expected),
        "a different exception is pending"
    );
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    message
}

// The C functions the call-family tests build objects from. Each writes
// what it saw into an atomic so the tests can check the convention the
// dispatch chose.

static VARARGS_LAST: AtomicI64 = AtomicI64::new(-1);
static NOARGS_HITS: AtomicU32 = AtomicU32::new(0);
static METH_O_LAST: AtomicI64 = AtomicI64::new(-1);
static FAST_SUM: AtomicI64 = AtomicI64::new(-1);
static FASTKW_KWNAMES: AtomicI64 = AtomicI64::new(-1);
static COUNTER: AtomicI64 = AtomicI64::new(0);
static WEAKREF_CB_HITS: AtomicU32 = AtomicU32::new(0);
static CAPSULE_DTOR_HITS: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn sum_two(_self: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    // SAFETY: METH_VARARGS passes a tuple of the two arguments.
    unsafe {
        let a = long::PyLong_AsLongLong(tuple::PyTuple_GetItem(args, 0));
        let b = long::PyLong_AsLongLong(tuple::PyTuple_GetItem(args, 1));
        VARARGS_LAST.store(a, Ordering::SeqCst);
        long::PyLong_FromLongLong(a.wrapping_add(b))
    }
}

unsafe extern "C" fn forty_two(_self: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    NOARGS_HITS.fetch_add(1, Ordering::SeqCst);
    long::PyLong_FromLong(42)
}

unsafe extern "C" fn doubled(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: METH_O passes the single argument directly.
    let value = unsafe { long::PyLong_AsLongLong(arg) };
    METH_O_LAST.store(value, Ordering::SeqCst);
    long::PyLong_FromLongLong(value * 2)
}

unsafe extern "C" fn fast_sum(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    let mut total: i64 = 0;
    // SAFETY: `args` holds `nargs` objects per the protocol.
    unsafe {
        for i in 0..nargs {
            total = total.wrapping_add(long::PyLong_AsLongLong(*args.add(i as usize)));
        }
    }
    FAST_SUM.store(total, Ordering::SeqCst);
    long::PyLong_FromLongLong(total)
}

unsafe extern "C" fn fast_kw_sum(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    let mut total: i64 = 0;
    // SAFETY: as `fast_sum`.
    unsafe {
        for i in 0..nargs {
            total = total.wrapping_add(long::PyLong_AsLongLong(*args.add(i as usize)));
        }
    }
    let kw = if kwnames.is_null() {
        -1
    } else {
        // SAFETY: `kwnames` is a tuple per the protocol.
        unsafe { tuple::PyTuple_Size(kwnames) as i64 }
    };
    FASTKW_KWNAMES.store(kw, Ordering::SeqCst);
    long::PyLong_FromLongLong(total)
}

unsafe extern "C" fn count_up(_self: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    let value = 1 + COUNTER.fetch_add(1, Ordering::SeqCst);
    long::PyLong_FromLongLong(value)
}

unsafe extern "C" fn weakref_note(_self: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    WEAKREF_CB_HITS.fetch_add(1, Ordering::SeqCst);
    singletons::none()
}

unsafe extern "C" fn capsule_note(_capsule: *mut PyObject) {
    CAPSULE_DTOR_HITS.fetch_add(1, Ordering::SeqCst);
}

static SUM_TWO_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"sum_two".as_ptr(),
        ml_meth: Some(sum_two),
        ml_flags: METH_VARARGS,
        ml_doc: core::ptr::null(),
    },
};
unsafe impl Sync for SyncDef {}
struct SyncDef {
    inner: PyMethodDef,
}

static FORTY_TWO_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"forty_two".as_ptr(),
        ml_meth: Some(forty_two),
        ml_flags: METH_NOARGS,
        ml_doc: core::ptr::null(),
    },
};
static DOUBLED_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"doubled".as_ptr(),
        ml_meth: Some(doubled),
        ml_flags: METH_O,
        ml_doc: core::ptr::null(),
    },
};
static FAST_SUM_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"fast_sum".as_ptr(),
        ml_meth: {
            let func: coil_capi_sys::_PyCFunctionFast = fast_sum;
            Some(unsafe {
                core::mem::transmute::<coil_capi_sys::_PyCFunctionFast, coil_capi_sys::PyCFunction>(
                    func,
                )
            })
        },
        ml_flags: METH_FASTCALL,
        ml_doc: core::ptr::null(),
    },
};
static FAST_KW_SUM_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"fast_kw_sum".as_ptr(),
        ml_meth: {
            let func: coil_capi_sys::_PyCFunctionFastWithKeywords = fast_kw_sum;
            Some(unsafe {
                core::mem::transmute::<
                    coil_capi_sys::_PyCFunctionFastWithKeywords,
                    coil_capi_sys::PyCFunction,
                >(func)
            })
        },
        ml_flags: METH_FASTCALL | METH_KEYWORDS,
        ml_doc: core::ptr::null(),
    },
};
static COUNT_UP_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"count_up".as_ptr(),
        ml_meth: Some(count_up),
        ml_flags: METH_NOARGS,
        ml_doc: core::ptr::null(),
    },
};
static WEAKREF_NOTE_DEF: SyncDef = SyncDef {
    inner: PyMethodDef {
        ml_name: c"weakref_note".as_ptr(),
        ml_meth: Some(weakref_note),
        ml_flags: METH_VARARGS,
        ml_doc: core::ptr::null(),
    },
};

fn cfunction(def: &SyncDef) -> *mut PyObject {
    // SAFETY: the definition is a static matching the convention its
    // flags select.
    unsafe { pycfunction::PyCFunction_New(&def.inner, core::ptr::null_mut()) }
}

#[test]
fn number_binary_operations() {
    init();
    // SAFETY: every call takes valid objects and returns a new
    // reference the test releases.
    unsafe {
        let a = int(7);
        let b = int(2);
        assert_int(pabs::PyNumber_Add(a, b), 9);
        assert_int(pabs::PyNumber_Subtract(a, b), 5);
        assert_int(pabs::PyNumber_Multiply(a, b), 14);
        let quotient = pabs::PyNumber_TrueDivide(a, b);
        assert!(!quotient.is_null());
        assert_eq!(float_::PyFloat_AsDouble(quotient), 3.5);
        drop(quotient);
        assert_int(pabs::PyNumber_FloorDivide(a, b), 3);
        assert_int(pabs::PyNumber_Remainder(a, b), 1);
        let divmod = pabs::PyNumber_Divmod(a, b);
        assert_int(tuple::PyTuple_GetItem(divmod, 0), 3);
        assert_int(tuple::PyTuple_GetItem(divmod, 1), 1);
        drop(divmod);
        assert_int(
            pabs::PyNumber_Power(int(2), int(10), singletons::none()),
            1024,
        );
        assert_int(pabs::PyNumber_Power(int(2), int(10), int(100)), 24);
        assert_int(pabs::PyNumber_Lshift(int(1), int(4)), 16);
        assert_int(pabs::PyNumber_Rshift(int(32), int(2)), 8);
        assert_int(pabs::PyNumber_And(int(12), int(10)), 8);
        assert_int(pabs::PyNumber_Or(int(12), int(10)), 14);
        assert_int(pabs::PyNumber_Xor(int(12), int(10)), 6);
        let mixed = pabs::PyNumber_Add(int(1), float_::PyFloat_FromDouble(2.5));
        assert!(!mixed.is_null());
        assert_eq!(float_::PyFloat_AsDouble(mixed), 3.5);
        drop(mixed);
        drop(b);
        drop(a);
    }
}

#[test]
fn number_binary_errors() {
    init();
    // SAFETY: the operands are valid; each failing call leaves one
    // pending error `take_error` clears.
    unsafe {
        let matrix = pabs::PyNumber_MatrixMultiply(int(2), int(3));
        assert!(matrix.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "unsupported operand type(s) for @: 'int' and 'int'"
        );

        let concatenation = pabs::PyNumber_Add(int(1), text("x"));
        assert!(concatenation.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "unsupported operand type(s) for +: 'int' and 'str'"
        );

        let division = pabs::PyNumber_TrueDivide(int(1), int(0));
        assert!(division.is_null());
        assert_eq!(take_error(exc::zero_division_error()), "division by zero");

        let floor = pabs::PyNumber_FloorDivide(int(1), int(0));
        assert!(floor.is_null());
        assert_eq!(
            take_error(exc::zero_division_error()),
            "integer division or modulo by zero"
        );

        let modulus = pabs::PyNumber_Remainder(int(1), int(0));
        assert!(modulus.is_null());
        assert_eq!(
            take_error(exc::zero_division_error()),
            "integer modulo by zero"
        );
    }
}

#[test]
fn number_unary_operations() {
    init();
    // SAFETY: every call takes a valid object.
    unsafe {
        assert_int(pabs::PyNumber_Negative(int(5)), -5);
        assert_int(pabs::PyNumber_Positive(int(5)), 5);
        assert_int(pabs::PyNumber_Invert(int(5)), -6);
        assert_int(pabs::PyNumber_Absolute(int(-7)), 7);
        assert_int(pabs::PyNumber_Absolute(int(7)), 7);

        let negative = pabs::PyNumber_Negative(text("x"));
        assert!(negative.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "bad operand type for unary -: 'str'"
        );
    }
}

#[test]
fn number_inplace_operations() {
    init();
    // SAFETY: the operands are valid; in-place entry points return a
    // new or retained reference either way.
    unsafe {
        let list = list_of(&[1]);
        let grown = pabs::PyNumber_InPlaceAdd(list, list_of(&[2]));
        assert!(core::ptr::eq(grown, list), "list += extends in place");
        assert_int(list::PyList_GetItem(grown, 0), 1);
        assert_int(list::PyList_GetItem(grown, 1), 2);
        drop(grown);
        drop(list);

        let sum = pabs::PyNumber_InPlaceAdd(int(2), int(3));
        assert_int(sum, 5);
        drop(sum);
        let difference = pabs::PyNumber_InPlaceSubtract(int(10), int(3));
        assert_int(difference, 7);
        drop(difference);
        let power = pabs::PyNumber_InPlacePower(int(2), int(10), singletons::none());
        assert_int(power, 1024);
        drop(power);
        let shift = pabs::PyNumber_InPlaceLshift(int(1), int(4));
        assert_int(shift, 16);
        drop(shift);
    }
}

#[test]
fn number_checks_index_and_conversions() {
    init();
    // SAFETY: every call takes a valid object.
    unsafe {
        assert_eq!(pabs::PyNumber_Check(int(5)), 1);
        assert_eq!(pabs::PyNumber_Check(float_::PyFloat_FromDouble(1.5)), 1);
        let string = text("5");
        assert_eq!(pabs::PyNumber_Check(string), 0);
        drop(string);

        assert_eq!(pabs::PyIndex_Check(singletons::true_()), 1);
        assert_eq!(pabs::PyIndex_Check(text("x")), 0);

        let index = pabs::PyNumber_Index(singletons::true_());
        assert_int(index, 1);
        drop(index);
        let index = pabs::_PyNumber_Index(big("1000000000000"));
        assert_int(index, 1000000000000);
        drop(index);

        let refused = pabs::PyNumber_Index(text("5"));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'str' object cannot be interpreted as an integer"
        );

        let truncated = pabs::PyNumber_Long(float_::PyFloat_FromDouble(3.7));
        assert_int(truncated, 3);
        drop(truncated);
        // Parsing a str defers to `PyLong_FromUnicodeObject`, which this
        // runtime does not grow yet; the entry point names it.
        let parsed = pabs::PyNumber_Long(text("12"));
        assert!(parsed.is_null());
        assert_eq!(
            take_error(exc::system_error()),
            "coil: PyLong_FromUnicodeObject is not implemented"
        );
        // As with `PyNumber_Long`, a str defers to the string parser
        // this runtime names rather than grows.
        let floated = pabs::PyNumber_Float(text("2.5"));
        assert!(floated.is_null());
        assert_eq!(
            take_error(exc::system_error()),
            "coil: PyFloat_FromString is not implemented"
        );
        let floated = pabs::PyNumber_Float(singletons::true_());
        assert!(!floated.is_null());
        assert_eq!(float_::PyFloat_AsDouble(floated), 1.0);
        drop(floated);

        let hex = pabs::PyNumber_ToBase(int(255), 16);
        assert_text(hex, "0xff");
        drop(hex);
        let negative_hex = pabs::PyNumber_ToBase(int(-255), 16);
        assert_text(negative_hex, "-0xff");
        drop(negative_hex);

        let bad_base = pabs::PyNumber_ToBase(int(5), 7);
        assert!(bad_base.is_null());
        // Only the four string-conversion bases are allowed here; the
        // 2..36 range belongs to the `int()` constructor.
        assert_eq!(
            take_error(exc::system_error()),
            "PyNumber_ToBase: base must be 2, 8, 10 or 16"
        );
        let bad_value = pabs::PyNumber_ToBase(text("x"), 16);
        assert!(bad_value.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'str' object cannot be interpreted as an integer"
        );

        assert_eq!(pabs::PyNumber_AsSsize_t(int(5), exc::overflow_error()), 5);
        assert_eq!(pabs::PyNumber_AsSsize_t(int(-5), exc::overflow_error()), -5);
        let overflowed =
            pabs::PyNumber_AsSsize_t(big("99999999999999999999999"), exc::overflow_error());
        assert_eq!(overflowed, -1);
        assert_eq!(
            take_error(exc::overflow_error()),
            "cannot fit 'int' into an index-sized integer"
        );
        let clipped =
            pabs::PyNumber_AsSsize_t(big("99999999999999999999999"), core::ptr::null_mut());
        assert_eq!(clipped, isize::MAX);
        assert!(tstate::occurred().is_null());
        let clipped =
            pabs::PyNumber_AsSsize_t(big("-99999999999999999999999"), core::ptr::null_mut());
        assert_eq!(clipped, isize::MIN);
        let refused = pabs::PyNumber_AsSsize_t(text("x"), exc::overflow_error());
        assert_eq!(refused, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'str' object cannot be interpreted as an integer"
        );
    }
}

#[test]
fn sequence_size_item_and_errors() {
    init();
    // SAFETY: every call takes valid objects; failing calls leave one
    // pending error `take_error` clears.
    unsafe {
        let values = list_of(&[10, 20, 30]);
        let fixed = tuple_of(&[1, 2]);
        let word = text("abc");
        let blob = bytes_of(b"hi");
        assert_eq!(pabs::PySequence_Check(values), 1);
        assert_eq!(pabs::PySequence_Check(dict_of(&[("k", 1)])), 0);
        assert_eq!(pabs::PySequence_Check(singletons::none()), 0);
        assert_eq!(pabs::PySequence_Size(values), 3);
        assert_eq!(pabs::PySequence_Length(fixed), 2);
        assert_eq!(pabs::PySequence_Size(word), 3);
        assert_eq!(pabs::PySequence_Size(blob), 2);
        drop(blob);
        drop(word);

        assert_int(pabs::PySequence_GetItem(values, 1), 20);
        assert_int(pabs::PySequence_GetItem(values, -1), 30);
        assert_eq!(pabs::PySequence_SetItem(values, 1, int(21)), 0);
        assert_int(list::PyList_GetItem(values, 1), 21);
        assert_eq!(pabs::PySequence_DelItem(values, 1), 0);
        assert_eq!(list::PyList_Size(values), 2);
        assert_int(pabs::PySequence_GetItem(values, 1), 30);
        drop(values);
        drop(fixed);

        let out_of_range = pabs::PySequence_GetItem(list_of(&[1]), 5);
        assert!(out_of_range.is_null());
        assert_eq!(take_error(exc::index_error()), "list index out of range");

        let unindexed = pabs::PySequence_GetItem(int(5), 0);
        assert!(unindexed.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object does not support indexing"
        );

        let sized = pabs::PySequence_Size(int(5));
        assert_eq!(sized, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "object of type 'int' has no len()"
        );

        let set = pabs::PySequence_SetItem(int(5), 0, int(1));
        assert_eq!(set, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object does not support item assignment"
        );

        let deleted = pabs::PySequence_DelItem(int(5), 0);
        assert_eq!(deleted, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object doesn't support item deletion"
        );
    }
}

#[test]
fn sequence_concat_and_repeat() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let concatenated = pabs::PySequence_Concat(list_of(&[1]), list_of(&[2]));
        assert_eq!(list::PyList_Size(concatenated), 2);
        drop(concatenated);
        let concatenated = pabs::PySequence_Concat(text("ab"), text("cd"));
        assert_text(concatenated, "abcd");
        drop(concatenated);
        let concatenated = pabs::PySequence_Concat(text("ab"), int(1));
        assert!(concatenated.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "can only concatenate str (not \"int\") to str"
        );

        let repeated = pabs::PySequence_Repeat(text("ab"), 3);
        assert_text(repeated, "ababab");
        drop(repeated);
        let repeated = pabs::PySequence_Repeat(text("ab"), 0);
        assert_text(repeated, "");
        drop(repeated);
        let repeated = pabs::PySequence_Repeat(list_of(&[7]), 2);
        assert_eq!(list::PyList_Size(repeated), 2);
        drop(repeated);
        let reflected = pabs::PySequence_Repeat(int(3), 2);
        assert!(reflected.is_null());
        // The "can't multiply sequence by non-int" wording belongs to the
        // number protocol; the sequence entry point names its operand.
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object can't be repeated"
        );

        let base = list_of(&[1]);
        let extended = pabs::PySequence_InPlaceConcat(base, list_of(&[2]));
        assert!(core::ptr::eq(extended, base));
        assert_eq!(list::PyList_Size(extended), 2);
        drop(extended);
        let doubled = pabs::PySequence_InPlaceRepeat(base, 2);
        assert!(core::ptr::eq(doubled, base));
        assert_eq!(list::PyList_Size(doubled), 4);
        drop(doubled);
    }
}

#[test]
fn sequence_slice_gaps_name_the_missing_type() {
    init();
    // SAFETY: the operands are valid; each call leaves the pending
    // error `take_error` clears.
    unsafe {
        let values = list_of(&[1, 2, 3]);
        let sliced = pabs::PySequence_GetSlice(values, 0, 2);
        assert!(sliced.is_null());
        assert_eq!(
            take_error(exc::not_implemented_error()),
            "coil: slice objects are not implemented"
        );
        let assigned = pabs::PySequence_SetSlice(values, 0, 1, list_of(&[9]));
        assert_eq!(assigned, -1);
        assert_eq!(
            take_error(exc::not_implemented_error()),
            "coil: slice objects are not implemented"
        );
        let deleted = pabs::PySequence_DelSlice(values, 0, 1);
        assert_eq!(deleted, -1);
        assert_eq!(
            take_error(exc::not_implemented_error()),
            "coil: slice objects are not implemented"
        );
        drop(values);

        let unsliceable = pabs::PySequence_GetSlice(int(5), 0, 1);
        assert!(unsliceable.is_null());
        assert_eq!(take_error(exc::type_error()), "'int' object is unsliceable");
        let unassignable = pabs::PySequence_SetSlice(int(5), 0, 1, singletons::none());
        assert_eq!(unassignable, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object doesn't support slice assignment"
        );
        let undeletable = pabs::PySequence_DelSlice(int(5), 0, 1);
        assert_eq!(undeletable, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object doesn't support slice deletion"
        );
    }
}

#[test]
fn sequence_tuple_list_and_fast() {
    init();
    // SAFETY: every call takes a valid object.
    unsafe {
        let from_list = pabs::PySequence_Tuple(list_of(&[1, 2]));
        assert_eq!(tuple::PyTuple_Size(from_list), 2);
        drop(from_list);
        let from_string = pabs::PySequence_Tuple(text("ab"));
        assert_text(tuple::PyTuple_GetItem(from_string, 0), "a");
        assert_text(tuple::PyTuple_GetItem(from_string, 1), "b");
        drop(from_string);
        let from_dict = pabs::PySequence_Tuple(dict_of(&[("k", 1)]));
        assert_eq!(tuple::PyTuple_Size(from_dict), 1);
        drop(from_dict);
        let refused = pabs::PySequence_Tuple(int(5));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object is not iterable"
        );

        let from_tuple = pabs::PySequence_List(tuple_of(&[1, 2]));
        assert_eq!(list::PyList_Size(from_tuple), 2);
        drop(from_tuple);
        let from_string = pabs::PySequence_List(text("ab"));
        assert_eq!(list::PyList_Size(from_string), 2);
        drop(from_string);

        let a_tuple = tuple_of(&[1]);
        let same = pabs::PySequence_Fast(a_tuple, c"expected a sequence".as_ptr());
        assert!(core::ptr::eq(same, a_tuple));
        drop(same);
        drop(a_tuple);
        let message = c"expected a series".as_ptr();
        let refused = pabs::PySequence_Fast(int(5), message);
        assert!(refused.is_null());
        assert_eq!(take_error(exc::type_error()), "expected a series");
    }
}

#[test]
fn sequence_contains_count_and_index() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let values = list_of(&[1, 1, 2]);
        assert_eq!(pabs::PySequence_Contains(values, int(2)), 1);
        assert_eq!(pabs::PySequence_Contains(values, int(3)), 0);
        assert_eq!(pabs::PySequence_Count(values, int(1)), 2);
        assert_eq!(pabs::PySequence_Index(values, int(2)), 2);
        let missing = pabs::PySequence_Index(values, int(9));
        assert_eq!(missing, -1);
        assert_eq!(
            take_error(exc::value_error()),
            "sequence.index(x): x not in sequence"
        );
        drop(values);

        // The sequence comes first: `PySequence_In(w, v)` asks whether
        // `w` contains `v`.
        assert_eq!(pabs::PySequence_In(text("abc"), text("b")), 1);
        assert_eq!(pabs::PySequence_In(text("abc"), text("z")), 0);

        let membership = dict_of(&[("k", 1)]);
        assert_eq!(pabs::PySequence_Contains(membership, text("k")), 1);
        assert_eq!(pabs::PySequence_Contains(membership, text("z")), 0);
        drop(membership);

        let not_iterable = pabs::PySequence_Contains(int(5), int(5));
        assert_eq!(not_iterable, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "argument of type 'int' is not iterable"
        );
    }
}

#[test]
fn mapping_protocol() {
    init();
    // SAFETY: every call takes valid objects; failing calls leave one
    // pending error `take_error` clears.
    unsafe {
        let mapping = dict_of(&[("a", 1), ("b", 2)]);
        let sequence = list_of(&[1]);

        assert_eq!(pabs::PyMapping_Check(mapping), 1);
        // Lists carry CPython's `list_as_mapping` with all three slots.
        assert_eq!(pabs::PyMapping_Check(sequence), 1);
        assert_eq!(pabs::PyMapping_Check(singletons::none()), 0);
        assert_eq!(pabs::PyMapping_Check(core::ptr::null_mut()), 0);

        assert_eq!(pabs::PyMapping_Size(mapping), 2);
        assert_eq!(pabs::PyMapping_Length(mapping), 2);
        assert_eq!(pabs::PyMapping_Size(sequence), 1);
        let sized = pabs::PyMapping_Size(int(5));
        assert_eq!(sized, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "object of type 'int' has no len()"
        );
        drop(sequence);

        let key = c"a".as_ptr();
        assert_int(pabs::PyMapping_GetItemString(mapping, key), 1);
        assert_eq!(pabs::PyMapping_HasKeyString(mapping, key), 1);
        assert_eq!(pabs::PyMapping_HasKeyString(mapping, c"z".as_ptr()), 0);
        let key_object = text("b");
        assert_eq!(pabs::PyMapping_HasKey(mapping, key_object), 1);
        assert_int(pabs::PyObject_GetItem(mapping, key_object), 2);
        drop(key_object);
        assert_eq!(
            pabs::PyMapping_SetItemString(mapping, c"c".as_ptr(), int(3)),
            0
        );
        assert_eq!(dict::PyDict_Size(mapping), 3);
        assert_eq!(pabs::PyObject_SetItem(mapping, text("d"), int(4)), 0);
        assert_eq!(dict::PyDict_Size(mapping), 4);
        assert_eq!(pabs::PyObject_DelItemString(mapping, c"d".as_ptr()), 0);
        let key_object = text("c");
        assert_eq!(pabs::PyObject_DelItem(mapping, key_object), 0);
        drop(key_object);
        assert_eq!(dict::PyDict_Size(mapping), 2);

        let keys = pabs::PyMapping_Keys(mapping);
        assert_eq!(list::PyList_Size(keys), 2);
        drop(keys);
        let values = pabs::PyMapping_Values(mapping);
        assert_eq!(list::PyList_Size(values), 2);
        drop(values);
        let items = pabs::PyMapping_Items(mapping);
        assert_eq!(list::PyList_Size(items), 2);
        drop(items);
        drop(mapping);

        let missing = pabs::PyObject_GetItem(dict_of(&[("a", 1)]), text("z"));
        assert!(missing.is_null());
        take_error(exc::key_error());

        let unscriptable = pabs::PyObject_GetItem(int(5), text("x"));
        assert!(unscriptable.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object is not subscriptable"
        );
        let unassignable = pabs::PyObject_SetItem(int(5), text("x"), int(1));
        assert_eq!(unassignable, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object does not support item assignment"
        );
        let undeletable = pabs::PyObject_DelItem(int(5), text("x"));
        assert_eq!(undeletable, -1);
        // `PyObject_DelItem` keeps the uncontracted wording;
        // `PySequence_DelItem` alone shortens it.
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object does not support item deletion"
        );
    }
}

#[test]
fn iterators_walk_and_stop_quietly() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let values = list_of(&[10, 20]);
        let iterator = capi::object::PyObject_GetIter(values);
        assert!(!iterator.is_null());
        // Lists iterate through their own iterator type, like CPython's
        // `list_iterator`; `PySeqIter_Type` is the no-tp_iter fallback.
        let name = core::ffi::CStr::from_ptr((*type_of(iterator)).tp_name).to_bytes();
        assert_eq!(name, b"list_iterator");
        assert_eq!(pabs::PyIter_Check(iterator), 1);
        assert_eq!(pabs::PyIter_Check(values), 0);
        assert_eq!(pabs::PyAIter_Check(iterator), 0);
        assert_int(pabs::PyIter_Next(iterator), 10);
        assert_int(pabs::PyIter_Next(iterator), 20);
        let spent = pabs::PyIter_Next(iterator);
        assert!(spent.is_null());
        assert!(tstate::occurred().is_null(), "exhaustion stops quietly");
        let spent_again = pabs::PyIter_Next(iterator);
        assert!(spent_again.is_null());
        assert!(tstate::occurred().is_null());
        assert!(core::ptr::eq(
            capi::object::PyObject_SelfIter(iterator),
            iterator
        ));
        drop(iterator);
        drop(iterator);
        drop(values);

        let fresh = capi::object::PyObject_GetIter(tuple_of(&[1, 2]));
        assert!(core::ptr::eq(capi::object::PyObject_GetIter(fresh), fresh));
        drop(fresh);

        let refused = capi::object::PyObject_GetIter(int(5));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object is not iterable"
        );
        let refused = capi::object::PyObject_GetAIter(int(5));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object is not an async iterable"
        );
    }
}

/// `header::type_of` without importing the private module path.
fn type_of(object: *mut PyObject) -> *mut coil_capi_sys::PyTypeObject {
    // SAFETY: the object is a runtime object.
    unsafe { (*object).ob_type }
}

#[test]
fn iterator_send_reports_the_stop() {
    init();
    // SAFETY: the iterator and arguments are valid objects.
    unsafe {
        let iterator = capi::object::PyObject_GetIter(list_of(&[1]));
        let mut result: *mut PyObject = core::ptr::null_mut();
        assert_eq!(
            pabs::PyIter_Send(iterator, singletons::none(), &raw mut result),
            coil_capi_sys::PYGEN_NEXT
        );
        assert_int(result, 1);
        drop(result);
        assert_eq!(
            pabs::PyIter_Send(iterator, singletons::none(), &raw mut result),
            coil_capi_sys::PYGEN_ERROR
        );
        assert!(result.is_null());
        assert!(tstate::occurred().is_null());
        drop(iterator);
    }
}

#[test]
fn sequence_and_callable_iterators() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let walker = capi::iterobject::PySeqIter_New(text("abc"));
        assert!(!walker.is_null());
        assert!(core::ptr::eq(
            type_of(walker),
            &raw mut capi::iterobject::PySeqIter_Type,
        ));
        assert_text(pabs::PyIter_Next(walker), "a");
        assert_text(pabs::PyIter_Next(walker), "b");
        assert_text(pabs::PyIter_Next(walker), "c");
        assert!(pabs::PyIter_Next(walker).is_null());
        drop(walker);

        let counter = cfunction(&COUNT_UP_DEF);
        let until_three = capi::iterobject::PyCallIter_New(counter, int(3));
        assert!(!until_three.is_null());
        assert!(core::ptr::eq(
            type_of(until_three),
            &raw mut capi::iterobject::PyCallIter_Type,
        ));
        assert_int(pabs::PyIter_Next(until_three), 1);
        assert_int(pabs::PyIter_Next(until_three), 2);
        assert!(pabs::PyIter_Next(until_three).is_null());
        assert!(tstate::occurred().is_null());
        assert!(pabs::PyIter_Next(until_three).is_null());
        drop(until_three);
        drop(counter);
        COUNTER.store(0, Ordering::SeqCst);

        assert!(pabs::PyIter_Check(int(5)) == 0);
        let next = capi::object::_PyObject_NextNotImplemented(int(5));
        assert!(next.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object is not iterable"
        );
    }
}

#[test]
fn callable_check() {
    init();
    // SAFETY: the arguments are valid objects or null.
    unsafe {
        let callable = cfunction(&SUM_TWO_DEF);
        assert_eq!(pabs::PyCallable_Check(callable), 1);
        drop(callable);
        assert_eq!(pabs::PyCallable_Check(int(5)), 0);
        assert_eq!(pabs::PyCallable_Check(core::ptr::null_mut()), 0);
        // Static types carry no tp_call until the class machinery lands
        // (spec section on heap types owns that); the check reports the
        // slot, and `int`'s metaclass has none here.
        let long_type: *mut PyObject = (&raw mut long::PyLong_Type).cast();
        assert_eq!(pabs::PyCallable_Check(long_type), 0);
    }
}

#[test]
fn call_family_covers_every_convention() {
    init();
    // SAFETY: every call takes valid objects built above.
    unsafe {
        let sum = cfunction(&SUM_TWO_DEF);
        let answer = cfunction(&FORTY_TWO_DEF);
        let double = cfunction(&DOUBLED_DEF);
        let fast = cfunction(&FAST_SUM_DEF);
        let fast_kw = cfunction(&FAST_KW_SUM_DEF);

        let args = tuple_of(&[2, 3]);
        assert_int(
            capi::call::PyObject_Call(sum, args, core::ptr::null_mut()),
            5,
        );
        assert_int(capi::call::PyObject_CallObject(sum, args), 5);
        assert_int(
            capi::call::_PyObject_Call(core::ptr::null_mut(), sum, args, core::ptr::null_mut()),
            5,
        );
        drop(args);

        let hits = NOARGS_HITS.load(Ordering::SeqCst);
        assert_int(capi::call::PyObject_CallNoArgs(answer), 42);
        assert_eq!(NOARGS_HITS.load(Ordering::SeqCst), hits + 1);

        assert_int(capi::call::PyObject_CallOneArg(double, int(21)), 42);
        assert_eq!(METH_O_LAST.load(Ordering::SeqCst), 21);

        let stack = [int(2), int(3)];
        let moved = capi::call::PyObject_Vectorcall(
            fast,
            stack.as_ptr(),
            2 | capi::call::PY_VECTORCALL_ARGUMENTS_OFFSET,
            core::ptr::null_mut(),
        );
        assert_int(moved, 5);
        assert_eq!(FAST_SUM.load(Ordering::SeqCst), 5);
        drop(stack[0]);
        drop(stack[1]);

        let nargsf = 3usize | capi::call::PY_VECTORCALL_ARGUMENTS_OFFSET;
        assert_eq!(capi::call::PyVectorcall_NARGS(nargsf), 3);
        assert!(capi::call::PyVectorcall_Function(sum).is_none());
        assert!(capi::call::PyVectorcall_Function(double).is_some());
        assert!(capi::call::PyVectorcall_Function(fast).is_some());
        assert!(capi::call::PyVectorcall_Function(int(5)).is_none());

        let arguments = tuple_of(&[4, 6]);
        assert_int(
            capi::call::PyVectorcall_Call(fast, arguments, core::ptr::null_mut()),
            10,
        );
        drop(arguments);
        let empty = tuple::PyTuple_New(0);
        let refused = capi::call::PyVectorcall_Call(int(5), empty, core::ptr::null_mut());
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "'int' object does not support vectorcall"
        );
        drop(empty);

        let stack = [int(1), int(2)];
        assert_int(
            capi::call::PyObject_VectorcallDict(
                fast_kw,
                stack.as_ptr(),
                2 | capi::call::PY_VECTORCALL_ARGUMENTS_OFFSET,
                core::ptr::null_mut(),
            ),
            3,
        );
        // Both a null and an empty dictionary arrive as a null kwnames
        // tuple; only a non-empty one goes through the unpacking path.
        assert_eq!(FASTKW_KWNAMES.load(Ordering::SeqCst), -1);
        let with_names = dict::PyDict_New();
        assert_int(
            capi::call::PyObject_VectorcallDict(
                fast_kw,
                stack.as_ptr(),
                2 | capi::call::PY_VECTORCALL_ARGUMENTS_OFFSET,
                with_names,
            ),
            3,
        );
        assert_eq!(FASTKW_KWNAMES.load(Ordering::SeqCst), -1);
        drop(with_names);
        drop(stack[1]);
        drop(stack[0]);

        let stack = [int(5)];
        assert_int(
            capi::call::_PyObject_FastCall(double, stack.as_ptr(), 1),
            10,
        );
        drop(stack[0]);

        let stack = [sum, int(2), int(3)];
        assert_int(
            capi::call::_PyObject_MakeTpCall(
                core::ptr::null_mut(),
                sum,
                stack.as_ptr().add(1),
                2,
                core::ptr::null_mut(),
            ),
            5,
        );
        drop(stack[2]);
        drop(stack[1]);

        drop(fast_kw);
        drop(fast);
        drop(double);
        drop(answer);
        drop(sum);
    }
}

#[test]
fn call_family_builds_arguments_from_format_strings() {
    init();
    // SAFETY: every call takes valid objects; the slots carry the C
    // values the format names.
    unsafe {
        let sum = cfunction(&SUM_TWO_DEF);
        let double = cfunction(&DOUBLED_DEF);
        let format = c"ii".as_ptr();
        assert_int(
            capi::call::PyObject_CallFunction(sum, format, 2u64, 3u64, 0, 0, 0, 0, 0, 0),
            5,
        );
        assert_int(
            capi::call::_PyObject_CallFunction_SizeT(sum, format, 2u64, 3u64, 0, 0, 0, 0, 0, 0),
            5,
        );
        assert_int(
            capi::call::PyObject_CallFunction(double, c"i".as_ptr(), 21u64, 0, 0, 0, 0, 0, 0, 0),
            42,
        );
        let an_int = int(9);
        assert_int(
            capi::call::PyObject_CallFunction(
                double,
                c"O".as_ptr(),
                an_int as usize as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            18,
        );
        drop(an_int);
        assert_int(
            capi::call::PyObject_CallFunctionObjArgs(
                double,
                int(8),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            ),
            16,
        );
        let no_arguments = tuple::PyTuple_New(0);
        assert_int(
            capi::call::_PyObject_Call_Prepend(
                core::ptr::null_mut(),
                double,
                int(4),
                no_arguments,
                core::ptr::null_mut(),
            ),
            8,
        );
        drop(no_arguments);
        drop(double);
        drop(sum);
    }
}

#[test]
fn call_family_reaches_methods() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let module = python312::modules::PyModule_New(c"protocol".as_ptr());
        assert!(!module.is_null());
        let double = cfunction(&DOUBLED_DEF);
        assert_eq!(
            dict::PyDict_SetItem(
                python312::modules::PyModule_GetDict(module),
                text("double"),
                double
            ),
            0
        );

        let name = text("double");
        assert_int(
            capi::call::PyObject_CallMethodObjArgs(
                module,
                name,
                int(21),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            ),
            42,
        );
        assert_int(
            capi::call::PyObject_CallMethod(
                module,
                c"double".as_ptr(),
                c"i".as_ptr(),
                21u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            42,
        );
        assert_int(
            capi::call::_PyObject_CallMethod(
                module,
                name,
                c"i".as_ptr(),
                21u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            42,
        );
        assert_int(
            capi::call::_PyObject_CallMethod_SizeT(
                module,
                c"double".as_ptr(),
                c"i".as_ptr(),
                21u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ),
            42,
        );

        let stack = [module, int(21)];
        assert_int(
            capi::call::PyObject_VectorcallMethod(
                name,
                stack.as_ptr(),
                2 | capi::call::PY_VECTORCALL_ARGUMENTS_OFFSET,
                core::ptr::null_mut(),
            ),
            42,
        );
        drop(stack[1]);

        let missing = capi::call::PyObject_CallMethod(
            module,
            c"absent".as_ptr(),
            c"i".as_ptr(),
            1u64,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        assert!(missing.is_null());
        take_error(exc::attribute_error());

        drop(name);
        drop(double);
        drop(module);
    }
}

#[test]
fn object_attributes_on_modules() {
    init();
    // SAFETY: the module and names are valid objects.
    unsafe {
        let module = python312::modules::PyModule_New(c"attrs".as_ptr());
        assert!(!module.is_null());
        let dictionary = capi::object::_PyObject_GetDictPtr(module);
        assert!(!dictionary.read().is_null());

        let answer = int(42);
        assert_eq!(
            capi::object::PyObject_SetAttrString(module, c"answer".as_ptr(), answer),
            0
        );
        assert!(core::ptr::eq(
            capi::object::PyObject_GetAttrString(module, c"answer".as_ptr()),
            answer,
        ));
        let name = text("answer");
        assert!(core::ptr::eq(
            capi::object::PyObject_GetAttr(module, name),
            answer
        ));
        drop(name);
        assert_eq!(
            capi::object::PyObject_HasAttrString(module, c"answer".as_ptr()),
            1
        );
        assert_eq!(
            capi::object::PyObject_HasAttrString(module, c"missing".as_ptr()),
            0
        );

        assert_eq!(
            capi::object::PyObject_SetAttrString(module, c"answer".as_ptr(), int(43)),
            0
        );
        assert_int(
            capi::object::PyObject_GetAttrString(module, c"answer".as_ptr()),
            43,
        );

        let name = text("answer");
        assert_eq!(capi::object::PyObject_HasAttr(module, name), 1);
        drop(name);
        let name = text("missing");
        assert_eq!(capi::object::PyObject_HasAttr(module, name), 0);
        drop(name);

        let mut found: *mut PyObject = core::ptr::null_mut();
        let name = text("answer");
        assert_eq!(
            capi::object::_PyObject_LookupAttr(module, name, &raw mut found),
            1
        );
        assert_int(found, 43);
        drop(found);
        let name = text("missing");
        found = core::ptr::null_mut();
        assert_eq!(
            capi::object::_PyObject_LookupAttr(module, name, &raw mut found),
            0
        );
        assert!(found.is_null());
        assert!(tstate::occurred().is_null());
        drop(name);

        let mut method: *mut PyObject = core::ptr::null_mut();
        let name = text("answer");
        assert_eq!(
            capi::object::_PyObject_GetMethod(module, name, &raw mut method),
            0
        );
        assert_int(method, 43);
        drop(method);
        drop(name);

        let missing = capi::object::PyObject_GetAttrString(module, c"nope".as_ptr());
        assert!(missing.is_null());
        assert_eq!(
            take_error(exc::attribute_error()),
            "module 'attrs' has no attribute 'nope'"
        );

        let name = text("answer");
        assert!(core::ptr::eq(
            capi::object::PyObject_GenericGetAttr(module, name),
            generic_found(module, name),
        ));
        drop(name);

        let refused = capi::object::PyObject_SetAttrString(int(5), c"x".as_ptr(), int(1));
        assert_eq!(refused, -1);
        take_error(exc::attribute_error());

        drop(answer);
        drop(module);
    }
}

/// `_PyObject_GenericGetAttrWithDict`'s plain-dict path, for the identity
/// assertion above.
fn generic_found(module: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the call follows the entry point's contract.
    unsafe {
        capi::object::_PyObject_GenericGetAttrWithDict(module, name, core::ptr::null_mut(), 0)
    }
}

#[test]
fn object_generic_dict_entry_points() {
    init();
    // SAFETY: the module and dictionaries are valid objects.
    unsafe {
        let module = python312::modules::PyModule_New(c"generic".as_ptr());
        assert_eq!(
            capi::object::PyObject_SetAttrString(module, c"a".as_ptr(), int(1)),
            0
        );
        let name = text("a");
        assert_int(
            capi::object::_PyObject_GenericGetAttrWithDict(module, name, core::ptr::null_mut(), 0),
            1,
        );
        let other = dict::PyDict_New();
        assert_eq!(dict::PyDict_SetItem(other, name, int(7)), 0);
        assert_int(
            capi::object::_PyObject_GenericGetAttrWithDict(module, name, other, 0),
            7,
        );
        // CPython's module init seeds the five `__dunder__` entries, so the
        // dict holds them plus `a`.
        assert_eq!(
            dict::PyDict_Size(capi::object::_PyObject_GetDictPtr(module).read()),
            6
        );
        assert_eq!(
            capi::object::PyObject_GenericSetAttr(module, name, int(9)),
            0
        );
        assert_int(
            capi::object::PyObject_GetAttrString(module, c"a".as_ptr()),
            9,
        );
        drop(other);
        drop(name);

        let exposed = capi::object::PyObject_GenericGetDict(module, core::ptr::null_mut());
        assert_eq!(dict::PyDict_Size(exposed), 6);
        drop(exposed);
        let replacement = dict::PyDict_New();
        assert_eq!(
            capi::object::PyObject_GenericSetDict(module, replacement, core::ptr::null_mut()),
            0
        );
        assert!(core::ptr::eq(
            capi::object::_PyObject_GetDictPtr(module).read(),
            replacement
        ));
        drop(replacement);
        drop(module);
    }
}

#[test]
fn object_repr_str_ascii_bytes_and_hash() {
    init();
    // SAFETY: every call takes a valid object.
    unsafe {
        assert_text(unicode::PyObject_Repr(int(5)), "5");
        assert_text(unicode::PyObject_Str(int(5)), "5");
        assert_text(unicode::PyObject_Repr(text("hi")), "'hi'");
        assert_text(unicode::PyObject_Str(text("hi")), "hi");
        assert_text(unicode::PyObject_Str(singletons::none()), "None");
        assert_text(unicode::PyObject_Repr(singletons::none()), "None");
        assert_text(unicode::PyObject_ASCII(text("café")), "'caf\\xe9'");
        // CPython's PyBytes_FromObject refuses str without an encoding.
        let refused = bytes::PyObject_Bytes(text("abc"));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "cannot convert 'str' object to bytes"
        );
        let refused = bytes::PyObject_Bytes(int(5));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "cannot convert 'int' object to bytes"
        );
        let encoded = bytes::PyObject_Bytes(bytes_of(b"abc"));
        assert_eq!(bytes::PyBytes_Size(encoded), 3);
        drop(encoded);

        assert_eq!(capi::object::PyObject_Hash(int(5)), 5);
        assert_eq!(capi::object::PyObject_Hash(int(-1)), -2);
        assert_eq!(capi::object::PyObject_Hash(text("")), 0);
        assert_eq!(
            capi::object::PyObject_Hash(text("hello")),
            capi::object::PyObject_Hash(bytes_of(b"hello"))
        );
        let unhashable = capi::object::PyObject_Hash(list_of(&[1]));
        assert_eq!(unhashable, -1);
        assert_eq!(take_error(exc::type_error()), "unhashable type: 'list'");
    }
}

#[test]
fn object_truthiness() {
    init();
    // SAFETY: every call takes a valid object.
    let falsy = [
        singletons::none(),
        singletons::false_(),
        int(0),
        float_::PyFloat_FromDouble(0.0),
        text(""),
        bytes_of(b""),
        list_of(&[]),
        tuple_of(&[]),
        dict_of(&[]),
    ];
    for value in falsy {
        // SAFETY: the object is a valid runtime object.
        unsafe {
            assert_eq!(capi::object::PyObject_IsTrue(value), 0);
            assert_eq!(capi::object::PyObject_Not(value), 1);
        }
        drop(value);
    }
    let truthy = [
        singletons::true_(),
        int(1),
        float_::PyFloat_FromDouble(0.5),
        text("x"),
        bytes_of(b"x"),
        list_of(&[1]),
        tuple_of(&[1]),
        dict_of(&[("k", 1)]),
    ];
    for value in truthy {
        // SAFETY: the object is a valid runtime object.
        unsafe {
            assert_eq!(capi::object::PyObject_IsTrue(value), 1);
            assert_eq!(capi::object::PyObject_Not(value), 0);
        }
        drop(value);
    }
}

#[test]
fn object_rich_comparisons() {
    init();
    // SAFETY: every call takes valid objects; results are bools or new
    // references the test releases.
    unsafe {
        let one = int(1);
        let other = int(1);
        let two = int(2);
        let float_one = float_::PyFloat_FromDouble(1.0);
        assert!(core::ptr::eq(
            capi::object::PyObject_RichCompare(one, other, typeobject::Py_EQ),
            singletons::true_(),
        ));
        assert!(core::ptr::eq(
            capi::object::PyObject_RichCompare(one, two, typeobject::Py_LT),
            singletons::true_(),
        ));
        assert!(core::ptr::eq(
            capi::object::PyObject_RichCompare(one, two, typeobject::Py_GE),
            singletons::false_(),
        ));
        assert!(core::ptr::eq(
            capi::object::PyObject_RichCompare(
                singletons::none(),
                singletons::none(),
                typeobject::Py_EQ
            ),
            singletons::true_(),
        ));

        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, other, typeobject::Py_EQ),
            1
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, two, typeobject::Py_EQ),
            0
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, two, typeobject::Py_NE),
            1
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, two, typeobject::Py_LT),
            1
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, two, typeobject::Py_GE),
            0
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, float_one, typeobject::Py_EQ),
            1,
            "int and float compare by value"
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, text("x"), typeobject::Py_EQ),
            0
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(one, text("x"), typeobject::Py_NE),
            1
        );
        assert_eq!(
            capi::object::PyObject_RichCompareBool(text("a"), text("b"), typeobject::Py_LT),
            1,
            "strings order lexicographically"
        );
        drop(float_one);
        drop(two);
        drop(other);
        drop(one);
    }
}

#[test]
fn object_type_checks() {
    init();
    // SAFETY: every call takes valid objects.
    unsafe {
        let five = int(5);
        let long_type: *mut PyObject = (&raw mut long::PyLong_Type).cast();
        let bool_type: *mut PyObject = (&raw mut bool_::PyBool_Type).cast();
        let float_type: *mut PyObject = (&raw mut float_::PyFloat_Type).cast();
        assert!(core::ptr::eq(
            capi::object::PyObject_Type(five),
            long_type.cast()
        ));
        assert!(core::ptr::eq(
            capi::object::PyObject_Type(singletons::true_()),
            bool_type.cast(),
        ));

        assert_eq!(capi::object::PyObject_IsInstance(five, long_type), 1);
        assert_eq!(
            capi::object::PyObject_IsInstance(singletons::true_(), long_type),
            1
        );
        assert_eq!(
            capi::object::PyObject_IsInstance(singletons::true_(), bool_type),
            1
        );
        assert_eq!(capi::object::PyObject_IsInstance(five, float_type), 0);
        let classes = tuple::PyTuple_New(2);
        counted::Py_IncRef(float_type);
        tuple::PyTuple_SetItem(classes, 0, float_type);
        tuple::PyTuple_SetItem(classes, 1, long_type);
        assert_eq!(capi::object::PyObject_IsInstance(five, classes), 1);
        drop(classes);

        assert_eq!(capi::object::PyObject_IsSubclass(bool_type, long_type), 1);
        assert_eq!(capi::object::PyObject_IsSubclass(long_type, bool_type), 0);
        let classes = tuple::PyTuple_New(2);
        counted::Py_IncRef(float_type);
        tuple::PyTuple_SetItem(classes, 0, float_type);
        tuple::PyTuple_SetItem(classes, 1, long_type);
        assert_eq!(capi::object::PyObject_IsSubclass(bool_type, classes), 1);
        drop(classes);

        assert_eq!(capi::object::_PyObject_RealIsInstance(five, long_type), 1);
        assert_eq!(
            capi::object::_PyObject_RealIsSubclass(bool_type, long_type),
            1
        );
        assert_eq!(capi::object::_PyObject_RealIsInstance(five, float_type), 0);
        drop(five);
    }
}

#[test]
fn object_length_dir_format_and_print() {
    init();
    // SAFETY: every call takes a valid object or the CRT file the tests
    // opened.
    unsafe {
        let values = list_of(&[1, 2, 3]);
        assert_eq!(capi::object::PyObject_Length(values), 3);
        assert_eq!(capi::object::PyObject_Size(values), 3);
        assert_eq!(capi::object::PyObject_LengthHint(values, 9), 3);
        assert_eq!(capi::object::PyObject_LengthHint(int(5), 9), 9);
        assert_eq!(capi::object::PyObject_LengthHint(singletons::none(), 7), 7);
        drop(values);

        let unmeasured = capi::object::PyObject_Length(int(5));
        assert_eq!(unmeasured, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "object of type 'int' has no len()"
        );

        let module = python312::modules::PyModule_New(c"listing".as_ptr());
        assert_eq!(
            capi::object::PyObject_SetAttrString(module, c"zeta".as_ptr(), int(1)),
            0
        );
        assert_eq!(
            capi::object::PyObject_SetAttrString(module, c"alpha".as_ptr(), int(2)),
            0
        );
        let names = capi::object::PyObject_Dir(module);
        // The seeded module entries sort ahead of the test's own names.
        assert_eq!(list::PyList_Size(names), 7);
        assert_text(list::PyList_GetItem(names, 0), "__doc__");
        assert_text(list::PyList_GetItem(names, 1), "__loader__");
        assert_text(list::PyList_GetItem(names, 2), "__name__");
        assert_text(list::PyList_GetItem(names, 3), "__package__");
        assert_text(list::PyList_GetItem(names, 4), "__spec__");
        assert_text(list::PyList_GetItem(names, 5), "alpha");
        assert_text(list::PyList_GetItem(names, 6), "zeta");
        drop(names);
        drop(module);

        assert_text(
            capi::object::PyObject_Format(int(5), core::ptr::null_mut()),
            "5",
        );
        assert_text(capi::object::PyObject_Format(int(5), text("")), "5");
        let literal = text("same");
        assert!(core::ptr::eq(
            capi::object::PyObject_Format(literal, core::ptr::null_mut()),
            literal,
        ));
        drop(capi::object::PyObject_Format(literal, text("")));
        drop(literal);
        let refused = capi::object::PyObject_Format(int(5), singletons::none());
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::system_error()),
            "Format specifier must be a string, not NoneType"
        );
        let refused = capi::object::PyObject_Format(int(255), text("x"));
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "unsupported format string passed to int.__format__"
        );

        let file = tmpfile();
        assert!(!file.is_null());
        assert_eq!(capi::object::PyObject_Print(int(5), file, 0), 0);
        let (out, end) = printed(file, 0);
        assert_eq!(out, "5");
        assert_eq!(capi::object::PyObject_Print(text("hi"), file, 1), 0);
        let (out, end) = printed(file, end);
        assert_eq!(out, "hi");
        assert_eq!(
            capi::object::PyObject_Print(core::ptr::null_mut(), file, 0),
            0
        );
        let (out, _) = printed(file, end);
        assert_eq!(out, "<nil>");
        close(file);
    }
}

/// The CRT's temporary stream the print tests write into.
unsafe fn tmpfile() -> *mut c_void {
    unsafe extern "C" {
        fn tmpfile() -> *mut c_void;
    }
    // SAFETY: the CRT call stands alone.
    unsafe { tmpfile() }
}

/// Close the stream the print tests wrote into.
unsafe fn close(file: *mut c_void) {
    unsafe extern "C" {
        fn fclose(stream: *mut c_void) -> c_int;
    }
    // SAFETY: the CRT call takes the stream `tmpfile` returned.
    unsafe { assert_eq!(fclose(file), 0) };
}

/// The bytes written to `file` since offset `from`, flushed, with the new
/// end offset for the next call.
unsafe fn printed(file: *mut c_void, from: usize) -> (String, usize) {
    unsafe extern "C" {
        fn fflush(stream: *mut c_void) -> c_int;
        fn fseek(stream: *mut c_void, offset: core::ffi::c_long, whence: c_int) -> c_int;
        fn ftell(stream: *mut c_void) -> core::ffi::c_long;
        fn fread(ptr: *mut c_void, size: usize, count: usize, stream: *mut c_void) -> usize;
    }
    // SAFETY: the CRT calls take the stream `tmpfile` returned.
    unsafe {
        assert_eq!(fflush(file), 0);
        let end = ftell(file);
        assert!(end >= 0);
        assert!(end as usize >= from);
        assert_eq!(fseek(file, from as core::ffi::c_long, 0), 0);
        let mut buffer = vec![0u8; end as usize - from];
        let read = fread(buffer.as_mut_ptr().cast(), 1, buffer.len(), file);
        assert_eq!(read, buffer.len());
        assert_eq!(fseek(file, 0, 2), 0);
        (String::from_utf8_lossy(&buffer).into_owned(), end as usize)
    }
}

#[test]
fn object_finalizer_entry_points() {
    init();
    // A type without tp_finalize must pass through both entry points
    // without disturbing the object or the error state.
    // SAFETY: the objects are valid runtime objects the test owns.
    unsafe {
        capi::object::PyObject_CallFinalizer(int(5));
        assert!(tstate::occurred().is_null());

        // `PyObject_CallFinalizerFromDealloc` serves objects whose
        // deallocation has begun: CPython asserts the count is already
        // zero. The test owns the list's only reference and hands it over
        // the same way, then takes it back.
        let object = list_of(&[1]);
        // SAFETY: the header write is within the object the test owns.
        (*object).ob_refcnt = 0;
        assert_eq!(capi::object::PyObject_CallFinalizerFromDealloc(object), 0);
        // SAFETY: the header reads and write are within the object.
        assert_eq!((*object).ob_refcnt, 0);
        (*object).ob_refcnt = 1;
        drop(object);
        assert!(tstate::occurred().is_null());
    }
}

#[test]
fn object_file_descriptors() {
    init();
    // SAFETY: every call takes a valid object; failures leave one
    // pending error `take_error` clears.
    unsafe {
        assert_eq!(capi::object::PyObject_AsFileDescriptor(int(7)), 7);
        assert_eq!(capi::object::PyObject_AsFileDescriptor(int(-1)), -1);
        take_error(exc::value_error());
        let refused = capi::object::PyObject_AsFileDescriptor(text("x"));
        assert_eq!(refused, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "argument must be an int, or have a fileno() method."
        );
        let refused = capi::object::PyObject_AsFileDescriptor(singletons::none());
        assert_eq!(refused, -1);
        assert_eq!(
            take_error(exc::type_error()),
            "argument must be an int, or have a fileno() method."
        );
    }
}

#[test]
fn buffer_protocol_on_bytes() {
    init();
    // SAFETY: the calls follow the buffer protocol's contract.
    unsafe {
        let blob = bytes_of(b"abc");
        assert_eq!(pabs::PyObject_CheckBuffer(blob), 1);
        assert_eq!(pabs::PyObject_CheckBuffer(int(5)), 0);

        let mut view = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(blob, &raw mut view, PyBUF_SIMPLE),
            0
        );
        assert_eq!(view.len, 3);
        assert_eq!(view.itemsize, 1);
        assert_eq!(view.readonly, 1);
        assert_eq!(view.ndim, 1);
        assert!(core::ptr::eq(view.obj, blob));
        let data = core::slice::from_raw_parts(view.buf.cast::<u8>(), 3);
        assert_eq!(data, b"abc");
        bytes::PyBuffer_Release(&raw mut view);
        assert!(view.obj.is_null(), "release drops the exporter");

        let mut view = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(blob, &raw mut view, PyBUF_WRITABLE),
            -1
        );
        assert_eq!(take_error(exc::buffer_error()), "Object is not writable.");

        let mut view = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(blob, &raw mut view, PyBUF_SIMPLE | PyBUF_FORMAT),
            0
        );
        assert_eq!(core::ffi::CStr::from_ptr(view.format).to_bytes(), b"B");
        bytes::PyBuffer_Release(&raw mut view);

        let mut view = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(int(5), &raw mut view, PyBUF_SIMPLE),
            -1
        );
        assert_eq!(
            take_error(exc::type_error()),
            "a bytes-like object is required, not 'int'"
        );
        drop(blob);
    }
}

#[test]
fn buffer_fill_info_and_helpers() {
    init();
    let mut backing = [0u8; 4];
    // SAFETY: the calls follow each entry point's contract.
    unsafe {
        let mut view = blank_view();
        assert_eq!(
            bytes::PyBuffer_FillInfo(
                &raw mut view,
                core::ptr::null_mut(),
                backing.as_mut_ptr().cast(),
                4,
                0,
                PyBUF_SIMPLE | PyBUF_STRIDES,
            ),
            0,
        );
        assert!(!view.strides.is_null(), "STRIDES fills the strides");
        assert!(view.obj.is_null());
        assert_eq!(view.len, 4);
        assert_eq!(view.readonly, 0);

        let refused = bytes::PyBuffer_FillInfo(
            &raw mut view,
            core::ptr::null_mut(),
            backing.as_mut_ptr().cast(),
            4,
            1,
            PyBUF_WRITABLE,
        );
        assert_eq!(refused, -1);
        assert_eq!(take_error(exc::buffer_error()), "Object is not writable.");

        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'C' as c_char), 1);
        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'F' as c_char), 1);
        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'A' as c_char), 1);

        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"B".as_ptr()), 1);
        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"h".as_ptr()), 2);
        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"i".as_ptr()), 4);
        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"q".as_ptr()), 8);
        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"f".as_ptr()), 4);
        assert_eq!(pabs::PyBuffer_SizeFromFormat(c"d".as_ptr()), 8);
        let unsupported = pabs::PyBuffer_SizeFromFormat(c"z".as_ptr());
        assert_eq!(unsupported, -1);
        take_error(exc::not_implemented_error());

        let mut shape = [2isize, 3];
        let mut strides = [0isize; 2];
        pabs::PyBuffer_FillContiguousStrides(
            2,
            shape.as_mut_ptr(),
            strides.as_mut_ptr(),
            4,
            b'C' as c_char,
        );
        assert_eq!(strides, [12, 4]);
        pabs::PyBuffer_FillContiguousStrides(
            2,
            shape.as_mut_ptr(),
            strides.as_mut_ptr(),
            4,
            b'F' as c_char,
        );
        assert_eq!(strides, [4, 8]);

        let indices = [3isize];
        let pointer = pabs::PyBuffer_GetPointer(&view, indices.as_ptr());
        assert!(core::ptr::eq(
            pointer.cast::<u8>(),
            view.buf.cast::<u8>().add(3),
        ));
    }
}

#[test]
fn buffer_contiguous_copies_round_trip() {
    init();
    // A 2x3 float64-free lattice of four-byte elements in Fortran order:
    // element (i, j) lives at byte offset 4*i + 8*j, values 0..5.
    let mut backing: [u8; 24] = [0; 24];
    for element in 0..6isize {
        let offset = (element * 4) as usize;
        backing[offset..offset + 4].copy_from_slice(&(element as u32).to_le_bytes());
    }
    let shape = [2isize, 3];
    let strides = [4isize, 8];
    // SAFETY: the view describes `backing`, and the copy entry points
    // stay inside the six elements.
    unsafe {
        let view = Py_buffer {
            buf: backing.as_mut_ptr().cast(),
            obj: core::ptr::null_mut(),
            len: 24,
            itemsize: 4,
            readonly: 1,
            ndim: 2,
            format: c"i".as_ptr().cast_mut(),
            shape: shape.as_ptr().cast_mut(),
            strides: strides.as_ptr().cast_mut(),
            suboffsets: core::ptr::null_mut(),
            internal: core::ptr::null_mut(),
        };
        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'C' as c_char), 0);
        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'F' as c_char), 1);
        assert_eq!(pabs::PyBuffer_IsContiguous(&view, b'A' as c_char), 1);

        let mut linearized = [0u8; 24];
        assert_eq!(
            capi::memoryobject::PyBuffer_ToContiguous(
                linearized.as_mut_ptr().cast(),
                &view,
                24,
                b'C' as c_char,
            ),
            0,
        );
        let c_order: Vec<u32> = (0..6)
            .map(|i| u32::from_le_bytes(linearized[i * 4..i * 4 + 4].try_into().unwrap()))
            .collect();
        assert_eq!(c_order, vec![0, 2, 4, 1, 3, 5], "C order walks rows");

        // The inverse: a Fortran-order destination refilled from the
        // linearized buffer restores the original layout.
        let mut restored: [u8; 24] = [0; 24];
        let destination = Py_buffer {
            buf: restored.as_mut_ptr().cast(),
            obj: core::ptr::null_mut(),
            len: 24,
            itemsize: 4,
            readonly: 0,
            ndim: 2,
            format: c"i".as_ptr().cast_mut(),
            shape: shape.as_ptr().cast_mut(),
            strides: strides.as_ptr().cast_mut(),
            suboffsets: core::ptr::null_mut(),
            internal: core::ptr::null_mut(),
        };
        // The `fort` argument describes the source buffer's layout, not
        // the destination's: with 'F' the F-contiguous destination takes
        // CPython's memcpy branch and receives the bytes verbatim, while
        // 'C' walks the destination last-axis-fastest and undoes the
        // linearization above.
        assert_eq!(
            pabs::PyBuffer_FromContiguous(
                &destination,
                linearized.as_ptr().cast(),
                24,
                b'F' as c_char,
            ),
            0,
        );
        assert_eq!(restored, linearized);
        assert_eq!(
            pabs::PyBuffer_FromContiguous(
                &destination,
                linearized.as_ptr().cast(),
                24,
                b'C' as c_char,
            ),
            0,
        );
        assert_eq!(restored, backing);
    }
}

#[test]
fn memoryview_from_object_and_memory() {
    init();
    // SAFETY: the calls follow the memoryview entry points' contracts.
    unsafe {
        let blob = bytes_of(b"hello");
        let view = capi::memoryobject::PyMemoryView_FromObject(blob);
        assert!(!view.is_null());
        assert_eq!(pabs::PyObject_CheckBuffer(view), 1);
        let mut exported = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(view, &raw mut exported, PyBUF_SIMPLE),
            0
        );
        assert_eq!(exported.len, 5);
        let data = core::slice::from_raw_parts(exported.buf.cast::<u8>(), 5);
        assert_eq!(data, b"hello");
        bytes::PyBuffer_Release(&raw mut exported);
        assert_eq!(
            capi::object::PyObject_Hash(view),
            capi::object::PyObject_Hash(blob),
        );
        drop(view);
        drop(blob);

        let mut memory = *b"hello";
        let writable =
            capi::memoryobject::PyMemoryView_FromMemory(memory.as_mut_ptr().cast(), 5, PyBUF_WRITE);
        assert!(!writable.is_null());
        let unhashable = capi::object::PyObject_Hash(writable);
        assert_eq!(unhashable, -1);
        assert_eq!(
            take_error(exc::value_error()),
            "cannot hash writable memoryview object"
        );
        drop(writable);

        let readonly =
            capi::memoryobject::PyMemoryView_FromMemory(memory.as_mut_ptr().cast(), 5, PyBUF_READ);
        assert!(!readonly.is_null());
        assert_eq!(
            capi::object::PyObject_Hash(readonly),
            capi::object::PyObject_Hash(bytes_of(b"hello")),
        );
        drop(readonly);

        let mut info = blank_view();
        assert_eq!(
            bytes::PyBuffer_FillInfo(
                &raw mut info,
                core::ptr::null_mut(),
                memory.as_mut_ptr().cast(),
                5,
                1,
                PyBUF_SIMPLE,
            ),
            0,
        );
        let from_buffer = capi::memoryobject::PyMemoryView_FromBuffer(&info);
        assert!(!from_buffer.is_null());
        let mut exported = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(from_buffer, &raw mut exported, PyBUF_SIMPLE),
            0,
        );
        assert_eq!(exported.len, 5);
        bytes::PyBuffer_Release(&raw mut exported);
        drop(from_buffer);

        let contiguous = capi::memoryobject::PyMemoryView_GetContiguous(
            bytes_of(b"hello"),
            PyBUF_READ,
            b'C' as c_char,
        );
        assert!(!contiguous.is_null());
        let mut exported = blank_view();
        assert_eq!(
            bytes::PyObject_GetBuffer(contiguous, &raw mut exported, PyBUF_SIMPLE),
            0,
        );
        let data = core::slice::from_raw_parts(exported.buf.cast::<u8>(), 5);
        assert_eq!(data, b"hello");
        bytes::PyBuffer_Release(&raw mut exported);
        drop(contiguous);

        let refused = capi::memoryobject::PyMemoryView_GetContiguous(
            bytes_of(b"hello"),
            PyBUF_WRITE,
            b'C' as c_char,
        );
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::buffer_error()),
            "underlying buffer is not writable"
        );
    }
}

#[test]
// The capsule contract is pointer identity of the stored destructor.
#[allow(unpredictable_function_pointer_comparisons)]
fn capsules_round_trip_and_report_their_misuse() {
    init();
    static PAYLOAD: [u8; 2] = [1, 2];
    // SAFETY: the calls follow the capsule entry points' contracts.
    unsafe {
        let name = c"coil.test.capsule".as_ptr();
        let wrong = c"coil.other".as_ptr();
        let capsule =
            capi::capsule::PyCapsule_New(PAYLOAD.as_ptr().cast::<c_void>().cast_mut(), name, None);
        assert!(!capsule.is_null());
        assert_eq!(capi::capsule::PyCapsule_IsValid(capsule, name), 1);
        assert_eq!(capi::capsule::PyCapsule_IsValid(capsule, wrong), 0);
        assert_eq!(capi::capsule::PyCapsule_IsValid(int(5), name), 0);
        assert!(core::ptr::eq(
            capi::capsule::PyCapsule_GetPointer(capsule, name),
            PAYLOAD.as_ptr().cast::<c_void>().cast_mut(),
        ));
        assert!(core::ptr::eq(
            capi::capsule::PyCapsule_GetName(capsule),
            name
        ));
        assert!(capi::capsule::PyCapsule_GetDestructor(capsule).is_none());
        assert!(capi::capsule::PyCapsule_GetContext(capsule).is_null());

        static OTHER: [u8; 1] = [9];
        assert_eq!(
            capi::capsule::PyCapsule_SetPointer(
                capsule,
                OTHER.as_ptr().cast::<c_void>().cast_mut()
            ),
            0,
        );
        assert!(core::ptr::eq(
            capi::capsule::PyCapsule_GetPointer(capsule, name),
            OTHER.as_ptr().cast::<c_void>().cast_mut(),
        ));
        let refused = capi::capsule::PyCapsule_SetPointer(capsule, core::ptr::null_mut());
        assert_eq!(refused, -1);
        assert_eq!(
            take_error(exc::value_error()),
            "PyCapsule_SetPointer called with null pointer"
        );

        assert_eq!(capi::capsule::PyCapsule_SetName(capsule, wrong), 0);
        assert!(core::ptr::eq(
            capi::capsule::PyCapsule_GetName(capsule),
            wrong
        ));
        let refused = capi::capsule::PyCapsule_GetPointer(capsule, name);
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::value_error()),
            "PyCapsule_GetPointer called with incorrect name"
        );
        assert_eq!(capi::capsule::PyCapsule_SetName(capsule, name), 0);

        assert_eq!(
            capi::capsule::PyCapsule_SetContext(
                capsule,
                PAYLOAD.as_ptr().cast::<c_void>().cast_mut()
            ),
            0,
        );
        assert!(core::ptr::eq(
            capi::capsule::PyCapsule_GetContext(capsule),
            PAYLOAD.as_ptr().cast::<c_void>().cast_mut(),
        ));
        assert_eq!(
            capi::capsule::PyCapsule_SetDestructor(capsule, Some(capsule_note)),
            0,
        );
        let destructor = capi::capsule::PyCapsule_GetDestructor(capsule);
        assert!(destructor == Some(capsule_note as unsafe extern "C" fn(*mut PyObject)));

        let invalid = capi::capsule::PyCapsule_GetPointer(int(5), name);
        assert!(invalid.is_null());
        assert_eq!(
            take_error(exc::value_error()),
            "PyCapsule_GetPointer called with invalid PyCapsule object"
        );
        let refused = capi::capsule::PyCapsule_New(core::ptr::null_mut(), name, None);
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::value_error()),
            "PyCapsule_New called with null pointer"
        );

        let representation = utf8(unicode::PyObject_Repr(capsule));
        assert!(representation.starts_with("<capsule object \"coil.test.capsule\" at "));

        let hits = CAPSULE_DTOR_HITS.load(Ordering::SeqCst);
        drop(capsule);
        assert_eq!(CAPSULE_DTOR_HITS.load(Ordering::SeqCst), hits + 1);

        let imported = capi::capsule::PyCapsule_Import(c"coil.nowhere".as_ptr(), 0);
        assert!(imported.is_null());
        assert_eq!(
            take_error(exc::import_error()),
            "PyCapsule_Import could not import module \"coil\""
        );
    }
}

#[test]
fn gc_tracking_and_allocation_round_trips() {
    init();
    // SAFETY: the calls follow each entry point's contract.
    unsafe {
        let values = list_of(&[1]);
        assert_eq!(capi::gc::PyObject_GC_IsTracked(values), 1);
        assert_eq!(capi::gc::PyObject_GC_IsTracked(int(5)), 0);
        assert_eq!(capi::gc::PyObject_GC_IsFinalized(values), 0);
        capi::gc::PyObject_GC_UnTrack(values);
        capi::gc::PyObject_GC_Track(values);
        drop(values);

        let mapping = dict_of(&[("k", 1)]);
        assert_eq!(capi::gc::PyObject_IS_GC(mapping), 1);
        drop(mapping);
        assert_eq!(capi::gc::PyObject_IS_GC(int(5)), 0);

        let allocated = capi::gc::_PyObject_GC_New(&raw mut dict::PyDict_Type);
        assert!(!allocated.is_null());
        assert!(core::ptr::eq(
            type_of(allocated),
            &raw mut dict::PyDict_Type
        ));
        capi::gc::PyObject_GC_Del(allocated);

        let sized = capi::gc::_PyObject_GC_NewVar(&raw mut dict::PyDict_Type, 0);
        assert!(!sized.is_null());
        assert_eq!((*sized.cast::<PyVarObject>()).ob_size, 0);
        capi::gc::PyObject_GC_Del(sized.cast());

        let block = python312::alloc::pymem::PyObject_Malloc(64);
        assert!(!block.is_null());
        let initialized = counted::PyObject_Init(block.cast(), &raw mut dict::PyDict_Type);
        assert!(core::ptr::eq(initialized, block.cast()));
        assert!(core::ptr::eq(
            type_of(initialized),
            &raw mut dict::PyDict_Type
        ));
        python312::alloc::pymem::PyObject_Free(block.cast());

        let block = python312::alloc::pymem::PyObject_Malloc(64);
        let initialized = capi::gc::PyObject_InitVar(block.cast(), &raw mut dict::PyDict_Type, 3);
        assert_eq!((*initialized.cast::<PyVarObject>()).ob_size, 3);
        python312::alloc::pymem::PyObject_Free(initialized.cast());

        let sized = capi::gc::_PyObject_NewVar(&raw mut dict::PyDict_Type, 2);
        assert!(!sized.is_null());
        assert_eq!((*sized).ob_size, 2);
        capi::gc::PyObject_GC_Del(sized.cast());

        let refused =
            capi::gc::PyObject_InitVar(core::ptr::null_mut(), &raw mut dict::PyDict_Type, 0);
        assert!(refused.is_null());
        take_error(exc::memory_error());
    }
}

#[test]
fn weakref_clearing_runs_callbacks() {
    init();
    let long_type: *mut PyObject = (&raw mut long::PyLong_Type).cast();
    let hits = WEAKREF_CB_HITS.load(Ordering::SeqCst);
    // SAFETY: the calls follow the weak-reference entry points'
    // contracts.
    unsafe {
        let callback = cfunction(&WEAKREF_NOTE_DEF);
        let reference = python312::gc::weakref::PyWeakref_NewRef(long_type, callback);
        assert!(!reference.is_null());
        let mut referent: *mut PyObject = core::ptr::null_mut();
        assert_eq!(
            python312::gc::weakref::PyWeakref_GetRef(reference, &raw mut referent),
            1
        );
        assert!(core::ptr::eq(referent, long_type));
        drop(referent);

        capi::object::PyObject_ClearWeakRefs(long_type);
        assert_eq!(WEAKREF_CB_HITS.load(Ordering::SeqCst), hits + 1);
        referent = core::ptr::null_mut();
        assert_eq!(
            python312::gc::weakref::PyWeakref_GetRef(reference, &raw mut referent),
            0
        );
        assert!(referent.is_null());
        drop(reference);
        drop(callback);

        let refused = python312::gc::weakref::PyWeakref_NewRef(int(5), core::ptr::null_mut());
        assert!(refused.is_null());
        assert_eq!(
            take_error(exc::type_error()),
            "cannot create weak reference to 'int' object"
        );
        let refused = python312::gc::weakref::PyWeakref_NewRef(long_type, int(5));
        assert!(refused.is_null());
        assert_eq!(take_error(exc::type_error()), "callback must be callable");
        let refused = python312::gc::weakref::PyWeakref_NewProxy(long_type, core::ptr::null_mut());
        assert!(refused.is_null());
        take_error(exc::not_implemented_error());
    }
}

#[test]
fn allocator_domains_zero_and_preserve() {
    init();
    // SAFETY: the blocks come from the matching allocator domain.
    unsafe {
        let block = python312::alloc::pymem::PyMem_Calloc(4, 8);
        assert!(!block.is_null());
        let words = core::slice::from_raw_parts(block.cast::<u64>(), 4);
        assert!(words.iter().all(|word| *word == 0), "calloc zeroes");
        let grown = python312::alloc::pymem::PyMem_Realloc(block, 16);
        assert!(!grown.is_null());
        python312::alloc::pymem::PyMem_Free(grown);

        let block = python312::alloc::pymem::PyObject_Calloc(2, 16);
        assert!(!block.is_null());
        let words = core::slice::from_raw_parts(block.cast::<u64>(), 4);
        assert!(words.iter().all(|word| *word == 0));
        python312::alloc::pymem::PyObject_Free(block.cast());

        let block = python312::alloc::pymem::PyMem_RawMalloc(8);
        assert!(!block.is_null());
        core::ptr::write_bytes(block.cast::<u8>(), 0x5a, 8);
        let block = python312::alloc::pymem::PyMem_RawRealloc(block, 32);
        assert!(!block.is_null());
        let bytes_out = core::slice::from_raw_parts(block.cast::<u8>(), 8);
        assert!(bytes_out.iter().all(|byte| *byte == 0x5a));
        python312::alloc::pymem::PyMem_RawFree(block);
    }
}

/// The manifest's names under this task's header families: the abstract
/// object protocol (`PyObject_*`, `_PyObject_*`), number, sequence and
/// mapping protocols, iterators, buffers and memoryview, capsules,
/// vectorcall and the allocator entry points.
const FAMILY_PREFIXES: &[&str] = &[
    "PyObject_",
    "_PyObject_",
    "PyNumber_",
    "_PyNumber_",
    "PySequence_",
    "_PySequence_",
    "PyMapping_",
    "PyIter_",
    "PyCallable_",
    "PyBuffer_",
    "PyMemoryView_",
    "PyCapsule_",
    "PyVectorcall_",
    "PySeqIter_",
    "PyCallIter_",
    "PyMem_",
];

#[test]
fn no_family_stub_remains_in_the_manifest() {
    init();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifest/capi_targets.txt");
    let manifest = fs::read_to_string(path).unwrap();
    let mut checked = 0;
    for line in manifest.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split(' ').next().expect("manifest rows are names");
        if FAMILY_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            assert_eq!(
                capi::is_stub(name),
                Some(false),
                "{name} must be past its stub"
            );
            checked += 1;
        }
    }
    assert!(checked > 150, "the family filter must match the manifest");
}

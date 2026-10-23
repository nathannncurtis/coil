//! Layout and object-model tests for `coil-rt`'s headers, singletons and
//! type objects, against `coil-rt-abi`'s pinned numbers (spec sections 8.1,
//! 8.2, 8.5).

use std::sync::Once;

use coil_capi_sys::{Py_TPFLAGS_DISALLOW_INSTANTIATION, Py_TPFLAGS_READY, PyTypeObject};
use python312::object::{header, singletons, typeobject};
use python312::types::exc as exceptions;
use python312::types::{bool_, complex, float_, long};

const INT_TYPE: *mut PyTypeObject = &raw mut long::PyLong_Type;
const BOOL_TYPE: *mut PyTypeObject = &raw mut bool_::PyBool_Type;
const FLOAT_TYPE: *mut PyTypeObject = &raw mut float_::PyFloat_Type;
const COMPLEX_TYPE: *mut PyTypeObject = &raw mut complex::PyComplex_Type;
const OBJECT_TYPE: *mut PyTypeObject = &raw mut typeobject::PyBaseObject_Type;
const TYPE_TYPE: *mut PyTypeObject = &raw mut typeobject::PyType_Type;

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

/// The static types' `tp_name`s, read once after init.
fn tp_name(t: *mut PyTypeObject) -> &'static str {
    // SAFETY: every static type readied by `init` has a static `tp_name`.
    unsafe { std::ffi::CStr::from_ptr((*t).tp_name).to_str().unwrap() }
}

/// A slot address as a comparable integer. `f` is always a concrete fn
/// pointer here, so the cast is well-defined.
macro_rules! slot_addr {
    ($slot:expr) => {
        $slot.map(|f| f as usize)
    };
}

#[test]
fn object_layouts_match_the_abi() {
    use coil_rt_abi::cp312;
    assert_eq!(
        std::mem::offset_of!(coil_capi_sys::PyObject, ob_refcnt),
        cp312::object::OB_REFCNT
    );
    assert_eq!(
        std::mem::offset_of!(coil_capi_sys::PyObject, ob_type),
        cp312::object::OB_TYPE
    );
    assert_eq!(
        std::mem::size_of::<coil_capi_sys::PyObject>(),
        cp312::object::SIZEOF
    );

    assert_eq!(
        std::mem::offset_of!(long::PyLongObject, lv_tag),
        cp312::long::LV_TAG
    );
    assert_eq!(
        std::mem::offset_of!(long::PyLongObject, ob_digit),
        cp312::long::OB_DIGIT
    );
    assert_eq!(
        std::mem::size_of::<long::PyLongObject>(),
        cp312::long::SIZEOF
    );

    assert_eq!(
        std::mem::offset_of!(float_::PyFloatObject, ob_fval),
        cp312::float::OB_FVAL
    );
    assert_eq!(
        std::mem::size_of::<float_::PyFloatObject>(),
        cp312::float::SIZEOF
    );

    assert_eq!(
        std::mem::offset_of!(complex::PyComplexObject, cval),
        cp312::complex::CVAL
    );
    assert_eq!(
        std::mem::size_of::<complex::PyComplexObject>(),
        cp312::complex::SIZEOF
    );

    // `bool` instances are `PyLongObject`s; the ABI pins the full size.
    assert_eq!(
        std::mem::size_of::<long::PyLongObject>(),
        cp312::bool::SIZEOF
    );
}

#[test]
fn static_types_ready_idempotently() {
    init();
    for t in [
        INT_TYPE,
        BOOL_TYPE,
        FLOAT_TYPE,
        COMPLEX_TYPE,
        OBJECT_TYPE,
        TYPE_TYPE,
        singletons::none_type(),
        singletons::ellipsis_type(),
        singletons::not_implemented_type(),
        exceptions::base_exception_type(),
    ] {
        // SAFETY: each type was readied by `init`.
        let flags = unsafe { (*t).tp_flags };
        assert_ne!(flags & Py_TPFLAGS_READY, 0, "{}", tp_name(t));
    }
    assert_eq!(typeobject::init_static_types(), 0);
}

#[test]
fn type_objects_match_cpython_sizes_and_names() {
    init();
    // SAFETY: reading static, readied types field by field.
    unsafe {
        assert_eq!(tp_name(INT_TYPE), "int");
        assert_eq!((*INT_TYPE).tp_basicsize, 24);
        assert_eq!((*INT_TYPE).tp_itemsize, 4);

        assert_eq!(tp_name(BOOL_TYPE), "bool");
        assert_eq!((*BOOL_TYPE).tp_basicsize, 24);
        assert_eq!((*BOOL_TYPE).tp_itemsize, 4);

        assert_eq!(tp_name(FLOAT_TYPE), "float");
        assert_eq!((*FLOAT_TYPE).tp_basicsize, 24);

        assert_eq!(tp_name(COMPLEX_TYPE), "complex");
        assert_eq!((*COMPLEX_TYPE).tp_basicsize, 32);

        assert_eq!(tp_name(OBJECT_TYPE), "object");
        assert_eq!((*OBJECT_TYPE).tp_basicsize, 16);
        assert_eq!(tp_name(TYPE_TYPE), "type");

        assert!(core::ptr::eq((*BOOL_TYPE).tp_base, INT_TYPE));
        assert!(core::ptr::eq((*INT_TYPE).tp_base, OBJECT_TYPE));
        assert!(core::ptr::eq((*FLOAT_TYPE).tp_base, OBJECT_TYPE));
        // Every type object's own type is `type`.
        assert!(core::ptr::eq((*INT_TYPE).ob_type, TYPE_TYPE));
        assert!(core::ptr::eq((*TYPE_TYPE).ob_type, TYPE_TYPE));
    }
}

#[test]
fn singletons_are_immortal_and_shared() {
    init();
    for object in [
        singletons::none(),
        singletons::true_(),
        singletons::false_(),
        singletons::not_implemented(),
        singletons::ellipsis(),
    ] {
        // SAFETY: immortal singletons this runtime produced.
        unsafe {
            assert_eq!(header::refcnt(object), header::IMMORTAL_REFCNT);
            assert!(header::is_immortal(object));
        }
    }
    // SAFETY: allocation results are checked before any dereference.
    unsafe {
        assert!(core::ptr::eq(
            header::type_of(singletons::none()),
            singletons::none_type()
        ));
        assert!(core::ptr::eq(
            header::type_of(singletons::true_()),
            BOOL_TYPE
        ));
        assert!(core::ptr::eq(
            header::type_of(singletons::ellipsis()),
            singletons::ellipsis_type()
        ));
    }
    assert!(core::ptr::eq(
        bool_::PyBool_FromLong(1),
        singletons::true_()
    ));
    assert!(core::ptr::eq(
        bool_::PyBool_FromLong(0),
        singletons::false_()
    ));
    assert!(core::ptr::eq(
        bool_::PyBool_FromLong(-5),
        singletons::true_()
    ));
}

#[test]
fn small_integers_are_shared() {
    init();
    for value in [-5i64, -1, 0, 1, 2, 255, 256] {
        let singleton = singletons::get_small_int(value);
        assert!(core::ptr::eq(singleton, singletons::get_small_int(value)));
        // SAFETY: reading the singleton's header.
        unsafe {
            assert!(core::ptr::eq(header::type_of(singleton), INT_TYPE));
            assert!(header::is_immortal(singleton));
        }
    }
    assert!(singletons::get_small_int(-6).is_null());
    assert!(singletons::get_small_int(257).is_null());
    // FromLong hands back the singletons in range, fresh objects beyond it.
    assert!(core::ptr::eq(
        long::PyLong_FromLong(42),
        singletons::get_small_int(42)
    ));
    let big = long::PyLong_FromLong(1000);
    assert!(!core::ptr::eq(big, singletons::get_small_int(1000)));
    // SAFETY: `big` came from `PyLong_FromLong`'s heap allocation.
    unsafe { python312::alloc::pymem::PyObject_Free(big.cast()) };
}

#[test]
fn mro_follows_c3_for_the_builtin_chain() {
    init();
    let addresses =
        |ts: &[*mut PyTypeObject]| -> Vec<usize> { ts.iter().map(|t| *t as usize).collect() };
    // SAFETY: all types below were readied by `init`.
    unsafe {
        assert_eq!(
            addresses(&typeobject::mro_of(BOOL_TYPE)),
            addresses(&[BOOL_TYPE, INT_TYPE, OBJECT_TYPE])
        );
        assert_eq!(
            addresses(&typeobject::mro_of(INT_TYPE)),
            addresses(&[INT_TYPE, OBJECT_TYPE])
        );
        assert_eq!(
            addresses(&typeobject::mro_of(TYPE_TYPE)),
            addresses(&[TYPE_TYPE, OBJECT_TYPE])
        );
        assert_eq!(typeobject::PyType_IsSubtype(BOOL_TYPE, INT_TYPE), 1);
        assert_eq!(typeobject::PyType_IsSubtype(INT_TYPE, BOOL_TYPE), 0);
        assert_eq!(typeobject::PyType_IsSubtype(FLOAT_TYPE, OBJECT_TYPE), 1);
    }
}

#[test]
fn slot_inheritance_fills_the_subtypes() {
    init();
    // SAFETY: reading readied static types field by field.
    unsafe {
        let int_hash = slot_addr!((*INT_TYPE).tp_hash);
        assert_eq!(slot_addr!((*BOOL_TYPE).tp_hash), int_hash);
        assert!(!(*FLOAT_TYPE).tp_hash.is_none());
        assert!((*COMPLEX_TYPE).tp_hash.is_some());

        let int_free = slot_addr!((*INT_TYPE).tp_free);
        assert_eq!(slot_addr!((*BOOL_TYPE).tp_free), slot_addr!(int_free));

        // `bool` keeps its own four slots and inherits the rest of `int`'s.
        let bool_table = (*BOOL_TYPE).tp_as_number;
        let int_table = (*INT_TYPE).tp_as_number;
        assert_ne!(
            slot_addr!((*bool_table).nb_and),
            slot_addr!((*int_table).nb_and)
        );
        assert_eq!(
            slot_addr!((*bool_table).nb_add),
            slot_addr!((*int_table).nb_add)
        );
        assert_eq!(
            slot_addr!((*bool_table).nb_multiply),
            slot_addr!((*int_table).nb_multiply)
        );
        assert_eq!(
            slot_addr!((*bool_table).nb_power),
            slot_addr!((*int_table).nb_power)
        );
        assert_eq!(
            slot_addr!((*bool_table).nb_int),
            slot_addr!((*int_table).nb_int)
        );
    }
}

#[test]
fn bool_slots_keep_bool_results_for_bool_operands() {
    init();
    let t = singletons::true_();
    let f = singletons::false_();
    // SAFETY: the ready `bool` table's slots take any objects.
    unsafe {
        let table = (*BOOL_TYPE).tp_as_number;
        let and = (*table).nb_and.unwrap();
        assert!(core::ptr::eq(and(t, f), singletons::false_()));
        assert!(core::ptr::eq(and(t, t), singletons::true_()));
        let xor = (*table).nb_xor.unwrap();
        assert!(core::ptr::eq(xor(t, f), singletons::true_()));
        let or = (*table).nb_or.unwrap();
        assert!(core::ptr::eq(or(f, f), singletons::false_()));
        // `int`'s slots inherited through the table: True + True == 2.
        let add = (*table).nb_add.unwrap();
        let two = add(t, t);
        assert_eq!(long::format(two.cast(), 10, false).as_deref(), Some("2"));
        let invert = (*table).nb_invert.unwrap();
        let minus_two = invert(t);
        assert_eq!(
            long::format(minus_two.cast(), 10, false).as_deref(),
            Some("-2")
        );
        // A `bool` mixed with an `int` goes through `int`'s slot and
        // yields the int, not a bool.
        let five = long::PyLong_FromLong(5);
        assert!(core::ptr::eq(and(t, five), singletons::get_small_int(1)));
    }
}

#[test]
fn number_objects_carry_their_types() {
    init();
    let f = float_::PyFloat_FromDouble(2.5);
    assert!(!f.is_null());
    // SAFETY: allocation results are checked before any dereference.
    unsafe {
        assert!(core::ptr::eq(header::type_of(f), FLOAT_TYPE));
        assert_eq!(float_::PyFloat_AsDouble(f), 2.5);
        assert_eq!(header::refcnt(f), 1);
    }

    let c = complex::PyComplex_FromDoubles(1.5, -2.5);
    assert!(!c.is_null());
    // SAFETY: as above.
    unsafe {
        assert!(core::ptr::eq(header::type_of(c), COMPLEX_TYPE));
        assert_eq!(complex::PyComplex_RealAsDouble(c), 1.5);
        assert_eq!(complex::PyComplex_ImagAsDouble(c), -2.5);
    }
    let from_c = complex::PyComplex_FromCComplex(complex::Py_complex {
        real: 0.0,
        imag: 1.0,
    });
    assert!(!from_c.is_null());
    // SAFETY: as above.
    unsafe {
        assert_eq!(complex::PyComplex_ImagAsDouble(from_c), 1.0);
        assert_eq!(complex::PyComplex_AsCComplex(from_c).real, 0.0);
    }
}

#[test]
fn interim_error_state_round_trips() {
    init();
    python312::tstate::clear();
    assert!(python312::tstate::occurred().is_null());
    python312::tstate::set_string(exceptions::value_error(), b"some message");
    assert!(core::ptr::eq(
        python312::tstate::occurred(),
        exceptions::value_error()
    ));
    assert_eq!(python312::tstate::message(), b"some message");
    python312::tstate::clear();
    assert!(python312::tstate::occurred().is_null());
}

#[test]
fn static_types_are_immortal() {
    init();
    // The exception accessors return the static type objects themselves.
    for ty in [
        exceptions::type_error(),
        exceptions::value_error(),
        exceptions::overflow_error(),
        exceptions::zero_division_error(),
        exceptions::memory_error(),
        exceptions::system_error(),
        exceptions::not_implemented_error(),
    ] {
        // SAFETY: immortal static types this runtime readied.
        unsafe {
            assert_eq!(header::refcnt(ty), header::IMMORTAL_REFCNT);
            assert!(header::is_immortal(ty));
        }
    }
    for ty in [
        OBJECT_TYPE,
        TYPE_TYPE,
        INT_TYPE,
        BOOL_TYPE,
        FLOAT_TYPE,
        COMPLEX_TYPE,
        singletons::none_type(),
        singletons::ellipsis_type(),
        singletons::not_implemented_type(),
        exceptions::base_exception_type(),
    ] {
        // SAFETY: immortal static types this runtime readied.
        unsafe {
            assert_eq!(header::refcnt(ty.cast()), header::IMMORTAL_REFCNT);
            assert!(header::is_immortal(ty.cast()), "{}", tp_name(ty));
        }
    }
}

#[test]
fn identity_richcompare_defers_with_not_implemented() {
    init();
    // SAFETY: allocation results are checked before any dereference.
    unsafe {
        let a = typeobject::PyType_GenericAlloc(OBJECT_TYPE, 0);
        let b = typeobject::PyType_GenericAlloc(OBJECT_TYPE, 0);
        assert!(!a.is_null() && !b.is_null());
        let compare = (*OBJECT_TYPE).tp_richcompare.unwrap();
        // `==` is identity and otherwise NotImplemented so the reflected
        // operand gets its chance; `!=` inverts the `==` answer.
        assert!(core::ptr::eq(
            compare(a, a, typeobject::Py_EQ),
            singletons::true_()
        ));
        assert!(core::ptr::eq(
            compare(a, b, typeobject::Py_EQ),
            singletons::not_implemented()
        ));
        assert!(core::ptr::eq(
            compare(a, a, typeobject::Py_NE),
            singletons::false_()
        ));
        assert!(core::ptr::eq(
            compare(a, b, typeobject::Py_NE),
            singletons::not_implemented()
        ));
        assert!(core::ptr::eq(
            compare(a, b, typeobject::Py_LT),
            singletons::not_implemented()
        ));
        python312::alloc::pymem::PyObject_Free(a.cast());
        python312::alloc::pymem::PyObject_Free(b.cast());
    }
}

#[test]
fn type_object_keeps_its_new_slot() {
    init();
    // SAFETY: reading and calling the static type's slot.
    unsafe {
        // CPython's `type` carries type_new and is never marked
        // DISALLOW_INSTANTIATION; the slot is the spec's stub until the
        // heap-type machinery arrives.
        let new = (*TYPE_TYPE).tp_new.unwrap();
        assert_eq!((*TYPE_TYPE).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION, 0);
        python312::tstate::clear();
        assert!(new(TYPE_TYPE, core::ptr::null_mut(), core::ptr::null_mut()).is_null());
        assert!(core::ptr::eq(
            python312::tstate::occurred(),
            exceptions::system_error()
        ));
        assert_eq!(
            python312::tstate::message(),
            b"coil: type_new is not implemented"
        );
        python312::tstate::clear();
    }
}

#[test]
fn match_self_flags_match_cpython() {
    init();
    // CPython 3.12 sets the private value-comparison hint on `int` and
    // `float` alone among this runtime's types.
    // SAFETY: reading static, readied types.
    unsafe {
        assert_ne!((*INT_TYPE).tp_flags & typeobject::_Py_TPFLAGS_MATCH_SELF, 0);
        assert_ne!(
            (*FLOAT_TYPE).tp_flags & typeobject::_Py_TPFLAGS_MATCH_SELF,
            0
        );
        assert_eq!(
            (*BOOL_TYPE).tp_flags & typeobject::_Py_TPFLAGS_MATCH_SELF,
            0
        );
        assert_eq!(
            (*COMPLEX_TYPE).tp_flags & typeobject::_Py_TPFLAGS_MATCH_SELF,
            0
        );
    }
}

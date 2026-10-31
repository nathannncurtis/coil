//! `iterobject.h`: the sequence and callable iterator types (spec section
//! 10.1). CPython's Python-level methods (`__length_hint__`,
//! `__reduce__`, `__setstate__`) live in `tp_methods` and are not
//! installed here; the iteration protocol is.

use core::ffi::{c_int, c_void};

use coil_capi_sys::{Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, PyObject, PyTypeObject, visitproc};

use crate::object::typeobject::{Py_EQ, PyType_GenericAlloc, zero_type};
use crate::tstate;
use crate::types::exc as exceptions;

/// CPython's `seqiterobject`: iteration state over a sequence.
#[repr(C)]
struct PySeqIterObject {
    ob_base: PyObject,
    it_index: isize,
    it_seq: *mut PyObject,
}

unsafe extern "C" fn iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object is a sequence iterator this runtime allocated.
    unsafe {
        let it = &mut *object.cast::<PySeqIterObject>();
        let seq = core::mem::replace(&mut it.it_seq, core::ptr::null_mut());
        exceptions::release_object(seq);
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn iter_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the field read is within the iterator.
    let referent = unsafe { (*object.cast::<PySeqIterObject>()).it_seq };
    if !referent.is_null() {
        // SAFETY: the visitor contract.
        return unsafe { visit(referent, arg) };
    }
    0
}

unsafe extern "C" fn iter_iternext(iterator: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller passed a sequence iterator per the slot contract.
    unsafe {
        let it = &mut *iterator.cast::<PySeqIterObject>();
        let seq = it.it_seq;
        if seq.is_null() {
            return core::ptr::null_mut();
        }
        if it.it_index == isize::MAX {
            tstate::set_string(exceptions::overflow_error(), b"iter index too large");
            return core::ptr::null_mut();
        }

        let result = crate::capi::r#abstract::PySequence_GetItem(seq, it.it_index);
        if !result.is_null() {
            it.it_index += 1;
            return result;
        }
        // A spent sequence drops the reference and stops quietly; every
        // other error stands.
        let index_or_stop = exceptions::PyErr_ExceptionMatches(exceptions::index_error()) != 0
            || exceptions::PyErr_ExceptionMatches(exceptions::stop_iteration()) != 0;
        if index_or_stop {
            tstate::clear();
            it.it_seq = core::ptr::null_mut();
            exceptions::release_object(seq);
        }
        core::ptr::null_mut()
    }
}

const fn seqiter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PySeqIterObject>() as isize;
    t.tp_dealloc = Some(iter_dealloc);
    t.tp_getattro = Some(crate::capi::object::PyObject_GenericGetAttr);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(iter_traverse);
    t.tp_iter = Some(crate::capi::object::PyObject_SelfIter);
    t.tp_iternext = Some(iter_iternext);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The sequence-iterator type (CPython's `PySeqIter_Type`).
#[unsafe(no_mangle)]
pub static mut PySeqIter_Type: PyTypeObject = seqiter_type();

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `seq` must be a valid sequence object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySeqIter_New(seq: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument follows the C contract.
    unsafe {
        if crate::capi::r#abstract::PySequence_Check(seq) == 0 {
            tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return core::ptr::null_mut();
        }
        // SAFETY: the static type is readied by `init`.
        let object = PyType_GenericAlloc(&raw mut PySeqIter_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let it = &mut *object.cast::<PySeqIterObject>();
        it.it_index = 0;
        exceptions::retain_object(seq);
        it.it_seq = seq;
        object
    }
}

/// CPython's `calliterobject`: call a function until a sentinel appears.
#[repr(C)]
struct PyCallIterObject {
    ob_base: PyObject,
    it_callable: *mut PyObject,
    it_sentinel: *mut PyObject,
}

unsafe extern "C" fn calliter_dealloc(object: *mut PyObject) {
    // SAFETY: the object is a callable iterator this runtime allocated.
    unsafe {
        let it = &mut *object.cast::<PyCallIterObject>();
        let callable = core::mem::replace(&mut it.it_callable, core::ptr::null_mut());
        let sentinel = core::mem::replace(&mut it.it_sentinel, core::ptr::null_mut());
        exceptions::release_object(callable);
        exceptions::release_object(sentinel);
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn calliter_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the field reads are within the iterator.
    unsafe {
        let it = &*object.cast::<PyCallIterObject>();
        if !it.it_callable.is_null() {
            // SAFETY: the visitor contract.
            visit(it.it_callable, arg);
        }
        if !it.it_sentinel.is_null() {
            // SAFETY: the visitor contract.
            visit(it.it_sentinel, arg);
        }
    }
    0
}

unsafe extern "C" fn calliter_iternext(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller passed a callable iterator per the slot contract.
    unsafe {
        let it = &mut *object.cast::<PyCallIterObject>();
        if it.it_callable.is_null() {
            return core::ptr::null_mut();
        }
        let result = crate::capi::call::PyObject_CallNoArgs(it.it_callable);
        if !result.is_null() && !it.it_sentinel.is_null() {
            let ok = crate::capi::object::PyObject_RichCompareBool(it.it_sentinel, result, Py_EQ);
            if ok == 0 {
                return result;
            }
            if ok > 0 {
                let callable = core::mem::replace(&mut it.it_callable, core::ptr::null_mut());
                let sentinel = core::mem::replace(&mut it.it_sentinel, core::ptr::null_mut());
                exceptions::release_object(callable);
                exceptions::release_object(sentinel);
            }
        } else if exceptions::PyErr_ExceptionMatches(exceptions::stop_iteration()) != 0 {
            tstate::clear();
            let callable = core::mem::replace(&mut it.it_callable, core::ptr::null_mut());
            let sentinel = core::mem::replace(&mut it.it_sentinel, core::ptr::null_mut());
            exceptions::release_object(callable);
            exceptions::release_object(sentinel);
        }
        exceptions::release_object(result);
        core::ptr::null_mut()
    }
}

const fn calliter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"callable_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyCallIterObject>() as isize;
    t.tp_dealloc = Some(calliter_dealloc);
    t.tp_getattro = Some(crate::capi::object::PyObject_GenericGetAttr);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(calliter_traverse);
    t.tp_iter = Some(crate::capi::object::PyObject_SelfIter);
    t.tp_iternext = Some(calliter_iternext);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The callable-iterator type (CPython's `PyCallIter_Type`).
#[unsafe(no_mangle)]
pub static mut PyCallIter_Type: PyTypeObject = calliter_type();

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` and `sentinel` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCallIter_New(
    callable: *mut PyObject,
    sentinel: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the static type is readied by `init`; the arguments follow
    // the C contract.
    unsafe {
        let object = PyType_GenericAlloc(&raw mut PyCallIter_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let it = &mut *object.cast::<PyCallIterObject>();
        exceptions::retain_object(callable);
        it.it_callable = callable;
        exceptions::retain_object(sentinel);
        it.it_sentinel = sentinel;
        object
    }
}

/// Ready both types; `init_static_types` calls this once.
pub fn init() -> c_int {
    // SAFETY: init runs before any concurrent access can exist.
    unsafe {
        for t in [&raw mut PySeqIter_Type, &raw mut PyCallIter_Type] {
            if crate::object::typeobject::PyType_Ready(t) < 0 {
                return -1;
            }
            crate::gc::counted::note_coil_type(t);
        }
    }
    0
}

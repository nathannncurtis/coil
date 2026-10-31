//! `builtin_function_or_method`: CPython 3.12's exact `PyCFunctionObject`
//! layout and calling-convention dispatch (methodobject.c), which the
//! native modules' functions are and the module layer calls. `tp_repr`
//! and the doc/name descriptors stay with the str task; `METH_METHOD`
//! belongs to the class machinery that needs it.

use core::ffi::{c_int, c_void};

use coil_capi_sys::{
    _PyCFunctionFast, _PyCFunctionFastWithKeywords, METH_FASTCALL, METH_KEYWORDS, METH_NOARGS,
    METH_O, METH_VARARGS, Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC,
    Py_TPFLAGS_HAVE_VECTORCALL, PyCFunction, PyCFunctionWithKeywords, PyObject, PyTypeObject,
};

use crate::object::{header, typeobject};
use crate::tstate;
use crate::types::exc as exceptions;
use crate::util;

/// CPython 3.12's `PyCFunctionObject` (exact).
#[repr(C)]
struct PyCFunctionObject {
    ob_refcnt: isize,
    ob_type: *mut PyTypeObject,
    m_ml: *const coil_capi_sys::PyMethodDef,
    m_self: *mut PyObject,
    m_module: *mut PyObject,
    m_weakreflist: *mut PyObject,
    vectorcall: Option<coil_capi_sys::vectorcallfunc>,
}

const _: () = {
    assert!(core::mem::offset_of!(PyCFunctionObject, m_ml) == coil_rt_abi::cp312::cfunction::M_ML);
    assert!(
        core::mem::offset_of!(PyCFunctionObject, m_self) == coil_rt_abi::cp312::cfunction::M_SELF
    );
    assert!(
        core::mem::offset_of!(PyCFunctionObject, m_module)
            == coil_rt_abi::cp312::cfunction::M_MODULE
    );
    assert!(
        core::mem::offset_of!(PyCFunctionObject, m_weakreflist)
            == coil_rt_abi::cp312::cfunction::M_WEAKREFLIST
    );
    assert!(
        core::mem::offset_of!(PyCFunctionObject, vectorcall)
            == coil_rt_abi::cp312::cfunction::VECTORCALL
    );
};

/// `PyCFunction_Check` by type identity (CPython's check is exact; no
/// subtype of `builtin_function_or_method` exists).
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub unsafe fn is_cfunction(op: *const PyObject) -> bool {
    // SAFETY: the header read is within the object.
    !op.is_null() && unsafe { header::type_of(op) } == &raw mut PyCFunction_Type
}

/// The method definition of a C function object.
///
/// # Safety
///
/// `func` must be a C function object.
pub(crate) unsafe fn method_def(func: *const PyObject) -> *const coil_capi_sys::PyMethodDef {
    // SAFETY: the field read is within the object.
    unsafe { (*func.cast::<PyCFunctionObject>()).m_ml }
}

/// The bound `self` of a C function object (the module, or the object a
/// method descriptor bound).
///
/// # Safety
///
/// `func` must be a C function object.
pub(crate) unsafe fn bound_self(func: *const PyObject) -> *mut PyObject {
    // SAFETY: the field read is within the object.
    unsafe { (*func.cast::<PyCFunctionObject>()).m_self }
}

/// Call a C function object from a packed argument vector (the
/// `cfunction_vectorcall_*` dispatch of methodobject.c).
///
/// # Safety
///
/// `func` must be a C function object; `args` holds `nargs` valid
/// objects; `kwnames` is null or a tuple of str objects naming the
/// trailing arguments — none exist in this runtime yet, so a non-empty
/// `kwnames` raises the way CPython's does.
pub(crate) unsafe fn vectorcall(
    func: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the definition and self reads are within the object.
    let (ml, self_) = unsafe { (method_def(func), bound_self(func)) };
    // SAFETY: the flag and function reads are within the definition.
    let flags = unsafe { (*ml).ml_flags };
    let name = unsafe { util::cstr((*ml).ml_name) };
    let takes_keywords = flags & METH_KEYWORDS != 0;
    // SAFETY: the size read follows the calling machinery's contract
    // on `kwnames`.
    if !takes_keywords && !kwnames.is_null() && unsafe { crate::util::var_size(kwnames) } > 0 {
        // `_PyObject_FunctionStr` renders the qualified name; with no
        // module objects the qualified name is the bare function name.
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes no keyword arguments").as_bytes(),
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the function pointer read is within the definition.
    let meth = unsafe { (*ml).ml_meth };
    // SAFETY: `args` covers `nargs` object pointers; a zero-count call
    // may pass null, which the empty slice spells.
    let slice: &[*mut PyObject] = if nargs > 0 {
        unsafe { core::slice::from_raw_parts(args, nargs as usize) }
    } else {
        &[]
    };
    // SAFETY: the flags were validated at construction; the function
    // pointer matches the calling convention the flags select.
    unsafe {
        match flags & (METH_VARARGS | METH_FASTCALL | METH_NOARGS | METH_O) {
            METH_NOARGS => {
                let meth: PyCFunction = core::mem::transmute(meth);
                if nargs != 0 {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!("{name}() takes no arguments ({nargs} given)").as_bytes(),
                    );
                    return core::ptr::null_mut();
                }
                meth(self_, core::ptr::null_mut())
            }
            METH_O => {
                let meth: PyCFunction = core::mem::transmute(meth);
                if nargs != 1 {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!("{name}() takes exactly one argument ({nargs} given)").as_bytes(),
                    );
                    return core::ptr::null_mut();
                }
                meth(self_, slice[0])
            }
            METH_FASTCALL => {
                if takes_keywords {
                    let meth: _PyCFunctionFastWithKeywords = core::mem::transmute(meth);
                    meth(self_, slice.as_ptr(), nargs, kwnames)
                } else {
                    let meth: _PyCFunctionFast = core::mem::transmute(meth);
                    meth(self_, slice.as_ptr(), nargs)
                }
            }
            METH_VARARGS => {
                let meth: PyCFunction = core::mem::transmute(meth);
                let tuple = crate::types::tuple::from_array(slice);
                let result = meth(self_, tuple);
                crate::alloc::free_block(tuple.cast());
                result
            }
            _ => {
                tstate::set_string(
                    exceptions::system_error(),
                    format!("{name}() method: bad call flags").as_bytes(),
                );
                core::ptr::null_mut()
            }
        }
    }
}

/// `cfunction_vectorcall_FASTCALL` and its siblings: one trampoline the
/// `PyCFunction_NewEx` convention check installs. It adapts the
/// `vectorcallfunc` signature — `nargsf`, with the offset flag in its
/// top bit — to the plain count the dispatcher takes.
unsafe extern "C" fn cfunction_vectorcall_trampoline(
    func: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    const PY_VECTORCALL_ARGUMENTS_OFFSET: usize = 1usize << 63;
    // SAFETY: the delegate follows the vectorcall protocol's contract.
    unsafe {
        vectorcall(
            func,
            args,
            (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as isize,
            kwnames,
        )
    }
}

/// `cfunction_call` (tp_call): unpack the tuple and delegate.
unsafe extern "C" fn cfunction_call(
    func: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the definition read is within the object.
    let ml = unsafe { method_def(func) };
    // SAFETY: the flag read is within the definition.
    let flags = unsafe { (*ml).ml_flags };
    let is_varargs = flags & METH_VARARGS != 0;
    let takes_keywords = flags & METH_KEYWORDS != 0;
    if is_varargs && !takes_keywords {
        // SAFETY: the name read is within the definition.
        let name = unsafe { util::cstr((*ml).ml_name) };
        // SAFETY: the size read follows the calling machinery's
        // contract on `kwargs`.
        if !kwargs.is_null() && unsafe { crate::util::dict_used(kwargs) } > 0 {
            tstate::set_string(
                exceptions::type_error(),
                format!("{name}() takes no keyword arguments").as_bytes(),
            );
            return core::ptr::null_mut();
        }
    }
    if is_varargs {
        // SAFETY: the self and function reads are within the objects.
        let self_ = unsafe { bound_self(func) };
        let meth = unsafe { (*ml).ml_meth.unwrap() };
        if takes_keywords {
            let meth: PyCFunctionWithKeywords = unsafe { core::mem::transmute(meth) };
            return unsafe { meth(self_, args, kwargs) };
        }
        return unsafe { meth(self_, args) };
    }
    // Everything else goes through the vectorcall dispatch with the
    // tuple unpacked (`PyVectorcall_Call`).
    // SAFETY: the layout-only reads follow the calling machinery's
    // contract on `args`/`kwargs`.
    // SAFETY: the size read follows the calling machinery's contract
    // on `args`.
    let nargs = unsafe { crate::util::var_size(args) };
    let mut flat: Vec<*mut PyObject> = Vec::with_capacity(nargs.max(0) as usize);
    for i in 0..nargs {
        // SAFETY: the item read is within the tuple.
        flat.push(unsafe { crate::types::tuple::PyTuple_GetItem(args, i) });
    }
    unsafe { vectorcall(func, flat.as_ptr(), nargs, kwargs) }
}

unsafe extern "C" fn cfunction_dealloc(object: *mut PyObject) {
    // SAFETY: the field reads are within the object.
    unsafe {
        let func = object.cast::<PyCFunctionObject>();
        exceptions::release_object((*func).m_self);
        exceptions::release_object((*func).m_module);
    }
    // C function objects live on the GC heap; the sweep owns the block.
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn cfunction_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the referents are PyObject pointers or null.
    unsafe {
        let func = object.cast::<PyCFunctionObject>();
        for referent in [(*func).m_self, (*func).m_module] {
            if !referent.is_null() && visit(referent, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

/// `meth_hash`: the bound self and the C function pointer.
unsafe extern "C" fn meth_hash(a: *mut PyObject) -> isize {
    // SAFETY: the field reads are within the object.
    unsafe {
        let func = a.cast::<PyCFunctionObject>();
        let mut x = typeobject::_Py_HashPointer((*func).m_self as *const c_void);
        if let Some(meth) = (*(*func).m_ml).ml_meth {
            x ^= typeobject::_Py_HashPointer(meth as *const c_void);
        }
        if x == -1 {
            x = -2;
        }
        x
    }
}

const fn cfunction_type() -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = c"builtin_function_or_method".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::cfunction::SIZEOF as isize;
    t.tp_itemsize = 0;
    t.tp_dealloc = Some(cfunction_dealloc);
    t.tp_vectorcall_offset = coil_rt_abi::cp312::cfunction::VECTORCALL as isize;
    t.tp_as_number = core::ptr::null_mut();
    t.tp_flags =
        Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_VECTORCALL | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
    t.tp_traverse = Some(cfunction_traverse);
    t.tp_hash = Some(meth_hash);
    t.tp_call = Some(cfunction_call);
    t.tp_alloc = Some(typeobject::PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `builtin_function_or_method` type object (CPython's
/// `PyCFunction_Type`).
#[unsafe(no_mangle)]
pub static mut PyCFunction_Type: PyTypeObject = cfunction_type();

/// See the CPython 3.12 contract: bind a method definition to `self_`.
/// `module` may be null.
///
/// # Safety
///
/// `ml` must point to a definition whose flags name a calling convention
/// this type dispatches and whose lifetime outlives the object; `self_`
/// and `module` must be null or valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_NewEx(
    ml: *const coil_capi_sys::PyMethodDef,
    self_: *mut PyObject,
    module: *mut PyObject,
) -> *mut PyObject {
    if ml.is_null() || unsafe { (*ml).ml_name.is_null() } || unsafe { (*ml).ml_meth.is_none() } {
        tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `PyCFunction_Type` is static and readied by init.
    let object = unsafe { typeobject::PyType_GenericAlloc(&raw mut PyCFunction_Type, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        let func = object.cast::<PyCFunctionObject>();
        (*func).m_ml = ml;
        (*func).m_self = self_;
        exceptions::retain_object(self_);
        (*func).m_module = module;
        exceptions::retain_object(module);
        // `PyCMethod_New`'s trampoline choice: METH_VARARGS functions
        // are more efficient through tp_call, everything else gets the
        // per-convention vectorcall.
        let flags = (*ml).ml_flags;
        let convention = flags & (METH_VARARGS | METH_FASTCALL | METH_NOARGS | METH_O);
        (*func).vectorcall = if convention == METH_VARARGS {
            None
        } else {
            Some(cfunction_vectorcall_trampoline)
        };
    }
    object
}

/// See the CPython 3.12 contract: `PyCFunction_NewEx` with a null module.
///
/// # Safety
///
/// As for `PyCFunction_NewEx`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_New(
    ml: *const coil_capi_sys::PyMethodDef,
    self_: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller upholds the `PyCFunction_NewEx` contract.
    unsafe { PyCFunction_NewEx(ml, self_, core::ptr::null_mut()) }
}

/// See the CPython 3.12 contract: the C function pointer the object was
/// built from.
///
/// # Safety
///
/// `func` must be null or point to a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetFunction(func: *mut PyObject) -> *mut c_void {
    // SAFETY: the cfunction contract.
    if unsafe { !is_cfunction(func) } {
        tstate::set_string(
            exceptions::type_error(),
            b"bad argument type for built-in operation",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the definition read follows the type check.
    let ml = unsafe { method_def(func) };
    // SAFETY: the function pointer read is within the definition; a
    // cfunction always carries one.
    let meth = unsafe { (*ml).ml_meth.unwrap() };
    meth as *mut c_void
}

/// See the CPython 3.12 contract: the bound `self` of the object.
///
/// # Safety
///
/// `func` must be null or point to a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetSelf(func: *mut PyObject) -> *mut PyObject {
    // SAFETY: the cfunction contract.
    if unsafe { !is_cfunction(func) } {
        tstate::set_string(
            exceptions::type_error(),
            b"bad argument type for built-in operation",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the self read follows the type check.
    unsafe { bound_self(func) }
}

/// See the CPython 3.12 contract: the calling-convention flags the object
/// was built with.
///
/// # Safety
///
/// `func` must be null or point to a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetFlags(func: *mut PyObject) -> c_int {
    // SAFETY: the cfunction contract.
    if unsafe { !is_cfunction(func) } {
        tstate::set_string(
            exceptions::type_error(),
            b"bad argument type for built-in operation",
        );
        return -1;
    }
    // SAFETY: the definition read follows the type check.
    let ml = unsafe { method_def(func) };
    // SAFETY: the flag read is within the definition.
    unsafe { (*ml).ml_flags }
}

//! `object.h`: the abstract object protocol that is not number, sequence
//! or mapping specific (spec section 10.1) — attributes, hashing, truth,
//! rich comparison, printing, iteration entry, formatting, finalizers and
//! the generic attribute machinery types install in `tp_getattro`.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_MANAGED_DICT, Py_TPFLAGS_METHOD_DESCRIPTOR, Py_TPFLAGS_READY, PyObject, PyTypeObject,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{PyType_IsSubtype, PyType_Ready, chain_is_subtype, mro_of};
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::unicode;
use crate::util;

// ------------------------------------------------------------ shared helpers

/// A type name truncated the way CPython's `%.200s` and friends print it.
fn name_truncated(name: &str, limit: usize) -> &str {
    match name.char_indices().nth(limit) {
        Some((i, _)) => &name[..i],
        None => name,
    }
}

/// `Py_TYPE(o)->tp_name` as a truncated string slice.
///
/// # Safety
///
/// `object` must be a valid object.
unsafe fn type_name(object: *mut PyObject, limit: usize) -> &'static str {
    // SAFETY: the type name is a valid C string on live types.
    name_truncated(
        unsafe { util::cstr((*header::type_of(object)).tp_name) },
        limit,
    )
}

/// CPython's `null_error`.
fn null_error() -> *mut PyObject {
    tstate::set_string(
        exceptions::system_error(),
        b"null argument to internal routine",
    );
    core::ptr::null_mut()
}

/// CPython's `type_error(msg, o)`: `msg` with one `%.200s` for the type.
unsafe fn type_error(message: &str, object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object's type is valid.
    let name = unsafe { type_name(object, 200) };
    // The message carries exactly one `%.200s` (or plain `%s`).
    match message.find("%.200s").or_else(|| message.find("%s")) {
        Some(at) => tstate::set_string(
            exceptions::type_error(),
            format!(
                "{}{}{}",
                &message[..at],
                name,
                &message[at
                    + if message[at..].starts_with("%.200s") {
                        6
                    } else {
                        2
                    }..]
            )
            .as_bytes(),
        ),
        None => tstate::set_string(exceptions::type_error(), message.as_bytes()),
    }
    core::ptr::null_mut()
}

/// `_PyType_Lookup`: `name` through `ty`'s MRO, borrowed, null when absent.
///
/// # Safety
///
/// `ty` must be a type object; `name` a string object.
unsafe fn type_lookup(ty: *mut PyTypeObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the MRO walk and dict reads respect the type's structures.
    unsafe {
        for base in mro_of(ty) {
            let dict = (*base).tp_dict;
            if dict.is_null() {
                continue;
            }
            let hit = crate::types::dict::PyDict_GetItemWithError(dict, name);
            if !hit.is_null() {
                return hit;
            }
            if !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
        }
    }
    core::ptr::null_mut()
}

/// `PyDescr_IsData`: a descriptor with a `tp_descr_set` is a data
/// descriptor, which wins over any instance dictionary entry.
///
/// # Safety
///
/// `descr` must be a valid object.
unsafe fn descr_is_data(descr: *mut PyObject) -> bool {
    // SAFETY: the slot read is within the descriptor's type.
    unsafe { (*header::type_of(descr)).tp_descr_set.is_some() }
}

/// CPython's `_PyObject_ComputedDictPointer`: where the instance
/// dictionary lives, honouring negative (variable-sized) offsets.
///
/// # Safety
///
/// `object` must be a valid object.
unsafe fn computed_dict_pointer(object: *mut PyObject) -> *mut *mut PyObject {
    // SAFETY: the offset arithmetic follows the type's own sizes.
    unsafe {
        let ty = header::type_of(object);
        let mut dictoffset = (*ty).tp_dictoffset;
        if dictoffset == 0 {
            return core::ptr::null_mut();
        }
        if dictoffset < 0 {
            let tsize = (*object.cast::<coil_capi_sys::PyVarObject>()).ob_size;
            let tsize = if tsize < 0 { -tsize } else { tsize };
            let basic = (*ty).tp_basicsize.max(0);
            let item = (*ty).tp_itemsize.max(0);
            dictoffset += basic + item * tsize;
        }
        (object as *mut u8).add(dictoffset as usize) as *mut *mut PyObject
    }
}

// -------------------------------------------------------- attribute protocol

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` and `name` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAttr(v: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        let ty = header::type_of(v);
        if !unicode::check(name) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute name must be string, not '{}'",
                    type_name(name, 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }

        if let Some(getattro) = (*ty).tp_getattro {
            return getattro(v, name);
        }
        if let Some(getattr) = (*ty).tp_getattr {
            // SAFETY: the name's cached UTF-8 is NUL-terminated.
            let name_str = unicode::as_utf8_and_size(name, core::ptr::null_mut());
            if name_str.is_null() {
                return core::ptr::null_mut();
            }
            return getattr(v, name_str.cast_mut());
        }
        let name_bytes = unicode::as_utf8_vec(name);
        let name_text = String::from_utf8_lossy(&name_bytes);
        tstate::set_string(
            exceptions::attribute_error(),
            format!(
                "'{}' object has no attribute '{}'",
                type_name(v, 100),
                name_text
            )
            .as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object; `name` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAttrString(
    v: *mut PyObject,
    name: *const c_char,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if let Some(getattr) = (*header::type_of(v)).tp_getattr {
            return getattr(v, name.cast_mut());
        }
        let name = if name.is_null() {
            return null_error();
        } else {
            name
        };
        // SAFETY: `name` is NUL-terminated by contract.
        let name_object = unicode::PyUnicode_FromString(name);
        if name_object.is_null() {
            return core::ptr::null_mut();
        }
        let result = PyObject_GetAttr(v, name_object);
        exceptions::release_object(name_object);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be null or a valid object; `name` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttrString(v: *mut PyObject, name: *const c_char) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if let Some(getattr) = (*header::type_of(v)).tp_getattr {
            let result = getattr(v, name.cast_mut());
            if !result.is_null() {
                exceptions::release_object(result);
                return 1;
            }
            tstate::clear();
            return 0;
        }
        if name.is_null() {
            tstate::clear();
            return 0;
        }
        let name_object = unicode::PyUnicode_FromString(name);
        if name_object.is_null() {
            tstate::clear();
            return 0;
        }
        let ok = PyObject_HasAttr(v, name_object);
        exceptions::release_object(name_object);
        ok
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` and `name` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttr(v: *mut PyObject, name: *mut PyObject) -> c_int {
    // SAFETY: the lookup follows the C contract.
    unsafe {
        let mut result = core::ptr::null_mut();
        if _PyObject_LookupAttr(v, name, &raw mut result) < 0 {
            tstate::clear();
            return 0;
        }
        if result.is_null() {
            return 0;
        }
        exceptions::release_object(result);
        1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v`, `name` and `value` must be valid objects; `value` may be null to
/// delete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetAttr(
    v: *mut PyObject,
    name: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        let ty = header::type_of(v);
        if !unicode::check(name) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute name must be string, not '{}'",
                    type_name(name, 200)
                )
                .as_bytes(),
            );
            return -1;
        }
        if let Some(setattro) = (*ty).tp_setattro {
            return setattro(v, name, value);
        }
        if let Some(setattr) = (*ty).tp_setattr {
            // SAFETY: the name's cached UTF-8 is NUL-terminated.
            let name_str = unicode::as_utf8_and_size(name, core::ptr::null_mut());
            if name_str.is_null() {
                return -1;
            }
            return setattr(v, name_str.cast_mut(), value);
        }
        let name_bytes = unicode::as_utf8_vec(name);
        let name_text = String::from_utf8_lossy(&name_bytes);
        if (*ty).tp_getattr.is_none() && (*ty).tp_getattro.is_none() {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "'{}' object has no attributes ({} .{})",
                    type_name(v, 100),
                    if value.is_null() { "del" } else { "assign to" },
                    name_text
                )
                .as_bytes(),
            );
        } else {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "'{}' object has only read-only attributes ({} .{})",
                    type_name(v, 100),
                    if value.is_null() { "del" } else { "assign to" },
                    name_text
                )
                .as_bytes(),
            );
        }
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object; `name` a NUL-terminated string; `w` a
/// valid object or null to delete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetAttrString(
    v: *mut PyObject,
    name: *const c_char,
    w: *mut PyObject,
) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if let Some(setattr) = (*header::type_of(v)).tp_setattr {
            return setattr(v, name.cast_mut(), w);
        }
        if name.is_null() {
            return -1;
        }
        // SAFETY: `name` is NUL-terminated by contract.
        let name_object = unicode::PyUnicode_FromString(name);
        if name_object.is_null() {
            return -1;
        }
        let result = PyObject_SetAttr(v, name_object, w);
        exceptions::release_object(name_object);
        result
    }
}

/// `_PyObject_LookupAttr`: `getattr` that reports "absent" separately
/// from "error". 1 found (`*result` owned), 0 absent, -1 error.
///
/// # Safety
///
/// `v` and `name` must be valid objects; `result` writable.
// The generic-attr identity test is CPython's own; the slot is this
// DLL's `no_mangle` function, its address as fixed as C's.
#[unsafe(no_mangle)]
#[allow(unpredictable_function_pointer_comparisons)]
pub unsafe extern "C" fn _PyObject_LookupAttr(
    v: *mut PyObject,
    name: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        let ty = header::type_of(v);
        if !unicode::check(name) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute name must be string, not '{}'",
                    type_name(name, 200)
                )
                .as_bytes(),
            );
            *result = core::ptr::null_mut();
            return -1;
        }

        if (*ty).tp_getattro == Some(PyObject_GenericGetAttr) {
            // `suppress = 1` turns a miss into a clean return.
            *result = _PyObject_GenericGetAttrWithDict(v, name, core::ptr::null_mut(), 1);
            if !(*result).is_null() {
                return 1;
            }
            if !tstate::occurred().is_null() {
                return -1;
            }
            return 0;
        }

        if let Some(getattro) = (*ty).tp_getattro {
            *result = getattro(v, name);
        } else if let Some(getattr) = (*ty).tp_getattr {
            // SAFETY: the name's cached UTF-8 is NUL-terminated.
            let name_str = unicode::as_utf8_and_size(name, core::ptr::null_mut());
            if name_str.is_null() {
                *result = core::ptr::null_mut();
                return -1;
            }
            *result = getattr(v, name_str.cast_mut());
        } else {
            *result = core::ptr::null_mut();
            return 0;
        }

        if !(*result).is_null() {
            return 1;
        }
        if exceptions::PyErr_ExceptionMatches(exceptions::attribute_error()) == 0 {
            return -1;
        }
        tstate::clear();
        0
    }
}

/// See the CPython 3.12 contract for this entry point: a special method
/// through the type's MRO, bound to `self`; null without an exception
/// when the type does not define it.
///
/// # Safety
///
/// `self_` and `name` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_LookupSpecial(
    self_: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the MRO walk and descriptor call follow the C contract.
    unsafe {
        let ty = header::type_of(self_);
        let res = type_lookup(ty, name);
        if res.is_null() {
            return core::ptr::null_mut();
        }
        if let Some(get) = (*header::type_of(res)).tp_descr_get {
            return get(res, self_, ty.cast());
        }
        exceptions::retain_object(res);
        res
    }
}

// -------------------------------------------------- generic attribute machinery

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` and `name` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericGetAttr(
    obj: *mut PyObject,
    name: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the delegation follows the C contract.
    unsafe { _PyObject_GenericGetAttrWithDict(obj, name, core::ptr::null_mut(), 0) }
}

/// CPython's `_PyObject_GenericGetAttrWithDict`: data descriptors first,
/// then the instance dictionary, then non-data descriptors.
///
/// # Safety
///
/// `obj` and `name` must be valid objects; `dict` null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GenericGetAttrWithDict(
    obj: *mut PyObject,
    name: *mut PyObject,
    dict: *mut PyObject,
    suppress: c_int,
) -> *mut PyObject {
    // SAFETY: the lookups and descriptor calls follow CPython's order.
    unsafe {
        let ty = header::type_of(obj);
        if !unicode::check(name) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute name must be string, not '{}'",
                    type_name(name, 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }

        if (*ty).tp_flags & Py_TPFLAGS_READY == 0 && PyType_Ready(ty) < 0 {
            return core::ptr::null_mut();
        }

        let descr = type_lookup(ty, name);
        let mut descr_get = None;
        if !descr.is_null() {
            descr_get = (*header::type_of(descr)).tp_descr_get;
            if let Some(get) = descr_get
                && descr_is_data(descr)
            {
                let res = get(descr, obj, ty.cast());
                if res.is_null()
                    && suppress != 0
                    && exceptions::PyErr_ExceptionMatches(exceptions::attribute_error()) != 0
                {
                    tstate::clear();
                }
                return res;
            }
        }

        let mut dict = dict;
        if dict.is_null() {
            let dictptr = computed_dict_pointer(obj);
            if !dictptr.is_null() {
                dict = core::ptr::read(dictptr);
            }
        }
        if !dict.is_null() {
            let res = crate::types::dict::PyDict_GetItemWithError(dict, name);
            if !res.is_null() {
                exceptions::retain_object(res);
                return res;
            }
            if !tstate::occurred().is_null() {
                if suppress != 0
                    && exceptions::PyErr_ExceptionMatches(exceptions::attribute_error()) != 0
                {
                    tstate::clear();
                } else {
                    return core::ptr::null_mut();
                }
            }
        }

        if let Some(get) = descr_get {
            let res = get(descr, obj, ty.cast());
            if res.is_null()
                && suppress != 0
                && exceptions::PyErr_ExceptionMatches(exceptions::attribute_error()) != 0
            {
                tstate::clear();
            }
            return res;
        }
        if !descr.is_null() {
            exceptions::retain_object(descr);
            return descr;
        }

        if suppress == 0 {
            let name_bytes = unicode::as_utf8_vec(name);
            let name_text = String::from_utf8_lossy(&name_bytes);
            tstate::set_string(
                exceptions::attribute_error(),
                format!(
                    "'{}' object has no attribute '{}'",
                    type_name(obj, 100),
                    name_text
                )
                .as_bytes(),
            );
        }
        core::ptr::null_mut()
    }
}

/// CPython's `_PyObject_GetMethod`, the `LOAD_METHOD` fast path: 1 with
/// `*method` an unbound method descriptor, 0 with a bound attribute.
///
/// # Safety
///
/// `obj` and `name` must be valid objects; `method` writable and null.
#[unsafe(no_mangle)]
#[allow(unpredictable_function_pointer_comparisons)]
pub unsafe extern "C" fn _PyObject_GetMethod(
    obj: *mut PyObject,
    name: *mut PyObject,
    method: *mut *mut PyObject,
) -> c_int {
    // SAFETY: the lookups mirror `_PyObject_GenericGetAttrWithDict`.
    unsafe {
        let ty = header::type_of(obj);
        if (*ty).tp_flags & Py_TPFLAGS_READY == 0 && PyType_Ready(ty) < 0 {
            *method = core::ptr::null_mut();
            return 0;
        }

        if (*ty).tp_getattro != Some(PyObject_GenericGetAttr) || !unicode::check_exact(name) {
            *method = PyObject_GetAttr(obj, name);
            return 0;
        }

        let descr = type_lookup(ty, name);
        let mut descr_get = None;
        let mut meth_found = false;
        if !descr.is_null() {
            let descr_type = header::type_of(descr);
            if (*descr_type).tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
                meth_found = true;
            } else {
                descr_get = (*descr_type).tp_descr_get;
                if let Some(get) = descr_get
                    && descr_is_data(descr)
                {
                    *method = get(descr, obj, ty.cast());
                    return 0;
                }
            }
        }

        let mut dict = core::ptr::null_mut();
        let dictptr = computed_dict_pointer(obj);
        if !dictptr.is_null() {
            dict = core::ptr::read(dictptr);
        }
        if !dict.is_null() {
            let attr = crate::types::dict::PyDict_GetItemWithError(dict, name);
            if !attr.is_null() {
                exceptions::retain_object(attr);
                *method = attr;
                return 0;
            }
            if !tstate::occurred().is_null() {
                *method = core::ptr::null_mut();
                return 0;
            }
        }

        if meth_found {
            exceptions::retain_object(descr);
            *method = descr;
            return 1;
        }
        if let Some(get) = descr_get {
            *method = get(descr, obj, ty.cast());
            return 0;
        }
        if !descr.is_null() {
            exceptions::retain_object(descr);
            *method = descr;
            return 0;
        }
        let name_bytes = unicode::as_utf8_vec(name);
        let name_text = String::from_utf8_lossy(&name_bytes);
        tstate::set_string(
            exceptions::attribute_error(),
            format!(
                "'{}' object has no attribute '{}'",
                type_name(obj, 100),
                name_text
            )
            .as_bytes(),
        );
        *method = core::ptr::null_mut();
        0
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj`, `name` and `value` must be valid objects; `value` may be null
/// to delete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericSetAttr(
    obj: *mut PyObject,
    name: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the delegation follows the C contract.
    unsafe { _PyObject_GenericSetAttrWithDict(obj, name, value, core::ptr::null_mut()) }
}

/// CPython's `_PyObject_GenericSetAttrWithDict`: data descriptors first,
/// then the instance dictionary slot.
///
/// # Safety
///
/// `obj` and `name` must be valid objects; `value` an object or null to
/// delete; `dict` null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GenericSetAttrWithDict(
    obj: *mut PyObject,
    name: *mut PyObject,
    value: *mut PyObject,
    dict: *mut PyObject,
) -> c_int {
    // SAFETY: the writes follow CPython's order.
    unsafe {
        let ty = header::type_of(obj);
        if !unicode::check(name) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute name must be string, not '{}'",
                    type_name(name, 200)
                )
                .as_bytes(),
            );
            return -1;
        }
        if (*ty).tp_flags & Py_TPFLAGS_READY == 0 && PyType_Ready(ty) < 0 {
            return -1;
        }

        let descr = type_lookup(ty, name);
        if !descr.is_null()
            && let Some(set) = (*header::type_of(descr)).tp_descr_set
        {
            return set(descr, obj, value);
        }

        let dict = if dict.is_null() {
            let dictptr = computed_dict_pointer(obj);
            if dictptr.is_null() {
                let name_bytes = unicode::as_utf8_vec(name);
                let name_text = String::from_utf8_lossy(&name_bytes);
                if descr.is_null() {
                    tstate::set_string(
                        exceptions::attribute_error(),
                        format!(
                            "'{}' object has no attribute '{}'",
                            type_name(obj, 100),
                            name_text
                        )
                        .as_bytes(),
                    );
                } else {
                    tstate::set_string(
                        exceptions::attribute_error(),
                        format!(
                            "'{}' object attribute '{}' is read-only",
                            type_name(obj, 100),
                            name_text
                        )
                        .as_bytes(),
                    );
                }
                return -1;
            }
            // `_PyObjectDict_SetItem`: the dictionary appears on first
            // store, deletions included (a delete against the empty new
            // dictionary raises the `KeyError` the error check below
            // turns into `AttributeError`).
            if core::ptr::read(dictptr).is_null() {
                let new_dict = crate::types::dict::PyDict_New();
                if new_dict.is_null() {
                    return -1;
                }
                core::ptr::write(dictptr, new_dict);
            }
            core::ptr::read(dictptr)
        } else {
            dict
        };
        let res = if value.is_null() {
            crate::types::dict::PyDict_DelItem(dict, name)
        } else {
            crate::types::dict::PyDict_SetItem(dict, name, value)
        };

        if res < 0 && exceptions::PyErr_ExceptionMatches(exceptions::key_error()) != 0 {
            let name_bytes = unicode::as_utf8_vec(name);
            let name_text = String::from_utf8_lossy(&name_bytes);
            if PyType_IsSubtype(ty, &raw mut crate::object::typeobject::PyType_Type) != 0 {
                let class_name = util::cstr((*obj.cast::<PyTypeObject>()).tp_name);
                tstate::set_string(
                    exceptions::attribute_error(),
                    format!(
                        "type object '{}' has no attribute '{}'",
                        name_truncated(class_name, 50),
                        name_text
                    )
                    .as_bytes(),
                );
            } else {
                tstate::set_string(
                    exceptions::attribute_error(),
                    format!(
                        "'{}' object has no attribute '{}'",
                        type_name(obj, 100),
                        name_text
                    )
                    .as_bytes(),
                );
            }
        }
        res
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` and `value` must be valid objects; `value` non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericGetDict(
    obj: *mut PyObject,
    _context: *mut c_void,
) -> *mut PyObject {
    // SAFETY: the dict pointer follows the type's layout.
    unsafe {
        let dictptr = _PyObject_GetDictPtr(obj);
        if dictptr.is_null() {
            tstate::set_string(
                exceptions::attribute_error(),
                b"This object has no __dict__",
            );
            return core::ptr::null_mut();
        }
        let dict = core::ptr::read(dictptr);
        if dict.is_null() {
            let dict = crate::types::dict::PyDict_New();
            if dict.is_null() {
                return core::ptr::null_mut();
            }
            core::ptr::write(dictptr, dict);
        }
        exceptions::retain_object(core::ptr::read(dictptr));
        core::ptr::read(dictptr)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` and `value` must be valid objects; `value` non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericSetDict(
    obj: *mut PyObject,
    value: *mut PyObject,
    _context: *mut c_void,
) -> c_int {
    // SAFETY: the dict pointer follows the type's layout.
    unsafe {
        let dictptr = _PyObject_GetDictPtr(obj);
        if dictptr.is_null() {
            tstate::set_string(
                exceptions::attribute_error(),
                b"This object has no __dict__",
            );
            return -1;
        }
        if value.is_null() {
            tstate::set_string(exceptions::type_error(), b"cannot delete __dict__");
            return -1;
        }
        if !crate::types::dict::is_dict(value) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "__dict__ must be set to a dictionary, not a '{}'",
                    type_name(value, 200)
                )
                .as_bytes(),
            );
            return -1;
        }
        let old = core::ptr::replace(dictptr, value);
        exceptions::retain_object(value);
        exceptions::release_object(old);
        0
    }
}

/// CPython's `_PyObject_GetDictPtr`, the documented way to reach an
/// object's `__dict__` slot.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GetDictPtr(obj: *mut PyObject) -> *mut *mut PyObject {
    // SAFETY: the flag read and the offset arithmetic are the type's own.
    unsafe {
        if (*header::type_of(obj)).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 {
            return computed_dict_pointer(obj);
        }
        // This runtime's types carry a `tp_dictoffset`; managed inline
        // dictionaries are a store the type machinery does not build.
        tstate::set_string(
            exceptions::not_implemented_error(),
            b"coil: inline managed dictionaries are not implemented",
        );
        core::ptr::null_mut()
    }
}

// ------------------------------------------------------------- hash and truth

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Hash(v: *mut PyObject) -> isize {
    // SAFETY: the slot read and call respect the object's type.
    unsafe {
        let ty = header::type_of(v);
        if let Some(hash) = (*ty).tp_hash {
            return hash(v);
        }
        // A type inheriting straight from `object` may not be readied
        // yet; `PyType_Ready` can fill `tp_hash` (CPython relies on the
        // same fallback).
        if (*ty).tp_flags & Py_TPFLAGS_READY == 0 {
            if PyType_Ready(ty) < 0 {
                return -1;
            }
            if let Some(hash) = (*ty).tp_hash {
                return hash(v);
            }
        }
        let name = util::cstr((*ty).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("unhashable type: '{}'", name_truncated(name, 200)).as_bytes(),
        );
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsTrue(v: *mut PyObject) -> c_int {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        if core::ptr::eq(v, singletons::true_()) {
            return 1;
        }
        if core::ptr::eq(v, singletons::false_()) || core::ptr::eq(v, singletons::none()) {
            return 0;
        }
        let ty = header::type_of(v);
        let res: isize = if let Some(nb_bool) = (*ty).tp_as_number.as_ref().and_then(|m| m.nb_bool)
        {
            nb_bool(v) as isize
        } else if let Some(mp_length) = (*ty).tp_as_mapping.as_ref().and_then(|m| m.mp_length) {
            mp_length(v)
        } else if let Some(sq_length) = (*ty).tp_as_sequence.as_ref().and_then(|m| m.sq_length) {
            sq_length(v)
        } else {
            return 1;
        };
        // Negative values are -1 (error) or -2 (false, already negative
        // length), both below zero; CPython narrows through the cast.
        if res > 0 { 1 } else { res as c_int }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Not(v: *mut PyObject) -> c_int {
    // SAFETY: the delegation follows the C contract.
    let result = unsafe { PyObject_IsTrue(v) };
    if result < 0 {
        return result;
    }
    c_int::from(result == 0)
}

// ------------------------------------------------------------- representations

/// `PyObject_Print` writes through the C runtime's stdio: `fwrite`,
/// `ferror` and `clearerr` resolve against the platform CRT.
///
/// # Safety
///
/// `fp` must be a live `FILE *`; `op` a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Print(op: *mut PyObject, fp: *mut c_void, flags: c_int) -> c_int {
    // The C runtime's stdio entry points; both CI legs link a CRT that
    // exports them.
    unsafe extern "C" {
        fn fwrite(ptr: *const c_void, size: usize, count: usize, stream: *mut c_void) -> usize;
        fn ferror(stream: *mut c_void) -> c_int;
        fn clearerr(stream: *mut c_void);
    }
    const PY_PRINT_RAW: c_int = 1;
    // SAFETY: the CRT calls take the `FILE *` the caller provided.
    unsafe {
        clearerr(fp);
        let mut ret = 0;
        if op.is_null() {
            fwrite(b"<nil>".as_ptr().cast(), 1, 5, fp);
        } else if header::refcnt(op) <= 0 {
            let text = format!("<refcnt {} at {:p}>", header::refcnt(op), op);
            fwrite(text.as_ptr().cast(), 1, text.len(), fp);
        } else {
            let string = if flags & PY_PRINT_RAW != 0 {
                crate::types::unicode::PyObject_Str(op)
            } else {
                crate::types::unicode::PyObject_Repr(op)
            };
            if string.is_null() {
                ret = -1;
            } else {
                // SAFETY: the cached UTF-8 buffer belongs to the string.
                let bytes = unicode::as_utf8_and_size(string, core::ptr::null_mut());
                if bytes.is_null() {
                    ret = -1;
                } else {
                    // SAFETY: the same contract gives a NUL-terminated
                    // buffer whose length the write must not include.
                    let len = core::ffi::CStr::from_ptr(bytes).to_bytes().len();
                    fwrite(bytes.cast(), 1, len, fp);
                }
                exceptions::release_object(string);
            }
        }
        if ret == 0 && ferror(fp) != 0 {
            // `PyErr_SetFromErrno`'s place: the CRT's errno as an
            // `OSError` message.
            tstate::set_string(
                exceptions::os_error(),
                std::io::Error::last_os_error().to_string().as_bytes(),
            );
            clearerr(fp);
            ret = -1;
        }
        ret
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Dir(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument follows the C contract.
    unsafe {
        if obj.is_null() {
            // `dir()` with no argument reads the current frame's locals;
            // this runtime's frame-locals accessor does not exist yet.
            tstate::set_string(
                exceptions::not_implemented_error(),
                b"coil: PyObject_Dir without an object needs frame locals",
            );
            return core::ptr::null_mut();
        }
        // `_dir_object`: `__dir__` is a special method, so the lookup
        // goes through the type, never the instance dictionary.
        let name_object = unicode::PyUnicode_FromString(c"__dir__".as_ptr());
        if name_object.is_null() {
            return core::ptr::null_mut();
        }
        let ty = header::type_of(obj);
        let dirfunc = type_lookup(ty, name_object);
        exceptions::release_object(name_object);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        if !dirfunc.is_null() {
            let bound = if let Some(get) = (*header::type_of(dirfunc)).tp_descr_get {
                get(dirfunc, obj, ty.cast())
            } else {
                exceptions::retain_object(dirfunc);
                dirfunc
            };
            if bound.is_null() {
                return core::ptr::null_mut();
            }
            let result = crate::capi::call::PyObject_CallNoArgs(bound);
            exceptions::release_object(bound);
            if result.is_null() {
                return core::ptr::null_mut();
            }
            let sorted = crate::capi::r#abstract::PySequence_List(result);
            exceptions::release_object(result);
            if sorted.is_null() {
                return core::ptr::null_mut();
            }
            if crate::types::list::PyList_Sort(sorted) < 0 {
                exceptions::release_object(sorted);
                return core::ptr::null_mut();
            }
            return sorted;
        }
        // Every CPython object inherits `object.__dir__`; the type
        // dictionaries that would carry it are not populated yet, so a
        // miss here runs its body instead of CPython's "object does not
        // provide __dir__" `TypeError`: the instance dictionary's keys
        // merged with the attributes every class in the MRO defines
        // (`merge_class_dict` walks the same dictionaries through
        // `__dict__`/`__bases__` lookups that miss here too).
        let dict = crate::types::dict::PyDict_New();
        if dict.is_null() {
            return core::ptr::null_mut();
        }
        let dictptr = computed_dict_pointer(obj);
        let instance_dict = if dictptr.is_null() {
            core::ptr::null_mut()
        } else {
            core::ptr::read(dictptr)
        };
        let mut merged = true;
        if !instance_dict.is_null() && crate::types::dict::PyDict_Update(dict, instance_dict) < 0 {
            merged = false;
        }
        if merged {
            for base in mro_of(header::type_of(obj)) {
                let class_dict = (*base).tp_dict;
                if class_dict.is_null() || crate::types::dict::PyDict_Size(class_dict) == 0 {
                    continue;
                }
                if crate::types::dict::PyDict_Update(dict, class_dict) < 0 {
                    merged = false;
                    break;
                }
            }
        }
        let keys = if merged {
            crate::types::dict::PyDict_Keys(dict)
        } else {
            core::ptr::null_mut()
        };
        exceptions::release_object(dict);
        if keys.is_null() {
            return core::ptr::null_mut();
        }
        if crate::types::list::PyList_Sort(keys) < 0 {
            exceptions::release_object(keys);
            return core::ptr::null_mut();
        }
        keys
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object; `format_spec` a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Format(
    obj: *mut PyObject,
    format_spec: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the lookups follow the C contract.
    unsafe {
        if !format_spec.is_null() && !unicode::check(format_spec) {
            tstate::set_string(
                exceptions::system_error(),
                format!(
                    "Format specifier must be a string, not {}",
                    type_name(format_spec, 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        // The empty-spec fast path returns the object's own string.
        if format_spec.is_null() || unicode::get_length(format_spec) == 0 {
            if unicode::check_exact(obj) {
                exceptions::retain_object(obj);
                return obj;
            }
            if crate::types::long::is_long(obj)
                && core::ptr::eq(
                    header::type_of(obj),
                    &raw mut crate::types::long::PyLong_Type,
                )
            {
                return crate::types::unicode::PyObject_Str(obj);
            }
        }
        let empty = if format_spec.is_null() {
            unicode::str_from_utf8("")
        } else {
            core::ptr::null_mut()
        };
        let spec = if empty.is_null() { format_spec } else { empty };

        // `__format__` comes from the type; every CPython object
        // inherits `object.__format__`, whose body the empty type
        // dictionaries cannot supply yet, so it runs inline: an empty
        // specifier means `str(obj)`, anything else is an error.
        let name_object = unicode::PyUnicode_FromString(c"__format__".as_ptr());
        if name_object.is_null() {
            if !empty.is_null() {
                exceptions::release_object(empty);
            }
            return core::ptr::null_mut();
        }
        let ty = header::type_of(obj);
        let method = type_lookup(ty, name_object);
        exceptions::release_object(name_object);
        let result = if method.is_null() {
            if !tstate::occurred().is_null() {
                core::ptr::null_mut()
            } else {
                // `object.__format__`'s body.
                if unicode::get_length(spec) == 0 {
                    crate::types::unicode::PyObject_Str(obj)
                } else {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!(
                            "unsupported format string passed to {}.__format__",
                            name_truncated(util::cstr((*ty).tp_name), 200)
                        )
                        .as_bytes(),
                    );
                    core::ptr::null_mut()
                }
            }
        } else {
            let descriptor = (*header::type_of(method)).tp_descr_get;
            let bound = if let Some(get) = descriptor {
                get(method, obj, ty.cast())
            } else {
                exceptions::retain_object(method);
                method
            };
            if bound.is_null() {
                core::ptr::null_mut()
            } else {
                let formatted = crate::capi::call::PyObject_CallOneArg(bound, spec);
                exceptions::release_object(bound);
                if !formatted.is_null() && !unicode::check(formatted) {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!(
                            "__format__ must return a str, not {}",
                            type_name(formatted, 200)
                        )
                        .as_bytes(),
                    );
                    exceptions::release_object(formatted);
                    core::ptr::null_mut()
                } else {
                    formatted
                }
            }
        };
        if !empty.is_null() {
            exceptions::release_object(empty);
        }
        result
    }
}

// -------------------------------------------------------- type and iteration

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Type(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return null_error();
    }
    // SAFETY: the type read is within the object.
    let ty = unsafe { header::type_of(o) };
    // SAFETY: a type object of this runtime.
    unsafe { exceptions::retain_object(ty.cast()) };
    ty.cast()
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SelfIter(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: a runtime object whose reference the caller owns.
    unsafe { exceptions::retain_object(obj) };
    obj
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `self_` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NextNotImplemented(self_: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type read is within the object.
    let ty = unsafe { header::type_of(self_) };
    // SAFETY: the type name is a valid C string.
    let name = unsafe { util::cstr((*ty).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!("'{}' object is not iterable", name_truncated(name, 200)).as_bytes(),
    );
    core::ptr::null_mut()
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetIter(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        let ty = header::type_of(o);
        match (*ty).tp_iter {
            None => {
                if crate::capi::r#abstract::PySequence_Check(o) != 0 {
                    return crate::capi::iterobject::PySeqIter_New(o);
                }
                type_error("'%.200s' object is not iterable", o)
            }
            Some(iter) => {
                let result = iter(o);
                if !result.is_null() && crate::capi::r#abstract::PyIter_Check(result) == 0 {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!(
                            "iter() returned non-iterator of type '{}'",
                            type_name(result, 100)
                        )
                        .as_bytes(),
                    );
                    exceptions::release_object(result);
                    return core::ptr::null_mut();
                }
                result
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAIter(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        let ty = header::type_of(o);
        let aiter = (*ty)
            .tp_as_async
            .as_ref()
            .and_then(|methods| methods.am_aiter);
        match aiter {
            None => type_error("'%.200s' object is not an async iterable", o),
            Some(aiter) => {
                let it = aiter(o);
                if !it.is_null() && crate::capi::r#abstract::PyAIter_Check(it) == 0 {
                    tstate::set_string(
                        exceptions::type_error(),
                        format!(
                            "aiter() returned not an async iterator of type '{}'",
                            type_name(it, 100)
                        )
                        .as_bytes(),
                    );
                    exceptions::release_object(it);
                    return core::ptr::null_mut();
                }
                it
            }
        }
    }
}

// ------------------------------------------------------------------- lengths

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Size(o: *mut PyObject) -> isize {
    // SAFETY: the slot reads and calls respect the object's type.
    unsafe {
        if o.is_null() {
            null_error();
            return -1;
        }
        let ty = header::type_of(o);
        if let Some(length) = (*ty).tp_as_sequence.as_ref().and_then(|m| m.sq_length) {
            return length(o);
        }
        crate::capi::r#abstract::PyMapping_Size(o)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Length(o: *mut PyObject) -> isize {
    // SAFETY: the delegation follows the C contract.
    unsafe { PyObject_Size(o) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_LengthHint(o: *mut PyObject, defaultvalue: isize) -> isize {
    // SAFETY: the length and method lookups follow the C contract.
    unsafe {
        let ty = header::type_of(o);
        let has_len = (*ty)
            .tp_as_sequence
            .as_ref()
            .is_some_and(|m| m.sq_length.is_some())
            || (*ty)
                .tp_as_mapping
                .as_ref()
                .is_some_and(|m| m.mp_length.is_some());
        if has_len {
            let result = PyObject_Length(o);
            if result >= 0 {
                return result;
            }
            if exceptions::PyErr_ExceptionMatches(exceptions::type_error()) == 0 {
                return -1;
            }
            tstate::clear();
        }
        // `__length_hint__` is a special method: the type supplies it,
        // never the instance dictionary.
        let name_object = unicode::PyUnicode_FromString(c"__length_hint__".as_ptr());
        if name_object.is_null() {
            return -1;
        }
        let hint = type_lookup(ty, name_object);
        exceptions::release_object(name_object);
        if !tstate::occurred().is_null() {
            return -1;
        }
        if hint.is_null() {
            return defaultvalue;
        }
        let bound = if let Some(get) = (*header::type_of(hint)).tp_descr_get {
            get(hint, o, ty.cast())
        } else {
            exceptions::retain_object(hint);
            hint
        };
        if bound.is_null() {
            return -1;
        }
        let result = crate::capi::call::PyObject_CallNoArgs(bound);
        exceptions::release_object(bound);
        if result.is_null() {
            if exceptions::PyErr_ExceptionMatches(exceptions::type_error()) != 0 {
                tstate::clear();
                return defaultvalue;
            }
            return -1;
        }
        if result == crate::object::singletons::not_implemented() {
            exceptions::release_object(result);
            return defaultvalue;
        }
        if !core::ptr::eq(
            header::type_of(result),
            &raw mut crate::types::long::PyLong_Type,
        ) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "__length_hint__ must be an integer, not {}",
                    type_name(result, 200)
                )
                .as_bytes(),
            );
            exceptions::release_object(result);
            return -1;
        }
        let value = crate::types::long::PyLong_AsSsize_t(result);
        exceptions::release_object(result);
        if value < 0 && !tstate::occurred().is_null() {
            return -1;
        }
        if value < 0 {
            tstate::set_string(
                exceptions::value_error(),
                b"__length_hint__() should return >= 0",
            );
            return -1;
        }
        value
    }
}

// ---------------------------------------------------------- rich comparison

/// CPython's `_Py_SwappedOp`: rich comparison operators to their swapped
/// version.
const SWAPPED_OP: [c_int; 6] = [4, 5, 2, 3, 0, 1];
/// The operators as message text.
const OP_STRINGS: [&str; 6] = ["<", "<=", "==", "!=", ">", ">="];

/// CPython's `do_richcompare`: reflected slot first when the right
/// operand's type is a proper subclass, then left, then right, then the
/// identity default for `==` and `!=`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
unsafe fn do_richcompare(v: *mut PyObject, w: *mut PyObject, op: c_int) -> *mut PyObject {
    use crate::object::typeobject::{Py_EQ, Py_NE};
    // SAFETY: the slot reads and calls respect both objects' types.
    unsafe {
        let ty_v = header::type_of(v);
        let ty_w = header::type_of(w);
        let mut checked_reverse = false;
        if !core::ptr::eq(ty_v, ty_w)
            && PyType_IsSubtype(ty_w, ty_v) != 0
            && let Some(f) = (*ty_w).tp_richcompare
        {
            checked_reverse = true;
            let res = f(w, v, SWAPPED_OP[op as usize]);
            if !core::ptr::eq(res, singletons::not_implemented()) {
                return res;
            }
            exceptions::release_object(res);
        }
        if let Some(f) = (*ty_v).tp_richcompare {
            let res = f(v, w, op);
            if !core::ptr::eq(res, singletons::not_implemented()) {
                return res;
            }
            exceptions::release_object(res);
        }
        if !checked_reverse && let Some(f) = (*ty_w).tp_richcompare {
            let res = f(w, v, SWAPPED_OP[op as usize]);
            if !core::ptr::eq(res, singletons::not_implemented()) {
                return res;
            }
            exceptions::release_object(res);
        }
        match op {
            x if x == Py_EQ => {
                let res = if core::ptr::eq(v, w) {
                    singletons::true_()
                } else {
                    singletons::false_()
                };
                exceptions::retain_object(res);
                res
            }
            x if x == Py_NE => {
                let res = if core::ptr::eq(v, w) {
                    singletons::false_()
                } else {
                    singletons::true_()
                };
                exceptions::retain_object(res);
                res
            }
            _ => {
                tstate::set_string(
                    exceptions::type_error(),
                    format!(
                        "'{}' not supported between instances of '{}' and '{}'",
                        OP_STRINGS[op as usize],
                        type_name(v, 100),
                        type_name(w, 100)
                    )
                    .as_bytes(),
                );
                core::ptr::null_mut()
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_RichCompare(
    v: *mut PyObject,
    w: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: the comparison follows the C contract.
    unsafe {
        use crate::object::typeobject::{Py_GE, Py_LT};
        debug_assert!((Py_LT..=Py_GE).contains(&op));
        if v.is_null() || w.is_null() {
            if tstate::occurred().is_null() {
                tstate::set_string(
                    exceptions::system_error(),
                    b"null argument to internal routine",
                );
            }
            return core::ptr::null_mut();
        }
        if !crate::capi::recursion::enter(" in comparison") {
            return core::ptr::null_mut();
        }
        let result = do_richcompare(v, w, op);
        crate::capi::recursion::leave();
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_RichCompareBool(
    v: *mut PyObject,
    w: *mut PyObject,
    op: c_int,
) -> c_int {
    use crate::object::typeobject::{Py_EQ, Py_NE};
    // Identity implies equality, and vice versa for inequality.
    if core::ptr::eq(v, w) {
        if op == Py_EQ {
            return 1;
        }
        if op == Py_NE {
            return 0;
        }
    }
    // SAFETY: the comparison follows the C contract.
    let res = unsafe { PyObject_RichCompare(v, w, op) };
    if res.is_null() {
        return -1;
    }
    // SAFETY: a live result object.
    let ok = if unsafe { crate::types::bool_::is_bool(res) } {
        c_int::from(core::ptr::eq(res, singletons::true_()))
    } else {
        // SAFETY: the delegation follows the C contract.
        unsafe { PyObject_IsTrue(res) }
    };
    // SAFETY: the runtime owns this reference.
    unsafe { exceptions::release_object(res) };
    ok
}

// ------------------------------------------------------------ isinstance tree

/// CPython's `abstract_get_bases`: the class's `__bases__` tuple.
///
/// # Safety
///
/// `cls` must be a valid object.
unsafe fn abstract_get_bases(cls: *mut PyObject) -> *mut PyObject {
    // SAFETY: the lookup follows the C contract.
    unsafe {
        let name_object = unicode::PyUnicode_FromString(c"__bases__".as_ptr());
        if name_object.is_null() {
            return core::ptr::null_mut();
        }
        let mut bases = core::ptr::null_mut();
        let found = _PyObject_LookupAttr(cls, name_object, &raw mut bases);
        exceptions::release_object(name_object);
        let _ = found;
        if !bases.is_null() && !crate::types::tuple::is_tuple(bases) {
            exceptions::release_object(bases);
            return core::ptr::null_mut();
        }
        bases
    }
}

/// CPython's `abstract_issubclass`: `derived` against `cls` through the
/// `__bases__` graph.
///
/// # Safety
///
/// `derived` and `cls` must be valid objects.
unsafe fn abstract_issubclass(derived: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the walk follows the C contract.
    unsafe {
        let mut derived = derived;
        let mut bases = core::ptr::null_mut();
        loop {
            if core::ptr::eq(derived, cls) {
                exceptions::release_object(bases);
                return 1;
            }
            let next = abstract_get_bases(derived);
            exceptions::release_object(bases);
            bases = next;
            if bases.is_null() {
                if !tstate::occurred().is_null() {
                    return -1;
                }
                return 0;
            }
            let count = crate::types::tuple::PyTuple_Size(bases);
            if count == 0 {
                exceptions::release_object(bases);
                return 0;
            }
            if count == 1 {
                derived = crate::types::tuple::PyTuple_GetItem(bases, 0);
                continue;
            }
            break;
        }
        if !crate::capi::recursion::enter(" in __issubclass__") {
            exceptions::release_object(bases);
            return -1;
        }
        let mut result = 0;
        for i in 0..crate::types::tuple::PyTuple_Size(bases) {
            result = abstract_issubclass(crate::types::tuple::PyTuple_GetItem(bases, i), cls);
            if result != 0 {
                break;
            }
        }
        crate::capi::recursion::leave();
        exceptions::release_object(bases);
        result
    }
}

/// CPython's `check_class`: `cls` must expose a `__bases__` tuple.
///
/// # Safety
///
/// `cls` must be a valid object.
unsafe fn check_class(cls: *mut PyObject, error: &str) -> bool {
    // SAFETY: the lookup follows the C contract.
    unsafe {
        let bases = abstract_get_bases(cls);
        if bases.is_null() {
            if tstate::occurred().is_null() {
                tstate::set_string(exceptions::type_error(), error.as_bytes());
            }
            return false;
        }
        exceptions::release_object(bases);
        true
    }
}

/// CPython's `object_isinstance`: the type check and the `__class__`
/// override.
///
/// # Safety
///
/// `inst` and `cls` must be valid objects.
unsafe fn object_isinstance(inst: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the lookups follow the C contract.
    unsafe {
        let is_type = chain_is_subtype(
            header::type_of(cls),
            &raw mut crate::object::typeobject::PyType_Type,
        );
        let mut retval;
        if is_type {
            retval = c_int::from(chain_is_subtype(
                header::type_of(inst),
                cls.cast::<PyTypeObject>(),
            ));
            if retval == 0 {
                let name_object = unicode::PyUnicode_FromString(c"__class__".as_ptr());
                if name_object.is_null() {
                    return -1;
                }
                let mut icls = core::ptr::null_mut();
                let found = _PyObject_LookupAttr(inst, name_object, &raw mut icls);
                exceptions::release_object(name_object);
                if found < 0 {
                    return -1;
                }
                if !icls.is_null() {
                    if !core::ptr::eq(icls, header::type_of(inst).cast())
                        && chain_is_subtype(
                            header::type_of(icls),
                            &raw mut crate::object::typeobject::PyType_Type,
                        )
                    {
                        retval = PyType_IsSubtype(
                            icls.cast::<PyTypeObject>(),
                            cls.cast::<PyTypeObject>(),
                        );
                    } else {
                        retval = 0;
                    }
                    exceptions::release_object(icls);
                }
            }
        } else {
            if !check_class(
                cls,
                "isinstance() arg 2 must be a type, a tuple of types, or a union",
            ) {
                return -1;
            }
            let name_object = unicode::PyUnicode_FromString(c"__class__".as_ptr());
            if name_object.is_null() {
                return -1;
            }
            let mut icls = core::ptr::null_mut();
            let found = _PyObject_LookupAttr(inst, name_object, &raw mut icls);
            exceptions::release_object(name_object);
            if found < 0 {
                return -1;
            }
            if icls.is_null() {
                return 0;
            }
            retval = abstract_issubclass(icls, cls);
            exceptions::release_object(icls);
        }
        retval
    }
}

/// CPython's `recursive_issubclass`.
///
/// # Safety
///
/// `derived` and `cls` must be valid objects.
unsafe fn recursive_issubclass(derived: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the checks follow the C contract.
    unsafe {
        let is_type = |o: *mut PyObject| {
            chain_is_subtype(
                header::type_of(o),
                &raw mut crate::object::typeobject::PyType_Type,
            )
        };
        if is_type(cls) && is_type(derived) {
            return PyType_IsSubtype(derived.cast::<PyTypeObject>(), cls.cast::<PyTypeObject>());
        }
        if !check_class(derived, "issubclass() arg 1 must be a class") {
            return -1;
        }
        if !check_class(
            cls,
            "issubclass() arg 2 must be a class, a tuple of classes, or a union",
        ) {
            return -1;
        }
        abstract_issubclass(derived, cls)
    }
}

/// CPython's `object_recursive_isinstance`.
///
/// # Safety
///
/// `inst` and `cls` must be valid objects.
unsafe fn object_recursive_isinstance(inst: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the checks follow the C contract.
    unsafe {
        if core::ptr::eq(header::type_of(inst), cls.cast::<PyTypeObject>()) {
            return 1;
        }
        // `type.__instancecheck__` is this fast path.
        if core::ptr::eq(
            header::type_of(cls),
            &raw mut crate::object::typeobject::PyType_Type,
        ) {
            return object_isinstance(inst, cls);
        }
        if crate::types::tuple::is_tuple(cls) {
            if !crate::capi::recursion::enter(" in __instancecheck__") {
                return -1;
            }
            let mut r = 0;
            for i in 0..crate::types::tuple::PyTuple_Size(cls) {
                r = object_recursive_isinstance(inst, crate::types::tuple::PyTuple_GetItem(cls, i));
                if r != 0 {
                    break;
                }
            }
            crate::capi::recursion::leave();
            return r;
        }
        // A `__instancecheck__` on the class's metaclass overrides the
        // plain check.
        let name_object = unicode::PyUnicode_FromString(c"__instancecheck__".as_ptr());
        if name_object.is_null() {
            return -1;
        }
        let checker = type_lookup(header::type_of(cls), name_object);
        exceptions::release_object(name_object);
        if !tstate::occurred().is_null() {
            return -1;
        }
        if !checker.is_null() {
            let bound = if let Some(get) = (*header::type_of(checker)).tp_descr_get {
                get(checker, cls, header::type_of(cls).cast())
            } else {
                exceptions::retain_object(checker);
                checker
            };
            if bound.is_null() {
                return -1;
            }
            if !crate::capi::recursion::enter(" in __instancecheck__") {
                exceptions::release_object(bound);
                return -1;
            }
            let res = crate::capi::call::PyObject_CallOneArg(bound, inst);
            crate::capi::recursion::leave();
            exceptions::release_object(bound);
            if res.is_null() {
                return -1;
            }
            let ok = PyObject_IsTrue(res);
            exceptions::release_object(res);
            return ok;
        }
        object_isinstance(inst, cls)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `inst` and `cls` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsInstance(inst: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the delegation follows the C contract.
    unsafe { object_recursive_isinstance(inst, cls) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `derived` and `cls` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsSubclass(derived: *mut PyObject, cls: *mut PyObject) -> c_int {
    // SAFETY: the checks follow the C contract.
    unsafe {
        // `type.__subclasscheck__` is this fast path.
        if core::ptr::eq(
            header::type_of(cls),
            &raw mut crate::object::typeobject::PyType_Type,
        ) {
            if core::ptr::eq(derived, cls) {
                return 1;
            }
            return recursive_issubclass(derived, cls);
        }
        if crate::types::tuple::is_tuple(cls) {
            if !crate::capi::recursion::enter(" in __subclasscheck__") {
                return -1;
            }
            let mut r = 0;
            for i in 0..crate::types::tuple::PyTuple_Size(cls) {
                r = PyObject_IsSubclass(derived, crate::types::tuple::PyTuple_GetItem(cls, i));
                if r != 0 {
                    break;
                }
            }
            crate::capi::recursion::leave();
            return r;
        }
        // A `__subclasscheck__` on the class's metaclass overrides the
        // plain check.
        let name_object = unicode::PyUnicode_FromString(c"__subclasscheck__".as_ptr());
        if name_object.is_null() {
            return -1;
        }
        let checker = type_lookup(header::type_of(cls), name_object);
        exceptions::release_object(name_object);
        if !tstate::occurred().is_null() {
            return -1;
        }
        if !checker.is_null() {
            let bound = if let Some(get) = (*header::type_of(checker)).tp_descr_get {
                get(checker, cls, header::type_of(cls).cast())
            } else {
                exceptions::retain_object(checker);
                checker
            };
            if bound.is_null() {
                return -1;
            }
            if !crate::capi::recursion::enter(" in __subclasscheck__") {
                exceptions::release_object(bound);
                return -1;
            }
            let res = crate::capi::call::PyObject_CallOneArg(bound, derived);
            crate::capi::recursion::leave();
            exceptions::release_object(bound);
            if res.is_null() {
                return -1;
            }
            let ok = PyObject_IsTrue(res);
            exceptions::release_object(res);
            return ok;
        }
        recursive_issubclass(derived, cls)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `inst` and `cls` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_RealIsInstance(
    inst: *mut PyObject,
    cls: *mut PyObject,
) -> c_int {
    // SAFETY: the delegation follows the C contract.
    unsafe { object_isinstance(inst, cls) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `derived` and `cls` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_RealIsSubclass(
    derived: *mut PyObject,
    cls: *mut PyObject,
) -> c_int {
    // SAFETY: the delegation follows the C contract.
    unsafe { recursive_issubclass(derived, cls) }
}

// ------------------------------------------------------------- finalizers

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `self_` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFinalizer(self_: *mut PyObject) {
    // SAFETY: the slot read follows the object's type.
    unsafe {
        let ty = header::type_of(self_);
        let Some(finalize) = (*ty).tp_finalize else {
            return;
        };
        // `tp_finalize` runs once, ever: the page bitmap bit is the
        // runtime's record of that, shared with the collector's own
        // finalization phase.
        if let Some((page, granule)) = crate::gc::counted::object_page(self_ as usize)
            && page.finalized(granule)
        {
            return;
        }
        if let Some((page, granule)) = crate::gc::counted::object_page(self_ as usize) {
            page.set_finalized(granule);
        }
        finalize(self_);
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `self_` must be a valid object whose deallocation has begun.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFinalizerFromDealloc(self_: *mut PyObject) -> c_int {
    // SAFETY: the resurrection dance follows the C contract.
    unsafe {
        // A temporary resurrection: the finalizer may hand the object out
        // again, and a zero count during that call is fatal.
        if !header::is_immortal(self_) {
            (*self_).ob_refcnt += 1;
        }
        PyObject_CallFinalizer(self_);
        if !header::is_immortal(self_) {
            (*self_).ob_refcnt -= 1;
        }
        if header::refcnt(self_) == 0 {
            return 0;
        }
        // `tp_finalize` resurrected it through a C reference: the caller
        // must behave as if the deallocation never began. (The runtime's
        // own dealloc driver watches the heap counts as well, before it
        // reaches a `tp_dealloc`.)
        -1
    }
}

// ------------------------------------------------------- managed dictionaries

/// CPython's `_PyObject_ClearManagedDict`: empty an object's managed
/// inline dictionary.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_ClearManagedDict(obj: *mut PyObject) {
    // SAFETY: the flag read is within the type object.
    unsafe {
        if (*header::type_of(obj)).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 {
            return;
        }
        tstate::set_string(
            exceptions::not_implemented_error(),
            b"coil: inline managed dictionaries are not implemented",
        );
    }
}

/// CPython's `_PyObject_VisitManagedDict`.
///
/// # Safety
///
/// `obj` must be a valid object; `visit` a valid visitor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_VisitManagedDict(
    obj: *mut PyObject,
    _visit: coil_capi_sys::visitproc,
    _arg: *mut c_void,
) -> c_int {
    // SAFETY: the flag read is within the type object.
    unsafe {
        if (*header::type_of(obj)).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 {
            return 0;
        }
        tstate::set_string(
            exceptions::not_implemented_error(),
            b"coil: inline managed dictionaries are not implemented",
        );
        -1
    }
}

// --------------------------------------------------------------- weakrefs

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `object` must be a valid object whose deallocation has begun.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_ClearWeakRefs(object: *mut PyObject) {
    // SAFETY: the weak-reference table owns the rows for this referent.
    unsafe { crate::gc::weakref::clear_weakrefs(object) }
}

// ----------------------------------------------------------- file descriptors

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsFileDescriptor(o: *mut PyObject) -> c_int {
    // SAFETY: the lookups follow the C contract.
    unsafe {
        let fd;
        if crate::types::long::is_long(o) {
            fd = crate::types::long::PyLong_AsLong(o) as c_int;
        } else {
            let name_object = unicode::PyUnicode_FromString(c"fileno".as_ptr());
            if name_object.is_null() {
                return -1;
            }
            let mut meth = core::ptr::null_mut();
            let found = _PyObject_LookupAttr(o, name_object, &raw mut meth);
            exceptions::release_object(name_object);
            if found < 0 {
                return -1;
            }
            if found == 0 {
                tstate::set_string(
                    exceptions::type_error(),
                    b"argument must be an int, or have a fileno() method.",
                );
                return -1;
            }
            let fno = crate::capi::call::PyObject_CallNoArgs(meth);
            exceptions::release_object(meth);
            if fno.is_null() {
                return -1;
            }
            if !crate::types::long::is_long(fno) {
                tstate::set_string(exceptions::type_error(), b"fileno() returned a non-integer");
                exceptions::release_object(fno);
                return -1;
            }
            fd = crate::types::long::PyLong_AsLong(fno) as c_int;
            exceptions::release_object(fno);
        }

        if fd == -1 && !tstate::occurred().is_null() {
            return -1;
        }
        if fd < 0 {
            tstate::set_string(
                exceptions::value_error(),
                format!("file descriptor cannot be a negative integer ({})", fd).as_bytes(),
            );
            return -1;
        }
        fd
    }
}

//! `object.h`'s call family (`_PyObject_Call`, `PyObject_Call*`,
//! `PyVectorcall_*`) and `call.h`: calling objects through `tp_call`,
//! the vectorcall protocol, and the C-format argument builders
//! (spec section 10.1). Variadic entry points follow the crate's
//! convention of eight fixed slots standing in for the C argument list.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{Py_TPFLAGS_HAVE_VECTORCALL, PyObject, c_long, c_ulong, vectorcallfunc};

use crate::capi::recursion;
use crate::object::header;
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::tuple::PyTupleObject;
use crate::types::unicode;
use crate::util;

/// `PY_VECTORCALL_ARGUMENTS_OFFSET`: the flag bit of `nargsf` that
/// tells the callee `args[-1]` holds the callable itself.
pub const PY_VECTORCALL_ARGUMENTS_OFFSET: usize = 1usize << 63;

/// CPython 3.12's inline `PyVectorcall_NARGS`: the positional count out
/// of a `nargsf` word.
#[unsafe(no_mangle)]
pub extern "C" fn PyVectorcall_NARGS(nargsf: usize) -> isize {
    (nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET) as isize
}

/// CPython's `_PyVectorcall_FunctionInline`, minus the assertion that
/// the callable is callable (the caller checks `tp_call` itself).
///
/// # Safety
///
/// `callable` must be a valid object.
unsafe fn vectorcall_function(callable: *mut PyObject) -> Option<vectorcallfunc> {
    // SAFETY: the flag, offset and pointer reads are the type's own.
    unsafe {
        let ty = header::type_of(callable);
        if (*ty).tp_flags & Py_TPFLAGS_HAVE_VECTORCALL == 0 {
            return None;
        }
        let offset = (*ty).tp_vectorcall_offset;
        if offset <= 0 {
            return None;
        }
        let ptr = (callable as *mut u8).add(offset as usize) as *const Option<vectorcallfunc>;
        core::ptr::read(ptr)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyVectorcall_Function(callable: *mut PyObject) -> Option<vectorcallfunc> {
    // SAFETY: the reads follow the vectorcall protocol.
    unsafe { vectorcall_function(callable) }
}

/// CPython's `_Py_CheckFunctionResult` for the `callable` form: a null
/// result needs an exception, a non-null result needs none.
///
/// # Safety
///
/// `result` is a reference the runtime owns when non-null.
unsafe fn check_function_result(callable: *mut PyObject, result: *mut PyObject) -> *mut PyObject {
    // SAFETY: the pending-error reads are this thread's own.
    unsafe {
        if result.is_null() {
            if tstate::occurred().is_null() {
                // CPython prints the callable with `%R`.
                let repr = unicode::PyObject_Repr(callable);
                let name = if repr.is_null() {
                    tstate::clear();
                    util::cstr((*header::type_of(callable)).tp_name).to_string()
                } else {
                    let text = String::from_utf8_lossy(&unicode::as_utf8_vec(repr)).into_owned();
                    exceptions::release_object(repr);
                    text
                };
                tstate::set_string(
                    exceptions::system_error(),
                    format!("{name} returned NULL without setting an exception").as_bytes(),
                );
            }
            return core::ptr::null_mut();
        }
        if !tstate::occurred().is_null() {
            let repr = unicode::PyObject_Repr(callable);
            let name = if repr.is_null() {
                tstate::clear();
                util::cstr((*header::type_of(callable)).tp_name).to_string()
            } else {
                let text = String::from_utf8_lossy(&unicode::as_utf8_vec(repr)).into_owned();
                exceptions::release_object(repr);
                text
            };
            exceptions::release_object(result);
            // `_PyErr_FormatFromCauseTstate`: the pending exception is
            // chained below the new one.
            let cause = exceptions::PyErr_GetRaisedException();
            tstate::set_string(
                exceptions::system_error(),
                format!("{name} returned a result with an exception set").as_bytes(),
            );
            (*tstate::current()).context = cause;
            return core::ptr::null_mut();
        }
        result
    }
}

/// CPython's `object_is_not_callable` (modules have no special hint
/// here; the runtime's module type is another task's).
///
/// # Safety
///
/// `callable` must be a valid object.
unsafe fn object_is_not_callable(callable: *mut PyObject) {
    // SAFETY: the type name is a valid C string on a live type.
    let name = unsafe { util::cstr((*header::type_of(callable)).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!("'{}' object is not callable", truncate(name, 200)).as_bytes(),
    );
}

fn truncate(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// CPython's `_PyStack_AsDict`: the keyword names/values of a
/// vectorcall stack as a dictionary.
///
/// # Safety
///
/// `values` holds `PyTuple_Size(kwnames)` valid objects.
unsafe fn stack_as_dict(values: &[*mut PyObject], kwnames: *mut PyObject) -> *mut PyObject {
    // SAFETY: the tuple items and the dictionary are this runtime's.
    unsafe {
        let count = crate::types::tuple::PyTuple_Size(kwnames);
        let dict = crate::types::dict::PyDict_New();
        if dict.is_null() {
            return core::ptr::null_mut();
        }
        for i in 0..count {
            let key = crate::types::tuple::PyTuple_GetItem(kwnames, i);
            if crate::types::dict::PyDict_SetItem(dict, key, values[i as usize]) < 0 {
                exceptions::release_object(dict);
                return core::ptr::null_mut();
            }
        }
        dict
    }
}

/// CPython's `_PyStack_UnpackDict`: positional arguments plus a keyword
/// dictionary as a vectorcall stack with `kwnames`. Returns the stack
/// (owned references, one leading slot for `args[-1]`) and the names.
///
/// # Safety
///
/// `args` holds `nargs` valid objects; `kwargs` is a dictionary.
unsafe fn stack_unpack_dict(
    args: &[*mut PyObject],
    kwargs: *mut PyObject,
) -> Option<(Vec<*mut PyObject>, *mut PyObject)> {
    // SAFETY: the dictionary walk is this runtime's.
    unsafe {
        let nkwargs = crate::types::dict::PyDict_Size(kwargs);
        let kwnames = crate::types::tuple::PyTuple_New(nkwargs);
        if kwnames.is_null() {
            return None;
        }
        let mut stack: Vec<*mut PyObject> = Vec::with_capacity(1 + args.len() + nkwargs as usize);
        stack.push(core::ptr::null_mut());
        for arg in args {
            exceptions::retain_object(*arg);
            stack.push(*arg);
        }
        let mut pos: isize = 0;
        let mut key = core::ptr::null_mut();
        let mut value = core::ptr::null_mut();
        let mut i: isize = 0;
        let mut keys_are_strings = true;
        while crate::types::dict::PyDict_Next(kwargs, &raw mut pos, &raw mut key, &raw mut value)
            != 0
        {
            keys_are_strings &= unicode::check(key);
            if crate::types::tuple::PyTuple_SetItem(kwnames, i, key) < 0 {
                break;
            }
            exceptions::retain_object(value);
            stack.push(value);
            i += 1;
        }
        if !keys_are_strings {
            tstate::set_string(exceptions::type_error(), b"keywords must be strings");
            // `_PyStack_UnpackDict_Free`.
            for arg in &stack[1..] {
                exceptions::release_object(*arg);
            }
            for j in 0..crate::types::tuple::PyTuple_Size(kwnames) {
                let name = crate::types::tuple::PyTuple_GetItem(kwnames, j);
                if !name.is_null() {
                    exceptions::release_object(name);
                }
            }
            exceptions::release_object(kwnames);
            return None;
        }
        Some((stack, kwnames))
    }
}

/// Free a stack produced by [`stack_unpack_dict`].
///
/// # Safety
///
/// The stack holds owned references from index 1 and `kwnames` an
/// owned tuple of names.
unsafe fn stack_unpack_dict_free(stack: &[*mut PyObject], kwnames: *mut PyObject) {
    // SAFETY: the references are this runtime's own.
    unsafe {
        for arg in &stack[1..] {
            exceptions::release_object(*arg);
        }
        for j in 0..crate::types::tuple::PyTuple_Size(kwnames) {
            let name = crate::types::tuple::PyTuple_GetItem(kwnames, j);
            if !name.is_null() {
                exceptions::release_object(name);
            }
        }
        exceptions::release_object(kwnames);
    }
}

/// CPython's `_PyObject_MakeTpCall`: build the argument tuple and
/// keyword dictionary and go through `tp_call`. The leading thread-state
/// parameter is part of the exported ABI; the runtime's thread state is
/// per-thread global, so it is ignored.
///
/// # Safety
///
/// `callable` is a valid object, `args` holds `nargs` valid objects,
/// `keywords` null, a tuple of keyword names, or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_MakeTpCall(
    _tstate: *mut c_void,
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    keywords: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the argument build and the call follow the C contract.
    unsafe {
        let call = (*header::type_of(callable)).tp_call;
        if call.is_none() {
            object_is_not_callable(callable);
            return core::ptr::null_mut();
        }
        let positional = if args.is_null() {
            &[][..]
        } else {
            core::slice::from_raw_parts(args, nargs as usize)
        };
        let argstuple = crate::types::tuple::from_array(positional);
        if argstuple.is_null() {
            return core::ptr::null_mut();
        }

        let mut kwdict = keywords;
        let mut made_dict = false;
        if !keywords.is_null()
            && !crate::types::dict::is_dict(keywords)
            && crate::types::tuple::is_tuple(keywords)
            && crate::types::tuple::PyTuple_Size(keywords) > 0
        {
            // The vectorcall-style keyword tail: the names sit in the
            // tuple, the values past the positional arguments.
            let count = crate::types::tuple::PyTuple_Size(keywords);
            let values = core::slice::from_raw_parts(args.add(positional.len()), count as usize);
            kwdict = stack_as_dict(values, keywords);
            made_dict = true;
            if kwdict.is_null() {
                exceptions::release_object(argstuple);
                return core::ptr::null_mut();
            }
        } else if !keywords.is_null()
            && !crate::types::dict::is_dict(keywords)
            && !crate::types::tuple::is_tuple(keywords)
        {
            kwdict = core::ptr::null_mut();
        }

        let mut result = core::ptr::null_mut();
        if recursion::enter(" while calling a Python object") {
            result = call.unwrap()(callable, argstuple, kwdict);
            recursion::leave();
        }
        exceptions::release_object(argstuple);
        if made_dict {
            exceptions::release_object(kwdict);
        }
        check_function_result(callable, result)
    }
}

/// CPython's `_PyObject_VectorcallTstate`: the shared core of the
/// vectorcall entry points — the vectorcall slot when the type has one,
/// `tp_call` otherwise.
///
/// # Safety
///
/// `args` holds `PyVectorcall_NARGS(nargsf)` valid objects when the
/// count is non-zero; `kwnames` null or a tuple of strings.
unsafe fn vectorcall_tstate(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        match vectorcall_function(callable) {
            None => {
                let nargs = PyVectorcall_NARGS(nargsf);
                _PyObject_MakeTpCall(core::ptr::null_mut(), callable, args, nargs, kwnames)
            }
            Some(func) => {
                let res = func(callable, args, nargsf, kwnames);
                check_function_result(callable, res)
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// As [`vectorcall_tstate`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe { vectorcall_tstate(callable, args, nargsf, kwnames) }
}

/// The tuple's `ob_item` array. Tuple pointers are not item arrays:
/// `ob_item` sits behind the header and size fields.
///
/// # Safety
///
/// `tuple` is a tuple of this runtime.
unsafe fn tuple_items(tuple: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (*tuple.cast::<PyTupleObject>()).ob_item.as_mut_ptr() }
}

/// CPython's `_PyVectorcall_Call`: call a known vectorcall function
/// with a tuple of positional arguments and a keyword dictionary.
///
/// # Safety
///
/// `func` is the callable's vectorcall slot; `tuple` a tuple;
/// `kwargs` null or a dictionary.
unsafe fn vectorcall_call(
    func: vectorcallfunc,
    callable: *mut PyObject,
    tuple: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the tuple items are this runtime's.
    unsafe {
        let nargs = crate::types::tuple::PyTuple_Size(tuple);
        let empty_kwargs = kwargs.is_null() || crate::types::dict::PyDict_Size(kwargs) == 0;
        if empty_kwargs {
            let items = tuple_items(tuple);
            let res = func(callable, items, nargs as usize, core::ptr::null_mut());
            return check_function_result(callable, res);
        }
        let positional = core::slice::from_raw_parts(tuple_items(tuple), nargs as usize);
        let Some((stack, kwnames)) = stack_unpack_dict(positional, kwargs) else {
            return core::ptr::null_mut();
        };
        let res = func(
            callable,
            stack[1..].as_ptr(),
            (nargs as usize) | PY_VECTORCALL_ARGUMENTS_OFFSET,
            kwnames,
        );
        stack_unpack_dict_free(&stack, kwnames);
        check_function_result(callable, res)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable`, `tuple` and `kwargs` follow the C contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyVectorcall_Call(
    callable: *mut PyObject,
    tuple: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the offset read follows the vectorcall protocol.
    unsafe {
        let offset = (*header::type_of(callable)).tp_vectorcall_offset;
        if offset <= 0 {
            not_supporting_vectorcall(callable);
            return core::ptr::null_mut();
        }
        let ptr = (callable as *mut u8).add(offset as usize) as *const Option<vectorcallfunc>;
        let Some(func) = core::ptr::read(ptr) else {
            not_supporting_vectorcall(callable);
            return core::ptr::null_mut();
        };
        vectorcall_call(func, callable, tuple, kwargs)
    }
}

/// # Safety
///
/// `callable` must be a valid object.
unsafe fn not_supporting_vectorcall(callable: *mut PyObject) {
    // SAFETY: the type name is a valid C string on a live type.
    let name = unsafe { util::cstr((*header::type_of(callable)).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!(
            "'{}' object does not support vectorcall",
            truncate(name, 200)
        )
        .as_bytes(),
    );
}

/// CPython's `_PyObject_FastCallDictTstate`: positional arguments plus a
/// keyword dictionary through the fastest protocol available.
///
/// # Safety
///
/// `args` holds `PyVectorcall_NARGS(nargsf)` valid objects; `kwargs`
/// null or a dictionary.
unsafe fn fastcall_dict_tstate(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        let nargs = PyVectorcall_NARGS(nargsf);
        let positional = if args.is_null() {
            &[][..]
        } else {
            core::slice::from_raw_parts(args, nargs as usize)
        };
        let Some(func) = vectorcall_function(callable) else {
            return _PyObject_MakeTpCall(core::ptr::null_mut(), callable, args, nargs, kwargs);
        };
        let res;
        if kwargs.is_null() || crate::types::dict::PyDict_Size(kwargs) == 0 {
            res = func(callable, args, nargsf, core::ptr::null_mut());
        } else {
            let Some((stack, kwnames)) = stack_unpack_dict(positional, kwargs) else {
                return core::ptr::null_mut();
            };
            res = func(
                callable,
                stack[1..].as_ptr(),
                (nargs as usize) | PY_VECTORCALL_ARGUMENTS_OFFSET,
                kwnames,
            );
            stack_unpack_dict_free(&stack, kwnames);
        }
        check_function_result(callable, res)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// As [`fastcall_dict_tstate`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_VectorcallDict(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe { fastcall_dict_tstate(callable, args, nargsf, kwargs) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `func` is a valid object; `args` holds `nargs` valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_FastCall(
    func: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe { vectorcall_tstate(func, args, nargs as usize, core::ptr::null_mut()) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `func` is a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallNoArgs(func: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe { vectorcall_tstate(func, core::ptr::null(), 0, core::ptr::null_mut()) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `func` and `arg` are valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallOneArg(
    func: *mut PyObject,
    arg: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the two-slot stack mirrors CPython's, with the leading
    // slot for `PY_VECTORCALL_ARGUMENTS_OFFSET`.
    unsafe {
        let mut stack = [core::ptr::null_mut::<PyObject>(); 2];
        stack[1] = arg;
        vectorcall_tstate(
            func,
            stack.as_ptr().add(1),
            1 | PY_VECTORCALL_ARGUMENTS_OFFSET,
            core::ptr::null_mut(),
        )
    }
}

/// See the CPython 3.12 contract for this entry point (`_PyObject_Call`
/// with the runtime's own thread state).
///
/// # Safety
///
/// `callable` is a valid object, `args` a tuple, `kwargs` null or a
/// dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_Call(
    _tstate: *mut c_void,
    callable: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if let Some(func) = vectorcall_function(callable) {
            return vectorcall_call(func, callable, args, kwargs);
        }
        let Some(call) = (*header::type_of(callable)).tp_call else {
            object_is_not_callable(callable);
            return core::ptr::null_mut();
        };
        if !recursion::enter(" while calling a Python object") {
            return core::ptr::null_mut();
        }
        let result = call(callable, args, kwargs);
        recursion::leave();
        check_function_result(callable, result)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` is a valid object, `args` a tuple, `kwargs` null or a
/// dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Call(
    callable: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the delegation follows the C contract.
    unsafe { _PyObject_Call(core::ptr::null_mut(), callable, args, kwargs) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` is a valid object; `args` a tuple or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallObject(
    callable: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if args.is_null() {
            return PyObject_CallNoArgs(callable);
        }
        if !crate::types::tuple::is_tuple(args) {
            tstate::set_string(exceptions::type_error(), b"argument list must be a tuple");
            return core::ptr::null_mut();
        }
        _PyObject_Call(core::ptr::null_mut(), callable, args, core::ptr::null_mut())
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` is a valid object, `obj` a valid object, `args` a tuple,
/// `kwargs` null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_Call_Prepend(
    _tstate: *mut c_void,
    callable: *mut PyObject,
    obj: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the stack build follows the C contract.
    unsafe {
        let argcount = crate::types::tuple::PyTuple_Size(args);
        let mut stack: Vec<*mut PyObject> = Vec::with_capacity(argcount as usize + 1);
        stack.push(obj);
        for i in 0..argcount {
            stack.push(crate::types::tuple::PyTuple_GetItem(args, i));
        }
        fastcall_dict_tstate(callable, stack.as_ptr(), (argcount + 1) as usize, kwargs)
    }
}

// ------------------------------------------------ format-string call family

/// CPython's `do_mkvalue` over the slot stand-in: one value out of one
/// format character and one 64-bit slot.
///
/// # Safety
///
/// `slot` holds the C value the format character expects.
unsafe fn build_value(code: u8, slot: u64) -> *mut PyObject {
    // SAFETY: the constructors are the runtime's own.
    unsafe {
        match code {
            b'O' | b'S' | b'N' => {
                let object = slot as *mut PyObject;
                if object.is_null() {
                    tstate::set_string(
                        exceptions::system_error(),
                        b"NULL object passed to Py_BuildValue",
                    );
                    return core::ptr::null_mut();
                }
                if code != b'N' {
                    exceptions::retain_object(object);
                }
                object
            }
            b'b' | b'h' | b'i' => crate::types::long::PyLong_FromLong(slot as c_long),
            b'B' | b'H' => crate::types::long::PyLong_FromLong(slot as u32 as c_long),
            b'l' | b'n' => crate::types::long::PyLong_FromLong(slot as c_long),
            b'I' | b'k' => crate::types::long::PyLong_FromUnsignedLong(slot as c_ulong),
            b'L' => crate::types::long::PyLong_FromLongLong(slot as i64),
            b'K' => crate::types::long::PyLong_FromUnsignedLongLong(slot),
            b'c' => {
                let ch = slot as u8;
                crate::types::bytes::PyBytes_FromStringAndSize(&raw const ch as *const c_char, 1)
            }
            b'C' => crate::types::unicode::PyUnicode_FromOrdinal(slot as c_int),
            b's' | b'z' | b'U' => {
                let text = slot as *const c_char;
                if text.is_null() {
                    exceptions::retain_object(crate::object::singletons::none());
                    crate::object::singletons::none()
                } else {
                    unicode::PyUnicode_FromString(text)
                }
            }
            b'y' => {
                let text = slot as *const c_char;
                if text.is_null() {
                    exceptions::retain_object(crate::object::singletons::none());
                    crate::object::singletons::none()
                } else {
                    let len = core::ffi::CStr::from_ptr(text).to_bytes().len() as isize;
                    crate::types::bytes::PyBytes_FromStringAndSize(text, len)
                }
            }
            other => {
                tstate::set_string(
                    exceptions::system_error(),
                    format!(
                        "coil: unsupported format character '{}' in call-family argument list",
                        other as char
                    )
                    .as_bytes(),
                );
                core::ptr::null_mut()
            }
        }
    }
}

/// CPython's `va_build_stack`/`do_mkstack` over the slot stand-in: the
/// values named by `format` out of the slots. Parenthesised groups build
/// a single tuple argument, CPython's `"()"` style.
///
/// # Safety
///
/// Every slot must hold the value its format character expects.
unsafe fn build_stack(format: &[u8], slots: &[u64]) -> Option<Vec<*mut PyObject>> {
    // SAFETY: the constructors are the runtime's own.
    unsafe {
        let mut stack: Vec<*mut PyObject> = Vec::new();
        let mut slot = 0usize;
        let mut i = 0usize;
        while i < format.len() {
            let code = format[i];
            match code {
                b' ' | b'\t' | b':' | b',' => {
                    i += 1;
                    continue;
                }
                b'(' | b'[' | b'{' => {
                    let (end, close) = match code {
                        b'(' => (b')', b')'),
                        b'[' => (b']', b']'),
                        _ => (b'}', b'}'),
                    };
                    let inner_start = i + 1;
                    let mut depth = 1usize;
                    let mut j = inner_start;
                    while j < format.len() && depth > 0 {
                        if format[j] == code {
                            depth += 1;
                        } else if format[j] == end {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    if depth != 0 {
                        tstate::set_string(
                            exceptions::system_error(),
                            b"unmatched paren in format",
                        );
                        for arg in &stack {
                            exceptions::release_object(*arg);
                        }
                        return None;
                    }
                    let Some(inner) = build_stack(&format[inner_start..j - 1], &slots[slot..])
                    else {
                        for arg in &stack {
                            exceptions::release_object(*arg);
                        }
                        return None;
                    };
                    slot += inner.len();
                    let group = match close {
                        b')' => crate::types::tuple::from_array(&inner),
                        b']' => {
                            let list = crate::types::list::PyList_New(inner.len() as isize);
                            if !list.is_null() {
                                for (k, item) in inner.iter().enumerate() {
                                    if crate::types::list::PyList_SetItem(list, k as isize, *item)
                                        < 0
                                    {
                                        exceptions::release_object(list);
                                        for arg in &stack {
                                            exceptions::release_object(*arg);
                                        }
                                        return None;
                                    }
                                }
                            }
                            list
                        }
                        _ => {
                            let dict = crate::types::dict::PyDict_New();
                            if !dict.is_null() {
                                let mut k = 0usize;
                                while k + 1 < inner.len() {
                                    if crate::types::dict::PyDict_SetItem(
                                        dict,
                                        inner[k],
                                        inner[k + 1],
                                    ) < 0
                                    {
                                        exceptions::release_object(dict);
                                        for arg in &stack {
                                            exceptions::release_object(*arg);
                                        }
                                        return None;
                                    }
                                    k += 2;
                                }
                            }
                            dict
                        }
                    };
                    // The group owns its members; the inner builds were
                    // owned references that transfer into the container.
                    if group.is_null() {
                        for arg in &stack {
                            exceptions::release_object(*arg);
                        }
                        return None;
                    }
                    stack.push(group);
                    i = j;
                    continue;
                }
                _ => {
                    let value = build_value(code, *slots.get(slot)?);
                    if value.is_null() {
                        for arg in &stack {
                            exceptions::release_object(*arg);
                        }
                        return None;
                    }
                    slot += 1;
                    stack.push(value);
                    i += 1;
                    continue;
                }
            }
        }
        Some(stack)
    }
}

/// CPython's `_PyObject_CallFunctionVa` over the slot stand-in.
///
/// # Safety
///
/// The slots carry the C values the format names.
unsafe fn call_function_va(
    callable: *mut PyObject,
    format: *const c_char,
    slots: &[u64; 8],
) -> *mut PyObject {
    // SAFETY: the parse and call follow the C contract.
    unsafe {
        if callable.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        if format.is_null() || *format == 0 {
            return PyObject_CallNoArgs(callable);
        }
        let bytes = core::ffi::CStr::from_ptr(format).to_bytes();
        let Some(stack) = build_stack(bytes, slots) else {
            return core::ptr::null_mut();
        };
        if stack.len() == 1 && crate::types::tuple::is_tuple(stack[0]) {
            // CPython's backward-compatibility case: a single tuple
            // argument means "call with the tuple unpacked".
            let args = stack[0];
            let count = crate::types::tuple::PyTuple_Size(args);
            let items = if count == 0 {
                core::ptr::null()
            } else {
                tuple_items(args)
            };
            let result = vectorcall_tstate(callable, items, count as usize, core::ptr::null_mut());
            exceptions::release_object(args);
            return result;
        }
        let result = vectorcall_tstate(
            callable,
            if stack.is_empty() {
                core::ptr::null()
            } else {
                stack.as_ptr()
            },
            stack.len(),
            core::ptr::null_mut(),
        );
        for arg in &stack {
            exceptions::release_object(*arg);
        }
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// The slots carry the C values the format names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFunction(
    callable: *mut PyObject,
    format: *const c_char,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
    arg7: u64,
    arg8: u64,
) -> *mut PyObject {
    // SAFETY: the parse and call follow the C contract.
    unsafe {
        call_function_va(
            callable,
            format,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// The slots carry the C values the format names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallFunction_SizeT(
    callable: *mut PyObject,
    format: *const c_char,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
    arg7: u64,
    arg8: u64,
) -> *mut PyObject {
    // SAFETY: the parse and call follow the C contract.
    unsafe {
        call_function_va(
            callable,
            format,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        )
    }
}

/// CPython's `callmethod`: a resolved attribute must be callable.
///
/// # Safety
///
/// `callable` is a valid object; the slots carry the format's values.
unsafe fn callmethod(
    callable: *mut PyObject,
    format: *const c_char,
    slots: &[u64; 8],
) -> *mut PyObject {
    // SAFETY: the call follows the C contract.
    unsafe {
        if crate::capi::r#abstract::PyCallable_Check(callable) == 0 {
            let name = util::cstr((*header::type_of(callable)).tp_name);
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "attribute of type '{}' is not callable",
                    truncate(name, 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        call_function_va(callable, format, slots)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` is a valid object; the slots carry the format's values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallMethod(
    obj: *mut PyObject,
    name: *const c_char,
    format: *const c_char,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
    arg7: u64,
    arg8: u64,
) -> *mut PyObject {
    // SAFETY: the lookup and call follow the C contract.
    unsafe {
        if obj.is_null() || name.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        let callable = crate::capi::object::PyObject_GetAttrString(obj, name);
        if callable.is_null() {
            return core::ptr::null_mut();
        }
        let result = callmethod(
            callable,
            format,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        );
        exceptions::release_object(callable);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` and `name` are valid objects; the slots carry the format's
/// values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallMethod(
    obj: *mut PyObject,
    name: *mut PyObject,
    format: *const c_char,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
    arg7: u64,
    arg8: u64,
) -> *mut PyObject {
    // SAFETY: the lookup and call follow the C contract.
    unsafe {
        if obj.is_null() || name.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        let callable = crate::capi::object::PyObject_GetAttr(obj, name);
        if callable.is_null() {
            return core::ptr::null_mut();
        }
        let result = callmethod(
            callable,
            format,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        );
        exceptions::release_object(callable);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` is a valid object; the slots carry the format's values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallMethod_SizeT(
    obj: *mut PyObject,
    name: *const c_char,
    format: *const c_char,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
    arg7: u64,
    arg8: u64,
) -> *mut PyObject {
    // SAFETY: the lookup and call follow the C contract.
    unsafe {
        if obj.is_null() || name.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        let callable = crate::capi::object::PyObject_GetAttrString(obj, name);
        if callable.is_null() {
            return core::ptr::null_mut();
        }
        let result = callmethod(
            callable,
            format,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        );
        exceptions::release_object(callable);
        result
    }
}

// ----------------------------------------------- NULL-terminated call family

/// CPython's `object_vacall` over the slot stand-in: `base` (the object
/// a method was resolved on, when unbound) followed by a
/// null-terminated argument list.
///
/// # Safety
///
/// The slots after `base` are objects or null.
unsafe fn object_vacall(
    base: *mut PyObject,
    callable: *mut PyObject,
    args: &[*mut PyObject],
) -> *mut PyObject {
    // SAFETY: the call follows the C contract.
    unsafe {
        if callable.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        let end = args
            .iter()
            .position(|arg| arg.is_null())
            .unwrap_or(args.len());
        let mut stack: Vec<*mut PyObject> = Vec::with_capacity(end + 1);
        if !base.is_null() {
            stack.push(base);
        }
        stack.extend_from_slice(&args[..end]);
        vectorcall_tstate(
            callable,
            if stack.is_empty() {
                core::ptr::null()
            } else {
                stack.as_ptr()
            },
            stack.len(),
            core::ptr::null_mut(),
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` and `name` are valid objects; the slots are objects, the list
/// null-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallMethodObjArgs(
    obj: *mut PyObject,
    name: *mut PyObject,
    arg1: *mut PyObject,
    arg2: *mut PyObject,
    arg3: *mut PyObject,
    arg4: *mut PyObject,
    arg5: *mut PyObject,
    arg6: *mut PyObject,
    arg7: *mut PyObject,
    arg8: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the method resolution and call follow the C contract.
    unsafe {
        if obj.is_null() || name.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"null argument to internal routine",
            );
            return core::ptr::null_mut();
        }
        let mut callable = core::ptr::null_mut();
        let is_method = crate::capi::object::_PyObject_GetMethod(obj, name, &raw mut callable);
        if callable.is_null() {
            if tstate::occurred().is_null() {
                tstate::set_string(
                    exceptions::system_error(),
                    b"null argument to internal routine",
                );
            }
            return core::ptr::null_mut();
        }
        let slots = [arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8];
        let result = object_vacall(
            if is_method != 0 {
                obj
            } else {
                core::ptr::null_mut()
            },
            callable,
            &slots,
        );
        exceptions::release_object(callable);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `callable` is a valid object; the slots are objects, the list
/// null-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFunctionObjArgs(
    callable: *mut PyObject,
    arg1: *mut PyObject,
    arg2: *mut PyObject,
    arg3: *mut PyObject,
    arg4: *mut PyObject,
    arg5: *mut PyObject,
    arg6: *mut PyObject,
    arg7: *mut PyObject,
    arg8: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the call follows the C contract.
    unsafe {
        object_vacall(
            core::ptr::null_mut(),
            callable,
            &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8],
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name` is a valid object; `args` holds
/// `PyVectorcall_NARGS(nargsf)` valid objects starting with the object
/// the method is resolved on; `kwnames` null or a tuple.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_VectorcallMethod(
    name: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the method resolution and call follow the C contract.
    unsafe {
        let mut callable = core::ptr::null_mut();
        let unbound = crate::capi::object::_PyObject_GetMethod(*args, name, &raw mut callable);
        if callable.is_null() {
            if tstate::occurred().is_null() {
                tstate::set_string(
                    exceptions::system_error(),
                    b"null argument to internal routine",
                );
            }
            return core::ptr::null_mut();
        }
        let (args, nargsf) = if unbound != 0 {
            // An unbound descriptor takes the object as its first
            // argument; `args[-1]` must not be written.
            (args, nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET)
        } else {
            // A bound method: drop the object from the front.
            (args.add(1), nargsf - 1)
        };
        let result = vectorcall_tstate(callable, args, nargsf, kwnames);
        exceptions::release_object(callable);
        result
    }
}

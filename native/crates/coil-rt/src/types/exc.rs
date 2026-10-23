//! The exception hierarchy and the `PyErr_*` protocol (spec sections 8.2
//! and 10.5): `BaseException`'s exact instance layout, every 3.12
//! exception type as a static type object with its `PyExc_*` pointer slot,
//! and the error indicator of section 10.2's thread state — set, fetch and
//! restore, the raised-exception protocol, normalization, chaining as
//! `_PyErr_SetObject` does it, formatting, `PyErr_NewException`'s heap
//! types and `PyErr_WriteUnraisable`.
//!
//! The message of a `PyErr_SetString`-style exception rides in the thread
//! state until str objects exist to carry it in `args` (the container
//! tasks); `args`, `dict` and `notes` stay null on runtime-created
//! instances, and a value passed to `PyErr_SetObject` that is not an
//! exception instance is released rather than kept, since no tuple can
//! hold it yet.
//!
//! Chaining is CPython's: the exception an active handler block holds —
//! the top of the `exc_info` stack — becomes the new exception's
//! `context` (`_PyErr_SetObject`), never the pending exception it
//! replaces, and a context link that would close a cycle is cut.
//! Reference counts are full CPython counts: exception fields own their
//! referents and `tp_dealloc` releases them at zero.
//!
//! `PyErr_Format` is variadic in C; Rust cannot read a `va_list` on the
//! stable toolchain, and the messages this runtime formats carry only
//! integers, pointers and C strings — registers every ABI passes
//! identically — so the export takes eight fixed integer slots. Float
//! conversions (different registers on the ABIs this runtime targets)
//! are not handled, and arguments past the eighth are ignored.

use core::ffi::{c_char, c_int, c_void};
use core::ptr;

use coil_capi_sys::{
    Py_TPFLAGS_BASE_EXC_SUBCLASS, Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC,
    Py_TPFLAGS_HEAPTYPE, PyObject, PyTypeObject, visitproc,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{self, zero_type};
use crate::tstate;
use crate::util;

// --------------------------------------------------------------- the layout

macro_rules! layout_field {
    ($(#[$doc:meta])* $get:ident, $set:ident, $offset:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// `exc` must be null or an instance of a `BaseException` subclass.
        pub(crate) unsafe fn $get(exc: *mut PyObject) -> *mut PyObject {
            if exc.is_null() {
                return ptr::null_mut();
            }
            // SAFETY: the field is at the pinned offset of the exact layout.
            unsafe {
                core::ptr::read_unaligned((exc as *const u8).add($offset) as *const *mut PyObject)
            }
        }

        #[doc = concat!("Write the ", stringify!($get), " field.")]
        ///
        /// # Safety
        ///
        /// `exc` must be an instance of a `BaseException` subclass and
        /// `value` null or an object whose reference the field takes over.
        pub(crate) unsafe fn $set(exc: *mut PyObject, value: *mut PyObject) {
            // SAFETY: as the getter, and the field is writable.
            unsafe {
                core::ptr::write_unaligned(
                    (exc as *mut u8).add($offset) as *mut *mut PyObject,
                    value,
                )
            }
        }
    };
}

/// Reads and writes of the exact `PyBaseExceptionObject` fields (spec 8.2)
/// through the pinned offsets of the ABI module.
mod field {
    use super::*;

    layout_field!(
        /// The instance's `__dict__`, or null.
        dict,
        set_dict,
        coil_rt_abi::cp312::base_exception::DICT
    );
    layout_field!(
        /// The constructor arguments, or null until tuples exist.
        args,
        set_args,
        coil_rt_abi::cp312::base_exception::ARGS
    );
    layout_field!(
        /// `__notes__`, or null.
        notes,
        set_notes,
        coil_rt_abi::cp312::base_exception::NOTES
    );
    layout_field!(
        /// The traceback, or null.
        traceback,
        set_traceback,
        coil_rt_abi::cp312::base_exception::TRACEBACK
    );
    layout_field!(
        /// The chained `__context__`, or null.
        context,
        set_context,
        coil_rt_abi::cp312::base_exception::CONTEXT
    );
    layout_field!(
        /// The chained `__cause__`, or null.
        cause,
        set_cause,
        coil_rt_abi::cp312::base_exception::CAUSE
    );
}

// ------------------------------------------------------------ reference counts

/// Take a reference to `object` for an exception field (CPython's
/// `Py_INCREF`); immortal objects do not count.
///
/// # Safety
///
/// `object` must be null or a valid `PyObject` header.
pub(crate) unsafe fn retain_object(object: *mut PyObject) {
    if object.is_null() || unsafe { header::is_immortal(object) } {
        return;
    }
    // SAFETY: the reference count is the header's first field and this
    // thread owns its reference.
    unsafe { (*object).ob_refcnt += 1 };
}

/// Drop a reference to `object`, destroying it at zero (CPython's
/// `Py_DECREF`); immortal objects are untouched.
///
/// # Safety
///
/// `object` must be null or an object this runtime created whose header is
/// not concurrently accessed.
pub(crate) unsafe fn release_object(object: *mut PyObject) {
    if object.is_null() || unsafe { header::is_immortal(object) } {
        return;
    }
    // SAFETY: as `retain_object`; the dealloc call hands the dead object to
    // its own type.
    unsafe {
        (*object).ob_refcnt -= 1;
        if (*object).ob_refcnt == 0 {
            let ty = header::type_of(object);
            if let Some(dealloc) = (*ty).tp_dealloc {
                dealloc(object);
            } else {
                // Every runtime object is a heap block; a type without a
                // dealloc slot reaches here only through these fields.
                crate::alloc::free_block(object.cast());
            }
        }
    };
}

// --------------------------------------------------------- BaseException slots

unsafe extern "C" fn base_exception_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: every field read is within the instance layout.
    unsafe {
        for referent in [
            field::dict(object),
            field::args(object),
            field::notes(object),
            field::traceback(object),
            field::context(object),
            field::cause(object),
        ] {
            if !referent.is_null() && visit(referent, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn base_exception_clear(object: *mut PyObject) -> c_int {
    // CPython's `BaseException_clear`: null each owned field before
    // releasing it (`Py_CLEAR`'s order).
    // SAFETY: the field accesses are within the instance layout.
    unsafe {
        macro_rules! clear_field {
            ($get:ident, $set:ident) => {{
                let referent = field::$get(object);
                field::$set(object, ptr::null_mut());
                release_object(referent);
            }};
        }
        clear_field!(dict, set_dict);
        clear_field!(args, set_args);
        clear_field!(notes, set_notes);
        clear_field!(traceback, set_traceback);
        clear_field!(context, set_context);
        clear_field!(cause, set_cause);
    }
    0
}

unsafe extern "C" fn base_exception_dealloc(object: *mut PyObject) {
    // CPython's `BaseException_dealloc` with `subtype_dealloc`'s tail:
    // clear the owned fields, free the block, then release the reference
    // the allocation took on the type — a no-op for the immortal static
    // types, the last instance's reference for a heap type.
    // SAFETY: the object is an instance with the exact layout.
    unsafe {
        base_exception_clear(object);
        let ty = header::type_of(object);
        crate::alloc::free_block(object.cast());
        release_object(ty.cast());
    }
}

unsafe extern "C" fn base_exception_new(
    type_: *mut PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    // CPython's `BaseException_new`: a zeroed allocation is the whole
    // instance — every field null, `suppress_context` 0.
    // SAFETY: `type_` was readied, so `tp_alloc` is filled.
    unsafe {
        match (*type_).tp_alloc {
            Some(alloc) => alloc(type_, 0),
            None => {
                tstate::set_string(
                    type_error(),
                    format!("cannot create '{}' instances", util::cstr((*type_).tp_name))
                        .as_bytes(),
                );
                ptr::null_mut()
            }
        }
    }
}

unsafe extern "C" fn base_exception_init(
    object: *mut PyObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> c_int {
    // CPython's `BaseException_init`: no keywords; `args` becomes the
    // instance's arguments. The tuple the calling machinery passes is kept
    // as-is until tuple objects own their protocol.
    // SAFETY: layout reads of the argument containers.
    unsafe {
        if util::dict_used(kwds) > 0 {
            tstate::set_string(type_error(), b"BaseException() takes no keyword arguments");
            return -1;
        }
        let old = field::args(object);
        retain_object(args);
        field::set_args(object, args);
        release_object(old);
    }
    0
}

const fn base_exception_type_object() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"BaseException".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::base_exception::SIZEOF as isize;
    t.tp_dealloc = Some(base_exception_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_BASE_EXC_SUBCLASS
        | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(base_exception_traverse);
    t.tp_clear = Some(base_exception_clear);
    t.tp_init = Some(base_exception_init);
    t.tp_alloc = Some(typeobject::PyType_GenericAlloc);
    t.tp_new = Some(base_exception_new);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `BaseException` type object.
static mut _PyExc_BaseException_Type: PyTypeObject = base_exception_type_object();

/// CPython's `PyExc_BaseException`.
#[unsafe(no_mangle)]
pub static mut PyExc_BaseException: *mut PyObject =
    &raw mut _PyExc_BaseException_Type as *mut PyObject;

/// The `BaseException` type.
pub fn base_exception() -> *mut PyObject {
    &raw mut _PyExc_BaseException_Type as *mut PyObject
}

/// The `BaseException` type object.
pub fn base_exception_type() -> *mut PyTypeObject {
    &raw mut _PyExc_BaseException_Type
}

// ---------------------------------------------------------------- the hierarchy

macro_rules! exception_types {
    ($(
        $(#[$doc:meta])*
        $name:literal, $snake:ident,
        $static_name:ident $(, $pyexc:ident)?
        = $base:expr;
    )+) => {
        $(
            $(#[$doc])*
            static mut $static_name: PyTypeObject = {
                let mut t = zero_type();
                t.tp_name = concat!($name, "\0").as_ptr() as *const c_char;
                t.tp_dealloc = Some(base_exception_dealloc);
                t.tp_flags = Py_TPFLAGS_DEFAULT
                    | Py_TPFLAGS_BASETYPE
                    | Py_TPFLAGS_BASE_EXC_SUBCLASS
                    | Py_TPFLAGS_HAVE_GC;
                t.tp_traverse = Some(base_exception_traverse);
                t.tp_clear = Some(base_exception_clear);
                // `tp_init`/`tp_alloc` are pinned here rather than left to
                // inheritance: a slot whose value the base only inherited
                // from `object` is not copied down.
                t.tp_init = Some(base_exception_init);
                t.tp_alloc = Some(typeobject::PyType_GenericAlloc);
                t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
                t.tp_base = $base;
                t
            };

            $(
                /// CPython's pointer slot for this exception type; the C
                /// API addresses exception types through these.
                #[unsafe(no_mangle)]
                pub static mut $pyexc: *mut PyObject =
                    &raw mut $static_name as *mut PyObject;
            )?

            #[doc = concat!("The `", $name, "` type.")]
            pub fn $snake() -> *mut PyObject {
                &raw mut $static_name as *mut PyObject
            }
        )+

        /// Every exception subtype in dependency order, parents first; the
        /// `PyExc_*` slot of each sits beside it at link time.
        pub(crate) static SUBTYPE_OBJECTS: SubtypeList = SubtypeList(&[$(&raw mut $static_name),+]);
    };
}

/// The `Exception` type object.
pub(crate) fn exception_type() -> *mut PyTypeObject {
    &raw mut _PyExc_Exception_Type
}

/// The `BaseExceptionGroup` type object.
pub(crate) fn base_exception_group_type() -> *mut PyTypeObject {
    &raw mut _PyExc_BaseExceptionGroup_Type
}

/// The `ExceptionGroup` type object.
pub(crate) fn exception_group_type() -> *mut PyTypeObject {
    &raw mut _PyExc_ExceptionGroup_Type
}

/// A static module exception subtype (`PyErr_NewException`'s shape for
/// a plain base): the base exception behaviour under a module-qualified
/// name. The module owns readied-ness.
pub(crate) const fn module_exception_type(
    tp_name: *const c_char,
    base: *mut PyTypeObject,
) -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = tp_name;
    t.tp_dealloc = Some(base_exception_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_BASE_EXC_SUBCLASS
        | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(base_exception_traverse);
    t.tp_clear = Some(base_exception_clear);
    // As in `exception_types!`: inherited-from-`object` slots are pinned
    // here rather than left to inheritance.
    t.tp_init = Some(base_exception_init);
    t.tp_alloc = Some(typeobject::PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t.tp_base = base;
    t
}

/// The static exception subtype list: read-only raw pointers behind a
/// `Sync` wrapper the macro fills in.
pub(crate) struct SubtypeList(&'static [*mut PyTypeObject]);
// SAFETY: the list is written once by `init_exception_types` before any
// other thread can observe it, and read-only afterwards.
unsafe impl Sync for SubtypeList {}

exception_types! {
    /// `Exception`.
    "Exception", exception,
    _PyExc_Exception_Type, PyExc_Exception
    = &raw mut _PyExc_BaseException_Type;
    /// `BaseExceptionGroup`.
    "BaseExceptionGroup", base_exception_group,
    _PyExc_BaseExceptionGroup_Type, PyExc_BaseExceptionGroup
    = &raw mut _PyExc_BaseException_Type;
    /// `GeneratorExit`.
    "GeneratorExit", generator_exit,
    _PyExc_GeneratorExit_Type, PyExc_GeneratorExit
    = &raw mut _PyExc_BaseException_Type;
    /// `KeyboardInterrupt`.
    "KeyboardInterrupt", keyboard_interrupt,
    _PyExc_KeyboardInterrupt_Type, PyExc_KeyboardInterrupt
    = &raw mut _PyExc_BaseException_Type;
    /// `SystemExit`.
    "SystemExit", system_exit,
    _PyExc_SystemExit_Type, PyExc_SystemExit
    = &raw mut _PyExc_BaseException_Type;
    /// `ExceptionGroup`.
    ///
    /// The real bases are `Exception` and `BaseExceptionGroup`, whose MRO
    /// `init_exception_types` registers once both are readied; 3.12.10's
    /// `python312.dll` exports no `PyExc_ExceptionGroup` slot, so this type
    /// has none.
    "ExceptionGroup", exception_group,
    _PyExc_ExceptionGroup_Type
    = &raw mut _PyExc_Exception_Type;
    /// `StopAsyncIteration`.
    "StopAsyncIteration", stop_async_iteration,
    _PyExc_StopAsyncIteration_Type, PyExc_StopAsyncIteration
    = &raw mut _PyExc_Exception_Type;
    /// `StopIteration`.
    "StopIteration", stop_iteration,
    _PyExc_StopIteration_Type, PyExc_StopIteration
    = &raw mut _PyExc_Exception_Type;
    /// `ArithmeticError`.
    "ArithmeticError", arithmetic_error,
    _PyExc_ArithmeticError_Type, PyExc_ArithmeticError
    = &raw mut _PyExc_Exception_Type;
    /// `FloatingPointError`.
    "FloatingPointError", floating_point_error,
    _PyExc_FloatingPointError_Type, PyExc_FloatingPointError
    = &raw mut _PyExc_ArithmeticError_Type;
    /// `OverflowError`.
    "OverflowError", overflow_error,
    _PyExc_OverflowError_Type, PyExc_OverflowError
    = &raw mut _PyExc_ArithmeticError_Type;
    /// `ZeroDivisionError`.
    "ZeroDivisionError", zero_division_error,
    _PyExc_ZeroDivisionError_Type, PyExc_ZeroDivisionError
    = &raw mut _PyExc_ArithmeticError_Type;
    /// `AssertionError`.
    "AssertionError", assertion_error,
    _PyExc_AssertionError_Type, PyExc_AssertionError
    = &raw mut _PyExc_Exception_Type;
    /// `AttributeError`.
    "AttributeError", attribute_error,
    _PyExc_AttributeError_Type, PyExc_AttributeError
    = &raw mut _PyExc_Exception_Type;
    /// `BufferError`.
    "BufferError", buffer_error,
    _PyExc_BufferError_Type, PyExc_BufferError
    = &raw mut _PyExc_Exception_Type;
    /// `EOFError`.
    "EOFError", eof_error,
    _PyExc_EOFError_Type, PyExc_EOFError
    = &raw mut _PyExc_Exception_Type;
    /// `ImportError`.
    "ImportError", import_error,
    _PyExc_ImportError_Type, PyExc_ImportError
    = &raw mut _PyExc_Exception_Type;
    /// `ModuleNotFoundError`.
    "ModuleNotFoundError", module_not_found_error,
    _PyExc_ModuleNotFoundError_Type, PyExc_ModuleNotFoundError
    = &raw mut _PyExc_ImportError_Type;
    /// `LookupError`.
    "LookupError", lookup_error,
    _PyExc_LookupError_Type, PyExc_LookupError
    = &raw mut _PyExc_Exception_Type;
    /// `IndexError`.
    "IndexError", index_error,
    _PyExc_IndexError_Type, PyExc_IndexError
    = &raw mut _PyExc_LookupError_Type;
    /// `KeyError`.
    "KeyError", key_error,
    _PyExc_KeyError_Type, PyExc_KeyError
    = &raw mut _PyExc_LookupError_Type;
    /// `MemoryError`.
    "MemoryError", memory_error,
    _PyExc_MemoryError_Type, PyExc_MemoryError
    = &raw mut _PyExc_Exception_Type;
    /// `NameError`.
    "NameError", name_error,
    _PyExc_NameError_Type, PyExc_NameError
    = &raw mut _PyExc_Exception_Type;
    /// `UnboundLocalError`.
    "UnboundLocalError", unbound_local_error,
    _PyExc_UnboundLocalError_Type, PyExc_UnboundLocalError
    = &raw mut _PyExc_NameError_Type;
    /// `OSError`.
    "OSError", os_error,
    _PyExc_OSError_Type, PyExc_OSError
    = &raw mut _PyExc_Exception_Type;
    /// `ReferenceError`.
    "ReferenceError", reference_error,
    _PyExc_ReferenceError_Type, PyExc_ReferenceError
    = &raw mut _PyExc_Exception_Type;
    /// `RuntimeError`.
    "RuntimeError", runtime_error,
    _PyExc_RuntimeError_Type, PyExc_RuntimeError
    = &raw mut _PyExc_Exception_Type;
    /// `NotImplementedError`.
    "NotImplementedError", not_implemented_error,
    _PyExc_NotImplementedError_Type, PyExc_NotImplementedError
    = &raw mut _PyExc_RuntimeError_Type;
    /// `RecursionError`.
    "RecursionError", recursion_error,
    _PyExc_RecursionError_Type, PyExc_RecursionError
    = &raw mut _PyExc_RuntimeError_Type;
    /// `SyntaxError`.
    "SyntaxError", syntax_error,
    _PyExc_SyntaxError_Type, PyExc_SyntaxError
    = &raw mut _PyExc_Exception_Type;
    /// `IndentationError`.
    "IndentationError", indentation_error,
    _PyExc_IndentationError_Type, PyExc_IndentationError
    = &raw mut _PyExc_SyntaxError_Type;
    /// `TabError`.
    "TabError", tab_error,
    _PyExc_TabError_Type, PyExc_TabError
    = &raw mut _PyExc_IndentationError_Type;
    /// `SystemError`.
    "SystemError", system_error,
    _PyExc_SystemError_Type, PyExc_SystemError
    = &raw mut _PyExc_Exception_Type;
    /// `TypeError`.
    "TypeError", type_error,
    _PyExc_TypeError_Type, PyExc_TypeError
    = &raw mut _PyExc_Exception_Type;
    /// `ValueError`.
    "ValueError", value_error,
    _PyExc_ValueError_Type, PyExc_ValueError
    = &raw mut _PyExc_Exception_Type;
    /// `UnicodeError`.
    "UnicodeError", unicode_error,
    _PyExc_UnicodeError_Type, PyExc_UnicodeError
    = &raw mut _PyExc_ValueError_Type;
    /// `UnicodeDecodeError`.
    "UnicodeDecodeError", unicode_decode_error,
    _PyExc_UnicodeDecodeError_Type, PyExc_UnicodeDecodeError
    = &raw mut _PyExc_UnicodeError_Type;
    /// `UnicodeEncodeError`.
    "UnicodeEncodeError", unicode_encode_error,
    _PyExc_UnicodeEncodeError_Type, PyExc_UnicodeEncodeError
    = &raw mut _PyExc_UnicodeError_Type;
    /// `UnicodeTranslateError`.
    "UnicodeTranslateError", unicode_translate_error,
    _PyExc_UnicodeTranslateError_Type, PyExc_UnicodeTranslateError
    = &raw mut _PyExc_UnicodeError_Type;
    /// `Warning`.
    "Warning", warning,
    _PyExc_Warning_Type, PyExc_Warning
    = &raw mut _PyExc_Exception_Type;
    /// `BytesWarning`.
    "BytesWarning", bytes_warning,
    _PyExc_BytesWarning_Type, PyExc_BytesWarning
    = &raw mut _PyExc_Warning_Type;
    /// `DeprecationWarning`.
    "DeprecationWarning", deprecation_warning,
    _PyExc_DeprecationWarning_Type, PyExc_DeprecationWarning
    = &raw mut _PyExc_Warning_Type;
    /// `EncodingWarning`.
    "EncodingWarning", encoding_warning,
    _PyExc_EncodingWarning_Type, PyExc_EncodingWarning
    = &raw mut _PyExc_Warning_Type;
    /// `FutureWarning`.
    "FutureWarning", future_warning,
    _PyExc_FutureWarning_Type, PyExc_FutureWarning
    = &raw mut _PyExc_Warning_Type;
    /// `ImportWarning`.
    "ImportWarning", import_warning,
    _PyExc_ImportWarning_Type, PyExc_ImportWarning
    = &raw mut _PyExc_Warning_Type;
    /// `PendingDeprecationWarning`.
    "PendingDeprecationWarning", pending_deprecation_warning,
    _PyExc_PendingDeprecationWarning_Type, PyExc_PendingDeprecationWarning
    = &raw mut _PyExc_Warning_Type;
    /// `ResourceWarning`.
    "ResourceWarning", resource_warning,
    _PyExc_ResourceWarning_Type, PyExc_ResourceWarning
    = &raw mut _PyExc_Warning_Type;
    /// `RuntimeWarning`.
    "RuntimeWarning", runtime_warning,
    _PyExc_RuntimeWarning_Type, PyExc_RuntimeWarning
    = &raw mut _PyExc_Warning_Type;
    /// `SyntaxWarning`.
    "SyntaxWarning", syntax_warning,
    _PyExc_SyntaxWarning_Type, PyExc_SyntaxWarning
    = &raw mut _PyExc_Warning_Type;
    /// `UnicodeWarning`.
    "UnicodeWarning", unicode_warning,
    _PyExc_UnicodeWarning_Type, PyExc_UnicodeWarning
    = &raw mut _PyExc_Warning_Type;
    /// `UserWarning`.
    "UserWarning", user_warning,
    _PyExc_UserWarning_Type, PyExc_UserWarning
    = &raw mut _PyExc_Warning_Type;
    /// `BlockingIOError`.
    "BlockingIOError", blocking_io_error,
    _PyExc_BlockingIOError_Type, PyExc_BlockingIOError
    = &raw mut _PyExc_OSError_Type;
    /// `ChildProcessError`.
    "ChildProcessError", child_process_error,
    _PyExc_ChildProcessError_Type, PyExc_ChildProcessError
    = &raw mut _PyExc_OSError_Type;
    /// `ConnectionError`.
    "ConnectionError", connection_error,
    _PyExc_ConnectionError_Type, PyExc_ConnectionError
    = &raw mut _PyExc_OSError_Type;
    /// `BrokenPipeError`.
    "BrokenPipeError", broken_pipe_error,
    _PyExc_BrokenPipeError_Type, PyExc_BrokenPipeError
    = &raw mut _PyExc_ConnectionError_Type;
    /// `ConnectionAbortedError`.
    "ConnectionAbortedError", connection_aborted_error,
    _PyExc_ConnectionAbortedError_Type, PyExc_ConnectionAbortedError
    = &raw mut _PyExc_ConnectionError_Type;
    /// `ConnectionRefusedError`.
    "ConnectionRefusedError", connection_refused_error,
    _PyExc_ConnectionRefusedError_Type, PyExc_ConnectionRefusedError
    = &raw mut _PyExc_ConnectionError_Type;
    /// `ConnectionResetError`.
    "ConnectionResetError", connection_reset_error,
    _PyExc_ConnectionResetError_Type, PyExc_ConnectionResetError
    = &raw mut _PyExc_ConnectionError_Type;
    /// `FileExistsError`.
    "FileExistsError", file_exists_error,
    _PyExc_FileExistsError_Type, PyExc_FileExistsError
    = &raw mut _PyExc_OSError_Type;
    /// `FileNotFoundError`.
    "FileNotFoundError", file_not_found_error,
    _PyExc_FileNotFoundError_Type, PyExc_FileNotFoundError
    = &raw mut _PyExc_OSError_Type;
    /// `InterruptedError`.
    "InterruptedError", interrupted_error,
    _PyExc_InterruptedError_Type, PyExc_InterruptedError
    = &raw mut _PyExc_OSError_Type;
    /// `IsADirectoryError`.
    "IsADirectoryError", is_a_directory_error,
    _PyExc_IsADirectoryError_Type, PyExc_IsADirectoryError
    = &raw mut _PyExc_OSError_Type;
    /// `NotADirectoryError`.
    "NotADirectoryError", not_a_directory_error,
    _PyExc_NotADirectoryError_Type, PyExc_NotADirectoryError
    = &raw mut _PyExc_OSError_Type;
    /// `PermissionError`.
    "PermissionError", permission_error,
    _PyExc_PermissionError_Type, PyExc_PermissionError
    = &raw mut _PyExc_OSError_Type;
    /// `ProcessLookupError`.
    "ProcessLookupError", process_lookup_error,
    _PyExc_ProcessLookupError_Type, PyExc_ProcessLookupError
    = &raw mut _PyExc_OSError_Type;
    /// `TimeoutError`.
    "TimeoutError", timeout_error,
    _PyExc_TimeoutError_Type, PyExc_TimeoutError
    = &raw mut _PyExc_OSError_Type;
}

// ------------------------------------------------------------------- aliases

/// CPython's `PyExc_IOError`: an alias of `PyExc_OSError`.
#[unsafe(no_mangle)]
pub static mut PyExc_IOError: *mut PyObject = &raw mut _PyExc_OSError_Type as *mut PyObject;

/// CPython's `PyExc_EnvironmentError`: an alias of `PyExc_OSError`.
#[unsafe(no_mangle)]
pub static mut PyExc_EnvironmentError: *mut PyObject =
    &raw mut _PyExc_OSError_Type as *mut PyObject;

/// CPython's `PyExc_WindowsError`: an alias of `PyExc_OSError`, exported on
/// every platform this runtime builds for, like the rest of the
/// impersonated DLL's table.
#[unsafe(no_mangle)]
pub static mut PyExc_WindowsError: *mut PyObject = &raw mut _PyExc_OSError_Type as *mut PyObject;

// --------------------------------------------------------- the MemoryError copy

/// The exact `BaseException` instance layout of section 8.2, for the
/// preallocated instance below: every field access on it — `args`,
/// `PyException_GetContext` at offset 48, the collector's traverse — must
/// stay inside the static.
#[repr(C)]
struct PreallocatedException {
    header: PyObject,
    dict: *mut PyObject,
    args: *mut PyObject,
    notes: *mut PyObject,
    traceback: *mut PyObject,
    context: *mut PyObject,
    cause: *mut PyObject,
    suppress_context: c_char,
}

const _: () = {
    assert!(
        core::mem::size_of::<PreallocatedException>() == coil_rt_abi::cp312::base_exception::SIZEOF
    );
};

/// CPython pre-allocates its `MemoryError` instance so `PyErr_NoMemory`
/// never allocates (`errors.c`); ours is an immortal instance of the exact
/// layout with every exception field null, shared by every thread that
/// raises it.
static mut MEMORY_ERROR_INSTANCE: PreallocatedException = PreallocatedException {
    header: PyObject {
        ob_refcnt: crate::object::header::IMMORTAL_REFCNT,
        ob_type: &raw mut _PyExc_MemoryError_Type,
    },
    dict: ptr::null_mut(),
    args: ptr::null_mut(),
    notes: ptr::null_mut(),
    traceback: ptr::null_mut(),
    context: ptr::null_mut(),
    cause: ptr::null_mut(),
    suppress_context: 0,
};

/// The shared `MemoryError` instance.
fn memory_error_instance() -> *mut PyObject {
    &raw mut MEMORY_ERROR_INSTANCE as *mut PyObject
}

// ----------------------------------------------------------------------- init

/// Ready the hierarchy in dependency order. Idempotent through each type's
/// READY flag; `typeobject::init_static_types` calls it after the core
/// types. `ExceptionGroup`'s real MRO is registered once its two bases are
/// readied, standing in for the `tp_bases` tuple until tuples exist.
pub fn init_exception_types() -> c_int {
    // SAFETY: init runs before any concurrent use; see `PyType_Ready`.
    unsafe {
        if typeobject::PyType_Ready(base_exception_type()) < 0 {
            return -1;
        }
        for t in SUBTYPE_OBJECTS.0 {
            if typeobject::PyType_Ready(*t) < 0 {
                return -1;
            }
        }
        if !typeobject::register_mro(
            exception_group_type(),
            &[exception_type(), base_exception_group_type()],
        ) {
            return -1;
        }
    }
    0
}

// ------------------------------------------------------------ the error state

/// Whether `object` is an instance of a `BaseException` subclass
/// (CPython's `PyExceptionInstance_Check`).
fn is_exception_instance(object: *mut PyObject) -> bool {
    !object.is_null()
        // SAFETY: a header read of a live object.
        && unsafe {
            typeobject::PyType_IsSubtype(header::type_of(object), base_exception_type()) != 0
        }
}

/// Whether `object` is an exception class (CPython's
/// `PyExceptionClass_Check`): a type that derives from `BaseException`.
fn is_exception_class(object: *mut PyObject) -> bool {
    !object.is_null()
        // SAFETY: header reads of a live object.
        && unsafe {
            typeobject::PyType_IsSubtype(
                header::type_of(object),
                &raw mut typeobject::PyType_Type,
            ) != 0
                && typeobject::PyType_IsSubtype(
                    object as *mut PyTypeObject,
                    base_exception_type(),
                ) != 0
        }
}

/// `_PyErr_CreateException` without a calling machinery: an empty
/// instance of `exception_type` through its `tp_new` — the value CPython
/// would pass as the constructor argument rides in `args`, which stays
/// null until tuples exist. Null on failure with that failure set.
unsafe fn create_instance(exception_type: *mut PyTypeObject) -> *mut PyObject {
    // SAFETY: the type was readied, so `tp_new` is callable on it.
    match unsafe { (*exception_type).tp_new } {
        Some(new) => {
            // SAFETY: `tp_new`'s contract over this runtime's own type.
            let exc = unsafe { new(exception_type, ptr::null_mut(), ptr::null_mut()) };
            if exc.is_null() {
                // `tp_new` set the failure (allocation failure, usually).
                return ptr::null_mut();
            }
            if !is_exception_instance(exc) {
                let name = unsafe { util::cstr((*exception_type).tp_name) };
                set_string(
                    type_error(),
                    format!("calling {name} should have returned an instance of BaseException")
                        .as_bytes(),
                );
                // SAFETY: the non-instance result is exclusively ours.
                unsafe { release_object(exc) };
                return ptr::null_mut();
            }
            exc
        }
        None => {
            let name = unsafe { util::cstr((*exception_type).tp_name) };
            set_string(
                type_error(),
                format!("cannot create '{name}' instances").as_bytes(),
            );
            ptr::null_mut()
        }
    }
}

/// Steal the pending exception and its message (`_PyErr_GetRaisedException`).
fn get_raised() -> *mut PyObject {
    let state = tstate::current();
    // SAFETY: `state` is the current thread's own state.
    unsafe {
        let exc = (*state).current_exception;
        (*state).current_exception = ptr::null_mut();
        (*state).message.clear();
        exc
    }
}

/// Install `exc` (ownership taken) as the pending exception and clear the
/// message sidecar (`_PyErr_SetRaisedException`).
fn set_raised_owned(exc: *mut PyObject) {
    let state = tstate::current();
    // SAFETY: `state` is the current thread's own state.
    unsafe {
        let old = (*state).current_exception;
        (*state).current_exception = exc;
        (*state).message.clear();
        release_object(old);
    }
}

/// CPython's `_PyErr_GetTopmostException`: the innermost `exc_info`
/// entry holding an exception, skipping null and `None` values on the
/// way out. With no active handler the stack is empty and there is
/// nothing to chain — the pending exception is never a chaining source.
fn topmost_exception_value(state: *mut tstate::ThreadState) -> *mut PyObject {
    // SAFETY: `state` is a live thread state of the current thread.
    unsafe {
        for item in (*state).exc_info.iter().rev() {
            if !item.exc_value.is_null() && !ptr::eq(item.exc_value, singletons::none()) {
                return item.exc_value;
            }
        }
    }
    ptr::null_mut()
}

/// `PyException_SetContext`'s body: the context field takes `context`'s
/// reference (3.12's `Py_XSETREF` — the caller's reference is stolen),
/// releasing the context it replaces.
unsafe fn set_context_field(exc: *mut PyObject, context: *mut PyObject) {
    // SAFETY: the field is within the instance and owned while held.
    unsafe {
        let replaced = field::context(exc);
        field::set_context(exc, context);
        release_object(replaced);
    }
}

/// The implicit-chaining step of `_PyErr_SetObject`: `old`'s new
/// reference — taken up front, as CPython's `Py_INCREF` does — becomes
/// `exc`'s `context`, replacing any context `exc` already has. A context
/// link that would close a cycle is cut first, with the same
/// tortoise-and-hare walk so a pre-existing cycle cannot hang it.
unsafe fn chain_context(exc: *mut PyObject, old: *mut PyObject) {
    // SAFETY: every field access is within exception instances and the
    // referents are owned while held.
    unsafe {
        retain_object(old);
        let mut node = old;
        let mut slow = old;
        let mut toggle = false;
        loop {
            let context = field::context(node);
            if context.is_null() {
                break;
            }
            if ptr::eq(context, exc) {
                // `PyException_SetContext(o, NULL)`: the link's reference
                // goes with it.
                field::set_context(node, ptr::null_mut());
                release_object(context);
                break;
            }
            node = context;
            if ptr::eq(node, slow) {
                break;
            }
            if toggle {
                slow = field::context(slow);
            }
            toggle = !toggle;
        }
        set_context_field(exc, old);
    }
}

/// Record an instance of `exc_type` with `message` as the pending
/// exception — `PyErr_SetString`'s body and the error path every runtime
/// helper raises through. The exception an active handler holds is
/// chained below it as `_PyErr_SetObject` does.
pub fn set_string(exc_type: *mut PyObject, message: &[u8]) {
    // SAFETY: `exc_type` is an exception class of a readied type.
    unsafe {
        let state = tstate::current();
        let exc = create_instance(exc_type as *mut PyTypeObject);
        if exc.is_null() {
            // The failure is already set, message included.
            return;
        }
        let topmost = topmost_exception_value(state);
        if !topmost.is_null() && !ptr::eq(topmost, exc) {
            chain_context(exc, topmost);
        }
        set_raised_owned(exc);
        // SAFETY: `state` is the current thread's own state.
        (*state).message.extend_from_slice(message);
    }
}

/// The pending exception's type, or null when none is set
/// (`PyErr_Occurred`'s borrowed result).
pub fn occurred() -> *mut PyObject {
    let state = tstate::current();
    // SAFETY: `state` is the current thread's own state.
    let exc = unsafe { (*state).current_exception };
    if exc.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: a header read of the pending exception.
    unsafe { header::type_of(exc).cast() }
}

/// The raw message recorded by the pending `PyErr_SetString`-style
/// exception, empty when none is set or it was raised by other means.
pub fn message() -> Vec<u8> {
    // SAFETY: `tstate::current()` is the current thread's own state.
    unsafe { (*tstate::current()).message.clone() }
}

/// Drop the pending exception (`PyErr_Clear`).
pub fn clear() {
    set_raised_owned(ptr::null_mut());
}

/// `_PyErr_Restore`: install `(type_, value, traceback)` as the pending
/// exception, taking ownership of all three.
unsafe fn restore_raw(type_: *mut PyObject, value: *mut PyObject, traceback: *mut PyObject) {
    // SAFETY: the error state and the exception fields stay within this
    // runtime's objects.
    unsafe {
        if type_.is_null() {
            debug_assert!(
                value.is_null() && traceback.is_null(),
                "PyErr_Restore: type is NULL but value or traceback is not"
            );
            set_raised_owned(ptr::null_mut());
            release_object(value);
            release_object(traceback);
            return;
        }
        if !is_exception_class(type_) {
            set_string(
                system_error(),
                b"PyErr_Restore: type is not a BaseException subclass",
            );
            release_object(type_);
            release_object(value);
            release_object(traceback);
            return;
        }
        let ty = type_ as *mut PyTypeObject;
        // Already normalized only when the types match exactly.
        let value = if !value.is_null() && ptr::eq(header::type_of(value), ty) {
            value
        } else {
            let exc = create_instance(ty);
            release_object(value);
            if exc.is_null() {
                release_object(type_);
                release_object(traceback);
                return;
            }
            exc
        };
        // A traceback is a traceback object or None; neither can be built
        // until the frame task owns the traceback type, so anything else
        // is CPython's TypeError.
        let traceback = if traceback.is_null() || ptr::eq(traceback, singletons::none()) {
            release_object(traceback);
            ptr::null_mut()
        } else {
            set_string(type_error(), b"traceback must be a Traceback or None");
            release_object(value);
            release_object(type_);
            release_object(traceback);
            return;
        };
        let old = field::traceback(value);
        field::set_traceback(value, traceback);
        release_object(old);
        release_object(type_);
        set_raised_owned(value);
    }
}

/// `_PyErr_SetObject`'s body: normalize `value` to an instance of
/// `exception`, chain the exception an active handler holds below it,
/// and install it; `exception` is borrowed, `value` borrowed and released.
unsafe fn set_object(exception: *mut PyObject, value: *mut PyObject) {
    // SAFETY: the error state and the exception fields stay within this
    // runtime's objects.
    unsafe {
        if exception.is_null() || !is_exception_class(exception) {
            set_string(
                system_error(),
                format!(
                    "_PyErr_SetObject: exception {} is not a BaseException subclass",
                    default_repr(exception)
                )
                .as_bytes(),
            );
            return;
        }
        let mut is_subclass = false;
        if !value.is_null() && is_exception_instance(value) {
            is_subclass = typeobject::PyType_IsSubtype(
                header::type_of(value),
                exception as *mut PyTypeObject,
            ) != 0;
        }
        retain_object(value);
        let value = if is_subclass {
            value
        } else {
            // Normalize now; the pending exception is cleared first
            // (CPython issue #23571) because creation can fail.
            set_raised_owned(ptr::null_mut());
            let fixed = create_instance(exception as *mut PyTypeObject);
            release_object(value);
            if fixed.is_null() {
                return;
            }
            fixed
        };
        let topmost = topmost_exception_value(tstate::current());
        if !topmost.is_null() {
            retain_object(topmost);
            if ptr::eq(topmost, value) {
                release_object(topmost);
            } else {
                chain_context(value, topmost);
            }
        }
        set_raised_owned(value);
    }
}

// --------------------------------------------------------------- PyErr_* core

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// The error state belongs to the calling thread and must not be
/// concurrently cleared by it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Occurred() -> *mut PyObject {
    occurred()
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Clear() {
    clear();
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `string` must be null or a null-terminated C string that outlives the
/// call; `exception` an exception class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetString(exception: *mut PyObject, string: *const c_char) {
    // SAFETY: the caller's contract on `string`.
    let bytes = if string.is_null() {
        &[][..]
    } else {
        unsafe { core::ffi::CStr::from_ptr(string).to_bytes() }
    };
    set_string(exception, bytes);
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `exception` must be an exception class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetNone(exception: *mut PyObject) {
    // SAFETY: the caller's contract on `exception`.
    unsafe { set_object(exception, ptr::null_mut()) }
}

/// See the CPython 3.12 contract for this entry point: `value` is
/// normalized to an instance of `exception`, and the exception an active
/// handler holds becomes its `context`, as `_PyErr_SetObject` does.
///
/// # Safety
///
/// `exception` must be null or an exception class; `value` any object or
/// null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetObject(exception: *mut PyObject, value: *mut PyObject) {
    // SAFETY: the caller's contract on `exception` and `value`.
    unsafe { set_object(exception, value) }
}

/// See the CPython 3.12 contract for this entry point; ownership of all
/// three arguments is taken.
///
/// # Safety
///
/// `type_` must be null or an exception class; `value` and `traceback`
/// objects of this runtime or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Restore(
    type_: *mut PyObject,
    value: *mut PyObject,
    traceback: *mut PyObject,
) {
    // SAFETY: the caller's contract on the three arguments.
    unsafe { restore_raw(type_, value, traceback) }
}

/// See the CPython 3.12 contract for this entry point; ownership of each
/// result moves to the caller.
///
/// # Safety
///
/// Each out-pointer must be null or point at writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Fetch(
    p_type: *mut *mut PyObject,
    p_value: *mut *mut PyObject,
    p_traceback: *mut *mut PyObject,
) {
    let exc = get_raised();
    // SAFETY: the caller's contract on the out-pointers.
    unsafe {
        if !p_type.is_null() {
            *p_type = if exc.is_null() {
                ptr::null_mut()
            } else {
                let ty = header::type_of(exc);
                retain_object(ty.cast());
                ty.cast()
            };
        }
        if !p_value.is_null() {
            *p_value = exc;
        }
        if !p_traceback.is_null() {
            *p_traceback = if exc.is_null() {
                ptr::null_mut()
            } else {
                let tb = field::traceback(exc);
                retain_object(tb);
                tb
            };
        }
    }
}

/// See the CPython 3.12 contract for this entry point; ownership of the
/// result moves to the caller.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_GetRaisedException() -> *mut PyObject {
    get_raised()
}

/// See the CPython 3.12 contract for this entry point; ownership of `exc`
/// is taken.
///
/// # Safety
///
/// `exc` must be null or an exception instance of this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetRaisedException(exc: *mut PyObject) {
    set_raised_owned(exc);
}

/// See the CPython 3.12 contract for this entry point. CPython loops
/// until normalization succeeds; this runtime takes the single fetch of
/// the failure, which is the same state CPython's first restart leaves.
///
/// # Safety
///
/// Each pointer must point at writable storage holding null or objects
/// of this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NormalizeException(
    p_exc: *mut *mut PyObject,
    p_val: *mut *mut PyObject,
    p_traceback: *mut *mut PyObject,
) {
    // SAFETY: the caller's contract on the three slots.
    unsafe {
        let mut type_ = *p_exc;
        if type_.is_null() {
            return;
        }
        let mut value = *p_val;
        if is_exception_class(type_) {
            let ty = type_ as *mut PyTypeObject;
            let mut in_class: *mut PyTypeObject = ptr::null_mut();
            let mut is_subclass = false;
            if !value.is_null() && is_exception_instance(value) {
                in_class = header::type_of(value);
                is_subclass = typeobject::PyType_IsSubtype(in_class, ty) != 0;
            }
            if !is_subclass {
                let fixed = create_instance(ty);
                release_object(value);
                if fixed.is_null() {
                    release_object(type_);
                    let failure = get_raised();
                    if failure.is_null() {
                        *p_exc = ptr::null_mut();
                        *p_val = ptr::null_mut();
                        if !p_traceback.is_null() {
                            *p_traceback = ptr::null_mut();
                        }
                        return;
                    }
                    let failure_type = header::type_of(failure);
                    retain_object(failure_type.cast());
                    *p_exc = failure_type.cast();
                    *p_val = failure;
                    return;
                }
                value = fixed;
            } else if !ptr::eq(in_class.cast(), type_) {
                // Believe the instance.
                retain_object(in_class.cast());
                release_object(type_);
                type_ = in_class.cast();
            }
        }
        *p_exc = type_;
        *p_val = value;
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// The error state belongs to the calling thread and must not be
/// concurrently cleared by it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ExceptionMatches(exc: *mut PyObject) -> c_int {
    given_exception_matches(occurred(), exc)
}

/// See the CPython 3.12 contract for this entry point. A tuple `exc` —
/// CPython matches any of its elements — cannot exist until the tuple
/// type does; there is nothing to unfold yet.
///
/// # Safety
///
/// Either argument may be any object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GivenExceptionMatches(
    given: *mut PyObject,
    exc: *mut PyObject,
) -> c_int {
    given_exception_matches(given, exc)
}

fn given_exception_matches(given: *mut PyObject, exc: *mut PyObject) -> c_int {
    if given.is_null() || exc.is_null() {
        return 0;
    }
    // `exc` may be an instance: its class is what matches against.
    // SAFETY: header reads of live objects.
    let exc = unsafe {
        if is_exception_instance(exc) {
            header::type_of(exc).cast()
        } else if !is_exception_class(exc) {
            return 0;
        } else {
            exc
        }
    };
    // `given` matches as itself when it is a class, its type otherwise.
    // SAFETY: as above.
    let given_class: *mut PyObject = unsafe {
        if is_exception_class(given) {
            given
        } else {
            header::type_of(given).cast()
        }
    };
    // SAFETY: both are type objects now.
    unsafe {
        typeobject::PyType_IsSubtype(given_class as *mut PyTypeObject, exc as *mut PyTypeObject)
    }
}

/// See the CPython 3.12 contract for this entry point: the preallocated
/// `MemoryError`, so the raise never allocates. Always returns null.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_NoMemory() -> *mut PyObject {
    set_raised_owned(memory_error_instance());
    ptr::null_mut()
}

// ------------------------------------------------------------------- formats

/// The `PyUnicode_FromFormatV` subset this runtime's messages use:
/// `%d`/`%i`, `%u`, `%x`, `%X`, `%o`, `%c`, `%s`, `%p` (the `l` and `z`
/// prefixes change nothing on the 64-bit ABIs this runtime targets), the
/// object forms `%R`, `%S`, `%A` and `%U`, the two-argument `%V`, and
/// `%%`. Float conversions are not handled — they pass through the
/// registers the integer slots cannot carry — and unknown specs copy
/// verbatim.
fn format_message(format: &[u8], args: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut next = 0usize;
    let mut i = 0usize;
    while i < format.len() {
        if format[i] != b'%' {
            out.push(format[i]);
            i += 1;
            continue;
        }
        i += 1;
        while i < format.len() && (format[i] == b'l' || format[i] == b'z') {
            i += 1;
        }
        if i >= format.len() {
            out.push(b'%');
            break;
        }
        let spec = format[i];
        i += 1;
        let mut arg = || {
            let value = args.get(next).copied().unwrap_or(0);
            next += 1;
            value
        };
        match spec {
            b'%' => out.push(b'%'),
            b'd' | b'i' => out.extend_from_slice((arg() as i64).to_string().as_bytes()),
            b'u' => out.extend_from_slice(arg().to_string().as_bytes()),
            b'x' => out.extend_from_slice(format!("{:x}", arg()).as_bytes()),
            b'X' => out.extend_from_slice(format!("{:X}", arg()).as_bytes()),
            b'o' => out.extend_from_slice(format!("{:o}", arg()).as_bytes()),
            b'c' => {
                let code = arg() as u32;
                if let Some(character) = char::from_u32(code) {
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                }
            }
            b's' => {
                let pointer = arg() as *const c_char;
                if pointer.is_null() {
                    out.extend_from_slice(b"(null)");
                } else {
                    // SAFETY: `%s` arguments are null-terminated C strings.
                    out.extend_from_slice(unsafe { core::ffi::CStr::from_ptr(pointer).to_bytes() });
                }
            }
            b'p' => out.extend_from_slice(format!("{:#x}", arg()).as_bytes()),
            b'R' | b'S' | b'A' | b'U' => {
                out.extend_from_slice(default_repr(arg() as *mut PyObject).as_bytes())
            }
            b'V' => {
                let object = arg() as *mut PyObject;
                let fallback = arg() as *const c_char;
                if object.is_null() && !fallback.is_null() {
                    // SAFETY: `%V`'s fallback is a null-terminated string.
                    out.extend_from_slice(unsafe {
                        core::ffi::CStr::from_ptr(fallback).to_bytes()
                    });
                } else {
                    out.extend_from_slice(default_repr(object).as_bytes());
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The default `<type object at address>` form `%R`/`%S`/`%A`/`%U`
/// render until the str and repr machinery exists.
fn default_repr(object: *mut PyObject) -> String {
    if object.is_null() {
        return "(null)".into();
    }
    // SAFETY: header and type-name reads of a live object.
    let ty = unsafe { header::type_of(object) };
    let name = unsafe { util::cstr((*ty).tp_name) };
    format!("<{name} object at {:#x}>", object.addr())
}

/// See the CPython 3.12 contract for this entry point. The C signature is
/// variadic; eight fixed integer slots stand in for the argument list (see
/// the module docs).
///
/// # Safety
///
/// `format` must be a null-terminated C string; `exception` an exception
/// class.
#[allow(clippy::too_many_arguments)] // the C signature's variadic tail
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Format(
    exception: *mut PyObject,
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
    // SAFETY: `format` is a null-terminated C string by contract.
    let bytes = if format.is_null() {
        &[][..]
    } else {
        unsafe { core::ffi::CStr::from_ptr(format).to_bytes() }
    };
    set_string(
        exception,
        &format_message(bytes, &[arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8]),
    );
    ptr::null_mut()
}

// ---------------------------------------------------------- heap exception types

/// `PyErr_NewException`'s body: a heap type named `name` deriving from
/// `base`, `dict` retained as its `tp_dict`. The `str` pieces CPython
/// fills (`ht_name`, `ht_qualname`, `__module__`) stay null until the
/// str type exists; `tp_name` carries the full `module.class` name.
unsafe fn new_exception(
    name: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the block, its fields and the type system's own.
    unsafe {
        let name_bytes = if name.is_null() {
            &[][..]
        } else {
            core::ffi::CStr::from_ptr(name).to_bytes()
        };
        if !name_bytes.contains(&b'.') {
            set_string(
                system_error(),
                b"PyErr_NewException: name must be module.class",
            );
            return ptr::null_mut();
        }
        let base = if base.is_null() { exception() } else { base };
        if !is_exception_class(base) {
            set_string(
                type_error(),
                b"PyErr_NewException: base must be a BaseException subclass",
            );
            return ptr::null_mut();
        }
        let block = crate::alloc::alloc_object(coil_rt_abi::cp312::heaptypeobject::SIZEOF);
        if block.is_null() {
            set_string(memory_error(), b"");
            return ptr::null_mut();
        }
        let ty = block.cast::<PyTypeObject>();
        header::c_init(ty.cast(), &raw mut typeobject::PyType_Type);
        let buffer = crate::alloc::alloc_block(name_bytes.len() + 1);
        if buffer.is_null() {
            crate::alloc::free_block(block);
            set_string(memory_error(), b"");
            return ptr::null_mut();
        }
        // SAFETY: a fresh block sized for the name and its terminator.
        ptr::copy_nonoverlapping(name_bytes.as_ptr(), buffer, name_bytes.len());
        ptr::write(buffer.add(name_bytes.len()), 0);
        (*ty).tp_name = buffer.cast();
        core::ptr::write_unaligned(
            (ty as *mut u8).add(coil_rt_abi::cp312::heaptypeobject::HT_TPNAME) as *mut *mut u8,
            buffer,
        );
        (*ty).tp_flags = Py_TPFLAGS_DEFAULT
            | Py_TPFLAGS_BASETYPE
            | Py_TPFLAGS_BASE_EXC_SUBCLASS
            | Py_TPFLAGS_HAVE_GC
            | Py_TPFLAGS_HEAPTYPE;
        // Instances free through the shared instance dealloc; the type
        // object itself frees through the metatype's `tp_dealloc`.
        (*ty).tp_dealloc = Some(base_exception_dealloc);
        (*ty).tp_traverse = Some(base_exception_traverse);
        (*ty).tp_clear = Some(base_exception_clear);
        (*ty).tp_alloc = Some(typeobject::PyType_GenericAlloc);
        (*ty).tp_free = Some(crate::alloc::pymem::PyObject_Free);
        retain_object(base);
        (*ty).tp_base = base as *mut PyTypeObject;
        retain_object(dict);
        if typeobject::PyType_Ready(ty) < 0 {
            release_object(ty.cast());
            return ptr::null_mut();
        }
        (*ty).tp_dict = dict;
        ty.cast()
    }
}

/// See the CPython 3.12 contract for this entry point; ownership of the
/// result moves to the caller. A `base` that is a tuple of bases —
/// CPython's multi-inheritance form — is rejected until the tuple type
/// exists to carry it.
///
/// # Safety
///
/// `name` must be null or a null-terminated C string; `base` and `dict`
/// objects of this runtime or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewException(
    name: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller's contract on the three arguments.
    unsafe { new_exception(name, base, dict) }
}

/// See the CPython 3.12 contract for this entry point; the docstring is
/// kept where the type machinery reads it (`tp_doc`), borrowed for the
/// type's lifetime, until the str type can hold it as `__doc__`.
///
/// # Safety
///
/// `name` and `doc` must be null or null-terminated C strings; `base`
/// and `dict` objects of this runtime or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewExceptionWithDoc(
    name: *const c_char,
    doc: *const c_char,
    base: *mut PyObject,
    dict: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller's contract on the arguments.
    let type_ = unsafe { new_exception(name, base, dict) };
    if !type_.is_null() && !doc.is_null() {
        // SAFETY: the type is a heap type this runtime just built.
        unsafe { (*(type_ as *mut PyTypeObject)).tp_doc = doc };
    }
    type_
}

/// See the CPython 3.12 contract for this entry point: the default
/// unraisable hook, printing the exception the way
/// `_PyErr_WriteUnraisableMsg` does, with objects in the default repr
/// form until the str machinery exists. The pending exception is cleared.
///
/// # Safety
///
/// `obj` must be an object of this runtime or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WriteUnraisable(obj: *mut PyObject) {
    use std::io::Write;
    // The message sidecar goes with the pending exception, so it is read
    // before the fetch.
    let message = message();
    let exc = get_raised();
    if exc.is_null() {
        return;
    }
    let mut line = String::new();
    if !obj.is_null() && !ptr::eq(obj, singletons::none()) {
        line.push_str("Exception ignored in: ");
        line.push_str(&default_repr(obj));
        line.push('\n');
    }
    // SAFETY: header and type-name reads of the fetched exception.
    let ty = unsafe { header::type_of(exc) };
    line.push_str(unsafe { util::cstr((*ty).tp_name) });
    line.push_str(": ");
    line.push_str(&String::from_utf8_lossy(&message));
    line.push('\n');
    let _ = std::io::stderr().write_all(line.as_bytes());
    // SAFETY: the exception is exclusively ours.
    unsafe { release_object(exc) };
}

/// See the CPython 3.12 contract for this entry point; the result is a
/// new reference.
///
/// # Safety
///
/// `self_` must be null or an exception instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetContext(self_: *mut PyObject) -> *mut PyObject {
    if !is_exception_instance(self_) {
        set_string(
            system_error(),
            b"bad argument to internal function (PyException_GetContext)",
        );
        return ptr::null_mut();
    }
    // SAFETY: the field read is within the instance.
    let context = unsafe { field::context(self_) };
    // SAFETY: the caller owns the new reference.
    unsafe { retain_object(context) };
    context
}

/// See the CPython 3.12 contract for this entry point; the context
/// field takes `context`'s reference (3.12's `Py_XSETREF`).
///
/// # Safety
///
/// `self_` must be an exception instance; `context` any object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetContext(
    self_: *mut PyObject,
    context: *mut PyObject,
) -> c_int {
    if !is_exception_instance(self_) {
        set_string(
            system_error(),
            b"bad argument to internal function (PyException_SetContext)",
        );
        return -1;
    }
    // SAFETY: the instance owns the context field.
    unsafe { set_context_field(self_, context) };
    0
}

// --------------------------------------------------------------- Windows err

/// See the CPython 3.12 contract for this entry point: `OSError` with
/// `FormatMessage`'s text for `ierr` (or `GetLastError()` when zero).
/// The message is rendered from the std error string with CPython's
/// trailing trim; it rides in the thread's message sidecar until `args`
/// can hold it. Always returns null.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_SetFromWindowsErr(ierr: c_int) -> *mut PyObject {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }
    let err = if ierr == 0 {
        // SAFETY: thread-local kernel32 state.
        unsafe { GetLastError() }
    } else {
        ierr as u32
    };
    // CPython trims FormatMessage's trailing whitespace and dots.
    let mut text = std::io::Error::from_raw_os_error(err as i32).to_string();
    if let Some(cut) = text.find(" (os error ") {
        text.truncate(cut);
    }
    while matches!(text.as_bytes().last(), Some(b' ') | Some(b'.')) {
        text.pop();
    }
    set_string(os_error(), format!("[WinError {err}] {text}").as_bytes());
    ptr::null_mut()
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::typeobject;
    use std::sync::{Mutex, MutexGuard, Once};

    static INIT: Once = Once::new();

    /// The tests touch process-global state — static type objects, heap
    /// types through the allocator, the shared `MemoryError` instance and
    /// the thread's exception state — so they serialize, like the GIL tests
    /// do; Rust's default parallel test threads would race them.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn init() -> MutexGuard<'static, ()> {
        INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
        SERIAL.lock().unwrap()
    }

    /// A read of `exc`'s `context` field, borrowed.
    fn context(exc: *mut PyObject) -> *mut PyObject {
        // SAFETY: every raised instance has the exact layout.
        unsafe { field::context(exc) }
    }

    /// A read of `exc`'s reference count.
    fn refcnt(object: *mut PyObject) -> isize {
        // SAFETY: a header read of a live object.
        unsafe { header::refcnt(object) }
    }

    #[test]
    fn set_fetch_restore_round_trips() {
        let _serial = init();
        clear();
        assert!(occurred().is_null());

        set_string(value_error(), b"first message");
        assert!(ptr::eq(occurred(), value_error()));
        assert_eq!(message(), b"first message");
        // Matches read the pending exception's type.
        // SAFETY: matches only read the error state.
        unsafe {
            assert_eq!(PyErr_ExceptionMatches(value_error()), 1);
            assert_eq!(PyErr_ExceptionMatches(exception()), 1);
            assert_eq!(PyErr_ExceptionMatches(type_error()), 0);
        }
        // A class argument matches itself.
        assert_eq!(given_exception_matches(value_error(), value_error()), 1);

        let mut raised_type = ptr::null_mut();
        let mut raised = ptr::null_mut();
        let mut traceback = ptr::null_mut();
        // SAFETY: the three slots are local variables.
        unsafe {
            PyErr_Fetch(&mut raised_type, &mut raised, &mut traceback);
        }
        assert!(occurred().is_null());
        assert!(message().is_empty());
        assert!(ptr::eq(raised_type, value_error()));
        assert!(!raised.is_null());
        assert!(traceback.is_null());
        // SAFETY: a header read of the fetched exception.
        assert!(ptr::eq(
            unsafe { header::type_of(raised) }.cast(),
            value_error()
        ));
        assert_eq!(refcnt(raised), 1);

        // Restore moves ownership back; the round trip repeats.
        // SAFETY: the fetched triple, still owned.
        unsafe {
            PyErr_Restore(raised_type, raised, ptr::null_mut());
        }
        assert!(ptr::eq(occurred(), value_error()));
        assert_eq!(message(), b"");

        // The raised-exception protocol: the same state through 3.12's
        // one-object form.
        // SAFETY: the pending exception moves out.
        let raised = PyErr_GetRaisedException();
        assert!(occurred().is_null());
        // SAFETY: ownership of the instance moves back in.
        unsafe {
            PyErr_SetRaisedException(raised);
        }
        assert!(ptr::eq(occurred(), value_error()));
        clear();
    }

    #[test]
    fn instances_match_the_hierarchy() {
        let _serial = init();
        set_string(zero_division_error(), b"div");
        // SAFETY: matches only read the pending state.
        unsafe {
            assert_eq!(PyErr_ExceptionMatches(arithmetic_error()), 1);
            assert_eq!(PyErr_ExceptionMatches(exception()), 1);
            assert_eq!(PyErr_ExceptionMatches(base_exception()), 1);
            assert_eq!(PyErr_ExceptionMatches(overflow_error()), 0);
            assert_eq!(PyErr_ExceptionMatches(unicode_decode_error()), 0);
            // A class argument matches its subtypes.
            assert_eq!(
                PyErr_GivenExceptionMatches(lookup_error(), lookup_error()),
                1
            );
            assert_eq!(PyErr_GivenExceptionMatches(key_error(), lookup_error()), 1);
        }
        clear();

        // An instance argument matches through its own type.
        // SAFETY: creating an instance of a readied exception type.
        let instance = unsafe { create_instance(file_not_found_error_type_of()) };
        assert!(!instance.is_null());
        assert_eq!(given_exception_matches(instance, os_error()), 1);
        assert_eq!(given_exception_matches(instance, exception()), 1);
        assert_eq!(given_exception_matches(instance, value_error()), 0);
        // SAFETY: the instance is exclusively ours.
        unsafe { release_object(instance) };
    }

    #[test]
    fn exception_group_has_both_bases() {
        let _serial = init();
        // SAFETY: subtype checks read the readied types' MROs.
        unsafe {
            assert_eq!(
                typeobject::PyType_IsSubtype(exception_group_type(), exception_type()),
                1
            );
            assert_eq!(
                typeobject::PyType_IsSubtype(exception_group_type(), base_exception_group_type()),
                1
            );
            assert_eq!(
                typeobject::PyType_IsSubtype(exception_group_type(), base_exception_type()),
                1
            );
            // The linearization keeps C3's order: the direct bases before
            // their common ancestor.
            let mro = typeobject::mro_of(exception_group_type());
            assert!(ptr::eq(mro[1], exception_type()));
            assert!(ptr::eq(mro[2], base_exception_group_type()));
            assert!(ptr::eq(mro[3], base_exception_type()));
        }
    }

    #[test]
    fn pyexc_slots_point_at_the_types() {
        let _serial = init();
        // SAFETY: reads of exported slots' values and type fields.
        unsafe {
            // The data export carries the type object's address.
            assert!(ptr::eq(PyExc_BaseException, base_exception()));
            assert!(ptr::eq(PyExc_Exception, exception()));
            assert!(ptr::eq(PyExc_KeyError, key_error()));
            assert!(ptr::eq(PyExc_TimeoutError, timeout_error()));
            assert!(ptr::eq(PyExc_UnicodeDecodeError, unicode_decode_error()));
            assert!(ptr::eq(PyExc_UserWarning, user_warning()));
            // The aliases of OSError.
            assert!(ptr::eq(PyExc_IOError, os_error()));
            assert!(ptr::eq(PyExc_EnvironmentError, os_error()));
            assert!(ptr::eq(PyExc_WindowsError, os_error()));
            // Each export points at a readied type named for it.
            for export in [
                PyExc_IndexError,
                PyExc_ModuleNotFoundError,
                PyExc_NotImplementedError,
                PyExc_RecursionError,
                PyExc_TabError,
            ] {
                let ty = header::type_of(export);
                assert_eq!(util::cstr((*ty).tp_name).find('\0'), None);
            }
        }
    }

    #[test]
    fn set_object_normalizes_and_chains_from_the_handler_stack() {
        let _serial = init();
        clear();

        // A non-instance value is normalized away: the pending exception
        // becomes an instance of `exception`.
        // SAFETY: `None` is an immortal singleton this runtime owns.
        unsafe {
            PyErr_SetObject(type_error(), singletons::none());
        }
        assert!(ptr::eq(occurred(), type_error()));
        // SAFETY: the instance moves out for inspection.
        let raised = PyErr_GetRaisedException();
        assert!(context(raised).is_null());
        // SAFETY: the instance is exclusively ours again.
        unsafe { release_object(raised) };

        // An instance value whose type derives from `exception` is kept.
        // SAFETY: an instance of a readied exception type.
        let instance = unsafe { create_instance(timeout_error_type_of()) };
        let before = refcnt(instance);
        // SAFETY: SetObject borrows both arguments.
        unsafe {
            PyErr_SetObject(exception(), instance);
        }
        assert!(ptr::eq(occurred(), timeout_error()));
        // SAFETY: the instance moves out; SetObject's reference is the
        // pending exception's.
        let raised = PyErr_GetRaisedException();
        assert!(ptr::eq(raised, instance));
        assert_eq!(refcnt(raised), before + 1);
        // SAFETY: exclusively ours.
        unsafe { release_object(raised) };
        // SAFETY: our own original reference.
        unsafe { release_object(instance) };
        clear();

        // Chaining: the exception the innermost handler holds becomes the
        // new exception's context, exactly once.
        // SAFETY: an instance of a readied exception type.
        let handled = unsafe { create_instance(key_error() as *mut PyTypeObject) };
        let state = tstate::current();
        // SAFETY: the current thread's own state; the stack item owns one
        // reference, as the eval loop's push does.
        unsafe {
            retain_object(handled);
            (*state).exc_info.push(tstate::PyErr_StackItem {
                exc_type: key_error(),
                exc_value: handled,
                exc_traceback: ptr::null_mut(),
            });
        }
        set_string(index_error(), b"under the handler");
        // SAFETY: the pending instance moves out.
        let raised = PyErr_GetRaisedException();
        assert!(ptr::eq(context(raised), handled));
        // Ours, the stack's and the context field's.
        assert_eq!(refcnt(handled), 3);
        // Raising again under the same handler links the same context
        // below the new exception; the first raised instance keeps its
        // own context link until it is released.
        set_string(lookup_error(), b"replaced");
        // SAFETY: as above.
        let second = PyErr_GetRaisedException();
        assert!(ptr::eq(context(second), handled));
        assert_eq!(refcnt(handled), 4);
        // SAFETY: exclusively ours; releasing both raised instances drops
        // their context links.
        unsafe {
            release_object(raised);
            release_object(second);
        }
        assert_eq!(refcnt(handled), 2);
        // SAFETY: the current thread's own state; the stack's reference
        // goes with the pop.
        unsafe {
            let item = (*state).exc_info.pop().unwrap();
            release_object(item.exc_value);
        }
        assert_eq!(refcnt(handled), 1);
        // SAFETY: ours.
        unsafe { release_object(handled) };
        clear();

        // Without an active handler there is no chaining: the replaced
        // pending exception is released, not linked.
        // SAFETY: an instance of a readied exception type.
        let first = unsafe { create_instance(key_error() as *mut PyTypeObject) };
        set_raised_owned(first);
        set_string(value_error(), b"no handler");
        // SAFETY: the pending instance moves out.
        let raised = PyErr_GetRaisedException();
        assert!(context(raised).is_null());
        // `first` reached zero when the pending slot replaced it.
        // SAFETY: exclusively ours.
        unsafe { release_object(raised) };
        clear();
    }

    #[test]
    fn chaining_cuts_cycles() {
        let _serial = init();
        clear();

        // An instance raised twice through SetObject keeps one pending
        // reference and links no context to itself.
        // SAFETY: an instance of a readied exception type.
        let instance = unsafe { create_instance(stop_iteration() as *mut PyTypeObject) };
        let before = refcnt(instance);
        // SAFETY: SetObject borrows both arguments.
        unsafe {
            PyErr_SetObject(exception(), instance);
            PyErr_SetObject(exception(), instance);
        }
        assert_eq!(refcnt(instance), before + 1);
        // SAFETY: the pending exception moves out for inspection.
        let raised = PyErr_GetRaisedException();
        assert!(ptr::eq(raised, instance));
        assert!(context(raised).is_null());
        // SAFETY: exclusively ours.
        unsafe {
            release_object(raised);
            release_object(instance);
        }
        clear();

        // A context link pointing back at the exception being set is
        // cut: the walk finds it and PyException_SetContext(o, NULL)
        // drops its reference before the new context goes in.
        // SAFETY: an instance of a readied exception type.
        let chained = unsafe { create_instance(key_error() as *mut PyTypeObject) };
        // handled.context owns a reference to chained; setting chained
        // again, with handled the innermost handler's exception, would
        // close the cycle chained -> handled -> chained.
        // SAFETY: an instance of a readied exception type.
        let handled = unsafe { create_exception_pointing_at(chained) };
        // Ours, and the context field's.
        let chained_refs = refcnt(chained);
        assert_eq!(chained_refs, 2);
        // SAFETY: the current thread's own state; the stack item owns
        // one reference of handled.
        let state = tstate::current();
        unsafe {
            retain_object(handled);
            (*state).exc_info.push(tstate::PyErr_StackItem {
                exc_type: index_error(),
                exc_value: handled,
                exc_traceback: ptr::null_mut(),
            });
        }
        // SAFETY: SetObject borrows both arguments.
        unsafe {
            PyErr_SetObject(exception(), chained);
        }
        // SetObject's reference became the pending exception's and the
        // cut released the back-edge's: still ours and the pending's.
        assert_eq!(refcnt(chained), chained_refs);
        assert!(context(handled).is_null());
        // The pending exception moves out for inspection.
        let raised = PyErr_GetRaisedException();
        assert!(ptr::eq(raised, chained));
        // The new context is the handler's exception.
        assert!(ptr::eq(context(raised), handled));
        // SAFETY: exclusively ours; the pop takes the stack's reference
        // of handled, and releasing chained drops the context link.
        unsafe {
            release_object(raised);
            let item = (*state).exc_info.pop().unwrap();
            release_object(item.exc_value);
            release_object(handled);
            release_object(chained);
        }
        clear();
    }

    /// An instance whose context field owns a reference to `context`;
    /// the caller keeps its own reference as well.
    unsafe fn create_exception_pointing_at(context: *mut PyObject) -> *mut PyObject {
        // SAFETY: a readied exception type.
        let exc = unsafe { create_instance(index_error() as *mut PyTypeObject) };
        // SAFETY: the instance is ours; the field takes one new reference.
        unsafe {
            retain_object(context);
            set_context_field(exc, context);
        }
        exc
    }

    #[test]
    fn normalize_turns_a_class_into_an_instance() {
        let _serial = init();
        clear();
        // SAFETY: an instance of a readied exception type.
        let instance = unsafe { create_instance(value_error() as *mut PyTypeObject) };
        set_raised_owned(instance);
        let mut raised_type = ptr::null_mut();
        let mut raised = ptr::null_mut();
        let mut traceback = ptr::null_mut();
        // SAFETY: local slots.
        unsafe {
            PyErr_Fetch(&mut raised_type, &mut raised, &mut traceback);
        }
        // A legacy caller's (class, null, null) triple.
        raised = ptr::null_mut();
        // SAFETY: the slots hold the fetched type or null.
        unsafe {
            PyErr_NormalizeException(&mut raised_type, &mut raised, &mut traceback);
        }
        assert!(ptr::eq(raised_type, value_error()));
        assert!(!raised.is_null());
        assert!(is_exception_instance(raised));
        // SAFETY: the results are ours.
        unsafe {
            release_object(raised_type);
            release_object(raised);
        }

        // An instance whose class derives from the given type is believed.
        // SAFETY: an instance of a readied exception type.
        let mut instance = unsafe { create_instance(key_error() as *mut PyTypeObject) };
        let mut raised_type: *mut PyObject = lookup_error();
        // SAFETY: a static immortal type and our own instance.
        unsafe {
            PyErr_NormalizeException(&mut raised_type, &mut instance, ptr::null_mut());
        }
        assert!(ptr::eq(raised_type, key_error()));
        // SAFETY: ours.
        unsafe { release_object(instance) };
        clear();
    }

    #[test]
    fn format_covers_the_message_set() {
        let _serial = init();
        clear();
        // The walker, directly.
        let args = [7u64, c"plain".as_ptr() as u64, 0x1234u64];
        assert_eq!(
            format_message(b"code %d: %s (%p)", &args),
            b"code 7: plain (0x1234)".to_vec()
        );
        assert_eq!(
            format_message(b"%u %x %X %o %i %%", &[255u64, 255, 255, 8, (-2i64) as u64]),
            b"255 ff FF 10 -2 %".to_vec()
        );
        assert_eq!(
            format_message(b"%c%c", &[0x41u64, 0x2603]),
            b"A\xE2\x98\x83".to_vec()
        );
        // Unknown and missing arguments degrade instead of failing.
        assert_eq!(format_message(b"%q %y", &[]), b"q y".to_vec());
        assert_eq!(format_message(b"%d", &[]), b"0".to_vec());

        // End to end: the exception carries the formatted message.
        let name = c"value is %d short".as_ptr();
        // SAFETY: a readied exception type, a static format string and
        // integer arguments.
        unsafe {
            PyErr_Format(value_error(), name, 12, 0, 0, 0, 0, 0, 0, 0);
        }
        assert!(ptr::eq(occurred(), value_error()));
        assert_eq!(message(), b"value is 12 short");
        clear();
    }

    #[test]
    fn no_memory_raises_the_shared_instance() {
        let _serial = init();
        clear();
        assert!(PyErr_NoMemory().is_null());
        assert!(ptr::eq(occurred(), memory_error()));
        // SAFETY: a match reads the pending state.
        unsafe {
            assert_eq!(PyErr_ExceptionMatches(memory_error()), 1);
            assert_eq!(PyErr_ExceptionMatches(exception()), 1);
        }
        // SAFETY: the shared instance moves out; immortal, so releasing
        // changes nothing.
        let raised = PyErr_GetRaisedException();
        assert!(ptr::eq(raised, memory_error_instance()));
        // The shared instance runs the exception protocol like any other
        // instance: the fields the protocol touches — the context at
        // offset 48 — live inside the preallocated block, which is the
        // full 8.2 size exactly so these writes cannot run past it.
        // SAFETY: an instance of a readied exception type.
        let other = unsafe { create_instance(key_error() as *mut PyTypeObject) };
        // SAFETY: protocol calls on our own objects; SetContext steals our
        // single reference on `other` into the field.
        unsafe {
            PyException_SetContext(raised, other);
            let seen = PyException_GetContext(raised);
            assert!(ptr::eq(seen, other));
            release_object(seen);
            PyException_SetContext(raised, ptr::null_mut());
        }
        // SAFETY: immortals ignore releases.
        unsafe { release_object(raised) };
        clear();
    }

    #[test]
    fn new_exception_builds_heap_types() {
        let _serial = init();
        clear();
        // SAFETY: static C strings and null arguments.
        let heap = unsafe {
            PyErr_NewException(c"mymod.MyError".as_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        assert!(!heap.is_null());
        // SAFETY: the heap type this runtime just built.
        let ty = heap as *mut PyTypeObject;
        unsafe {
            assert_eq!(util::cstr((*ty).tp_name), "mymod.MyError");
            assert_eq!(header::refcnt(heap), 1);
            assert!((*ty).tp_flags & Py_TPFLAGS_HEAPTYPE != 0);
            assert!((*ty).tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS != 0);
            // The default base is Exception.
            assert_eq!(typeobject::PyType_IsSubtype(ty, exception_type()), 1);
            // Instances carry the exact BaseException layout and the heap
            // type.
            assert_eq!(
                (*ty).tp_basicsize,
                coil_rt_abi::cp312::base_exception::SIZEOF as isize
            );
        }
        // SAFETY: the heap type is readied, so `tp_new` runs.
        let instance = unsafe { create_instance(ty) };
        // SAFETY: a header read of our own instance.
        assert!(ptr::eq(unsafe { header::type_of(instance) }.cast(), heap));
        set_raised_owned(instance);
        // SAFETY: the pending instance moves out and is released.
        unsafe {
            release_object(PyErr_GetRaisedException());
        }

        // A dotted name is required, a foreign base is refused.
        // SAFETY: static C strings and null arguments.
        unsafe {
            assert!(
                PyErr_NewException(c"NoModule".as_ptr(), ptr::null_mut(), ptr::null_mut())
                    .is_null()
            );
            assert!(ptr::eq(occurred(), system_error()));
            clear();
            assert!(
                PyErr_NewException(c"m.Raises".as_ptr(), singletons::none(), ptr::null_mut())
                    .is_null()
            );
            assert!(ptr::eq(occurred(), type_error()));
            clear();

            // WithDoc keeps the docstring where the type reads it.
            let documented = PyErr_NewExceptionWithDoc(
                c"m.Documented".as_ptr(),
                c"the doc".as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            assert!(!documented.is_null());
            assert!(ptr::eq(
                (*(documented as *mut PyTypeObject)).tp_doc,
                c"the doc".as_ptr()
            ));
            // Ready registered each heap type's MRO; the heap dealloc
            // must forget it again or the address-keyed entry outlives
            // the freed block.
            assert!(typeobject::mro_registered(documented as *mut PyTypeObject));
            assert!(typeobject::mro_registered(heap as *mut PyTypeObject));
            // Releasing each type object runs the metatype's dealloc,
            // which frees the block and must forget the entry first.
            release_object(documented);
            release_object(heap);
            assert!(!typeobject::mro_registered(documented as *mut PyTypeObject));
            assert!(!typeobject::mro_registered(heap as *mut PyTypeObject));
        }
        clear();
    }

    #[test]
    fn write_unraisable_clears_the_state() {
        let _serial = init();
        clear();
        set_string(runtime_error(), b"cannot be raised");
        // SAFETY: the object argument may be any object or null.
        unsafe {
            PyErr_WriteUnraisable(runtime_error());
        }
        assert!(occurred().is_null());
        // With no pending exception the call is a no-op.
        // SAFETY: as above.
        unsafe {
            PyErr_WriteUnraisable(ptr::null_mut());
        }
        assert!(occurred().is_null());
    }

    #[test]
    fn set_none_and_restore_traceback_shapes() {
        let _serial = init();
        clear();
        // SAFETY: a readied exception type.
        unsafe {
            PyErr_SetNone(type_error());
        }
        assert!(ptr::eq(occurred(), type_error()));
        assert!(message().is_empty());
        clear();

        // Restore with a null type clears; with None as the traceback it
        // is accepted; a foreign object is the TypeError CPython raises.
        // SAFETY: immortal static types and the None singleton.
        unsafe {
            PyErr_Restore(value_error(), ptr::null_mut(), ptr::null_mut());
        }
        assert!(ptr::eq(occurred(), value_error()));
        // SAFETY: as above.
        unsafe {
            PyErr_Restore(ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        }
        assert!(occurred().is_null());
        // SAFETY: an instance created through the protocol.
        let instance = unsafe { create_instance(value_error() as *mut PyTypeObject) };
        // SAFETY: our own instance and the None singleton.
        unsafe {
            PyErr_Restore(value_error(), instance, singletons::none());
        }
        assert!(ptr::eq(occurred(), value_error()));
        // SAFETY: our own instance.
        unsafe {
            PyErr_Restore(type_error(), ptr::null_mut(), singletons::none());
        }
        assert!(ptr::eq(occurred(), type_error()));
        clear();
        // A foreign traceback object is refused.
        // SAFETY: static exception types and the RuntimeError type.
        unsafe {
            PyErr_Restore(type_error(), ptr::null_mut(), runtime_error());
        }
        assert!(ptr::eq(occurred(), type_error()));
        clear();
    }

    /// The `FileNotFoundError` type object, which the hierarchy does not
    /// export an accessor for.
    fn file_not_found_error_type_of() -> *mut PyTypeObject {
        // SAFETY: reading an exported slot's value; nothing reassigns it.
        unsafe { PyExc_FileNotFoundError.cast() }
    }

    /// The `TimeoutError` type object.
    fn timeout_error_type_of() -> *mut PyTypeObject {
        // SAFETY: as above.
        unsafe { PyExc_TimeoutError.cast() }
    }
}

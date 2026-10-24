//! The 16-byte `PyObject` header every object starts with, and CPython's
//! PEP 683 immortality convention (spec section 8.1).

use coil_capi_sys::{PyObject, PyTypeObject};

/// CPython 3.12's immortal reference count: `0xFFFF_FFFF`, the value the
/// inline `Py_INCREF`/`Py_DECREF` of extensions treat as a no-op.
pub const IMMORTAL_REFCNT: isize = 0xFFFF_FFFF;

/// Write a fresh immortal header over `object`: the immortal reference
/// count and `ob_type`, for the singletons and static types (PEP 683).
///
/// # Safety
///
/// `object` must point to 16 writable bytes of an allocation that is not
/// concurrently accessed.
pub unsafe fn init(object: *mut PyObject, ob_type: *mut PyTypeObject) {
    // SAFETY: the caller guarantees 16 writable bytes.
    unsafe {
        (*object).ob_refcnt = IMMORTAL_REFCNT;
        (*object).ob_type = ob_type;
    }
}

/// Write the header a C API constructor's result carries: `ob_type` plus
/// the one reference the caller owns (spec section 9.3).
///
/// # Safety
///
/// `object` must point to 16 writable bytes of an allocation that is not
/// concurrently accessed.
pub unsafe fn c_init(object: *mut PyObject, ob_type: *mut PyTypeObject) {
    // SAFETY: the caller guarantees 16 writable bytes.
    unsafe {
        (*object).ob_refcnt = 1;
        (*object).ob_type = ob_type;
        // The constructor's result is a Python object, whatever raw
        // allocation it came from: tag its granule, the role the
        // `is_object` tagging of `PyObject_Init` plays for the collector
        // (spec sections 9.2 and 9.3). Counted-regime objects sit 16
        // bytes into their block; the tag belongs at the object.
        if let Some(page) = crate::alloc::page_of(object as usize) {
            let granule = page.granule_of(page.base(), object as usize);
            page.set_is_object(granule);
            // And it is C-created (section 9.3): a T object of this
            // provenance whose reference count reaches zero without it
            // having escaped into compiled code is freed at once.
            page.set_c_created(granule);
        }
    }
}

/// `ob_type` of `object`.
///
/// # Safety
///
/// `object` must point to a valid `PyObject` header.
pub unsafe fn type_of(object: *const PyObject) -> *mut PyTypeObject {
    // SAFETY: the header is the first 16 bytes of the object.
    unsafe { (*object).ob_type }
}

/// `ob_refcnt` of `object`.
///
/// # Safety
///
/// `object` must point to a valid `PyObject` header.
pub unsafe fn refcnt(object: *const PyObject) -> isize {
    // SAFETY: the header is the first 16 bytes of the object.
    unsafe { (*object).ob_refcnt }
}

/// Whether `object` is immortal by the 3.12 convention.
///
/// # Safety
///
/// `object` must point to a valid `PyObject` header.
pub unsafe fn is_immortal(object: *const PyObject) -> bool {
    // SAFETY: reads only the header.
    unsafe { refcnt(object) == IMMORTAL_REFCNT }
}

/// Record the reference a C API constructor hands out: a heap object of the
/// tracing regime starts with the one reference the caller owns (spec
/// section 9.3); immortal singletons keep their special count. Returns
/// `object` for tail calls.
///
/// # Safety
///
/// `object` must be null or a pointer this runtime produced.
pub unsafe fn c_new_reference(object: *mut PyObject) -> *mut PyObject {
    if object.is_null() || unsafe { is_immortal(object) } {
        return object;
    }
    // SAFETY: the object is this runtime's own and not yet shared.
    unsafe { (*object).ob_refcnt = 1 };
    object
}

//! `list`: CPython 3.12's `PyListObject` — the exact layout whose inline
//! `PyList_GET_ITEM`/`PyList_SET_ITEM` access works through `ob_item` at
//! offset 24 — CPython's growth policy, the slice entry points, in-place
//! reverse and sort, the `PyList_*` C API, and the per-object lock of
//! spec section 8.6 guarding every mutation (the Coil tail at offset 40;
//! `tp_basicsize` includes it, as section 8.2 requires).

use core::ffi::c_int;

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, Py_TPFLAGS_LIST_SUBCLASS,
    Py_TPFLAGS_SEQUENCE, PyObject, PyTypeObject, PyVarObject,
};

use crate::object::exceptions;
use crate::object::header;
use crate::object::lock;
use crate::object::typeobject::{Py_LT, PyType_GenericAlloc, zero_type};
use crate::types::{Cmp, rich_compare};

/// `list`'s private extra flag (`_Py_TPFLAGS_MATCH_SELF`).
const PY_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

/// CPython 3.12's `PyListObject` with the Coil tail: the var-object
/// header, the item array pointer and over-allocation count of
/// CPython's struct, then the per-object lock (spec sections 8.2, 8.6).
#[repr(C)]
pub struct PyListObject {
    pub ob_base: PyVarObject,
    pub ob_item: *mut *mut PyObject,
    pub ob_allocated: isize,
    pub ob_lock: std::sync::atomic::AtomicU8,
    pub _tail_pad: [u8; 7],
}

/// The offset of the per-object lock within a list.
pub const LOCK: usize = 40;

/// `PyList_Check` by the fast subclass flag.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_list(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: the flag read is within the type object.
    !ty.is_null() && unsafe { (*ty).tp_flags } & Py_TPFLAGS_LIST_SUBCLASS != 0
}

/// The list's lock byte.
///
/// # Safety
///
/// `op` must be a list object.
unsafe fn lock_of(op: *mut PyObject) -> &'static std::sync::atomic::AtomicU8 {
    // SAFETY: the lock is a fixed field of the list layout.
    unsafe { &*((op as *const u8).add(LOCK) as *const std::sync::atomic::AtomicU8) }
}

/// The lock guard every mutating entry point holds.
///
/// # Safety
///
/// `op` must be a list object.
unsafe fn locked(op: *mut PyObject) -> lock::Guard {
    // SAFETY: the lock field is within the list.
    unsafe { lock::Guard::new(lock_of(op)) }
}

/// CPython's `list_resize` (the caller holds the lock): the growth
/// pattern 0, 4, 8, 16, 24, 32, 40, 52, … with the shrink-by-half rule.
/// False on failure with `MemoryError` set.
unsafe fn list_resize(self_: *mut PyListObject, newsize: isize) -> bool {
    let allocated = unsafe { (*self_).ob_allocated };
    if allocated >= newsize && newsize >= (allocated >> 1) {
        unsafe { (*self_).ob_base.ob_size = newsize };
        return true;
    }
    let mut new_allocated = ((newsize as usize) + (newsize as usize >> 3) + 6) & !3usize;
    if (newsize - unsafe { (*self_).ob_base.ob_size }) as usize > new_allocated - newsize as usize {
        new_allocated = (newsize as usize + 3) & !3usize;
    }
    if newsize == 0 {
        new_allocated = 0;
    }
    let Some(bytes) = new_allocated.checked_mul(core::mem::size_of::<*mut PyObject>()) else {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return false;
    };
    // SAFETY: the item array is a plain-allocator block, if any.
    // SAFETY: the item array belongs to this list alone here.
    let items = unsafe { crate::alloc::pymem::plain_realloc((*self_).ob_item.cast(), bytes) };
    if items.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return false;
    }
    unsafe {
        (*self_).ob_item = items.cast();
        (*self_).ob_base.ob_size = newsize;
        (*self_).ob_allocated = new_allocated as isize;
    }
    true
}

/// `app1`: append under the lock (listobject.c).
fn app1(self_: *mut PyListObject, v: *mut PyObject) -> c_int {
    if v.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `self_` is a list; the guard covers the size read, resize
    // and store.
    unsafe {
        let _guard = locked(self_.cast());
        let n = (*self_).ob_base.ob_size;
        if !list_resize(self_, n + 1) {
            return -1;
        }
        *(*self_).ob_item.add(n as usize) = v;
    }
    0
}

/// `ins1`: insert under the lock (listobject.c).
fn ins1(self_: *mut PyListObject, where_: isize, v: *mut PyObject) -> c_int {
    if v.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: `self_` is a list; the guard covers the size read, resize
    // and shift.
    unsafe {
        let _guard = locked(self_.cast());
        let n = (*self_).ob_base.ob_size;
        if !list_resize(self_, n + 1) {
            return -1;
        }
        let mut where_ = where_;
        if where_ < 0 {
            where_ += n;
            if where_ < 0 {
                where_ = 0;
            }
        }
        if where_ > n {
            where_ = n;
        }
        let items = (*self_).ob_item;
        let mut i = n;
        while {
            i -= 1;
            i >= where_
        } {
            *items.add(i as usize + 1) = *items.add(i as usize);
        }
        *items.add(where_ as usize) = v;
    }
    0
}

unsafe extern "C" fn list_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe {
        let items = (*object.cast::<PyListObject>()).ob_item;
        crate::alloc::pymem::plain_free(items.cast());
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn list_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> c_int {
    let list = object.cast::<PyListObject>();
    // SAFETY: the size read and item reads are within the list's data.
    unsafe {
        let size = (*list).ob_base.ob_size;
        for i in 0..size.max(0) as usize {
            let item = *(*list).ob_item.add(i);
            if !item.is_null() && visit(item, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

const fn list_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"list".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyListObject>() as isize;
    t.tp_dealloc = Some(list_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_HAVE_GC
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_LIST_SUBCLASS
        | Py_TPFLAGS_SEQUENCE
        | PY_TPFLAGS_MATCH_SELF;
    t.tp_traverse = Some(list_traverse);
    t.tp_iter = Some(list_iter);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `list` type object (CPython's `PyList_Type`).
#[unsafe(no_mangle)]
pub static mut PyList_Type: PyTypeObject = list_type();

// ------------------------------------------------------------- C API entry

/// See the CPython 3.12 contract for this entry point: a list with NULL
/// items the caller must fill before exposing it.
#[unsafe(no_mangle)]
pub extern "C" fn PyList_New(size: isize) -> *mut PyObject {
    if size < 0 {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `PyList_Type` is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(&raw mut PyList_Type, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let list = object.cast::<PyListObject>();
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*list).ob_base.ob_size = size;
        (*list).ob_allocated = size;
        if size > 0 {
            let items = crate::alloc::pymem::plain_calloc(
                size as usize,
                core::mem::size_of::<*mut PyObject>(),
            );
            if items.is_null() {
                crate::tstate::set_string(exceptions::memory_error(), b"");
                crate::alloc::free_block(object.cast());
                return core::ptr::null_mut();
            }
            (*list).ob_item = items.cast();
        }
    }
    object
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Size(op: *mut PyObject) -> isize {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the var-object header is the first 24 bytes.
    unsafe { (*op.cast::<PyListObject>()).ob_base.ob_size }
}

/// See the CPython 3.12 contract for this entry point: a borrowed item.
///
/// # Safety
///
/// `op` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetItem(op: *mut PyObject, i: isize) -> *mut PyObject {
    if !(unsafe { is_list(op) }) || i < 0 {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    let list = op.cast::<PyListObject>();
    // SAFETY: the size read is within the list.
    if i >= unsafe { (*list).ob_base.ob_size } {
        crate::tstate::set_string(exceptions::index_error(), b"list index out of range");
        return core::ptr::null_mut();
    }
    // SAFETY: the item read is within the item array.
    unsafe { *(*list).ob_item.add(i as usize) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a list object; `newitem` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetItem(
    op: *mut PyObject,
    i: isize,
    newitem: *mut PyObject,
) -> c_int {
    if !(unsafe { is_list(op) }) || i < 0 || newitem.is_null() {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    let list = op.cast::<PyListObject>();
    // SAFETY: the guard covers the size read and item store.
    unsafe {
        let _guard = locked(op);
        if i >= (*list).ob_base.ob_size {
            crate::tstate::set_string(
                exceptions::index_error(),
                b"list assignment index out of range",
            );
            return -1;
        }
        *(*list).ob_item.add(i as usize) = newitem;
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a list object; `newitem` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Insert(
    op: *mut PyObject,
    where_: isize,
    newitem: *mut PyObject,
) -> c_int {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    ins1(op.cast::<PyListObject>(), where_, newitem)
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a list object; `newitem` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Append(op: *mut PyObject, newitem: *mut PyObject) -> c_int {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    app1(op.cast::<PyListObject>(), newitem)
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `a` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetSlice(
    a: *mut PyObject,
    ilow: isize,
    ihigh: isize,
) -> *mut PyObject {
    if !(unsafe { is_list(a) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    let list = a.cast::<PyListObject>();
    // SAFETY: the size read is within the list.
    let n = unsafe { (*list).ob_base.ob_size };
    let ilow = ilow.clamp(0, n);
    let ihigh = ihigh.clamp(ilow, n);
    // SAFETY: the new list's items are written below its exposure.
    let result = PyList_New(ihigh - ilow);
    if result.is_null() {
        return core::ptr::null_mut();
    }
    if ihigh > ilow {
        // SAFETY: the item-array pointer read is within the new list.
        let dst = unsafe { (*result.cast::<PyListObject>()).ob_item };
        // SAFETY: the source slice is within the item array; the
        // destination was allocated for exactly that many slots.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (*list).ob_item.add(ilow as usize),
                dst,
                (ihigh - ilow) as usize,
            );
        }
    }
    result
}

/// See the CPython 3.12 contract for this entry point: `v` null deletes
/// the slice; otherwise the slice is replaced by `v`'s items.
///
/// # Safety
///
/// `a` must be a list object; `v` must be null or an iterable object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetSlice(
    a: *mut PyObject,
    ilow: isize,
    ihigh: isize,
    v: *mut PyObject,
) -> c_int {
    if !(unsafe { is_list(a) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    let b: Vec<*mut PyObject> = if v.is_null() {
        Vec::new()
    } else {
        // SAFETY: `v` is a valid object of a readied type.
        match unsafe { collect_iterable(v) } {
            Ok(items) => items,
            Err(()) => return -1,
        }
    };
    let list = a.cast::<PyListObject>();
    // SAFETY: the guard covers the resize and the item moves below.
    unsafe {
        let _guard = locked(a);
        let n = (*list).ob_base.ob_size;
        let ilow = ilow.clamp(0, n);
        let ihigh = ihigh.clamp(ilow, n);
        let norig = ihigh - ilow;
        if !list_resize(list, n - norig + b.len() as isize) {
            return -1;
        }
        let items = (*list).ob_item;
        // Shift the tail past the slice to its new position.
        if n - ihigh > 0 {
            core::ptr::copy(
                items.add(ihigh as usize),
                items.add((ilow + b.len() as isize) as usize),
                (n - ihigh) as usize,
            );
        }
        // Fill the slice.
        for (i, item) in b.iter().enumerate() {
            *items.add(ilow as usize + i) = *item;
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_AsTuple(op: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    let list = op.cast::<PyListObject>();
    // SAFETY: the size read and slice are within the list's data.
    unsafe {
        let size = (*list).ob_base.ob_size;
        let items = core::slice::from_raw_parts((*list).ob_item, size.max(0) as usize);
        crate::types::tuple::from_array(items)
    }
}

/// See the CPython 3.12 contract for this entry point: in-place reverse.
///
/// # Safety
///
/// `op` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Reverse(op: *mut PyObject) -> c_int {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    let list = op.cast::<PyListObject>();
    // SAFETY: the guard covers the item swaps.
    unsafe {
        let _guard = locked(op);
        let size = (*list).ob_base.ob_size;
        let items = (*list).ob_item;
        for i in 0..size / 2 {
            core::ptr::swap(items.add(i as usize), items.add((size - i - 1) as usize));
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point: in-place stable
/// sort in ascending order (`listobject.c`'s `listsort_impl` semantics:
/// `<` through the rich-comparison slots, `TypeError` when unsupported).
///
/// # Safety
///
/// `op` must be a list object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Sort(op: *mut PyObject) -> c_int {
    if !(unsafe { is_list(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    let list = op.cast::<PyListObject>();
    // SAFETY: the guard covers the item reads and writes.
    unsafe {
        let _guard = locked(op);
        let size = (*list).ob_base.ob_size;
        if size < 2 {
            return 0;
        }
        let items = core::slice::from_raw_parts_mut((*list).ob_item, size as usize);
        // Bottom-up merge sort with `a < b` runs: stable, `n log n`.
        let mut buf: Vec<*mut PyObject> = Vec::with_capacity(items.len());
        let mut width = 1;
        while width < items.len() {
            let mut start = 0;
            while start < items.len() {
                let mid = (start + width).min(items.len());
                let end = (start + 2 * width).min(items.len());
                if merge_runs(&mut items[start..end], mid - start, &mut buf).is_err() {
                    return -1;
                }
                start = end;
            }
            width *= 2;
        }
    }
    0
}

/// Whether `a < b` for the sort; `Err` when `<` is unsupported or raised.
unsafe fn op_lt(a: *mut PyObject, b: *mut PyObject) -> Result<bool, ()> {
    // SAFETY: both are valid objects.
    match unsafe { rich_compare(a, b, Py_LT) } {
        Cmp::Bool(value) => Ok(value),
        Cmp::NotImplemented => {
            // SAFETY: header reads within the objects.
            unsafe {
                let an = crate::util::cstr((*header::type_of(a)).tp_name);
                let bn = crate::util::cstr((*header::type_of(b)).tp_name);
                crate::tstate::set_string(
                    exceptions::type_error(),
                    format!("'<' not supported between instances of '{an}' and '{bn}'").as_bytes(),
                );
            }
            Err(())
        }
        Cmp::Error => Err(()),
    }
}

/// Stable in-place merge of `slice`, whose first run has `mid_left`
/// elements, through `buf`; comparisons go through `op_lt`.
unsafe fn merge_runs(
    slice: &mut [*mut PyObject],
    mid_left: usize,
    buf: &mut Vec<*mut PyObject>,
) -> Result<(), ()> {
    buf.clear();
    buf.extend_from_slice(&slice[..mid_left]);
    let mut left = 0;
    let mut right = mid_left;
    let mut out = 0;
    // SAFETY: every indexed access is within `slice`; `buf` holds the
    // saved first run.
    unsafe {
        while left < buf.len() && right < slice.len() {
            // Taking from the left run on ties keeps the sort stable.
            if op_lt(buf[left], slice[right])? {
                slice[out] = buf[left];
                left += 1;
            } else {
                slice[out] = slice[right];
                right += 1;
            }
            out += 1;
        }
        while left < buf.len() {
            slice[out] = buf[left];
            left += 1;
            out += 1;
        }
    }
    Ok(())
}

/// Collect an arbitrary iterable through its `tp_iter`/`tp_iternext`
/// slots (`PySequence_Fast`'s role for this runtime's container types).
/// Err with the exception set on failure.
///
/// # Safety
///
/// `v` must be a valid object of a readied type.
pub(crate) unsafe fn collect_iterable(v: *mut PyObject) -> Result<Vec<*mut PyObject>, ()> {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(v) };
    // SAFETY: the slot read is within the type object.
    let Some(iter) = (unsafe { (*ty).tp_iter }) else {
        let name = unsafe { crate::util::cstr((*ty).tp_name) };
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not iterable").as_bytes(),
        );
        return Err(());
    };
    // SAFETY: the slot takes the object.
    let it = unsafe { iter(v) };
    if it.is_null() {
        return Err(());
    }
    let mut items = Vec::new();
    // SAFETY: the slot read is within the iterator's type.
    let next = unsafe { (*header::type_of(it)).tp_iternext };
    if let Some(next) = next {
        loop {
            // SAFETY: the slot takes the iterator.
            let item = unsafe { next(it) };
            if item.is_null() {
                if !crate::tstate::occurred().is_null() {
                    return Err(());
                }
                break;
            }
            items.push(item);
        }
    }
    Ok(items)
}

// ------------------------------------------------------------- iteration

/// CPython's `_PyListIterObject` (a free-layout object).
#[repr(C)]
struct PyListIterObject {
    ob_base: PyObject,
    it_index: isize,
    it_seq: *mut PyObject,
}

unsafe extern "C" fn list_iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn list_iter_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    let it = object.cast::<PyListIterObject>();
    // SAFETY: the field read is within the iterator's allocation.
    unsafe {
        let seq = (*it).it_seq;
        if !seq.is_null() && visit(seq, arg) != 0 {
            return -1;
        }
    }
    0
}

unsafe extern "C" fn list_iter_iternext(object: *mut PyObject) -> *mut PyObject {
    let it = object.cast::<PyListIterObject>();
    // SAFETY: the iterator fields are within the allocation.
    unsafe {
        let seq = (*it).it_seq;
        if seq.is_null() {
            return core::ptr::null_mut();
        }
        let size = (*seq.cast::<PyListObject>()).ob_base.ob_size;
        if (*it).it_index < size {
            let item = *(*seq.cast::<PyListObject>())
                .ob_item
                .add((*it).it_index as usize);
            (*it).it_index += 1;
            return item;
        }
        // Unlike the dict iterators, a list iterator simply stops when it
        // runs off the end, whatever the list did meanwhile.
        (*it).it_seq = core::ptr::null_mut();
        core::ptr::null_mut()
    }
}

const fn list_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"list_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyListIterObject>() as isize;
    t.tp_dealloc = Some(list_iter_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(list_iter_traverse);
    t.tp_iter = Some(crate::types::tuple::self_iter);
    t.tp_iternext = Some(list_iter_iternext);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `list_iterator` type (CPython's `PyListIter_Type`).
pub(crate) static mut LIST_ITER_TYPE: PyTypeObject = list_iter_type();

/// The `list_iterator` type object, for init.
pub(crate) fn list_iter_type_ptr() -> *mut PyTypeObject {
    &raw mut LIST_ITER_TYPE
}

/// `list_iter` (`list.__iter__`).
unsafe extern "C" fn list_iter(seq: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_list(seq) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `LIST_ITER_TYPE` is static; init readies it.
    let it = unsafe { PyType_GenericAlloc(&raw mut LIST_ITER_TYPE, 0) }.cast::<PyListIterObject>();
    if it.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*it).it_index = 0;
        (*it).it_seq = seq;
    }
    it.cast()
}

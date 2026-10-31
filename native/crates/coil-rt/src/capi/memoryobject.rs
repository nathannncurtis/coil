//! `memoryobject.h`: the `memoryview` type, the managed buffer it chains
//! through, and the `PyMemoryView_*` constructors (spec section 10.1). The
//! layouts are CPython's, byte for byte: a memoryview owns a private copy
//! of the exporter's view whose shape, strides and suboffsets live in the
//! variable-size `ob_array` tail.
//!
//! CPython-level methods (`cast`, `tobytes`, indexing, iteration) are not
//! part of this surface and are not installed.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, Py_buffer, PyBUF_ANY_CONTIGUOUS, PyBUF_C_CONTIGUOUS,
    PyBUF_F_CONTIGUOUS, PyBUF_FORMAT, PyBUF_FULL_RO, PyBUF_INDIRECT, PyBUF_MAX_NDIM, PyBUF_ND,
    PyBUF_STRIDES, PyBUF_WRITABLE, PyBUF_WRITE, PyBufferProcs, PyObject, PyTypeObject, PyVarObject,
    visitproc,
};

use crate::object::header;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};
use crate::tstate;
use crate::types::exc as exceptions;
use crate::util;

const MANAGED_BUFFER_RELEASED: isize = 0x001;
const MANAGED_BUFFER_FREE_FORMAT: isize = 0x002;

const MEMORYVIEW_RELEASED: isize = 0x001;
const MEMORYVIEW_C: isize = 0x002;
const MEMORYVIEW_FORTRAN: isize = 0x004;
const MEMORYVIEW_SCALAR: isize = 0x008;
const MEMORYVIEW_PIL: isize = 0x010;
const MEMORYVIEW_RESTRICTED: isize = 0x020;

/// CPython's `_PyManagedBufferObject`: one shared snapshot of an
/// exporter's buffer every chained memoryview reads through.
#[repr(C)]
struct PyManagedBufferObject {
    ob_base: PyObject,
    flags: isize,
    exports: isize,
    master: Py_buffer,
}

/// CPython's `BASE_INACCESSIBLE`: the view or its managed buffer has been
/// released.
unsafe fn base_inaccessible(mv: *mut PyObject) -> bool {
    // SAFETY: the object is a memoryview this runtime allocated.
    unsafe {
        let mv = &*mv.cast::<PyMemoryViewObject>();
        mv.flags & MEMORYVIEW_RELEASED != 0
            || (*mv.mbuf.cast::<PyManagedBufferObject>()).flags & MANAGED_BUFFER_RELEASED != 0
    }
}

unsafe extern "C" fn mbuf_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the field read is within the managed buffer.
    let referent = unsafe { (*object.cast::<PyManagedBufferObject>()).master.obj };
    if !referent.is_null() {
        // SAFETY: the visitor contract.
        return unsafe { visit(referent, arg) };
    }
    0
}

/// CPython's `mbuf_release`: give the master buffer back to the exporter,
/// once, however many views were chained to it.
unsafe fn mbuf_release(self_: *mut PyObject) {
    // SAFETY: the object is a managed buffer this runtime allocated.
    unsafe {
        let mbuf = &mut *self_.cast::<PyManagedBufferObject>();
        if mbuf.flags & MANAGED_BUFFER_RELEASED != 0 {
            return;
        }
        mbuf.flags |= MANAGED_BUFFER_RELEASED;
        // `PyBuffer_Release` drops the reference the master view's `obj`
        // carries and nulls the field.
        crate::types::bytes::PyBuffer_Release(&raw mut mbuf.master);
    }
}

unsafe extern "C" fn mbuf_dealloc(object: *mut PyObject) {
    // SAFETY: the object is a managed buffer this runtime allocated.
    unsafe {
        mbuf_release(object);
        let mbuf = &mut *object.cast::<PyManagedBufferObject>();
        if mbuf.flags & MANAGED_BUFFER_FREE_FORMAT != 0 {
            // SAFETY: the format buffer came from `PyMem_Malloc`.
            crate::alloc::pymem::PyMem_Free(mbuf.master.format.cast());
        }
        crate::alloc::free_block(object.cast());
    }
}

const fn managed_buffer_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"managedbuffer".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyManagedBufferObject>() as isize;
    t.tp_dealloc = Some(mbuf_dealloc);
    t.tp_traverse = Some(mbuf_traverse);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The managed-buffer type (CPython's `_PyManagedBuffer_Type`, private).
static mut MANAGED_BUFFER_TYPE: PyTypeObject = managed_buffer_type();

/// CPython's `PyMemoryViewObject`.
#[repr(C)]
struct PyMemoryViewObject {
    ob_base: PyVarObject,
    mbuf: *mut PyObject,
    hash: isize,
    flags: isize,
    exports: isize,
    view: Py_buffer,
    weakreflist: *mut PyObject,
    ob_array: [isize; 1],
}

/// The byte offset of the memoryview's variable-size tail.
const OB_ARRAY: usize = core::mem::size_of::<PyMemoryViewObject>() - core::mem::size_of::<isize>();
/// The byte offset of the weak-reference list field.
const WEAKREFLIST: usize = OB_ARRAY - core::mem::size_of::<*mut PyObject>();

/// `PyMemoryView_Check`: memoryviews are final in CPython, so exact.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub unsafe fn is_memoryview(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    core::ptr::eq(unsafe { header::type_of(op) }, &raw mut PyMemoryView_Type)
}

unsafe extern "C" fn memory_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the field read is within the memoryview.
    let referent = unsafe { (*object.cast::<PyMemoryViewObject>()).mbuf };
    if !referent.is_null() {
        // SAFETY: the visitor contract.
        return unsafe { visit(referent, arg) };
    }
    0
}

unsafe extern "C" fn memory_clear(object: *mut PyObject) -> c_int {
    // SAFETY: the object is a memoryview this runtime allocated; a view
    // with live exports is left whole, as CPython's `memory_clear` does.
    unsafe {
        let mv = &mut *object.cast::<PyMemoryViewObject>();
        if mv.exports == 0 {
            _memory_release(object);
            let mbuf = core::mem::replace(&mut mv.mbuf, core::ptr::null_mut());
            crate::types::exc::release_object(mbuf);
        }
    }
    0
}

/// CPython's `_memory_release`: drop this view's registration with the
/// managed buffer, releasing the exporter's buffer with the last one.
unsafe fn _memory_release(object: *mut PyObject) {
    // SAFETY: the object is a memoryview this runtime allocated.
    unsafe {
        let mv = &mut *object.cast::<PyMemoryViewObject>();
        if mv.flags & MEMORYVIEW_RELEASED != 0 {
            return;
        }
        mv.flags |= MEMORYVIEW_RELEASED;
        let mbuf = mv.mbuf;
        if !mbuf.is_null() {
            let buffer = &mut *mbuf.cast::<PyManagedBufferObject>();
            buffer.exports -= 1;
            if buffer.exports == 0 {
                mbuf_release(mbuf);
            }
        }
    }
}

unsafe extern "C" fn memory_dealloc(object: *mut PyObject) {
    // SAFETY: the object is a memoryview this runtime allocated.
    unsafe {
        debug_assert!((*object.cast::<PyMemoryViewObject>()).exports == 0);
        _memory_release(object);
        let mv = &mut *object.cast::<PyMemoryViewObject>();
        let mbuf = core::mem::replace(&mut mv.mbuf, core::ptr::null_mut());
        crate::types::exc::release_object(mbuf);
        if !mv.weakreflist.is_null() {
            crate::capi::object::PyObject_ClearWeakRefs(object);
        }
        crate::alloc::free_block(object.cast());
    }
}

unsafe extern "C" fn memory_repr(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field read is within the memoryview.
    let flags = unsafe { (*object.cast::<PyMemoryViewObject>()).flags };
    let text = if flags & MEMORYVIEW_RELEASED != 0 {
        format!("<released memory at {object:p}>")
    } else {
        format!("<memory at {object:p}>")
    };
    crate::types::unicode::str_from_utf8(&text)
}

/// CPython's `get_native_fmtchar`: the native size of a single-character
/// (optionally `@`-prefixed) format, or `None`.
pub(crate) fn native_format_size(fmt: *const c_char) -> Option<isize> {
    if fmt.is_null() {
        return None;
    }
    // SAFETY: the format is a NUL-terminated string by contract.
    let fmt = unsafe { util::cstr(fmt) };
    let fmt = fmt.strip_prefix('@').unwrap_or(fmt);
    if fmt.len() != 1 {
        return None;
    }
    // The x64 CPython sizes: `long` is 4 bytes under MSVC and 8 elsewhere,
    // the remaining codes agree on both target ABIs.
    Some(match fmt.as_bytes()[0] {
        b'c' | b'b' | b'B' | b'?' => 1,
        b'h' | b'H' => 2,
        b'i' | b'I' => 4,
        b'l' | b'L' => core::mem::size_of::<core::ffi::c_long>() as isize,
        b'q' | b'Q' | b'n' | b'N' | b'P' => 8,
        b'e' => 2,
        b'f' => 4,
        b'd' => 8,
        _ => return None,
    })
}

/// CPython's `IS_BYTE_FORMAT` over the format's first character.
fn is_byte_format(fmt: *const c_char) -> bool {
    if fmt.is_null() {
        return false;
    }
    // SAFETY: the format is a NUL-terminated string by contract.
    let fmt = unsafe { util::cstr(fmt) };
    let fmt = fmt.strip_prefix('@').unwrap_or(fmt);
    fmt == "b" || fmt == "B" || fmt == "c"
}

unsafe extern "C" fn memory_hash(object: *mut PyObject) -> isize {
    // SAFETY: the object is a memoryview this runtime allocated; the body
    // follows CPython's `memory_hash` step for step.
    unsafe {
        let mv = &mut *object.cast::<PyMemoryViewObject>();
        if mv.hash != -1 {
            return mv.hash;
        }
        let view = &mv.view;
        if base_inaccessible(object) {
            tstate::set_string(
                exceptions::value_error(),
                b"operation forbidden on released memoryview object",
            );
            return -1;
        }
        if view.readonly == 0 {
            tstate::set_string(
                exceptions::value_error(),
                b"cannot hash writable memoryview object",
            );
            return -1;
        }
        if !is_byte_format(view.format) {
            tstate::set_string(
                exceptions::value_error(),
                b"memoryview: hashing is restricted to formats 'B', 'b' or 'c'",
            );
            return -1;
        }
        if !view.obj.is_null() && crate::capi::object::PyObject_Hash(view.obj) == -1 {
            return -1;
        }
        // A contiguous view hashes from its own buffer; anything else is
        // linearised first, exactly as CPython does.
        if mv.flags & (MEMORYVIEW_SCALAR | MEMORYVIEW_C) == 0 {
            // SAFETY: the length is the view's own.
            let mem = crate::alloc::pymem::PyMem_Malloc(view.len.max(0) as usize) as *mut u8;
            if mem.is_null() {
                tstate::set_string(exceptions::memory_error(), b"");
                return -1;
            }
            // SAFETY: the slice covers exactly the allocation.
            let slice = core::slice::from_raw_parts_mut(mem, view.len.max(0) as usize);
            let ok = buffer_to_contiguous(slice, view, b'C').is_ok();
            let hash = if ok {
                crate::types::bytes::_Py_HashBytes(mem.cast(), view.len)
            } else {
                -1
            };
            crate::alloc::pymem::PyMem_Free(mem.cast());
            mv.hash = hash;
        } else {
            mv.hash = crate::types::bytes::_Py_HashBytes(view.buf, view.len);
        }
        mv.hash
    }
}

/// The memoryview's own `bf_getbuffer`: re-export the private view under
/// the requester's flags (CPython's `memory_getbuf`).
unsafe extern "C" fn memory_getbuf(
    self_: *mut PyObject,
    view: *mut Py_buffer,
    flags: c_int,
) -> c_int {
    // SAFETY: the object is a memoryview this runtime allocated.
    unsafe {
        let mv = &mut *self_.cast::<PyMemoryViewObject>();
        let base = &mv.view;
        let baseflags = mv.flags;

        if base_inaccessible(self_) {
            tstate::set_string(
                exceptions::value_error(),
                b"operation forbidden on released memoryview object",
            );
            return -1;
        }
        if baseflags & MEMORYVIEW_RESTRICTED != 0 {
            tstate::set_string(
                exceptions::value_error(),
                b"cannot create new view on restricted memoryview",
            );
            return -1;
        }

        *view = core::ptr::read(base);
        (*view).obj = core::ptr::null_mut();

        if flags & PyBUF_WRITABLE != 0 && base.readonly != 0 {
            tstate::set_string(
                exceptions::buffer_error(),
                b"memoryview: underlying buffer is not writable",
            );
            return -1;
        }
        if flags & PyBUF_FORMAT == 0 {
            (*view).format = core::ptr::null_mut();
        }
        if flags & PyBUF_C_CONTIGUOUS == PyBUF_C_CONTIGUOUS
            && baseflags & (MEMORYVIEW_SCALAR | MEMORYVIEW_C) == 0
        {
            tstate::set_string(
                exceptions::buffer_error(),
                b"memoryview: underlying buffer is not C-contiguous",
            );
            return -1;
        }
        if flags & PyBUF_F_CONTIGUOUS == PyBUF_F_CONTIGUOUS
            && baseflags & (MEMORYVIEW_SCALAR | MEMORYVIEW_FORTRAN) == 0
        {
            tstate::set_string(
                exceptions::buffer_error(),
                b"memoryview: underlying buffer is not Fortran contiguous",
            );
            return -1;
        }
        if flags & PyBUF_ANY_CONTIGUOUS == PyBUF_ANY_CONTIGUOUS
            && baseflags & (MEMORYVIEW_SCALAR | MEMORYVIEW_C | MEMORYVIEW_FORTRAN) == 0
        {
            tstate::set_string(
                exceptions::buffer_error(),
                b"memoryview: underlying buffer is not contiguous",
            );
            return -1;
        }
        if flags & PyBUF_INDIRECT != PyBUF_INDIRECT && baseflags & MEMORYVIEW_PIL != 0 {
            tstate::set_string(
                exceptions::buffer_error(),
                b"memoryview: underlying buffer requires suboffsets",
            );
            return -1;
        }
        if flags & PyBUF_STRIDES != PyBUF_STRIDES {
            if baseflags & (MEMORYVIEW_SCALAR | MEMORYVIEW_C) == 0 {
                tstate::set_string(
                    exceptions::buffer_error(),
                    b"memoryview: underlying buffer is not C-contiguous",
                );
                return -1;
            }
            (*view).strides = core::ptr::null_mut();
        }
        if flags & PyBUF_ND != PyBUF_ND {
            if !(*view).format.is_null() {
                tstate::set_string(
                    exceptions::buffer_error(),
                    b"memoryview: cannot cast to unsigned bytes if the format flag is present",
                );
                return -1;
            }
            (*view).ndim = 1;
            (*view).shape = core::ptr::null_mut();
        }

        crate::types::exc::retain_object(self_);
        (*view).obj = self_;
        mv.exports += 1;
        0
    }
}

unsafe extern "C" fn memory_releasebuf(self_: *mut PyObject, _view: *mut Py_buffer) {
    // SAFETY: the object is a memoryview with a live export.
    unsafe { (*self_.cast::<PyMemoryViewObject>()).exports -= 1 };
}

static mut MEMORY_AS_BUFFER: PyBufferProcs = PyBufferProcs {
    bf_getbuffer: Some(memory_getbuf),
    bf_releasebuffer: Some(memory_releasebuf),
};

const fn memoryview_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"memoryview".as_ptr();
    t.tp_basicsize = OB_ARRAY as isize;
    t.tp_itemsize = core::mem::size_of::<isize>() as isize;
    t.tp_dealloc = Some(memory_dealloc);
    t.tp_repr = Some(memory_repr);
    t.tp_hash = Some(memory_hash);
    t.tp_getattro = Some(crate::capi::object::PyObject_GenericGetAttr);
    t.tp_as_buffer = &raw mut MEMORY_AS_BUFFER;
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(memory_traverse);
    t.tp_clear = Some(memory_clear);
    t.tp_weaklistoffset = WEAKREFLIST as isize;
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `memoryview` type object (CPython's `PyMemoryView_Type`).
#[unsafe(no_mangle)]
pub static mut PyMemoryView_Type: PyTypeObject = memoryview_type();

// ------------------------------------------------------ buffer copy helpers

/// `copy_base`'s overlap rule: two ranges that cannot overlap copy with
/// `memcpy`, anything else with `memmove`.
unsafe fn copy_bytes(dest: *mut u8, src: *const u8, size: usize) {
    // SAFETY: the caller guarantees `size` valid bytes at both pointers.
    unsafe {
        if size == 0 {
            return;
        }
        let (dl, sl) = (dest as usize, src as usize);
        if dl.wrapping_add(size) < sl || sl.wrapping_add(size) < dl {
            core::ptr::copy_nonoverlapping(src, dest, size);
        } else {
            core::ptr::copy(src, dest, size);
        }
    }
}

/// CPython's `ADJUST_PTR`: suboffset dereference for indirect layouts.
///
/// # Safety
///
/// `ptr` points into a live buffer whose dimension-`dim` suboffsets entry
/// is non-negative.
unsafe fn adjust_ptr(ptr: *mut u8, suboffsets: *const isize, dim: usize) -> *mut u8 {
    // SAFETY: the suboffsets array covers `dim` per the buffer contract.
    unsafe {
        if !suboffsets.is_null() && *suboffsets.add(dim) >= 0 {
            return (ptr as *mut *mut u8)
                .read()
                .add(*suboffsets.add(dim) as usize);
        }
        ptr
    }
}

/// CPython's `copy_rec`: copy `shape` element by element between two
/// buffers of identical logical structure.
#[allow(clippy::too_many_arguments)]
unsafe fn copy_rec(
    shape: &[isize],
    itemsize: isize,
    dptr: *mut u8,
    dstrides: *const isize,
    dsuboffsets: *const isize,
    sptr: *mut u8,
    sstrides: *const isize,
    ssuboffsets: *const isize,
    mem: *mut u8,
) {
    // SAFETY: the buffers have full information and identical structure.
    unsafe {
        if shape.len() == 1 {
            if mem.is_null() {
                copy_bytes(dptr, sptr, (shape[0] * itemsize) as usize);
            } else {
                let mut p = mem;
                let mut s = sptr;
                for _ in 0..shape[0] {
                    let xs = adjust_ptr(s, ssuboffsets, 0);
                    core::ptr::copy_nonoverlapping(xs, p, itemsize as usize);
                    p = p.add(itemsize as usize);
                    s = s.add(sstrides.read() as usize);
                }
                let mut p = mem;
                let mut d = dptr;
                for _ in 0..shape[0] {
                    let xd = adjust_ptr(d, dsuboffsets, 0);
                    core::ptr::copy_nonoverlapping(p, xd, itemsize as usize);
                    p = p.add(itemsize as usize);
                    d = d.add(dstrides.read() as usize);
                }
            }
            return;
        }
        let mut d = dptr;
        let mut s = sptr;
        for _ in 0..shape[0] {
            let xd = adjust_ptr(d, dsuboffsets, 0);
            let xs = adjust_ptr(s, ssuboffsets, 0);
            copy_rec(
                &shape[1..],
                itemsize,
                xd,
                dstrides.add(1),
                if dsuboffsets.is_null() {
                    core::ptr::null()
                } else {
                    dsuboffsets.add(1)
                },
                xs,
                sstrides.add(1),
                if ssuboffsets.is_null() {
                    core::ptr::null()
                } else {
                    ssuboffsets.add(1)
                },
                mem,
            );
            d = d.add(dstrides.read() as usize);
            s = s.add(sstrides.read() as usize);
        }
    }
}

/// No suboffsets in the last dimension of either buffer (CPython's
/// `HAVE_SUBOFFSETS_IN_LAST_DIM` over both buffers).
unsafe fn last_dim_is_contiguous(dest: &Py_buffer, src: &Py_buffer) -> bool {
    // SAFETY: both buffers have full information.
    unsafe {
        let d = dest.suboffsets.is_null() || *dest.suboffsets.add(dest.ndim as usize - 1) < 0;
        let s = src.suboffsets.is_null() || *src.suboffsets.add(src.ndim as usize - 1) < 0;
        d && s
            && *dest.strides.add(dest.ndim as usize - 1) == dest.itemsize
            && *src.strides.add(src.ndim as usize - 1) == src.itemsize
    }
}

/// CPython's `equiv_format`: formats match after a leading `@`, and the
/// item sizes agree.
fn equiv_format(dest: &Py_buffer, src: &Py_buffer) -> bool {
    let read = |fmt: *const c_char| -> Option<Vec<u8>> {
        if fmt.is_null() {
            return None;
        }
        // SAFETY: the format is a NUL-terminated string by contract.
        let mut fmt = unsafe { util::cstr(fmt) }.as_bytes().to_vec();
        if fmt.first() == Some(&b'@') {
            fmt.remove(0);
        }
        Some(fmt)
    };
    match (read(dest.format), read(src.format)) {
        (Some(d), Some(s)) => d == s && dest.itemsize == src.itemsize,
        _ => false,
    }
}

/// CPython's `equiv_shape`: equal, or identical up to a shared zero.
unsafe fn equiv_shape(dest: &Py_buffer, src: &Py_buffer) -> bool {
    // SAFETY: both buffers have full information.
    unsafe {
        if dest.ndim != src.ndim {
            return false;
        }
        for i in 0..dest.ndim as usize {
            if *dest.shape.add(i) != *src.shape.add(i) {
                return false;
            }
            if *dest.shape.add(i) == 0 {
                break;
            }
        }
        true
    }
}

/// CPython's `copy_buffer`: copy `src` to `dest`, structures identical.
/// `Ok(())`, or `Err(())` with the structure error set.
unsafe fn copy_buffer(dest: &Py_buffer, src: &Py_buffer) -> Result<(), ()> {
    // SAFETY: the buffers have full information and identical structure.
    unsafe {
        if !equiv_format(dest, src) || !equiv_shape(dest, src) {
            tstate::set_string(
                exceptions::value_error(),
                b"memoryview assignment: lvalue and rvalue have different structures",
            );
            return Err(());
        }
        let last = dest.ndim as usize - 1;
        let mut mem = core::ptr::null_mut();
        if !last_dim_is_contiguous(dest, src) {
            mem =
                crate::alloc::pymem::PyMem_Malloc((*dest.shape.add(last) * dest.itemsize) as usize)
                    as *mut u8;
            if mem.is_null() {
                tstate::set_string(exceptions::memory_error(), b"");
                return Err(());
            }
        }
        // SAFETY: the shape array covers the buffer's dimensions.
        let shape = core::slice::from_raw_parts(dest.shape, dest.ndim as usize);
        copy_rec(
            shape,
            dest.itemsize,
            dest.buf.cast(),
            dest.strides,
            dest.suboffsets,
            src.buf.cast(),
            src.strides,
            src.suboffsets,
            mem,
        );
        if !mem.is_null() {
            crate::alloc::pymem::PyMem_Free(mem.cast());
        }
        Ok(())
    }
}

/// CPython's `init_strides_from_shape`: strides for C order.
///
/// # Safety
///
/// The view has `ndim > 0` and a writable strides array of that length.
unsafe fn init_strides_from_shape(view: &Py_buffer) {
    // SAFETY: the strides array covers the dimensions.
    unsafe {
        let ndim = view.ndim as usize;
        *view.strides.add(ndim - 1) = view.itemsize;
        for i in (0..ndim - 1).rev() {
            *view.strides.add(i) = *view.strides.add(i + 1) * *view.shape.add(i + 1);
        }
    }
}

/// CPython's `init_fortran_strides_from_shape`: strides for Fortran order.
///
/// # Safety
///
/// The view has `ndim > 0` and a writable strides array of that length.
unsafe fn init_fortran_strides_from_shape(view: &Py_buffer) {
    // SAFETY: the strides array covers the dimensions.
    unsafe {
        let ndim = view.ndim as usize;
        *view.strides.add(0) = view.itemsize;
        for i in 1..ndim {
            *view.strides.add(i) = *view.strides.add(i - 1) * *view.shape.add(i - 1);
        }
    }
}

/// CPython's `buffer_to_contiguous`: linearise `src` into `mem` in the
/// requested order. `mem.len()` must equal `src.len`.
unsafe fn buffer_to_contiguous(mem: &mut [u8], src: &Py_buffer, order: u8) -> Result<(), ()> {
    // SAFETY: the caller guarantees `mem` covers `src.len` writable bytes.
    unsafe {
        // The destination strides get their own array: writing through
        // `src`'s would reorder the source mid-copy.
        let count = src.ndim.max(0) as usize;
        let strides = crate::alloc::pymem::PyMem_Malloc(count * core::mem::size_of::<isize>());
        if strides.is_null() {
            tstate::set_string(exceptions::memory_error(), b"");
            return Err(());
        }
        let mut dest = core::ptr::read(src);
        dest.buf = mem.as_mut_ptr().cast();
        dest.strides = strides.cast();
        if order == b'C' || order == b'A' {
            init_strides_from_shape(&dest);
        } else {
            init_fortran_strides_from_shape(&dest);
        }
        dest.suboffsets = core::ptr::null_mut();
        let result = copy_buffer(&dest, src);
        crate::alloc::pymem::PyMem_Free(strides);
        result
    }
}

// ---------------------------------------------------------- the constructors

/// CPython's `mbuf_alloc`: a fresh, unregistered managed buffer.
unsafe fn mbuf_alloc() -> *mut PyObject {
    // SAFETY: the static type is readied by `init`.
    let object = unsafe { PyType_GenericAlloc(&raw mut MANAGED_BUFFER_TYPE, 0) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block is zeroed and exclusively owned.
    unsafe {
        let mbuf = &mut *object.cast::<PyManagedBufferObject>();
        mbuf.flags = 0;
        mbuf.exports = 0;
        mbuf.master.obj = core::ptr::null_mut();
    }
    object
}

/// CPython's `_PyManagedBuffer_FromObject`: a managed buffer holding the
/// buffer `base` exports under `flags`.
unsafe fn managed_buffer_from_object(base: *mut PyObject, flags: c_int) -> *mut PyObject {
    // SAFETY: `base` is a valid object per the contract.
    unsafe {
        let mbuf = mbuf_alloc();
        if mbuf.is_null() {
            return core::ptr::null_mut();
        }
        let master = &raw mut (*mbuf.cast::<PyManagedBufferObject>()).master;
        if crate::types::bytes::PyObject_GetBuffer(base, master, flags) < 0 {
            (*master).obj = core::ptr::null_mut();
            crate::types::exc::release_object(mbuf);
            return core::ptr::null_mut();
        }
        mbuf
    }
}

/// CPython's `memory_alloc`: a memoryview with `ndim`'s three arrays in
/// its tail, unregistered and uninitialised beyond the header fields.
unsafe fn memory_alloc(ndim: c_int) -> *mut PyObject {
    // SAFETY: the static type is readied by `init`.
    unsafe {
        let object = PyType_GenericAlloc(&raw mut PyMemoryView_Type, 3 * ndim as isize);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let mv = &mut *object.cast::<PyMemoryViewObject>();
        mv.ob_base.ob_size = 3 * ndim as isize;
        mv.mbuf = core::ptr::null_mut();
        mv.hash = -1;
        mv.flags = 0;
        mv.exports = 0;
        mv.view.ndim = ndim;
        let tail = (object as *mut u8).add(OB_ARRAY) as *mut isize;
        mv.view.shape = tail;
        mv.view.strides = tail.add(ndim as usize);
        mv.view.suboffsets = tail.add(2 * ndim as usize);
        mv.weakreflist = core::ptr::null_mut();
        object
    }
}

/// CPython's `init_shared_values`: the fields the view shares with its
/// managed buffer's master.
unsafe fn init_shared_values(dest: &mut Py_buffer, src: &Py_buffer) {
    dest.obj = src.obj;
    dest.buf = src.buf;
    dest.len = src.len;
    dest.itemsize = src.itemsize;
    dest.readonly = src.readonly;
    dest.format = if src.format.is_null() {
        c"B".as_ptr().cast_mut()
    } else {
        src.format
    };
    dest.internal = src.internal;
}

/// CPython's `init_shape_strides`: copy shape and strides, reconstructing
/// either when the source view was built without them.
unsafe fn init_shape_strides(dest: &mut Py_buffer, src: &Py_buffer) {
    // SAFETY: the arrays behind the pointers cover the dimensions.
    unsafe {
        if src.ndim == 0 {
            dest.shape = core::ptr::null_mut();
            dest.strides = core::ptr::null_mut();
            return;
        }
        if src.ndim == 1 {
            *dest.shape = if src.shape.is_null() {
                src.len / src.itemsize
            } else {
                *src.shape
            };
            *dest.strides = if src.strides.is_null() {
                src.itemsize
            } else {
                *src.strides
            };
            return;
        }
        for i in 0..src.ndim as usize {
            *dest.shape.add(i) = *src.shape.add(i);
        }
        if !src.strides.is_null() {
            for i in 0..src.ndim as usize {
                *dest.strides.add(i) = *src.strides.add(i);
            }
        } else {
            init_strides_from_shape(dest);
        }
    }
}

/// CPython's `init_suboffsets`.
unsafe fn init_suboffsets(dest: &mut Py_buffer, src: &Py_buffer) {
    // SAFETY: the arrays behind the pointers cover the dimensions.
    unsafe {
        if src.suboffsets.is_null() {
            dest.suboffsets = core::ptr::null_mut();
            return;
        }
        for i in 0..src.ndim as usize {
            *dest.suboffsets.add(i) = *src.suboffsets.add(i);
        }
    }
}

/// CPython's `init_flags`: the view's contiguity flags.
unsafe fn init_flags(mv: *mut PyObject) {
    // SAFETY: the object is a memoryview this runtime allocated.
    unsafe {
        let mv = &mut *mv.cast::<PyMemoryViewObject>();
        let view = &mv.view;
        let mut flags = 0;
        match view.ndim {
            0 => flags |= MEMORYVIEW_SCALAR | MEMORYVIEW_C | MEMORYVIEW_FORTRAN,
            1 => {
                if *view.shape == 1 || *view.strides == view.itemsize {
                    flags |= MEMORYVIEW_C | MEMORYVIEW_FORTRAN;
                }
            }
            _ => {
                if crate::capi::r#abstract::PyBuffer_IsContiguous(view, b'C' as c_char) != 0 {
                    flags |= MEMORYVIEW_C;
                }
                if crate::capi::r#abstract::PyBuffer_IsContiguous(view, b'F' as c_char) != 0 {
                    flags |= MEMORYVIEW_FORTRAN;
                }
            }
        }
        if !view.suboffsets.is_null() {
            flags |= MEMORYVIEW_PIL;
            flags &= !(MEMORYVIEW_C | MEMORYVIEW_FORTRAN);
        }
        mv.flags = flags;
    }
}

/// CPython's `mbuf_add_view`: a view of `src` (the master when null)
/// registered with `mbuf`, with complete buffer information.
unsafe fn mbuf_add_view(mbuf: *mut PyObject, src: *const Py_buffer) -> *mut PyObject {
    // SAFETY: `mbuf` is a live managed buffer and `src` its view or null.
    unsafe {
        let src = if src.is_null() {
            &raw const (*mbuf.cast::<PyManagedBufferObject>()).master
        } else {
            src
        };
        if (*src).ndim > PyBUF_MAX_NDIM {
            tstate::set_string(
                exceptions::value_error(),
                b"memoryview: number of dimensions must not exceed 64",
            );
            return core::ptr::null_mut();
        }
        let mv = memory_alloc((*src).ndim);
        if mv.is_null() {
            return core::ptr::null_mut();
        }
        let view = &raw mut (*mv.cast::<PyMemoryViewObject>()).view;
        init_shared_values(&mut *view, &*src);
        init_shape_strides(&mut *view, &*src);
        init_suboffsets(&mut *view, &*src);
        init_flags(mv);
        (*mv.cast::<PyMemoryViewObject>()).mbuf = mbuf;
        crate::types::exc::retain_object(mbuf);
        (*mbuf.cast::<PyManagedBufferObject>()).exports += 1;
        mv
    }
}

/// CPython's `mbuf_add_incomplete_view`: registered, shared values set,
/// shape and strides left to the caller.
unsafe fn mbuf_add_incomplete_view(mbuf: *mut PyObject, ndim: c_int) -> *mut PyObject {
    // SAFETY: `mbuf` is a live managed buffer.
    unsafe {
        let mv = memory_alloc(ndim);
        if mv.is_null() {
            return core::ptr::null_mut();
        }
        let view = &raw mut (*mv.cast::<PyMemoryViewObject>()).view;
        init_shared_values(&mut *view, &(*mbuf.cast::<PyManagedBufferObject>()).master);
        (*mv.cast::<PyMemoryViewObject>()).mbuf = mbuf;
        crate::types::exc::retain_object(mbuf);
        (*mbuf.cast::<PyManagedBufferObject>()).exports += 1;
        mv
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `mem` must hold `size` readable (readable and writable for
/// `PyBUF_WRITE`) bytes for the view's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromMemory(
    mem: *mut c_char,
    size: isize,
    flags: c_int,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        let mbuf = mbuf_alloc();
        if mbuf.is_null() {
            return core::ptr::null_mut();
        }
        let master = &raw mut (*mbuf.cast::<PyManagedBufferObject>()).master;
        let readonly = if flags == PyBUF_WRITE { 0 } else { 1 };
        // The return is CPython's `(void)`: `PyBUF_FULL_RO` cannot fail on
        // a null obj.
        crate::types::bytes::PyBuffer_FillInfo(
            master,
            core::ptr::null_mut(),
            mem.cast(),
            size,
            readonly,
            PyBUF_FULL_RO,
        );
        let mv = mbuf_add_view(mbuf, core::ptr::null());
        crate::types::exc::release_object(mbuf);
        mv
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `info` must point to a view struct whose buffer fields stay valid for
/// the memoryview's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromBuffer(info: *const Py_buffer) -> *mut PyObject {
    // SAFETY: the argument follows the C contract.
    unsafe {
        if (*info).buf.is_null() {
            tstate::set_string(
                exceptions::value_error(),
                b"PyMemoryView_FromBuffer(): info->buf must not be NULL",
            );
            return core::ptr::null_mut();
        }
        let mbuf = mbuf_alloc();
        if mbuf.is_null() {
            return core::ptr::null_mut();
        }
        // The caller's `info->obj` is a borrowed reference CPython never
        // releases: the master is a copy with a null `obj`.
        (*mbuf.cast::<PyManagedBufferObject>()).master = core::ptr::read(info);
        (*mbuf.cast::<PyManagedBufferObject>()).master.obj = core::ptr::null_mut();
        let mv = mbuf_add_view(mbuf, core::ptr::null());
        crate::types::exc::release_object(mbuf);
        mv
    }
}

/// CPython's `PyMemoryView_FromObjectAndFlags`.
unsafe fn from_object_and_flags(v: *mut PyObject, flags: c_int) -> *mut PyObject {
    // SAFETY: `v` follows the C contract.
    unsafe {
        if is_memoryview(v) {
            let mv = v.cast::<PyMemoryViewObject>();
            if base_inaccessible(v) {
                tstate::set_string(
                    exceptions::value_error(),
                    b"operation forbidden on released memoryview object",
                );
                return core::ptr::null_mut();
            }
            if (*mv).flags & MEMORYVIEW_RESTRICTED != 0 {
                tstate::set_string(
                    exceptions::value_error(),
                    b"cannot create new view on restricted memoryview",
                );
                return core::ptr::null_mut();
            }
            return mbuf_add_view((*mv).mbuf, &(*mv).view);
        }
        if crate::capi::r#abstract::PyObject_CheckBuffer(v) != 0 {
            let mbuf = managed_buffer_from_object(v, flags);
            if mbuf.is_null() {
                return core::ptr::null_mut();
            }
            let ret = mbuf_add_view(mbuf, core::ptr::null_mut());
            crate::types::exc::release_object(mbuf);
            return ret;
        }
        // SAFETY: the type name is a valid C string on live types.
        let name = util::cstr((*header::type_of(v)).tp_name);
        // CPython's `%.200s` truncates the name at 200 characters.
        let name = match name.char_indices().nth(200) {
            Some((i, _)) => &name[..i],
            None => name,
        };
        tstate::set_string(
            exceptions::type_error(),
            format!(
                "memoryview: a bytes-like object is required, not '{}'",
                name
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
/// `v` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromObject(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument follows the C contract.
    unsafe { from_object_and_flags(v, PyBUF_FULL_RO) }
}

/// CPython's `memory_from_contiguous_copy`: a read-only view over a fresh
/// bytes object holding `src` linearised in the requested order.
unsafe fn memory_from_contiguous_copy(src: &Py_buffer, order: u8) -> *mut PyObject {
    // SAFETY: `src` has full information and `ndim > 0`.
    unsafe {
        let bytes = crate::types::bytes::PyBytes_FromStringAndSize(core::ptr::null(), src.len);
        if bytes.is_null() {
            return core::ptr::null_mut();
        }
        let mbuf = managed_buffer_from_object(bytes, PyBUF_FULL_RO);
        crate::types::exc::release_object(bytes);
        if mbuf.is_null() {
            return core::ptr::null_mut();
        }
        // The exporter's format string may vanish with it; the master owns
        // its copy for as long as it lives.
        if !src.format.is_null() {
            // SAFETY: the format is a NUL-terminated string by contract.
            let fmt = core::ffi::CStr::from_ptr(src.format).to_bytes();
            let copy = crate::alloc::pymem::PyMem_Malloc(fmt.len() + 1) as *mut c_char;
            if copy.is_null() {
                tstate::set_string(exceptions::memory_error(), b"");
                crate::types::exc::release_object(mbuf);
                return core::ptr::null_mut();
            }
            core::ptr::copy_nonoverlapping(fmt.as_ptr().cast(), copy, fmt.len() + 1);
            let buffer = &mut *mbuf.cast::<PyManagedBufferObject>();
            buffer.master.format = copy;
            buffer.flags |= MANAGED_BUFFER_FREE_FORMAT;
        }
        let mv = mbuf_add_incomplete_view(mbuf, src.ndim);
        crate::types::exc::release_object(mbuf);
        if mv.is_null() {
            return core::ptr::null_mut();
        }
        let view = &raw mut (*mv.cast::<PyMemoryViewObject>()).view;
        (*view).itemsize = src.itemsize;
        for i in 0..src.ndim as usize {
            *(*view).shape.add(i) = *src.shape.add(i);
        }
        if order == b'C' || order == b'A' {
            init_strides_from_shape(&*view);
        } else {
            init_fortran_strides_from_shape(&*view);
        }
        (*view).suboffsets = core::ptr::null_mut();
        init_flags(mv);
        if copy_buffer(&*view, src).is_err() {
            crate::types::exc::release_object(mv);
            return core::ptr::null_mut();
        }
        mv
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_GetContiguous(
    obj: *mut PyObject,
    buffertype: c_int,
    order: c_char,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        let mv = from_object_and_flags(obj, PyBUF_FULL_RO);
        if mv.is_null() {
            return core::ptr::null_mut();
        }
        let view = &raw mut (*mv.cast::<PyMemoryViewObject>()).view;
        if buffertype == PyBUF_WRITE && (*view).readonly != 0 {
            tstate::set_string(
                exceptions::buffer_error(),
                b"underlying buffer is not writable",
            );
            crate::types::exc::release_object(mv);
            return core::ptr::null_mut();
        }
        if crate::capi::r#abstract::PyBuffer_IsContiguous(&*view, order) != 0 {
            return mv;
        }
        if buffertype == PyBUF_WRITE {
            tstate::set_string(
                exceptions::buffer_error(),
                b"writable contiguous buffer requested for a non-contiguous object.",
            );
            crate::types::exc::release_object(mv);
            return core::ptr::null_mut();
        }
        let ret = memory_from_contiguous_copy(&*view, order as u8);
        crate::types::exc::release_object(mv);
        ret
    }
}

/// `PyBuffer_ToContiguous` (memoryobject.c): copy `src` into `buf` in the
/// requested order.
///
/// # Safety
///
/// `buf` must hold `len` writable bytes; `src` must be a valid view with
/// full buffer information.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_ToContiguous(
    buf: *mut c_void,
    src: *const Py_buffer,
    len: isize,
    order: c_char,
) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if len != (*src).len {
            tstate::set_string(
                exceptions::value_error(),
                b"PyBuffer_ToContiguous: len != view->len",
            );
            return -1;
        }
        if crate::capi::r#abstract::PyBuffer_IsContiguous(&*src, order) != 0 {
            core::ptr::copy_nonoverlapping(
                (*src).buf.cast::<u8>(),
                buf.cast(),
                len.max(0) as usize,
            );
            return 0;
        }
        // SAFETY: the caller guarantees `len` writable bytes at `buf`.
        let mem = core::slice::from_raw_parts_mut(buf.cast(), len.max(0) as usize);
        if buffer_to_contiguous(mem, &*src, order as u8).is_err() {
            return -1;
        }
        0
    }
}

/// Ready both types; `init_static_types` calls this once.
pub fn init() -> c_int {
    // SAFETY: init runs before any concurrent access can exist.
    unsafe {
        for t in [&raw mut MANAGED_BUFFER_TYPE, &raw mut PyMemoryView_Type] {
            if crate::object::typeobject::PyType_Ready(t) < 0 {
                return -1;
            }
            crate::gc::counted::note_coil_type(t);
        }
    }
    0
}

//! Layout of the declared types against the Windows x64 ABI of the pinned
//! 3.12.10 headers: MSVC packs them with 8-byte alignment and a 32-bit
//! `long`. These are the numbers the cp312 constants in `coil-rt-abi` must
//! agree with wherever both name the same struct.

use coil_capi_sys::*;
use core::mem::{offset_of, size_of};

#[test]
fn object_and_var_object() {
    assert_eq!(size_of::<PyObject>(), 16);
    assert_eq!(offset_of!(PyObject, ob_refcnt), 0);
    assert_eq!(offset_of!(PyObject, ob_type), 8);

    assert_eq!(size_of::<PyVarObject>(), 24);
    assert_eq!(offset_of!(PyVarObject, ob_base), 0);
    assert_eq!(offset_of!(PyVarObject, ob_size), 16);
}

#[test]
fn method_tables() {
    // 36 pointer-sized slots in header order.
    assert_eq!(size_of::<PyNumberMethods>(), 288);
    assert_eq!(offset_of!(PyNumberMethods, nb_add), 0);
    assert_eq!(offset_of!(PyNumberMethods, nb_subtract), 8);
    assert_eq!(offset_of!(PyNumberMethods, nb_multiply), 16);
    assert_eq!(offset_of!(PyNumberMethods, nb_remainder), 24);
    assert_eq!(offset_of!(PyNumberMethods, nb_divmod), 32);
    assert_eq!(offset_of!(PyNumberMethods, nb_power), 40);
    assert_eq!(offset_of!(PyNumberMethods, nb_negative), 48);
    assert_eq!(offset_of!(PyNumberMethods, nb_positive), 56);
    assert_eq!(offset_of!(PyNumberMethods, nb_absolute), 64);
    assert_eq!(offset_of!(PyNumberMethods, nb_bool), 72);
    assert_eq!(offset_of!(PyNumberMethods, nb_invert), 80);
    assert_eq!(offset_of!(PyNumberMethods, nb_lshift), 88);
    assert_eq!(offset_of!(PyNumberMethods, nb_rshift), 96);
    assert_eq!(offset_of!(PyNumberMethods, nb_and), 104);
    assert_eq!(offset_of!(PyNumberMethods, nb_xor), 112);
    assert_eq!(offset_of!(PyNumberMethods, nb_or), 120);
    assert_eq!(offset_of!(PyNumberMethods, nb_int), 128);
    assert_eq!(offset_of!(PyNumberMethods, nb_reserved), 136);
    assert_eq!(offset_of!(PyNumberMethods, nb_float), 144);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_add), 152);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_subtract), 160);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_multiply), 168);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_remainder), 176);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_power), 184);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_lshift), 192);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_rshift), 200);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_and), 208);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_xor), 216);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_or), 224);
    assert_eq!(offset_of!(PyNumberMethods, nb_floor_divide), 232);
    assert_eq!(offset_of!(PyNumberMethods, nb_true_divide), 240);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_floor_divide), 248);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_true_divide), 256);
    assert_eq!(offset_of!(PyNumberMethods, nb_index), 264);
    assert_eq!(offset_of!(PyNumberMethods, nb_matrix_multiply), 272);
    assert_eq!(offset_of!(PyNumberMethods, nb_inplace_matrix_multiply), 280);

    assert_eq!(size_of::<PySequenceMethods>(), 80);
    assert_eq!(offset_of!(PySequenceMethods, sq_length), 0);
    assert_eq!(offset_of!(PySequenceMethods, sq_concat), 8);
    assert_eq!(offset_of!(PySequenceMethods, sq_repeat), 16);
    assert_eq!(offset_of!(PySequenceMethods, sq_item), 24);
    assert_eq!(offset_of!(PySequenceMethods, was_sq_slice), 32);
    assert_eq!(offset_of!(PySequenceMethods, sq_ass_item), 40);
    assert_eq!(offset_of!(PySequenceMethods, was_sq_ass_slice), 48);
    assert_eq!(offset_of!(PySequenceMethods, sq_contains), 56);
    assert_eq!(offset_of!(PySequenceMethods, sq_inplace_concat), 64);
    assert_eq!(offset_of!(PySequenceMethods, sq_inplace_repeat), 72);

    assert_eq!(size_of::<PyMappingMethods>(), 24);
    assert_eq!(offset_of!(PyMappingMethods, mp_length), 0);
    assert_eq!(offset_of!(PyMappingMethods, mp_subscript), 8);
    assert_eq!(offset_of!(PyMappingMethods, mp_ass_subscript), 16);

    assert_eq!(size_of::<PyAsyncMethods>(), 32);
    assert_eq!(offset_of!(PyAsyncMethods, am_await), 0);
    assert_eq!(offset_of!(PyAsyncMethods, am_aiter), 8);
    assert_eq!(offset_of!(PyAsyncMethods, am_anext), 16);
    assert_eq!(offset_of!(PyAsyncMethods, am_send), 24);

    assert_eq!(size_of::<PyBufferProcs>(), 16);
    assert_eq!(offset_of!(PyBufferProcs, bf_getbuffer), 0);
    assert_eq!(offset_of!(PyBufferProcs, bf_releasebuffer), 8);
}

#[test]
fn type_object() {
    assert_eq!(size_of::<PyTypeObject>(), 416);
    // PyObject_VAR_HEAD
    assert_eq!(offset_of!(PyTypeObject, ob_refcnt), 0);
    assert_eq!(offset_of!(PyTypeObject, ob_type), 8);
    assert_eq!(offset_of!(PyTypeObject, ob_size), 16);
    assert_eq!(offset_of!(PyTypeObject, tp_name), 24);
    assert_eq!(offset_of!(PyTypeObject, tp_basicsize), 32);
    assert_eq!(offset_of!(PyTypeObject, tp_itemsize), 40);
    assert_eq!(offset_of!(PyTypeObject, tp_dealloc), 48);
    assert_eq!(offset_of!(PyTypeObject, tp_vectorcall_offset), 56);
    assert_eq!(offset_of!(PyTypeObject, tp_getattr), 64);
    assert_eq!(offset_of!(PyTypeObject, tp_setattr), 72);
    assert_eq!(offset_of!(PyTypeObject, tp_as_async), 80);
    assert_eq!(offset_of!(PyTypeObject, tp_repr), 88);
    assert_eq!(offset_of!(PyTypeObject, tp_as_number), 96);
    assert_eq!(offset_of!(PyTypeObject, tp_as_sequence), 104);
    assert_eq!(offset_of!(PyTypeObject, tp_as_mapping), 112);
    assert_eq!(offset_of!(PyTypeObject, tp_hash), 120);
    assert_eq!(offset_of!(PyTypeObject, tp_call), 128);
    assert_eq!(offset_of!(PyTypeObject, tp_str), 136);
    assert_eq!(offset_of!(PyTypeObject, tp_getattro), 144);
    assert_eq!(offset_of!(PyTypeObject, tp_setattro), 152);
    assert_eq!(offset_of!(PyTypeObject, tp_as_buffer), 160);
    // unsigned long is 4 bytes on Windows x64; tp_doc skips the padding.
    assert_eq!(offset_of!(PyTypeObject, tp_flags), 168);
    assert_eq!(offset_of!(PyTypeObject, tp_doc), 176);
    assert_eq!(offset_of!(PyTypeObject, tp_traverse), 184);
    assert_eq!(offset_of!(PyTypeObject, tp_clear), 192);
    assert_eq!(offset_of!(PyTypeObject, tp_richcompare), 200);
    assert_eq!(offset_of!(PyTypeObject, tp_weaklistoffset), 208);
    assert_eq!(offset_of!(PyTypeObject, tp_iter), 216);
    assert_eq!(offset_of!(PyTypeObject, tp_iternext), 224);
    assert_eq!(offset_of!(PyTypeObject, tp_methods), 232);
    assert_eq!(offset_of!(PyTypeObject, tp_members), 240);
    assert_eq!(offset_of!(PyTypeObject, tp_getset), 248);
    assert_eq!(offset_of!(PyTypeObject, tp_base), 256);
    assert_eq!(offset_of!(PyTypeObject, tp_dict), 264);
    assert_eq!(offset_of!(PyTypeObject, tp_descr_get), 272);
    assert_eq!(offset_of!(PyTypeObject, tp_descr_set), 280);
    assert_eq!(offset_of!(PyTypeObject, tp_dictoffset), 288);
    assert_eq!(offset_of!(PyTypeObject, tp_init), 296);
    assert_eq!(offset_of!(PyTypeObject, tp_alloc), 304);
    assert_eq!(offset_of!(PyTypeObject, tp_new), 312);
    assert_eq!(offset_of!(PyTypeObject, tp_free), 320);
    assert_eq!(offset_of!(PyTypeObject, tp_is_gc), 328);
    assert_eq!(offset_of!(PyTypeObject, tp_bases), 336);
    assert_eq!(offset_of!(PyTypeObject, tp_mro), 344);
    assert_eq!(offset_of!(PyTypeObject, tp_cache), 352);
    assert_eq!(offset_of!(PyTypeObject, tp_subclasses), 360);
    assert_eq!(offset_of!(PyTypeObject, tp_weaklist), 368);
    assert_eq!(offset_of!(PyTypeObject, tp_del), 376);
    // unsigned int again; tp_finalize skips the padding.
    assert_eq!(offset_of!(PyTypeObject, tp_version_tag), 384);
    assert_eq!(offset_of!(PyTypeObject, tp_finalize), 392);
    assert_eq!(offset_of!(PyTypeObject, tp_vectorcall), 400);
    assert_eq!(offset_of!(PyTypeObject, tp_watched), 408);
}

#[test]
fn method_and_member_definitions() {
    assert_eq!(size_of::<PyMethodDef>(), 32);
    assert_eq!(offset_of!(PyMethodDef, ml_name), 0);
    assert_eq!(offset_of!(PyMethodDef, ml_meth), 8);
    assert_eq!(offset_of!(PyMethodDef, ml_flags), 16);
    assert_eq!(offset_of!(PyMethodDef, ml_doc), 24);

    assert_eq!(size_of::<PyGetSetDef>(), 40);
    assert_eq!(offset_of!(PyGetSetDef, name), 0);
    assert_eq!(offset_of!(PyGetSetDef, get), 8);
    assert_eq!(offset_of!(PyGetSetDef, set), 16);
    assert_eq!(offset_of!(PyGetSetDef, doc), 24);
    assert_eq!(offset_of!(PyGetSetDef, closure), 32);

    assert_eq!(size_of::<PyMemberDef>(), 40);
    assert_eq!(offset_of!(PyMemberDef, name), 0);
    assert_eq!(offset_of!(PyMemberDef, type_), 8);
    assert_eq!(offset_of!(PyMemberDef, offset), 16);
    assert_eq!(offset_of!(PyMemberDef, flags), 24);
    assert_eq!(offset_of!(PyMemberDef, doc), 32);
}

#[test]
fn module_definitions() {
    assert_eq!(size_of::<PyModuleDef_Base>(), 40);
    assert_eq!(offset_of!(PyModuleDef_Base, ob_base), 0);
    assert_eq!(offset_of!(PyModuleDef_Base, m_init), 16);
    assert_eq!(offset_of!(PyModuleDef_Base, m_index), 24);
    assert_eq!(offset_of!(PyModuleDef_Base, m_copy), 32);

    assert_eq!(size_of::<PyModuleDef_Slot>(), 16);
    assert_eq!(offset_of!(PyModuleDef_Slot, slot), 0);
    assert_eq!(offset_of!(PyModuleDef_Slot, value), 8);

    assert_eq!(size_of::<PyModuleDef>(), 104);
    assert_eq!(offset_of!(PyModuleDef, m_base), 0);
    assert_eq!(offset_of!(PyModuleDef, m_name), 40);
    assert_eq!(offset_of!(PyModuleDef, m_doc), 48);
    assert_eq!(offset_of!(PyModuleDef, m_size), 56);
    assert_eq!(offset_of!(PyModuleDef, m_methods), 64);
    assert_eq!(offset_of!(PyModuleDef, m_slots), 72);
    assert_eq!(offset_of!(PyModuleDef, m_traverse), 80);
    assert_eq!(offset_of!(PyModuleDef, m_clear), 88);
    assert_eq!(offset_of!(PyModuleDef, m_free), 96);
}

#[test]
fn type_spec_slots() {
    assert_eq!(size_of::<PyType_Slot>(), 16);
    assert_eq!(offset_of!(PyType_Slot, slot), 0);
    assert_eq!(offset_of!(PyType_Slot, pfunc), 8);

    assert_eq!(size_of::<PyType_Spec>(), 32);
    assert_eq!(offset_of!(PyType_Spec, name), 0);
    assert_eq!(offset_of!(PyType_Spec, basicsize), 8);
    assert_eq!(offset_of!(PyType_Spec, itemsize), 12);
    assert_eq!(offset_of!(PyType_Spec, flags), 16);
    assert_eq!(offset_of!(PyType_Spec, slots), 24);
}

#[test]
fn buffer() {
    assert_eq!(size_of::<Py_buffer>(), 80);
    assert_eq!(offset_of!(Py_buffer, buf), 0);
    assert_eq!(offset_of!(Py_buffer, obj), 8);
    assert_eq!(offset_of!(Py_buffer, len), 16);
    assert_eq!(offset_of!(Py_buffer, itemsize), 24);
    assert_eq!(offset_of!(Py_buffer, readonly), 32);
    assert_eq!(offset_of!(Py_buffer, ndim), 36);
    assert_eq!(offset_of!(Py_buffer, format), 40);
    assert_eq!(offset_of!(Py_buffer, shape), 48);
    assert_eq!(offset_of!(Py_buffer, strides), 56);
    assert_eq!(offset_of!(Py_buffer, suboffsets), 64);
    assert_eq!(offset_of!(Py_buffer, internal), 72);
}

#[test]
fn abi_widths() {
    // The aliases pin the target ABI, not the building host's.
    assert_eq!(size_of::<c_long>(), 4);
    assert_eq!(size_of::<c_ulong>(), 4);
    assert_eq!(size_of::<Py_ssize_t>(), 8);
    assert_eq!(size_of::<Py_hash_t>(), 8);
    assert_eq!(size_of::<vectorcallfunc>(), 8);
    assert_eq!(size_of::<PyCFunction>(), 8);
}

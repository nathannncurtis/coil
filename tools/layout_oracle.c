/* Layout oracle for coil-rt-abi (spec section 8.2).
 *
 * tools/layout_oracle.py compiles this file against the pinned CPython
 * 3.12.10 headers with MSVC on Windows x64 and runs it. It prints one line
 * per constant in native/crates/coil-rt-abi/src/cp312.rs:
 *
 *   <struct>.<field> offset <bytes>   for every field constant
 *   <struct> sizeof <bytes>           for every SIZEOF constant
 *
 * The struct and field names in the output are the Rust module and constant
 * names, lowercased; they are labels, not C spellings. Nested members are
 * printed flattened (long_value.lv_tag -> long.lv_tag, m_base.m_init ->
 * moduledef.m_init) and the heap type's underscore members drop the leading
 * underscore (_ht_tpname -> heaptypeobject.ht_tpname).
 *
 * "bool" is CPython's PyLongObject: 3.12 has no PyBoolObject, the bool
 * singletons are PyLongObject instances (boolobject.h).
 */

#include <stddef.h>
#include <stdio.h>

#include "Python.h"

#define FIELD(type, member, label) \
    printf(label " offset %zu\n", offsetof(type, member))
#define SIZEOF(type, label) printf(label " sizeof %zu\n", sizeof(type))

int main(void) {
    /* The header every object starts with (section 8.1). */
    FIELD(PyObject, ob_refcnt, "object.ob_refcnt");
    FIELD(PyObject, ob_type, "object.ob_type");
    SIZEOF(PyObject, "object");
    FIELD(PyVarObject, ob_base, "varobject.ob_base");
    FIELD(PyVarObject, ob_size, "varobject.ob_size");
    SIZEOF(PyVarObject, "varobject");

    /* Exact layouts (section 8.2): byte-for-byte CPython. */
    FIELD(PyTupleObject, ob_item, "tuple.ob_item");
    SIZEOF(PyTupleObject, "tuple");

    FIELD(PyListObject, ob_item, "list.ob_item");
    FIELD(PyListObject, allocated, "list.allocated");
    SIZEOF(PyListObject, "list");

    FIELD(PyBytesObject, ob_shash, "bytes.ob_shash");
    FIELD(PyBytesObject, ob_sval, "bytes.ob_sval");
    SIZEOF(PyBytesObject, "bytes");

    FIELD(PyByteArrayObject, ob_alloc, "bytearray.ob_alloc");
    FIELD(PyByteArrayObject, ob_bytes, "bytearray.ob_bytes");
    FIELD(PyByteArrayObject, ob_start, "bytearray.ob_start");
    FIELD(PyByteArrayObject, ob_exports, "bytearray.ob_exports");
    SIZEOF(PyByteArrayObject, "bytearray");

    /* Compact ASCII (PyASCIIObject), compact Unicode (PyCompactUnicodeObject)
     * and legacy (PyUnicodeObject) strings; the state bitfields are not
     * addressable, so only the field itself is reported. */
    FIELD(PyASCIIObject, length, "ascii.length");
    FIELD(PyASCIIObject, hash, "ascii.hash");
    FIELD(PyASCIIObject, state, "ascii.state");
    SIZEOF(PyASCIIObject, "ascii");

    FIELD(PyCompactUnicodeObject, utf8_length, "compact_unicode.utf8_length");
    FIELD(PyCompactUnicodeObject, utf8, "compact_unicode.utf8");
    SIZEOF(PyCompactUnicodeObject, "compact_unicode");

    FIELD(PyUnicodeObject, data, "unicode.data");
    SIZEOF(PyUnicodeObject, "unicode");

    FIELD(PyFloatObject, ob_fval, "float.ob_fval");
    SIZEOF(PyFloatObject, "float");

    FIELD(PyComplexObject, cval, "complex.cval");
    FIELD(PyComplexObject, cval.real, "complex.cval_real");
    FIELD(PyComplexObject, cval.imag, "complex.cval_imag");
    SIZEOF(PyComplexObject, "complex");

    FIELD(PyLongObject, long_value, "long.long_value");
    FIELD(PyLongObject, long_value.lv_tag, "long.lv_tag");
    FIELD(PyLongObject, long_value.ob_digit, "long.ob_digit");
    SIZEOF(PyLongObject, "long");

    SIZEOF(PyLongObject, "bool");

    FIELD(PyMemoryViewObject, mbuf, "memoryview.mbuf");
    FIELD(PyMemoryViewObject, hash, "memoryview.hash");
    FIELD(PyMemoryViewObject, flags, "memoryview.flags");
    FIELD(PyMemoryViewObject, exports, "memoryview.exports");
    FIELD(PyMemoryViewObject, view, "memoryview.view");
    FIELD(PyMemoryViewObject, weakreflist, "memoryview.weakreflist");
    FIELD(PyMemoryViewObject, ob_array, "memoryview.ob_array");
    SIZEOF(PyMemoryViewObject, "memoryview");

    FIELD(Py_buffer, buf, "buffer.buf");
    FIELD(Py_buffer, obj, "buffer.obj");
    FIELD(Py_buffer, len, "buffer.len");
    FIELD(Py_buffer, itemsize, "buffer.itemsize");
    FIELD(Py_buffer, readonly, "buffer.readonly");
    FIELD(Py_buffer, ndim, "buffer.ndim");
    FIELD(Py_buffer, format, "buffer.format");
    FIELD(Py_buffer, shape, "buffer.shape");
    FIELD(Py_buffer, strides, "buffer.strides");
    FIELD(Py_buffer, suboffsets, "buffer.suboffsets");
    FIELD(Py_buffer, internal, "buffer.internal");
    SIZEOF(Py_buffer, "buffer");

    FIELD(PyCellObject, ob_ref, "cell.ob_ref");
    SIZEOF(PyCellObject, "cell");

    FIELD(PyWeakReference, wr_object, "weakref.wr_object");
    FIELD(PyWeakReference, wr_callback, "weakref.wr_callback");
    FIELD(PyWeakReference, hash, "weakref.hash");
    FIELD(PyWeakReference, wr_prev, "weakref.wr_prev");
    FIELD(PyWeakReference, wr_next, "weakref.wr_next");
    FIELD(PyWeakReference, vectorcall, "weakref.vectorcall");
    SIZEOF(PyWeakReference, "weakref");

    FIELD(PyCFunctionObject, m_ml, "cfunction.m_ml");
    FIELD(PyCFunctionObject, m_self, "cfunction.m_self");
    FIELD(PyCFunctionObject, m_module, "cfunction.m_module");
    FIELD(PyCFunctionObject, m_weakreflist, "cfunction.m_weakreflist");
    FIELD(PyCFunctionObject, vectorcall, "cfunction.vectorcall");
    SIZEOF(PyCFunctionObject, "cfunction");

    FIELD(PyCMethodObject, func, "cmethod.func");
    FIELD(PyCMethodObject, mm_class, "cmethod.mm_class");
    SIZEOF(PyCMethodObject, "cmethod");

    FIELD(PyMethodDescrObject, d_common.d_type, "method_descr.d_type");
    FIELD(PyMethodDescrObject, d_common.d_name, "method_descr.d_name");
    FIELD(PyMethodDescrObject, d_common.d_qualname, "method_descr.d_qualname");
    FIELD(PyMethodDescrObject, d_method, "method_descr.d_method");
    FIELD(PyMethodDescrObject, vectorcall, "method_descr.vectorcall");
    SIZEOF(PyMethodDescrObject, "method_descr");

    FIELD(PyMemberDescrObject, d_common.d_type, "member_descr.d_type");
    FIELD(PyMemberDescrObject, d_common.d_name, "member_descr.d_name");
    FIELD(PyMemberDescrObject, d_common.d_qualname, "member_descr.d_qualname");
    FIELD(PyMemberDescrObject, d_member, "member_descr.d_member");
    SIZEOF(PyMemberDescrObject, "member_descr");

    FIELD(PyGetSetDescrObject, d_common.d_type, "getset_descr.d_type");
    FIELD(PyGetSetDescrObject, d_common.d_name, "getset_descr.d_name");
    FIELD(PyGetSetDescrObject, d_common.d_qualname, "getset_descr.d_qualname");
    FIELD(PyGetSetDescrObject, d_getset, "getset_descr.d_getset");
    SIZEOF(PyGetSetDescrObject, "getset_descr");

    FIELD(PyWrapperDescrObject, d_common.d_type, "wrapper_descr.d_type");
    FIELD(PyWrapperDescrObject, d_common.d_name, "wrapper_descr.d_name");
    FIELD(PyWrapperDescrObject, d_common.d_qualname, "wrapper_descr.d_qualname");
    FIELD(PyWrapperDescrObject, d_base, "wrapper_descr.d_base");
    FIELD(PyWrapperDescrObject, d_wrapped, "wrapper_descr.d_wrapped");
    SIZEOF(PyWrapperDescrObject, "wrapper_descr");

    FIELD(PyMethodObject, im_func, "method.im_func");
    FIELD(PyMethodObject, im_self, "method.im_self");
    FIELD(PyMethodObject, im_weakreflist, "method.im_weakreflist");
    FIELD(PyMethodObject, vectorcall, "method.vectorcall");
    SIZEOF(PyMethodObject, "method");

    FIELD(PySliceObject, start, "slice.start");
    FIELD(PySliceObject, stop, "slice.stop");
    FIELD(PySliceObject, step, "slice.step");
    SIZEOF(PySliceObject, "slice");

    FIELD(PyBaseExceptionObject, dict, "base_exception.dict");
    FIELD(PyBaseExceptionObject, args, "base_exception.args");
    FIELD(PyBaseExceptionObject, notes, "base_exception.notes");
    FIELD(PyBaseExceptionObject, traceback, "base_exception.traceback");
    FIELD(PyBaseExceptionObject, context, "base_exception.context");
    FIELD(PyBaseExceptionObject, cause, "base_exception.cause");
    FIELD(PyBaseExceptionObject, suppress_context, "base_exception.suppress_context");
    SIZEOF(PyBaseExceptionObject, "base_exception");

    FIELD(PyTracebackObject, tb_next, "traceback.tb_next");
    FIELD(PyTracebackObject, tb_frame, "traceback.tb_frame");
    FIELD(PyTracebackObject, tb_lasti, "traceback.tb_lasti");
    FIELD(PyTracebackObject, tb_lineno, "traceback.tb_lineno");
    SIZEOF(PyTracebackObject, "traceback");

    FIELD(PyModuleDef, m_base, "moduledef.m_base");
    FIELD(PyModuleDef, m_base.m_init, "moduledef.m_init");
    FIELD(PyModuleDef, m_base.m_index, "moduledef.m_index");
    FIELD(PyModuleDef, m_base.m_copy, "moduledef.m_copy");
    FIELD(PyModuleDef, m_name, "moduledef.m_name");
    FIELD(PyModuleDef, m_doc, "moduledef.m_doc");
    FIELD(PyModuleDef, m_size, "moduledef.m_size");
    FIELD(PyModuleDef, m_methods, "moduledef.m_methods");
    FIELD(PyModuleDef, m_slots, "moduledef.m_slots");
    FIELD(PyModuleDef, m_traverse, "moduledef.m_traverse");
    FIELD(PyModuleDef, m_clear, "moduledef.m_clear");
    FIELD(PyModuleDef, m_free, "moduledef.m_free");
    SIZEOF(PyModuleDef, "moduledef");

    FIELD(PyTypeObject, tp_name, "typeobject.tp_name");
    FIELD(PyTypeObject, tp_basicsize, "typeobject.tp_basicsize");
    FIELD(PyTypeObject, tp_itemsize, "typeobject.tp_itemsize");
    FIELD(PyTypeObject, tp_dealloc, "typeobject.tp_dealloc");
    FIELD(PyTypeObject, tp_vectorcall_offset, "typeobject.tp_vectorcall_offset");
    FIELD(PyTypeObject, tp_getattr, "typeobject.tp_getattr");
    FIELD(PyTypeObject, tp_setattr, "typeobject.tp_setattr");
    FIELD(PyTypeObject, tp_as_async, "typeobject.tp_as_async");
    FIELD(PyTypeObject, tp_repr, "typeobject.tp_repr");
    FIELD(PyTypeObject, tp_as_number, "typeobject.tp_as_number");
    FIELD(PyTypeObject, tp_as_sequence, "typeobject.tp_as_sequence");
    FIELD(PyTypeObject, tp_as_mapping, "typeobject.tp_as_mapping");
    FIELD(PyTypeObject, tp_hash, "typeobject.tp_hash");
    FIELD(PyTypeObject, tp_call, "typeobject.tp_call");
    FIELD(PyTypeObject, tp_str, "typeobject.tp_str");
    FIELD(PyTypeObject, tp_getattro, "typeobject.tp_getattro");
    FIELD(PyTypeObject, tp_setattro, "typeobject.tp_setattro");
    FIELD(PyTypeObject, tp_as_buffer, "typeobject.tp_as_buffer");
    FIELD(PyTypeObject, tp_flags, "typeobject.tp_flags");
    FIELD(PyTypeObject, tp_doc, "typeobject.tp_doc");
    FIELD(PyTypeObject, tp_traverse, "typeobject.tp_traverse");
    FIELD(PyTypeObject, tp_clear, "typeobject.tp_clear");
    FIELD(PyTypeObject, tp_richcompare, "typeobject.tp_richcompare");
    FIELD(PyTypeObject, tp_weaklistoffset, "typeobject.tp_weaklistoffset");
    FIELD(PyTypeObject, tp_iter, "typeobject.tp_iter");
    FIELD(PyTypeObject, tp_iternext, "typeobject.tp_iternext");
    FIELD(PyTypeObject, tp_methods, "typeobject.tp_methods");
    FIELD(PyTypeObject, tp_members, "typeobject.tp_members");
    FIELD(PyTypeObject, tp_getset, "typeobject.tp_getset");
    FIELD(PyTypeObject, tp_base, "typeobject.tp_base");
    FIELD(PyTypeObject, tp_dict, "typeobject.tp_dict");
    FIELD(PyTypeObject, tp_descr_get, "typeobject.tp_descr_get");
    FIELD(PyTypeObject, tp_descr_set, "typeobject.tp_descr_set");
    FIELD(PyTypeObject, tp_dictoffset, "typeobject.tp_dictoffset");
    FIELD(PyTypeObject, tp_init, "typeobject.tp_init");
    FIELD(PyTypeObject, tp_alloc, "typeobject.tp_alloc");
    FIELD(PyTypeObject, tp_new, "typeobject.tp_new");
    FIELD(PyTypeObject, tp_free, "typeobject.tp_free");
    FIELD(PyTypeObject, tp_is_gc, "typeobject.tp_is_gc");
    FIELD(PyTypeObject, tp_bases, "typeobject.tp_bases");
    FIELD(PyTypeObject, tp_mro, "typeobject.tp_mro");
    FIELD(PyTypeObject, tp_cache, "typeobject.tp_cache");
    FIELD(PyTypeObject, tp_subclasses, "typeobject.tp_subclasses");
    FIELD(PyTypeObject, tp_weaklist, "typeobject.tp_weaklist");
    FIELD(PyTypeObject, tp_del, "typeobject.tp_del");
    FIELD(PyTypeObject, tp_version_tag, "typeobject.tp_version_tag");
    FIELD(PyTypeObject, tp_finalize, "typeobject.tp_finalize");
    FIELD(PyTypeObject, tp_vectorcall, "typeobject.tp_vectorcall");
    FIELD(PyTypeObject, tp_watched, "typeobject.tp_watched");
    SIZEOF(PyTypeObject, "typeobject");

    FIELD(PyHeapTypeObject, ht_type, "heaptypeobject.ht_type");
    FIELD(PyHeapTypeObject, as_async, "heaptypeobject.as_async");
    FIELD(PyHeapTypeObject, as_number, "heaptypeobject.as_number");
    FIELD(PyHeapTypeObject, as_mapping, "heaptypeobject.as_mapping");
    FIELD(PyHeapTypeObject, as_sequence, "heaptypeobject.as_sequence");
    FIELD(PyHeapTypeObject, as_buffer, "heaptypeobject.as_buffer");
    FIELD(PyHeapTypeObject, ht_name, "heaptypeobject.ht_name");
    FIELD(PyHeapTypeObject, ht_slots, "heaptypeobject.ht_slots");
    FIELD(PyHeapTypeObject, ht_qualname, "heaptypeobject.ht_qualname");
    FIELD(PyHeapTypeObject, ht_cached_keys, "heaptypeobject.ht_cached_keys");
    FIELD(PyHeapTypeObject, ht_module, "heaptypeobject.ht_module");
    FIELD(PyHeapTypeObject, _ht_tpname, "heaptypeobject.ht_tpname");
    FIELD(PyHeapTypeObject, _spec_cache, "heaptypeobject.spec_cache");
    SIZEOF(PyHeapTypeObject, "heaptypeobject");

    /* Prefix layouts (section 8.2): only the leading fields listed below are
     * guaranteed to match CPython; the tail is Coil's. SIZEOF still records
     * the full CPython struct for reference. */
    FIELD(PyDictObject, ma_used, "dict.ma_used");
    FIELD(PyDictObject, ma_version_tag, "dict.ma_version_tag");
    SIZEOF(PyDictObject, "dict");

    FIELD(PySetObject, fill, "set.fill");
    FIELD(PySetObject, used, "set.used");
    SIZEOF(PySetObject, "set");

    FIELD(PyFunctionObject, func_globals, "function.func_globals");
    FIELD(PyFunctionObject, func_builtins, "function.func_builtins");
    FIELD(PyFunctionObject, func_name, "function.func_name");
    FIELD(PyFunctionObject, func_qualname, "function.func_qualname");
    FIELD(PyFunctionObject, func_code, "function.func_code");
    FIELD(PyFunctionObject, func_defaults, "function.func_defaults");
    FIELD(PyFunctionObject, func_kwdefaults, "function.func_kwdefaults");
    FIELD(PyFunctionObject, func_closure, "function.func_closure");
    SIZEOF(PyFunctionObject, "function");

    FIELD(PyCodeObject, co_flags, "code.co_flags");
    FIELD(PyCodeObject, co_argcount, "code.co_argcount");
    FIELD(PyCodeObject, co_posonlyargcount, "code.co_posonlyargcount");
    FIELD(PyCodeObject, co_kwonlyargcount, "code.co_kwonlyargcount");
    SIZEOF(PyCodeObject, "code");

    return 0;
}

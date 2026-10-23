//! The C API stub registry behind `coil_rt_stub_count` and `coil_rt_is_stub`
//! (spec section 10.1): the gate tools/capi_usage.py --check runs the v1
//! extension set's target list against.

use coil_capi_sys::PYTHON312_EXPORTS;
use python312::capi;

fn export_names() -> Vec<&'static str> {
    PYTHON312_EXPORTS
        .lines()
        .map(|line| {
            line.split(' ')
                .next()
                .expect("export lines are '<name> <kind>'")
        })
        .collect()
}

#[test]
fn every_export_is_a_stub_until_the_export_layer_lands() {
    let exports = export_names();
    assert_eq!(exports.len(), 1706);
    for name in ["PyArg_ParseTuple", "PyFloat_Type", "_Py_Dealloc"] {
        assert_eq!(capi::is_stub(name), Some(true), "{name} must be a stub");
    }
    assert_eq!(capi::stub_names().len(), exports.len());
    assert_eq!(capi::coil_rt_stub_count(), exports.len());
}

#[test]
fn names_outside_the_export_list_are_not_exports() {
    assert_eq!(capi::is_stub("coil_rt_is_stub"), None);
    assert_eq!(capi::is_stub("PyErr_NotAnExport"), None);
}

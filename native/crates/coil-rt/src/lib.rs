//! coil-rt: the runtime behind `python312.dll` — heap, collector, object
//! model, built-in types, native modules, the C API and the `coil_rt_*`
//! ABI compiled code calls (spec section 10).

// Names keep the exact C spelling of the C API they mirror.
#![allow(non_upper_case_globals)]

pub mod abi_generated;
pub mod alloc;
pub mod arch;
pub mod capi;
mod capi_stubs;
pub mod eval_breaker;
pub mod gc;
pub mod gil;
pub mod import;
pub mod modules;
pub mod object;
pub mod tstate;
pub mod types;
pub mod util;

/// The SHA-256 of `native/abi.toml` this runtime was built against, for
/// `coil_rt_init` to compare against the hash embedded in an executable
/// (spec section 2).
#[unsafe(no_mangle)]
pub extern "C" fn coil_rt_abi_sha256() -> *const u8 {
    abi_generated::ABI_SHA256.as_ptr()
}

/// Initialize the runtime: the static types and the import machinery
/// (`sys.modules`, `sys`, `builtins`, `_imp`). Idempotent; 0 on success.
///
/// The ABI-hash and module-table registration arrive with compiled code and
/// are not part of this bootstrap.
#[unsafe(no_mangle)]
pub extern "C" fn coil_rt_init() -> core::ffi::c_int {
    if crate::object::typeobject::init_static_types() < 0 {
        return -1;
    }
    crate::import::init()
}

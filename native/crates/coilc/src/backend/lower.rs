//! Lowering Coil IR into machine-shaped IR: explicit `coil_rt_*` calls, boxing,
//! barriers, safepoint polls, exception-flag tests and frame-slot assignment
//! (spec section 12.1).

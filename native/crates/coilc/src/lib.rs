//! `coilc`: Coil's native compiler - front end, IR, analysis, optimisation,
//! codegen, linking and inspect (spec section 2).

pub mod analysis;
pub mod backend;
pub mod job;
pub mod link;
pub mod opt;
pub mod world;

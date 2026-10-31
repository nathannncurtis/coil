//! Coil IR: the typed SSA intermediate representation with explicit exception
//! edges, shared by the compiler middle end and the IR interpreter.

pub mod builder;
pub mod interp;
pub mod ir;
pub mod print;
pub mod serialize;
pub mod types;
pub mod verify;

include!(concat!(env!("OUT_DIR"), "/schema_hash.rs"));

//! The object core: the 16-byte header, immortal objects and singletons,
//! type objects and their initialisation (spec sections 8.1, 8.2 and 8.5).

pub mod exceptions;
pub mod header;
pub mod lock;
pub mod singletons;
pub mod typeobject;

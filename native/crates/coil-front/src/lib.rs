//! Coil's front end: the tokenizer, parser, symtable and lowering that turn Python source into Coil IR.

pub mod ast;
mod ast_generated;
pub mod dump;
pub mod intern;
pub mod tokenizer;
mod unicode_tables;

//! The linker: object files into a PE32+ executable with `.text .rdata .data
//! .pdata .xdata .reloc .rsrc .idata` sections (spec sections 2 and 13.3).
//! This task covers the core: `.text/.rdata/.data/.bss` input, imports,
//! relocations, the PE writer and the `.pdata`/`.xdata` unwind tables;
//! `.rsrc` is filled from `rsrc.rs` when resources are given.

pub mod imports;
pub mod objfile;
pub mod pe;
pub mod reloc;

pub mod pdata;
pub mod rsrc;

use std::fmt;

use self::rsrc::RsrcError;

/// Everything the linker rejects: bad input geometry, unknown names, or a
/// relocation that does not fit. The message names the offending item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    DuplicateSymbol(String),
    UnknownSymbol(String),
    UnknownImport {
        dll: String,
        name: String,
    },
    EntryNotInText(String),
    SymbolOutOfBounds {
        name: String,
        section: &'static str,
        offset: u32,
        size: u32,
    },
    RelocOutOfBounds {
        section: &'static str,
        offset: u32,
        len: u32,
        size: u32,
    },
    OverlappingRelocations {
        section: &'static str,
        first: u32,
        second: u32,
    },
    BadAlignment {
        what: &'static str,
        value: u32,
    },
    Rel32OutOfRange {
        place: u32,
        target: u32,
    },
    EmptyImportList(String),
    EmptyExportList(String),
    EmptyExportName,
    TooManyExports {
        count: usize,
    },
    NameHasNul(&'static str),
    BadUnwindInfo {
        function: String,
        reason: String,
    },
    FunctionRange {
        name: String,
        begin: u32,
        end: u32,
        text_size: u32,
    },
    OverlappingFunctions {
        first: String,
        second: String,
    },
    LayoutOverflow,
    /// The resource inputs could not be built into a `.rsrc` section.
    Rsrc(RsrcError),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::DuplicateSymbol(name) => {
                write!(f, "symbol '{name}' defined more than once")
            }
            LinkError::UnknownSymbol(name) => write!(f, "undefined symbol '{name}'"),
            LinkError::UnknownImport { dll, name } => {
                write!(f, "'{dll}' does not import '{name}'")
            }
            LinkError::EntryNotInText(name) => {
                write!(f, "entry symbol '{name}' is not in .text")
            }
            LinkError::SymbolOutOfBounds {
                name,
                section,
                offset,
                size,
            } => write!(
                f,
                "symbol '{name}' at {section}+{offset:#x} lies outside its {size:#x} bytes"
            ),
            LinkError::RelocOutOfBounds {
                section,
                offset,
                len,
                size,
            } => write!(
                f,
                "relocation at {section}+{offset:#x} needs {len} bytes but the section holds {size:#x}"
            ),
            LinkError::OverlappingRelocations {
                section,
                first,
                second,
            } => write!(
                f,
                "relocations at {section}+{first:#x} and {section}+{second:#x} overlap"
            ),
            LinkError::BadAlignment { what, value } => write!(
                f,
                "{what} alignment {value:#x} is not a power of two up to 0x1000"
            ),
            LinkError::Rel32OutOfRange { place, target } => write!(
                f,
                "rel32 from {place:#x} to {target:#x} does not fit in 32 bits"
            ),
            LinkError::EmptyImportList(dll) => write!(f, "'{dll}' imports nothing"),
            LinkError::EmptyExportList(dll) => write!(f, "'{dll}' exports nothing"),
            LinkError::EmptyExportName => write!(f, "an export name is empty"),
            LinkError::TooManyExports { count } => {
                write!(f, "{count} exports do not fit the 16-bit ordinal table")
            }
            LinkError::NameHasNul(what) => write!(f, "{what} contains a NUL byte"),
            LinkError::BadUnwindInfo { function, reason } => {
                write!(
                    f,
                    "function '{function}' has unusable unwind info: {reason}"
                )
            }
            LinkError::FunctionRange {
                name,
                begin,
                end,
                text_size,
            } => write!(
                f,
                "function '{name}' spans {begin:#x}..{end:#x}, which is not inside .text's {text_size:#x} bytes"
            ),
            LinkError::OverlappingFunctions { first, second } => {
                write!(f, "functions '{first}' and '{second}' overlap in .text")
            }
            LinkError::LayoutOverflow => write!(f, "image layout arithmetic overflowed"),
            LinkError::Rsrc(error) => write!(f, "resources: {error}"),
        }
    }
}

impl std::error::Error for LinkError {}

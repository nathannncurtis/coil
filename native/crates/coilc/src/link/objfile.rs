//! The in-memory object file the backend produces: sections, symbols and
//! relocations, consumed by the PE writer (spec sections 2 and 12.1).

use std::collections::{BTreeMap, BTreeSet};

use super::LinkError;

/// `SizeOfStackReserve` when the input does not ask for one: 16 MiB, the
/// main-thread stack of a compiled Coil app (spec section 12.4).
pub const DEFAULT_STACK_RESERVE: u64 = 16 * 1024 * 1024;

/// One of the four input sections. `.bss` has no file bytes; its size lives
/// in [`ObjectFile::bss`] and it is laid out after `.data`'s own bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionId {
    Text,
    RData,
    Data,
    Bss,
}

impl SectionId {
    pub fn name(self) -> &'static str {
        match self {
            SectionId::Text => ".text",
            SectionId::RData => ".rdata",
            SectionId::Data => ".data",
            SectionId::Bss => ".bss",
        }
    }
}

/// File bytes of one input section, with the alignment its symbols expect.
#[derive(Clone, Debug, Default)]
pub struct SectionContent {
    pub bytes: Vec<u8>,
    pub align: u32,
}

/// The `.bss` contribution: virtual bytes only.
#[derive(Clone, Copy, Debug)]
pub struct BssSpec {
    pub size: u32,
    pub align: u32,
}

impl Default for BssSpec {
    fn default() -> BssSpec {
        BssSpec { size: 0, align: 1 }
    }
}

/// A named location: symbol resolution is by name, deterministically, and
/// the image build processes symbols in sorted order (spec section 13.3).
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub section: SectionId,
    pub offset: u32,
}

/// What a relocation points at: a defined symbol, or an imported function
/// reached through its IAT slot (see `imports.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Symbol(String),
    Import { dll: String, name: String },
}

/// Where a relocation's fixup bytes sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    pub section: SectionId,
    pub offset: u32,
}

/// `Rel32` writes a RIP-relative 32-bit displacement at `at`: the value is
/// `target - (at + 4) + addend`, the four covering the displacement field
/// itself. `Abs64` writes the 8-byte absolute image address `target +
/// addend` and contributes an `IMAGE_REL_BASED_DIR64` base relocation so
/// ASLR keeps it correct (spec section 12.2).
#[derive(Clone, Debug)]
pub enum Reloc {
    Rel32 {
        at: Place,
        target: Target,
        addend: i32,
    },
    Abs64 {
        at: Place,
        target: Target,
        addend: i64,
    },
}

impl Reloc {
    pub fn at(&self) -> Place {
        match self {
            Reloc::Rel32 { at, .. } | Reloc::Abs64 { at, .. } => *at,
        }
    }

    pub fn target(&self) -> &Target {
        match self {
            Reloc::Rel32 { target, .. } | Reloc::Abs64 { target, .. } => target,
        }
    }

    fn fixup_len(&self) -> u32 {
        match self {
            Reloc::Rel32 { .. } => 4,
            Reloc::Abs64 { .. } => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsystem {
    Console,
    Gui,
}

/// The linker's whole input: one translation unit with fixed section names.
#[derive(Clone, Debug)]
pub struct ObjectFile {
    pub text: SectionContent,
    pub rdata: SectionContent,
    pub data: SectionContent,
    pub bss: BssSpec,
    pub symbols: Vec<Symbol>,
    pub relocs: Vec<Reloc>,
    /// DLL name -> imported function names; the loader resolves by name.
    pub imports: BTreeMap<String, Vec<String>>,
    /// Entry symbol; must be defined in `.text`.
    pub entry: String,
    pub subsystem: Subsystem,
    /// `SizeOfStackReserve`; `None` means [`DEFAULT_STACK_RESERVE`].
    pub stack_reserve: Option<u64>,
}

impl ObjectFile {
    /// Checks every invariant the writer relies on, reporting the first
    /// violation in input order.
    pub fn validate(&self) -> Result<(), LinkError> {
        check_align(".text", self.text.align)?;
        check_align(".rdata", self.rdata.align)?;
        check_align(".data", self.data.align)?;
        check_align(".bss", self.bss.align)?;

        let mut names = BTreeSet::new();
        for symbol in &self.symbols {
            if !names.insert(symbol.name.as_str()) {
                return Err(LinkError::DuplicateSymbol(symbol.name.clone()));
            }
            let size = self.section_size(symbol.section);
            if symbol.offset >= size {
                return Err(LinkError::SymbolOutOfBounds {
                    name: symbol.name.clone(),
                    section: symbol.section.name(),
                    offset: symbol.offset,
                    size,
                });
            }
        }

        let mut spans: Vec<(SectionId, u32, u32)> = self
            .relocs
            .iter()
            .map(|reloc| (reloc.at().section, reloc.at().offset, reloc.fixup_len()))
            .collect();
        spans.sort_unstable();
        for pair in spans.windows(2) {
            let (section, start, len) = pair[0];
            let (next_section, next_start, _) = pair[1];
            if section == next_section && next_start < start + len {
                return Err(LinkError::OverlappingRelocations {
                    section: section.name(),
                    first: start,
                    second: next_start,
                });
            }
        }
        for reloc in &self.relocs {
            let at = reloc.at();
            let size = self.section_size(at.section);
            let len = reloc.fixup_len();
            if at.section == SectionId::Bss
                || u64::from(at.offset) + u64::from(len) > u64::from(size)
            {
                return Err(LinkError::RelocOutOfBounds {
                    section: at.section.name(),
                    offset: at.offset,
                    len,
                    size,
                });
            }
            match reloc.target() {
                Target::Symbol(name) => {
                    if !names.contains(name.as_str()) {
                        return Err(LinkError::UnknownSymbol(name.clone()));
                    }
                }
                Target::Import { dll, name } => {
                    if !self
                        .imports
                        .get(dll)
                        .is_some_and(|funcs| funcs.iter().any(|f| f == name))
                    {
                        return Err(LinkError::UnknownImport {
                            dll: dll.clone(),
                            name: name.clone(),
                        });
                    }
                }
            }
        }

        for (dll, funcs) in &self.imports {
            if dll.contains('\0') || funcs.iter().any(|name| name.contains('\0')) {
                return Err(LinkError::NameHasNul("an import name"));
            }
            if funcs.is_empty() {
                return Err(LinkError::EmptyImportList(dll.clone()));
            }
        }

        match self.symbol(&self.entry) {
            Some(symbol) if symbol.section == SectionId::Text => Ok(()),
            Some(symbol) => Err(LinkError::EntryNotInText(symbol.name.clone())),
            None => Err(LinkError::UnknownSymbol(self.entry.clone())),
        }
    }

    /// The `SizeOfStackReserve` to write: the requested value or the
    /// 16 MiB default.
    pub fn stack_reserve(&self) -> u64 {
        self.stack_reserve.unwrap_or(DEFAULT_STACK_RESERVE)
    }

    fn section_size(&self, section: SectionId) -> u32 {
        match section {
            SectionId::Text => self.text.bytes.len() as u32,
            SectionId::RData => self.rdata.bytes.len() as u32,
            SectionId::Data => self.data.bytes.len() as u32,
            SectionId::Bss => self.bss.size,
        }
    }

    fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|symbol| symbol.name == name)
    }
}

fn check_align(what: &'static str, value: u32) -> Result<(), LinkError> {
    if value == 0 || !value.is_power_of_two() || value > 0x1000 {
        return Err(LinkError::BadAlignment { what, value });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BssSpec, LinkError, ObjectFile, Place, Reloc, SectionContent, SectionId, Subsystem, Symbol,
        Target,
    };

    fn object() -> ObjectFile {
        ObjectFile {
            text: SectionContent {
                bytes: vec![0xC3; 16],
                align: 16,
            },
            rdata: SectionContent {
                bytes: Vec::new(),
                align: 8,
            },
            data: SectionContent {
                bytes: vec![0; 16],
                align: 8,
            },
            bss: BssSpec { size: 8, align: 8 },
            symbols: vec![
                Symbol {
                    name: "entry".into(),
                    section: SectionId::Text,
                    offset: 0,
                },
                Symbol {
                    name: "value".into(),
                    section: SectionId::Data,
                    offset: 0,
                },
            ],
            relocs: Vec::new(),
            imports: [("kernel32.dll".to_string(), vec!["ExitProcess".to_string()])]
                .into_iter()
                .collect(),
            entry: "entry".into(),
            subsystem: Subsystem::Console,
            stack_reserve: None,
        }
    }

    #[test]
    fn accepts_the_minimal_object() {
        assert_eq!(object().validate(), Ok(()));
        assert_eq!(object().stack_reserve(), super::DEFAULT_STACK_RESERVE);
    }

    #[test]
    fn rejects_bad_alignment_duplicate_and_entry() {
        let mut obj = object();
        obj.text.align = 3;
        assert_eq!(
            obj.validate(),
            Err(LinkError::BadAlignment {
                what: ".text",
                value: 3
            })
        );

        let mut obj = object();
        obj.symbols.push(Symbol {
            name: "entry".into(),
            section: SectionId::Text,
            offset: 8,
        });
        assert_eq!(
            obj.validate(),
            Err(LinkError::DuplicateSymbol("entry".into()))
        );

        let mut obj = object();
        obj.entry = "value".into();
        assert_eq!(
            obj.validate(),
            Err(LinkError::EntryNotInText("value".into()))
        );
    }

    #[test]
    fn rejects_bad_relocations() {
        let at = Place {
            section: SectionId::Data,
            offset: 0,
        };
        let mut obj = object();
        obj.relocs = vec![
            Reloc::Abs64 {
                at,
                target: Target::Symbol("value".into()),
                addend: 0,
            },
            Reloc::Rel32 {
                at: Place {
                    section: SectionId::Data,
                    offset: 4,
                },
                target: Target::Symbol("value".into()),
                addend: 0,
            },
        ];
        assert_eq!(
            obj.validate(),
            Err(LinkError::OverlappingRelocations {
                section: ".data",
                first: 0,
                second: 4,
            })
        );

        obj.relocs = vec![Reloc::Abs64 {
            at,
            target: Target::Import {
                dll: "kernel32.dll".into(),
                name: "GetCurrentProcessId".into(),
            },
            addend: 0,
        }];
        assert_eq!(
            obj.validate(),
            Err(LinkError::UnknownImport {
                dll: "kernel32.dll".into(),
                name: "GetCurrentProcessId".into(),
            })
        );

        obj.relocs = vec![Reloc::Abs64 {
            at: Place {
                section: SectionId::Bss,
                offset: 0,
            },
            target: Target::Symbol("value".into()),
            addend: 0,
        }];
        assert_eq!(
            obj.validate(),
            Err(LinkError::RelocOutOfBounds {
                section: ".bss",
                offset: 0,
                len: 8,
                size: 8,
            })
        );
    }
}

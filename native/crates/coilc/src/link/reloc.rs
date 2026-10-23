//! Resolves `Rel32` and `Abs64` relocations against the image layout and
//! writes the `.reloc` base-relocation table (`IMAGE_REL_BASED_DIR64`) for
//! every absolute address, so ASLR works (spec section 12.2).

use super::LinkError;
use super::imports::ImportTable;
use super::objfile::{ObjectFile, Reloc, SectionId, Target};
use super::pe::{IMAGE_BASE, Layout};

/// `IMAGE_REL_BASED_DIR64`.
const DIR64: u16 = 10;

/// The mutable section bytes relocations are written into, seeded from the
/// object file.
pub struct SectionBytes {
    pub text: Vec<u8>,
    pub rdata: Vec<u8>,
    pub data: Vec<u8>,
}

impl SectionBytes {
    pub fn new(obj: &ObjectFile) -> SectionBytes {
        SectionBytes {
            text: obj.text.bytes.clone(),
            rdata: obj.rdata.bytes.clone(),
            data: obj.data.bytes.clone(),
        }
    }

    fn bytes_mut(&mut self, section: SectionId) -> Result<&mut Vec<u8>, LinkError> {
        match section {
            SectionId::Text => Ok(&mut self.text),
            SectionId::RData => Ok(&mut self.rdata),
            SectionId::Data => Ok(&mut self.data),
            // `validate` rejects placements in `.bss` already.
            SectionId::Bss => Err(LinkError::RelocOutOfBounds {
                section: SectionId::Bss.name(),
                offset: 0,
                len: 0,
                size: 0,
            }),
        }
    }
}

/// Applies every relocation and returns the sorted RVAs of all `Abs64`
/// fixups, which become the DIR64 base relocations.
pub fn apply(
    obj: &ObjectFile,
    layout: &Layout,
    imports: &ImportTable,
    sections: &mut SectionBytes,
) -> Result<Vec<u32>, LinkError> {
    let symbols = layout.symbol_rvas(obj)?;
    let mut dir64 = Vec::new();
    for reloc in &obj.relocs {
        let target_rva = match reloc.target() {
            Target::Symbol(name) => *symbols
                .get(name)
                .ok_or_else(|| LinkError::UnknownSymbol(name.clone()))?,
            Target::Import { dll, name } => *imports
                .slots
                .get(&(dll.clone(), name.clone()))
                .ok_or_else(|| LinkError::UnknownImport {
                    dll: dll.clone(),
                    name: name.clone(),
                })?,
        };
        let at = reloc.at();
        let place_rva = layout.place_rva(at);
        let bytes = sections.bytes_mut(at.section)?;
        let offset = at.offset as usize;
        match reloc {
            Reloc::Rel32 { addend, .. } => {
                let value = i64::from(target_rva) + i64::from(*addend) - i64::from(place_rva) - 4;
                let value = i32::try_from(value).map_err(|_| LinkError::Rel32OutOfRange {
                    place: place_rva,
                    target: target_rva,
                })?;
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            Reloc::Abs64 { addend, .. } => {
                let value = i128::from(IMAGE_BASE) + i128::from(target_rva) + i128::from(*addend);
                let value = u64::try_from(value).map_err(|_| LinkError::LayoutOverflow)?;
                bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                dir64.push(place_rva);
            }
        }
    }
    dir64.sort_unstable();
    dir64.dedup();
    Ok(dir64)
}

/// Builds `.reloc`: one block per 4 KiB page, entries in RVA order, each
/// block zero-padded to a multiple of four. Empty input produces an empty
/// table and the writer omits the section and its directory.
pub fn base_relocation_table(dir64: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut page_start = 0usize;
    while page_start < dir64.len() {
        let page = dir64[page_start] & !0xFFF;
        let mut page_end = page_start + 1;
        while page_end < dir64.len() && dir64[page_end] & !0xFFF == page {
            page_end += 1;
        }
        let entries = page_end - page_start;
        let block_size = (8 + entries * 2 + entries % 2 * 2) as u32;
        bytes.extend_from_slice(&page.to_le_bytes());
        bytes.extend_from_slice(&block_size.to_le_bytes());
        for &rva in &dir64[page_start..page_end] {
            let entry = (DIR64 << 12) | (rva & 0xFFF) as u16;
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        if entries % 2 == 1 {
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }
        page_start = page_end;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::base_relocation_table;

    fn entry(offset: u16) -> [u8; 2] {
        ((10 << 12) | offset).to_le_bytes()
    }

    #[test]
    fn one_block_per_page_with_sorted_entries() {
        let bytes = base_relocation_table(&[0x1004, 0x2FF8, 0x3000, 0x3008]);
        let mut expected = Vec::new();
        // Blocks round up to a multiple of four, so a lone entry is padded.
        expected.extend_from_slice(&0x1000u32.to_le_bytes());
        expected.extend_from_slice(&12u32.to_le_bytes());
        expected.extend_from_slice(&entry(0x004));
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&0x2000u32.to_le_bytes());
        expected.extend_from_slice(&12u32.to_le_bytes());
        expected.extend_from_slice(&entry(0xFF8));
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&0x3000u32.to_le_bytes());
        expected.extend_from_slice(&12u32.to_le_bytes());
        expected.extend_from_slice(&entry(0x000));
        expected.extend_from_slice(&entry(0x008));
        assert_eq!(bytes, expected);
    }

    #[test]
    fn empty_table_is_empty() {
        assert!(base_relocation_table(&[]).is_empty());
    }
}

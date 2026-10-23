//! The PE32+ writer: DOS stub, NT headers, optional header, section table
//! and data directories for `IMAGE_FILE_MACHINE_AMD64` (spec sections 2
//! and 13.3). The exception (`.pdata`/`.xdata`) and resource (`.rsrc`)
//! data-directory entries are left empty on purpose: `link/pdata.rs` and
//! `link/rsrc.rs` own them and are separate tasks.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::LinkError;
use super::imports;
use super::objfile::{ObjectFile, Place, SectionId, Subsystem};
use super::reloc::{self, SectionBytes};

/// Preferred load address; ASLR moves the image and `.reloc` restores the
/// absolute addresses.
pub const IMAGE_BASE: u64 = 0x1_4000_0000;
pub const SECTION_ALIGNMENT: u32 = 0x1000;
pub const FILE_ALIGNMENT: u32 = 0x200;

/// `DYNAMIC_BASE | HIGH_ENTROPY_VA | NX_COMPAT | TERMINAL_SERVER_AWARE`.
const DLL_CHARACTERISTICS: u16 = 0x0020 | 0x0040 | 0x0100 | 0x8000;

const MACHINE_AMD64: u16 = 0x8664;
const OPTIONAL_HEADER_MAGIC_PE32PLUS: u16 = 0x20B;
const SUBSYSTEM_GUI: u16 = 2;
const SUBSYSTEM_CONSOLE: u16 = 3;
/// `EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE`.
const FILE_CHARACTERISTICS: u16 = 0x0002 | 0x0020;

const SIZE_OF_OPTIONAL_HEADER: u32 = 240;
const NUMBER_OF_RVA_AND_SIZES: u32 = 16;

const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x0000_0040;
const IMAGE_SCN_MEM_DISCARDABLE: u32 = 0x0200_0000;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

/// Canonical DOS header with `e_lfanew = 0x80` followed by the standard
/// "This program cannot be run in DOS mode" stub.
const DOS_PROGRAM: [u8; 0x80] = *b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff\x00\x00\xb8\x00\x00\x00\x00\x00\x00\x00@\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x00\x00\x00\x0e\x1f\xba\x0e\x00\xb4\x09\xcd!\xb8\x01L\xcd!This program cannot be run in DOS mode.\x0d\x0d\x0a$\x00\x00\x00\x00\x00\x00\x00";

/// `e_lfanew`: the NT headers follow the 0x80-byte DOS program.
const PE_OFFSET: usize = 0x80;

/// Data-directory indices, for the entries this writer fills.
const DIRECTORY_IMPORT: usize = 1;
const DIRECTORY_BASERELOC: usize = 5;
const DIRECTORY_IAT: usize = 12;

/// The linked image plus the facts the tests assert against.
pub struct LinkOutput {
    pub image: Vec<u8>,
    pub layout: Layout,
    /// `(dll, function)` -> RVA of the function's IAT slot.
    pub iat_slots: BTreeMap<(String, String), u32>,
    /// Sorted RVAs of every `Abs64` fixup, in `.reloc` order.
    pub dir64: Vec<u32>,
}

/// Where each section landed in the image.
pub struct Layout {
    pub entry_rva: u32,
    pub text_rva: u32,
    pub rdata_rva: u32,
    pub data_rva: u32,
    /// Where `.bss` starts inside `.data`'s virtual extent.
    pub bss_offset: u32,
    /// `.data`'s virtual size: its file bytes plus `.bss`.
    pub data_vsize: u32,
    pub idata_rva: u32,
    pub idata_vsize: u32,
    /// `(rva, size)` for the IAT data directory; `(0, 0)` without imports.
    pub iat_directory: (u32, u32),
    pub reloc_rva: u32,
    pub reloc_vsize: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
}

impl Layout {
    /// `name -> RVA` for every defined symbol, in sorted order.
    pub fn symbol_rvas(&self, obj: &ObjectFile) -> Result<BTreeMap<String, u32>, LinkError> {
        let mut map = BTreeMap::new();
        for symbol in &obj.symbols {
            let rva = self.place_rva(Place {
                section: symbol.section,
                offset: symbol.offset,
            });
            map.insert(symbol.name.clone(), rva);
        }
        Ok(map)
    }

    /// The RVA of a place: its section base plus offset, with `.bss`
    /// symbols living after `.data`'s own bytes.
    pub fn place_rva(&self, place: Place) -> u32 {
        let base = match place.section {
            SectionId::Text => self.text_rva,
            SectionId::RData => self.rdata_rva,
            SectionId::Data => self.data_rva,
            SectionId::Bss => self.data_rva + self.bss_offset,
        };
        base + place.offset
    }
}

struct SectionOut {
    name: &'static str,
    characteristics: u32,
    /// Virtual size: file bytes, plus `.bss` for `.data`.
    vsize: u32,
    raw: Vec<u8>,
}

/// Links `obj` into a PE32+ image. Deterministic: same input, same bytes,
/// with `TimeDateStamp` the first four bytes of the SHA-256 of the image
/// with that field zeroed (MSVC `/Brepro`, spec section 13.3).
pub fn link(obj: &ObjectFile) -> Result<Vec<u8>, LinkError> {
    Ok(link_image(obj)?.image)
}

/// As [`link`], keeping the layout and import facts for inspection.
pub fn link_image(obj: &ObjectFile) -> Result<LinkOutput, LinkError> {
    obj.validate()?;

    let text_rva = SECTION_ALIGNMENT;
    let rdata_rva = align_up(text_rva + obj.text.bytes.len() as u32, SECTION_ALIGNMENT)?;
    let data_rva = align_up(rdata_rva + obj.rdata.bytes.len() as u32, SECTION_ALIGNMENT)?;
    let bss_offset = align_up(obj.data.bytes.len() as u32, obj.bss.align)?;
    let data_vsize = bss_offset
        .checked_add(obj.bss.size)
        .ok_or(LinkError::LayoutOverflow)?;
    let idata_vsize = imports::size(obj)?;
    let idata_rva = align_up(data_rva + data_vsize, SECTION_ALIGNMENT)?;
    let reloc_rva = align_up(idata_rva + idata_vsize, SECTION_ALIGNMENT)?;

    let entry_offset = obj
        .symbols
        .iter()
        .find(|symbol| symbol.name == obj.entry)
        .map(|symbol| symbol.offset)
        .ok_or_else(|| LinkError::UnknownSymbol(obj.entry.clone()))?;
    let entry_rva = text_rva + entry_offset;

    let mut layout = Layout {
        entry_rva,
        text_rva,
        rdata_rva,
        data_rva,
        bss_offset,
        data_vsize,
        idata_rva,
        idata_vsize,
        iat_directory: (0, 0),
        reloc_rva,
        reloc_vsize: 0,
        size_of_image: 0,
        size_of_headers: 0,
    };

    let import_table = imports::build(obj, idata_rva)?;
    layout.iat_directory = import_table.iat_directory;

    let mut sections = SectionBytes::new(obj);
    let dir64 = reloc::apply(obj, &layout, &import_table, &mut sections)?;
    let reloc_bytes = reloc::base_relocation_table(&dir64);
    layout.reloc_vsize = reloc_bytes.len() as u32;
    layout.size_of_image = align_up(reloc_rva + layout.reloc_vsize, SECTION_ALIGNMENT)?;

    // Empty sections are omitted rather than emitted with zero size and an
    // RVA that would collide with the next section's; `.text` always has
    // content because `validate` requires the entry symbol inside it, and
    // `.data` stays when `.bss` rides in its virtual extent. No symbol or
    // relocation can name an omitted section, so the RVAs assigned above
    // are never observable for them.
    let mut table = vec![SectionOut {
        name: ".text",
        characteristics: IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ,
        vsize: sections.text.len() as u32,
        raw: sections.text,
    }];
    if !sections.rdata.is_empty() {
        table.push(SectionOut {
            name: ".rdata",
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ,
            vsize: sections.rdata.len() as u32,
            raw: sections.rdata,
        });
    }
    if !sections.data.is_empty() || obj.bss.size > 0 {
        table.push(SectionOut {
            name: ".data",
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_MEM_WRITE,
            vsize: data_vsize,
            raw: sections.data,
        });
    }
    if !obj.imports.is_empty() {
        table.push(SectionOut {
            name: ".idata",
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_READ
                | IMAGE_SCN_MEM_WRITE,
            vsize: idata_vsize,
            raw: import_table.bytes,
        });
    }
    if !reloc_bytes.is_empty() {
        table.push(SectionOut {
            name: ".reloc",
            characteristics: IMAGE_SCN_CNT_INITIALIZED_DATA
                | IMAGE_SCN_MEM_DISCARDABLE
                | IMAGE_SCN_MEM_READ,
            vsize: layout.reloc_vsize,
            raw: reloc_bytes,
        });
    }

    let image = write_image(obj, &layout, &table)?;
    Ok(LinkOutput {
        image,
        layout,
        iat_slots: import_table.slots,
        dir64,
    })
}

fn write_image(
    obj: &ObjectFile,
    layout: &Layout,
    table: &[SectionOut],
) -> Result<Vec<u8>, LinkError> {
    let size_of_headers = align_up(
        0x80 + 4 + 20 + SIZE_OF_OPTIONAL_HEADER + 40 * table.len() as u32,
        FILE_ALIGNMENT,
    )?;

    let mut raw_sizes = Vec::with_capacity(table.len());
    let mut pointers = Vec::with_capacity(table.len());
    let mut pointer = size_of_headers;
    for section in table {
        pointers.push(pointer);
        let raw_size = align_up(section.raw.len() as u32, FILE_ALIGNMENT)?;
        raw_sizes.push(raw_size);
        pointer = pointer
            .checked_add(raw_size)
            .ok_or(LinkError::LayoutOverflow)?;
    }
    let file_size = pointer;

    let mut file = vec![0u8; file_size as usize];
    file[..0x80].copy_from_slice(&DOS_PROGRAM);
    file[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");

    // FileHeader; TimeDateStamp stays zero until the image is complete.
    let mut at = PE_OFFSET + 4;
    at = put_u16(&mut file, at, MACHINE_AMD64);
    at = put_u16(&mut file, at, table.len() as u16);
    at += 4; // TimeDateStamp
    at = put_u32(&mut file, at, 0); // PointerToSymbolTable
    at = put_u32(&mut file, at, 0); // NumberOfSymbols
    at = put_u16(&mut file, at, SIZE_OF_OPTIONAL_HEADER as u16);
    at = put_u16(&mut file, at, FILE_CHARACTERISTICS);

    let optional = optional_header(obj, layout, table, &raw_sizes, size_of_headers)?;
    file[at..at + optional.len()].copy_from_slice(&optional);
    at += optional.len();

    for (index, section) in table.iter().enumerate() {
        let name = section.name.as_bytes();
        debug_assert!(name.len() <= 8, "section names are at most 8 bytes");
        file[at..at + name.len()].copy_from_slice(name); // the rest stays zero
        let mut field = at + 8;
        field = put_u32(&mut file, field, section.vsize);
        field = put_u32(&mut file, field, section_rva(layout, section.name));
        field = put_u32(&mut file, field, raw_sizes[index]);
        field = put_u32(&mut file, field, pointers[index]);
        field = put_u32(&mut file, field, 0); // PointerToRelocations
        field = put_u32(&mut file, field, 0); // PointerToLinenumbers
        field = put_u16(&mut file, field, 0); // NumberOfRelocations
        field = put_u16(&mut file, field, 0); // NumberOfLinenumbers
        put_u32(&mut file, field, section.characteristics);
        at += 40;
    }

    let mut pointer = size_of_headers;
    for (index, section) in table.iter().enumerate() {
        file[pointer as usize..pointer as usize + section.raw.len()].copy_from_slice(&section.raw);
        pointer += raw_sizes[index];
    }

    // MSVC /Brepro: hash the finished image with the field zeroed, then
    // store the first four digest bytes (spec section 13.3).
    let stamp = u32::from_be_bytes(Sha256::digest(&file)[..4].try_into().unwrap());
    put_u32(&mut file, PE_OFFSET + 8, stamp);

    Ok(file)
}

fn section_rva(layout: &Layout, name: &str) -> u32 {
    match name {
        ".text" => layout.text_rva,
        ".rdata" => layout.rdata_rva,
        ".data" => layout.data_rva,
        ".idata" => layout.idata_rva,
        ".reloc" => layout.reloc_rva,
        _ => unreachable!("the writer only places the five known sections"),
    }
}

fn optional_header(
    obj: &ObjectFile,
    layout: &Layout,
    table: &[SectionOut],
    raw_sizes: &[u32],
    size_of_headers: u32,
) -> Result<Vec<u8>, LinkError> {
    let raw_of = |name: &str| -> u32 {
        table
            .iter()
            .zip(raw_sizes)
            .find(|(section, _)| section.name == name)
            .map(|(_, raw_size)| *raw_size)
            .unwrap_or(0)
    };
    let size_of_code = raw_of(".text");
    let size_of_initialized_data =
        raw_of(".rdata") + raw_of(".data") + raw_of(".idata") + raw_of(".reloc");

    let mut oh: Vec<u8> = Vec::with_capacity(SIZE_OF_OPTIONAL_HEADER as usize);
    push_u16(&mut oh, OPTIONAL_HEADER_MAGIC_PE32PLUS);
    oh.push(0); // MajorLinkerVersion
    oh.push(0); // MinorLinkerVersion
    push_u32(&mut oh, size_of_code);
    push_u32(&mut oh, size_of_initialized_data);
    push_u32(&mut oh, obj.bss.size);
    push_u32(&mut oh, layout.entry_rva);
    push_u32(&mut oh, layout.text_rva); // BaseOfCode
    push_u64(&mut oh, IMAGE_BASE);
    push_u32(&mut oh, SECTION_ALIGNMENT);
    push_u32(&mut oh, FILE_ALIGNMENT);
    push_u16(&mut oh, 6); // MajorOperatingSystemVersion
    push_u16(&mut oh, 0);
    push_u16(&mut oh, 0); // image version
    push_u16(&mut oh, 0);
    push_u16(&mut oh, 6); // MajorSubsystemVersion
    push_u16(&mut oh, 0);
    push_u32(&mut oh, 0); // Win32VersionValue
    push_u32(&mut oh, layout.size_of_image);
    push_u32(&mut oh, size_of_headers);
    push_u32(&mut oh, 0); // CheckSum: the spec pins zero
    push_u16(
        &mut oh,
        match obj.subsystem {
            Subsystem::Console => SUBSYSTEM_CONSOLE,
            Subsystem::Gui => SUBSYSTEM_GUI,
        },
    );
    push_u16(&mut oh, DLL_CHARACTERISTICS);
    push_u64(&mut oh, obj.stack_reserve());
    push_u64(&mut oh, 0x1000); // SizeOfStackCommit
    push_u64(&mut oh, 0x100000); // SizeOfHeapReserve
    push_u64(&mut oh, 0x1000); // SizeOfHeapCommit
    push_u32(&mut oh, 0); // LoaderFlags
    push_u32(&mut oh, NUMBER_OF_RVA_AND_SIZES);

    let import = if obj.imports.is_empty() {
        (0, 0)
    } else {
        (layout.idata_rva, layout.idata_vsize)
    };
    let base_reloc = if layout.reloc_vsize == 0 {
        (0, 0)
    } else {
        (layout.reloc_rva, layout.reloc_vsize)
    };
    for index in 0..NUMBER_OF_RVA_AND_SIZES as usize {
        // The exception and resource directories stay empty; `pdata.rs`
        // and `rsrc.rs` own them.
        let (rva, size) = match index {
            DIRECTORY_IMPORT => import,
            DIRECTORY_BASERELOC => base_reloc,
            DIRECTORY_IAT => layout.iat_directory,
            _ => (0, 0),
        };
        push_u32(&mut oh, rva);
        push_u32(&mut oh, size);
    }
    debug_assert_eq!(oh.len(), SIZE_OF_OPTIONAL_HEADER as usize);
    Ok(oh)
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u16(bytes: &mut [u8], at: usize, value: u16) -> usize {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    at + 2
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) -> usize {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    at + 4
}

/// Rounds `value` up to a multiple of `align`; every alignment here is a
/// fixed power of two or was validated as one.
fn align_up(value: u32, align: u32) -> Result<u32, LinkError> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
        .ok_or(LinkError::LayoutOverflow)
}

#[cfg(test)]
mod tests {
    use super::{IMAGE_BASE, link, link_image};
    use crate::link::objfile::{
        BssSpec, ObjectFile, Place, Reloc, SectionContent, SectionId, Subsystem, Symbol, Target,
    };
    use sha2::{Digest, Sha256};

    fn object() -> ObjectFile {
        ObjectFile {
            text: SectionContent {
                bytes: vec![0xC3],
                align: 16,
            },
            rdata: SectionContent {
                bytes: vec![1; 4],
                align: 8,
            },
            data: SectionContent {
                bytes: vec![0; 8],
                align: 8,
            },
            bss: BssSpec { size: 16, align: 8 },
            symbols: vec![
                Symbol {
                    name: "entry".into(),
                    section: SectionId::Text,
                    offset: 0,
                },
                Symbol {
                    name: "slot".into(),
                    section: SectionId::Data,
                    offset: 0,
                },
                Symbol {
                    name: "msg".into(),
                    section: SectionId::RData,
                    offset: 0,
                },
            ],
            relocs: vec![Reloc::Abs64 {
                at: Place {
                    section: SectionId::Data,
                    offset: 0,
                },
                target: Target::Symbol("msg".into()),
                addend: 0,
            }],
            imports: [("kernel32.dll".to_string(), vec!["ExitProcess".to_string()])]
                .into_iter()
                .collect(),
            entry: "entry".into(),
            subsystem: Subsystem::Console,
            stack_reserve: None,
        }
    }

    #[test]
    fn deterministic_with_brepro_stamp() {
        let first = link(&object()).unwrap();
        let second = link(&object()).unwrap();
        assert_eq!(first, second);

        let linked = link_image(&object()).unwrap();
        let stamp_offset = 0x80 + 4 + 4; // e_lfanew + signature + Machine/NumberOfSections
        let stamp = u32::from_le_bytes(first[stamp_offset..stamp_offset + 4].try_into().unwrap());
        assert_ne!(stamp, 0);

        // MSVC /Brepro: the first four SHA-256 bytes of the image with the
        // field zeroed (spec section 13.3). CheckSum is written as zero, so
        // only the stamp needs clearing.
        let mut zeroed = first.clone();
        zeroed[stamp_offset..stamp_offset + 4].copy_from_slice(&0u32.to_le_bytes());
        let digest = Sha256::digest(&zeroed);
        assert_eq!(stamp, u32::from_be_bytes(digest[..4].try_into().unwrap()));

        // Headers the writer pins.
        assert_eq!(&first[stamp_offset - 8..stamp_offset - 4], b"PE\0\0");
        assert_eq!(
            u16::from_le_bytes(first[0x84..0x86].try_into().unwrap()),
            0x8664
        );
        assert_eq!(
            u16::from_le_bytes(first[0x98..0x9A].try_into().unwrap()),
            0x20B
        );

        // The Abs64 fixup holds the static address of `msg`: headers take
        // 0x400 and `.text`/`.rdata` one raw page each, so `.data`'s bytes
        // start at file offset 0x800.
        let slot = &first[0x800..0x808];
        let expected = IMAGE_BASE + u64::from(linked.layout.rdata_rva);
        assert_eq!(u64::from_le_bytes(slot.try_into().unwrap()), expected);
    }

    /// Returns `(name, VirtualSize, VirtualAddress, SizeOfRawData)` for each
    /// section header of the image, in table order.
    fn section_headers(image: &[u8]) -> Vec<(String, u32, u32, u32)> {
        let n = u16::from_le_bytes(image[0x86..0x88].try_into().unwrap()) as usize;
        let mut headers = Vec::with_capacity(n);
        for index in 0..n {
            let at = 0x188 + index * 40;
            let name = String::from_utf8_lossy(&image[at..at + 8])
                .trim_end_matches('\0')
                .to_string();
            headers.push((
                name,
                u32::from_le_bytes(image[at + 8..at + 12].try_into().unwrap()),
                u32::from_le_bytes(image[at + 12..at + 16].try_into().unwrap()),
                u32::from_le_bytes(image[at + 16..at + 20].try_into().unwrap()),
            ));
        }
        headers
    }

    fn assert_no_empty_or_overlapping_sections(headers: &[(String, u32, u32, u32)]) {
        for (name, vsize, rva, _raw_size) in headers {
            assert!(vsize > &0, "{name} has VirtualSize 0");
            assert!(rva > &0, "{name} has RVA 0");
        }
        for pair in headers.windows(2) {
            let (_, prev_size, prev_rva, _) = pair[0];
            let (next_name, _, next_rva, _) = &pair[1];
            assert!(
                *next_rva >= prev_rva + prev_size,
                "{next_name} at 0x{next_rva:x} overlaps the previous section"
            );
        }
    }

    #[test]
    fn empty_sections_are_omitted() {
        // No `.rdata` content and no `.data` bytes or `.bss`: only `.text`
        // and `.idata` may appear, with no zero-size or overlapping entries.
        let mut obj = object();
        obj.rdata = SectionContent {
            bytes: vec![],
            align: 8,
        };
        obj.data = SectionContent {
            bytes: vec![],
            align: 8,
        };
        obj.bss = BssSpec { size: 0, align: 8 };
        obj.symbols
            .retain(|symbol| symbol.name != "msg" && symbol.name != "slot");
        obj.relocs.clear();

        let linked = link_image(&obj).unwrap();
        let headers = section_headers(&linked.image);
        assert_eq!(
            headers
                .iter()
                .map(|(name, ..)| name.as_str())
                .collect::<Vec<_>>(),
            [".text", ".idata"]
        );
        assert_no_empty_or_overlapping_sections(&headers);
    }

    #[test]
    fn bss_only_data_keeps_one_data_section() {
        // No `.data` bytes but a `.bss` riding in `.data`'s virtual extent:
        // the section stays with VirtualSize covering `.bss` and no raw size.
        let mut obj = object();
        obj.rdata = SectionContent {
            bytes: vec![],
            align: 8,
        };
        obj.data = SectionContent {
            bytes: vec![],
            align: 8,
        };
        obj.symbols
            .retain(|symbol| symbol.name != "msg" && symbol.name != "slot");
        obj.relocs.clear();

        let linked = link_image(&obj).unwrap();
        let headers = section_headers(&linked.image);
        assert_eq!(
            headers
                .iter()
                .map(|(name, ..)| name.as_str())
                .collect::<Vec<_>>(),
            [".text", ".data", ".idata"]
        );
        assert_eq!(headers[1], (".data".to_string(), 16, 0x2000, 0));
        assert_no_empty_or_overlapping_sections(&headers);
    }
}

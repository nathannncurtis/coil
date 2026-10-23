//! The `.rsrc` section: `RT_GROUP_ICON`/`RT_ICON` from the `.ico`,
//! `RT_VERSION` from the versioninfo dict, `RT_MANIFEST` from the launcher
//! manifest (spec section 13.3).
//!
//! The bytes mirror what the Windows-side writers in
//! `src/coil/platforms/windows.py` put into an exe: the `VS_VERSIONINFO`
//! layout is `set_version_info`'s (a Python test asserts byte identity)
//! and the icon group is `set_exe_icon`'s `GRPICONDIR`, so a linked image
//! carries the same resources `UpdateResource` would have written.

use std::fmt;

/// Resource type numbers; the directory levels sort by ID ascending.
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_VERSION: u32 = 16;
const RT_MANIFEST: u32 = 24;

/// `set_exe_icon` writes icons language-neutral and the manifest follows;
/// `set_version_info` must land on 0x0409 so `UpdateResource` replaces
/// python.exe's shipped VS_VERSION_INFO rather than adding a second one.
const LANG_NEUTRAL: u32 = 0;
const LANG_EN_US: u32 = 0x0409;

/// High bit of a directory entry's offset field: the target is another
/// directory, not a data entry.
const SUBDIRECTORY: u32 = 0x8000_0000;

/// Why a resource section could not be built; the message names the
/// offending input part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RsrcError {
    IconTooSmall,
    NotAnIcon { dir_type: u16 },
    IconHasNoImages,
    TruncatedIconDirectory { index: usize },
    IconImageOutOfBounds { index: usize },
    VersionFieldTooLong { what: &'static str },
    VersionPartOutOfRange { what: &'static str, value: i128 },
    SectionTooLarge,
}

impl fmt::Display for RsrcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RsrcError::IconTooSmall => {
                write!(f, "the .ico file is smaller than its 6-byte header")
            }
            RsrcError::NotAnIcon { dir_type } => {
                write!(f, "the .ico directory type is {dir_type}, not 1 (icon)")
            }
            RsrcError::IconHasNoImages => write!(f, "the .ico directory lists no images"),
            RsrcError::TruncatedIconDirectory { index } => {
                write!(f, "the .ico directory entry {index} is cut off")
            }
            RsrcError::IconImageOutOfBounds { index } => {
                write!(f, "the .ico image {index} lies outside the file")
            }
            RsrcError::VersionFieldTooLong { what } => {
                write!(f, "{what} does not fit the 16-bit version-info lengths")
            }
            RsrcError::VersionPartOutOfRange { what, value } => {
                write!(
                    f,
                    "{what} packs to a version value of {value}, which the 32-bit fixed version fields cannot hold"
                )
            }
            RsrcError::SectionTooLarge => write!(f, "the resource section grew past 4 GiB"),
        }
    }
}

impl std::error::Error for RsrcError {}

/// The version fields `set_version_info` takes, with its defaults already
/// applied: the product name for a missing description or internal name,
/// the output file's name for a missing original filename, 1.0.0.0 for
/// missing versions and empty strings for the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionInfo {
    pub product_name: String,
    pub file_description: String,
    pub file_version: String,
    pub product_version: String,
    pub company_name: String,
    pub legal_copyright: String,
    pub internal_name: String,
    pub original_filename: String,
    pub comments: String,
}

/// The resources a linked image carries; every field is optional and one
/// with all of them `None` builds no section at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct Resources<'a> {
    /// Raw `.ico` file bytes: every image becomes an `RT_ICON` resource
    /// (IDs 1..count) and one `RT_GROUP_ICON` with ID 1 references them.
    pub icon: Option<&'a [u8]>,
    /// Version fields for the `RT_VERSION` resource (ID 1).
    pub version: Option<&'a VersionInfo>,
    /// Manifest XML for the `RT_MANIFEST` resource (ID 1).
    pub manifest: Option<&'a [u8]>,
}

/// Builds the whole `.rsrc` section: the resource directory tree, the data
/// entries and the resource bytes. `section_rva` is the RVA the section
/// gets in the linked image; the data entries' `OffsetToData` fields hold
/// absolute RVAs, while every directory-level offset is relative to the
/// section start. Directory levels sort by ID ascending, so the types come
/// out as `RT_ICON`, `RT_GROUP_ICON`, `RT_VERSION`, `RT_MANIFEST`.
pub fn build(resources: &Resources<'_>, section_rva: u32) -> Result<Vec<u8>, RsrcError> {
    let mut leaves: Vec<(u32, u32, u32, Vec<u8>)> = Vec::new();
    if let Some(ico) = resources.icon {
        let (entries, group) = parse_icon(ico)?;
        for (index, entry) in entries.iter().enumerate() {
            leaves.push((RT_ICON, index as u32 + 1, LANG_NEUTRAL, entry.image.clone()));
        }
        leaves.push((RT_GROUP_ICON, 1, LANG_NEUTRAL, group));
    }
    if let Some(info) = resources.version {
        leaves.push((RT_VERSION, 1, LANG_EN_US, vs_version_info(info)?));
    }
    if let Some(xml) = resources.manifest {
        leaves.push((RT_MANIFEST, 1, LANG_NEUTRAL, xml.to_vec()));
    }
    if leaves.is_empty() {
        return Ok(Vec::new());
    }
    leaves.sort_by_key(|leaf| (leaf.0, leaf.1, leaf.2));

    // One level-1 entry per type; levels 2 and 3 hold one entry per
    // resource ID and per language. All offsets fit u32 because the whole
    // section must.
    let mut types: Vec<(u32, usize, usize)> = Vec::new(); // (type id, first leaf, leaf count)
    for (index, leaf) in leaves.iter().enumerate() {
        match types.last_mut() {
            Some((id, _, count)) if *id == leaf.0 => *count += 1,
            _ => types.push((leaf.0, index, 1)),
        }
    }

    let mut offset = 16 + 8 * types.len() as u32; // root directory + entries
    let mut type_dir_offset = Vec::with_capacity(types.len());
    for &(_, _, count) in &types {
        type_dir_offset.push(offset);
        offset = advance(offset, 16 + 8 * count as u32)?;
    }
    let mut lang_dir_offset = Vec::with_capacity(leaves.len());
    for _ in &leaves {
        lang_dir_offset.push(offset);
        offset = advance(offset, 16 + 8)?;
    }
    let mut data_entry_offset = Vec::with_capacity(leaves.len());
    for _ in &leaves {
        data_entry_offset.push(offset);
        offset = advance(offset, 16)?;
    }
    let mut blob_offset = Vec::with_capacity(leaves.len());
    for leaf in &leaves {
        blob_offset.push(offset);
        offset = align4(advance(offset, leaf.3.len() as u32)?)?;
    }

    let mut out = vec![0u8; offset as usize];
    // IMAGE_RESOURCE_DIRECTORY: everything but the entry counts stays zero.
    put_u16(&mut out, 14, types.len() as u16);
    let mut at = 16usize;
    for (type_index, &(type_id, _, _)) in types.iter().enumerate() {
        put_u32(&mut out, at, type_id);
        put_u32(&mut out, at + 4, SUBDIRECTORY | type_dir_offset[type_index]);
        at += 8;
    }
    for (type_index, &(_, first, count)) in types.iter().enumerate() {
        let dir = type_dir_offset[type_index] as usize;
        put_u16(&mut out, dir + 14, count as u16);
        let mut at = dir + 16;
        for leaf_index in first..first + count {
            put_u32(&mut out, at, leaves[leaf_index].1); // resource ID
            put_u32(&mut out, at + 4, SUBDIRECTORY | lang_dir_offset[leaf_index]);
            at += 8;
        }
    }
    for (leaf_index, leaf) in leaves.iter().enumerate() {
        let dir = lang_dir_offset[leaf_index] as usize;
        put_u16(&mut out, dir + 14, 1);
        put_u32(&mut out, dir + 16, leaf.2); // language
        put_u32(&mut out, dir + 20, data_entry_offset[leaf_index]);
    }
    // IMAGE_RESOURCE_DATA_ENTRY: absolute RVA and byte count, code page and
    // reserved zero.
    for (leaf_index, leaf) in leaves.iter().enumerate() {
        let entry = data_entry_offset[leaf_index] as usize;
        let data_rva = section_rva
            .checked_add(blob_offset[leaf_index])
            .ok_or(RsrcError::SectionTooLarge)?;
        put_u32(&mut out, entry, data_rva);
        put_u32(&mut out, entry + 4, leaf.3.len() as u32);
    }
    for (leaf_index, leaf) in leaves.iter().enumerate() {
        let at = blob_offset[leaf_index] as usize;
        out[at..at + leaf.3.len()].copy_from_slice(&leaf.3);
    }
    Ok(out)
}

/// One `ICONDIRENTRY` plus the raw image bytes it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IconEntry {
    width: u8,
    height: u8,
    colors: u8,
    reserved: u8,
    planes: u16,
    bit_count: u16,
    image: Vec<u8>,
}

/// Splits an `.ico` into its images and the `GRPICONDIR` group resource
/// that references them, byte-for-byte what `set_exe_icon` builds: a
/// 6-byte header and one 14-byte entry per image whose resource IDs are
/// 1..count. Unlike `set_exe_icon`'s slice, an image that would reach past
/// the end of the file is an error rather than a silent truncation.
fn parse_icon(ico: &[u8]) -> Result<(Vec<IconEntry>, Vec<u8>), RsrcError> {
    if ico.len() < 6 {
        return Err(RsrcError::IconTooSmall);
    }
    let dir_type = u16::from_le_bytes(ico[2..4].try_into().unwrap());
    let count = u16::from_le_bytes(ico[4..6].try_into().unwrap());
    if dir_type != 1 {
        return Err(RsrcError::NotAnIcon { dir_type });
    }
    if count == 0 {
        return Err(RsrcError::IconHasNoImages);
    }
    let mut entries = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let at = 6 + index * 16;
        if at + 16 > ico.len() {
            return Err(RsrcError::TruncatedIconDirectory { index });
        }
        let image_offset = u32::from_le_bytes(ico[at + 12..at + 16].try_into().unwrap());
        let image_size = u32::from_le_bytes(ico[at + 8..at + 12].try_into().unwrap());
        if u64::from(image_offset) + u64::from(image_size) > ico.len() as u64 {
            return Err(RsrcError::IconImageOutOfBounds { index });
        }
        entries.push(IconEntry {
            width: ico[at],
            height: ico[at + 1],
            colors: ico[at + 2],
            reserved: ico[at + 3],
            planes: u16::from_le_bytes(ico[at + 4..at + 6].try_into().unwrap()),
            bit_count: u16::from_le_bytes(ico[at + 6..at + 8].try_into().unwrap()),
            image: ico[image_offset as usize..(image_offset + image_size) as usize].to_vec(),
        });
    }

    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&count.to_le_bytes());
    for (index, entry) in entries.iter().enumerate() {
        let id = index as u16 + 1;
        group.extend_from_slice(&[entry.width, entry.height, entry.colors, entry.reserved]);
        group.extend_from_slice(&entry.planes.to_le_bytes());
        group.extend_from_slice(&entry.bit_count.to_le_bytes());
        group.extend_from_slice(&(entry.image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    Ok((entries, group))
}

/// Builds the `VS_VERSIONINFO` resource bytes exactly as
/// `set_version_info` does, quirks included: a string entry's
/// `wValueLength` counts code points plus one, not UTF-16 units, and
/// empty company, copyright and comments fields are left out of the string
/// table altogether. The string order is the reference writer's.
pub fn vs_version_info(info: &VersionInfo) -> Result<Vec<u8>, RsrcError> {
    let mut strings: Vec<(&str, &str)> = vec![
        ("FileDescription", info.file_description.as_str()),
        ("FileVersion", info.file_version.as_str()),
        ("InternalName", info.internal_name.as_str()),
        ("ProductName", info.product_name.as_str()),
        ("ProductVersion", info.product_version.as_str()),
        ("OriginalFilename", info.original_filename.as_str()),
    ];
    if !info.company_name.is_empty() {
        strings.push(("CompanyName", info.company_name.as_str()));
    }
    if !info.legal_copyright.is_empty() {
        strings.push(("LegalCopyright", info.legal_copyright.as_str()));
    }
    if !info.comments.is_empty() {
        strings.push(("Comments", info.comments.as_str()));
    }

    // StringFileInfo > StringTable 040904B0 (US English, Unicode).
    let mut table = header(0, 1);
    table.extend_from_slice(&utf16_nul_terminated("040904B0"));
    pad4(&mut table);
    for (key, value) in &strings {
        table.extend_from_slice(&string_entry(key, value)?);
    }
    let table = sized(table, "the version string table")?;

    let mut string_file_info = header(0, 1);
    string_file_info.extend_from_slice(&utf16_nul_terminated("StringFileInfo"));
    pad4(&mut string_file_info);
    string_file_info.extend_from_slice(&table);
    let string_file_info = sized(string_file_info, "the StringFileInfo block")?;

    // VarFileInfo > Var: Translation = US English, Unicode.
    let translation = 0x0409u16
        .to_le_bytes()
        .into_iter()
        .chain(0x04B0u16.to_le_bytes())
        .collect::<Vec<u8>>();
    let mut var = header(translation.len() as u16 / 2, 0);
    var.extend_from_slice(&utf16_nul_terminated("Translation"));
    pad4(&mut var);
    var.extend_from_slice(&translation);
    let var = sized(var, "the Translation var")?;

    let mut var_file_info = header(0, 1);
    var_file_info.extend_from_slice(&utf16_nul_terminated("VarFileInfo"));
    pad4(&mut var_file_info);
    var_file_info.extend_from_slice(&var);
    let var_file_info = sized(var_file_info, "the VarFileInfo block")?;

    let fv = version_dwords(&parse_version(info.file_version.as_str()), "file_version")?;
    let pv = version_dwords(
        &parse_version(info.product_version.as_str()),
        "product_version",
    )?;
    let fixed: Vec<u8> = [
        0xFEEF_04BD, // dwSignature
        0x0001_0000, // dwStrucVersion
        fv[0],       // dwFileVersionMS
        fv[1],       // dwFileVersionLS
        pv[0],       // dwProductVersionMS
        pv[1],       // dwProductVersionLS
        0x3F,        // dwFileFlagsMask
        0,           // dwFileFlags
        0x0004_0004, // dwFileOS = VOS_NT_WINDOWS32
        1,           // dwFileType = VFT_APP
        0,           // dwFileSubtype
        0,           // dwFileDateMS
        0,           // dwFileDateLS
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();

    let mut root = header(fixed.len() as u16, 0);
    root.extend_from_slice(&utf16_nul_terminated("VS_VERSION_INFO"));
    pad4(&mut root);
    root.extend_from_slice(&fixed);
    pad4(&mut root);
    root.extend_from_slice(&string_file_info);
    pad4(&mut root);
    root.extend_from_slice(&var_file_info);
    sized(root, "the VS_VERSION_INFO block")
}

/// The decimal-digit blocks of Unicode 15.0, the table the Python 3.12
/// `int()` reads: every script's numerals are ten consecutive digits 0-9,
/// so each start alone identifies its block. Mathematical digits repeat
/// the trick five times in a row.
const DIGIT_BLOCKS: [u32; 68] = [
    0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80,
    0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0,
    0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60,
    0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0,
    0x1E950, 0x1FBF0,
];

/// The value of a Unicode decimal digit, `None` for anything else (so
/// superscripts and fractions, which `int()` also rejects, miss).
fn decimal_digit(c: char) -> Option<u32> {
    let code = c as u32;
    let block = DIGIT_BLOCKS
        .partition_point(|&start| start <= code)
        .checked_sub(1)?;
    let offset = code - DIGIT_BLOCKS[block];
    (offset < 10).then_some(offset)
}

/// Python's `int()` on one dot-separated part: surrounding Unicode
/// whitespace and a sign are allowed, digits may come from any decimal
/// block, and underscores may separate digits. `None` is the `ValueError`
/// that `set_version_info` turns into a zero part. The value saturates
/// where Python would grow past `i64`; such parts cannot be packed anyway.
fn python_int(part: &str) -> Option<i64> {
    let text = part.trim();
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    let mut value: i64 = 0;
    let mut digits = 0;
    let mut underscore_pending = false;
    for c in rest.chars() {
        if c == '_' {
            // Underscores separate digits: never leading, doubled or final.
            if digits == 0 || underscore_pending {
                return None;
            }
            underscore_pending = true;
            continue;
        }
        let digit = decimal_digit(c)?;
        value = value.saturating_mul(10).saturating_add(i64::from(digit));
        digits += 1;
        underscore_pending = false;
    }
    if digits == 0 || underscore_pending {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `set_version_info`'s version parsing: `-` reads as `.`, at most four
/// parts are kept, and a part `int()` rejects is zero.
fn parse_version(value: &str) -> [i64; 4] {
    let mut parts = [0i64; 4];
    for (index, part) in value.replace('-', ".").split('.').take(4).enumerate() {
        parts[index] = python_int(part).unwrap_or(0);
    }
    parts
}

/// The two DWORDs the four parts of one version string pack into. The
/// reference combines each pair with `(hi << 16) | lo` — Python's bitwise
/// OR, not a hiword/loword placement — and `struct.pack('<I', ...)` lets
/// its error escape when the result is negative or over 0xFFFFFFFF, so
/// those inputs are refused here too.
fn version_dwords(parts: &[i64; 4], what: &'static str) -> Result<[u32; 2], RsrcError> {
    let combine = |hi: i64, lo: i64| -> Result<u32, RsrcError> {
        let combined: i128 = (i128::from(hi) << 16) | i128::from(lo);
        u32::try_from(combined).map_err(|_| RsrcError::VersionPartOutOfRange {
            what,
            value: combined,
        })
    };
    Ok([combine(parts[0], parts[1])?, combine(parts[2], parts[3])?])
}

/// The `wLength`/`wValueLength`/`wType` header every version structure
/// starts with; `wLength` is a placeholder until [`sized`].
fn header(value_length: u16, value_type: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&value_length.to_le_bytes());
    out.extend_from_slice(&value_type.to_le_bytes());
    out
}

/// A NUL-terminated UTF-16 string, the `Key`/`Value` encoding throughout
/// the version resource.
fn utf16_nul_terminated(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(u16::to_le_bytes)
        .chain(0u16.to_le_bytes())
        .collect()
}

fn pad4(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// Patches `wLength` to the whole structure's byte count.
fn sized(mut bytes: Vec<u8>, what: &'static str) -> Result<Vec<u8>, RsrcError> {
    let len = u16::try_from(bytes.len()).map_err(|_| RsrcError::VersionFieldTooLong { what })?;
    put_u16(&mut bytes, 0, len);
    Ok(bytes)
}

/// One `String` child of the string table.
fn string_entry(key: &str, value: &str) -> Result<Vec<u8>, RsrcError> {
    let value_length = value.chars().count() + 1; // WCHARs, NUL included
    let value_length = u16::try_from(value_length).map_err(|_| RsrcError::VersionFieldTooLong {
        what: "a version string value",
    })?;
    let mut out = header(value_length, 1);
    out.extend_from_slice(&utf16_nul_terminated(key));
    pad4(&mut out);
    out.extend_from_slice(&utf16_nul_terminated(value));
    pad4(&mut out);
    sized(out, "a version string entry")
}

fn advance(offset: u32, len: u32) -> Result<u32, RsrcError> {
    offset.checked_add(len).ok_or(RsrcError::SectionTooLarge)
}

fn align4(offset: u32) -> Result<u32, RsrcError> {
    offset
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or(RsrcError::SectionTooLarge)
}

fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::{
        LANG_EN_US, LANG_NEUTRAL, RT_GROUP_ICON, RT_ICON, RT_MANIFEST, RT_VERSION, Resources,
        RsrcError, VersionInfo, build, parse_icon, parse_version, python_int, version_dwords,
        vs_version_info,
    };

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn version() -> VersionInfo {
        VersionInfo {
            product_name: "Coil Demo".into(),
            file_description: "Coil Demo Application".into(),
            file_version: "1.2.3.4".into(),
            product_version: "1.2.0.0".into(),
            company_name: "Acme Corp".into(),
            legal_copyright: "Copyright (c) 2026 Acme Corp".into(),
            internal_name: "coil-demo".into(),
            original_filename: "demo.exe".into(),
            comments: "Built by the platform team".into(),
        }
    }

    /// A minimal `.ico`: header, one 16-byte directory entry per image,
    /// then the raw images.
    fn test_ico(images: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for (index, image) in images.iter().enumerate() {
            out.extend_from_slice(&[index as u8 + 1, 2 * (index as u8 + 1), 0, 0]);
            out.extend_from_slice(&1u16.to_le_bytes()); // planes
            out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            out.extend_from_slice(&(image.len() as u32).to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += image.len();
        }
        for image in images {
            out.extend_from_slice(image);
        }
        out
    }

    #[test]
    fn version_parts_follow_set_version_info() {
        assert_eq!(parse_version("1.2.3.4"), [1, 2, 3, 4]);
        assert_eq!(parse_version("6-1.2"), [6, 1, 2, 0]);
        assert_eq!(parse_version("1.0b2.3.4.5"), [1, 0, 3, 4]);
        assert_eq!(parse_version(""), [0, 0, 0, 0]);
        assert_eq!(parse_version("7"), [7, 0, 0, 0]);
    }

    /// The parts go through `int()`, so its whole grammar counts: digits
    /// in any script and underscores between digits parse, everything
    /// else is the `ValueError` the reference reads as a zero part.
    #[test]
    fn version_parts_read_int_like_digits_and_underscores() {
        assert_eq!(parse_version("1_2.3"), [12, 3, 0, 0]);
        assert_eq!(parse_version("1_0_2"), [102, 0, 0, 0]);
        assert_eq!(parse_version(" 1_0 "), [10, 0, 0, 0]);
        assert_eq!(parse_version("+7"), [7, 0, 0, 0]);
        assert_eq!(parse_version("１２.３"), [12, 3, 0, 0]); // fullwidth
        assert_eq!(parse_version("١٢"), [12, 0, 0, 0]); // Arabic-Indic
        assert_eq!(parse_version("𝟏𝟐"), [12, 0, 0, 0]); // mathematical
        assert_eq!(parse_version("1١2"), [112, 0, 0, 0]); // mixed scripts

        assert_eq!(parse_version("1__2"), [0, 0, 0, 0]);
        assert_eq!(parse_version("1_"), [0, 0, 0, 0]);
        assert_eq!(parse_version("_1"), [0, 0, 0, 0]);
        assert_eq!(parse_version("1 2"), [0, 0, 0, 0]);
        assert_eq!(parse_version("2³"), [0, 0, 0, 0]); // superscript
        assert_eq!(parse_version("\u{1c}1"), [0, 0, 0, 0]); // not int() space
    }

    /// `parse_version` never sees a `-` (`set_version_info` turns it into
    /// a separator first), but the `int()` mirror still takes signs.
    #[test]
    fn int_parsing_takes_a_sign_like_python() {
        assert_eq!(python_int("-1_2"), Some(-12));
        assert_eq!(python_int("+1_2"), Some(12));
        assert_eq!(python_int("- 1"), None);
        assert_eq!(python_int(""), None);
        assert_eq!(python_int("+"), None);
        assert_eq!(python_int("-"), None);
    }

    /// The reference packs `(hi << 16) | lo` with Python's bitwise OR, so
    /// only the combined DWORD has to fit; `struct.pack` fails otherwise
    /// and the builder refuses the same inputs.
    #[test]
    fn version_dwords_pack_the_way_struct_pack_does() {
        let what = "file_version";
        assert_eq!(
            version_dwords(&[1, 2, 3, 4], what),
            Ok([(1 << 16) | 2, (3 << 16) | 4])
        );
        // A large low part is fine while the OR still fits 32 bits.
        assert_eq!(version_dwords(&[0, 429821, 0, 0], what), Ok([429821, 0]));
        assert_eq!(
            version_dwords(&[1, 65536, 65535, 65536], what),
            Ok([65536, (65535 << 16) | 65536])
        );
        // One bit past 0xFFFFFFFF, and struct.pack would already fail.
        assert_eq!(
            version_dwords(&[65536, 0, 0, 0], what),
            Err(RsrcError::VersionPartOutOfRange {
                what,
                value: 1 << 32
            })
        );
        assert_eq!(
            version_dwords(&[0, 4294967296, 0, 0], what),
            Err(RsrcError::VersionPartOutOfRange {
                what,
                value: 4294967296
            })
        );
        // Negative parts can only arrive via python_int, never through
        // the '-' in a version string.
        assert_eq!(
            version_dwords(&[-1, 0, 0, 0], what),
            Err(RsrcError::VersionPartOutOfRange {
                what,
                value: -65536
            })
        );
        assert!(matches!(
            version_dwords(&[i64::MAX, 0, 0, 0], "product_version"),
            Err(RsrcError::VersionPartOutOfRange {
                what: "product_version",
                ..
            })
        ));
    }

    #[test]
    fn icon_errors_name_the_bad_part() {
        assert_eq!(parse_icon(&[0; 5]), Err(RsrcError::IconTooSmall));

        let mut ico = test_ico(&[&[0x11; 4]]);
        ico[2..4].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(parse_icon(&ico), Err(RsrcError::NotAnIcon { dir_type: 2 }));

        let mut ico = test_ico(&[&[0x11; 4]]);
        ico[4..6].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_icon(&ico), Err(RsrcError::IconHasNoImages));

        let ico = test_ico(&[&[0x11; 4]]);
        let truncated = &ico[..14]; // header plus half of the entry
        assert_eq!(
            parse_icon(truncated),
            Err(RsrcError::TruncatedIconDirectory { index: 0 })
        );

        let mut ico = test_ico(&[&[0x11; 4]]);
        let past_end = ico.len() as u32 + 1;
        ico[6 + 12..6 + 16].copy_from_slice(&past_end.to_le_bytes());
        assert_eq!(
            parse_icon(&ico),
            Err(RsrcError::IconImageOutOfBounds { index: 0 })
        );
    }

    #[test]
    fn group_icon_references_each_image() {
        let ico = test_ico(&[&[0xAA; 32], &[0xBB; 16]]);
        let (entries, group) = parse_icon(&ico).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].image, vec![0xAA; 32]);
        assert_eq!(entries[1].image, vec![0xBB; 16]);

        let mut expected = Vec::new();
        expected.extend_from_slice(&0u16.to_le_bytes());
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&2u16.to_le_bytes());
        for (index, entry) in entries.iter().enumerate() {
            expected.extend_from_slice(&[entry.width, entry.height, entry.colors, entry.reserved]);
            expected.extend_from_slice(&entry.planes.to_le_bytes());
            expected.extend_from_slice(&entry.bit_count.to_le_bytes());
            expected.extend_from_slice(&(entry.image.len() as u32).to_le_bytes());
            expected.extend_from_slice(&(index as u16 + 1).to_le_bytes());
        }
        assert_eq!(group, expected);
    }

    #[test]
    fn version_info_lengths_patch_up() {
        let bytes = vs_version_info(&version()).unwrap();
        assert_eq!(u16_at(&bytes, 0) as usize, bytes.len());
        // Root header, padded VS_VERSION_INFO key and the 13 fixed DWORDs.
        assert!(bytes.len() > 6 + 32 + 52);
    }

    #[test]
    fn empty_resources_build_nothing() {
        assert_eq!(build(&Resources::default(), 0x3000), Ok(Vec::new()));
    }

    /// Enumerates a built section's leaves as (type, id, lang, bytes) by
    /// walking the directory tree, following subdirectory bits and data
    /// entries exactly as the loader would.
    fn walk(
        bytes: &[u8],
        section_rva: u32,
        at: usize,
        path: &mut Vec<u32>,
        out: &mut Vec<(u32, u32, u32, Vec<u8>)>,
    ) {
        assert_eq!(u16_at(bytes, at + 12), 0); // no named entries anywhere
        let count = u16_at(bytes, at + 14) as usize;
        for index in 0..count {
            let entry = at + 16 + 8 * index;
            path.push(u32_at(bytes, entry));
            let offset = u32_at(bytes, entry + 4);
            if offset & super::SUBDIRECTORY != 0 {
                walk(
                    bytes,
                    section_rva,
                    (offset & !super::SUBDIRECTORY) as usize,
                    path,
                    out,
                );
            } else {
                let data_rva = u32_at(bytes, offset as usize);
                let size = u32_at(bytes, offset as usize + 4) as usize;
                let start = (data_rva - section_rva) as usize;
                out.push((
                    path[0],
                    path[1],
                    path[2],
                    bytes[start..start + size].to_vec(),
                ));
            }
            path.pop();
        }
    }

    #[test]
    fn directory_tree_sorts_and_resolves_the_leaves() {
        let icon = test_ico(&[&[0xAA; 32], &[0xBB; 16]]);
        let info = version();
        let manifest = b"<assembly/>";
        let section_rva = 0x6000;
        let bytes = build(
            &Resources {
                icon: Some(&icon),
                version: Some(&info),
                manifest: Some(manifest),
            },
            section_rva,
        )
        .unwrap();

        let (_, group) = parse_icon(&icon).unwrap();
        let mut expected = vec![
            (RT_ICON, 1, LANG_NEUTRAL, vec![0xAA; 32]),
            (RT_ICON, 2, LANG_NEUTRAL, vec![0xBB; 16]),
            (RT_GROUP_ICON, 1, LANG_NEUTRAL, group),
            (RT_VERSION, 1, LANG_EN_US, vs_version_info(&info).unwrap()),
            (RT_MANIFEST, 1, LANG_NEUTRAL, manifest.to_vec()),
        ];
        expected.sort_by_key(|leaf| (leaf.0, leaf.1, leaf.2));

        let mut leaves = Vec::new();
        walk(&bytes, section_rva, 0, &mut Vec::new(), &mut leaves);
        assert_eq!(leaves, expected);
    }
}

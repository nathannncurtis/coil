//! The import directory, ILT, IAT and hint/name table (spec section 10.1):
//! every imported function is reached through its IAT slot, so a `Rel32` to
//! an import resolves to the slot's address.

use std::collections::BTreeMap;

use super::LinkError;
use super::objfile::ObjectFile;

/// The built `.idata` and where each import's IAT slot landed.
pub struct ImportTable {
    /// The `.idata` section's content; the section is read-write because the
    /// loader rewrites the IAT at load time.
    pub bytes: Vec<u8>,
    /// Size for the import data directory: the whole `.idata` extent.
    pub directory_size: u32,
    /// `(rva, size)` for the IAT data directory, zero when there are no
    /// imports.
    pub iat_directory: (u32, u32),
    /// `(dll, function)` -> RVA of the function's IAT slot.
    pub slots: BTreeMap<(String, String), u32>,
}

/// Where each piece of one DLL's import record goes, as byte offsets from
/// the start of `.idata`.
struct PlanDll {
    name: String,
    original_first_thunk: u32,
    first_thunk: u32,
    name_offset: u32,
    funcs: Vec<PlanFunc>,
}

struct PlanFunc {
    name: String,
    hint_offset: u32,
}

struct Plan {
    size: u32,
    first_thunk: u32,
    thunk_bytes: u32,
    dlls: Vec<PlanDll>,
}

/// The `.idata` byte count [`build`] produces for `obj`; `.reloc` placement
/// needs it before the table itself exists.
pub fn size(obj: &ObjectFile) -> Result<u32, LinkError> {
    Ok(plan(obj)?.size)
}

/// Builds `.idata` for `obj`'s imports at `idata_rva`: descriptors, every
/// ILT, every IAT, DLL name strings, then hint/name entries — DLLs and
/// functions in sorted order, so the bytes depend only on the import set
/// (spec section 13.3).
pub fn build(obj: &ObjectFile, idata_rva: u32) -> Result<ImportTable, LinkError> {
    let plan = plan(obj)?;
    let mut bytes = vec![0u8; plan.size as usize];
    let put32 = |bytes: &mut Vec<u8>, offset: u32, value: u32| {
        bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    };
    let put64 = |bytes: &mut Vec<u8>, offset: u32, value: u64| {
        bytes[offset as usize..offset as usize + 8].copy_from_slice(&value.to_le_bytes());
    };

    let mut slots = BTreeMap::new();
    for (index, dll) in plan.dlls.iter().enumerate() {
        let descriptor = (index as u32) * 20;
        put32(&mut bytes, descriptor, idata_rva + dll.original_first_thunk);
        put32(&mut bytes, descriptor + 12, idata_rva + dll.name_offset);
        put32(&mut bytes, descriptor + 16, idata_rva + dll.first_thunk);
        for (slot, func) in dll.funcs.iter().enumerate() {
            let hint_rva = idata_rva + func.hint_offset;
            let ilt = dll.original_first_thunk + (slot as u32) * 8;
            let iat = dll.first_thunk + (slot as u32) * 8;
            put64(&mut bytes, ilt, u64::from(hint_rva));
            put64(&mut bytes, iat, u64::from(hint_rva));
            let hint = func.hint_offset as usize;
            bytes[hint..hint + 2].copy_from_slice(&0u16.to_le_bytes());
            bytes[hint + 2..hint + 2 + func.name.len()].copy_from_slice(func.name.as_bytes());
            slots.insert(
                (dll.name.clone(), func.name.clone()),
                idata_rva + dll.first_thunk + (slot as u32) * 8,
            );
        }
        let name = dll.name_offset as usize;
        bytes[name..name + dll.name.len()].copy_from_slice(dll.name.as_bytes());
    }

    Ok(ImportTable {
        directory_size: plan.size,
        iat_directory: if plan.dlls.is_empty() {
            (0, 0)
        } else {
            (idata_rva + plan.first_thunk, plan.thunk_bytes)
        },
        bytes,
        slots,
    })
}

fn plan(obj: &ObjectFile) -> Result<Plan, LinkError> {
    let mut sorted_dlls: Vec<(&String, &Vec<String>)> = obj.imports.iter().collect();
    sorted_dlls.sort_by(|a, b| a.0.cmp(b.0));

    let count = |funcs: &[String]| (funcs.len() as u64) + 1; // + null terminator
    let descriptors = ((sorted_dlls.len() as u64) + 1) * 20;
    let thunks: u64 = sorted_dlls.iter().map(|(_, funcs)| count(funcs) * 8).sum();

    // One group at a time so the IATs stay contiguous and the IAT data
    // directory covers them exactly: descriptors, ILTs, IATs, DLL names,
    // hint/name entries.
    let mut offset = descriptors;
    let mut dlls: Vec<PlanDll> = Vec::new();
    for (dll, funcs) in &sorted_dlls {
        let ilt = offset;
        offset += count(funcs) * 8;
        dlls.push(PlanDll {
            name: (*dll).clone(),
            original_first_thunk: ilt as u32,
            first_thunk: 0,
            name_offset: 0,
            funcs: Vec::new(),
        });
    }
    let first_thunk = offset as u32;
    for (dll, (_, funcs)) in dlls.iter_mut().zip(&sorted_dlls) {
        dll.first_thunk = offset as u32;
        offset += count(funcs) * 8;
    }
    for (dll, (_, funcs)) in dlls.iter_mut().zip(&sorted_dlls) {
        dll.name_offset = offset as u32;
        offset += dll.name.len() as u64 + 1;
        let mut sorted: Vec<&String> = funcs.iter().collect();
        sorted.sort();
        dll.funcs = sorted
            .into_iter()
            .map(|name| {
                // Hint/name entries are word-aligned; the pad lands before
                // the entry (the DLL name above may leave odd parity), not
                // after it.
                offset += offset & 1;
                let hint_offset = offset;
                offset += 2 + name.len() as u64 + 1;
                PlanFunc {
                    name: (*name).clone(),
                    hint_offset: hint_offset as u32,
                }
            })
            .collect();
    }

    Ok(Plan {
        size: u32::try_from(offset).map_err(|_| LinkError::LayoutOverflow)?,
        first_thunk,
        thunk_bytes: u32::try_from(thunks).map_err(|_| LinkError::LayoutOverflow)?,
        dlls,
    })
}

#[cfg(test)]
mod tests {
    use super::{build, size};
    use crate::link::objfile::{BssSpec, ObjectFile, SectionContent, SectionId, Subsystem, Symbol};

    fn object(dlls: &[(&str, &[&str])]) -> ObjectFile {
        ObjectFile {
            text: SectionContent {
                bytes: vec![0xC3],
                align: 16,
            },
            rdata: SectionContent {
                bytes: Vec::new(),
                align: 8,
            },
            data: SectionContent {
                bytes: Vec::new(),
                align: 8,
            },
            bss: BssSpec::default(),
            symbols: vec![Symbol {
                name: "entry".into(),
                section: SectionId::Text,
                offset: 0,
            }],
            relocs: Vec::new(),
            imports: dlls
                .iter()
                .map(|(dll, funcs)| {
                    (
                        (*dll).to_string(),
                        funcs.iter().map(|func| (*func).to_string()).collect(),
                    )
                })
                .collect(),
            entry: "entry".into(),
            subsystem: Subsystem::Console,
            stack_reserve: None,
        }
    }

    #[test]
    fn two_dlls_sorted_with_contiguous_iat() {
        let obj = object(&[
            ("user32.dll", &["GetDesktopWindow"]),
            ("kernel32.dll", &["ExitProcess", "GetCurrentProcessId"]),
        ]);
        let idata_rva = 0x4000;
        let table = build(&obj, idata_rva).unwrap();
        assert_eq!(table.bytes.len() as u32, size(&obj).unwrap());

        // Three descriptors (two DLLs plus the terminator) come first, then
        // kernel32's two-entry ILT, then user32's one-entry ILT, then the
        // IATs; DLLs are sorted by name, kernel32 first.
        let kernel32_ilt: usize = 3 * 20;
        let user32_ilt: usize = kernel32_ilt + 3 * 8;
        let kernel32_iat: usize = user32_ilt + 2 * 8;
        let user32_iat: usize = kernel32_iat + 3 * 8;
        let read32 =
            |offset: usize| u32::from_le_bytes(table.bytes[offset..offset + 4].try_into().unwrap());
        let read64 =
            |offset: usize| u64::from_le_bytes(table.bytes[offset..offset + 8].try_into().unwrap());
        assert_eq!(read32(0), idata_rva + kernel32_ilt as u32); // OriginalFirstThunk
        assert_eq!(read32(16), idata_rva + kernel32_iat as u32); // FirstThunk
        assert_eq!(read32(20) - idata_rva, user32_ilt as u32);
        assert_eq!(read32(20 + 16) - idata_rva, user32_iat as u32);
        assert_eq!(read64(kernel32_ilt), read64(kernel32_iat)); // ILT and IAT agree
        assert_eq!(read64(kernel32_ilt) >> 63, 0); // by name, no ordinal bit
        assert_eq!(read64(kernel32_ilt + 16), 0); // null thunk after two entries
        assert_eq!(read64(kernel32_iat + 16), 0);

        let (iat_rva, iat_size) = table.iat_directory;
        assert_eq!(iat_rva, idata_rva + kernel32_iat as u32);
        assert_eq!(iat_size, 3 * 8 + 2 * 8);
        for rva in table.slots.values() {
            assert!(*rva >= iat_rva && *rva < iat_rva + iat_size);
        }
        assert_eq!(
            table.slots[&("kernel32.dll".to_string(), "ExitProcess".to_string())],
            iat_rva // the first kernel32 slot
        );
    }

    #[test]
    fn hint_name_entries_are_word_aligned() {
        // Both DLL names ("kernel32.dll\0" is 13 bytes, "user32.dll\0"
        // is 11) leave odd parity behind them, so an alignment done after
        // an entry instead of before it would place the first hint/name
        // entry of each DLL at an odd RVA.
        let obj = object(&[
            ("user32.dll", &["GetDesktopWindow"]),
            ("kernel32.dll", &["ExitProcess", "GetCurrentProcessId"]),
        ]);
        let idata_rva = 0x4000;
        let table = build(&obj, idata_rva).unwrap();
        assert_eq!(idata_rva % 2, 0); // parity is relative to the table

        let mut descriptor = 0;
        loop {
            let oft =
                u32::from_le_bytes(table.bytes[descriptor..descriptor + 4].try_into().unwrap());
            if oft == 0 {
                break; // the null terminator descriptor
            }
            let name = u32::from_le_bytes(
                table.bytes[descriptor + 12..descriptor + 16]
                    .try_into()
                    .unwrap(),
            ) - idata_rva;
            let end = table.bytes[name as usize..]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap() as u32
                + name
                + 1;
            let mut thunk = (oft - idata_rva) as usize;
            loop {
                let entry = u64::from_le_bytes(table.bytes[thunk..thunk + 8].try_into().unwrap());
                if entry == 0 {
                    break;
                }
                assert_eq!(entry >> 63, 0); // by name
                let hint = u32::try_from(entry).unwrap() - idata_rva;
                assert_eq!(hint % 2, 0, "hint/name entry at odd RVA");
                assert!(hint as usize > end as usize, "entry inside the DLL name");
                assert_eq!(&table.bytes[hint as usize..hint as usize + 2], &[0, 0]);
                thunk += 8;
            }
            descriptor += 20;
        }
    }

    #[test]
    fn no_imports_leave_directories_empty() {
        let obj = object(&[]);
        let table = build(&obj, 0x4000).unwrap();
        assert_eq!(table.iat_directory, (0, 0));
        assert!(table.slots.is_empty());
        assert_eq!(table.bytes.len(), 20); // the null descriptor alone
    }
}

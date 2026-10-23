//! The `.pdata` and `.xdata` exception tables: one `RUNTIME_FUNCTION` per
//! compiled function (spec section 12.4). `.pdata` is sorted by
//! `BeginAddress`, as `RtlLookupFunctionEntry` binary-searches it, and every
//! entry's `UnwindDataAddress` points at its `UNWIND_INFO` in `.xdata`, where
//! the entries sit concatenated in the same order, each already a multiple of
//! four bytes.

use crate::backend::x86_64::unwind;

use super::LinkError;
use super::objfile::{FunctionUnwind, ObjectFile};

/// `sizeof(RUNTIME_FUNCTION)`: three 32-bit RVAs.
pub const RUNTIME_FUNCTION_SIZE: u32 = 12;

/// The built `.pdata` and `.xdata`.
#[derive(Debug, PartialEq, Eq)]
pub struct UnwindTables {
    pub pdata: Vec<u8>,
    pub xdata: Vec<u8>,
}

/// The `.pdata` and `.xdata` byte counts [`build`] produces for `obj`;
/// section placement needs them before the tables exist. Both are zero
/// without unwind entries.
pub fn sizes(obj: &ObjectFile) -> Result<(u32, u32), LinkError> {
    let mut xdata = 0u32;
    for function in &obj.unwind {
        let info = unwind_info(function)?;
        xdata = xdata
            .checked_add(info.len() as u32)
            .ok_or(LinkError::LayoutOverflow)?;
    }
    Ok((RUNTIME_FUNCTION_SIZE * obj.unwind.len() as u32, xdata))
}

/// Builds both tables: functions sorted by `begin` (the order `.pdata`
/// requires), `UNWIND_INFO` entries concatenated into `.xdata` in that same
/// order, and each `RUNTIME_FUNCTION` carrying `.text` RVAs and its entry's
/// `.xdata` address. `text_rva` and `xdata_rva` are the sections' image RVAs.
pub fn build(obj: &ObjectFile, text_rva: u32, xdata_rva: u32) -> Result<UnwindTables, LinkError> {
    let mut functions: Vec<&FunctionUnwind> = obj.unwind.iter().collect();
    functions.sort_by_key(|function| function.begin);

    let text_size = obj.text.bytes.len() as u32;
    let mut xdata = Vec::new();
    let mut pdata = Vec::new();
    for (index, function) in functions.iter().enumerate() {
        if function.begin >= function.end || function.end > text_size {
            return Err(LinkError::FunctionRange {
                name: function.name.clone(),
                begin: function.begin,
                end: function.end,
                text_size,
            });
        }
        if index > 0 && functions[index - 1].end > function.begin {
            return Err(LinkError::OverlappingFunctions {
                first: functions[index - 1].name.clone(),
                second: function.name.clone(),
            });
        }

        let unwind_rva = xdata_rva + xdata.len() as u32;
        pdata.extend_from_slice(&(text_rva + function.begin).to_le_bytes());
        pdata.extend_from_slice(&(text_rva + function.end).to_le_bytes());
        pdata.extend_from_slice(&unwind_rva.to_le_bytes());
        xdata.extend_from_slice(&unwind_info(function)?);
    }

    Ok(UnwindTables { pdata, xdata })
}

fn unwind_info(function: &FunctionUnwind) -> Result<Vec<u8>, LinkError> {
    unwind::unwind_info(&function.prologue).map_err(|error| LinkError::BadUnwindInfo {
        function: function.name.clone(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RUNTIME_FUNCTION_SIZE, build, sizes};
    use crate::backend::x86_64::regs::Gpr;
    use crate::backend::x86_64::unwind::{Prologue, unwind_info};
    use crate::link::LinkError;
    use crate::link::objfile::{
        BssSpec, FunctionUnwind, ObjectFile, SectionContent, SectionId, Subsystem, Symbol,
    };

    const TEXT_RVA: u32 = 0x1000;
    const XDATA_RVA: u32 = 0x5000;

    fn function(name: &str, begin: u32, end: u32, prologue: Prologue) -> FunctionUnwind {
        FunctionUnwind {
            name: name.into(),
            begin,
            end,
            prologue,
        }
    }

    fn object(unwind: Vec<FunctionUnwind>) -> ObjectFile {
        let bytes = unwind
            .iter()
            .map(|function| function.end)
            .max()
            .unwrap_or(1)
            .max(1) as usize;
        ObjectFile {
            text: SectionContent {
                bytes: vec![0xCC; bytes],
                align: 16,
            },
            rdata: SectionContent {
                bytes: vec![],
                align: 8,
            },
            data: SectionContent {
                bytes: vec![],
                align: 8,
            },
            bss: BssSpec { size: 0, align: 8 },
            symbols: vec![Symbol {
                name: "entry".into(),
                section: SectionId::Text,
                offset: 0,
            }],
            relocs: Vec::new(),
            imports: BTreeMap::new(),
            unwind,
            entry: "entry".into(),
            subsystem: Subsystem::Console,
            stack_reserve: None,
        }
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn runtime_functions_sorted_with_xdata_offsets() {
        // Given out of order, with differing prologues.
        let early = Prologue::new(0x30, &[Gpr::Rsi]);
        let mid = Prologue::new(0x98, &[Gpr::Rbx, Gpr::R12]);
        let late = Prologue::new(0x20, &[]);
        let obj = object(vec![
            function("mid", 0x40, 0x80, mid.clone()),
            function("late", 0x80, 0xC0, late.clone()),
            function("early", 0x0, 0x40, early.clone()),
        ]);

        let (pdata_size, xdata_size) = sizes(&obj).unwrap();
        let tables = build(&obj, TEXT_RVA, XDATA_RVA).unwrap();
        assert_eq!(tables.pdata.len() as u32, pdata_size);
        assert_eq!(pdata_size, 3 * RUNTIME_FUNCTION_SIZE);
        assert_eq!(tables.xdata.len() as u32, xdata_size);

        // .xdata: the three entries back to back in .pdata order.
        let first = unwind_info(&early).unwrap().len() as u32;
        let second = unwind_info(&mid).unwrap().len() as u32;
        assert_eq!(
            tables.xdata,
            [
                unwind_info(&early).unwrap(),
                unwind_info(&mid).unwrap(),
                unwind_info(&late).unwrap()
            ]
            .concat()
        );

        // Sorted by BeginAddress, extents in .text, unwind data in .xdata.
        let entry = |index: usize| {
            (
                u32_at(&tables.pdata, index * 12),
                u32_at(&tables.pdata, index * 12 + 4),
                u32_at(&tables.pdata, index * 12 + 8),
            )
        };
        assert_eq!(entry(0), (TEXT_RVA, TEXT_RVA + 0x40, XDATA_RVA));
        assert_eq!(
            entry(1),
            (TEXT_RVA + 0x40, TEXT_RVA + 0x80, XDATA_RVA + first)
        );
        assert_eq!(
            entry(2),
            (TEXT_RVA + 0x80, TEXT_RVA + 0xC0, XDATA_RVA + first + second)
        );
    }

    #[test]
    fn empty_input_produces_nothing() {
        let obj = object(vec![]);
        assert_eq!(sizes(&obj).unwrap(), (0, 0));
        let tables = build(&obj, TEXT_RVA, XDATA_RVA).unwrap();
        assert!(tables.pdata.is_empty());
        assert!(tables.xdata.is_empty());
    }

    #[test]
    fn rejects_bad_ranges_and_prologues() {
        let obj = object(vec![function("hole", 0x10, 0x8, Prologue::new(8, &[]))]);
        assert_eq!(
            build(&obj, TEXT_RVA, XDATA_RVA),
            Err(LinkError::FunctionRange {
                name: "hole".into(),
                begin: 0x10,
                end: 0x8,
                text_size: 0x8,
            })
        );

        // The text is one byte short of the claimed extent.
        let obj = object(vec![function("past", 0x0, 0x20, Prologue::new(8, &[]))]);
        let mut obj = obj;
        obj.text.bytes.pop();
        assert_eq!(
            build(&obj, TEXT_RVA, XDATA_RVA),
            Err(LinkError::FunctionRange {
                name: "past".into(),
                begin: 0x0,
                end: 0x20,
                text_size: 0x1F,
            })
        );

        let obj = object(vec![
            function("first", 0x0, 0x20, Prologue::new(8, &[])),
            function("second", 0x10, 0x30, Prologue::new(8, &[])),
        ]);
        assert_eq!(
            build(&obj, TEXT_RVA, XDATA_RVA),
            Err(LinkError::OverlappingFunctions {
                first: "first".into(),
                second: "second".into(),
            })
        );

        let obj = object(vec![function("odd", 0x0, 0x10, Prologue::new(0x24, &[]))]);
        assert!(matches!(
            build(&obj, TEXT_RVA, XDATA_RVA),
            Err(LinkError::BadUnwindInfo { function, .. }) if function == "odd"
        ));
    }
}

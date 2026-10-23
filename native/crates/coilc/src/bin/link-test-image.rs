//! Links the fixed hand-assembled image the linker tests inspect and run.
//!
//! The program calls `kernel32!ExitProcess(42)` through the IAT and reads
//! its argument through a `.data` pointer fixed up by an `Abs64`
//! relocation, so a missing or wrong base-relocation table crashes under
//! ASLR instead of exiting 42. Bytes are written out by hand: no encoder
//! is involved. One `key=value` fact per line describes the layout so the
//! tests can assert exact addresses.

use std::process::ExitCode;

use coilc::link::objfile::{
    BssSpec, ObjectFile, Place, Reloc, SectionContent, SectionId, Subsystem, Symbol, Target,
};
use coilc::link::pe;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(output) = args.next() else {
        eprintln!("usage: link-test-image <output.exe> [--gui]");
        return ExitCode::from(2);
    };
    let subsystem = match args.next().as_deref() {
        None => Subsystem::Console,
        Some("--gui") => Subsystem::Gui,
        Some(flag) => {
            eprintln!("link-test-image: unknown flag {flag}");
            return ExitCode::from(2);
        }
    };

    let obj = test_object(subsystem);
    let linked = match pe::link_image(&obj) {
        Ok(linked) => linked,
        Err(error) => {
            eprintln!("link-test-image: {error}");
            return ExitCode::from(1);
        }
    };
    let symbols = match linked.layout.symbol_rvas(&obj) {
        Ok(symbols) => symbols,
        Err(error) => {
            eprintln!("link-test-image: {error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = std::fs::write(&output, &linked.image) {
        eprintln!("link-test-image: {error}");
        return ExitCode::from(1);
    }

    let mut facts: Vec<(&str, u32)> = vec![
        ("entry_rva", linked.layout.entry_rva),
        ("text_rva", linked.layout.text_rva),
        ("rdata_rva", linked.layout.rdata_rva),
        ("data_rva", linked.layout.data_rva),
        ("bss_offset", linked.layout.bss_offset),
        ("data_vsize", linked.layout.data_vsize),
        ("data_raw", align_200(obj.data.bytes.len() as u32)),
        ("idata_rva", linked.layout.idata_rva),
        ("idata_size", linked.layout.idata_vsize),
        ("iat_rva", linked.layout.iat_directory.0),
        ("iat_size", linked.layout.iat_directory.1),
        ("reloc_rva", linked.layout.reloc_rva),
        ("reloc_size", linked.layout.reloc_vsize),
        ("size_of_image", linked.layout.size_of_image),
        ("bss_size", obj.bss.size),
    ];
    for name in [
        "text_cont",
        "msg",
        "rdata_slot",
        "value",
        "ptr_slot",
        "func_ptr",
        "scratch",
    ] {
        match symbols.get(name) {
            Some(rva) => facts.push((name, *rva)),
            None => {
                eprintln!("link-test-image: symbol {name} was defined");
                return ExitCode::from(1);
            }
        }
    }
    match linked
        .iat_slots
        .get(&("kernel32.dll".to_string(), "ExitProcess".to_string()))
    {
        Some(rva) => facts.push(("iat_exit_process_rva", *rva)),
        None => {
            eprintln!("link-test-image: ExitProcess was imported");
            return ExitCode::from(1);
        }
    }
    for (key, value) in facts {
        println!("{key}={value}");
    }
    ExitCode::SUCCESS
}

fn align_200(value: u32) -> u32 {
    (value + 0x1FF) & !0x1FF
}

/// The program, written as bytes:
///
/// ```text
/// 00: 48 83 EC 28        sub rsp, 0x28          ; shadow space, 16-aligned
/// 04: E9 rel32           jmp text_cont          ; Rel32 -> .text symbol
/// 09: CC                 int3                   ; pad
/// 0A: 48 8D 0D rel32     lea rcx, [rip+disp]    ; Rel32 -> msg + 2 (addend)
/// 11: 48 8B 05 rel32     mov rax, [rip+disp]    ; Rel32 -> ptr_slot (.data)
/// 18: 8B 08              mov ecx, [rax]         ; Abs64'ed pointer to `value`
/// 1A: 83 C1 28           add ecx, 40            ; 2 + 40 = exit code 42
/// 1D: FF 15 rel32        call [rip+disp]        ; Rel32 -> IAT slot of ExitProcess
/// 23: CC CC CC           int3                   ; pad
/// ```
///
/// `user32.dll` and `GetCurrentProcessId` are imported but unused: they
/// exercise multi-DLL descriptors and multi-entry IAT arrays.
fn test_object(subsystem: Subsystem) -> ObjectFile {
    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]);
    text.push(0xE9);
    let jmp_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.push(0xCC);
    let text_cont = text.len() as u32;
    text.extend_from_slice(&[0x48, 0x8D, 0x0D]);
    let lea_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.extend_from_slice(&[0x48, 0x8B, 0x05]);
    let mov_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.extend_from_slice(&[0x8B, 0x08]);
    text.extend_from_slice(&[0x83, 0xC1, 0x28]);
    text.extend_from_slice(&[0xFF, 0x15]);
    let call_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.extend_from_slice(&[0xCC, 0xCC, 0xCC]);

    let mut rdata: Vec<u8> = Vec::new();
    rdata.extend_from_slice(b"coil\x00link\x00");
    while !rdata.len().is_multiple_of(8) {
        rdata.push(0);
    }
    let rdata_slot = rdata.len() as u32;
    rdata.extend_from_slice(&[0; 8]);

    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&2u32.to_le_bytes()); // `value`
    data.extend_from_slice(&[0; 4]); // keep `ptr_slot` 8-aligned
    let ptr_slot = data.len() as u32;
    data.extend_from_slice(&[0; 8]);
    let func_ptr = data.len() as u32;
    data.extend_from_slice(&[0; 8]);

    ObjectFile {
        text: SectionContent {
            bytes: text,
            align: 16,
        },
        rdata: SectionContent {
            bytes: rdata,
            align: 8,
        },
        data: SectionContent {
            bytes: data,
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
                name: "text_cont".into(),
                section: SectionId::Text,
                offset: text_cont,
            },
            Symbol {
                name: "msg".into(),
                section: SectionId::RData,
                offset: 0,
            },
            Symbol {
                name: "rdata_slot".into(),
                section: SectionId::RData,
                offset: rdata_slot,
            },
            Symbol {
                name: "value".into(),
                section: SectionId::Data,
                offset: 0,
            },
            Symbol {
                name: "ptr_slot".into(),
                section: SectionId::Data,
                offset: ptr_slot,
            },
            Symbol {
                name: "func_ptr".into(),
                section: SectionId::Data,
                offset: func_ptr,
            },
            Symbol {
                name: "scratch".into(),
                section: SectionId::Bss,
                offset: 0,
            },
        ],
        relocs: vec![
            Reloc::Rel32 {
                at: Place {
                    section: SectionId::Text,
                    offset: jmp_at,
                },
                target: Target::Symbol("text_cont".into()),
                addend: 0,
            },
            Reloc::Rel32 {
                at: Place {
                    section: SectionId::Text,
                    offset: lea_at,
                },
                target: Target::Symbol("msg".into()),
                addend: 2,
            },
            Reloc::Rel32 {
                at: Place {
                    section: SectionId::Text,
                    offset: mov_at,
                },
                target: Target::Symbol("ptr_slot".into()),
                addend: 0,
            },
            Reloc::Rel32 {
                at: Place {
                    section: SectionId::Text,
                    offset: call_at,
                },
                target: Target::Import {
                    dll: "kernel32.dll".into(),
                    name: "ExitProcess".into(),
                },
                addend: 0,
            },
            Reloc::Abs64 {
                at: Place {
                    section: SectionId::RData,
                    offset: rdata_slot,
                },
                target: Target::Symbol("msg".into()),
                addend: 0,
            },
            Reloc::Abs64 {
                at: Place {
                    section: SectionId::Data,
                    offset: ptr_slot,
                },
                target: Target::Symbol("value".into()),
                addend: 0,
            },
            Reloc::Abs64 {
                at: Place {
                    section: SectionId::Data,
                    offset: func_ptr,
                },
                target: Target::Symbol("text_cont".into()),
                addend: 0,
            },
        ],
        imports: [
            (
                "kernel32.dll".to_string(),
                vec!["ExitProcess".to_string(), "GetCurrentProcessId".to_string()],
            ),
            (
                "user32.dll".to_string(),
                vec!["GetDesktopWindow".to_string()],
            ),
        ]
        .into_iter()
        .collect(),
        entry: "entry".into(),
        subsystem,
        stack_reserve: None,
    }
}

//! Links the fixed hand-assembled image the linker tests inspect and run.
//!
//! The program reads its load base from the PEB (`gs:[0x60]`, then
//! `ImageBaseAddress`) and compares it with the link-time `IMAGE_BASE`, so
//! its exit code reports where it landed: 42 when the image was rebased
//! and the `Abs64`-fixed `.data` pointer still dereferenced correctly, 43
//! when it still sits at the preferred base (where the statically written
//! pointer is the right one). A missing or wrong base-relocation table
//! crashes under ASLR instead of exiting 42. Bytes are written out by
//! hand: no encoder is involved. One `key=value` fact per line describes
//! the layout so the tests can assert exact addresses. `--no-constants`
//! drops `.rdata` and everything referring to it, leaving an image whose
//! linker must omit the empty section.

use std::process::ExitCode;

use coilc::link::objfile::{
    BssSpec, ObjectFile, Place, Reloc, SectionContent, SectionId, Subsystem, Symbol, Target,
};
use coilc::link::pe;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(output) = args.next() else {
        eprintln!("usage: link-test-image <output.exe> [--gui] [--no-constants]");
        return ExitCode::from(2);
    };
    let mut subsystem = Subsystem::Console;
    let mut no_constants = false;
    for arg in args {
        match arg.as_str() {
            "--gui" => subsystem = Subsystem::Gui,
            "--no-constants" => no_constants = true,
            flag => {
                eprintln!("link-test-image: unknown flag {flag}");
                return ExitCode::from(2);
            }
        }
    }

    let obj = test_object(subsystem, no_constants);
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
    let mut names = vec![
        "text_cont",
        "probe",
        "value",
        "ptr_slot",
        "func_ptr",
        "scratch",
    ];
    if !no_constants {
        names.extend_from_slice(&["msg", "rdata_slot"]);
    }
    for name in names {
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

/// The program, written as bytes (`--no-constants` omits the `lea` at
/// `0x0A` and everything `.rdata`-related, shifting the rest left):
///
/// ```text
/// 00: 48 83 EC 28        sub rsp, 0x28          ; shadow space, 16-aligned
/// 04: E9 rel32           jmp probe              ; Rel32 -> .text symbol
/// 09: CC                 int3                   ; pad
/// 0A: 48 8D 0D rel32     lea rcx, [rip+disp]    ; Rel32 -> msg + 2 (addend)
/// 11: 48 8B 05 rel32     mov rax, [rip+disp]    ; Rel32 -> ptr_slot (.data)
/// 18: 8B 08              mov ecx, [rax]         ; Abs64'ed pointer to `value`
/// 1A: 41 03 C8           add ecx, r8d           ; +1 only at the preferred base
/// 1D: 83 C1 28           add ecx, 40            ; 2 + 40 + flag = 42 rebased, 43 static
/// 20: FF 15 rel32        call [rip+disp]        ; Rel32 -> IAT slot of ExitProcess
/// 26: CC CC              int3                   ; pad
/// 28: 65 48 8B 04 25 60 00 00 00   mov rax, gs:[0x60]  ; PEB
/// 31: 48 8B 40 10                 mov rax, [rax+0x10] ; PEB->ImageBaseAddress
/// 35: 48 B9 <IMAGE_BASE imm64>    movabs rcx, 0x140000000
/// 3F: 48 39 C8                    cmp rax, rcx
/// 42: 41 B8 01 00 00 00           mov r8d, 1     ; sitting at the preferred base
/// 48: 75 02                       jne rebased    ; skip the r8d = 0 store
/// 4A: EB BE                       jmp deref
/// 4C: 41 B8 00 00 00 00   rebased: mov r8d, 0    ; loader moved the image
/// 52: EB B6                       jmp deref
/// ```
///
/// The exit code therefore pins down the load base: 42 means the image was
/// rebased and the relocated pointer dereferenced `value`, 43 means it
/// still sat at 0x140000000 where the unrelocated pointer is the correct
/// one. A wrong `.reloc` under ASLR leaves the stale static address in
/// `ptr_slot` and the deref faults instead of exiting.
///
/// `user32.dll` and `GetCurrentProcessId` are imported but unused: they
/// exercise multi-DLL descriptors and multi-entry IAT arrays.
fn test_object(subsystem: Subsystem, no_constants: bool) -> ObjectFile {
    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(&[0x48, 0x83, 0xEC, 0x28]);
    text.push(0xE9);
    let jmp_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.push(0xCC);
    let deref = text.len() as u32;
    let mut relocs = Vec::new();
    if !no_constants {
        text.extend_from_slice(&[0x48, 0x8D, 0x0D]);
        let lea_at = text.len() as u32;
        text.extend_from_slice(&[0; 4]);
        relocs.push(Reloc::Rel32 {
            at: Place {
                section: SectionId::Text,
                offset: lea_at,
            },
            target: Target::Symbol("msg".into()),
            addend: 2,
        });
    }
    text.extend_from_slice(&[0x48, 0x8B, 0x05]);
    let mov_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.extend_from_slice(&[0x8B, 0x08]);
    text.extend_from_slice(&[0x41, 0x03, 0xC8]);
    text.extend_from_slice(&[0x83, 0xC1, 0x28]);
    text.extend_from_slice(&[0xFF, 0x15]);
    let call_at = text.len() as u32;
    text.extend_from_slice(&[0; 4]);
    text.extend_from_slice(&[0xCC, 0xCC]);

    let probe = text.len() as u32;
    text.extend_from_slice(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x60, 0x00, 0x00, 0x00]);
    text.extend_from_slice(&[0x48, 0x8B, 0x40, 0x10]);
    text.extend_from_slice(&[0x48, 0xB9]);
    text.extend_from_slice(&pe::IMAGE_BASE.to_le_bytes());
    text.extend_from_slice(&[0x48, 0x39, 0xC8]);
    text.extend_from_slice(&[0x41, 0xB8, 0x01, 0x00, 0x00, 0x00]);
    let jne_next = text.len() as u32 + 2;
    let jmp_next = jne_next + 2;
    let rebased = jmp_next;
    text.extend_from_slice(&[0x75, (rebased - jne_next) as u8]);
    let disp = deref as i32 - jmp_next as i32;
    assert!((-128..=127).contains(&disp), "jmp deref out of rel8 range");
    text.extend_from_slice(&[0xEB, disp as u8]);
    assert_eq!(text.len() as u32, rebased);
    text.extend_from_slice(&[0x41, 0xB8, 0x00, 0x00, 0x00, 0x00]);
    let disp = deref as i32 - (text.len() as u32 + 2) as i32;
    assert!((-128..=127).contains(&disp), "jmp deref out of rel8 range");
    text.extend_from_slice(&[0xEB, disp as u8]);

    relocs.insert(
        0,
        Reloc::Rel32 {
            at: Place {
                section: SectionId::Text,
                offset: jmp_at,
            },
            target: Target::Symbol("probe".into()),
            addend: 0,
        },
    );
    relocs.push(Reloc::Rel32 {
        at: Place {
            section: SectionId::Text,
            offset: mov_at,
        },
        target: Target::Symbol("ptr_slot".into()),
        addend: 0,
    });
    relocs.push(Reloc::Rel32 {
        at: Place {
            section: SectionId::Text,
            offset: call_at,
        },
        target: Target::Import {
            dll: "kernel32.dll".into(),
            name: "ExitProcess".into(),
        },
        addend: 0,
    });

    let mut rdata: Vec<u8> = Vec::new();
    let mut rdata_slot = 0;
    if !no_constants {
        rdata.extend_from_slice(b"coil\x00link\x00");
        while !rdata.len().is_multiple_of(8) {
            rdata.push(0);
        }
        rdata_slot = rdata.len() as u32;
        rdata.extend_from_slice(&[0; 8]);
        relocs.push(Reloc::Abs64 {
            at: Place {
                section: SectionId::RData,
                offset: rdata_slot,
            },
            target: Target::Symbol("msg".into()),
            addend: 0,
        });
    }

    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&2u32.to_le_bytes()); // `value`
    data.extend_from_slice(&[0; 4]); // keep `ptr_slot` 8-aligned
    let ptr_slot = data.len() as u32;
    data.extend_from_slice(&[0; 8]);
    let func_ptr = data.len() as u32;
    data.extend_from_slice(&[0; 8]);
    relocs.push(Reloc::Abs64 {
        at: Place {
            section: SectionId::Data,
            offset: ptr_slot,
        },
        target: Target::Symbol("value".into()),
        addend: 0,
    });
    relocs.push(Reloc::Abs64 {
        at: Place {
            section: SectionId::Data,
            offset: func_ptr,
        },
        target: Target::Symbol("text_cont".into()),
        addend: 0,
    });

    let mut symbols = vec![
        Symbol {
            name: "entry".into(),
            section: SectionId::Text,
            offset: 0,
        },
        Symbol {
            name: "text_cont".into(),
            section: SectionId::Text,
            offset: deref,
        },
        Symbol {
            name: "probe".into(),
            section: SectionId::Text,
            offset: probe,
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
    ];
    if !no_constants {
        symbols.push(Symbol {
            name: "msg".into(),
            section: SectionId::RData,
            offset: 0,
        });
        symbols.push(Symbol {
            name: "rdata_slot".into(),
            section: SectionId::RData,
            offset: rdata_slot,
        });
    }

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
        symbols,
        relocs,
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

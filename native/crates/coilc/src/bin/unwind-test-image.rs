//! Links the unwind-walk image the pdata tests inspect and, on Windows, run.
//!
//! The program carries one function per prologue shape: `main` and
//! `small_fn` (small `UWOP_ALLOC_SMALL` frames), `small_saved`
//! (`UWOP_PUSH_NONVOL` callee saves),
//! `alloc_large` (`UWOP_ALLOC_LARGE` with saved registers), `chain`
//! (alloc-large, recursive for a deliberately deep call chain) and `walker`
//! (a 0x3008-byte probed frame). Prologue bytes come from
//! `backend::x86_64::unwind`, so the `.pdata`/`.xdata` the linker writes
//! describes exactly what executes; the bodies and epilogues are
//! hand-assembled bytes with intra-function branch targets patched by a
//! tiny label pass.
//!
//! `walker` captures a `CONTEXT` with `ntdll!RtlCaptureContext` and walks to
//! `main` with `RtlLookupFunctionEntry` + `RtlVirtualUnwind`: every frame's
//! return address must fall inside the expected function's `.pdata` extent,
//! the looked-up `RUNTIME_FUNCTION` must be exactly that function's, the
//! return address must sit at the reported stack pointer, that pointer must
//! be `8 (mod 16)` as the ABI fixes, and `EstablisherFrame` must equal it
//! minus 8, since every frame here pushes rbp first. Exit code 42 means the
//! walk reached `main` with every frame verified; 1xx/2xx name the failed
//! check.
//!
//! One `key=value` fact per line describes the linked image so the tests can
//! assert exact addresses without duplicating the layout.

use std::process::ExitCode;

use coilc::backend::x86_64::regs::Gpr;
use coilc::backend::x86_64::unwind::{Prologue, prologue_bytes};
use coilc::link::objfile::{
    BssSpec, FunctionUnwind, ObjectFile, Place, Reloc, SectionContent, SectionId, Subsystem,
    Symbol, Target,
};
use coilc::link::pe;

/// `chain` recursion count: the deliberate depth of the call chain.
const DEPTH: u32 = 24;

/// `sizeof(CONTEXT)` on x64; lives at the top of `walker`'s frame.
const CONTEXT_SIZE: u32 = 0x4F0;
/// `CONTEXT.Rsp` / `CONTEXT.Rip` offsets.
const CTX_RSP: i32 = 0x98;
const CTX_RIP: i32 = 0xF8;

fn main() -> ExitCode {
    let Some(output) = std::env::args().nth(1) else {
        eprintln!("usage: unwind-test-image <output.exe>");
        return ExitCode::from(2);
    };

    let program = program();
    let linked = match pe::link_image(&program.object) {
        Ok(linked) => linked,
        Err(error) => {
            eprintln!("unwind-test-image: {error}");
            return ExitCode::from(1);
        }
    };
    let symbols = match linked.layout.symbol_rvas(&program.object) {
        Ok(symbols) => symbols,
        Err(error) => {
            eprintln!("unwind-test-image: {error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = std::fs::write(&output, &linked.image) {
        eprintln!("unwind-test-image: {error}");
        return ExitCode::from(1);
    }

    let mut facts: Vec<(String, u32)> = vec![
        ("text_rva".into(), linked.layout.text_rva),
        ("pdata_rva".into(), linked.layout.pdata_rva),
        ("pdata_size".into(), linked.layout.pdata_vsize),
        ("xdata_rva".into(), linked.layout.xdata_rva),
        ("xdata_size".into(), linked.layout.xdata_vsize),
        (
            "exception_dir_rva".into(),
            linked.layout.exception_directory.0,
        ),
        (
            "exception_dir_size".into(),
            linked.layout.exception_directory.1,
        ),
        ("size_of_image".into(), linked.layout.size_of_image),
        ("depth".into(), DEPTH),
    ];
    for function in &program.object.unwind {
        facts.push((
            format!("{}_rva", function.name),
            linked.layout.text_rva + function.begin,
        ));
        facts.push((
            format!("{}_size", function.name),
            function.end - function.begin,
        ));
        facts.push((
            format!("{}_prologue", function.name),
            prologue_bytes(&function.prologue).unwrap().len() as u32,
        ));
    }
    for name in ["small_fn", "small_saved", "alloc_large", "chain", "walker"] {
        match symbols.get(name) {
            Some(rva) => facts.push((format!("sym_{name}"), *rva)),
            None => {
                eprintln!("unwind-test-image: symbol {name} was not in the linked layout");
                return ExitCode::from(1);
            }
        }
    }
    for (key, value) in facts {
        println!("{key}={value}");
    }
    ExitCode::SUCCESS
}

/// Hand-assembles one function body: bytes plus `Rel32` symbol fixups and
/// short intra-function branches resolved through labels, since the walker's
/// checks need exact instruction encodings.
struct Body {
    bytes: Vec<u8>,
    /// Fixups to patch once labels bind: (byte offset, size, label).
    branches: Vec<(u32, u8, usize)>,
    /// `Rel32` fixups the linker resolves: byte offset -> named target.
    symbols: Vec<(u32, Target)>,
    labels: Vec<Option<u32>>,
}

impl Body {
    fn new() -> Body {
        Body {
            bytes: Vec::new(),
            branches: Vec::new(),
            symbols: Vec::new(),
            labels: Vec::new(),
        }
    }

    fn u8(&mut self, byte: u8) {
        self.bytes.push(byte);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn disp32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn bind(&mut self, label: usize) {
        self.labels[label] = Some(self.bytes.len() as u32);
    }

    fn jcc8(&mut self, opcode: u8, label: usize) {
        self.u8(opcode);
        self.branches.push((self.bytes.len() as u32, 1, label));
        self.u8(0);
    }

    fn jmp8(&mut self, label: usize) {
        self.jcc8(0xEB, label);
    }

    fn jmp32(&mut self, label: usize) {
        self.u8(0xE9);
        self.branches.push((self.bytes.len() as u32, 4, label));
        self.bytes.extend_from_slice(&[0; 4]);
    }

    /// Near conditional branch, `0F 8x rel32`: the walker's checks reach
    /// past rel8 across the loop body.
    fn jcc32(&mut self, opcode: u8, label: usize) {
        self.bytes(&[0x0F, opcode]);
        self.branches.push((self.bytes.len() as u32, 4, label));
        self.bytes.extend_from_slice(&[0; 4]);
    }

    fn call_sym(&mut self, name: &str) {
        self.u8(0xE8);
        self.rel32(Target::Symbol(name.into()));
    }

    fn lea_rip(&mut self, reg: u8, name: &str) {
        // 48 8D /r with reg in ModRM.reg and RIP-relative rm = 0x05 | reg<<3.
        self.bytes(&[0x48, 0x8D, 0x05 | reg << 3]);
        self.rel32(Target::Symbol(name.into()));
    }

    fn call_iat(&mut self, dll: &str, name: &str) {
        self.bytes(&[0xFF, 0x15]);
        self.rel32(Target::Import {
            dll: dll.into(),
            name: name.into(),
        });
    }

    fn rel32(&mut self, target: Target) {
        self.symbols.push((self.bytes.len() as u32, target));
        self.bytes.extend_from_slice(&[0; 4]);
    }

    /// Patches every branch against its bound label; branch displacements
    /// count from the end of the branch instruction.
    fn finish(mut self) -> (Vec<u8>, Vec<(u32, Target)>) {
        for (at, size, label) in &self.branches {
            let target = self.labels[*label].expect("branch to an unbound label") as i64;
            let next = i64::from(*at) + i64::from(*size);
            let disp = target - next;
            if *size == 1 {
                let disp = i8::try_from(disp).expect("short branch out of range");
                self.bytes[*at as usize] = disp as u8;
            } else {
                let disp = i32::try_from(disp).expect("branch out of range");
                self.bytes[*at as usize..*at as usize + 4].copy_from_slice(&disp.to_le_bytes());
            }
        }
        (self.bytes, self.symbols)
    }
}

/// The epilogue every function shares: pops in reverse push order, then the
/// frame pointer and return address. `RtlVirtualUnwind` never reads
/// epilogues, so any correct sequence serves.
fn epilogue(saved: &[Gpr]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for reg in saved.iter().rev() {
        if reg.extended() {
            bytes.push(0x41);
        }
        bytes.push(0x58 | reg.number());
    }
    bytes.extend_from_slice(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
    bytes.push(0x5D); // pop rbp
    bytes.push(0xC3); // ret
    bytes
}

struct Program {
    object: ObjectFile,
}

/// Builds the whole object: six functions in `.text` (16-byte aligned,
/// `int3`-padded), the expectation table in `.data` and the ntdll/kernel32
/// imports the walker and `main` need.
fn program() -> Program {
    let prologue_main = Prologue::new(0x20, &[]);
    let prologue_small = Prologue::new(0x20, &[]);
    let prologue_small_saved = Prologue::new(0x30, &[Gpr::Rbx, Gpr::Rsi]);
    let prologue_alloc_large = Prologue::new(0x98, &[Gpr::Rbx, Gpr::Rsi, Gpr::Rdi]);
    let prologue_chain = Prologue::new(0x98, &[Gpr::Rbx, Gpr::R12, Gpr::R13]);
    // 0x3008 > one page: the probed shape. Only the top 0x510 bytes hold
    // scratch; the rest exercises the probe loop and the large allocation.
    const WALKER_FRAME: u32 = 0x3008;
    let prologue_walker = Prologue::new(WALKER_FRAME, &[Gpr::Rbx, Gpr::R12, Gpr::R13]);

    // main: entry. Propagates the deep chain's result to ExitProcess.
    let mut main = Body::new();
    main.call_sym("small_fn");
    main.bytes(&[0x89, 0xC1]); // mov ecx, eax
    main.call_iat("kernel32.dll", "ExitProcess");
    main.bytes(&[0x0F, 0x0B]); // ud2: ExitProcess does not return

    let mut small = Body::new();
    small.call_sym("small_saved");

    let mut small_saved = Body::new();
    small_saved.call_sym("alloc_large");

    let mut alloc_large = Body::new();
    // DEPTH-1: `chain(n)` places n+1 frames (n down to 0), and the walk
    // expects exactly DEPTH chain frames before alloc_large.
    alloc_large.bytes(&[0xB9]); // mov ecx, DEPTH - 1
    alloc_large.disp32(DEPTH as i32 - 1);
    alloc_large.call_sym("chain");

    // chain(depth in rcx): recurse DEPTH times, then run the walk.
    let mut chain = Body::new();
    chain.bytes(&[0x48, 0x89, 0xCB]); // mov rbx, rcx
    chain.bytes(&[0x48, 0x85, 0xDB]); // test rbx, rbx
    let chain_bottom = chain.label();
    chain.jcc8(0x74, chain_bottom); // jz bottom
    chain.bytes(&[0x48, 0x8D, 0x8B]); // lea rcx, [rbx-1]
    chain.disp32(-1);
    chain.call_sym("chain");
    let chain_out = chain.label();
    chain.jmp8(chain_out);
    chain.bind(chain_bottom);
    chain.call_sym("walker");
    chain.bind(chain_out);

    let walker = walker_body();

    let mut text: Vec<u8> = Vec::new();
    let mut symbols = Vec::new();
    let mut relocs: Vec<(u32, Target)> = Vec::new();
    let mut unwind: Vec<(String, Prologue, u32, u32)> = Vec::new();
    for (name, prologue, body) in [
        ("main", prologue_main, main),
        ("small_fn", prologue_small, small),
        ("small_saved", prologue_small_saved, small_saved),
        ("alloc_large", prologue_alloc_large, alloc_large),
        ("chain", prologue_chain, chain),
        ("walker", prologue_walker, walker),
    ] {
        while !text.len().is_multiple_of(16) {
            text.push(0xCC);
        }
        let begin = text.len() as u32;
        let prologue_bytes = prologue_bytes(&prologue).unwrap();
        text.extend_from_slice(&prologue_bytes);
        let (body_bytes, body_symbols) = body.finish();
        for (at, target) in body_symbols {
            relocs.push((begin + prologue_bytes.len() as u32 + at, target));
        }
        text.extend_from_slice(&body_bytes);
        text.extend_from_slice(&epilogue(&prologue.saved));
        let end = text.len() as u32;
        symbols.push(Symbol {
            name: name.into(),
            section: SectionId::Text,
            offset: begin,
        });
        unwind.push((name.into(), prologue, begin, end));
    }

    // The expectation table: which function each unwound frame's return
    // address must lie inside, in call order from walker: chain (DEPTH
    // times), alloc_large, small_saved, small_fn, main. Absolute begin
    // pointers (loader-relocated) plus sizes.
    let mut data: Vec<u8> = Vec::new();
    let mut data_relocs: Vec<(u32, Target)> = Vec::new();
    for (name, _prologue, begin, end) in &unwind {
        let order = match name.as_str() {
            "chain" => 0,
            "alloc_large" => 1,
            "small_saved" => 2,
            "small_fn" => 3,
            "main" => 4,
            _ => continue,
        };
        let at = order * 32;
        while data.len() < at + 32 {
            data.push(0);
        }
        data_relocs.push((at as u32, Target::Symbol(name.clone())));
        let size = end - begin;
        data[at + 16..at + 24].copy_from_slice(&u64::from(size).to_le_bytes());
    }
    symbols.push(Symbol {
        name: "expect_table".into(),
        section: SectionId::Data,
        offset: 0,
    });

    let object = ObjectFile {
        text: SectionContent {
            bytes: text,
            align: 16,
        },
        rdata: SectionContent {
            bytes: vec![],
            align: 8,
        },
        data: SectionContent {
            bytes: data,
            align: 8,
        },
        bss: BssSpec { size: 0, align: 8 },
        symbols,
        relocs: relocs
            .into_iter()
            .map(|(at, target)| Reloc::Rel32 {
                at: Place {
                    section: SectionId::Text,
                    offset: at,
                },
                target,
                addend: 0,
            })
            .chain(data_relocs.into_iter().map(|(at, target)| Reloc::Abs64 {
                at: Place {
                    section: SectionId::Data,
                    offset: at,
                },
                target,
                addend: 0,
            }))
            .collect(),
        imports: [
            ("kernel32.dll".to_string(), vec!["ExitProcess".to_string()]),
            (
                "ntdll.dll".to_string(),
                vec![
                    "RtlCaptureContext".to_string(),
                    "RtlLookupFunctionEntry".to_string(),
                    "RtlVirtualUnwind".to_string(),
                ],
            ),
        ]
        .into_iter()
        .collect(),
        unwind: unwind
            .into_iter()
            .map(|(name, prologue, begin, end)| FunctionUnwind {
                name,
                begin,
                end,
                prologue,
            })
            .collect(),
        entry: "main".into(),
        subsystem: Subsystem::Console,
        stack_reserve: None,
    };
    Program { object }
}

/// `walker`'s body. Scratch lives just below the frame pointer: the CONTEXT
/// at `rbp-0x4F0`, then the image-base, establisher-frame and expected-range
/// slots down to `rbp-0x510`.
fn walker_body() -> Body {
    // rbp-relative displacements.
    const CTX: i32 = -(CONTEXT_SIZE as i32);
    const CTX_RIP_AT: i32 = CTX + CTX_RIP;
    const CTX_RSP_AT: i32 = CTX + CTX_RSP;
    const IMAGE_BASE: i32 = CTX - 8;
    const ESTABLISHER: i32 = CTX - 16;
    const EXPECT_BEGIN: i32 = CTX - 24;
    const EXPECT_END: i32 = CTX - 32;

    let mut b = Body::new();

    // Fail blocks, forward-referenced by each check.
    let fail_lookup = b.label();
    let fail_range = b.label();
    let fail_handler = b.label();
    let fail_slot = b.label();
    let fail_align = b.label();
    let fail_frame = b.label();
    let fail_expected = b.label();
    let fail_missing = b.label();
    let fail_entry = b.label();
    let done = b.label();

    // Capture the context, then look up walker's own RUNTIME_FUNCTION and
    // check the captured pc lies inside it.
    b.bytes(&[0x48, 0x8D, 0x8D]); // lea rcx, [rbp+CTX]
    b.disp32(CTX);
    b.call_iat("ntdll.dll", "RtlCaptureContext");

    b.bytes(&[0x48, 0x8B, 0x9D]); // mov rbx, [rbp+CTX_RIP_AT]
    b.disp32(CTX_RIP_AT);
    b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x4D, 0x31, 0xC0]); // xor r8, r8
    b.call_iat("ntdll.dll", "RtlLookupFunctionEntry");
    b.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
    b.jcc32(0x84, fail_lookup); // jz
    b.bytes(&[0x49, 0x89, 0xC5]); // mov r13, rax

    b.bytes(&[0x41, 0x8B, 0x4D, 0x00]); // mov ecx, [r13] (BeginAddress)
    b.bytes(&[0x48, 0x03, 0x8D]); // add rcx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x41, 0x8B, 0x55, 0x04]); // mov edx, [r13+4] (EndAddress)
    b.bytes(&[0x48, 0x03, 0x95]); // add rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x48, 0x39, 0xCB]); // cmp rbx, rcx
    b.jcc32(0x82, fail_range); // jb
    b.bytes(&[0x48, 0x39, 0xD9]); // cmp rcx, rbx
    b.jcc32(0x86, fail_range); // jbe (rbx >= end)

    let walk_loop = b.label();
    b.bind(walk_loop);

    // RtlVirtualUnwind(0, base, pc=rbx, entry=r13, ctx, 0, &frame, 0). The
    // four stack arguments are re-stored every iteration: the call may
    // clobber them.
    b.bytes(&[0x48, 0x8D, 0x85]); // lea rax, [rbp+CTX]
    b.disp32(CTX);
    b.bytes(&[0x48, 0x89, 0x44, 0x24, 0x20]); // mov [rsp+0x20], rax
    b.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x28, 0x00, 0x00, 0x00, 0x00]);
    b.bytes(&[0x48, 0x8D, 0x85]); // lea rax, [rbp+ESTABLISHER]
    b.disp32(ESTABLISHER);
    b.bytes(&[0x48, 0x89, 0x44, 0x24, 0x30]); // mov [rsp+0x30], rax
    b.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x38, 0x00, 0x00, 0x00, 0x00]);
    b.bytes(&[0x31, 0xC9]); // xor ecx, ecx
    b.bytes(&[0x48, 0x8B, 0x95]); // mov rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x49, 0x89, 0xD8]); // mov r8, rbx
    b.bytes(&[0x4D, 0x89, 0xE9]); // mov r9, r13
    b.call_iat("ntdll.dll", "RtlVirtualUnwind");
    b.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
    b.jcc32(0x85, fail_handler); // jnz: no function here has a handler

    b.bytes(&[0x48, 0x8B, 0x9D]); // mov rbx, [rbp+CTX_RIP_AT] (return address)
    b.disp32(CTX_RIP_AT);
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+CTX_RSP_AT]
    b.disp32(CTX_RSP_AT);
    b.bytes(&[0x48, 0x39, 0x19]); // cmp [rcx], rbx
    b.jcc32(0x85, fail_slot); // jne: the return address is not at rsp
    b.bytes(&[0x48, 0xF7, 0xC1, 0x08, 0x00, 0x00, 0x00]); // test rcx, 8
    b.jcc32(0x84, fail_align); // jz: entry rsp must be 8 (mod 16)
    b.bytes(&[0x48, 0x8B, 0x85]); // mov rax, [rbp+ESTABLISHER]
    b.disp32(ESTABLISHER);
    b.bytes(&[0x48, 0x8D, 0x51, 0xF8]); // lea rdx, [rcx-8]
    b.bytes(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
    b.jcc32(0x85, fail_frame); // jne: frame pointer is rsp-8 after push rbp

    // Expected caller: which = 0 (chain) while r12 < DEPTH, then
    // r12 - DEPTH + 1 selects alloc_large..main in the expectation table.
    b.bytes(&[0x4C, 0x89, 0xE0]); // mov rax, r12
    b.bytes(&[0x48, 0x2D]); // sub rax, DEPTH-1
    b.disp32(DEPTH as i32 - 1);
    b.bytes(&[0x41, 0x81, 0xFC]); // cmp r12d, DEPTH
    b.disp32(DEPTH as i32);
    let have_which = b.label();
    b.jcc8(0x73, have_which); // jae
    b.bytes(&[0x48, 0x31, 0xC0]); // xor rax, rax
    b.bind(have_which);
    b.lea_rip(1, "expect_table"); // lea rcx, [rip+table]
    b.bytes(&[0x48, 0xC1, 0xE0, 0x05]); // shl rax, 5
    b.bytes(&[0x48, 0x01, 0xC8]); // add rax, rcx
    b.bytes(&[0x48, 0x8B, 0x10]); // mov rdx, [rax] (begin)
    b.bytes(&[0x48, 0x8B, 0x48, 0x10]); // mov rcx, [rax+16] (size)
    b.bytes(&[0x48, 0x01, 0xD1]); // add rcx, rdx (end)
    b.bytes(&[0x48, 0x89, 0x95]); // mov [rbp+EXPECT_BEGIN], rdx
    b.disp32(EXPECT_BEGIN);
    b.bytes(&[0x48, 0x89, 0x8D]); // mov [rbp+EXPECT_END], rcx
    b.disp32(EXPECT_END);
    b.bytes(&[0x48, 0x39, 0xD3]); // cmp rbx, rdx
    b.jcc32(0x82, fail_expected); // jb
    b.bytes(&[0x48, 0x39, 0xD9]); // cmp rcx, rbx
    b.jcc32(0x86, fail_expected); // jbe

    b.bytes(&[0x48, 0x83, 0xF8, 0x04]); // cmp rax, 4
    b.jcc32(0x84, done); // je: main's frame reached and verified

    // The lookup for the return address must return exactly the expected
    // function's RUNTIME_FUNCTION.
    b.bytes(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x4D, 0x31, 0xC0]); // xor r8, r8
    b.call_iat("ntdll.dll", "RtlLookupFunctionEntry");
    b.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
    b.jcc32(0x84, fail_missing); // jz
    b.bytes(&[0x44, 0x8B, 0x00]); // mov r8d, [rax]
    b.bytes(&[0x44, 0x8B, 0x48, 0x04]); // mov r9d, [rax+4]
    b.bytes(&[0x48, 0x8B, 0x95]); // mov rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x49, 0x01, 0xD0]); // add r8, rdx
    b.bytes(&[0x49, 0x01, 0xD1]); // add r9, rdx
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_BEGIN]
    b.disp32(EXPECT_BEGIN);
    b.bytes(&[0x4C, 0x39, 0xC1]); // cmp rcx, r8
    b.jcc32(0x85, fail_entry); // jne
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_END]
    b.disp32(EXPECT_END);
    b.bytes(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9
    b.jcc32(0x85, fail_entry); // jne

    b.bytes(&[0x49, 0xFF, 0xC4]); // inc r12
    b.bytes(&[0x49, 0x89, 0xC5]); // mov r13, rax
    b.jmp32(walk_loop);

    // Exit code 42: every frame walked back to main and verified. The
    // 1xx/2xx codes name the check that failed. Every block jumps to the
    // shared tail where the caller appends the epilogue and ret.
    let out = b.label();
    b.bind(done);
    b.bytes(&[0xB8]); // mov eax, 42
    b.disp32(42);
    b.jmp8(out);
    for (label, code) in [
        (fail_lookup, 101),
        (fail_range, 102),
        (fail_handler, 103),
        (fail_slot, 110),
        (fail_align, 111),
        (fail_frame, 112),
        (fail_expected, 140),
        (fail_missing, 130),
        (fail_entry, 141),
    ] {
        b.bind(label);
        b.bytes(&[0xB8]); // mov eax, code
        b.disp32(code);
        b.jmp8(out);
    }
    b.bind(out);
    b
}

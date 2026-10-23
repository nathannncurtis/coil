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
//! return address must sit just below the reported stack pointer, that
//! pointer must be 16-byte aligned as the ABI fixes, and
//! `EstablisherFrame` must equal it minus 0x10, since every frame here
//! pushes rbp first. Exit code 42 means the
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

    fn lea_import(&mut self, reg: u8, dll: &str, name: &str) {
        self.bytes(&[0x48, 0x8D, 0x05 | reg << 3]);
        self.rel32(Target::Import {
            dll: dll.into(),
            name: name.into(),
        });
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
    // Hand the walker a nonzero r12: its frame counter is callee-saved,
    // so the walk only passes if the walker zeroes it itself.
    chain.bytes(&[0x41, 0xBC, 0x5A, 0x5A, 0x5A, 0x5A]); // mov r12d, 0x5A5A5A5A
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
    symbols.push(Symbol {
        name: "hexdigits".into(),
        section: SectionId::RData,
        offset: 0,
    });

    let object = ObjectFile {
        text: SectionContent {
            bytes: text,
            align: 16,
        },
        rdata: SectionContent {
            bytes: b"0123456789abcdef".to_vec(),
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
            (
                "kernel32.dll".to_string(),
                vec![
                    "ExitProcess".to_string(),
                    "GetStdHandle".to_string(),
                    "WriteFile".to_string(),
                ],
            ),
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
    const STDOUT: i32 = CTX - 40;
    const MARK: i32 = CTX - 41;
    const WRITTEN: i32 = CTX - 48;
    const HEXBUF: i32 = CTX - 64;

    let mut b = Body::new();

    // One byte of trace per step: the test captures stdout, so a crash
    // inside an ntdll call still shows how far the walk got. Values are
    // printed as sixteen hex characters so no byte is lost to the pipe.
    fn write_hex_slot(b: &mut Body, slot: i32, stdout: i32, written: i32, hexbuf: i32) {
        for i in 0..8 {
            for (shift, at) in [(4, 2 * i), (0, 2 * i + 1)] {
                b.bytes(&[0x0F, 0xB6, 0x85]); // movzx eax, byte [rbp+slot+i]
                b.disp32(slot + i);
                if shift == 4 {
                    b.bytes(&[0xC0, 0xE8, 0x04]); // shr al, 4
                } else {
                    b.bytes(&[0x24, 0x0F]); // and al, 0x0F
                }
                b.lea_rip(1, "hexdigits"); // lea rcx, [rip+hexdigits]
                b.bytes(&[0x0F, 0xB6, 0x04, 0x01]); // movzx eax, byte [rcx+rax]
                b.bytes(&[0x88, 0x85]); // mov [rbp+hexbuf+at], al
                b.disp32(hexbuf + at);
            }
        }
        b.bytes(&[0x50]); // push rax
        b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+STDOUT]
        b.disp32(stdout);
        b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+hexbuf]
        b.disp32(hexbuf);
        b.bytes(&[0x41, 0xB8, 0x10, 0x00, 0x00, 0x00]); // mov r8d, 16
        b.bytes(&[0x4C, 0x8D, 0x8D]); // lea r9, [rbp+WRITTEN]
        b.disp32(written);
        b.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00]);
        b.call_iat("kernel32.dll", "WriteFile");
        b.bytes(&[0x58]); // pop rax
    }

    fn marker(b: &mut Body, byte: u8, stdout: i32, mark: i32, written: i32) {
        b.bytes(&[0x50]); // push rax: WriteFile clobbers the volatile regs
        b.bytes(&[0xC6, 0x85]); // mov byte [rbp+MARK], byte
        b.disp32(mark);
        b.bytes(&[byte]);
        b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+STDOUT]
        b.disp32(stdout);
        b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+MARK]
        b.disp32(mark);
        b.bytes(&[0x41, 0xB8, 0x01, 0x00, 0x00, 0x00]); // mov r8d, 1
        b.bytes(&[0x4C, 0x8D, 0x8D]); // lea r9, [rbp+WRITTEN]
        b.disp32(written);
        b.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00]);
        b.call_iat("kernel32.dll", "WriteFile");
        b.bytes(&[0x58]); // pop rax
    }

    // Fail blocks, forward-referenced by each check.
    let fail_lookup = b.label();
    let fail_range = b.label();
    let fail_handler = b.label();
    let fail_rsp = b.label();
    let fail_rip = b.label();
    let fail_slot = b.label();
    let fail_align = b.label();
    let fail_frame = b.label();
    let fail_expected = b.label();
    let fail_missing = b.label();
    let fail_entry = b.label();
    let done = b.label();

    // r12 counts verified frames. It is callee-saved, so zero it here:
    // the chain hands the walker a poisoned r12, and no r12 value may be
    // inherited from the process-start path.
    b.bytes(&[0x45, 0x31, 0xE4]); // xor r12d, r12d

    // The load base comes from the PEB: gs:[0x60] is the PEB, whose
    // ImageBaseAddress at +0x10 points at the mapped base.
    b.bytes(&[0x65, 0x48, 0x8B, 0x04, 0x25, 0x60, 0x00, 0x00, 0x00]); // mov rax, gs:[0x60]
    b.bytes(&[0x48, 0x8B, 0x40, 0x10]); // mov rax, [rax+0x10]
    b.bytes(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+IMAGE_BASE], rax
    b.disp32(IMAGE_BASE);
    b.bytes(&[0xB9, 0xF5, 0xFF, 0xFF, 0xFF]); // mov ecx, STD_OUTPUT_HANDLE
    b.call_iat("kernel32.dll", "GetStdHandle");
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+STDOUT], rax
    b.disp32(STDOUT);
    marker(&mut b, b'P', STDOUT, MARK, WRITTEN);

    // The CONTEXT must start zeroed with ContextFlags set, not stack
    // garbage: RtlCaptureContext stores over it and RtlVirtualUnwind reads
    // the flags. CONTEXT_FULL | CONTEXT_AMD64 = 0x0010000B at offset 0x30.
    b.bytes(&[0x48, 0x8D, 0xBD]); // lea rdi, [rbp+CTX]
    b.disp32(CTX);
    b.bytes(&[0xB9, 0x9E, 0x00, 0x00, 0x00]); // mov ecx, CONTEXT_SIZE/8
    b.bytes(&[0x31, 0xC0]); // xor eax, eax
    b.bytes(&[0xF3, 0x48, 0xAB]); // rep stosq
    b.bytes(&[0xC7, 0x85]); // mov dword [rbp+CTX+0x30], 0x0010000B
    b.disp32(CTX + 0x30);
    b.disp32(0x0010_000B);

    // Capture the context, then look up walker's own RUNTIME_FUNCTION and
    // check the captured pc lies inside it.
    b.bytes(&[0x48, 0x8D, 0x8D]); // lea rcx, [rbp+CTX]
    b.disp32(CTX);
    b.call_iat("ntdll.dll", "RtlCaptureContext");
    marker(&mut b, b'C', STDOUT, MARK, WRITTEN);

    b.bytes(&[0x48, 0x8B, 0x9D]); // mov rbx, [rbp+CTX_RIP_AT]
    b.disp32(CTX_RIP_AT);
    b.bytes(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+IMAGE_BASE]: ImageBase is out
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x4D, 0x31, 0xC0]); // xor r8, r8
    b.call_iat("ntdll.dll", "RtlLookupFunctionEntry");
    b.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
    b.jcc32(0x84, fail_lookup); // jz
    b.bytes(&[0x49, 0x89, 0xC5]); // mov r13, rax
    marker(&mut b, b'L', STDOUT, MARK, WRITTEN);

    b.bytes(&[0x41, 0x8B, 0x4D, 0x00]); // mov ecx, [r13] (BeginAddress)
    b.bytes(&[0x48, 0x03, 0x8D]); // add rcx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x41, 0x8B, 0x55, 0x04]); // mov edx, [r13+4] (EndAddress)
    b.bytes(&[0x48, 0x03, 0x95]); // add rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x48, 0x39, 0xCB]); // cmp rbx, rcx
    b.jcc32(0x82, fail_range); // jb
    b.bytes(&[0x48, 0x39, 0xDA]); // cmp rdx, rbx
    b.jcc32(0x86, fail_range); // jbe (rbx >= end)

    // Raw dump of what the first RtlVirtualUnwind receives: pc, base,
    // entry, ctx.Rsp, ctx.Rbp. The slots are reused by the loop below.
    b.bytes(&[0x48, 0x89, 0x9D]); // mov [rbp+EXPECT_BEGIN], rbx
    b.disp32(EXPECT_BEGIN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, IMAGE_BASE, STDOUT, WRITTEN, HEXBUF);
    b.bytes(&[0x4C, 0x89, 0xAD]); // mov [rbp+EXPECT_END], r13
    b.disp32(EXPECT_END);
    write_hex_slot(&mut b, EXPECT_END, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, CTX_RSP_AT, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, CTX + 0xA0, STDOUT, WRITTEN, HEXBUF);

    // The runtime view of the RUNTIME_FUNCTION and its UNWIND_INFO header:
    // begin/end, the unwind-data address, then the first 8 bytes there.
    b.bytes(&[0x49, 0x8B, 0x45, 0x00]); // mov rax, [r13] (begin, end)
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+EXPECT_BEGIN], rax
    b.disp32(EXPECT_BEGIN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);
    b.bytes(&[0x41, 0x8B, 0x45, 0x08]); // mov eax, [r13+8] (unwind rva)
    b.bytes(&[0x48, 0x03, 0x85]); // add rax, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+EXPECT_END], rax
    b.disp32(EXPECT_END);
    write_hex_slot(&mut b, EXPECT_END, STDOUT, WRITTEN, HEXBUF);
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_END]
    b.disp32(EXPECT_END);
    b.bytes(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+EXPECT_BEGIN], rax
    b.disp32(EXPECT_BEGIN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);

    // The resolved IAT slots the next calls go through, to prove the
    // loader really bound them (an unresolved slot points into .idata).
    b.lea_import(0, "ntdll.dll", "RtlVirtualUnwind"); // lea rax, [rip+slot]
    b.bytes(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+EXPECT_BEGIN], rax
    b.disp32(EXPECT_BEGIN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);
    b.lea_import(0, "kernel32.dll", "WriteFile");
    b.bytes(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+EXPECT_BEGIN], rax
    b.disp32(EXPECT_BEGIN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);

    let walk_loop = b.label();
    b.bind(walk_loop);
    marker(&mut b, b'V', STDOUT, MARK, WRITTEN);

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
    marker(&mut b, b'U', STDOUT, MARK, WRITTEN);

    b.bytes(&[0x48, 0x8B, 0x9D]); // mov rbx, [rbp+CTX_RIP_AT] (return address)
    b.disp32(CTX_RIP_AT);
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+CTX_RSP_AT]
    b.disp32(CTX_RSP_AT);

    // Plausibility guards: a poisoned context would fault on the reads
    // below, so bound the restored rsp near the live stack and the return
    // address inside .text (this image is far below 0x10000 bytes) and
    // report the unwind step instead.
    b.bytes(&[0x48, 0x8D, 0x84, 0x24, 0x00, 0x00, 0xFF, 0xFF]); // lea rax, [rsp-0x10000]
    b.bytes(&[0x48, 0x39, 0xC1]); // cmp rcx, rax
    b.jcc32(0x82, fail_rsp); // jb
    b.bytes(&[0x48, 0x8D, 0x84, 0x24, 0x00, 0x00, 0x10, 0x00]); // lea rax, [rsp+0x100000]
    b.bytes(&[0x48, 0x39, 0xC1]); // cmp rcx, rax
    b.jcc32(0x87, fail_rsp); // ja
    b.bytes(&[0x48, 0x8B, 0x85]); // mov rax, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x48, 0x05, 0x00, 0x10, 0x00, 0x00]); // add rax, 0x1000 (.text rva)
    b.bytes(&[0x48, 0x39, 0xC3]); // cmp rbx, rax
    b.jcc32(0x82, fail_rip); // jb
    b.bytes(&[0x48, 0x05, 0x00, 0xF0, 0x00, 0x00]); // add rax, 0xF000
    b.bytes(&[0x48, 0x39, 0xD8]); // cmp rax, rbx
    b.jcc32(0x86, fail_rip); // jbe

    b.bytes(&[0x48, 0x39, 0x59, 0xF8]); // cmp [rcx-8], rbx
    b.jcc32(0x85, fail_slot); // jne: the return address is not below rsp
    b.bytes(&[0x48, 0xF7, 0xC1, 0x08, 0x00, 0x00, 0x00]); // test rcx, 8
    b.jcc32(0x85, fail_align); // jnz: the caller's rsp must be 16-aligned
    b.bytes(&[0x48, 0x8B, 0x85]); // mov rax, [rbp+ESTABLISHER]
    b.disp32(ESTABLISHER);
    b.bytes(&[0x48, 0x8D, 0x51, 0xF0]); // lea rdx, [rcx-0x10]
    b.bytes(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
    b.jcc32(0x85, fail_frame); // jne: frame is rsp-0x10 (fp, saved fp, ret)
    marker(&mut b, b'G', STDOUT, MARK, WRITTEN);

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
    marker(&mut b, b'T', STDOUT, MARK, WRITTEN);
    write_hex_slot(&mut b, EXPECT_BEGIN, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, EXPECT_END, STDOUT, WRITTEN, HEXBUF);
    // The dumps call WriteFile, which clobbers the volatile registers:
    // reload the extent rather than trust rdx/rcx, and derive `which`
    // from the nonvolatile r12 only after the calls.
    b.bytes(&[0x48, 0x8B, 0x95]); // mov rdx, [rbp+EXPECT_BEGIN]
    b.disp32(EXPECT_BEGIN);
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_END]
    b.disp32(EXPECT_END);
    b.bytes(&[0x48, 0x39, 0xD3]); // cmp rbx, rdx
    b.jcc32(0x82, fail_expected); // jb
    b.bytes(&[0x48, 0x39, 0xCB]); // cmp rbx, rcx
    b.jcc32(0x83, fail_expected); // jae (rbx >= end)

    b.bytes(&[0x4C, 0x89, 0xE0]); // mov rax, r12
    b.bytes(&[0x48, 0x2D]); // sub rax, DEPTH-1
    b.disp32(DEPTH as i32 - 1);
    b.bytes(&[0x41, 0x81, 0xFC]); // cmp r12d, DEPTH
    b.disp32(DEPTH as i32);
    let have_exit = b.label();
    b.jcc8(0x73, have_exit); // jae
    b.bytes(&[0x48, 0x31, 0xC0]); // xor rax, rax
    b.bind(have_exit);
    b.bytes(&[0x49, 0x89, 0xC3]); // mov r11, rax: `which`
    b.bytes(&[0x45, 0x83, 0xFB, 0x04]); // cmp r11d, 4
    b.jcc32(0x84, done); // je: main's frame reached and verified
    marker(&mut b, b'E', STDOUT, MARK, WRITTEN);

    // The lookup for the return address must return exactly the expected
    // function's RUNTIME_FUNCTION.
    b.bytes(&[0x48, 0x89, 0xD9]); // mov rcx, rbx
    b.bytes(&[0x48, 0x8D, 0x95]); // lea rdx, [rbp+IMAGE_BASE]: ImageBase is out
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x4D, 0x31, 0xC0]); // xor r8, r8
    b.call_iat("ntdll.dll", "RtlLookupFunctionEntry");
    // The trace markers call WriteFile, which clobbers the volatile
    // registers, so read the entry before printing and keep it in CONTEXT
    // registers the walk never reads (Rax, Rcx, Rdx).
    b.bytes(&[0x48, 0x89, 0x85]); // mov [rbp+CTX+0x78], rax (entry pointer)
    b.disp32(CTX + 0x78);
    b.bytes(&[0x44, 0x8B, 0x00]); // mov r8d, [rax] (BeginAddress)
    b.bytes(&[0x44, 0x8B, 0x48, 0x04]); // mov r9d, [rax+4] (EndAddress)
    b.bytes(&[0x48, 0x8B, 0x95]); // mov rdx, [rbp+IMAGE_BASE]
    b.disp32(IMAGE_BASE);
    b.bytes(&[0x49, 0x01, 0xD0]); // add r8, rdx (begin abs)
    b.bytes(&[0x49, 0x01, 0xD1]); // add r9, rdx (end abs)
    b.bytes(&[0x4C, 0x89, 0x85]); // mov [rbp+CTX+0x80], r8
    b.disp32(CTX + 0x80);
    b.bytes(&[0x4C, 0x89, 0x8D]); // mov [rbp+CTX+0x88], r9
    b.disp32(CTX + 0x88);
    marker(&mut b, b'K', STDOUT, MARK, WRITTEN);
    b.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
    b.jcc32(0x84, fail_missing); // jz
    marker(&mut b, b'k', STDOUT, MARK, WRITTEN);
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_BEGIN]
    b.disp32(EXPECT_BEGIN);
    b.bytes(&[0x4C, 0x8B, 0x85]); // mov r8, [rbp+CTX+0x80] (begin abs)
    b.disp32(CTX + 0x80);
    b.bytes(&[0x4C, 0x39, 0xC1]); // cmp rcx, r8
    b.jcc32(0x85, fail_entry); // jne
    b.bytes(&[0x48, 0x8B, 0x8D]); // mov rcx, [rbp+EXPECT_END]
    b.disp32(EXPECT_END);
    b.bytes(&[0x4C, 0x8B, 0x8D]); // mov r9, [rbp+CTX+0x88] (end abs)
    b.disp32(CTX + 0x88);
    b.bytes(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9
    b.jcc32(0x85, fail_entry); // jne

    b.bytes(&[0x49, 0xFF, 0xC4]); // inc r12
    b.bytes(&[0x48, 0x8B, 0x85]); // mov rax, [rbp+CTX+0x78] (entry)
    b.disp32(CTX + 0x78);
    b.bytes(&[0x49, 0x89, 0xC5]); // mov r13, rax
    b.jmp32(walk_loop);

    // Exit code 42: every frame walked back to main and verified. The
    // 1xx/2xx codes name the check that failed. Every block jumps to the
    // shared tail where the caller appends the epilogue and ret.
    let out = b.label();
    b.bind(done);
    marker(&mut b, b'*', STDOUT, MARK, WRITTEN);
    b.bytes(&[0xB8]); // mov eax, 42
    b.disp32(42);
    b.jmp32(out);

    // The CONTEXT slots are dead once a check fails: park the failing
    // registers there and dump the reconstructed frame (return address,
    // stack pointer, establisher and frame pointer).
    let dump_frame = |b: &mut Body| {
        b.bytes(&[0x48, 0x89, 0x9D]); // mov [rbp+CTX_RIP_AT], rbx
        b.disp32(CTX_RIP_AT);
        write_hex_slot(b, CTX_RIP_AT, STDOUT, WRITTEN, HEXBUF);
        write_hex_slot(b, CTX_RSP_AT, STDOUT, WRITTEN, HEXBUF);
        write_hex_slot(b, ESTABLISHER, STDOUT, WRITTEN, HEXBUF);
        write_hex_slot(b, CTX + 0xA0, STDOUT, WRITTEN, HEXBUF);
    };

    // The step-numbered guards encode r12 so the exit code names the
    // unwind step that produced the implausible context.
    for (label, code) in [(fail_rsp, 200), (fail_rip, 220)] {
        b.bind(label);
        dump_frame(&mut b);
        b.bytes(&[0xB8]); // mov eax, code
        b.disp32(code);
        b.bytes(&[0x44, 0x01, 0xE0]); // add eax, r12d
        b.jmp32(out);
    }
    // The entry mismatch dumps what the lookup was handed and returned:
    // the pc (rbx) plus the stashed entry pointer and its absolute extent,
    // captured before the markers. The T dumps earlier in the same
    // iteration hold the expected extent.
    b.bind(fail_entry);
    b.bytes(&[0x48, 0x89, 0x9D]); // mov [rbp+CTX_RIP_AT], rbx (lookup pc)
    b.disp32(CTX_RIP_AT);
    write_hex_slot(&mut b, CTX_RIP_AT, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, CTX + 0x78, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, CTX + 0x80, STDOUT, WRITTEN, HEXBUF);
    write_hex_slot(&mut b, CTX + 0x88, STDOUT, WRITTEN, HEXBUF);
    b.bytes(&[0xB8]); // mov eax, 141
    b.disp32(141);
    b.jmp32(out);
    b.bind(fail_expected);
    dump_frame(&mut b);
    b.bytes(&[0xB8]); // mov eax, 140
    b.disp32(140);
    b.jmp32(out);
    for (label, code) in [
        (fail_lookup, 101),
        (fail_range, 102),
        (fail_handler, 103),
        (fail_slot, 110),
        (fail_align, 111),
        (fail_frame, 112),
        (fail_missing, 130),
    ] {
        b.bind(label);
        b.bytes(&[0xB8]); // mov eax, code
        b.disp32(code);
        b.jmp32(out);
    }
    b.bind(out);
    b
}

//! Differential tests for the x86-64 encoder against iced-x86 (spec section
//! 12.2): for every form, every register class combination (all 16 GPRs, all
//! 16 XMMs) and a sampled set of memory operands, both encoders must produce
//! the same bytes, and iced must decode coil's bytes back to the reference
//! instruction. Cases are generated here, not hand-listed.

use coilc::backend::x86_64::encode::{
    AluOp, Cond, EncodeError, Encoder, Group3Op, Loc, Mem, OpSize, PdOp, Reloc, RelocKind, Scale,
    SdCmpOp, SdOp, ShiftOp, SymbolId, XmmLoc,
};
use coilc::backend::x86_64::regs::{Gpr, Xmm};

use iced_x86::{Code, Decoder, Encoder as Iced, Instruction, MemoryOperand, Register};

/// Byte-compares coil's encoding with iced's, then decodes both and compares
/// the instructions. Returns 1 so suites can count their cases. The builder
/// receives coil's encoding length because iced states RIP-relative operands
/// as target addresses and rebases them by rip + length itself.
fn check_with(build: impl FnOnce(usize) -> Instruction, mine: &[u8]) -> usize {
    let instr = build(mine.len());
    let mut iced = Iced::new(64);
    iced.encode(&instr, 0)
        .expect("iced should encode the reference");
    let expected = iced.take_buffer();
    assert_eq!(mine, expected, "byte mismatch against {instr}");
    let mut a = Decoder::new(64, mine, 0);
    let from_coil = a.decode();
    let mut b = Decoder::new(64, &expected, 0);
    let from_iced = b.decode();
    assert_eq!(
        from_coil, from_iced,
        "decoded instruction mismatch against {instr}"
    );
    1
}

fn check(instr: Instruction, mine: &[u8]) -> usize {
    check_with(|_| instr, mine)
}

fn g(r: Gpr) -> Register {
    match r {
        Gpr::Rax => Register::RAX,
        Gpr::Rcx => Register::RCX,
        Gpr::Rdx => Register::RDX,
        Gpr::Rbx => Register::RBX,
        Gpr::Rsp => Register::RSP,
        Gpr::Rbp => Register::RBP,
        Gpr::Rsi => Register::RSI,
        Gpr::Rdi => Register::RDI,
        Gpr::R8 => Register::R8,
        Gpr::R9 => Register::R9,
        Gpr::R10 => Register::R10,
        Gpr::R11 => Register::R11,
        Gpr::R12 => Register::R12,
        Gpr::R13 => Register::R13,
        Gpr::R14 => Register::R14,
        Gpr::R15 => Register::R15,
    }
}

/// iced names registers per operand size; coil's Gpr maps to the 64-bit name
/// and every sized form picks the matching width.
fn gs(size: OpSize, r: Gpr) -> Register {
    match size {
        OpSize::S8 => match r {
            Gpr::Rax => Register::AL,
            Gpr::Rcx => Register::CL,
            Gpr::Rdx => Register::DL,
            Gpr::Rbx => Register::BL,
            Gpr::Rsp => Register::SPL,
            Gpr::Rbp => Register::BPL,
            Gpr::Rsi => Register::SIL,
            Gpr::Rdi => Register::DIL,
            Gpr::R8 => Register::R8L,
            Gpr::R9 => Register::R9L,
            Gpr::R10 => Register::R10L,
            Gpr::R11 => Register::R11L,
            Gpr::R12 => Register::R12L,
            Gpr::R13 => Register::R13L,
            Gpr::R14 => Register::R14L,
            Gpr::R15 => Register::R15L,
        },
        OpSize::S16 => match r {
            Gpr::Rax => Register::AX,
            Gpr::Rcx => Register::CX,
            Gpr::Rdx => Register::DX,
            Gpr::Rbx => Register::BX,
            Gpr::Rsp => Register::SP,
            Gpr::Rbp => Register::BP,
            Gpr::Rsi => Register::SI,
            Gpr::Rdi => Register::DI,
            Gpr::R8 => Register::R8W,
            Gpr::R9 => Register::R9W,
            Gpr::R10 => Register::R10W,
            Gpr::R11 => Register::R11W,
            Gpr::R12 => Register::R12W,
            Gpr::R13 => Register::R13W,
            Gpr::R14 => Register::R14W,
            Gpr::R15 => Register::R15W,
        },
        OpSize::S32 => match r {
            Gpr::Rax => Register::EAX,
            Gpr::Rcx => Register::ECX,
            Gpr::Rdx => Register::EDX,
            Gpr::Rbx => Register::EBX,
            Gpr::Rsp => Register::ESP,
            Gpr::Rbp => Register::EBP,
            Gpr::Rsi => Register::ESI,
            Gpr::Rdi => Register::EDI,
            Gpr::R8 => Register::R8D,
            Gpr::R9 => Register::R9D,
            Gpr::R10 => Register::R10D,
            Gpr::R11 => Register::R11D,
            Gpr::R12 => Register::R12D,
            Gpr::R13 => Register::R13D,
            Gpr::R14 => Register::R14D,
            Gpr::R15 => Register::R15D,
        },
        OpSize::S64 => g(r),
    }
}

fn x(r: Xmm) -> Register {
    match r {
        Xmm::Xmm0 => Register::XMM0,
        Xmm::Xmm1 => Register::XMM1,
        Xmm::Xmm2 => Register::XMM2,
        Xmm::Xmm3 => Register::XMM3,
        Xmm::Xmm4 => Register::XMM4,
        Xmm::Xmm5 => Register::XMM5,
        Xmm::Xmm6 => Register::XMM6,
        Xmm::Xmm7 => Register::XMM7,
        Xmm::Xmm8 => Register::XMM8,
        Xmm::Xmm9 => Register::XMM9,
        Xmm::Xmm10 => Register::XMM10,
        Xmm::Xmm11 => Register::XMM11,
        Xmm::Xmm12 => Register::XMM12,
        Xmm::Xmm13 => Register::XMM13,
        Xmm::Xmm14 => Register::XMM14,
        Xmm::Xmm15 => Register::XMM15,
    }
}

/// iced memory operand mirroring the hardware displacement rule the encoder
/// implements: no displacement at 0 (except rbp/r13 bases), disp8 when it
/// fits, the full disp32 otherwise (`displ_size` counts bytes, so disp32 is
/// 8 in iced's 64-bit mode). `len` is coil's encoding length, needed to turn
/// a RIP-relative raw displacement into the target address iced expects.
fn m2m(m: Mem, len: usize) -> MemoryOperand {
    let disp = i64::from(m.disp);
    if m.base.is_none() && m.index.is_none() {
        return MemoryOperand::new(
            Register::RIP,
            Register::None,
            1,
            disp + len as i64,
            8,
            false,
            Register::None,
        );
    }
    let displ_size = if m.base.is_none() {
        8
    } else {
        let frame = m.base == Some(Gpr::Rbp) || m.base == Some(Gpr::R13);
        if !frame && disp == 0 {
            0
        } else if (-128..=127).contains(&disp) {
            1
        } else {
            8
        }
    };
    MemoryOperand::new(
        m.base.map_or(Register::None, g),
        m.index.map_or(Register::None, g),
        1 << (m.scale as u32),
        disp,
        displ_size,
        false,
        Register::None,
    )
}

/// The sampled memory operand set: every base over each displacement class,
/// index/scale combinations over the bases with special ModRM/SIB handling,
/// RIP-relative forms and base-less indexed (disp32 absolute) forms.
fn mem_operands() -> Vec<Mem> {
    let mut mems = Vec::new();
    for base in Gpr::ALL {
        for disp in [0, 0x10, -0x80, 0x7F, -0x54321, 0x12345678] {
            mems.push(Mem::disp(base, disp));
        }
    }
    for base in [Gpr::Rax, Gpr::Rsp, Gpr::Rbp, Gpr::R12, Gpr::R13] {
        for index in [Gpr::Rcx, Gpr::R14] {
            for scale in [Scale::X1, Scale::X2, Scale::X4, Scale::X8] {
                for disp in [0, -1, 0x7FFF_FFFF] {
                    mems.push(Mem::indexed(base, index, scale, disp));
                }
            }
        }
    }
    for disp in [0, 0x7F, -0x1000] {
        mems.push(Mem::rip(disp));
    }
    for index in [Gpr::Rcx, Gpr::Rbp, Gpr::R13] {
        for scale in [Scale::X1, Scale::X8] {
            mems.push(Mem {
                base: None,
                index: Some(index),
                scale,
                disp: 0x1000,
                label: None,
            });
        }
    }
    mems
}

fn all_sizes() -> [OpSize; 4] {
    [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64]
}

fn word_sizes() -> [OpSize; 3] {
    [OpSize::S16, OpSize::S32, OpSize::S64]
}

fn mov_rr_code(size: OpSize) -> Code {
    match size {
        OpSize::S8 => Code::Mov_rm8_r8,
        OpSize::S16 => Code::Mov_rm16_r16,
        OpSize::S32 => Code::Mov_rm32_r32,
        OpSize::S64 => Code::Mov_rm64_r64,
    }
}

fn mov_rm_code(size: OpSize) -> Code {
    match size {
        OpSize::S8 => Code::Mov_r8_rm8,
        OpSize::S16 => Code::Mov_r16_rm16,
        OpSize::S32 => Code::Mov_r32_rm32,
        OpSize::S64 => Code::Mov_r64_rm64,
    }
}

fn mov_reg_imm_code(size: OpSize) -> Code {
    match size {
        OpSize::S8 => Code::Mov_r8_imm8,
        OpSize::S16 => Code::Mov_r16_imm16,
        OpSize::S32 => Code::Mov_r32_imm32,
        OpSize::S64 => Code::Mov_r64_imm64,
    }
}

const ALU_RM_REG: [[Code; 4]; 8] = [
    [
        Code::Add_rm8_r8,
        Code::Add_rm16_r16,
        Code::Add_rm32_r32,
        Code::Add_rm64_r64,
    ],
    [
        Code::Or_rm8_r8,
        Code::Or_rm16_r16,
        Code::Or_rm32_r32,
        Code::Or_rm64_r64,
    ],
    [
        Code::Adc_rm8_r8,
        Code::Adc_rm16_r16,
        Code::Adc_rm32_r32,
        Code::Adc_rm64_r64,
    ],
    [
        Code::Sbb_rm8_r8,
        Code::Sbb_rm16_r16,
        Code::Sbb_rm32_r32,
        Code::Sbb_rm64_r64,
    ],
    [
        Code::And_rm8_r8,
        Code::And_rm16_r16,
        Code::And_rm32_r32,
        Code::And_rm64_r64,
    ],
    [
        Code::Sub_rm8_r8,
        Code::Sub_rm16_r16,
        Code::Sub_rm32_r32,
        Code::Sub_rm64_r64,
    ],
    [
        Code::Xor_rm8_r8,
        Code::Xor_rm16_r16,
        Code::Xor_rm32_r32,
        Code::Xor_rm64_r64,
    ],
    [
        Code::Cmp_rm8_r8,
        Code::Cmp_rm16_r16,
        Code::Cmp_rm32_r32,
        Code::Cmp_rm64_r64,
    ],
];

const ALU_REG_RM: [[Code; 4]; 8] = [
    [
        Code::Add_r8_rm8,
        Code::Add_r16_rm16,
        Code::Add_r32_rm32,
        Code::Add_r64_rm64,
    ],
    [
        Code::Or_r8_rm8,
        Code::Or_r16_rm16,
        Code::Or_r32_rm32,
        Code::Or_r64_rm64,
    ],
    [
        Code::Adc_r8_rm8,
        Code::Adc_r16_rm16,
        Code::Adc_r32_rm32,
        Code::Adc_r64_rm64,
    ],
    [
        Code::Sbb_r8_rm8,
        Code::Sbb_r16_rm16,
        Code::Sbb_r32_rm32,
        Code::Sbb_r64_rm64,
    ],
    [
        Code::And_r8_rm8,
        Code::And_r16_rm16,
        Code::And_r32_rm32,
        Code::And_r64_rm64,
    ],
    [
        Code::Sub_r8_rm8,
        Code::Sub_r16_rm16,
        Code::Sub_r32_rm32,
        Code::Sub_r64_rm64,
    ],
    [
        Code::Xor_r8_rm8,
        Code::Xor_r16_rm16,
        Code::Xor_r32_rm32,
        Code::Xor_r64_rm64,
    ],
    [
        Code::Cmp_r8_rm8,
        Code::Cmp_r16_rm16,
        Code::Cmp_r32_rm32,
        Code::Cmp_r64_rm64,
    ],
];

const ALU_RM_IMM8: [[Code; 4]; 8] = [
    [
        Code::Add_rm8_imm8,
        Code::Add_rm16_imm8,
        Code::Add_rm32_imm8,
        Code::Add_rm64_imm8,
    ],
    [
        Code::Or_rm8_imm8,
        Code::Or_rm16_imm8,
        Code::Or_rm32_imm8,
        Code::Or_rm64_imm8,
    ],
    [
        Code::Adc_rm8_imm8,
        Code::Adc_rm16_imm8,
        Code::Adc_rm32_imm8,
        Code::Adc_rm64_imm8,
    ],
    [
        Code::Sbb_rm8_imm8,
        Code::Sbb_rm16_imm8,
        Code::Sbb_rm32_imm8,
        Code::Sbb_rm64_imm8,
    ],
    [
        Code::And_rm8_imm8,
        Code::And_rm16_imm8,
        Code::And_rm32_imm8,
        Code::And_rm64_imm8,
    ],
    [
        Code::Sub_rm8_imm8,
        Code::Sub_rm16_imm8,
        Code::Sub_rm32_imm8,
        Code::Sub_rm64_imm8,
    ],
    [
        Code::Xor_rm8_imm8,
        Code::Xor_rm16_imm8,
        Code::Xor_rm32_imm8,
        Code::Xor_rm64_imm8,
    ],
    [
        Code::Cmp_rm8_imm8,
        Code::Cmp_rm16_imm8,
        Code::Cmp_rm32_imm8,
        Code::Cmp_rm64_imm8,
    ],
];

const ALU_RM_IMM: [[Code; 4]; 8] = [
    [
        Code::Add_rm8_imm8,
        Code::Add_rm16_imm16,
        Code::Add_rm32_imm32,
        Code::Add_rm64_imm32,
    ],
    [
        Code::Or_rm8_imm8,
        Code::Or_rm16_imm16,
        Code::Or_rm32_imm32,
        Code::Or_rm64_imm32,
    ],
    [
        Code::Adc_rm8_imm8,
        Code::Adc_rm16_imm16,
        Code::Adc_rm32_imm32,
        Code::Adc_rm64_imm32,
    ],
    [
        Code::Sbb_rm8_imm8,
        Code::Sbb_rm16_imm16,
        Code::Sbb_rm32_imm32,
        Code::Sbb_rm64_imm32,
    ],
    [
        Code::And_rm8_imm8,
        Code::And_rm16_imm16,
        Code::And_rm32_imm32,
        Code::And_rm64_imm32,
    ],
    [
        Code::Sub_rm8_imm8,
        Code::Sub_rm16_imm16,
        Code::Sub_rm32_imm32,
        Code::Sub_rm64_imm32,
    ],
    [
        Code::Xor_rm8_imm8,
        Code::Xor_rm16_imm16,
        Code::Xor_rm32_imm32,
        Code::Xor_rm64_imm32,
    ],
    [
        Code::Cmp_rm8_imm8,
        Code::Cmp_rm16_imm16,
        Code::Cmp_rm32_imm32,
        Code::Cmp_rm64_imm32,
    ],
];

fn group3_code(op: Group3Op, size: OpSize) -> Code {
    match (op, size) {
        (Group3Op::Not, OpSize::S8) => Code::Not_rm8,
        (Group3Op::Not, OpSize::S16) => Code::Not_rm16,
        (Group3Op::Not, OpSize::S32) => Code::Not_rm32,
        (Group3Op::Not, OpSize::S64) => Code::Not_rm64,
        (Group3Op::Neg, OpSize::S8) => Code::Neg_rm8,
        (Group3Op::Neg, OpSize::S16) => Code::Neg_rm16,
        (Group3Op::Neg, OpSize::S32) => Code::Neg_rm32,
        (Group3Op::Neg, OpSize::S64) => Code::Neg_rm64,
        (Group3Op::Div, OpSize::S8) => Code::Div_rm8,
        (Group3Op::Div, OpSize::S16) => Code::Div_rm16,
        (Group3Op::Div, OpSize::S32) => Code::Div_rm32,
        (Group3Op::Div, OpSize::S64) => Code::Div_rm64,
        (Group3Op::Idiv, OpSize::S8) => Code::Idiv_rm8,
        (Group3Op::Idiv, OpSize::S16) => Code::Idiv_rm16,
        (Group3Op::Idiv, OpSize::S32) => Code::Idiv_rm32,
        (Group3Op::Idiv, OpSize::S64) => Code::Idiv_rm64,
    }
}

fn group3_emit(e: &mut Encoder, op: Group3Op, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
    match op {
        Group3Op::Not => e.not(size, dst),
        Group3Op::Neg => e.neg(size, dst),
        Group3Op::Div => e.div(size, dst),
        Group3Op::Idiv => e.idiv(size, dst),
    }
}

fn shift_code(op: ShiftOp, size: OpSize, by_cl: bool) -> Code {
    match (op, size, by_cl) {
        (ShiftOp::Shl, OpSize::S8, false) => Code::Shl_rm8_imm8,
        (ShiftOp::Shl, OpSize::S8, true) => Code::Shl_rm8_CL,
        (ShiftOp::Shl, OpSize::S16, false) => Code::Shl_rm16_imm8,
        (ShiftOp::Shl, OpSize::S16, true) => Code::Shl_rm16_CL,
        (ShiftOp::Shl, OpSize::S32, false) => Code::Shl_rm32_imm8,
        (ShiftOp::Shl, OpSize::S32, true) => Code::Shl_rm32_CL,
        (ShiftOp::Shl, OpSize::S64, false) => Code::Shl_rm64_imm8,
        (ShiftOp::Shl, OpSize::S64, true) => Code::Shl_rm64_CL,
        (ShiftOp::Shr, OpSize::S8, false) => Code::Shr_rm8_imm8,
        (ShiftOp::Shr, OpSize::S8, true) => Code::Shr_rm8_CL,
        (ShiftOp::Shr, OpSize::S16, false) => Code::Shr_rm16_imm8,
        (ShiftOp::Shr, OpSize::S16, true) => Code::Shr_rm16_CL,
        (ShiftOp::Shr, OpSize::S32, false) => Code::Shr_rm32_imm8,
        (ShiftOp::Shr, OpSize::S32, true) => Code::Shr_rm32_CL,
        (ShiftOp::Shr, OpSize::S64, false) => Code::Shr_rm64_imm8,
        (ShiftOp::Shr, OpSize::S64, true) => Code::Shr_rm64_CL,
        (ShiftOp::Sar, OpSize::S8, false) => Code::Sar_rm8_imm8,
        (ShiftOp::Sar, OpSize::S8, true) => Code::Sar_rm8_CL,
        (ShiftOp::Sar, OpSize::S16, false) => Code::Sar_rm16_imm8,
        (ShiftOp::Sar, OpSize::S16, true) => Code::Sar_rm16_CL,
        (ShiftOp::Sar, OpSize::S32, false) => Code::Sar_rm32_imm8,
        (ShiftOp::Sar, OpSize::S32, true) => Code::Sar_rm32_CL,
        (ShiftOp::Sar, OpSize::S64, false) => Code::Sar_rm64_imm8,
        (ShiftOp::Sar, OpSize::S64, true) => Code::Sar_rm64_CL,
    }
}

const SETCC: [Code; 16] = [
    Code::Seto_rm8,
    Code::Setno_rm8,
    Code::Setb_rm8,
    Code::Setae_rm8,
    Code::Sete_rm8,
    Code::Setne_rm8,
    Code::Setbe_rm8,
    Code::Seta_rm8,
    Code::Sets_rm8,
    Code::Setns_rm8,
    Code::Setp_rm8,
    Code::Setnp_rm8,
    Code::Setl_rm8,
    Code::Setge_rm8,
    Code::Setle_rm8,
    Code::Setg_rm8,
];

const JCC_REL8: [Code; 16] = [
    Code::Jo_rel8_64,
    Code::Jno_rel8_64,
    Code::Jb_rel8_64,
    Code::Jae_rel8_64,
    Code::Je_rel8_64,
    Code::Jne_rel8_64,
    Code::Jbe_rel8_64,
    Code::Ja_rel8_64,
    Code::Js_rel8_64,
    Code::Jns_rel8_64,
    Code::Jp_rel8_64,
    Code::Jnp_rel8_64,
    Code::Jl_rel8_64,
    Code::Jge_rel8_64,
    Code::Jle_rel8_64,
    Code::Jg_rel8_64,
];

const JCC_REL32: [Code; 16] = [
    Code::Jo_rel32_64,
    Code::Jno_rel32_64,
    Code::Jb_rel32_64,
    Code::Jae_rel32_64,
    Code::Je_rel32_64,
    Code::Jne_rel32_64,
    Code::Jbe_rel32_64,
    Code::Ja_rel32_64,
    Code::Js_rel32_64,
    Code::Jns_rel32_64,
    Code::Jp_rel32_64,
    Code::Jnp_rel32_64,
    Code::Jl_rel32_64,
    Code::Jge_rel32_64,
    Code::Jle_rel32_64,
    Code::Jg_rel32_64,
];

const CMOV: [[Code; 3]; 16] = [
    [
        Code::Cmovo_r16_rm16,
        Code::Cmovo_r32_rm32,
        Code::Cmovo_r64_rm64,
    ],
    [
        Code::Cmovno_r16_rm16,
        Code::Cmovno_r32_rm32,
        Code::Cmovno_r64_rm64,
    ],
    [
        Code::Cmovb_r16_rm16,
        Code::Cmovb_r32_rm32,
        Code::Cmovb_r64_rm64,
    ],
    [
        Code::Cmovae_r16_rm16,
        Code::Cmovae_r32_rm32,
        Code::Cmovae_r64_rm64,
    ],
    [
        Code::Cmove_r16_rm16,
        Code::Cmove_r32_rm32,
        Code::Cmove_r64_rm64,
    ],
    [
        Code::Cmovne_r16_rm16,
        Code::Cmovne_r32_rm32,
        Code::Cmovne_r64_rm64,
    ],
    [
        Code::Cmovbe_r16_rm16,
        Code::Cmovbe_r32_rm32,
        Code::Cmovbe_r64_rm64,
    ],
    [
        Code::Cmova_r16_rm16,
        Code::Cmova_r32_rm32,
        Code::Cmova_r64_rm64,
    ],
    [
        Code::Cmovs_r16_rm16,
        Code::Cmovs_r32_rm32,
        Code::Cmovs_r64_rm64,
    ],
    [
        Code::Cmovns_r16_rm16,
        Code::Cmovns_r32_rm32,
        Code::Cmovns_r64_rm64,
    ],
    [
        Code::Cmovp_r16_rm16,
        Code::Cmovp_r32_rm32,
        Code::Cmovp_r64_rm64,
    ],
    [
        Code::Cmovnp_r16_rm16,
        Code::Cmovnp_r32_rm32,
        Code::Cmovnp_r64_rm64,
    ],
    [
        Code::Cmovl_r16_rm16,
        Code::Cmovl_r32_rm32,
        Code::Cmovl_r64_rm64,
    ],
    [
        Code::Cmovge_r16_rm16,
        Code::Cmovge_r32_rm32,
        Code::Cmovge_r64_rm64,
    ],
    [
        Code::Cmovle_r16_rm16,
        Code::Cmovle_r32_rm32,
        Code::Cmovle_r64_rm64,
    ],
    [
        Code::Cmovg_r16_rm16,
        Code::Cmovg_r32_rm32,
        Code::Cmovg_r64_rm64,
    ],
];

const ALL_CONDS: [Cond; 16] = [
    Cond::O,
    Cond::NO,
    Cond::B,
    Cond::AE,
    Cond::E,
    Cond::NE,
    Cond::BE,
    Cond::A,
    Cond::S,
    Cond::NS,
    Cond::P,
    Cond::NP,
    Cond::L,
    Cond::GE,
    Cond::LE,
    Cond::G,
];

fn sd_code(op: SdOp) -> Code {
    match op {
        SdOp::Adds => Code::Addsd_xmm_xmmm64,
        SdOp::Mul => Code::Mulsd_xmm_xmmm64,
        SdOp::Subs => Code::Subsd_xmm_xmmm64,
        SdOp::Div => Code::Divsd_xmm_xmmm64,
        SdOp::Sqrt => Code::Sqrtsd_xmm_xmmm64,
    }
}

#[test]
fn mov_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    // r/r: every pair, every size.
    for size in all_sizes() {
        for dst in Gpr::ALL {
            for src in Gpr::ALL {
                let mut e = Encoder::new();
                e.mov_rr(size, dst, src);
                let instr =
                    Instruction::with2(mov_rr_code(size), gs(size, dst), gs(size, src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
    }
    // r/m (load) and m/r (store): full memory sample at 64 bits, a subset at
    // the other sizes.
    for size in all_sizes() {
        let sample: &[Mem] = &mems;
        for reg in Gpr::ALL {
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.mov_rm(size, reg, mem).unwrap();
                cases += check_with(
                    |len| {
                        Instruction::with2(mov_rm_code(size), gs(size, reg), m2m(mem, len)).unwrap()
                    },
                    &e.finish().unwrap().code,
                );

                let mut e = Encoder::new();
                e.mov_mr(size, mem, reg).unwrap();
                cases += check_with(
                    |len| {
                        Instruction::with2(mov_rr_code(size), m2m(mem, len), gs(size, reg)).unwrap()
                    },
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    // Register immediates: short forms at 8/16/32, sign-extended C7 /0 at 64.
    let imms: &[i64] = &[
        0,
        1,
        -1,
        0x7F,
        -0x80,
        0x80,
        -0x81,
        0x7FFF,
        -0x8000,
        0x8000,
        -0x8001,
        0x7FFF_FFFF,
        -0x8000_0000,
    ];
    for size in all_sizes() {
        let valid: Vec<i64> = imms
            .iter()
            .copied()
            .filter(|i| {
                let (lo, hi) = match size {
                    OpSize::S8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
                    OpSize::S16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
                    OpSize::S32 | OpSize::S64 => (i64::from(i32::MIN), i64::from(i32::MAX)),
                };
                *i >= lo && *i <= hi
            })
            .collect();
        for reg in Gpr::ALL {
            for imm in valid.iter().copied() {
                let mut e = Encoder::new();
                e.mov_reg_imm(size, reg, imm).unwrap();
                let code = if size == OpSize::S64 {
                    Code::Mov_rm64_imm32
                } else {
                    mov_reg_imm_code(size)
                };
                // iced takes the short forms' immediates as unsigned and the
                // C7 /0 form's as a signed i32.
                let instr = if size == OpSize::S8 || size == OpSize::S16 {
                    let imm_op: u32 = if size == OpSize::S8 {
                        imm as u8 as u32
                    } else {
                        imm as u16 as u32
                    };
                    Instruction::with2(code, gs(size, reg), imm_op).unwrap()
                } else {
                    Instruction::with2(code, gs(size, reg), imm as i32).unwrap()
                };
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
    }
    // movabs: full 64-bit immediates.
    for reg in Gpr::ALL {
        for imm in [
            0u64,
            1,
            0x8000_0000,
            u64::MAX,
            i64::MIN as u64,
            0x1234_5678_9ABC_DEF0,
        ] {
            let mut e = Encoder::new();
            e.mov_reg_imm64(reg, imm);
            let instr = Instruction::with2(Code::Mov_r64_imm64, g(reg), imm).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
    }
    println!("mov: {cases} differential cases");
}

#[test]
fn movzx_movsx_movsxd_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    let pairs = [
        (OpSize::S16, OpSize::S8),
        (OpSize::S16, OpSize::S16),
        (OpSize::S32, OpSize::S8),
        (OpSize::S32, OpSize::S16),
        (OpSize::S64, OpSize::S8),
        (OpSize::S64, OpSize::S16),
    ];
    for ext in ["movzx", "movsx"] {
        for (dst_size, src_size) in pairs {
            let code = match (ext, dst_size, src_size) {
                ("movzx", OpSize::S16, OpSize::S8) => Code::Movzx_r16_rm8,
                ("movzx", OpSize::S16, OpSize::S16) => Code::Movzx_r16_rm16,
                ("movzx", OpSize::S32, OpSize::S8) => Code::Movzx_r32_rm8,
                ("movzx", OpSize::S32, OpSize::S16) => Code::Movzx_r32_rm16,
                ("movzx", OpSize::S64, OpSize::S8) => Code::Movzx_r64_rm8,
                ("movzx", OpSize::S64, OpSize::S16) => Code::Movzx_r64_rm16,
                ("movsx", OpSize::S16, OpSize::S8) => Code::Movsx_r16_rm8,
                ("movsx", OpSize::S16, OpSize::S16) => Code::Movsx_r16_rm16,
                ("movsx", OpSize::S32, OpSize::S8) => Code::Movsx_r32_rm8,
                ("movsx", OpSize::S32, OpSize::S16) => Code::Movsx_r32_rm16,
                ("movsx", OpSize::S64, OpSize::S8) => Code::Movsx_r64_rm8,
                ("movsx", OpSize::S64, OpSize::S16) => Code::Movsx_r64_rm16,
                _ => unreachable!("pairs above are exhaustive"),
            };
            for dst in Gpr::ALL {
                for src in Gpr::ALL {
                    let mut e = Encoder::new();
                    let reg_src = Loc::Reg(src);
                    match ext {
                        "movzx" => e.movzx(dst_size, dst, src_size, reg_src).unwrap(),
                        _ => e.movsx(dst_size, dst, src_size, reg_src).unwrap(),
                    }
                    let instr =
                        Instruction::with2(code, gs(dst_size, dst), gs(src_size, src)).unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
            }
            let sample: &[Mem] = &mems;
            for dst in Gpr::ALL {
                for mem in sample.iter().copied() {
                    let mut e = Encoder::new();
                    match ext {
                        "movzx" => e.movzx(dst_size, dst, src_size, Loc::Mem(mem)).unwrap(),
                        _ => e.movsx(dst_size, dst, src_size, Loc::Mem(mem)).unwrap(),
                    }
                    cases += check_with(
                        |len| Instruction::with2(code, gs(dst_size, dst), m2m(mem, len)).unwrap(),
                        &e.finish().unwrap().code,
                    );
                }
            }
        }
    }
    // movsxd: 32-bit source into a 64-bit register.
    for dst in Gpr::ALL {
        for src in Gpr::ALL {
            let mut e = Encoder::new();
            e.movsxd(dst, Loc::Reg(src)).unwrap();
            let instr =
                Instruction::with2(Code::Movsxd_r64_rm32, g(dst), gs(OpSize::S32, src)).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
    }
    for dst in Gpr::ALL {
        for mem in mems.iter().copied() {
            let mut e = Encoder::new();
            e.movsxd(dst, Loc::Mem(mem)).unwrap();
            cases += check_with(
                |len| Instruction::with2(Code::Movsxd_r64_rm32, g(dst), m2m(mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    println!("movzx/movsx/movsxd: {cases} differential cases");
}

#[test]
fn lea_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for size in [OpSize::S16, OpSize::S32, OpSize::S64] {
        let sample: &[Mem] = &mems;
        let code = match size {
            OpSize::S16 => Code::Lea_r16_m,
            OpSize::S32 => Code::Lea_r32_m,
            _ => Code::Lea_r64_m,
        };
        for dst in Gpr::ALL {
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.lea(size, dst, mem).unwrap();
                cases += check_with(
                    |len| Instruction::with2(code, gs(size, dst), m2m(mem, len)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    println!("lea: {cases} differential cases");
}

#[test]
fn alu_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    let ops = [
        AluOp::Add,
        AluOp::Or,
        AluOp::Adc,
        AluOp::Sbb,
        AluOp::And,
        AluOp::Sub,
        AluOp::Xor,
        AluOp::Cmp,
    ];
    for op in ops {
        let oi = op as usize;
        // r/r: every register against a stride of partners, every size.
        for size in all_sizes() {
            let si = size as usize;
            for dst in Gpr::ALL {
                for src in Gpr::ALL.iter() {
                    let mut e = Encoder::new();
                    e.alu_rr(op, size, dst, *src);
                    let instr =
                        Instruction::with2(ALU_RM_REG[oi][si], gs(size, dst), gs(size, *src))
                            .unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
            }
        }
        // r/m and m/r over the memory sample.
        for size in all_sizes() {
            let si = size as usize;
            let sample: &[Mem] = &mems;
            for reg in Gpr::ALL {
                for mem in sample.iter().copied() {
                    let mut e = Encoder::new();
                    e.alu_rm(op, size, mem, reg).unwrap();
                    cases += check_with(
                        |len| {
                            Instruction::with2(ALU_RM_REG[oi][si], m2m(mem, len), gs(size, reg))
                                .unwrap()
                        },
                        &e.finish().unwrap().code,
                    );

                    let mut e = Encoder::new();
                    e.alu_mr(op, size, reg, mem).unwrap();
                    cases += check_with(
                        |len| {
                            Instruction::with2(ALU_REG_RM[oi][si], gs(size, reg), m2m(mem, len))
                                .unwrap()
                        },
                        &e.finish().unwrap().code,
                    );
                }
            }
        }
        // Register and memory immediates: the sign-extended imm8 form and the
        // operand-size form (which needs an immediate outside i8).
        for size in all_sizes() {
            let si = size as usize;
            let (imm8s, imms): (Vec<i64>, Vec<i64>) = match size {
                OpSize::S8 => (vec![0, 1, -1, 0x7F, -0x80], vec![0, 1, -1, 0x7F, -0x80]),
                OpSize::S16 => (vec![0, 0x7F, -0x80], vec![0x80, -0x81, 0x7FFF, -0x8000]),
                OpSize::S32 | OpSize::S64 => (
                    vec![0, 0x7F, -0x80],
                    vec![0x80, -0x81, 0x7FFF_FFFF, -0x8000_0000],
                ),
            };
            for reg in Gpr::ALL.iter() {
                for imm in imm8s.iter().copied() {
                    let mut e = Encoder::new();
                    e.alu_reg_imm(op, size, *reg, imm).unwrap();
                    let instr =
                        Instruction::with2(ALU_RM_IMM8[oi][si], gs(size, *reg), imm).unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
                for imm in imms.iter().copied() {
                    let mut e = Encoder::new();
                    e.alu_reg_imm(op, size, *reg, imm).unwrap();
                    let instr =
                        Instruction::with2(ALU_RM_IMM[oi][si], gs(size, *reg), imm).unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
            }
            for mem in mems.iter().copied() {
                for imm in imm8s.iter().copied() {
                    let mut e = Encoder::new();
                    e.alu_mem_imm(op, size, mem, imm).unwrap();
                    cases += check_with(
                        |len| {
                            Instruction::with2(ALU_RM_IMM8[oi][si], m2m(mem, len), imm as i32)
                                .unwrap()
                        },
                        &e.finish().unwrap().code,
                    );
                }
                for imm in imms.iter().copied() {
                    let mut e = Encoder::new();
                    e.alu_mem_imm(op, size, mem, imm).unwrap();
                    cases += check_with(
                        |len| {
                            Instruction::with2(ALU_RM_IMM[oi][si], m2m(mem, len), imm as i32)
                                .unwrap()
                        },
                        &e.finish().unwrap().code,
                    );
                }
            }
        }
    }
    println!("alu (add/or/adc/sbb/and/sub/xor/cmp): {cases} differential cases");
}

#[test]
fn test_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for size in all_sizes() {
        let code_rr = match size {
            OpSize::S8 => Code::Test_rm8_r8,
            OpSize::S16 => Code::Test_rm16_r16,
            OpSize::S32 => Code::Test_rm32_r32,
            OpSize::S64 => Code::Test_rm64_r64,
        };
        for a in Gpr::ALL {
            for b in Gpr::ALL.iter() {
                let mut e = Encoder::new();
                e.test_rr(size, a, *b);
                let instr = Instruction::with2(code_rr, gs(size, a), gs(size, *b)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        let sample: &[Mem] = &mems;
        for reg in Gpr::ALL {
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.test_rm(size, reg, mem).unwrap();
                cases += check_with(
                    |len| Instruction::with2(code_rr, m2m(mem, len), gs(size, reg)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
        let code_imm = match size {
            OpSize::S8 => Code::Test_rm8_imm8,
            OpSize::S16 => Code::Test_rm16_imm16,
            OpSize::S32 => Code::Test_rm32_imm32,
            OpSize::S64 => Code::Test_rm64_imm32,
        };
        let imms: &[i64] = match size {
            OpSize::S8 => &[0, 1, -1, 0x7F, -0x80],
            OpSize::S16 => &[0, 1, 0x7F, -0x80, 0x7FFF, -0x8000],
            OpSize::S32 | OpSize::S64 => &[0, 1, 0x7F, -0x80, 0x7FFF_FFFF, -0x8000_0000],
        };
        for reg in Gpr::ALL.iter() {
            for imm in imms.iter().copied() {
                let mut e = Encoder::new();
                e.test_reg_imm(size, *reg, imm).unwrap();
                let instr = Instruction::with2(code_imm, gs(size, *reg), imm).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for mem in mems.iter().copied() {
            for imm in imms.iter().copied() {
                let mut e = Encoder::new();
                e.test_mem_imm(size, mem, imm).unwrap();
                cases += check_with(
                    |len| Instruction::with2(code_imm, m2m(mem, len), imm as i32).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    println!("test: {cases} differential cases");
}

#[test]
fn imul_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for size in word_sizes() {
        let code_rr = match size {
            OpSize::S16 => Code::Imul_r16_rm16,
            OpSize::S32 => Code::Imul_r32_rm32,
            _ => Code::Imul_r64_rm64,
        };
        for dst in Gpr::ALL {
            for src in Gpr::ALL.iter() {
                let mut e = Encoder::new();
                e.imul_rr(size, dst, *src).unwrap();
                let instr = Instruction::with2(code_rr, gs(size, dst), gs(size, *src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        let sample: &[Mem] = &mems;
        for dst in Gpr::ALL {
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.imul_rm(size, dst, mem).unwrap();
                cases += check_with(
                    |len| Instruction::with2(code_rr, gs(size, dst), m2m(mem, len)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
        let imm8s = [1i64, -1, 0x7F, -0x80];
        let imms: &[i64] = match size {
            OpSize::S16 => &[0x80, -0x81, 0x7FFF, -0x8000],
            _ => &[0x80, -0x81, 0x7FFF_FFFF, -0x8000_0000],
        };
        for dst in Gpr::ALL {
            for src in Gpr::ALL.iter() {
                for imm in imm8s {
                    let code = match size {
                        OpSize::S16 => Code::Imul_r16_rm16_imm8,
                        OpSize::S32 => Code::Imul_r32_rm32_imm8,
                        _ => Code::Imul_r64_rm64_imm8,
                    };
                    let mut e = Encoder::new();
                    e.imul_rr_imm(size, dst, *src, imm).unwrap();
                    let instr = Instruction::with3(code, gs(size, dst), gs(size, *src), imm as i32)
                        .unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
                for imm in imms.iter().copied() {
                    let code = match size {
                        OpSize::S16 => Code::Imul_r16_rm16_imm16,
                        OpSize::S32 => Code::Imul_r32_rm32_imm32,
                        _ => Code::Imul_r64_rm64_imm32,
                    };
                    let mut e = Encoder::new();
                    e.imul_rr_imm(size, dst, *src, imm).unwrap();
                    let instr = Instruction::with3(code, gs(size, dst), gs(size, *src), imm as i32)
                        .unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
            }
        }
    }
    println!("imul: {cases} differential cases");
}

#[test]
fn unary_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    let ops = [Group3Op::Not, Group3Op::Neg, Group3Op::Div, Group3Op::Idiv];
    for op in ops {
        for size in all_sizes() {
            for reg in Gpr::ALL {
                let mut e = Encoder::new();
                group3_emit(&mut e, op, size, Loc::Reg(reg)).unwrap();
                let instr = Instruction::with1(group3_code(op, size), gs(size, reg)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
            let sample: &[Mem] = &mems;
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                group3_emit(&mut e, op, size, Loc::Mem(mem)).unwrap();
                cases += check_with(
                    |len| Instruction::with1(group3_code(op, size), m2m(mem, len)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    let mut e = Encoder::new();
    e.cqo();
    cases += check(Instruction::with(Code::Cqo), &e.finish().unwrap().code);
    println!("not/neg/div/idiv/cqo: {cases} differential cases");
}

#[test]
fn shift_and_bt_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for op in [ShiftOp::Shl, ShiftOp::Shr, ShiftOp::Sar] {
        for size in all_sizes() {
            for reg in Gpr::ALL {
                let mut e = Encoder::new();
                e.shift_cl(op, size, Loc::Reg(reg)).unwrap();
                let instr =
                    Instruction::with2(shift_code(op, size, true), gs(size, reg), Register::CL)
                        .unwrap();
                cases += check(instr, &e.finish().unwrap().code);

                let mut e = Encoder::new();
                e.shift_imm(op, size, Loc::Reg(reg), 3).unwrap();
                let instr =
                    Instruction::with2(shift_code(op, size, false), gs(size, reg), 3u32).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
            let sample: &[Mem] = &mems;
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.shift_cl(op, size, Loc::Mem(mem)).unwrap();
                cases += check_with(
                    |len| {
                        Instruction::with2(shift_code(op, size, true), m2m(mem, len), Register::CL)
                            .unwrap()
                    },
                    &e.finish().unwrap().code,
                );

                let mut e = Encoder::new();
                e.shift_imm(op, size, Loc::Mem(mem), 1).unwrap();
                cases += check_with(
                    |len| {
                        Instruction::with2(shift_code(op, size, false), m2m(mem, len), 1u32)
                            .unwrap()
                    },
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    for size in word_sizes() {
        let code_rr = match size {
            OpSize::S16 => Code::Bt_rm16_r16,
            OpSize::S32 => Code::Bt_rm32_r32,
            _ => Code::Bt_rm64_r64,
        };
        let code_imm = match size {
            OpSize::S16 => Code::Bt_rm16_imm8,
            OpSize::S32 => Code::Bt_rm32_imm8,
            _ => Code::Bt_rm64_imm8,
        };
        for dst in Gpr::ALL {
            for src in Gpr::ALL.iter() {
                let mut e = Encoder::new();
                e.bt_rr(size, dst, *src).unwrap();
                let instr = Instruction::with2(code_rr, gs(size, dst), gs(size, *src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
            let mut e = Encoder::new();
            e.bt_imm(size, Loc::Reg(dst), 5).unwrap();
            let instr = Instruction::with2(code_imm, gs(size, dst), 5u32).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
        for mem in mems.iter().copied() {
            let mut e = Encoder::new();
            e.bt_rm(size, mem, Gpr::R11).unwrap();
            cases += check_with(
                |len| Instruction::with2(code_rr, m2m(mem, len), gs(size, Gpr::R11)).unwrap(),
                &e.finish().unwrap().code,
            );

            let mut e = Encoder::new();
            e.bt_imm(size, Loc::Mem(mem), 63).unwrap();
            cases += check_with(
                |len| Instruction::with2(code_imm, m2m(mem, len), 63u32).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    println!("shift/bt: {cases} differential cases");
}

#[test]
fn setcc_cmovcc_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for (i, cc) in ALL_CONDS.into_iter().enumerate() {
        for reg in Gpr::ALL {
            let mut e = Encoder::new();
            e.setcc(cc, Loc::Reg(reg)).unwrap();
            let instr = Instruction::with1(SETCC[i], gs(OpSize::S8, reg)).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
        for mem in mems.iter().copied() {
            let mut e = Encoder::new();
            e.setcc(cc, Loc::Mem(mem)).unwrap();
            cases += check_with(
                |len| Instruction::with1(SETCC[i], m2m(mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    for cc in ALL_CONDS {
        for size in word_sizes() {
            let code = CMOV[cc as usize][size as usize - 1];
            for dst in Gpr::ALL.iter() {
                for src in Gpr::ALL.iter() {
                    let mut e = Encoder::new();
                    e.cmovcc(cc, size, *dst, Loc::Reg(*src)).unwrap();
                    let instr = Instruction::with2(code, gs(size, *dst), gs(size, *src)).unwrap();
                    cases += check(instr, &e.finish().unwrap().code);
                }
            }
            for mem in mems.iter() {
                let mut e = Encoder::new();
                e.cmovcc(cc, size, Gpr::R9, Loc::Mem(*mem)).unwrap();
                cases += check_with(
                    |len| Instruction::with2(code, gs(size, Gpr::R9), m2m(*mem, len)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    println!("setcc/cmovcc: {cases} differential cases");
}

#[test]
fn branch_forms() {
    let mut cases = 0;
    for (i, cc) in ALL_CONDS.into_iter().enumerate() {
        // Backward branch with the target already bound at the branch: rel8.
        let mut e = Encoder::new();
        let label = e.create_label();
        e.bind_label(label).unwrap();
        e.jcc(cc, label).unwrap();
        cases += check(
            Instruction::with_branch(JCC_REL8[i], 0).unwrap(),
            &e.finish().unwrap().code,
        );

        // Forward branch across filler: a rel32 fixup resolved by finish().
        let mut e = Encoder::new();
        let label = e.create_label();
        e.jcc(cc, label).unwrap();
        e.nop(9).unwrap();
        e.bind_label(label).unwrap();
        cases += check(
            Instruction::with_branch(JCC_REL32[i], 15).unwrap(),
            &e.finish().unwrap().code[..6],
        );
    }
    // jmp rel8 and rel32.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.bind_label(label).unwrap();
    e.jmp(label).unwrap();
    cases += check(
        Instruction::with_branch(Code::Jmp_rel8_64, 0).unwrap(),
        &e.finish().unwrap().code,
    );

    let mut e = Encoder::new();
    let label = e.create_label();
    e.jmp(label).unwrap();
    e.nop(7).unwrap();
    e.bind_label(label).unwrap();
    cases += check(
        Instruction::with_branch(Code::Jmp_rel32_64, 12).unwrap(),
        &e.finish().unwrap().code[..5],
    );

    // A bound backward branch beyond rel8 range still takes the rel32 form.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.bind_label(label).unwrap();
    for _ in 0..15 {
        e.nop(9).unwrap();
    }
    e.jmp(label).unwrap();
    let buf = e.finish().unwrap().code;
    cases += check(
        Instruction::with_branch(Code::Jmp_rel32_64, -135i64 as u64).unwrap(),
        &buf[buf.len() - 5..],
    );
    println!("jcc/jmp: {cases} differential cases");
}

#[test]
fn call_ret_push_pop_xchg_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    // call rel32, backward and forward.
    let mut e = Encoder::new();
    e.nop(3).unwrap();
    let label = e.create_label();
    e.call(label).unwrap();
    e.bind_label(label).unwrap();
    cases += check(
        Instruction::with_branch(Code::Call_rel32_64, 5).unwrap(),
        &e.finish().unwrap().code[3..],
    );
    let mut e = Encoder::new();
    let label = e.create_label();
    e.call(label).unwrap();
    e.nop(5).unwrap();
    e.bind_label(label).unwrap();
    cases += check(
        Instruction::with_branch(Code::Call_rel32_64, 10).unwrap(),
        &e.finish().unwrap().code[..5],
    );
    // call r/m.
    for reg in Gpr::ALL {
        let mut e = Encoder::new();
        e.call_rm(Loc::Reg(reg)).unwrap();
        let instr = Instruction::with1(Code::Call_rm64, g(reg)).unwrap();
        cases += check(instr, &e.finish().unwrap().code);
    }
    for mem in mems.iter().copied() {
        let mut e = Encoder::new();
        e.call_rm(Loc::Mem(mem)).unwrap();
        cases += check_with(
            |len| Instruction::with1(Code::Call_rm64, m2m(mem, len)).unwrap(),
            &e.finish().unwrap().code,
        );
    }
    let mut e = Encoder::new();
    e.ret();
    cases += check(Instruction::with(Code::Retnq), &e.finish().unwrap().code);
    for reg in Gpr::ALL {
        let mut e = Encoder::new();
        e.push(reg);
        cases += check(
            Instruction::with1(Code::Push_r64, g(reg)).unwrap(),
            &e.finish().unwrap().code,
        );
        let mut e = Encoder::new();
        e.pop(reg);
        cases += check(
            Instruction::with1(Code::Pop_r64, g(reg)).unwrap(),
            &e.finish().unwrap().code,
        );
    }
    for size in all_sizes() {
        let code = match size {
            OpSize::S8 => Code::Xchg_rm8_r8,
            OpSize::S16 => Code::Xchg_rm16_r16,
            OpSize::S32 => Code::Xchg_rm32_r32,
            OpSize::S64 => Code::Xchg_rm64_r64,
        };
        for a in Gpr::ALL {
            for b in Gpr::ALL.iter() {
                let mut e = Encoder::new();
                e.xchg(size, Loc::Reg(a), *b).unwrap();
                let instr = Instruction::with2(code, gs(size, a), gs(size, *b)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        let sample: &[Mem] = &mems;
        for mem in sample.iter().copied() {
            let mut e = Encoder::new();
            e.xchg(size, Loc::Mem(mem), Gpr::R10).unwrap();
            cases += check_with(
                |len| Instruction::with2(code, m2m(mem, len), gs(size, Gpr::R10)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    println!("call/ret/push/pop/xchg: {cases} differential cases");
}

#[test]
fn lock_cmpxchg_xadd_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    for size in all_sizes() {
        let cx_code = match size {
            OpSize::S8 => Code::Cmpxchg_rm8_r8,
            OpSize::S16 => Code::Cmpxchg_rm16_r16,
            OpSize::S32 => Code::Cmpxchg_rm32_r32,
            OpSize::S64 => Code::Cmpxchg_rm64_r64,
        };
        let xa_code = match size {
            OpSize::S8 => Code::Xadd_rm8_r8,
            OpSize::S16 => Code::Xadd_rm16_r16,
            OpSize::S32 => Code::Xadd_rm32_r32,
            OpSize::S64 => Code::Xadd_rm64_r64,
        };
        let sample: &[Mem] = &mems;
        for src in Gpr::ALL {
            for mem in sample.iter().copied() {
                let mut e = Encoder::new();
                e.lock_cmpxchg(size, mem, src).unwrap();
                cases += check_with(
                    |len| {
                        let mut instr =
                            Instruction::with2(cx_code, m2m(mem, len), gs(size, src)).unwrap();
                        instr.set_has_lock_prefix(true);
                        instr
                    },
                    &e.finish().unwrap().code,
                );

                let mut e = Encoder::new();
                e.lock_xadd(size, mem, src).unwrap();
                cases += check_with(
                    |len| {
                        let mut instr =
                            Instruction::with2(xa_code, m2m(mem, len), gs(size, src)).unwrap();
                        instr.set_has_lock_prefix(true);
                        instr
                    },
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    println!("lock cmpxchg/xadd: {cases} differential cases");
}

#[test]
fn misc_forms() {
    let mut cases = 0;
    let mut e = Encoder::new();
    e.mfence();
    cases += check(Instruction::with(Code::Mfence), &e.finish().unwrap().code);
    let mut e = Encoder::new();
    e.int3();
    cases += check(Instruction::with(Code::Int3), &e.finish().unwrap().code);
    let mut e = Encoder::new();
    e.ud2();
    cases += check(Instruction::with(Code::Ud2), &e.finish().unwrap().code);
    // Multi-byte nop padding.
    for len in 1..=9usize {
        let mut e = Encoder::new();
        e.nop(len).unwrap();
        let instr = match len {
            1 => Instruction::with(Code::Nopd),
            2 => Instruction::with(Code::Nopw),
            3 => Instruction::with1(
                Code::Nop_rm32,
                MemoryOperand::new(
                    Register::RAX,
                    Register::None,
                    1,
                    0,
                    0,
                    false,
                    Register::None,
                ),
            )
            .unwrap(),
            4 => Instruction::with1(
                Code::Nop_rm32,
                MemoryOperand::new(
                    Register::RAX,
                    Register::None,
                    1,
                    0,
                    1,
                    false,
                    Register::None,
                ),
            )
            .unwrap(),
            5 | 6 => Instruction::with1(
                if len == 5 {
                    Code::Nop_rm32
                } else {
                    Code::Nop_rm16
                },
                MemoryOperand::new(Register::RAX, Register::RAX, 1, 0, 1, false, Register::None),
            )
            .unwrap(),
            7 => Instruction::with1(
                Code::Nop_rm32,
                MemoryOperand::new(
                    Register::RAX,
                    Register::None,
                    1,
                    0,
                    8,
                    false,
                    Register::None,
                ),
            )
            .unwrap(),
            8 | 9 => Instruction::with1(
                if len == 8 {
                    Code::Nop_rm32
                } else {
                    Code::Nop_rm16
                },
                MemoryOperand::new(Register::RAX, Register::RAX, 1, 0, 8, false, Register::None),
            )
            .unwrap(),
            _ => unreachable!("loop bound is 1..=9"),
        };
        cases += check(instr, &e.finish().unwrap().code);
    }
    println!("mfence/int3/ud2/nop: {cases} differential cases");
}

#[test]
fn sse_forms() {
    let mut cases = 0;
    let mems = mem_operands();
    // movsd register-register, load and store.
    for dst in Xmm::ALL {
        for src in Xmm::ALL {
            let mut e = Encoder::new();
            e.movsd_rr(dst, src);
            let instr = Instruction::with2(Code::Movsd_xmm_xmmm64, x(dst), x(src)).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
    }
    for r in Xmm::ALL {
        for mem in mems.iter().copied() {
            let mut e = Encoder::new();
            e.movsd_rm(r, mem).unwrap();
            cases += check_with(
                |len| Instruction::with2(Code::Movsd_xmm_xmmm64, x(r), m2m(mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );

            let mut e = Encoder::new();
            e.movsd_mr(mem, r).unwrap();
            cases += check_with(
                |len| Instruction::with2(Code::Movsd_xmmm64_xmm, m2m(mem, len), x(r)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    // movq both ways between XMM and GPR.
    for r in Xmm::ALL {
        for gpr in Gpr::ALL {
            let mut e = Encoder::new();
            e.movq_xmm_gpr(r, gpr);
            let instr = Instruction::with2(Code::Movq_xmm_rm64, x(r), g(gpr)).unwrap();
            cases += check(instr, &e.finish().unwrap().code);

            let mut e = Encoder::new();
            e.movq_gpr_xmm(gpr, r);
            let instr = Instruction::with2(Code::Movq_rm64_xmm, g(gpr), x(r)).unwrap();
            cases += check(instr, &e.finish().unwrap().code);
        }
    }
    // Scalar arithmetic.
    for op in [SdOp::Adds, SdOp::Mul, SdOp::Subs, SdOp::Div, SdOp::Sqrt] {
        for dst in Xmm::ALL {
            for src in Xmm::ALL.iter() {
                let mut e = Encoder::new();
                e.sd_rr(op, dst, *src);
                let instr = Instruction::with2(sd_code(op), x(dst), x(*src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for dst in Xmm::ALL.iter() {
            for mem in mems.iter() {
                let mut e = Encoder::new();
                e.sd_rm(op, *dst, *mem).unwrap();
                cases += check_with(
                    |len| Instruction::with2(sd_code(op), x(*dst), m2m(*mem, len)).unwrap(),
                    &e.finish().unwrap().code,
                );
            }
        }
    }
    // Compares.
    for op in [SdCmpOp::Ucomis, SdCmpOp::Comis] {
        let code = match op {
            SdCmpOp::Ucomis => Code::Ucomisd_xmm_xmmm64,
            SdCmpOp::Comis => Code::Comisd_xmm_xmmm64,
        };
        for dst in Xmm::ALL {
            for src in Xmm::ALL.iter() {
                let mut e = Encoder::new();
                e.sd_cmp(op, dst, XmmLoc::Xmm(*src)).unwrap();
                let instr = Instruction::with2(code, x(dst), x(*src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for mem in mems.iter() {
            let mut e = Encoder::new();
            e.sd_cmp(op, Xmm::Xmm3, XmmLoc::Mem(*mem)).unwrap();
            cases += check_with(
                |len| Instruction::with2(code, x(Xmm::Xmm3), m2m(*mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    // Integer conversions, 32- and 64-bit sources and destinations.
    for size in [OpSize::S32, OpSize::S64] {
        let cvt_code = match size {
            OpSize::S32 => Code::Cvtsi2sd_xmm_rm32,
            _ => Code::Cvtsi2sd_xmm_rm64,
        };
        let t_code = match size {
            OpSize::S32 => Code::Cvttsd2si_r32_xmmm64,
            _ => Code::Cvttsd2si_r64_xmmm64,
        };
        for dst in Xmm::ALL {
            for src in Gpr::ALL {
                let mut e = Encoder::new();
                e.cvtsi2sd(dst, size, Loc::Reg(src)).unwrap();
                let instr = Instruction::with2(cvt_code, x(dst), gs(size, src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for dst in Gpr::ALL {
            for src in Xmm::ALL.iter() {
                let mut e = Encoder::new();
                e.cvttsd2si(dst, size, XmmLoc::Xmm(*src)).unwrap();
                let instr = Instruction::with2(t_code, gs(size, dst), x(*src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for mem in mems.iter() {
            let mut e = Encoder::new();
            e.cvtsi2sd(Xmm::Xmm5, size, Loc::Mem(*mem)).unwrap();
            cases += check_with(
                |len| Instruction::with2(cvt_code, x(Xmm::Xmm5), m2m(*mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );

            let mut e = Encoder::new();
            e.cvttsd2si(Gpr::R12, size, XmmLoc::Mem(*mem)).unwrap();
            cases += check_with(
                |len| Instruction::with2(t_code, gs(size, Gpr::R12), m2m(*mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    // Packed logic and roundsd (SSE4.1).
    for op in [PdOp::And, PdOp::Xor] {
        let code = match op {
            PdOp::And => Code::Andpd_xmm_xmmm128,
            PdOp::Xor => Code::Xorpd_xmm_xmmm128,
        };
        for dst in Xmm::ALL {
            for src in Xmm::ALL.iter() {
                let mut e = Encoder::new();
                e.pd_rr(op, dst, XmmLoc::Xmm(*src)).unwrap();
                let instr = Instruction::with2(code, x(dst), x(*src)).unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for mem in mems.iter() {
            let mut e = Encoder::new();
            e.pd_rr(op, Xmm::Xmm7, XmmLoc::Mem(*mem)).unwrap();
            cases += check_with(
                |len| Instruction::with2(code, x(Xmm::Xmm7), m2m(*mem, len)).unwrap(),
                &e.finish().unwrap().code,
            );
        }
    }
    for mode in [0u8, 4, 8, 12] {
        for dst in Xmm::ALL {
            for src in Xmm::ALL.iter() {
                let mut e = Encoder::new();
                e.roundsd(dst, XmmLoc::Xmm(*src), mode).unwrap();
                let instr = Instruction::with3(
                    Code::Roundsd_xmm_xmmm64_imm8,
                    x(dst),
                    x(*src),
                    u32::from(mode),
                )
                .unwrap();
                cases += check(instr, &e.finish().unwrap().code);
            }
        }
        for mem in mems.iter() {
            let mut e = Encoder::new();
            e.roundsd(Xmm::Xmm0, XmmLoc::Mem(*mem), mode).unwrap();
            cases += check_with(
                |len| {
                    Instruction::with3(
                        Code::Roundsd_xmm_xmmm64_imm8,
                        x(Xmm::Xmm0),
                        m2m(*mem, len),
                        u32::from(mode),
                    )
                    .unwrap()
                },
                &e.finish().unwrap().code,
            );
        }
    }
    println!("sse: {cases} differential cases");
}

#[test]
fn labels_fixups_and_relocs() {
    let mut cases = 0;
    // RIP-relative reference to a forward label through lea: the label sits 3
    // bytes past the end of the 7-byte lea.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.lea(OpSize::S64, Gpr::Rax, Mem::rip_label(label)).unwrap();
    e.nop(3).unwrap();
    e.bind_label(label).unwrap();
    let finished = e.finish().unwrap();
    assert_eq!(
        finished.code,
        [0x48, 0x8D, 0x05, 0x03, 0x00, 0x00, 0x00, 0x0F, 0x1F, 0x00]
    );
    cases += check(
        Instruction::with2(
            Code::Lea_r64_m,
            Register::RAX,
            MemoryOperand::new(
                Register::RIP,
                Register::None,
                1,
                10,
                8,
                false,
                Register::None,
            ),
        )
        .unwrap(),
        &finished.code[..7],
    );

    // RIP-relative memory operand followed by an immediate: the fixup base is
    // the end of the whole instruction.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.alu_mem_imm(AluOp::Cmp, OpSize::S64, Mem::rip_label(label), 5)
        .unwrap();
    e.nop(4).unwrap();
    e.bind_label(label).unwrap();
    let finished = e.finish().unwrap();
    assert_eq!(
        finished.code,
        [
            0x48, 0x83, 0x3D, 0x04, 0x00, 0x00, 0x00, 0x05, 0x0F, 0x1F, 0x40, 0x00,
        ]
    );
    cases += check(
        Instruction::with2(
            Code::Cmp_rm64_imm8,
            MemoryOperand::new(
                Register::RIP,
                Register::None,
                1,
                12,
                8,
                false,
                Register::None,
            ),
            5i32,
        )
        .unwrap(),
        &finished.code[..8],
    );

    // External symbols: call gets Rel32 at the displacement, movabs gets
    // Abs64 at the immediate.
    let sym = SymbolId(7);
    let mut e = Encoder::new();
    e.call_symbol(sym);
    e.mov_reg_symbol_addr(Gpr::R11, sym);
    let finished = e.finish().unwrap();
    assert_eq!(
        finished.code,
        [0xE8, 0, 0, 0, 0, 0x49, 0xBB, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        finished.relocs,
        vec![
            Reloc {
                kind: RelocKind::Rel32,
                symbol: sym,
                at: 1
            },
            Reloc {
                kind: RelocKind::Abs64,
                symbol: sym,
                at: 7
            },
        ]
    );
    cases += check(
        Instruction::with_branch(Code::Call_rel32_64, 5).unwrap(),
        &finished.code[..5],
    );
    cases += check(
        Instruction::with2(Code::Mov_r64_imm64, Register::R11, 0u64).unwrap(),
        &finished.code[5..],
    );

    // RIP-relative label with an addend on the displacement: lea is 7 bytes
    // at offset 0 and the label sits at 10, so the disp32 is 10 - 7 + 16.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.lea(
        OpSize::S64,
        Gpr::Rax,
        Mem {
            base: None,
            index: None,
            scale: Scale::X1,
            disp: 16,
            label: Some(label),
        },
    )
    .unwrap();
    e.nop(3).unwrap();
    e.bind_label(label).unwrap();
    let finished = e.finish().unwrap();
    assert_eq!(
        finished.code,
        [0x48, 0x8D, 0x05, 0x13, 0x00, 0x00, 0x00, 0x0F, 0x1F, 0x00]
    );
    cases += check(
        Instruction::with2(
            Code::Lea_r64_m,
            Register::RAX,
            MemoryOperand::new(
                Register::RIP,
                Register::None,
                1,
                26,
                8,
                false,
                Register::None,
            ),
        )
        .unwrap(),
        &finished.code[..7],
    );

    // movsd with a RIP-relative label operand: the label sits 8 bytes past
    // the end of the 8-byte movsd.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.movsd_rm(Xmm::Xmm2, Mem::rip_label(label)).unwrap();
    e.nop(8).unwrap();
    e.bind_label(label).unwrap();
    let finished = e.finish().unwrap();
    assert_eq!(
        finished.code,
        [
            0xF2, 0x0F, 0x10, 0x15, 0x08, 0x00, 0x00, 0x00, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ]
    );
    cases += check(
        Instruction::with2(
            Code::Movsd_xmm_xmmm64,
            Register::XMM2,
            MemoryOperand::new(
                Register::RIP,
                Register::None,
                1,
                16,
                8,
                false,
                Register::None,
            ),
        )
        .unwrap(),
        &finished.code[..8],
    );
    println!("labels/fixups/relocs: {cases} differential cases");
}

#[test]
fn error_cases() {
    // Immediates out of range for the operand size.
    let mut e = Encoder::new();
    assert!(matches!(
        e.mov_reg_imm(OpSize::S8, Gpr::Rax, 0x80),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.mov_reg_imm(OpSize::S16, Gpr::Rax, 0x8000),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.mov_reg_imm(OpSize::S32, Gpr::Rax, 0x1_0000_0000),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.alu_reg_imm(AluOp::Add, OpSize::S64, Gpr::Rax, 0x1_0000_0000),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.alu_reg_imm(AluOp::Add, OpSize::S8, Gpr::Rax, 0x80),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.test_reg_imm(OpSize::S16, Gpr::Rax, 0x8000),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    assert!(matches!(
        e.imul_rr_imm(OpSize::S64, Gpr::Rax, Gpr::Rcx, 0x1_0000_0000),
        Err(EncodeError::ImmOutOfRange { .. })
    ));
    // rsp as index.
    let mut e = Encoder::new();
    assert!(matches!(
        e.mov_rm(
            OpSize::S64,
            Gpr::Rax,
            Mem::indexed(Gpr::Rbp, Gpr::Rsp, Scale::X1, 0)
        ),
        Err(EncodeError::InvalidIndex)
    ));
    // A label can only be the RIP-relative displacement, never combined with
    // a base or index register.
    let mut e = Encoder::new();
    let label = e.create_label();
    assert!(matches!(
        e.mov_rm(
            OpSize::S64,
            Gpr::Rax,
            Mem {
                base: Some(Gpr::Rax),
                index: None,
                scale: Scale::X1,
                disp: 0,
                label: Some(label)
            }
        ),
        Err(EncodeError::LabelWithRegisters)
    ));
    let mut e = Encoder::new();
    let label = e.create_label();
    assert!(matches!(
        e.mov_rm(
            OpSize::S64,
            Gpr::Rax,
            Mem {
                base: None,
                index: Some(Gpr::Rcx),
                scale: Scale::X8,
                disp: 8,
                label: Some(label)
            }
        ),
        Err(EncodeError::LabelWithRegisters)
    ));
    // Sizes a form does not have.
    let mut e = Encoder::new();
    assert!(matches!(
        e.lea(OpSize::S8, Gpr::Rax, Mem::base(Gpr::Rcx)),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.imul_rr(OpSize::S8, Gpr::Rax, Gpr::Rcx),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.cmovcc(Cond::E, OpSize::S8, Gpr::Rax, Loc::Reg(Gpr::Rcx)),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.cvtsi2sd(Xmm::Xmm0, OpSize::S16, Loc::Reg(Gpr::Rcx)),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.cvttsd2si(Gpr::Rax, OpSize::S8, XmmLoc::Xmm(Xmm::Xmm0)),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.movzx(OpSize::S32, Gpr::Rax, OpSize::S32, Loc::Reg(Gpr::Rcx)),
        Err(EncodeError::InvalidSize { .. })
    ));
    assert!(matches!(
        e.bt_rr(OpSize::S8, Gpr::Rax, Gpr::Rcx),
        Err(EncodeError::InvalidSize { .. })
    ));
    // Padding lengths.
    let mut e = Encoder::new();
    assert!(matches!(e.nop(0), Err(EncodeError::InvalidNopLength(0))));
    assert!(matches!(e.nop(10), Err(EncodeError::InvalidNopLength(10))));
    // Labels: double bind and never bound.
    let mut e = Encoder::new();
    let label = e.create_label();
    e.bind_label(label).unwrap();
    assert!(matches!(
        e.bind_label(label),
        Err(EncodeError::LabelAlreadyBound(_))
    ));
    let mut e = Encoder::new();
    let label = e.create_label();
    e.jmp(label).unwrap();
    assert!(matches!(e.finish(), Err(EncodeError::LabelNotBound(_))));
}

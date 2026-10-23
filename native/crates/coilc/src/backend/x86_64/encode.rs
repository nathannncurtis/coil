//! The table-driven x86-64 encoder (REX/VEX, ModRM/SIB) over the instruction
//! forms the rules emit (spec section 12.2).
//!
//! Every form the backend emits has a legacy encoding (the SSE4.1 `roundsd`
//! included), so there is no VEX path. Labels carry branch and RIP-relative
//! fixups; [`Encoder::finish`] resolves intra-function ones and returns the
//! external relocations for the linker. Immediates and branch ranges are
//! validated, never truncated.

use super::regs::{Gpr, Xmm};
use std::fmt;

/// Operand size of an instruction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OpSize {
    S8,
    S16,
    S32,
    S64,
}

impl OpSize {
    pub const fn bytes(self) -> u32 {
        1 << (self as u32)
    }
}

/// Index scale of a memory operand; the discriminant is the SIB scale field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Scale {
    X1 = 0,
    X2 = 1,
    X4 = 2,
    X8 = 3,
}

/// Memory operand: `base + index*scale + disp`, or RIP-relative.
///
/// With no base and no index the operand is RIP-relative (`disp` as the
/// disp32, or `label` against a fixup); 64-bit mode has no plain absolute
/// addressing through this operand, since absolute addresses belong in data
/// tables with base relocations (spec section 12.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Mem {
    pub base: Option<Gpr>,
    pub index: Option<Gpr>,
    pub scale: Scale,
    pub disp: i32,
    pub label: Option<Label>,
}

impl Mem {
    pub const fn base(base: Gpr) -> Mem {
        Mem {
            base: Some(base),
            index: None,
            scale: Scale::X1,
            disp: 0,
            label: None,
        }
    }

    pub const fn disp(base: Gpr, disp: i32) -> Mem {
        Mem {
            base: Some(base),
            index: None,
            scale: Scale::X1,
            disp,
            label: None,
        }
    }

    pub const fn indexed(base: Gpr, index: Gpr, scale: Scale, disp: i32) -> Mem {
        Mem {
            base: Some(base),
            index: Some(index),
            scale,
            disp,
            label: None,
        }
    }

    pub const fn rip(disp: i32) -> Mem {
        Mem {
            base: None,
            index: None,
            scale: Scale::X1,
            disp,
            label: None,
        }
    }

    pub const fn rip_label(label: Label) -> Mem {
        Mem {
            base: None,
            index: None,
            scale: Scale::X1,
            disp: 0,
            label: Some(label),
        }
    }
}

/// A ModRM r/m location: a general purpose register or a memory operand.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Loc {
    Reg(Gpr),
    Mem(Mem),
}

/// An XMM r/m location for the SSE forms.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum XmmLoc {
    Xmm(Xmm),
    Mem(Mem),
}

/// Condition code; the discriminant is the low opcode nibble.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cond {
    O = 0,
    NO = 1,
    B = 2,
    AE = 3,
    E = 4,
    NE = 5,
    BE = 6,
    A = 7,
    S = 8,
    NS = 9,
    P = 10,
    NP = 11,
    L = 12,
    GE = 13,
    LE = 14,
    G = 15,
}

/// Group-1 arithmetic/logic operation; the discriminant is the ModRM reg
/// field shared by the r/m,r, r,r/m and imm forms.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AluOp {
    Add = 0,
    Or = 1,
    Adc = 2,
    Sbb = 3,
    And = 4,
    Sub = 5,
    Xor = 6,
    Cmp = 7,
}

/// Group-2 shift; the discriminant is the ModRM reg field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ShiftOp {
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

/// Group-3 (F6/F7) unary operation; the discriminant is the ModRM reg field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Group3Op {
    Not = 2,
    Neg = 3,
    Div = 6,
    Idiv = 7,
}

/// Scalar-double binary operation: F2-prefixed 0F opcode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SdOp {
    Adds = 0x58,
    Mul = 0x59,
    Subs = 0x5C,
    Div = 0x5E,
    Sqrt = 0x51,
}

/// Scalar-double compare: 66-prefixed 0F opcode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SdCmpOp {
    Ucomis = 0x2E,
    Comis = 0x2F,
}

/// Packed-double logic operation: 66-prefixed 0F opcode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PdOp {
    And = 0x54,
    Xor = 0x57,
}

/// A branch target or RIP-relative reference in the function being encoded.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Label(u32);

/// Identity of an external symbol; the linker maps ids to names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SymbolId(pub u32);

/// Kind of an external relocation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RelocKind {
    /// 32-bit displacement against a symbol: call/jump targets.
    Rel32,
    /// 64-bit absolute address of a symbol: data references.
    Abs64,
}

/// An external relocation `finish` hands to the linker, patching the 4 or 8
/// bytes at `at` once the symbol's address is known.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Reloc {
    pub kind: RelocKind,
    pub symbol: SymbolId,
    pub at: u32,
}

/// The encoder's output: code with all intra-function fixups resolved, plus
/// the external relocations.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    pub code: Vec<u8>,
    pub relocs: Vec<Reloc>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EncodeError {
    /// Immediate does not fit the operand size.
    ImmOutOfRange { size: OpSize, imm: i64 },
    /// Operand size is not valid for the form.
    InvalidSize { what: &'static str, size: OpSize },
    /// `rsp` cannot be an index register (SIB index 100 means "no index").
    InvalidIndex,
    /// The label was bound a second time.
    LabelAlreadyBound(Label),
    /// The label was referenced but never bound.
    LabelNotBound(Label),
    /// A branch displacement exceeds 32 bits: the function is too large.
    BranchOutOfRange(Label),
    /// Padding length outside 1..=9.
    InvalidNopLength(usize),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::ImmOutOfRange { size, imm } => {
                write!(f, "immediate {imm} does not fit a {size:?} operand")
            }
            EncodeError::InvalidSize { what, size } => {
                write!(f, "{what} does not support {size:?} operands")
            }
            EncodeError::InvalidIndex => write!(f, "rsp cannot be an index register"),
            EncodeError::LabelAlreadyBound(l) => write!(f, "label {:?} bound twice", l.0),
            EncodeError::LabelNotBound(l) => write!(f, "label {:?} never bound", l.0),
            EncodeError::BranchOutOfRange(l) => {
                write!(f, "branch to label {:?} exceeds 32-bit displacement", l.0)
            }
            EncodeError::InvalidNopLength(n) => write!(f, "nop padding length {n} outside 1..=9"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Displacement of a branch or RIP-relative reference, patched by `finish`.
struct Fixup {
    /// Offset of the 4-byte displacement field in the code.
    at: u32,
    /// Offset of the end of the instruction: the rel32/RIP base.
    next_ip: u32,
    label: Label,
}

enum LabelState {
    Unbound,
    Bound(u32),
}

/// The x86-64 byte emitter.
#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
    labels: Vec<LabelState>,
    fixups: Vec<Fixup>,
    relocs: Vec<Reloc>,
}

/// Register in the ModRM rm field of a register-only form.
enum RmReg {
    Gpr(Gpr),
    Xmm(Xmm),
}

/// ModRM/SIB/displacement layout of a memory operand.
struct MemEnc {
    mod_bits: u8,
    rm: u8,
    sib: Option<u8>,
    disp: i32,
    disp_len: u8,
    rex_x: bool,
    rex_b: bool,
    label: Option<Label>,
}

const ALU_MR: u8 = 0x00;
const ALU_RM: u8 = 0x02;

impl Encoder {
    pub fn new() -> Encoder {
        Encoder::default()
    }

    /// Offset of the next byte to be emitted, for line tables and unwind info.
    pub fn offset(&self) -> u32 {
        self.buf.len() as u32
    }

    /// Creates an unbound label.
    pub fn create_label(&mut self) -> Label {
        let id = self.labels.len() as u32;
        self.labels.push(LabelState::Unbound);
        Label(id)
    }

    /// Binds the label to the current offset.
    pub fn bind_label(&mut self, label: Label) -> Result<(), EncodeError> {
        let state = &mut self.labels[label.0 as usize];
        if matches!(state, LabelState::Unbound) {
            *state = LabelState::Bound(self.buf.len() as u32);
            Ok(())
        } else {
            Err(EncodeError::LabelAlreadyBound(label))
        }
    }

    /// Resolves all intra-function fixups and returns the code with the
    /// external relocations for the linker.
    pub fn finish(mut self) -> Result<Finished, EncodeError> {
        for (i, state) in self.labels.iter().enumerate() {
            if matches!(state, LabelState::Unbound) {
                return Err(EncodeError::LabelNotBound(Label(i as u32)));
            }
        }
        for fix in &self.fixups {
            let target = match self.labels[fix.label.0 as usize] {
                LabelState::Bound(at) => i64::from(at),
                LabelState::Unbound => unreachable!("checked above"),
            };
            let disp = i32::try_from(target - i64::from(fix.next_ip))
                .map_err(|_| EncodeError::BranchOutOfRange(fix.label))?;
            let at = fix.at as usize;
            self.buf[at..at + 4].copy_from_slice(&disp.to_le_bytes());
        }
        Ok(Finished {
            code: self.buf,
            relocs: self.relocs,
        })
    }

    // -- mov --------------------------------------------------------------

    pub fn mov_rr(&mut self, size: OpSize, dst: Gpr, src: Gpr) {
        self.emit_reg_form(
            size,
            legacy16(size),
            &[if size == OpSize::S8 { 0x88 } else { 0x89 }],
            src.code(),
            byte_rex(size, src),
            RmReg::Gpr(dst),
        );
    }

    /// `mov dst, [src]`.
    pub fn mov_rm(&mut self, size: OpSize, dst: Gpr, src: Mem) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0x8A } else { 0x8B };
        let fix = self.emit_mem_form(
            size,
            legacy16(size),
            &[opcode],
            dst.code(),
            byte_rex(size, dst),
            src,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    /// `mov [dst], src`.
    pub fn mov_mr(&mut self, size: OpSize, dst: Mem, src: Gpr) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0x88 } else { 0x89 };
        let fix = self.emit_mem_form(
            size,
            legacy16(size),
            &[opcode],
            src.code(),
            byte_rex(size, src),
            dst,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    /// Immediate move: short forms at 8/16/32 bits, `C7 /0` sign-extended or
    /// the 64-bit `B8+r` form at 64 bits.
    pub fn mov_reg_imm(&mut self, size: OpSize, dst: Gpr, imm: i64) -> Result<(), EncodeError> {
        match size {
            OpSize::S8 => {
                let imm = imm8(imm, size)?;
                let rex =
                    Self::rex_for(size, false, false, dst.extended(), dst.needs_rex_as_byte());
                self.emit_head(&[], &[0xB0 | dst.number()], rex);
                self.buf.push(imm as u8);
            }
            OpSize::S16 => {
                let imm = imm16(imm, size)?;
                let rex = Self::rex_for(size, false, false, dst.extended(), false);
                self.emit_head(legacy16(size), &[0xB8 | dst.number()], rex);
                self.push_imm(i64::from(imm), 2);
            }
            OpSize::S32 => {
                let imm = imm32(imm, size)?;
                let rex = Self::rex_for(size, false, false, dst.extended(), false);
                self.emit_head(&[], &[0xB8 | dst.number()], rex);
                self.push_imm(i64::from(imm), 4);
            }
            OpSize::S64 => {
                if let Ok(imm) = imm32(imm, size) {
                    self.emit_reg_form(size, &[], &[0xC7], 0, false, RmReg::Gpr(dst));
                    self.push_imm(i64::from(imm), 4);
                } else {
                    let rex = Self::rex_for(size, false, false, dst.extended(), false);
                    self.emit_head(&[], &[0xB8 | dst.number()], rex);
                    self.buf.extend_from_slice(&(imm as u64).to_le_bytes());
                }
            }
        }
        Ok(())
    }

    /// `movabs`: the full 64-bit immediate form.
    pub fn mov_reg_imm64(&mut self, dst: Gpr, imm: u64) {
        let rex = Self::rex_for(OpSize::S64, false, false, dst.extended(), false);
        self.emit_head(&[], &[0xB8 | dst.number()], rex);
        self.buf.extend_from_slice(&imm.to_le_bytes());
    }

    /// Loads a symbol's address with `movabs`, recording an `Abs64`
    /// relocation over the immediate for the linker.
    pub fn mov_reg_symbol_addr(&mut self, dst: Gpr, symbol: SymbolId) {
        self.mov_reg_imm64(dst, 0);
        let at = self.buf.len() as u32 - 8;
        self.relocs.push(Reloc {
            kind: RelocKind::Abs64,
            symbol,
            at,
        });
    }

    pub fn movzx(
        &mut self,
        dst_size: OpSize,
        dst: Gpr,
        src_size: OpSize,
        src: Loc,
    ) -> Result<(), EncodeError> {
        self.extended_move("movzx", dst_size, dst, src_size, src, 0xB6)
    }

    pub fn movsx(
        &mut self,
        dst_size: OpSize,
        dst: Gpr,
        src_size: OpSize,
        src: Loc,
    ) -> Result<(), EncodeError> {
        self.extended_move("movsx", dst_size, dst, src_size, src, 0xBE)
    }

    /// `movsxd`: 32-bit source, 64-bit destination.
    pub fn movsxd(&mut self, dst: Gpr, src: Loc) -> Result<(), EncodeError> {
        self.move_ext(OpSize::S64, dst, &[0x63], src, false)
    }

    pub fn lea(&mut self, size: OpSize, dst: Gpr, src: Mem) -> Result<(), EncodeError> {
        if size == OpSize::S8 {
            return Err(EncodeError::InvalidSize {
                what: "lea destination",
                size,
            });
        }
        let fix = self.emit_mem_form(size, legacy16(size), &[0x8D], dst.code(), false, src)?;
        self.close_rip(fix);
        Ok(())
    }

    // -- group 1: alu and test ---------------------------------------------

    pub fn alu_rr(&mut self, op: AluOp, size: OpSize, dst: Gpr, src: Gpr) {
        self.emit_reg_form(
            size,
            legacy16(size),
            &[ALU_MR + (op as u8) * 8 + u8::from(size != OpSize::S8)],
            src.code(),
            byte_rex(size, src),
            RmReg::Gpr(dst),
        );
    }

    /// `op [dst], src`.
    pub fn alu_rm(
        &mut self,
        op: AluOp,
        size: OpSize,
        dst: Mem,
        src: Gpr,
    ) -> Result<(), EncodeError> {
        let fix = self.emit_mem_form(
            size,
            legacy16(size),
            &[ALU_MR + (op as u8) * 8 + u8::from(size != OpSize::S8)],
            src.code(),
            byte_rex(size, src),
            dst,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    /// `op dst, [src]`.
    pub fn alu_mr(
        &mut self,
        op: AluOp,
        size: OpSize,
        dst: Gpr,
        src: Mem,
    ) -> Result<(), EncodeError> {
        let fix = self.emit_mem_form(
            size,
            legacy16(size),
            &[ALU_RM + (op as u8) * 8 + u8::from(size != OpSize::S8)],
            dst.code(),
            byte_rex(size, dst),
            src,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn alu_reg_imm(
        &mut self,
        op: AluOp,
        size: OpSize,
        dst: Gpr,
        imm: i64,
    ) -> Result<(), EncodeError> {
        let (opcode, imm_len) = alu_imm_opcode(size, imm)?;
        self.emit_reg_form(
            size,
            legacy16(size),
            &[opcode],
            op as u8,
            false,
            RmReg::Gpr(dst),
        );
        self.push_imm(imm, imm_len);
        Ok(())
    }

    pub fn alu_mem_imm(
        &mut self,
        op: AluOp,
        size: OpSize,
        dst: Mem,
        imm: i64,
    ) -> Result<(), EncodeError> {
        let (opcode, imm_len) = alu_imm_opcode(size, imm)?;
        let fix = self.emit_mem_form(size, legacy16(size), &[opcode], op as u8, false, dst)?;
        self.push_imm(imm, imm_len);
        self.close_rip(fix);
        Ok(())
    }

    pub fn test_rr(&mut self, size: OpSize, a: Gpr, b: Gpr) {
        let opcode = if size == OpSize::S8 { 0x84 } else { 0x85 };
        self.emit_reg_form(
            size,
            legacy16(size),
            &[opcode],
            b.code(),
            byte_rex(size, b),
            RmReg::Gpr(a),
        );
    }

    /// `test reg, [mem]` (commutative: one memory direction exists).
    pub fn test_rm(&mut self, size: OpSize, reg: Gpr, mem: Mem) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0x84 } else { 0x85 };
        let fix = self.emit_mem_form(
            size,
            legacy16(size),
            &[opcode],
            reg.code(),
            byte_rex(size, reg),
            mem,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn test_reg_imm(&mut self, size: OpSize, dst: Gpr, imm: i64) -> Result<(), EncodeError> {
        let (opcode, imm_len) = test_imm_opcode(size, imm)?;
        self.emit_reg_form(size, legacy16(size), &[opcode], 0, false, RmReg::Gpr(dst));
        self.push_imm(imm, imm_len);
        Ok(())
    }

    pub fn test_mem_imm(&mut self, size: OpSize, dst: Mem, imm: i64) -> Result<(), EncodeError> {
        let (opcode, imm_len) = test_imm_opcode(size, imm)?;
        let fix = self.emit_mem_form(size, legacy16(size), &[opcode], 0, false, dst)?;
        self.push_imm(imm, imm_len);
        self.close_rip(fix);
        Ok(())
    }

    // -- imul, group 3, shifts, bt ------------------------------------------

    pub fn imul_rr(&mut self, size: OpSize, dst: Gpr, src: Gpr) -> Result<(), EncodeError> {
        self.check_word_size("imul", size)?;
        self.emit_reg_form(
            size,
            legacy16(size),
            &[0x0F, 0xAF],
            dst.code(),
            false,
            RmReg::Gpr(src),
        );
        Ok(())
    }

    pub fn imul_rm(&mut self, size: OpSize, dst: Gpr, src: Mem) -> Result<(), EncodeError> {
        self.check_word_size("imul", size)?;
        let fix =
            self.emit_mem_form(size, legacy16(size), &[0x0F, 0xAF], dst.code(), false, src)?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn imul_rr_imm(
        &mut self,
        size: OpSize,
        dst: Gpr,
        src: Gpr,
        imm: i64,
    ) -> Result<(), EncodeError> {
        self.check_word_size("imul", size)?;
        let (opcode, imm_len) = imul_imm_opcode(size, imm)?;
        self.emit_reg_form(
            size,
            legacy16(size),
            &[opcode],
            dst.code(),
            false,
            RmReg::Gpr(src),
        );
        self.push_imm(imm, imm_len);
        Ok(())
    }

    pub fn not(&mut self, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        self.group3(Group3Op::Not, size, dst)
    }

    pub fn neg(&mut self, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        self.group3(Group3Op::Neg, size, dst)
    }

    pub fn div(&mut self, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        self.group3(Group3Op::Div, size, dst)
    }

    pub fn idiv(&mut self, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        self.group3(Group3Op::Idiv, size, dst)
    }

    /// Sign-extends `rax` into `rdx`.
    pub fn cqo(&mut self) {
        self.buf.extend_from_slice(&[0x48, 0x99]);
    }

    pub fn shift_cl(&mut self, op: ShiftOp, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0xD2 } else { 0xD3 };
        self.shift(op, size, opcode, dst, None)
    }

    pub fn shift_imm(
        &mut self,
        op: ShiftOp,
        size: OpSize,
        dst: Loc,
        imm: u8,
    ) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0xC0 } else { 0xC1 };
        self.shift(op, size, opcode, dst, Some(imm))
    }

    /// `bt r/m, reg`.
    pub fn bt_rr(&mut self, size: OpSize, dst: Gpr, src: Gpr) -> Result<(), EncodeError> {
        self.check_word_size("bt", size)?;
        self.emit_reg_form(
            size,
            legacy16(size),
            &[0x0F, 0xA3],
            src.code(),
            false,
            RmReg::Gpr(dst),
        );
        Ok(())
    }

    /// `bt [mem], reg`.
    pub fn bt_rm(&mut self, size: OpSize, dst: Mem, src: Gpr) -> Result<(), EncodeError> {
        self.check_word_size("bt", size)?;
        let fix =
            self.emit_mem_form(size, legacy16(size), &[0x0F, 0xA3], src.code(), false, dst)?;
        self.close_rip(fix);
        Ok(())
    }

    /// `bt r/m, imm8`.
    pub fn bt_imm(&mut self, size: OpSize, dst: Loc, imm: u8) -> Result<(), EncodeError> {
        self.check_word_size("bt", size)?;
        match dst {
            Loc::Reg(g) => {
                self.emit_reg_form(size, legacy16(size), &[0x0F, 0xBA], 4, false, RmReg::Gpr(g));
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(size, legacy16(size), &[0x0F, 0xBA], 4, false, m)?;
                self.close_rip(fix);
            }
        }
        self.buf.push(imm);
        Ok(())
    }

    // -- flags, branches, calls ---------------------------------------------

    /// `setcc`: the destination is a byte register or memory operand.
    pub fn setcc(&mut self, cc: Cond, dst: Loc) -> Result<(), EncodeError> {
        let opcodes = [0x0F, 0x90 + cc as u8];
        match dst {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    OpSize::S8,
                    &[],
                    &opcodes,
                    0,
                    g.needs_rex_as_byte(),
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(OpSize::S8, &[], &opcodes, 0, false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    pub fn cmovcc(
        &mut self,
        cc: Cond,
        size: OpSize,
        dst: Gpr,
        src: Loc,
    ) -> Result<(), EncodeError> {
        self.check_word_size("cmovcc", size)?;
        let opcodes = [0x0F, 0x40 + cc as u8];
        match src {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    size,
                    legacy16(size),
                    &opcodes,
                    dst.code(),
                    false,
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix =
                    self.emit_mem_form(size, legacy16(size), &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    /// Branches to an already bound nearby target with rel8, otherwise rel32;
    /// forward targets record a rel32 fixup for `finish`.
    pub fn jmp(&mut self, label: Label) -> Result<(), EncodeError> {
        self.rel_branch(label, Some(0xEB), &[0xE9])
    }

    pub fn jcc(&mut self, cc: Cond, label: Label) -> Result<(), EncodeError> {
        self.rel_branch(label, Some(0x70 + cc as u8), &[0x0F, 0x80 + cc as u8])
    }

    /// Near call to a label in the same function.
    pub fn call(&mut self, label: Label) -> Result<(), EncodeError> {
        self.rel_branch(label, None, &[0xE8])
    }

    /// Near call to an external symbol: the zero displacement is patched by
    /// the linker through the recorded `Rel32` relocation.
    pub fn call_symbol(&mut self, symbol: SymbolId) {
        self.buf.push(0xE8);
        let at = self.buf.len() as u32;
        self.buf.extend_from_slice(&[0; 4]);
        self.relocs.push(Reloc {
            kind: RelocKind::Rel32,
            symbol,
            at,
        });
    }

    pub fn call_rm(&mut self, target: Loc) -> Result<(), EncodeError> {
        // FF /2 defaults to a 64-bit operand in long mode; only the B bit of
        // a REX prefix can matter, so encode without W.
        match target {
            Loc::Reg(g) => {
                self.emit_reg_form(OpSize::S32, &[], &[0xFF], 2, false, RmReg::Gpr(g));
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(OpSize::S32, &[], &[0xFF], 2, false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    pub fn ret(&mut self) {
        self.buf.push(0xC3);
    }

    pub fn push(&mut self, r: Gpr) {
        let rex = Self::rex_for(OpSize::S32, false, false, r.extended(), false);
        self.emit_head(&[], &[0x50 | r.number()], rex);
    }

    pub fn pop(&mut self, r: Gpr) {
        let rex = Self::rex_for(OpSize::S32, false, false, r.extended(), false);
        self.emit_head(&[], &[0x58 | r.number()], rex);
    }

    pub fn xchg(&mut self, size: OpSize, a: Loc, b: Gpr) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0x86 } else { 0x87 };
        match a {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    size,
                    legacy16(size),
                    &[opcode],
                    b.code(),
                    byte_rex(size, b) || byte_rex(size, g),
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(
                    size,
                    legacy16(size),
                    &[opcode],
                    b.code(),
                    byte_rex(size, b),
                    m,
                )?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    /// `lock cmpxchg [dst], src`; the locked form only exists with a memory
    /// destination.
    pub fn lock_cmpxchg(&mut self, size: OpSize, dst: Mem, src: Gpr) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0xB0 } else { 0xB1 };
        let fix = self.emit_mem_form(
            size,
            lock16(size),
            &[0x0F, opcode],
            src.code(),
            byte_rex(size, src),
            dst,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    /// `lock xadd [dst], src`.
    pub fn lock_xadd(&mut self, size: OpSize, dst: Mem, src: Gpr) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0xC0 } else { 0xC1 };
        let fix = self.emit_mem_form(
            size,
            lock16(size),
            &[0x0F, opcode],
            src.code(),
            byte_rex(size, src),
            dst,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn mfence(&mut self) {
        self.buf.extend_from_slice(&[0x0F, 0xAE, 0xF0]);
    }

    pub fn int3(&mut self) {
        self.buf.push(0xCC);
    }

    pub fn ud2(&mut self) {
        self.buf.extend_from_slice(&[0x0F, 0x0B]);
    }

    /// Multi-byte nop padding, the canonical forms of length 1..=9.
    pub fn nop(&mut self, len: usize) -> Result<(), EncodeError> {
        match len {
            1 => self.buf.extend_from_slice(&[0x90]),
            2 => self.buf.extend_from_slice(&[0x66, 0x90]),
            3 => self.buf.extend_from_slice(&[0x0F, 0x1F, 0x00]),
            4 => self.buf.extend_from_slice(&[0x0F, 0x1F, 0x40, 0x00]),
            5 => self.buf.extend_from_slice(&[0x0F, 0x1F, 0x44, 0x00, 0x00]),
            6 => self
                .buf
                .extend_from_slice(&[0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00]),
            7 => self
                .buf
                .extend_from_slice(&[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00]),
            8 => self
                .buf
                .extend_from_slice(&[0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00]),
            9 => self
                .buf
                .extend_from_slice(&[0x66, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00]),
            _ => return Err(EncodeError::InvalidNopLength(len)),
        }
        Ok(())
    }

    // -- SSE2 scalar double and packed logic ---------------------------------

    pub fn movsd_rr(&mut self, dst: Xmm, src: Xmm) {
        self.emit_reg_form(
            OpSize::S32,
            &[0xF2],
            &[0x0F, 0x10],
            dst.code(),
            false,
            RmReg::Xmm(src),
        );
    }

    pub fn movsd_rm(&mut self, dst: Xmm, src: Mem) -> Result<(), EncodeError> {
        let fix =
            self.emit_mem_form(OpSize::S32, &[0xF2], &[0x0F, 0x10], dst.code(), false, src)?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn movsd_mr(&mut self, dst: Mem, src: Xmm) -> Result<(), EncodeError> {
        let fix =
            self.emit_mem_form(OpSize::S32, &[0xF2], &[0x0F, 0x11], src.code(), false, dst)?;
        self.close_rip(fix);
        Ok(())
    }

    /// `movq xmm, r64`.
    pub fn movq_xmm_gpr(&mut self, dst: Xmm, src: Gpr) {
        self.emit_reg_form(
            OpSize::S64,
            &[0x66],
            &[0x0F, 0x6E],
            dst.code(),
            false,
            RmReg::Gpr(src),
        );
    }

    /// `movq r64, xmm`.
    pub fn movq_gpr_xmm(&mut self, dst: Gpr, src: Xmm) {
        self.emit_reg_form(
            OpSize::S64,
            &[0x66],
            &[0x0F, 0x7E],
            src.code(),
            false,
            RmReg::Gpr(dst),
        );
    }

    pub fn sd_rr(&mut self, op: SdOp, dst: Xmm, src: Xmm) {
        self.emit_reg_form(
            OpSize::S32,
            &[0xF2],
            &[0x0F, op as u8],
            dst.code(),
            false,
            RmReg::Xmm(src),
        );
    }

    pub fn sd_rm(&mut self, op: SdOp, dst: Xmm, src: Mem) -> Result<(), EncodeError> {
        let fix = self.emit_mem_form(
            OpSize::S32,
            &[0xF2],
            &[0x0F, op as u8],
            dst.code(),
            false,
            src,
        )?;
        self.close_rip(fix);
        Ok(())
    }

    pub fn sd_cmp(&mut self, op: SdCmpOp, dst: Xmm, src: XmmLoc) -> Result<(), EncodeError> {
        let opcodes = [0x0F, op as u8];
        match src {
            XmmLoc::Xmm(x) => {
                self.emit_reg_form(
                    OpSize::S32,
                    &[0x66],
                    &opcodes,
                    dst.code(),
                    false,
                    RmReg::Xmm(x),
                );
            }
            XmmLoc::Mem(m) => {
                let fix =
                    self.emit_mem_form(OpSize::S32, &[0x66], &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    /// `cvtsi2sd`: 32- or 64-bit integer source.
    pub fn cvtsi2sd(&mut self, dst: Xmm, size: OpSize, src: Loc) -> Result<(), EncodeError> {
        if !matches!(size, OpSize::S32 | OpSize::S64) {
            return Err(EncodeError::InvalidSize {
                what: "cvtsi2sd",
                size,
            });
        }
        let opcodes = [0x0F, 0x2A];
        match src {
            Loc::Reg(g) => {
                self.emit_reg_form(size, &[0xF2], &opcodes, dst.code(), false, RmReg::Gpr(g));
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(size, &[0xF2], &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    /// `cvttsd2si`: 32- or 64-bit integer destination, truncating.
    pub fn cvttsd2si(&mut self, dst: Gpr, size: OpSize, src: XmmLoc) -> Result<(), EncodeError> {
        if !matches!(size, OpSize::S32 | OpSize::S64) {
            return Err(EncodeError::InvalidSize {
                what: "cvttsd2si",
                size,
            });
        }
        let opcodes = [0x0F, 0x2C];
        match src {
            XmmLoc::Xmm(x) => {
                self.emit_reg_form(size, &[0xF2], &opcodes, dst.code(), false, RmReg::Xmm(x));
            }
            XmmLoc::Mem(m) => {
                let fix = self.emit_mem_form(size, &[0xF2], &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    pub fn pd_rr(&mut self, op: PdOp, dst: Xmm, src: XmmLoc) -> Result<(), EncodeError> {
        let opcodes = [0x0F, op as u8];
        match src {
            XmmLoc::Xmm(x) => {
                self.emit_reg_form(
                    OpSize::S32,
                    &[0x66],
                    &opcodes,
                    dst.code(),
                    false,
                    RmReg::Xmm(x),
                );
            }
            XmmLoc::Mem(m) => {
                let fix =
                    self.emit_mem_form(OpSize::S32, &[0x66], &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    /// `roundsd`, SSE4.1: `mode` is the rounding/precision immediate.
    pub fn roundsd(&mut self, dst: Xmm, src: XmmLoc, mode: u8) -> Result<(), EncodeError> {
        let opcodes = [0x0F, 0x3A, 0x0B];
        match src {
            XmmLoc::Xmm(x) => {
                self.emit_reg_form(
                    OpSize::S32,
                    &[0x66],
                    &opcodes,
                    dst.code(),
                    false,
                    RmReg::Xmm(x),
                );
            }
            XmmLoc::Mem(m) => {
                let fix =
                    self.emit_mem_form(OpSize::S32, &[0x66], &opcodes, dst.code(), false, m)?;
                self.close_rip(fix);
            }
        }
        self.buf.push(mode);
        Ok(())
    }

    // -- encoding core -------------------------------------------------------

    fn check_word_size(&self, what: &'static str, size: OpSize) -> Result<(), EncodeError> {
        if size == OpSize::S8 {
            Err(EncodeError::InvalidSize { what, size })
        } else {
            Ok(())
        }
    }

    fn rex_for(size: OpSize, r: bool, x: bool, b: bool, byte: bool) -> Option<u8> {
        let w = size == OpSize::S64;
        if w || r || x || b || byte {
            Some(0x40 | u8::from(w) << 3 | u8::from(r) << 2 | u8::from(x) << 1 | u8::from(b))
        } else {
            None
        }
    }

    fn emit_head(&mut self, legacy: &[u8], opcodes: &[u8], rex: Option<u8>) {
        self.buf.extend_from_slice(legacy);
        if let Some(rex) = rex {
            self.buf.push(rex);
        }
        self.buf.extend_from_slice(opcodes);
    }

    fn emit_reg_form(
        &mut self,
        size: OpSize,
        legacy: &[u8],
        opcodes: &[u8],
        reg_field: u8,
        reg_byte_rex: bool,
        rm: RmReg,
    ) {
        let (b, rm_bits, rm_byte_rex) = match rm {
            RmReg::Gpr(g) => (g.extended(), g.number(), byte_rex(size, g)),
            RmReg::Xmm(x) => (x.extended(), x.number(), false),
        };
        let rex = Self::rex_for(size, reg_field >= 8, false, b, reg_byte_rex || rm_byte_rex);
        self.emit_head(legacy, opcodes, rex);
        self.buf.push(0b11 << 6 | (reg_field & 7) << 3 | rm_bits);
    }

    /// Emits an instruction with a memory r/m operand and returns the
    /// RIP-relative fixup to close once the instruction is complete.
    fn emit_mem_form(
        &mut self,
        size: OpSize,
        legacy: &[u8],
        opcodes: &[u8],
        reg_field: u8,
        reg_byte_rex: bool,
        mem: Mem,
    ) -> Result<Option<(Label, usize)>, EncodeError> {
        let enc = mem_encoding(&mem)?;
        let rex = Self::rex_for(size, reg_field >= 8, enc.rex_x, enc.rex_b, reg_byte_rex);
        self.emit_head(legacy, opcodes, rex);
        self.buf
            .push(enc.mod_bits << 6 | (reg_field & 7) << 3 | enc.rm);
        if let Some(sib) = enc.sib {
            self.buf.push(sib);
        }
        Ok(match enc.label {
            Some(label) => {
                let at = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                Some((label, at))
            }
            None => {
                self.push_disp(enc.disp, enc.disp_len);
                None
            }
        })
    }

    fn push_disp(&mut self, disp: i32, disp_len: u8) {
        match disp_len {
            0 => {}
            1 => self.buf.push(disp as u8),
            _ => self.buf.extend_from_slice(&disp.to_le_bytes()),
        }
    }

    fn push_imm(&mut self, imm: i64, imm_len: u8) {
        match imm_len {
            1 => self.buf.push(imm as u8),
            2 => self.buf.extend_from_slice(&(imm as i16).to_le_bytes()),
            _ => self.buf.extend_from_slice(&(imm as i32).to_le_bytes()),
        }
    }

    /// Records a RIP-relative fixup once the instruction's end offset is known.
    fn close_rip(&mut self, fix: Option<(Label, usize)>) {
        if let Some((label, at)) = fix {
            self.fixups.push(Fixup {
                at: at as u32,
                next_ip: self.buf.len() as u32,
                label,
            });
        }
    }

    fn rel_branch(
        &mut self,
        label: Label,
        rel8_opcode: Option<u8>,
        rel32_opcodes: &[u8],
    ) -> Result<(), EncodeError> {
        match self.labels[label.0 as usize] {
            LabelState::Bound(at) => {
                let here = self.buf.len() as i64;
                if let Some(op) = rel8_opcode
                    && let Ok(disp) = i8::try_from(at as i64 - (here + 2))
                {
                    self.buf.push(op);
                    self.buf.push(disp as u8);
                    return Ok(());
                }
                let next_ip = here + rel32_opcodes.len() as i64 + 4;
                let disp = i32::try_from(at as i64 - next_ip)
                    .map_err(|_| EncodeError::BranchOutOfRange(label))?;
                self.emit_head(&[], rel32_opcodes, None);
                self.buf.extend_from_slice(&disp.to_le_bytes());
                Ok(())
            }
            LabelState::Unbound => {
                self.emit_head(&[], rel32_opcodes, None);
                let at = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                self.fixups.push(Fixup {
                    at: at as u32,
                    next_ip: self.buf.len() as u32,
                    label,
                });
                Ok(())
            }
        }
    }

    fn group3(&mut self, op: Group3Op, size: OpSize, dst: Loc) -> Result<(), EncodeError> {
        let opcode = if size == OpSize::S8 { 0xF6 } else { 0xF7 };
        match dst {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    size,
                    legacy16(size),
                    &[opcode],
                    op as u8,
                    byte_rex(size, g),
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix =
                    self.emit_mem_form(size, legacy16(size), &[opcode], op as u8, false, m)?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }

    fn shift(
        &mut self,
        op: ShiftOp,
        size: OpSize,
        opcode: u8,
        dst: Loc,
        imm: Option<u8>,
    ) -> Result<(), EncodeError> {
        match dst {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    size,
                    legacy16(size),
                    &[opcode],
                    op as u8,
                    byte_rex(size, g),
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix =
                    self.emit_mem_form(size, legacy16(size), &[opcode], op as u8, false, m)?;
                self.close_rip(fix);
            }
        }
        if let Some(imm) = imm {
            self.buf.push(imm);
        }
        Ok(())
    }

    fn extended_move(
        &mut self,
        what: &'static str,
        dst_size: OpSize,
        dst: Gpr,
        src_size: OpSize,
        src: Loc,
        opcode: u8,
    ) -> Result<(), EncodeError> {
        let src_opcode = match src_size {
            OpSize::S8 => opcode,
            OpSize::S16 => opcode + 1,
            _ => {
                return Err(EncodeError::InvalidSize {
                    what,
                    size: src_size,
                });
            }
        };
        if dst_size == OpSize::S8 {
            return Err(EncodeError::InvalidSize {
                what,
                size: dst_size,
            });
        }
        let src_byte_rex =
            matches!(src, Loc::Reg(g) if src_size == OpSize::S8 && g.needs_rex_as_byte());
        self.move_ext(dst_size, dst, &[0x0F, src_opcode], src, src_byte_rex)
    }

    fn move_ext(
        &mut self,
        dst_size: OpSize,
        dst: Gpr,
        opcodes: &[u8],
        src: Loc,
        src_byte_rex: bool,
    ) -> Result<(), EncodeError> {
        match src {
            Loc::Reg(g) => {
                self.emit_reg_form(
                    dst_size,
                    legacy16(dst_size),
                    opcodes,
                    dst.code(),
                    src_byte_rex,
                    RmReg::Gpr(g),
                );
            }
            Loc::Mem(m) => {
                let fix = self.emit_mem_form(
                    dst_size,
                    legacy16(dst_size),
                    opcodes,
                    dst.code(),
                    src_byte_rex,
                    m,
                )?;
                self.close_rip(fix);
            }
        }
        Ok(())
    }
}

fn legacy16(size: OpSize) -> &'static [u8] {
    if size == OpSize::S16 { &[0x66] } else { &[] }
}

fn lock16(size: OpSize) -> &'static [u8] {
    if size == OpSize::S16 {
        &[0xF0, 0x66]
    } else {
        &[0xF0]
    }
}

fn byte_rex(size: OpSize, g: Gpr) -> bool {
    size == OpSize::S8 && g.needs_rex_as_byte()
}

fn imm8(imm: i64, size: OpSize) -> Result<i8, EncodeError> {
    i8::try_from(imm).map_err(|_| EncodeError::ImmOutOfRange { size, imm })
}

fn imm16(imm: i64, size: OpSize) -> Result<i16, EncodeError> {
    i16::try_from(imm).map_err(|_| EncodeError::ImmOutOfRange { size, imm })
}

fn imm32(imm: i64, size: OpSize) -> Result<i32, EncodeError> {
    i32::try_from(imm).map_err(|_| EncodeError::ImmOutOfRange { size, imm })
}

/// Group-1 immediate form: `80` at 8 bits, the sign-extended `83` when the
/// immediate fits a byte, otherwise `81` with an operand-sized immediate.
/// The operation's digit goes in the ModRM reg field, not the opcode.
fn alu_imm_opcode(size: OpSize, imm: i64) -> Result<(u8, u8), EncodeError> {
    if size == OpSize::S8 {
        imm8(imm, size)?;
        return Ok((0x80, 1));
    }
    if imm8(imm, size).is_ok() {
        return Ok((0x83, 1));
    }
    let imm_len = match size {
        OpSize::S16 => {
            imm16(imm, size)?;
            2
        }
        _ => {
            imm32(imm, size)?;
            4
        }
    };
    Ok((0x81, imm_len))
}

/// `test` has no short immediate form: `F6 /0` at 8 bits, `F7 /0` with an
/// operand-sized immediate otherwise (sign-extended at 64 bits).
fn test_imm_opcode(size: OpSize, imm: i64) -> Result<(u8, u8), EncodeError> {
    let imm_len = match size {
        OpSize::S8 => {
            imm8(imm, size)?;
            1
        }
        OpSize::S16 => {
            imm16(imm, size)?;
            2
        }
        _ => {
            imm32(imm, size)?;
            4
        }
    };
    Ok((if size == OpSize::S8 { 0xF6 } else { 0xF7 }, imm_len))
}

/// `imul r, r/m, imm`: `6B` when the immediate fits a byte, `69` with an
/// operand-sized (sign-extended at 64 bits) immediate otherwise.
fn imul_imm_opcode(size: OpSize, imm: i64) -> Result<(u8, u8), EncodeError> {
    if imm8(imm, size).is_ok() {
        return Ok((0x6B, 1));
    }
    let imm_len = match size {
        OpSize::S16 => {
            imm16(imm, size)?;
            2
        }
        _ => {
            imm32(imm, size)?;
            4
        }
    };
    Ok((0x69, imm_len))
}

/// ModRM/SIB layout for a memory operand, with the hardware's special cases:
/// `rsp`/`r12` bases need a SIB byte, `rbp`/`r13` bases cannot use `mod=00`,
/// and no base/index is RIP-relative.
fn mem_encoding(mem: &Mem) -> Result<MemEnc, EncodeError> {
    if mem.base.is_none() && mem.index.is_none() {
        return Ok(MemEnc {
            mod_bits: 0b00,
            rm: 0b101,
            sib: None,
            disp: mem.disp,
            disp_len: 4,
            rex_x: false,
            rex_b: false,
            label: mem.label,
        });
    }
    if mem.index == Some(Gpr::Rsp) {
        return Err(EncodeError::InvalidIndex);
    }
    let sib = mem.index.is_some() || matches!(mem.base, Some(b) if b == Gpr::Rsp || b == Gpr::R12);
    if sib {
        let idx_bits = match mem.index {
            Some(i) => i.number(),
            None => 0b100,
        };
        let base_bits = match mem.base {
            Some(b) => b.number(),
            // No base: SIB base 101 with mod=00 is a disp32 absolute.
            None => 0b101,
        };
        let (mod_bits, disp_len) = match mem.base {
            Some(b) => disp_size(b, mem.disp),
            None => (0b00, 4),
        };
        return Ok(MemEnc {
            mod_bits,
            rm: 0b100,
            sib: Some((mem.scale as u8) << 6 | idx_bits << 3 | base_bits),
            disp: mem.disp,
            disp_len,
            rex_x: mem.index.is_some_and(Gpr::extended),
            rex_b: mem.base.is_some_and(Gpr::extended),
            label: mem.label,
        });
    }
    let base = mem.base.expect("some: the no-base cases returned above");
    let (mod_bits, disp_len) = disp_size(base, mem.disp);
    Ok(MemEnc {
        mod_bits,
        rm: base.number(),
        sib: None,
        disp: mem.disp,
        disp_len,
        rex_x: false,
        rex_b: base.extended(),
        label: mem.label,
    })
}

fn disp_size(base: Gpr, disp: i32) -> (u8, u8) {
    // rbp/r13 without SIB: mod=00 means RIP-relative, so a displacement
    // (possibly an explicit 0) is always present.
    let frame = base == Gpr::Rbp || base == Gpr::R13;
    if !frame && disp == 0 {
        (0b00, 0)
    } else if disp >= i32::from(i8::MIN) && disp <= i32::from(i8::MAX) {
        (0b01, 1)
    } else {
        (0b10, 4)
    }
}

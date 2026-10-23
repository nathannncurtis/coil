//! Unwind info: `UNWIND_INFO`/`.xdata` prologue codes derived from the emitted
//! prologue (spec section 12.4).
//!
//! Every compiled function gets the same frame shape (spec section 12.2):
//! `push rbp; mov rbp, rsp`, then `sub rsp, N` (`N > 0x1000` preceded by a
//! probe loop that touches one page at a time, no `__chkstk` import), then the
//! callee-saved pushes. [`prologue_bytes`] emits that shape and
//! [`unwind_info`] the matching `UNWIND_INFO`: version 1, no handler flags,
//! `FrameRegister` = rbp, the `UWOP_PUSH_NONVOL`, `UWOP_SET_FPREG` and
//! `UWOP_ALLOC_SMALL`/`UWOP_ALLOC_LARGE` codes in reverse prologue order with
//! each code's offset pointing past its instruction. One source of truth: the
//! offsets come from the same byte assembly, so the codes always describe the
//! bytes [`prologue_bytes`] produces.

use std::fmt;

use super::regs::Gpr;

/// `UWOP_PUSH_NONVOL`: `OpInfo` is the register number.
const UWOP_PUSH_NONVOL: u8 = 0;
/// `UWOP_ALLOC_LARGE`: one extra slot holds `size / 8`.
const UWOP_ALLOC_LARGE: u8 = 1;
/// `UWOP_ALLOC_SMALL`: `OpInfo` is `size / 8`, so at most 14 slots.
const UWOP_ALLOC_SMALL: u8 = 2;
/// `UWOP_SET_FPREG`.
const UWOP_SET_FPREG: u8 = 3;

/// The largest frame `UWOP_ALLOC_SMALL` describes: `OpInfo` is 4 bits.
const MAX_ALLOC_SMALL: u32 = 14 * 8;
/// The largest frame one `UWOP_ALLOC_LARGE` extra slot describes: `0xFFFF * 8`.
const MAX_ALLOC_LARGE: u32 = 0xFFFF * 8;

/// Frames larger than a page are probed page by page (spec section 12.2).
const PROBE_THRESHOLD: u32 = 0x1000;

/// One unwind code plus the extra slots `UWOP_ALLOC_LARGE` needs.
struct Code {
    offset: u8,
    op: u8,
    opinfo: u8,
    extra: Vec<u16>,
}

impl Code {
    fn slots(&self) -> usize {
        1 + self.extra.len()
    }
}

/// The description of an emitted prologue the backend hands the linker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prologue {
    /// Bytes subtracted from `rsp`; a multiple of 8. `0` means no `sub`.
    pub frame: u32,
    /// Callee-saved registers pushed after the allocation, in push order.
    /// Only nonvolatile registers other than `rbp` (the frame register,
    /// pushed by the frame setup itself) may appear.
    pub saved: Vec<Gpr>,
}

impl Prologue {
    pub fn new(frame: u32, saved: &[Gpr]) -> Prologue {
        Prologue {
            frame,
            saved: saved.to_vec(),
        }
    }
}

/// Everything unwind generation rejects; the message names the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnwindError {
    FrameNotMultipleOf8(u32),
    FrameTooLarge(u32),
    BadSavedRegister(Gpr),
    DuplicateSavedRegister(Gpr),
}

impl fmt::Display for UnwindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnwindError::FrameNotMultipleOf8(frame) => {
                write!(f, "frame size {frame:#x} is not a multiple of 8")
            }
            UnwindError::FrameTooLarge(frame) => write!(
                f,
                "frame size {frame:#x} exceeds the {MAX_ALLOC_LARGE:#x} \
                 UWOP_ALLOC_LARGE describes"
            ),
            UnwindError::BadSavedRegister(reg) => {
                write!(f, "{reg:?} is not a callee-saved pushable register")
            }
            UnwindError::DuplicateSavedRegister(reg) => {
                write!(f, "{reg:?} is saved more than once")
            }
        }
    }
}

impl std::error::Error for UnwindError {}

/// The assembled prologue with its unwind codes, in prologue order.
struct Assembled {
    bytes: Vec<u8>,
    codes: Vec<Code>,
}

fn assemble(prologue: &Prologue) -> Result<Assembled, UnwindError> {
    if !prologue.frame.is_multiple_of(8) {
        return Err(UnwindError::FrameNotMultipleOf8(prologue.frame));
    }
    if prologue.frame > MAX_ALLOC_LARGE {
        return Err(UnwindError::FrameTooLarge(prologue.frame));
    }
    let mut seen = [false; 16];
    for &reg in &prologue.saved {
        let pushable = matches!(
            reg,
            Gpr::Rbx | Gpr::Rsi | Gpr::Rdi | Gpr::R12 | Gpr::R13 | Gpr::R14 | Gpr::R15
        );
        if !pushable {
            return Err(UnwindError::BadSavedRegister(reg));
        }
        if seen[reg.code() as usize] {
            return Err(UnwindError::DuplicateSavedRegister(reg));
        }
        seen[reg.code() as usize] = true;
    }

    let mut bytes = Vec::new();
    let mut codes = Vec::new();

    // push rbp; mov rbp, rsp — the frame pointer is always valid.
    bytes.push(0x55);
    codes.push(Code {
        offset: 1,
        op: UWOP_PUSH_NONVOL,
        opinfo: Gpr::Rbp.code(),
        extra: Vec::new(),
    });
    bytes.extend_from_slice(&[0x48, 0x89, 0xE5]);
    codes.push(Code {
        offset: 4,
        op: UWOP_SET_FPREG,
        opinfo: 0,
        extra: Vec::new(),
    });

    // Page probe before the allocation: walk rax down a page at a time and
    // touch it, so the guard page is hit before rsp moves past it. Only
    // volatile registers change; no unwind code describes it.
    if prologue.frame > PROBE_THRESHOLD {
        bytes.extend_from_slice(&[0x48, 0x8B, 0xC4]); // mov rax, rsp
        bytes.extend_from_slice(&[0x48, 0x8D, 0x8D]); // lea rcx, [rbp - frame]
        let disp = -(prologue.frame as i32);
        bytes.extend_from_slice(&disp.to_le_bytes());
        let loop_start = bytes.len() as u8;
        bytes.extend_from_slice(&[0x48, 0x2D, 0x00, 0x10, 0x00, 0x00]); // sub rax, 0x1000
        bytes.extend_from_slice(&[0x85, 0x00]); // test [rax], eax
        bytes.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
        bytes.push(0x77); // ja loop_start
        bytes.push(loop_start.wrapping_sub(bytes.len() as u8 + 1));
    }

    if prologue.frame > 0 {
        if prologue.frame <= MAX_ALLOC_SMALL {
            bytes.extend_from_slice(&[0x48, 0x83, 0xEC]); // sub rsp, imm8
            bytes.push(prologue.frame as u8);
            codes.push(Code {
                offset: bytes.len() as u8,
                op: UWOP_ALLOC_SMALL,
                opinfo: (prologue.frame / 8) as u8,
                extra: Vec::new(),
            });
        } else {
            bytes.extend_from_slice(&[0x48, 0x81, 0xEC]); // sub rsp, imm32
            bytes.extend_from_slice(&prologue.frame.to_le_bytes());
            codes.push(Code {
                offset: bytes.len() as u8,
                op: UWOP_ALLOC_LARGE,
                opinfo: 0,
                extra: vec![(prologue.frame / 8) as u16],
            });
        }
    }

    for &reg in &prologue.saved {
        if reg.extended() {
            bytes.push(0x41);
            bytes.push(0x50 | reg.number());
        } else {
            bytes.push(0x50 | reg.number());
        }
        codes.push(Code {
            offset: bytes.len() as u8,
            op: UWOP_PUSH_NONVOL,
            opinfo: reg.code(),
            extra: Vec::new(),
        });
    }

    Ok(Assembled { bytes, codes })
}

/// The canonical prologue bytes for `prologue`; the body and epilogue follow.
pub fn prologue_bytes(prologue: &Prologue) -> Result<Vec<u8>, UnwindError> {
    Ok(assemble(prologue)?.bytes)
}

/// The encoded `UNWIND_INFO` for `prologue`, padded to a multiple of four so
/// `.xdata` entries stay aligned.
pub fn unwind_info(prologue: &Prologue) -> Result<Vec<u8>, UnwindError> {
    let assembled = assemble(prologue)?;
    let slots: usize = assembled.codes.iter().map(Code::slots).sum();

    let mut bytes = Vec::with_capacity(4 + slots * 2 + 2);
    bytes.push(0x01); // Version 1, Flags 0: no handler (spec section 12.4)
    bytes.push(assembled.bytes.len() as u8);
    bytes.push(slots as u8);
    bytes.push(Gpr::Rbp.code()); // FrameRegister = rbp, FrameOffset = 0
    // Reverse prologue order: the unwinder replays from the last operation.
    for code in assembled.codes.iter().rev() {
        let word =
            u16::from(code.offset) | (u16::from(code.op) << 8) | (u16::from(code.opinfo) << 12);
        bytes.extend_from_slice(&word.to_le_bytes());
        for extra in &code.extra {
            bytes.extend_from_slice(&extra.to_le_bytes());
        }
    }
    if slots % 2 == 1 {
        bytes.extend_from_slice(&0u16.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{Prologue, UnwindError, prologue_bytes, unwind_info};
    use crate::backend::x86_64::regs::Gpr;

    const SET_FPREG: u8 = 3;
    const PUSH_NONVOL: u8 = 0;
    const ALLOC_SMALL: u8 = 2;
    const ALLOC_LARGE: u8 = 1;

    fn code(offset: u8, op: u8, opinfo: u8) -> [u8; 2] {
        (u16::from(offset) | (u16::from(op) << 8) | (u16::from(opinfo) << 12)).to_le_bytes()
    }

    /// The expected `UNWIND_INFO`: header, then every word.
    fn info(prologue_size: u8, slots: u8, words: &[[u8; 2]]) -> Vec<u8> {
        let mut out = vec![0x01, prologue_size, slots, 0x05];
        for word in words {
            out.extend_from_slice(word);
        }
        if words.len() % 2 == 1 {
            out.extend_from_slice(&[0, 0]);
        }
        out
    }

    #[test]
    fn frameless_prologue_is_push_and_fp_only() {
        let prologue = Prologue::new(0, &[]);
        assert_eq!(prologue_bytes(&prologue).unwrap(), [0x55, 0x48, 0x89, 0xE5]);
        assert_eq!(
            unwind_info(&prologue).unwrap(),
            info(
                4,
                2,
                &[code(4, SET_FPREG, 0), code(1, PUSH_NONVOL, Gpr::Rbp.code())]
            )
        );
    }

    #[test]
    fn small_frame_uses_alloc_small() {
        let prologue = Prologue::new(0x20, &[]);
        assert_eq!(
            prologue_bytes(&prologue).unwrap(),
            [0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x20]
        );
        // Three code slots: pad to four so the entry stays four-aligned.
        assert_eq!(
            unwind_info(&prologue).unwrap(),
            info(
                8,
                3,
                &[
                    code(8, ALLOC_SMALL, 4),
                    code(4, SET_FPREG, 0),
                    code(1, PUSH_NONVOL, Gpr::Rbp.code()),
                ]
            )
        );
    }

    #[test]
    fn saved_pushes_come_reversed_with_their_own_offsets() {
        let prologue = Prologue::new(0x30, &[Gpr::Rbx, Gpr::Rsi]);
        assert_eq!(
            prologue_bytes(&prologue).unwrap(),
            [0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x30, 0x53, 0x56]
        );
        // rsi pushed last (offset 10) is unwound first.
        assert_eq!(
            unwind_info(&prologue).unwrap(),
            info(
                0x0A,
                5,
                &[
                    code(10, PUSH_NONVOL, Gpr::Rsi.code()),
                    code(9, PUSH_NONVOL, Gpr::Rbx.code()),
                    code(8, ALLOC_SMALL, 6),
                    code(4, SET_FPREG, 0),
                    code(1, PUSH_NONVOL, Gpr::Rbp.code()),
                ]
            )
        );
    }

    #[test]
    fn extended_saved_registers_take_two_bytes() {
        let prologue = Prologue::new(0x40, &[Gpr::R12, Gpr::R15]);
        assert_eq!(
            prologue_bytes(&prologue).unwrap(),
            [
                0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x40, 0x41, 0x54, 0x41, 0x57
            ]
        );
        let info = unwind_info(&prologue).unwrap();
        // Six slots: even, so no padding.
        assert_eq!(info.len(), 4 + 12);
        assert_eq!(&info[4..6], &code(12, PUSH_NONVOL, Gpr::R15.code()));
        assert_eq!(&info[6..8], &code(10, PUSH_NONVOL, Gpr::R12.code()));
    }

    #[test]
    fn frames_past_alloc_small_use_alloc_large_with_imm32() {
        // 0x78 (120) fits imm8 but exceeds the 14-slot UWOP_ALLOC_SMALL cap.
        let prologue = Prologue::new(0x78, &[]);
        assert_eq!(
            prologue_bytes(&prologue).unwrap(),
            [
                0x55, 0x48, 0x89, 0xE5, 0x48, 0x81, 0xEC, 0x78, 0x00, 0x00, 0x00
            ]
        );
        // The size rides in the slot after the code: 0x78 / 8 = 15 slots.
        assert_eq!(
            unwind_info(&prologue).unwrap(),
            info(
                0x0B,
                4,
                &[
                    code(11, ALLOC_LARGE, 0),
                    15u16.to_le_bytes(),
                    code(4, SET_FPREG, 0),
                    code(1, PUSH_NONVOL, Gpr::Rbp.code()),
                ]
            )
        );
    }

    #[test]
    fn probed_frame_keeps_probe_bytes_out_of_the_codes() {
        let prologue = Prologue::new(0x3008, &[Gpr::Rbx, Gpr::R12, Gpr::R13]);
        assert_eq!(
            prologue_bytes(&prologue).unwrap(),
            [
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x48, 0x8B, 0xC4, // mov rax, rsp
                0x48, 0x8D, 0x8D, 0xF8, 0xCF, 0xFF, 0xFF, // lea rcx, [rbp - 0x3008]
                0x48, 0x2D, 0x00, 0x10, 0x00, 0x00, // sub rax, 0x1000
                0x85, 0x00, // test [rax], eax
                0x48, 0x39, 0xC8, // cmp rax, rcx
                0x77, 0xF3, // ja back 13 bytes
                0x48, 0x81, 0xEC, 0x08, 0x30, 0x00, 0x00, // sub rsp, 0x3008
                0x53, // push rbx
                0x41, 0x54, // push r12
                0x41, 0x55, // push r13
            ]
        );
        // Seven slots: three pushes, ALLOC_LARGE plus its size slot, SET_FPREG,
        // push rbp — the 23 probe bytes appear only through the offsets.
        assert_eq!(
            unwind_info(&prologue).unwrap(),
            info(
                0x27,
                7,
                &[
                    code(39, PUSH_NONVOL, Gpr::R13.code()),
                    code(37, PUSH_NONVOL, Gpr::R12.code()),
                    code(35, PUSH_NONVOL, Gpr::Rbx.code()),
                    code(34, ALLOC_LARGE, 0),
                    ((0x3008u32 / 8) as u16).to_le_bytes(),
                    code(4, SET_FPREG, 0),
                    code(1, PUSH_NONVOL, Gpr::Rbp.code()),
                ]
            )
        );
    }

    #[test]
    fn probe_starts_only_past_a_page() {
        // Exactly one page: no probe loop, imm8 sub.
        let at_page = Prologue::new(0x1000, &[]);
        assert_eq!(prologue_bytes(&at_page).unwrap().len(), 11);
        // One page and eight: probed, and the sub becomes imm32.
        let past_page = Prologue::new(0x1008, &[]);
        let bytes = prologue_bytes(&past_page).unwrap();
        assert_eq!(bytes.len(), 11 + 23);
        assert_eq!(&bytes[27..34], &[0x48, 0x81, 0xEC, 0x08, 0x10, 0x00, 0x00]);
    }

    #[test]
    fn rejects_bad_descriptions() {
        assert_eq!(
            unwind_info(&Prologue::new(0x24, &[])),
            Err(UnwindError::FrameNotMultipleOf8(0x24))
        );
        assert_eq!(
            unwind_info(&Prologue::new(0x80000, &[])),
            Err(UnwindError::FrameTooLarge(0x80000))
        );
        assert_eq!(
            unwind_info(&Prologue::new(8, &[Gpr::Rax])),
            Err(UnwindError::BadSavedRegister(Gpr::Rax))
        );
        assert_eq!(
            unwind_info(&Prologue::new(8, &[Gpr::Rbp])),
            Err(UnwindError::BadSavedRegister(Gpr::Rbp))
        );
        assert_eq!(
            unwind_info(&Prologue::new(8, &[Gpr::Rbx, Gpr::Rbx])),
            Err(UnwindError::DuplicateSavedRegister(Gpr::Rbx))
        );
    }
}

//! The x86-64 register set and its classes for allocation and the ABIs
//! (spec section 12.3).

/// A general purpose register. `code` is the hardware register number 0-15:
/// the low three bits are the ModRM/SIB field and bit 3 is the REX.B/R/X bit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Gpr {
    Rax,
    Rcx,
    Rdx,
    Rbx,
    Rsp,
    Rbp,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}

impl Gpr {
    pub const ALL: [Gpr; 16] = [
        Gpr::Rax,
        Gpr::Rcx,
        Gpr::Rdx,
        Gpr::Rbx,
        Gpr::Rsp,
        Gpr::Rbp,
        Gpr::Rsi,
        Gpr::Rdi,
        Gpr::R8,
        Gpr::R9,
        Gpr::R10,
        Gpr::R11,
        Gpr::R12,
        Gpr::R13,
        Gpr::R14,
        Gpr::R15,
    ];

    pub const fn code(self) -> u8 {
        self as u8
    }

    /// ModRM/SIB field: the low three bits of the code.
    pub const fn number(self) -> u8 {
        self as u8 & 7
    }

    /// REX.B/R/X bit: set for the registers only reachable with a REX prefix.
    pub const fn extended(self) -> bool {
        self as u8 & 8 != 0
    }

    /// 8-bit encodings reach spl/bpl/sil/dil and r8b-r15b only with a REX
    /// prefix; the high-byte aliases ah/ch/bh/dh are never used.
    pub const fn needs_rex_as_byte(self) -> bool {
        self as u8 >= 4
    }

    pub const fn from_code(code: u8) -> Option<Gpr> {
        match code {
            0 => Some(Gpr::Rax),
            1 => Some(Gpr::Rcx),
            2 => Some(Gpr::Rdx),
            3 => Some(Gpr::Rbx),
            4 => Some(Gpr::Rsp),
            5 => Some(Gpr::Rbp),
            6 => Some(Gpr::Rsi),
            7 => Some(Gpr::Rdi),
            8 => Some(Gpr::R8),
            9 => Some(Gpr::R9),
            10 => Some(Gpr::R10),
            11 => Some(Gpr::R11),
            12 => Some(Gpr::R12),
            13 => Some(Gpr::R13),
            14 => Some(Gpr::R14),
            15 => Some(Gpr::R15),
            _ => None,
        }
    }
}

/// An XMM register (SSE scalar double and packed 128-bit operands).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Xmm {
    Xmm0,
    Xmm1,
    Xmm2,
    Xmm3,
    Xmm4,
    Xmm5,
    Xmm6,
    Xmm7,
    Xmm8,
    Xmm9,
    Xmm10,
    Xmm11,
    Xmm12,
    Xmm13,
    Xmm14,
    Xmm15,
}

impl Xmm {
    pub const ALL: [Xmm; 16] = [
        Xmm::Xmm0,
        Xmm::Xmm1,
        Xmm::Xmm2,
        Xmm::Xmm3,
        Xmm::Xmm4,
        Xmm::Xmm5,
        Xmm::Xmm6,
        Xmm::Xmm7,
        Xmm::Xmm8,
        Xmm::Xmm9,
        Xmm::Xmm10,
        Xmm::Xmm11,
        Xmm::Xmm12,
        Xmm::Xmm13,
        Xmm::Xmm14,
        Xmm::Xmm15,
    ];

    pub const fn code(self) -> u8 {
        self as u8
    }

    /// ModRM reg/rm field: the low three bits of the code.
    pub const fn number(self) -> u8 {
        self as u8 & 7
    }

    /// REX.R/B bit: set for xmm8-xmm15.
    pub const fn extended(self) -> bool {
        self as u8 & 8 != 0
    }

    pub const fn from_code(code: u8) -> Option<Xmm> {
        match code {
            0 => Some(Xmm::Xmm0),
            1 => Some(Xmm::Xmm1),
            2 => Some(Xmm::Xmm2),
            3 => Some(Xmm::Xmm3),
            4 => Some(Xmm::Xmm4),
            5 => Some(Xmm::Xmm5),
            6 => Some(Xmm::Xmm6),
            7 => Some(Xmm::Xmm7),
            8 => Some(Xmm::Xmm8),
            9 => Some(Xmm::Xmm9),
            10 => Some(Xmm::Xmm10),
            11 => Some(Xmm::Xmm11),
            12 => Some(Xmm::Xmm12),
            13 => Some(Xmm::Xmm13),
            14 => Some(Xmm::Xmm14),
            15 => Some(Xmm::Xmm15),
            _ => None,
        }
    }
}

/// Register class of a [`Reg`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RegClass {
    Gpr,
    Xmm,
}

/// Any register: the unit the allocator and the ABIs name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Reg {
    Gpr(Gpr),
    Xmm(Xmm),
}

impl Reg {
    pub const fn class(self) -> RegClass {
        match self {
            Reg::Gpr(_) => RegClass::Gpr,
            Reg::Xmm(_) => RegClass::Xmm,
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Reg::Gpr(r) => r.code(),
            Reg::Xmm(r) => r.code(),
        }
    }
}

//! Closed-world compilation: reachability, keep rules and the compile report
//! (spec section 6). `reach` grows the reachable set from the entries until
//! it stops growing, `keep` applies the `[compile] keep` patterns, and the
//! result is the [`Program`] the later passes consume.

pub mod keep;
pub mod manifest;
pub mod reach;

use std::fmt;
use std::path::PathBuf;

use coil_ir::ir::Module;
use coil_ir::print::print_module;
use coil_ir::types::{ClassId, FnId};
use serde::{Deserialize, Serialize};

/// How a module of the program is provided (spec sections 10.6 and 11.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    /// Project or dependency source.
    Source,
    /// Pure Python compiled from the pinned stdlib tree.
    Stdlib,
    /// A shim file under `native/shims/`.
    Shim,
    /// The Rust runtime in `python312.dll`; no IR.
    Native,
    /// CPython's own `.pyd` extension; no IR.
    Pyd,
}

impl ModuleKind {
    /// The word the IR dump uses for a module with no IR to print.
    fn leaf_word(self) -> Option<&'static str> {
        match self {
            ModuleKind::Native => Some("native"),
            ModuleKind::Pyd => Some("pyd"),
            _ => None,
        }
    }
}

/// A module in the compiled program.
#[derive(Clone, Debug)]
pub struct ModuleState {
    pub name: String,
    /// The file the module was compiled from; empty for `Native` and `Pyd`
    /// leaves.
    pub file: PathBuf,
    pub kind: ModuleKind,
    pub is_package: bool,
    /// A keep rule matches the module: compiled whole and registered in the
    /// module table so a dynamic `import_module` finds it (spec section 6).
    pub kept: bool,
    /// The compiled IR; `None` for leaves.
    pub ir: Option<Module>,
}

/// A function of one module: indexes `Program::modules` and `Module::functions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnRef {
    pub module: u32,
    pub function: FnId,
}

// Ordered by module index, then function index (`FnId` itself is unordered).
impl Ord for FnRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.module, self.function.0).cmp(&(other.module, other.function.0))
    }
}

impl PartialOrd for FnRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A class of one module: indexes `Program::modules` and `Module::classes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClassRef {
    pub module: u32,
    pub class: ClassId,
}

impl Ord for ClassRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.module, self.class.0).cmp(&(other.module, other.class.0))
    }
}

impl PartialOrd for ClassRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The keep marks (spec section 6): what a `[compile] keep` rule pins down.
/// Kept modules register in the module table; kept classes are never sealed
/// and keep every member; kept functions always compile with a boxed entry.
#[derive(Clone, Debug, Default)]
pub struct KeepMarks {
    pub modules: std::collections::BTreeSet<u32>,
    pub functions: std::collections::BTreeSet<FnRef>,
    pub classes: std::collections::BTreeSet<ClassRef>,
}

/// The whole-program state `world` fixes and the later passes consume
/// (spec sections 5 and 6).
#[derive(Clone, Debug)]
pub struct Program {
    /// The closed module set, sorted by name.
    pub modules: Vec<ModuleState>,
    /// The reachable functions: the ones the build compiles and gives
    /// entries to.
    pub functions: std::collections::BTreeSet<FnRef>,
    /// The reachable classes.
    pub classes: std::collections::BTreeSet<ClassRef>,
    pub keep: KeepMarks,
    /// Reachable classes with no reachable subclass overrides, not kept, and
    /// a provably plain metaclass: candidates for direct calls (spec
    /// section 6).
    pub seal_candidates: std::collections::BTreeSet<ClassRef>,
}

impl Program {
    /// `--emit ir` after `world`: the IR of every compiled module in module
    /// order, with one line per runtime-provided leaf.
    pub fn dump_ir(&self) -> String {
        let mut out = String::new();
        for module in &self.modules {
            match (&module.ir, module.kind.leaf_word()) {
                (Some(ir), _) => out.push_str(&print_module(ir)),
                (None, Some(word)) => {
                    out.push_str(&format!("{} module {}\n", word, module.name));
                }
                (None, None) => {}
            }
        }
        out
    }
}

/// A keep pattern that failed to parse.
#[derive(Debug)]
pub struct PatternError {
    pub pattern: String,
    pub message: String,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid keep pattern '{}': {}",
            self.pattern, self.message
        )
    }
}

impl std::error::Error for PatternError {}

//! The compile job description: `job.json`, written by the Python driver
//! (`src/coil/native/driver.py`, spec section 13.2 step 4) and consumed by
//! `coilc build` and `coilc inspect`.
//!
//! The driver mirrors this schema; the JSON keys are the struct fields:
//!
//! ```json
//! {
//!   "entries": ["app"],
//!   "source_roots": ["src"],
//!   "dependency_roots": [],
//!   "stdlib_source_root": "~/.coil/cache/stdlib-src/3.12.10/Lib",
//!   "keep": ["plugins.*"],
//!   "abi": "cp312",
//!   "optimize": 0,
//!   "secure": false,
//!   "versioninfo": {},
//!   "subsystem": "console",
//!   "icon": "app.ico",
//!   "manifest": "launcher.manifest",
//!   "stack": "16MiB",
//!   "output": "dist/App.exe",
//!   "emit": "ir"
//! }
//! ```
//!
//! `entries`, `source_roots`, `abi`, `optimize` and `output` are required;
//! everything else defaults as shown (absent lists are empty, absent paths
//! and `versioninfo` are null, `emit` absent or null means none). Relative
//! paths - the roots, `icon`, `manifest` and `output` - resolve against the
//! job file's directory. Unknown fields are an error naming the field.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The compile job (spec section 13.2 step 4).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Modules compiled as entries, one executable each, resolved like any
    /// import against the roots below.
    pub entries: Vec<String>,
    /// Project source roots, searched in order.
    pub source_roots: Vec<PathBuf>,
    /// Installed dependency source roots, searched after the project roots.
    #[serde(default)]
    pub dependency_roots: Vec<PathBuf>,
    /// The pinned CPython `Lib/` tree (spec section 11.1), searched last.
    #[serde(default)]
    pub stdlib_source_root: Option<PathBuf>,
    /// Keep rules (spec section 6), the `[compile] keep` patterns.
    #[serde(default)]
    pub keep: Vec<String>,
    /// The impersonated ABI (spec section 1, decision 9).
    pub abi: Abi,
    /// Optimisation level, as `--optimize`: 0 keeps `assert`, 1 drops it,
    /// 2 also drops docstrings (spec section 4).
    pub optimize: Optimize,
    /// Secure mode: no source archive, no docstrings, basename `co_filename`
    /// (spec section 13.4).
    #[serde(default)]
    pub secure: bool,
    /// The VERSIONINFO dict for `RT_VERSION` (spec section 13.3), as the CLI's
    /// versioninfo resolution produces it.
    #[serde(default)]
    pub versioninfo: Option<serde_json::Value>,
    /// PE subsystem of the executable.
    #[serde(default)]
    pub subsystem: Subsystem,
    /// The `.ico` for `RT_GROUP_ICON`/`RT_ICON`.
    #[serde(default)]
    pub icon: Option<PathBuf>,
    /// The launcher manifest for `RT_MANIFEST`.
    #[serde(default)]
    pub manifest: Option<PathBuf>,
    /// Stack reserve of the executable, as `[compile] stack` (`"16MiB"`).
    #[serde(default)]
    pub stack: Option<String>,
    /// Path of the executable to write.
    pub output: PathBuf,
    /// What to dump while the pipeline runs: `"ir"` or `"asm"`; absent or
    /// null for none (spec sections 5 and 12).
    #[serde(default)]
    pub emit: Option<Emit>,
}

impl Job {
    /// Make the job's relative paths absolute against `base`, the job file's
    /// directory.
    fn anchor(&mut self, base: &Path) {
        fn rooted(base: &Path, path: PathBuf) -> PathBuf {
            if path.is_relative() {
                base.join(path)
            } else {
                path
            }
        }
        self.source_roots = self
            .source_roots
            .drain(..)
            .map(|p| rooted(base, p))
            .collect();
        self.dependency_roots = self
            .dependency_roots
            .drain(..)
            .map(|p| rooted(base, p))
            .collect();
        self.stdlib_source_root = self.stdlib_source_root.take().map(|p| rooted(base, p));
        self.icon = self.icon.take().map(|p| rooted(base, p));
        self.manifest = self.manifest.take().map(|p| rooted(base, p));
        self.output = rooted(base, std::mem::take(&mut self.output));
    }
}

/// Read and parse a job file; relative paths in it resolve against the job
/// file's directory.
pub fn load(path: &Path) -> Result<Job, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut job: Job = serde_json::from_str(&text).map_err(|source| LoadError::Parse {
        path: path.to_owned(),
        source,
    })?;
    job.anchor(path.parent().unwrap_or(Path::new(".")));
    Ok(job)
}

/// Everything reading and parsing a job file rejects.
#[derive(Debug)]
pub enum LoadError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Read { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            LoadError::Parse { path, source } => {
                write!(f, "invalid job {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Read { source, .. } => Some(source),
            LoadError::Parse { source, .. } => Some(source),
        }
    }
}

/// The impersonated ABI: cp312 first, then cp313 (spec section 1, decision 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Abi {
    #[serde(rename = "cp312")]
    Cp312,
}

impl Abi {
    pub fn as_str(self) -> &'static str {
        match self {
            Abi::Cp312 => "cp312",
        }
    }
}

/// Optimisation level: 0, 1 or 2, as `--optimize` (spec section 4). Any other
/// value is an error naming the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Optimize(pub u8);

impl<'de> Deserialize<'de> for Optimize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let level = u8::deserialize(deserializer)?;
        if level > 2 {
            return Err(serde::de::Error::custom(format!(
                "optimize must be 0, 1 or 2, got {level}"
            )));
        }
        Ok(Optimize(level))
    }
}

/// The PE subsystem: `console` (default) or `windows`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subsystem {
    #[default]
    Console,
    Windows,
}

/// What the build dumps while it runs (spec sections 5 and 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Emit {
    /// IR text after each pass.
    Ir,
    /// Disassembly of the linked image.
    Asm,
}

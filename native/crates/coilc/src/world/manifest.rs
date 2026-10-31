//! The stdlib manifest, `native/manifest/stdlib.toml` (spec sections 10.6 and
//! 11.1): how a compiled build provides every module of the embeddable
//! CPython 3.12.10 runtime. The file uses the small TOML subset this module
//! parses - `[module]` headers, `kind = "..."` and `reason = "..."` string
//! keys, `#` comments - so `coilc` carries no TOML dependency; anything
//! outside the subset is an error naming the line.

use std::fmt;
use std::path::{Path, PathBuf};

/// How a module of the embedded runtime is provided (spec section 10.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Pure Python, compiled from the pinned `Lib/` tree.
    Compiled,
    /// A Python file under `native/shims/`.
    Shim,
    /// The Rust runtime in `python312.dll`; a leaf with no source.
    Native,
    /// CPython's own extension from the embeddable zip; a leaf.
    Pyd,
    /// No compiled-build implementation; statically importing one is a build
    /// error naming the module and its reason (spec section 11.1).
    Unsupported,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "compiled" => Some(Kind::Compiled),
            "shim" => Some(Kind::Shim),
            "native" => Some(Kind::Native),
            "pyd" => Some(Kind::Pyd),
            "unsupported" => Some(Kind::Unsupported),
            _ => None,
        }
    }

    /// The program-side kind of an entry that resolves to a module; `None`
    /// for unsupported entries, which never do.
    pub fn module_kind(self) -> Option<super::ModuleKind> {
        match self {
            Kind::Compiled => Some(super::ModuleKind::Stdlib),
            Kind::Shim => Some(super::ModuleKind::Shim),
            Kind::Native => Some(super::ModuleKind::Native),
            Kind::Pyd => Some(super::ModuleKind::Pyd),
            Kind::Unsupported => None,
        }
    }
}

/// One manifest entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: Kind,
    /// Why the module is unsupported; required for `Kind::Unsupported`.
    pub reason: Option<String>,
}

/// The manifest: module name to entry.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    entries: std::collections::BTreeMap<String, Entry>,
}

impl Manifest {
    pub fn get(&self, module: &str) -> Option<&Entry> {
        self.entries.get(module)
    }

    /// Every entry, in module-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Entry)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.as_str(), entry))
    }

    /// Read and parse a manifest file.
    pub fn load(path: &Path) -> Result<Manifest, Error> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        parse(&text).map_err(|message| Error::Parse {
            path: path.to_owned(),
            message,
        })
    }
}

/// Locate the manifest and the shims directory: beside the executable (the
/// shipped layout, `src/coil/_runtime/`), else the checked-in development
/// paths relative to this crate.
pub fn discover() -> Result<(PathBuf, PathBuf), DiscoveryError> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        let manifest = dir.join("stdlib.toml");
        if manifest.is_file() {
            return Ok((manifest, dir.join("shims")));
        }
        tried.push(manifest);
    }
    // `CARGO_MANIFEST_DIR` is native/crates/coilc in development and tests.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("manifest")
        .join("stdlib.toml");
    if manifest.is_file() {
        let shims = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("shims");
        return Ok((manifest, shims));
    }
    tried.push(manifest);
    Err(DiscoveryError { tried })
}

/// Why the manifest could not be located.
#[derive(Debug)]
pub struct DiscoveryError {
    /// Every path that was tried and is not a manifest.
    pub tried: Vec<PathBuf>,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot find the stdlib manifest, tried:")?;
        for (i, path) in self.tried.iter().enumerate() {
            if i > 0 {
                write!(f, ",")?;
            }
            write!(f, " {}", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for DiscoveryError {}

/// Everything reading and parsing a manifest rejects.
#[derive(Debug)]
pub enum Error {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Parse { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Parse the manifest's TOML subset: `[module]` headers, `kind = "..."` and
/// `reason = "..."` keys, `#` comments and blank lines. A `reason` belongs to
/// `unsupported` entries only.
pub fn parse(text: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest::default();
    let mut section: Option<String> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = match raw.split_once('#') {
            Some((code, _)) => code.trim(),
            None => raw.trim(),
        };
        let what = |message: String| format!("line {}: {message}", index + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if name.is_empty() || !valid_name(name) {
                return Err(what(format!("invalid module name '{name}'")));
            }
            if manifest.entries.contains_key(name) {
                return Err(what(format!("duplicate module '{name}'")));
            }
            section = Some(name.to_owned());
            manifest.entries.insert(
                name.to_owned(),
                Entry {
                    kind: Kind::Compiled,
                    reason: None,
                },
            );
            continue;
        }
        let Some(name) = &section else {
            return Err(what("key outside a [module] section".to_owned()));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(what(format!("expected 'key = \"value\"', got '{line}'")));
        };
        let quoted = |value: &str| -> Result<String, String> {
            let text = value.trim();
            unquote(text)
                .ok_or_else(|| what(format!("expected a double-quoted string, got '{text}'")))
        };
        let entry = manifest.entries.get_mut(name).expect("section inserted");
        match key.trim() {
            "kind" => {
                let kind = quoted(value)?;
                entry.kind =
                    Kind::parse(&kind).ok_or_else(|| what(format!("unknown kind '{kind}'")))?;
            }
            "reason" => entry.reason = Some(quoted(value)?),
            other => return Err(what(format!("unknown key '{other}'"))),
        }
    }
    for (name, entry) in &manifest.entries {
        if entry.reason.is_some() != (entry.kind == Kind::Unsupported) {
            let message = if entry.reason.is_some() {
                "reason belongs to unsupported entries only"
            } else {
                "an unsupported entry needs a reason"
            };
            return Err(format!("module '{name}': {message}"));
        }
    }
    Ok(manifest)
}

/// A module header name: letters, digits, underscores and dashes (a TOML bare
/// key), which covers every runtime module name.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Strip one pair of double quotes; the manifest's reasons are plain ASCII
/// text, so no escape sequences.
fn unquote(text: &str) -> Option<String> {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kinds_and_reasons() {
        let m = parse(
            "# comment\n\
             [math]\nkind = \"native\"\n\n\
             [_pyio]\nkind = \"compiled\"\n\
             [_symtable] # trailing\nkind = \"unsupported\"\n  reason = \"no symtable\"\n",
        )
        .unwrap();
        assert_eq!(m.get("math").unwrap().kind, Kind::Native);
        assert_eq!(m.get("_pyio").unwrap().kind, Kind::Compiled);
        let symtable = m.get("_symtable").unwrap();
        assert_eq!(symtable.kind, Kind::Unsupported);
        assert_eq!(symtable.reason.as_deref(), Some("no symtable"));
        assert!(m.get("nope").is_none());
    }

    #[test]
    fn rejects_malformed_tables() {
        for (text, expected) in [
            ("kind = \"native\"", "outside a [module] section"),
            ("[math]\nkind = \"fast\"", "unknown kind 'fast'"),
            ("[math]\nweight = 3", "unknown key 'weight'"),
            ("[math]\nkind = native", "double-quoted string"),
            ("[]\nkind = \"native\"", "invalid module name"),
            ("[math]\n[math]\n", "duplicate module"),
            (
                "[math]\nkind = \"native\"\nreason = \"x\"",
                "reason belongs",
            ),
            ("[math]\nkind = \"unsupported\"", "needs a reason"),
        ] {
            let message = parse(text).unwrap_err();
            assert!(message.contains(expected), "{text:?}: {message}");
        }
    }

    #[test]
    fn discovers_the_checked_in_manifest() {
        let (manifest, shims) = discover().unwrap();
        assert!(
            manifest.is_file(),
            "manifest missing at {}",
            manifest.display()
        );
        assert!(
            std::fs::read_to_string(&manifest)
                .unwrap()
                .contains("[builtins]"),
            "the checked-in manifest is not the stdlib manifest"
        );
        assert!(
            shims.is_dir(),
            "shims directory missing at {}",
            shims.display()
        );
    }
}

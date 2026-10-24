//! `coilc build`: resolve the job's entries, run the per-module front end and
//! cache each module's IR (spec sections 2, 11.1 and 13.2 step 4). The module
//! set is the entries plus every module their static imports resolve to,
//! transitively; the closed-world refinement (attribute loads, keep rules,
//! dynamic imports) is `world`'s (spec section 6). The pipeline continues
//! from here: world, analysis, opt, backend and link consume the module set
//! this produces.
//!
//! The IR cache lives at `<cache>/native/ir/<abi>/<optimize>/<module>.ir`,
//! `<cache>` being `~/.coil/cache` or `COIL_CACHE` (spec section 11.1). A
//! cache entry is used when its recorded source SHA-256 matches the source on
//! disk and its serialized IR carries this coil-ir's schema hash; a changed
//! source or a different compiler misses and rebuilds the module.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use coil_front::lower::{LowerOptions, lower_module};
use coil_front::parser::parse_module;
use coil_front::symtable;
use coil_ir::ir::{Const, Function, Instr, Module};
use coil_ir::serialize;
use coil_ir::types::{ConstIdx, InstrId, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::job::Job;

/// The cache root: `COIL_CACHE` when set (so tests and side-by-side builds
/// stay out of the shared cache), else `~/.coil/cache` (spec section 11.1).
pub fn default_cache_root() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os("COIL_CACHE") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or(Error::CacheRoot)?;
    Ok(Path::new(&home).join(".coil").join("cache"))
}

/// What a build did, printed as its one-line summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub modules: usize,
    pub functions: usize,
    pub cache_hits: usize,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} modules, {} functions, {} cache hits",
            self.modules, self.functions, self.cache_hits
        )
    }
}

/// Why a module is in the build: a job entry, or the import of a module.
#[derive(Clone, Debug)]
pub enum Origin {
    Entry,
    Import { module: String, line: u32 },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Entry => write!(f, "as a job entry"),
            Origin::Import { module, line } => write!(f, "imported by {module} (line {line})"),
        }
    }
}

/// Everything `coilc build` rejects, naming the module or file that caused it.
#[derive(Debug)]
pub enum Error {
    /// No `COIL_CACHE`, `HOME` or `USERPROFILE` to hang the cache on.
    CacheRoot,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A module name no root resolves, for an entry or an import.
    Resolve { module: String, origin: Origin },
    /// A relative import reaching past the top-level package.
    RelativeBeyondTop {
        importer: String,
        level: i32,
        line: u32,
    },
    /// A parse, symtable or lowering failure, naming the module.
    Front { module: String, message: String },
    /// An `Import` instruction whose operands the lowering contract keeps
    /// compile-time constant were not.
    ImportShape {
        module: String,
        function: String,
        what: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CacheRoot => write!(
                f,
                "cannot determine the cache directory: set COIL_CACHE or HOME"
            ),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Resolve { module, origin } => {
                write!(f, "cannot resolve module '{module}' {origin}")
            }
            Error::RelativeBeyondTop {
                importer,
                level,
                line,
            } => write!(
                f,
                "relative import level {level} in module '{importer}' \
                 (line {line}) reaches past the top-level package"
            ),
            Error::Front { module, message } => write!(f, "module '{module}': {message}"),
            Error::ImportShape {
                module,
                function,
                what,
            } => write!(f, "module '{module}', function '{function}': {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A static import as the front end lowered it: the module part, the
/// from-list names (empty when there is none), the relative level and the
/// importing line. Stored in the cache meta so a cache hit schedules the
/// same closure a fresh compile would.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedImport {
    module: String,
    names: Vec<String>,
    level: i32,
    line: u32,
}

/// The per-module cache metadata committing `<module>.ir` to a source digest.
#[derive(Serialize, Deserialize)]
struct CacheMeta {
    source_sha256: String,
    imports: Vec<CachedImport>,
}

/// A resolved module: its file and whether it is a package (`__init__.py`).
struct Resolved {
    name: String,
    file: PathBuf,
    is_package: bool,
}

/// Build the job's module closure through the front end, using and filling
/// the IR cache.
pub fn run(job: &Job, cache_root: &Path) -> Result<Summary, Error> {
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(job.source_roots.iter().cloned());
    roots.extend(job.dependency_roots.iter().cloned());
    if let Some(stdlib) = &job.stdlib_source_root {
        roots.push(stdlib.clone());
    }
    let cache_dir = cache_root
        .join("native")
        .join("ir")
        .join(job.abi.as_str())
        .join(job.optimize.0.to_string());
    fs::create_dir_all(&cache_dir).map_err(|source| Error::Io {
        path: cache_dir.clone(),
        source,
    })?;
    let mut build = Build {
        roots,
        cache_dir,
        optimize: job.optimize.0,
        done: HashSet::new(),
        queue: job
            .entries
            .iter()
            .map(|entry| (entry.clone(), Origin::Entry))
            .collect(),
        modules: 0,
        functions: 0,
        cache_hits: 0,
    };
    while let Some((name, origin)) = build.queue.pop_front() {
        if build.done.contains(&name) {
            continue;
        }
        let chain = build.resolve(&name, &origin)?;
        for module in chain {
            if build.done.insert(module.name.clone()) {
                build.modules += 1;
                let imports = build.compile(&module)?;
                build.schedule(&module.name, module.is_package, &imports)?;
            }
        }
    }
    Ok(Summary {
        modules: build.modules,
        functions: build.functions,
        cache_hits: build.cache_hits,
    })
}

struct Build {
    roots: Vec<PathBuf>,
    cache_dir: PathBuf,
    optimize: u8,
    done: HashSet<String>,
    queue: VecDeque<(String, Origin)>,
    modules: usize,
    functions: usize,
    cache_hits: usize,
}

impl Build {
    fn resolve(&self, name: &str, origin: &Origin) -> Result<Vec<Resolved>, Error> {
        self.probe(name).ok_or_else(|| Error::Resolve {
            module: name.to_owned(),
            origin: origin.clone(),
        })
    }

    /// The chain a dotted name resolves to: every ancestor package's
    /// `__init__.py` and the module itself, from the first root that carries
    /// the whole chain. `None` when no root does.
    fn probe(&self, name: &str) -> Option<Vec<Resolved>> {
        let parts: Vec<&str> = name.split('.').collect();
        self.roots.iter().find_map(|root| resolve_in(root, &parts))
    }

    /// Front-end one module, from the cache when the source and schema match,
    /// returning its static imports for the closure walk.
    fn compile(&mut self, module: &Resolved) -> Result<Vec<CachedImport>, Error> {
        let source = fs::read(&module.file).map_err(|source| Error::Io {
            path: module.file.clone(),
            source,
        })?;
        let digest = sha256_hex(&source);
        if let Some((imports, ir)) = self.load_cache(module, &digest) {
            self.functions += ir.functions.len();
            self.cache_hits += 1;
            return Ok(imports);
        }
        let (ast, mut interner) =
            parse_module(&source, Some(module.file.as_path())).map_err(|error| Error::Front {
                module: module.name.clone(),
                message: positioned(&error.message, error.lineno),
            })?;
        let table = symtable::symtable(&ast, &mut interner).map_err(|error| Error::Front {
            module: module.name.clone(),
            message: positioned(&error.message, error.lineno),
        })?;
        let ir = lower_module(
            &module.name,
            module.file.clone(),
            &ast,
            &source,
            &interner,
            &table,
            LowerOptions {
                optimize: self.optimize,
            },
        )
        .map_err(|error| Error::Front {
            module: module.name.clone(),
            message: error.to_string(),
        })?;
        let imports = extract_imports(&ir)?;
        self.functions += ir.functions.len();
        self.store_cache(module, &digest, &ir, &imports)?;
        Ok(imports)
    }

    fn load_cache(&self, module: &Resolved, digest: &str) -> Option<(Vec<CachedImport>, Module)> {
        let ir_bytes = fs::read(self.cache_dir.join(format!("{}.ir", module.name))).ok()?;
        let meta_bytes = fs::read(self.cache_dir.join(format!("{}.meta", module.name))).ok()?;
        let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;
        if meta.source_sha256 != digest {
            return None;
        }
        let mut ir = serialize::deserialize(&ir_bytes).ok()?;
        // The cache is keyed by source content, so the same bytes at a new
        // path stay a hit; point the module at this job's file.
        ir.file = module.file.clone();
        Some((meta.imports, ir))
    }

    fn store_cache(
        &self,
        module: &Resolved,
        digest: &str,
        ir: &Module,
        imports: &[CachedImport],
    ) -> Result<(), Error> {
        let meta = CacheMeta {
            source_sha256: digest.to_owned(),
            imports: imports.to_vec(),
        };
        let meta_bytes = serde_json::to_vec(&meta).expect("cache metadata serializes to JSON");
        write_atomic(
            &self.cache_dir.join(format!("{}.ir", module.name)),
            &serialize::serialize(ir),
        )?;
        write_atomic(
            &self.cache_dir.join(format!("{}.meta", module.name)),
            &meta_bytes,
        )
    }

    /// Queue the modules an import refers to: the module (or the package a
    /// relative level resolves to) must resolve, and a from-list name only
    /// matters when it also exists as a module, otherwise it is a plain
    /// attribute of the imported package.
    fn schedule(
        &mut self,
        importer: &str,
        importer_is_package: bool,
        imports: &[CachedImport],
    ) -> Result<(), Error> {
        for import in imports {
            let origin = Origin::Import {
                module: importer.to_owned(),
                line: import.line,
            };
            let target = if import.level == 0 {
                import.module.clone()
            } else {
                let base = relative_base(importer, importer_is_package, import.level).ok_or_else(
                    || Error::RelativeBeyondTop {
                        importer: importer.to_owned(),
                        level: import.level,
                        line: import.line,
                    },
                )?;
                if import.module.is_empty() {
                    base
                } else {
                    format!("{base}.{}", import.module)
                }
            };
            self.schedule_required(&target, &origin);
            for name in &import.names {
                if name == "*" {
                    continue;
                }
                self.schedule_optional(&format!("{target}.{name}"), &origin);
            }
        }
        Ok(())
    }

    fn schedule_required(&mut self, name: &str, origin: &Origin) {
        self.queue.push_back((name.to_owned(), origin.clone()));
    }

    fn schedule_optional(&mut self, name: &str, origin: &Origin) {
        if self.probe(name).is_some() {
            self.schedule_required(name, origin);
        }
    }
}

/// The root-relative chain a dotted name resolves to within one root:
/// `a.b.c` needs `a/__init__.py` and `a/b/__init__.py`, then `c.py` or
/// `c/__init__.py`. `None` when the root does not carry the whole chain.
fn resolve_in(root: &Path, parts: &[&str]) -> Option<Vec<Resolved>> {
    let mut dir = root.to_path_buf();
    let mut name = String::new();
    let mut chain = Vec::with_capacity(parts.len());
    for part in &parts[..parts.len() - 1] {
        dir.push(part);
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(part);
        let init = dir.join("__init__.py");
        if !init.is_file() {
            return None;
        }
        chain.push(Resolved {
            name: name.clone(),
            file: init,
            is_package: true,
        });
    }
    let last = parts[parts.len() - 1];
    if !name.is_empty() {
        name.push('.');
    }
    name.push_str(last);
    let file = dir.join(format!("{last}.py"));
    if file.is_file() {
        chain.push(Resolved {
            name,
            file,
            is_package: false,
        });
        return Some(chain);
    }
    let init = dir.join(last).join("__init__.py");
    if init.is_file() {
        chain.push(Resolved {
            name,
            file: init,
            is_package: true,
        });
        return Some(chain);
    }
    None
}

/// The package a level-`level` relative import in `module` resolves against:
/// the module's own package, one level up per extra dot. `None` when it
/// reaches past the top-level package.
fn relative_base(module: &str, is_package: bool, level: i32) -> Option<String> {
    let up = usize::try_from(level - 1).ok()?;
    let mut parts: Vec<&str> = module.split('.').collect();
    if !is_package {
        parts.pop();
    }
    let keep = parts.len().checked_sub(up)?;
    if keep == 0 {
        return None;
    }
    Some(parts[..keep].join("."))
}

/// The static imports a lowered module performs: its `Import` instructions,
/// including the ones lowering synthesizes (`typing` for PEP 695 parameters,
/// `collections.abc` for isinstance bounds), which the compiled code executes
/// just the same.
fn extract_imports(module: &Module) -> Result<Vec<CachedImport>, Error> {
    let mut imports = Vec::new();
    for function in &module.functions {
        imports.extend(function_imports(module, function)?);
    }
    Ok(imports)
}

fn function_imports(module: &Module, function: &Function) -> Result<Vec<CachedImport>, Error> {
    let module_name = module.consts.sym_str(module.name);
    let shape = |what: &'static str| Error::ImportShape {
        module: module_name.to_owned(),
        function: module.consts.sym_str(function.qualname).to_owned(),
        what,
    };
    // Import operands are compile-time constants by the lowering contract;
    // index the instructions that define them.
    let mut consts: HashMap<Value, ConstIdx> = HashMap::new();
    for block in &function.blocks {
        for instr in &block.instrs {
            if let Instr::Const { idx, into } = instr {
                consts.insert(*into, *idx);
            }
        }
    }
    let mut imports = Vec::new();
    let mut id = InstrId(0);
    for block in &function.blocks {
        for instr in &block.instrs {
            if let Instr::Import {
                name,
                fromlist,
                level,
                ..
            } = instr
            {
                imports.push(CachedImport {
                    module: module.consts.sym_str(*name).to_owned(),
                    names: import_names(module, &consts, fromlist, &shape)?,
                    level: import_level(module, &consts, level, &shape)?,
                    line: line_of(&function.lines, id),
                });
            }
            id.0 += 1;
        }
    }
    Ok(imports)
}

fn import_level(
    module: &Module,
    consts: &HashMap<Value, ConstIdx>,
    level: &Value,
    shape: &impl Fn(&'static str) -> Error,
) -> Result<i32, Error> {
    let idx = consts
        .get(level)
        .ok_or_else(|| shape("import level is not a compile-time constant"))?;
    match &module.consts.consts[idx.0 as usize] {
        Const::Int(level) => {
            i32::try_from(*level).map_err(|_| shape("import level is out of range"))
        }
        _ => Err(shape("import level is not an integer")),
    }
}

fn import_names(
    module: &Module,
    consts: &HashMap<Value, ConstIdx>,
    fromlist: &Value,
    shape: &impl Fn(&'static str) -> Error,
) -> Result<Vec<String>, Error> {
    let idx = consts
        .get(fromlist)
        .ok_or_else(|| shape("import from-list is not a compile-time constant"))?;
    match &module.consts.consts[idx.0 as usize] {
        Const::None => Ok(Vec::new()),
        Const::Tuple(items) => items
            .iter()
            .map(|&item| match &module.consts.consts[item.0 as usize] {
                Const::Str(name) => Ok(module.consts.sym_str(*name).to_owned()),
                _ => Err(shape("from-list entry is not a string")),
            })
            .collect(),
        _ => Err(shape("import from-list is not a tuple")),
    }
}

/// The source line of an instruction: its recorded line, or the closest
/// recorded line before it.
fn line_of(lines: &[(InstrId, u32, u32)], id: InstrId) -> u32 {
    match lines.binary_search_by_key(&id, |&(key, _, _)| key) {
        Ok(at) => lines[at].1,
        Err(at) => at.checked_sub(1).map_or(0, |prev| lines[prev].1),
    }
}

/// A front-end message with its line, when the error carries one.
fn positioned(message: &str, lineno: Option<i32>) -> String {
    match lineno {
        Some(line) => format!("{message} (line {line})"),
        None => message.to_owned(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Write through a sibling temp file so a reader never sees a torn cache
/// entry; the metadata lands last, as the commit point.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).map_err(|source| Error::Io {
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, path).map_err(|source| Error::Io { path: tmp, source })
}

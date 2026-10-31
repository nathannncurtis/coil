//! The closed-world pass end to end (spec sections 6, 10.6 and 11.1): a
//! plugins package reached only through `import_module`, a module reached
//! through constant-name `getattr`, the manifest's kinds, the errors an
//! unsupported import or a missing shim name, and the keep marks and seal
//! candidates the program carries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use coil_ir::types::{ClassId, FnId};
use coilc::build;
use coilc::job;
use coilc::world::{ClassRef, FnRef, ModuleKind};

const MAIN_PLUGINS: &str = "import importlib\n\n\ndef load(name):\n    module = importlib.import_module(\"plugins.\" + name)\n    return module.value\n";
const IMPORTLIB: &str = "def import_module(name):\n    raise RuntimeError(name)\n";

const MAIN_GETATTR: &str = "import pkg\n\n\ndef go():\n    return getattr(pkg, \"leaf\")\n";
const PKG_INIT: &str = "import pkg.leaf\n";
const PKG_LEAF: &str = "def f():\n    return 42\n";

const MAIN_MODELS: &str = "import models.user\n\n\ndef go():\n    return models.user.Point(1, 2)\n";
const MODELS_USER: &str = "class Base:\n    def m(self):\n        return 1\n\n\nclass Sub(Base):\n    def m(self):\n        return 2\n\n\nclass Handler:\n    def handle(self):\n        return 0\n\n\nclass Point:\n    def __init__(self, x, y):\n        self.x = x\n        self.y = y\n\n\ndef helper(x):\n    return x + 1\n";

/// A scratch tree under the cargo target temp dir, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("coilc-world-{tag}"));
        let _ = fs::remove_dir_all(&root);
        Tree { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn job(&self, tag: &str, keep: &[&str]) -> PathBuf {
        let keep = if keep.is_empty() {
            String::new()
        } else {
            format!(
                ", \"keep\": [{}]",
                keep.iter()
                    .map(|rule| format!("\"{rule}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        self.write(
            &format!("{tag}.json"),
            &format!(
                r#"{{"entries": ["main"], "source_roots": ["src"]{keep}, "abi": "cp312",
                    "optimize": 0, "output": "app.exe"}}"#
            ),
        )
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn module_names(program: &coilc::world::Program) -> Vec<&str> {
    program
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect()
}

/// The index of a module in the program, in sorted order.
fn module_of(program: &coilc::world::Program, name: &str) -> usize {
    program
        .modules
        .iter()
        .position(|module| module.name == name)
        .unwrap_or_else(|| panic!("module '{name}' is not in the program"))
}

/// A class of the named module by its defining name.
fn class_of(program: &coilc::world::Program, module: &str, class: &str) -> ClassRef {
    let index = module_of(program, module);
    let ir = program.modules[index].ir.as_ref().unwrap();
    let id = ir
        .classes
        .iter()
        .position(|desc| ir.consts.sym_str(desc.name) == class)
        .unwrap_or_else(|| panic!("class '{class}' is not in '{module}'"));
    ClassRef {
        module: index as u32,
        class: ClassId(id as u32),
    }
}

#[test]
fn plugins_package_needs_the_keep_rule() {
    let tree = Tree::new("plugins");
    let cache = tree.path("cache");
    tree.write("src/main.py", MAIN_PLUGINS);
    tree.write("src/importlib.py", IMPORTLIB);
    tree.write("src/plugins/__init__.py", "VALUE = 1\n");
    tree.write("src/plugins/alpha.py", "value = 10\n");
    tree.write("src/plugins/beta.py", "value = 20\n");

    // Nothing statically imports `plugins`: the variable-driven
    // `import_module` cannot name it, so the build does not contain it.
    let job = job::load(&tree.job("plain", &[])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    assert_eq!(module_names(&outcome.program), ["importlib", "main"]);

    // With `plugins.*` declared, the package and every module under it join
    // the program, kept, so `import_module("plugins." + name)` finds them.
    let job = job::load(&tree.job("kept", &["plugins.*"])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    assert_eq!(
        module_names(&outcome.program),
        [
            "importlib",
            "main",
            "plugins",
            "plugins.alpha",
            "plugins.beta"
        ]
    );
    for name in ["plugins", "plugins.alpha", "plugins.beta"] {
        let index = module_of(&outcome.program, name);
        assert!(
            outcome.program.modules[index].kept,
            "'{name}' is not marked kept"
        );
        assert!(
            outcome.program.keep.modules.contains(&(index as u32)),
            "'{name}' has no keep mark"
        );
    }
}

#[test]
fn constant_name_getattr_reaches_a_module() {
    let tree = Tree::new("getattr");
    let cache = tree.path("cache");
    tree.write("src/main.py", MAIN_GETATTR);
    tree.write("src/pkg/__init__.py", PKG_INIT);
    tree.write("src/pkg/leaf.py", PKG_LEAF);
    let job = job::load(&tree.job("job", &[])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    assert_eq!(module_names(&outcome.program), ["main", "pkg", "pkg.leaf"]);
    // The attribute `getattr(pkg, "leaf")` loads is pkg.leaf's own code: its
    // function is in the reachable set.
    let index = module_of(&outcome.program, "pkg.leaf");
    let ir = outcome.program.modules[index].ir.as_ref().unwrap();
    let f = ir
        .functions
        .iter()
        .position(|function| ir.consts.sym_str(function.qualname) == "f")
        .expect("pkg.leaf.f is not in the IR");
    let f = FnRef {
        module: index as u32,
        function: FnId(f as u32),
    };
    assert!(
        outcome.program.functions.contains(&f),
        "pkg.leaf.f is not reachable"
    );
}

#[test]
fn manifest_leaves_resolve_without_source() {
    let tree = Tree::new("leaves");
    let cache = tree.path("cache");
    tree.write(
        "src/main.py",
        "import math\nimport select\n\n\ndef main():\n    return math.sqrt(4)\n",
    );
    let job = job::load(&tree.job("job", &[])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    assert_eq!(module_names(&outcome.program), ["main", "math", "select"]);
    for (name, kind) in [("math", ModuleKind::Native), ("select", ModuleKind::Pyd)] {
        let module = &outcome.program.modules[module_of(&outcome.program, name)];
        assert_eq!(module.kind, kind, "'{name}' kind");
        assert!(module.ir.is_none(), "'{name}' has no IR to carry");
        assert!(module.file.as_os_str().is_empty(), "'{name}' has no file");
    }
    // The IR dump names each runtime-provided leaf.
    let dump = outcome.program.dump_ir();
    assert!(dump.contains("native module math\n"), "dump: {dump}");
    assert!(dump.contains("pyd module select\n"), "dump: {dump}");
}

#[test]
fn unsupported_import_names_the_module_and_reason() {
    let tree = Tree::new("unsupported");
    let cache = tree.path("cache");
    tree.write("src/main.py", "import _symtable\n");
    let job = job::load(&tree.job("job", &[])).unwrap();
    let error = build::program(&job, &cache).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("module '_symtable'"), "error: {message}");
    assert!(message.contains("is not supported"), "error: {message}");
    assert!(message.contains("imported by main"), "error: {message}");
    assert!(
        message.contains("symtable is not CPython's"),
        "error: {message}"
    );
}

#[test]
fn a_missing_shim_names_the_path() {
    let tree = Tree::new("shim");
    let cache = tree.path("cache");
    tree.write("src/main.py", "import _json\n");
    let job = job::load(&tree.job("job", &[])).unwrap();
    let error = build::program(&job, &cache).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("module '_json'"), "error: {message}");
    assert!(message.contains("needs a shim"), "error: {message}");
    assert!(
        message.contains(&format!("shims{}_json.py", std::path::MAIN_SEPARATOR)),
        "error: {message}"
    );
}

#[test]
fn keep_marks_and_seal_candidates() {
    let tree = Tree::new("seal");
    let cache = tree.path("cache");
    tree.write("src/main.py", MAIN_MODELS);
    tree.write("src/models/__init__.py", "");
    tree.write("src/models/user.py", MODELS_USER);
    let job = job::load(&tree.job("job", &["models.user:Handler", "models.user:helper"])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();

    let handler = class_of(&outcome.program, "models.user", "Handler");
    let base = class_of(&outcome.program, "models.user", "Base");
    let sub = class_of(&outcome.program, "models.user", "Sub");
    let point = class_of(&outcome.program, "models.user", "Point");
    // The kept class keeps its mark and is never a seal candidate.
    assert!(
        outcome.program.keep.classes.contains(&handler),
        "Handler is not kept"
    );
    assert!(
        !outcome.program.seal_candidates.contains(&handler),
        "kept Handler must not seal"
    );
    // Base has a reachable subclass (Sub) overriding `m`; Sub itself and the
    // untouched Point are candidates.
    assert!(
        !outcome.program.seal_candidates.contains(&base),
        "Base is overridden by Sub"
    );
    assert!(
        outcome.program.seal_candidates.contains(&sub),
        "Sub should be a seal candidate"
    );
    assert!(
        outcome.program.seal_candidates.contains(&point),
        "Point should be a seal candidate"
    );

    // The kept function carries its mark; the module itself is not kept by a
    // member rule.
    let index = module_of(&outcome.program, "models.user");
    let ir = outcome.program.modules[index].ir.as_ref().unwrap();
    let helper = ir
        .functions
        .iter()
        .position(|function| ir.consts.sym_str(function.qualname) == "helper")
        .expect("models.user.helper is not in the IR");
    let helper = FnRef {
        module: index as u32,
        function: FnId(helper as u32),
    };
    assert!(
        outcome.program.keep.functions.contains(&helper),
        "models.user.helper is not kept"
    );
    assert!(
        !outcome.program.modules[index].kept,
        "a member rule must not keep the whole module"
    );
    assert!(outcome.program.keep.modules.is_empty());

    // A module rule promises no sealing either: no class of a kept module is
    // a seal candidate.
    let job = job::load(&tree.job("module", &["models.*"])).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    for name in ["Base", "Sub", "Handler", "Point"] {
        let kept = class_of(&outcome.program, "models.user", name);
        assert!(
            !outcome.program.seal_candidates.contains(&kept),
            "'{name}' of the kept module models.user must not seal"
        );
    }
    let index = module_of(&outcome.program, "models.user");
    assert!(outcome.program.modules[index].kept);
}

#[test]
fn emit_ir_prints_the_program_and_summary() {
    let tree = Tree::new("emit");
    let cache = tree.path("cache");
    tree.write(
        "src/main.py",
        "import math\n\n\ndef main():\n    return math.sqrt(4)\n",
    );
    let job_path = tree.write(
        "job.json",
        r#"{"entries": ["main"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 0, "output": "app.exe", "emit": "ir"}"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_coilc"))
        .arg("build")
        .arg(&job_path)
        .env("COIL_CACHE", &cache)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The program's IR first, the entry's own module among it, and one line
    // per runtime-provided leaf.
    assert!(stdout.contains("coil-ir module 'main'"), "stdout: {stdout}");
    assert!(stdout.contains("native module math\n"), "stdout: {stdout}");
    assert!(
        stdout.ends_with("coilc: 2 modules, 2 functions, 0 cache hits\n"),
        "stdout: {stdout}"
    );
}

//! Type inference over whole programs (spec section 7): a corpus of small
//! programs asserting the per-value types in text form - arithmetic loops,
//! class attribute types, per-tuple clones, the `Top` fallback, guard
//! narrowing, defaults and integer overflow - and the pinned standard
//! library run through inference without panics or fixpoint failures,
//! with the timing printed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use coil_ir::types::{ClassId, FnId};
use coilc::analysis::callgraph;
use coilc::analysis::infer::{self, ATy, Inference};
use coilc::analysis::shapes::Shapes;
use coilc::build;
use coilc::job;
use coilc::world::manifest::{self, Kind, Manifest};
use coilc::world::{ClassRef, FnRef, Program};

/// A scratch tree under the cargo target temp dir, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("coilc-infer-{tag}"));
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
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Compile a single-module program and run the analyses over it: the
/// program with every `Function::types` filled, plus the inference tables.
fn one_module(tag: &str, source: &str) -> (Program, Inference) {
    let tree = Tree::new(tag);
    let cache = tree.path("cache");
    tree.write("src/main.py", source);
    tree.write(
        "job.json",
        r#"{"entries": ["main"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 0, "output": "app.exe"}"#,
    );
    let job = job::load(&tree.path("job.json")).unwrap();
    let mut outcome = build::program(&job, &cache).unwrap();
    let reflective = callgraph::reflective(&outcome.program);
    let shapes = Shapes::analyze(&outcome.program, &reflective);
    let graph = callgraph::build(&outcome.program, &shapes);
    let inference = infer::analyze(&mut outcome.program, &shapes, &graph);
    (outcome.program, inference)
}

/// A function of the `main` module by qualname.
fn fn_of(program: &Program, qualname: &str) -> FnRef {
    let ir = program.modules[0].ir.as_ref().unwrap();
    let f = ir
        .functions
        .iter()
        .position(|function| ir.consts.sym_str(function.qualname) == qualname)
        .unwrap_or_else(|| panic!("'{qualname}' is not in the IR"));
    FnRef {
        module: 0,
        function: FnId(f as u32),
    }
}

#[test]
fn loop_indices_stay_int64_and_sums_widen() {
    let (program, _) = one_module(
        "loop",
        "\
n = 4
s = 0
for i in range(n):
    s = s + i
",
    );
    let text = program.dump_ir();
    assert!(
        text.contains(":Int64 = next "),
        "range elements should type Int64:\n{text}"
    );
    assert!(
        text.contains(":BigInt = binop add, "),
        "int addition should widen to BigInt:\n{text}"
    );
}

#[test]
fn class_attribute_types_flow_to_reads() {
    let (program, inference) = one_module(
        "attrs",
        "\
class Pt:
    def __init__(self):
        self.x = 3
        self.y = 1.5

p = Pt()
a = p.x
b = p.y
",
    );
    let ir = program.modules[0].ir.as_ref().unwrap();
    let c = ir
        .classes
        .iter()
        .position(|desc| ir.consts.sym_str(desc.name) == "Pt")
        .unwrap();
    let cr = ClassRef {
        module: 0,
        class: ClassId(c as u32),
    };
    assert_eq!(
        inference.attrs.get(&(cr, "x".to_owned())),
        Some(&ATy::Int64),
        "self.x = 3 should record the unboxed int:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        inference.attrs.get(&(cr, "y".to_owned())),
        Some(&ATy::Float64),
        "self.y = 1.5 should record the float:\n{}",
        inference.dump(&program)
    );
    let text = program.dump_ir();
    assert!(
        text.contains(":Int64 = getattr "),
        "p.x should read back the int:\n{text}"
    );
    assert!(
        text.contains(":Float64 = getattr "),
        "p.y should read back the float:\n{text}"
    );
}

#[test]
fn polymorphic_function_clones_per_argument_tuple() {
    let (program, inference) = one_module(
        "poly",
        "\
def add(a, b):
    return a + b

x = add(1, 2)
y = add(\"p\", \"q\")
",
    );
    let clones = &inference.table[&fn_of(&program, "add")];
    assert_eq!(
        clones.len(),
        2,
        "one clone per distinct tuple:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        clones[0].args,
        vec![ATy::Int64, ATy::Int64],
        "the int call site specialises on unboxed ints"
    );
    assert_eq!(clones[0].returns, ATy::BigInt, "unboxed ints add to BigInt");
    assert_eq!(
        clones[1].args,
        vec![ATy::Str, ATy::Str],
        "the str call site specialises on strs"
    );
    assert_eq!(clones[1].returns, ATy::Str, "strs concatenate to str");
    assert!(!clones[0].top && !clones[1].top);
}

#[test]
fn ninth_distinct_tuple_routes_to_the_top_clone() {
    let (program, inference) = one_module(
        "top",
        "\
def f(x):
    return x

f(1)
f(\"s\")
f(1.5)
f(True)
f(None)
f(b\"x\")
f([1])
f({})
f({1})
f(2)
",
    );
    let clones = &inference.table[&fn_of(&program, "f")];
    assert_eq!(
        clones.len(),
        9,
        "eight exact clones then one Top version:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        clones.iter().filter(|c| !c.top).count(),
        8,
        "the clone limit holds:\n{}",
        inference.dump(&program)
    );
    let top = clones.last().unwrap();
    assert!(top.top, "the ninth tuple routes to the Top clone");
    assert_eq!(
        top.args,
        vec![ATy::Top],
        "the Top clone takes Top arguments:\n{}",
        inference.dump(&program)
    );
    assert!(
        top.callers.contains(&(fn_of(&program, "<module>"), 0)),
        "the ninth call site uses the Top clone:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        clones[0].args,
        vec![ATy::Int64],
        "the tenth call (f(2)) reuses the Int64 clone"
    );
}

#[test]
fn isinstance_guards_narrow_on_the_true_edge() {
    let (program, inference) = one_module(
        "guard",
        "\
def f(x):
    if isinstance(x, int):
        return x + 1
    return 0

r = 0
for v in (1, \"s\"):
    r = f(v)
",
    );
    let f = fn_of(&program, "f");
    assert!(
        inference
            .guards
            .iter()
            .any(|g| g.at == f && !g.entry && g.ty == ATy::Int64),
        "a branch guard narrowing to the int member should be recorded:\n{}",
        inference.dump(&program)
    );
    let clones = &inference.table[&f];
    assert!(
        clones.len() == 1,
        "one call site through the union:\n{}",
        inference.dump(&program)
    );
    let text = program.dump_ir();
    assert!(
        text.contains(":BigInt = binop add, "),
        "x + 1 under the guard should add unboxed ints:\n{text}"
    );
}

#[test]
fn overflow_types_the_result_past_64_bits() {
    let (program, _) = one_module(
        "ovf",
        "\
a = 2 ** 64
b = 2 ** 10
m = 7 % 3
",
    );
    let text = program.dump_ir();
    assert!(
        text.contains(":BigInt = binop pow, "),
        "2 ** 64 does not fit in 64 bits:\n{text}"
    );
    assert!(
        text.contains(":Int64 = binop pow, "),
        "2 ** 10 fits in 64 bits:\n{text}"
    );
    assert!(
        text.contains(":Int64 = binop mod, "),
        "remainder keeps the unboxed int:\n{text}"
    );
}

#[test]
fn default_values_type_the_missing_arguments() {
    let (program, inference) = one_module(
        "defaults",
        "\
def sq(n=3):
    return n * n

r = sq()
",
    );
    let clones = &inference.table[&fn_of(&program, "sq")];
    assert_eq!(
        clones.len(),
        1,
        "one call site, one clone:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        clones[0].args,
        vec![ATy::Int64],
        "the default literal types the parameter:\n{}",
        inference.dump(&program)
    );
    assert_eq!(
        clones[0].returns,
        ATy::BigInt,
        "unboxed ints multiply to BigInt:\n{}",
        inference.dump(&program)
    );
}

/// The pinned `Lib/` tree: `COIL_CACHE` overrides the default root.
fn stdlib_lib() -> Option<PathBuf> {
    let root = match std::env::var("COIL_CACHE") {
        Ok(root) => PathBuf::from(root),
        Err(_) => PathBuf::from(std::env::var("HOME").ok()?)
            .join(".coil")
            .join("cache"),
    };
    let lib = root.join("stdlib-src").join("3.12.10").join("Lib");
    lib.is_dir().then_some(lib)
}

/// Every `.py` file under `Lib/` as a dotted module name, skipping the
/// modules the manifest provides another way (spec section 10.6).
fn stdlib_entries(lib: &Path, manifest: &Manifest) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(read) => read.filter_map(|e| e.ok().map(|e| e.path())).collect(),
            Err(_) => return,
        };
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == "__pycache__" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                let dotted = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}.{name}")
                };
                walk(&path, &dotted, out);
            } else if name == "__init__.py" {
                out.push(prefix.to_owned());
            } else if name.ends_with(".py") {
                let dotted = if prefix.is_empty() {
                    name.trim_end_matches(".py").to_owned()
                } else {
                    format!("{prefix}.{}", name.trim_end_matches(".py"))
                };
                out.push(dotted);
            }
        }
    }
    let mut all = Vec::new();
    walk(lib, "", &mut all);
    all.into_iter()
        .filter(|name| match manifest.get(name) {
            Some(entry) => entry.kind == Kind::Compiled,
            None => true,
        })
        .collect()
}

#[test]
fn stdlib_modules_infer_without_panicking() {
    let Some(lib) = stdlib_lib() else {
        eprintln!("skipping: the pinned Lib/ tree is not under the coil cache");
        return;
    };
    let (manifest_path, _shims) = manifest::discover().expect("the checked-in manifest");
    let manifest = Manifest::load(&manifest_path).expect("the manifest parses");
    let entries = stdlib_entries(&lib, &manifest);
    assert!(
        entries.len() > 1000,
        "the pinned Lib tree should yield its modules ({} found)",
        entries.len()
    );
    let tree = Tree::new("stdlib");
    let cache = tree.path("cache");
    let start = Instant::now();
    let mut built = 0usize;
    let mut skipped = 0usize;
    for name in &entries {
        tree.write(
            "job.json",
            &format!(
                r#"{{"entries": ["{name}"], "source_roots": [], "stdlib_source_root": "{}",
                    "abi": "cp312", "optimize": 0, "output": "app.exe"}}"#,
                lib.display()
            ),
        );
        let job = job::load(&tree.path("job.json")).unwrap();
        // A module the front end rejects (Lib/test's bad-syntax corpus, a
        // static import of an unsupported module) is that module's answer,
        // not an inference failure.
        let Ok(mut outcome) = build::program(&job, &cache) else {
            skipped += 1;
            continue;
        };
        let reflective = callgraph::reflective(&outcome.program);
        let shapes = Shapes::analyze(&outcome.program, &reflective);
        let graph = callgraph::build(&outcome.program, &shapes);
        infer::analyze(&mut outcome.program, &shapes, &graph);
        built += 1;
    }
    let elapsed = start.elapsed();
    println!(
        "coilc: inferred types over {built}/{} stdlib modules in {elapsed:.1?} ({skipped} rejected by the front end)",
        entries.len()
    );
    // What the front end builds today is the pure-Python modules whose
    // imports resolve: the shim files the manifest points at are not
    // checked in yet and unix built-ins (`posix` and friends) are outside
    // the manifest, so the rest are rejected before inference runs.
    assert!(
        built >= 100,
        "the front end should still build a real corpus ({built} of {})",
        entries.len()
    );
}

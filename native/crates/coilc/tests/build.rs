//! `coilc build` end to end (spec sections 2, 11.1 and 13.2 step 4): the
//! per-module front end over a small tree with a stdlib import, the IR cache
//! written, then hit on a second run, then missed only where a source
//! changed. The binary is driven with `COIL_CACHE` pointed inside the cargo
//! target temp dir, never at the shared cache.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use coilc::build::{self, Summary};
use coilc::job;

const APP: &str = "import demo_math\nimport helper\n\ndef main():\n    return demo_math.add(1, 2) + helper.twice(3)\n";
const HELPER: &str = "def twice(x):\n    return x * 2\n";
const DEMO_MATH: &str = "def add(a, b):\n    return a + b\n";

/// The job schema as the Python driver will write it (spec section 13.2
/// step 4); relative paths resolve against the job file's directory.
const JOB: &str = r#"{
  "entries": ["app"],
  "source_roots": ["src"],
  "stdlib_source_root": "stdlib",
  "abi": "cp312",
  "optimize": 0,
  "output": "dist/app.exe"
}
"#;

/// A scratch tree under the cargo target temp dir, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("coilc-build-{tag}"));
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

/// One app plus one helper from the source roots, one module from the stdlib
/// root: 3 modules, 2 functions each (the module body and one def).
fn write_tree(tree: &Tree) -> PathBuf {
    tree.write("src/app.py", APP);
    tree.write("src/helper.py", HELPER);
    tree.write("stdlib/demo_math.py", DEMO_MATH);
    tree.write("job.json", JOB)
}

#[test]
fn caches_per_module_ir_and_misses_changed_sources() {
    let tree = Tree::new("lib");
    let cache = tree.path("cache");
    let job_path = write_tree(&tree);
    let job = job::load(&job_path).unwrap();

    let first = build::run(&job, &cache).unwrap();
    assert_eq!(
        first,
        Summary {
            modules: 3,
            functions: 6,
            cache_hits: 0
        }
    );
    let dir = cache.join("native/ir/cp312/0");
    for name in ["app", "helper", "demo_math"] {
        assert!(
            dir.join(format!("{name}.ir")).is_file(),
            "{name}.ir missing"
        );
        assert!(
            dir.join(format!("{name}.meta")).is_file(),
            "{name}.meta missing"
        );
    }

    // The same job again: every module loads from the cache.
    let second = build::run(&job, &cache).unwrap();
    assert_eq!(
        second,
        Summary {
            modules: 3,
            functions: 6,
            cache_hits: 3
        }
    );

    // A changed source misses only its own module; importers still hit
    // because the cache is keyed per module.
    tree.write(
        "src/helper.py",
        "def twice(x):\n    return x * 2\n\ndef once(x):\n    return x\n",
    );
    let third = build::run(&job, &cache).unwrap();
    assert_eq!(
        third,
        Summary {
            modules: 3,
            functions: 7,
            cache_hits: 2
        }
    );
}

#[test]
fn binary_builds_and_prints_the_summary() {
    let tree = Tree::new("binary");
    let cache = tree.path("cache");
    let job_path = write_tree(&tree);

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_coilc"))
            .arg("build")
            .arg(&job_path)
            .env("COIL_CACHE", &cache)
            .output()
            .unwrap()
    };

    let first = run();
    assert!(
        first.status.success(),
        "first run failed: {} {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        "coilc: 3 modules, 6 functions, 0 cache hits\n"
    );

    let second = run();
    assert!(second.status.success());
    assert_eq!(
        String::from_utf8_lossy(&second.stdout),
        "coilc: 3 modules, 6 functions, 3 cache hits\n"
    );
}

#[test]
fn rejects_bad_jobs_naming_the_problem() {
    let tree = Tree::new("reject");
    let cache = tree.path("cache");
    let run = |job: &Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_coilc"))
            .arg("build")
            .arg(job)
            .env("COIL_CACHE", &cache)
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    // An unknown field is an error naming the field.
    let unknown_field = tree.write(
        "unknown.json",
        r#"{"entries": ["app"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 0, "output": "app.exe", "bogus": 1}"#,
    );
    let stderr = run(&unknown_field);
    assert!(stderr.contains("unknown field `bogus`"), "stderr: {stderr}");

    // An optimize level outside 0..=2 is an error naming the field.
    let bad_optimize = tree.write(
        "optimize.json",
        r#"{"entries": ["app"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 3, "output": "app.exe"}"#,
    );
    let stderr = run(&bad_optimize);
    assert!(
        stderr.contains("optimize must be 0, 1 or 2, got 3"),
        "stderr: {stderr}"
    );

    // An unresolvable entry fails the build naming the module.
    let missing_entry = tree.write(
        "entry.json",
        r#"{"entries": ["nope"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 0, "output": "app.exe"}"#,
    );
    let stderr = run(&missing_entry);
    assert!(
        stderr.contains("cannot resolve module 'nope' as a job entry"),
        "stderr: {stderr}"
    );
}

#[test]
fn walks_imports_across_roots_and_packages() {
    let tree = Tree::new("closure");
    let cache = tree.path("cache");
    tree.write(
        "src/app.py",
        "from pkg.outer import value\n\ndef main():\n    return value\n",
    );
    tree.write("src/pkg/__init__.py", "INIT = 1\n");
    tree.write(
        "src/pkg/outer.py",
        "from pkg.inner import secret\nfrom . import INIT\n\nvalue = secret + INIT\n",
    );
    tree.write("src/pkg/inner.py", "secret = 7\n");

    let job_path = tree.write(
        "job.json",
        r#"{"entries": ["app"], "source_roots": ["src"], "abi": "cp312",
            "optimize": 1, "output": "app.exe"}"#,
    );
    let job = job::load(&job_path).unwrap();
    let summary = build::run(&job, &cache).unwrap();
    // app, pkg, pkg.outer, pkg.inner; optimize 1 gets its own cache dir.
    assert_eq!(
        summary,
        Summary {
            modules: 4,
            functions: 5,
            cache_hits: 0
        }
    );
    let dir = cache.join("native/ir/cp312/1");
    for name in ["app", "pkg", "pkg.outer", "pkg.inner"] {
        assert!(
            dir.join(format!("{name}.ir")).is_file(),
            "{name}.ir missing"
        );
    }

    // A relative import reaching past the top-level package is an error
    // naming the importer.
    tree.write("src/pkg/outer.py", "from .. import gone\n");
    let error = build::run(&job, &cache).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("relative import level 2 in module 'pkg.outer'"),
        "error: {error}"
    );
}

//! The analyses over whole programs (spec sections 7 and 8.3): call edges
//! direct where the callee is known and dynamic carrying the name otherwise,
//! the components of the direct subgraph, reflective sites unsealing a
//! class, and the slot shapes of compiled classes.

use std::fs;
use std::path::{Path, PathBuf};

use coil_ir::types::{ClassId, FnId};
use coilc::analysis::callgraph::{self, Edge};
use coilc::analysis::shapes::{self, Shapes};
use coilc::build;
use coilc::job;
use coilc::world::{ClassRef, FnRef};

/// A scratch tree under the cargo target temp dir, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("coilc-analysis-{tag}"));
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

    fn job(&self) -> PathBuf {
        self.write(
            "job.json",
            r#"{"entries": ["main"], "source_roots": ["src"], "abi": "cp312",
                "optimize": 0, "output": "app.exe"}"#,
        )
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Compile a single-module program and hand back the analyses over it.
fn one_module(
    tag: &str,
    source: &str,
    keep: &[&str],
) -> (coilc::world::Program, Shapes, callgraph::CallGraph) {
    let tree = Tree::new(tag);
    let cache = tree.path("cache");
    tree.write("src/main.py", source);
    let mut job_json = String::from(
        r#"{"entries": ["main"], "source_roots": ["src"], "abi": "cp312", "optimize": 0, "output": "app.exe""#,
    );
    if !keep.is_empty() {
        let rules: Vec<String> = keep.iter().map(|r| format!("\"{r}\"")).collect();
        job_json.push_str(&format!(", \"keep\": [{}]", rules.join(", ")));
    }
    job_json.push('}');
    tree.write("job.json", &job_json);
    let job = job::load(&tree.path("job.json")).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    let program = outcome.program;
    let reflective = callgraph::reflective(&program);
    let shapes = Shapes::analyze(&program, &reflective);
    let graph = callgraph::build(&program, &shapes);
    (program, shapes, graph)
}

/// A function of the program by module and qualname.
fn fn_of(program: &coilc::world::Program, module: &str, qualname: &str) -> FnRef {
    let index = program
        .modules
        .iter()
        .position(|m| m.name == module)
        .unwrap_or_else(|| panic!("module '{module}' is not in the program"));
    let ir = program.modules[index].ir.as_ref().unwrap();
    let f = ir
        .functions
        .iter()
        .position(|function| ir.consts.sym_str(function.qualname) == qualname)
        .unwrap_or_else(|| panic!("'{module}.{qualname}' is not in the IR"));
    FnRef {
        module: index as u32,
        function: FnId(f as u32),
    }
}

/// A class of the program by module and defining name.
fn class_of(program: &coilc::world::Program, module: &str, class: &str) -> ClassRef {
    let index = program
        .modules
        .iter()
        .position(|m| m.name == module)
        .unwrap_or_else(|| panic!("module '{module}' is not in the program"));
    let ir = program.modules[index].ir.as_ref().unwrap();
    let c = ir
        .classes
        .iter()
        .position(|desc| ir.consts.sym_str(desc.name) == class)
        .unwrap_or_else(|| panic!("class '{class}' is not in '{module}'"));
    ClassRef {
        module: index as u32,
        class: ClassId(c as u32),
    }
}

#[test]
fn direct_edges_dynamic_edges_and_components() {
    let source = "\
def even(n):
    if n == 0:
        return True
    return odd(n - 1)

def odd(n):
    if n == 0:
        return False
    return even(n - 1)

def main():
    even(4)
    fact(5)
    return abs(-3)

def fact(n):
    return 1 if n <= 1 else n * fact(n - 1)
";
    let (program, _shapes, graph) = one_module("edges", source, &[]);
    // The known callees are direct; `abs` only has a name, and the module
    // body makes no calls. The components run callees-first: the mutual
    // recursion is one component, `fact`'s self-recursion its own.
    assert_eq!(
        graph.dump(&program),
        "\
main.<module>:
main.even: direct main.odd
main.odd: direct main.even
main.main: direct main.even, direct main.fact, dynamic 'abs'
main.fact: direct main.fact
scc 0: main.<module>
scc 1: main.even, main.odd
scc 2: main.fact
scc 3: main.main
"
    );
    let even = fn_of(&program, "main", "even");
    assert_eq!(
        graph.callees(even),
        [Edge::Direct(fn_of(&program, "main", "odd"))]
    );
    assert_eq!(
        graph.callers(even),
        [
            fn_of(&program, "main", "odd"),
            fn_of(&program, "main", "main")
        ]
    );
}

#[test]
fn sealed_methods_resolve_and_reflection_unseals() {
    let source = "\
class Sealed:
    def __init__(self):
        self.a = 1

    def get(self):
        return self.a

class Open:
    def __init__(self):
        self.b = 2

    def get(self):
        return self.b

def make_and_call():
    s = Sealed()
    return s.get()

def make_and_call_open():
    o = Open()
    return o.get()

def main():
    x = make_and_call()
    y = make_and_call_open()
    o = Open()
    g = getattr(o, \"ge\" + \"t\")
    return x + y + g(o)
";
    let (program, shapes, graph) = one_module("sealed", source, &[]);
    // The constructor call's result is a known instance, so `s.get()` on the
    // sealed class resolves and the constructor runs the class's own
    // `__init__`; `Open` flows to a `getattr` with a computed name, so it is
    // not sealed and its sites stay dynamic.
    let open = class_of(&program, "main", "Open");
    assert_eq!(
        callgraph::reflective(&program),
        [open].into_iter().collect(),
        "Open reached a reflective site"
    );
    assert_eq!(
        shapes.dump(&program),
        "main.Sealed slots=a sealed\nmain.Open slots=b\n"
    );
    let make = fn_of(&program, "main", "make_and_call");
    assert_eq!(
        graph.callees(make),
        [
            Edge::Direct(fn_of(&program, "main", "Sealed.__init__")),
            Edge::Direct(fn_of(&program, "main", "Sealed.get")),
        ]
    );
    let make_open = fn_of(&program, "main", "make_and_call_open");
    assert_eq!(
        graph.callees(make_open),
        [
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Dynamic(Some("get".to_owned())),
        ]
    );
    let get = fn_of(&program, "main", "Sealed.get");
    assert_eq!(graph.callers(get), [make]);
    // `main` itself: two known callees, an unsealed constructor, the
    // reflective `getattr` by name, and a call through its result, which
    // has no name.
    assert_eq!(
        graph.callees(fn_of(&program, "main", "main")),
        [
            Edge::Direct(fn_of(&program, "main", "make_and_call")),
            Edge::Direct(fn_of(&program, "main", "make_and_call_open")),
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Dynamic(Some("getattr".to_owned())),
            Edge::Dynamic(None),
        ]
    );
}

#[test]
fn slots_follow_first_store_order_with_slots_entries_last() {
    let source = "\
class Order:
    __slots__ = (\"z\",)

    def second(self):
        self.b = 2

    def __init__(self):
        self.a = 1

def main():
    o = Order()
    return o.second()
";
    let (program, shapes, graph) = one_module("order", source, &[]);
    // `__init__` runs first, the other methods follow in definition order,
    // and the `__slots__` entries sit after the inferred attributes.
    assert_eq!(shapes.dump(&program), "main.Order slots=a, b, z sealed\n");
    let order = class_of(&program, "main", "Order");
    let shape = shapes.get(order).unwrap();
    assert_eq!(shapes.slot(shape, "a"), Some(0));
    assert_eq!(shapes.slot(shape, "b"), Some(1));
    assert_eq!(shapes.slot(shape, "z"), Some(2));
    assert_eq!(shapes.slot(shape, "missing"), None);
    // A sealed class's constructor and method calls both resolve.
    assert_eq!(
        graph.callees(fn_of(&program, "main", "main")),
        [
            Edge::Direct(fn_of(&program, "main", "Order.__init__")),
            Edge::Direct(fn_of(&program, "main", "Order.second")),
        ]
    );
}

#[test]
fn a_class_past_the_slot_limit_gets_a_dict() {
    let mut source = String::from("class Wide:\n    def __init__(self):\n");
    for i in 0..33 {
        source.push_str(&format!("        self.a{i} = {i}\n"));
    }
    source.push_str("\nclass Tidy:\n    def __init__(self):\n");
    for i in 0..32 {
        source.push_str(&format!("        self.b{i} = {i}\n"));
    }
    source.push_str("\n\ndef main():\n    return 1\n");
    let (program, shapes, _graph) = one_module("limit", &source, &[]);
    let wide = shapes.get(class_of(&program, "main", "Wide")).unwrap();
    let tidy = shapes.get(class_of(&program, "main", "Tidy")).unwrap();
    // 33 stored attributes materialise a dict; 32 still fit (spec 8.3).
    assert_eq!(shapes.shape(wide).slots.len(), shapes::SLOT_LIMIT + 1);
    assert!(shapes.shape(wide).dict, "33 slots need a dict");
    assert_eq!(shapes.shape(tidy).slots.len(), shapes::SLOT_LIMIT);
    assert!(!shapes.shape(tidy).dict, "32 slots fit without a dict");
}

#[test]
fn method_calls_resolve_along_the_mro() {
    let source = "\
class B:
    def m(self):
        return 1

class C:
    def m(self):
        return 2

class D(B, C):
    def __init__(self):
        self.v = 0

    def m(self):
        return 3

class E(B, C):
    pass

class Base:
    def n(self):
        return 4

class Sub(Base):
    def n(self):
        return 5

def main():
    d = D()
    b = B()
    x = d.m() + b.m()
    e = E()
    y = e.m()
    s = Sub()
    base = Base()
    return x + y + s.n() + base.n()
";
    let (program, _shapes, graph) = one_module("mro", source, &[]);
    // `D` defines both methods, so both resolve; `E` defines neither and its
    // `m` resolves to `B.m`, the first along the MRO. `B` and `Base` are
    // overridden by reachable subclasses, so calls on them stay dynamic even
    // when the receiver is exactly that class.
    assert_eq!(
        graph.callees(fn_of(&program, "main", "main")),
        [
            Edge::Direct(fn_of(&program, "main", "D.__init__")),
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Direct(fn_of(&program, "main", "D.m")),
            Edge::Dynamic(Some("m".to_owned())),
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Direct(fn_of(&program, "main", "B.m")),
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Direct(fn_of(&program, "main", "Sub.n")),
            Edge::Dynamic(Some("n".to_owned())),
        ]
    );
}

#[test]
fn imports_add_body_edges_and_namespaces_reexport() {
    let tree = Tree::new("imports");
    let cache = tree.path("cache");
    tree.write(
        "src/main.py",
        "import pkg\n\n\ndef main():\n    x = pkg.mod.util()\n    from pkg.mod import util as u\n    return x + u()\n",
    );
    tree.write("src/pkg/__init__.py", "from pkg.mod import util\n");
    tree.write("src/pkg/mod.py", "def util():\n    return 1\n");
    let job = job::load(&tree.job()).unwrap();
    let outcome = build::program(&job, &cache).unwrap();
    let program = outcome.program;
    let shapes = Shapes::analyze(&program, &callgraph::reflective(&program));
    let graph = callgraph::build(&program, &shapes);
    // Each import runs the package chain's bodies and binds the target, so
    // `pkg.mod.util` resolves through the module namespace and the
    // from-import's re-export resolves to the defining function. The chain
    // of the import inside `pkg` itself skips `pkg`: a module's own body
    // never re-runs.
    assert_eq!(
        graph.dump(&program),
        "\
main.<module>: direct pkg.<module>
main.main: direct pkg.mod.util, direct pkg.mod.<module>, direct pkg.<module>, direct pkg.mod.util
pkg.<module>: direct pkg.mod.<module>
pkg.mod.<module>:
pkg.mod.util:
scc 0: pkg.mod.<module>
scc 1: pkg.<module>
scc 2: main.<module>
scc 3: pkg.mod.util
scc 4: main.main
"
    );
}

#[test]
fn a_kept_class_never_seals() {
    let source = "\
class Point:
    def __init__(self, x):
        self.x = x

    def get(self):
        return self.x

def main():
    p = Point(1)
    return p.get()
";
    let (program, shapes, graph) = one_module("keep", source, &["main:Point"]);
    let point = class_of(&program, "main", "Point");
    assert!(
        !shapes.is_sealed(point),
        "a kept class keeps every member, so it never seals"
    );
    assert_eq!(shapes.dump(&program), "main.Point slots=x\n");
    assert_eq!(
        graph.callees(fn_of(&program, "main", "main")),
        [
            Edge::Dynamic(Some("__init__".to_owned())),
            Edge::Dynamic(Some("get".to_owned())),
        ]
    );
    // Without the keep rule the same program seals and both sites resolve.
    let (program, shapes, graph) = one_module("noseal", source, &[]);
    assert!(shapes.is_sealed(class_of(&program, "main", "Point")));
    assert_eq!(
        graph.callees(fn_of(&program, "main", "main")),
        [
            Edge::Direct(fn_of(&program, "main", "Point.__init__")),
            Edge::Direct(fn_of(&program, "main", "Point.get")),
        ]
    );
}

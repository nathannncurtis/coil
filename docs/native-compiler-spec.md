# Native compilation (`--compile`): design

Status: design, 2026-10-21. Nothing in this document is built.

Coil today compiles `.py` to `.pyc`, bundles the Windows embeddable CPython runtime, and wraps both in a native launcher. `--compile` adds a second path: Python source goes through Coil's own compiler to x86-64 machine code, linked into a PE executable that runs on Coil's own runtime. No CPython interpreter, no bytecode, no C compiler on the user's machine.

The bytecode path stays. `--compile` is a build option, like `--secure`, and every existing option (modes, profiles, per-entry stamping, `.coilignore`, decompile) keeps its meaning under it.

## 1. Decisions

These are fixed. The rest of the document is designed around them.

1. `--compile` compiles Python to native machine code.
2. The compiler does not translate to C, does not invoke a C compiler, and the output does not run on CPython's interpreter. Go, Rust, SBCL, Chez, Julia and Crystal compile natively without going through C; so does this.
3. C extensions (numpy, pywin32, PyQt, Pillow, cryptography and the rest) are supported through a layer that implements the CPython C API, speaking to unmodified `.pyd` files.
4. Reference counting is not the memory model for compiled code. Compiled code never touches `ob_refcnt`.
5. The garbage collector for compiled-code objects is tracing and non-moving.
6. The code generator is Coil's own: no Cranelift, no LLVM. x86-64 first, ARM64 next, RISC-V on the roadmap.
7. The runtime (allocator, GC, object model, C API layer, native modules) and the compiler are written in Rust. Moving runtime pieces into Coil-compiled Python is open for later; the compiler is not self-hosted.
8. There are no proof-of-concept or demo milestones. The feature proves itself by being finished.
9. Impersonated ABIs: cp312 first, then cp313. Never 3.9 to 3.11.
10. `sys.implementation.name` and `platform.python_implementation()` report `"coil"`.
11. The wheel is a platform wheel (`win_amd64`) carrying `python312.dll`, `python3.dll`, `coil_dyn.dll` and `coilc.exe`.
12. `exec`, `eval` and `compile()` on non-constant input are supported. The parser and IR interpreter ship with an application only when it reaches them.
13. Memory owned by C extensions is released when C drops its last reference, with CPython's timing. Compiled code still never touches `ob_refcnt`; the collector for compiled-code objects is still tracing. Section 9 is designed to that requirement.
14. The early C API check is CPython's `_testcapi` driven from Rust tests.
15. Threads: v1 keeps the GIL everywhere with cp312 semantics. The roadmap is a build-time GIL choice on the free-threaded ABI (cp313t, cp314t): since the build sees every extension the application loads, `coil build` ships a GIL-free executable when every extension declares `Py_MOD_GIL_NOT_USED`, and otherwise ships with the GIL and names the extension that forced it in the compile report. So that this needs no runtime rewrite, the runtime's dict, list, set and allocator internals are thread-safe from the start, with PEP 703-style per-object locks that are cheap when uncontended.
16. The v1 extension compatibility set is in section 15.3.
17. Coil is licensed Apache-2.0 WITH LLVM-exception. The runtime and the generated code are covered by the LLVM exception, so a compiled application may redistribute the embedded runtime without Coil's attribution and notice conditions.

Everything marked **Recommendation** is a design choice made on the merits and open to change. Everything else follows from the decisions above or from how Coil already works. No open questions remain (section 19).

## 2. Architecture

```
coil build --compile                               (Python: src/coil/)
  │
  ├─ resolver / scanner / install_dependencies      existing; unchanged
  ├─ prepare_runtime                                existing; now used only for the
  │                                                  embeddable's .pyd and vendor DLLs
  ├─ coil.native.driver → coilc.exe <job.json>      (Rust: native/crates/coilc)
  │    ├─ front        source → AST → Coil IR       coil-front, per module, cached
  │    ├─ world        reachability, keep rules, compile report
  │    ├─ analysis     type inference, escape analysis, call graph
  │    ├─ opt          inlining, specialization, unboxing, DCE, GVN, bounds checks
  │    ├─ backend      generic lowering → x86_64 isel → regalloc → encode
  │    └─ link         PE32+: .text .rdata .data .pdata .xdata .reloc .rsrc .idata
  │                     → <Entry>.exe
  └─ packager (existing, extended)
       stage_dir/
         <Entry>.exe            compiled application: app + reachable stdlib + boot
         python312.dll          Coil runtime (Rust), named for the impersonated ABI
         python3.dll            stable-ABI forwarder to python312.dll
         coil_dyn.dll           parser + IR interpreter; present only if exec/eval/compile
                                or ast.parse are reachable
         vcruntime140.dll …     from the embeddable zip (extensions need them)
         _internal/
           *.pyd, *.dll         CPython's extension modules, OpenSSL, sqlite, libffi
           lib/                 dependencies: only their .pyd/.dll/data files
           app/                 project assets, .coil_meta.json, .coil_source.zip
```

Layers, from the bottom:

| Layer | Lives in | Responsibility | Interface |
| --- | --- | --- | --- |
| Runtime | `native/crates/coil-rt` → `python312.dll` | heap, GC, object model, type objects, builtins, native stdlib modules, C API exports, threads, import of `.pyd` | exported C ABI: the CPython API (for extensions) and `coil_rt_*` (for compiled code), described by `native/abi.toml` |
| Dynamic code | `native/crates/coil-dyn` → `coil_dyn.dll` | parser, symtable, lowering and IR interpreter for runtime `exec`/`eval`/`compile`/`ast.parse` | `coil_dyn_compile(src, filename, mode, flags) -> code object`, loaded by the runtime on first use |
| Compiled code | `<Entry>.exe` | application, reachable stdlib, module table, type descriptors, stack maps, line tables | calls `coil_rt_*`; registers its tables through `coil_rt_init(&moduledata)` |
| Compiler | `native/crates/coilc` → `coilc.exe` | front end, IR, analysis, optimisation, codegen, linking, inspect | `coilc build <job.json>`, `coilc inspect <job.json> --json` |
| Coil CLI and packager | `src/coil/` (existing) | flags, `coil.toml`, staging, portable payload, cache | unchanged entry points; one new `compile` argument; `src/coil/native/driver.py` runs `coilc.exe` |

`native/abi.toml` is the one contract between runtime and compiled code: every `coil_rt_*` entry point with its signature, every runtime struct offset compiled code touches, the type-descriptor, stack-map and line-table formats. `coil-rt`, `coilc` and `coil-dyn` include it through a build script; `coil_rt_init` compares the SHA-256 embedded in the exe with its own and refuses a mismatch.

**Recommendation** (Go-style toolchain): own front end, own IR, own backend, own linker, prebuilt runtime and compiler in the wheel. Adopted; decision 11 fixes the wheel shape.

## 3. Repository layout

```
src/coil/
  cli.py, builder.py, packager.py, config.py …    existing; small edits (section 13)
  native/
    driver.py            CompileJob → job.json → coilc.exe; cache paths; report parsing
    config.py            [compile] table
    report.py            compile report for coil inspect / coil doctor
  _runtime/              wheel data, built in CI by tools/build_native.py
    python312.dll python3.dll coil_dyn.dll coilc.exe abi.sha256 stdlib.toml shims/
native/                                            Cargo workspace
  Cargo.toml  rust-toolchain.toml  abi.toml
  manifest/stdlib.toml   per stdlib module: compiled | shim | native | unsupported
  shims/*.py             Python replacements for CPython built-in modules (section 10.6)
  crates/
    coil-front/          src/tokenizer.rs parser.rs ast.rs symtable.rs lower.rs
    coil-ir/             src/ir.rs types.rs builder.rs verify.rs print.rs serialize.rs interp.rs
    coilc/               src/main.rs job.rs
                         src/world/{reach.rs keep.rs report.rs}
                         src/analysis/{callgraph.rs infer.rs escape.rs shapes.rs signatures.rs}
                         src/opt/{inline.rs specialize.rs devirt.rs unbox.rs gvn.rs licm.rs bounds.rs dce.rs}
                         src/backend/{lower.rs regalloc.rs stackmap.rs linetable.rs}
                         src/backend/x86_64/{rules.rs isel.rs regs.rs abi.rs encode.rs unwind.rs}
                         src/backend/aarch64/              later, same five files
                         src/link/{objfile.rs pe.rs rsrc.rs pdata.rs reloc.rs imports.rs}
    coil-rt/             cdylib → python312.dll
                         src/alloc/   size classes, pages, large objects, side bitmaps, thread caches
                         src/gc/      mark, pins, zct, sweep, finalizers, weakrefs, pacing
                         src/object/  header, type objects, shapes, descriptors, locks
                         src/types/   long.rs unicode.rs bytes.rs tuple.rs list.rs dict.rs set.rs
                                      float.rs complex.rs func.rs code.rs frame.rs gen.rs exc.rs …
                         src/capi/    one file per CPython header
                         src/modules/ builtins.rs sys.rs nt.rs _thread.rs _sre.rs math.rs …
                         src/{import.rs tstate.rs gil.rs eval_breaker.rs}
                         src/arch/{x86_64.rs aarch64.rs}
    coil-rt-abi/         layouts per impersonated ABI: cp312.rs, cp313.rs, cp313t.rs (roadmap)
    coil-capi-sys/       C API declarations generated from the pinned headers
    coil-dyn/            cdylib → coil_dyn.dll (coil-front + coil-ir::interp + _ast bridge)
tools/
  build_native.py        cargo build --release for the workspace; copies outputs to src/coil/_runtime/
  gen_abi.py             abi.toml → Rust consts and the Python-side constants
  gen_capi.py            export list from the pinned headers; diff against python312.dll
  layout_oracle.c/.py    dev-time offsetof/sizeof check for coil-rt-abi
  capi_usage.py          import tables of wheels → symbol coverage (section 10.1)
tests/native/            Python-side tests that build and run real executables
native/crates/*/tests/   Rust unit and conformance tests
```

## 4. Front end (`coil-front`)

Input: the project's `.py` files (the same set `_build_exclude_matcher` selects today), the `.py` files of installed dependencies, the pinned CPython `Lib/` sources (section 11.1), and the shims.

The parser is a hand-written recursive-descent parser in Rust that implements the pinned 3.12.10 `Grammar/python.gram` and produces the same AST shapes as CPython's `ast` module (PEP 701 f-strings, PEP 695 type parameters, `match`, `except*`). ruff's `ruff_python_parser` (MIT) is the candidate base to evaluate before writing one from scratch; either way the acceptance test is the same: parse every file in the pinned `Lib/` and every test input in `Lib/test` with both `coil-front` and CPython 3.12's `ast.parse`, and compare `ast.dump` output. There is one parser: `coilc` links it for builds, `coil_dyn.dll` links it for runtime `exec`/`eval`/`compile`/`ast.parse`, and the runtime's `_ast` module builds node objects from its output.

Using Coil's own parser rather than the host's `ast` also removes host/target drift: a 3.9 host building for 3.12 would reject `match`; a 3.13 host would accept syntax 3.12 does not. The audits fought this class of drift for `.pyc` magic numbers.

`symtable.rs` reproduces CPython's `symtable.c`: local/global/nonlocal/cell/free, class-scope rules, comprehension scopes, `__class__` cells, PEP 695 annotation scopes. `lower.rs` turns the AST into Coil IR one function at a time, with explicit control flow for `with`, `try/finally`, `except*`, `match`, comprehensions and generator bodies (section 12.5).

Optimisation level follows `--optimize` exactly as today: 0 keeps `assert`, 1 drops it, 2 also drops docstrings. `__debug__` is a constant.

## 5. Coil IR (`coil-ir`)

One IR for the whole middle end: SSA, typed, with explicit exception edges. Serialised per module into the build cache (`serialize.rs`, versioned binary format keyed by the IR schema hash).

```rust
pub struct Module { pub name: Sym, pub file: PathBuf, pub functions: Vec<Function>,
                    pub classes: Vec<ClassDesc>, pub consts: ConstPool, pub body: FnId }
pub struct Function { pub qualname: Sym, pub params: Params, pub blocks: Vec<Block>, pub entry: BlockId,
                      pub locals: Vec<LocalMeta>, pub flags: FnFlags,   // GENERATOR | COROUTINE | ASYNC_GEN | HAS_CELLS …
                      pub types: Vec<Type>,                            // indexed by Value
                      pub lines: Vec<(InstrId, u32, u32)> }
pub struct Block { pub phis: Vec<Phi>, pub instrs: Vec<Instr>, pub term: Terminator,
                   pub handler: Option<BlockId> }                      // landing block, None = propagate
pub struct Value(pub u32);
pub enum Type { Top, Union(Box<[Type]>), Inst { cls: ClassId, exact: bool },
                Int64, Float64, Bool, None, BigInt, Str, Bytes, Tuple(Box<[Type]>), List, Dict, Set,
                Func(SigId), Module(ModId), Class(ClassId), Ext(TypeRef), Bottom }
```

`Type::Ext` is "instance of an extension-defined type" (numpy's `ndarray`, Qt's `QWidget`); section 9 gives it a different memory regime, so inference tracks it as its own lattice point.

Instruction families (`ir.rs` defines each with operand and result types):

- constants and moves: `Const`, `Phi`, `Copy`
- unboxed arithmetic with overflow results: `IAddOvf -> (Int64, Bool)`, `ISub/IMul/…`, `FAdd/…`, `ICmp/FCmp`, shifts and bit ops
- boxing: `Box(Int64|Float64|Bool)`, `Unbox(ty)` (guarded)
- dynamic operations: `BinOp(op)`, `UnaryOp`, `Compare`, `GetAttr(name)`, `SetAttr`, `DelAttr`, `GetItem`, `SetItem`, `DelItem`, `GetIter`, `Next`, `Call(callee, args, kwnames)`, `CallMethod(name, …)`, `Truth`, `Len`, `Contains`, `Format`, `BuildList/Tuple/Dict/Set/Str/Slice`, `Unpack(n, star)`, `Import(name, fromlist, level)`, `MakeFunction`, `MakeClass`, `MakeCell`, `LoadCell/StoreCell`, `LoadGlobal/StoreGlobal`, `LoadName/StoreName`
- static operations the optimiser introduces: `CallDirect(fn, args)`, `LoadField(shape, slot)`, `StoreField { barrier: BarrierKind }`, `AllocInst(shape)`, `AllocTuple(n)`, `TypeGuard(v, ty) -> ok, fail`, `ShapeGuard(v, shape)`, `BoundsCheck`, `ListGetUnchecked`, `TupleGetUnchecked`, `StackAlloc(shape)`
- control: `Jump`, `Branch`, `Switch`, `Return`, `Raise(exc, cause)`, `Reraise`, `Yield(state, value)`, `YieldFrom`, `Await`, `Safepoint`, `Unreachable`
- exceptions: every may-raise instruction is marked; lowering materialises "test the error flag after the call; branch to `handler`" (section 12.4). `CatchBegin(types) -> matched, exc`, `CatchEnd`, `PushExcInfo/PopExcInfo`, `FinallyEnter/FinallyExit(reason)`.

`BarrierKind` is `None` (the stored value's static type is a tracing-regime type), `Ext` (statically an extension-type instance: unconditional count update) or `Dynamic` (unknown: test the regime bit, then update). Section 9.3 explains what the barrier maintains.

`verify.rs` checks: SSA dominance, every may-raise instruction has a resolved handler, boxed/unboxed consistency, no `Top` in an unboxed operand, one terminator per block, `Yield` only in generator functions, barrier kinds consistent with operand types.

SSA is built during lowering with Braun, Buchwald, Hack, Leißa, Mallon and Zwinkau, "Simple and Efficient Construction of Static Single Assignment Form" (CC 2013): no dominance frontiers, works on the CFG as emitted.

`interp.rs` executes Coil IR over boxed values. It is the reference executor for IR tests and the engine behind runtime `exec`/`eval` (section 6). It implements every instruction; the optimiser's static instructions are never fed to it because dynamically compiled code stops after `lower` and `verify`.

Passes in `coilc` run in this order over the whole program after `world` has fixed the module set: `infer` → `specialize` → `inline` → `devirt` → `escape` → `unbox` → `gvn` → `licm` → `bounds` → `dce` → `infer` → `dce`. Each pass is `fn(&mut Program)` with `--emit ir` dumps after each, so a regression pins to one pass.

## 6. Closed world, keep rules, and dynamic code

**Recommendation**: closed-world compilation. Adopted. A `--compile` build contains every line of Python that can ever run in that executable. Precedents: R8/ProGuard keep rules, GraalVM native-image's reachability metadata, Shed Skin's whole-program inference.

`world/reach.rs` starts from the entry module and walks imports, attribute loads on modules, class and function references until the reachable set stops growing. A function called only through `getattr(obj, "name")` with a constant name is reached; one found through a non-constant name is not, unless a keep rule says so.

Keep rules live in `coil.toml`:

```toml
[compile]
keep = [
  "plugins.*",            # every module under plugins, all members, no DCE, no sealing
  "myapp.models.*.to_dict",
  "myapp.registry:Handler",
]
```

A kept module is compiled whole and registered in the module table so `importlib.import_module("plugins." + name)` finds it. A kept class keeps every member and is never sealed. A kept function is always compiled and has a boxed vectorcall entry. This is the contract Coil already has for dynamic imports (variable-driven `import_module` needs a declared dependency); `keep` names it in one place.

The build writes `dist/<Entry>.compile-report.txt` (and `.json` for `coil inspect`): every site where the closed world could not be proven, with file and line: non-constant `__import__`/`import_module`, `exec`/`eval`/`compile` with non-constant source, non-constant `getattr`/`setattr`/`hasattr` on sealed classes, `globals()`/`vars()`/`locals()` writes, `sys.modules` mutation, `__class__` assignment, `type(name, bases, ns)` with non-constant inputs, pickled classes not otherwise reached, and (section 10.3) the extension that forced the GIL. R8 prints the same kind of list.

Dynamic code is supported in three tiers:

1. Build-time evaluation. `exec`/`eval`/`compile` with a constant string compile like any other function. `collections.namedtuple(...)` and `@dataclass` with literal arguments are expanded at compile time, so their `exec`-generated methods are ordinary compiled functions (mypyc does the same for dataclasses). Without this every dataclass constructor would run interpreted.
2. Boxed dispatch. A value inference cannot narrow stays `Top`: a `PyObject*` that goes through the runtime's generic operations (`coil_rt_getattr`, `coil_rt_binop`, `coil_rt_call`), each with an inline cache (section 12.2). Full semantics, slower path. SBCL compiles untyped code the same way.
3. Runtime compilation. Non-constant `exec`/`eval`/`compile`, and `ast.parse`, load `coil_dyn.dll`: the same parser and lowering as the build, then `coil-ir::interp` over the boxed object model. The application never writes executable memory: no JIT, no RWX pages. The packager stages `coil_dyn.dll` only when the compile report shows one of these builtins reachable; `coil_rt` raises `RuntimeError("coil: dynamic code support was not bundled")` otherwise, which cannot happen for a reachable site.

Class members are dropped only from classes that never flow to a reflective site (non-constant `getattr`, `dir`, `vars`, `__dict__`, `inspect.*`, `pickle`) and are not kept. Methods of sealed classes (no reachable subclass overrides, not kept, metaclass is `type`) are candidates for direct calls.

## 7. Type inference and specialisation

Whole-program, flow-sensitive, with per-function specialisation on argument types: Agesen's Cartesian Product Algorithm (OOPSLA 1995) with bounded cloning, one clone per distinct argument-type tuple up to 8, then one `Top` version. Shed Skin and Starkiller used CPA for Python; Julia specialises on argument types the same way at a different time.

Inference uses literals, `isinstance` and `type(x) is T` guards, default values, annotations that are plain class names (as a starting point with a guard emitted where relied on, never as a semantic change), return types of specialised callees, attribute types per shape (section 8.3), container-literal element types, and hand-written signatures for builtins and runtime types (`analysis/signatures.rs`). `Int64` means "an int whose value fits in 64 bits"; every unboxed integer operation is overflow-checked and the overflow branch boxes to `BigInt`.

Where inference gives `Top` or a wide union, the value is boxed and the dynamic operation is emitted. There is no deoptimisation and no speculation without a guard; every guard has an explicit slow path in the IR.

`analysis/escape.rs` marks allocations that do not outlive their frame and never reach boxed or C code: tuples built for unpacking, iterators of `range`/`list`/`dict`, bound methods in `obj.m()`, lists consumed by a comprehension, frames of inlined generators. These become `StackAlloc` or are scalar-replaced.

## 8. Object model

### 8.1 Header

Every heap object starts with CPython's 16-byte header: `ob_refcnt: isize` at 0, `ob_type: *PyTypeObject` at 8; variable-size objects add `ob_size` at 16. Compiled code never reads or writes `ob_refcnt`. Immortal objects use CPython 3.12's convention (PEP 683): `ob_refcnt = 0xFFFF_FFFF`; the inline `Py_INCREF`/`Py_DECREF` in extensions already treat a negative 32-bit refcount as a no-op. `None`, `True`, `False`, `NotImplemented`, `Ellipsis`, small ints (-5..256, CPython's range, so `is` behaves identically), interned identifier strings and every type object are immortal.

### 8.2 Layout classes

| Class | Types | Rule |
| --- | --- | --- |
| Exact | `tuple`, `list`, `bytes`, `bytearray`, `str` (compact ASCII and compact Unicode), `float`, `complex`, `int` (`lv_tag` + 30-bit digits), `bool`, `memoryview`, `cell`, `weakref`, `PyCFunction`/`PyCMethod`, method descriptors (`PyMethodDescr`, `PyGetSetDescr`, `PyMemberDescr`, `PyWrapperDescr`), `method`, `slice`, `BaseException` and subclasses (`dict, args, notes, traceback, context, cause, suppress_context`), `traceback`, `PyModuleDef`, `PyTypeObject`/`PyHeapTypeObject`, `Py_buffer` | byte-for-byte the CPython 3.12 struct. Extensions reach into these through inline functions (`PyTuple_SET_ITEM`, `PyList_GET_ITEM`, `PyBytes_AS_STRING`, `PyUnicode_DATA`, `PyFloat_AS_DOUBLE`, `PyCell_GET`, `PyMemoryView_GET_BUFFER`; Cython reads `lv_tag`/`ob_digit`). Coil-private fields (per-object lock, list and set tails) follow the CPython struct and `tp_basicsize` includes them. |
| Prefix | `dict` (`ma_used`, `ma_version_tag` at 16/24; `PyDict_GET_SIZE` is inline), `set`/`frozenset` (`fill`, `used`; `PySet_GET_SIZE` is inline), `function` (`func_globals … func_closure` in CPython order), `code` (`co_flags`, `co_argcount` family) | first fields match CPython; the rest is Coil's. `ma_keys`/`ma_values`, the set table and the rest of `code` are never dereferenced outside `Py_BUILD_CORE`. |
| Free | instances of compiled classes, modules, frames, generators, iterators, shapes, closure cells of compiled code | Coil's layout; reached from C only through functions. |

`tools/layout_oracle.py` compiles a small C program against the pinned headers (dev machines and CI already have MSVC for `tools/build_launcher.py`) and emits `offsetof`/`sizeof` for every exact and prefix layout; `coil-rt-abi/src/cp312.rs` has the same numbers as `const` assertions.

### 8.3 Compiled classes and instances

Every class is a real `PyHeapTypeObject` with `Py_TPFLAGS_HEAPTYPE | BASETYPE | HAVE_GC | MANAGED_DICT | MANAGED_WEAKREF | HAVE_VECTORCALL`, `tp_dictoffset = 0`, subclass flags set by MRO, and every slot filled with runtime trampolines that perform the Python-level lookup (CPython's `slot_tp_call` family in `typeobject.c` is the model). `PyObject_Call` or `Py_TYPE(o)->tp_call` from an extension therefore works on any Coil object. The type descriptor carries direct entry points so a sealed class's trampoline is a direct jump.

An instance of a compiled class with no extension type in its MRO:

```
0   ob_refcnt
8   ob_type
16  shape*          hidden class: slot names, count, parent, transitions
24  dict*           NULL until __dict__ is materialised or an unknown attribute is set
32  weaklist*
40  lock            PEP 703-style byte mutex (section 8.6), padded
48  slots[n]        PyObject* per inferred attribute, then __slots__ entries
```

Shapes are V8/PyPy-style maps: a store of a new attribute transitions to a child shape or, past 32 attributes or on `__dict__` access, materialises a dict. `GetAttr` on a known shape is one load; on an unknown shape a shape check against an inline cache and a load. CPython 3.12's inline values under `Py_TPFLAGS_MANAGED_DICT` are the same idea, which is why `tp_dictoffset = 0` is what extensions already see.

A compiled class with an extension base (`class Window(QWidget)`, `class MyArray(np.ndarray)`) uses the base layout: `tp_basicsize = base.tp_basicsize + 40`, the Coil words after the base struct, allocation through the base's `tp_alloc`, construction through its `tp_new`/`tp_init` chain, destruction through its `tp_dealloc`; the GC traces the C part through the base's `tp_traverse` and the Coil part through the shape. This is `type_new`'s rule in CPython. Its instances are in the counted regime (section 9.3). A class whose metaclass is an extension type (PyQt's `sip.wrappertype`, ctypes' `_CDataMeta`) is created by calling that metaclass, which ends in the runtime's `type.__new__`; such classes are never sealed.

### 8.4 Functions, code, frames

A compiled function object has CPython's `PyFunctionObject` prefix, a `vectorcall` field pointing at a generated Win64-ABI wrapper, and Coil fields: the internal-ABI entry, the specialisation table, closure cells. `__code__` has `co_name`, `co_qualname`, `co_filename`, `co_firstlineno`, `co_flags`, `co_argcount`, `co_varnames`, `co_consts`, an empty `co_code` (deviation, 11.2); `co_lines()` works from the line table.

Frames live on the machine stack. `sys._getframe()`, `traceback`, `inspect.currentframe()` and `logging.findCaller` get frame objects materialised from the stack walk (9.4): `f_code`, `f_lineno`, `f_back`, `f_globals` work; `f_lasti` is 0; `f_locals` is a read-only snapshot of the boxed locals.

### 8.5 Hashing and equality

`hash()` uses CPython 3.12's algorithms: SipHash-1-3 with a per-process random key for `str`/`bytes`, the modular `2**61 - 1` numeric hash, the xxHash-derived tuple hash, pointer-derived default hashes. Set and dict iteration orders match CPython's for the same insertion sequence.

### 8.6 Per-object locks and the free-threaded header

Every mutable runtime container (dict, list, set, bytearray, deque, instances with a materialised dict, modules) owns a one-byte lock in Coil's part of its layout, with the states and parking-lot protocol of PEP 703's `PyMutex`: a CAS 0→1 on the uncontended path, a parked waiter list on contention. Runtime operations that mutate or read-while-mutating take it; `Py_BEGIN_CRITICAL_SECTION` (3.13) maps onto it. Under the v1 GIL the lock is always uncontended and costs the CAS; the allocator keeps per-thread page caches (`alloc/thread_cache.rs`) under the same rule. Nothing in dict, list, set or the allocator assumes a global lock, so the free-threaded build (section 10.3) changes the thread-state machine, not the containers.

The cp313t layout (`coil-rt-abi/src/cp313t.rs`, roadmap) maps onto the pin design as follows. PEP 703's header is `ob_tid: usize, _padding: u16, ob_mutex: u8, ob_gc_bits: u8, ob_ref_local: u32, ob_ref_shared: isize, ob_type` (32 bytes). Coil sets `ob_tid = 0` on every object, which no thread owns, so the inline free-threaded `Py_INCREF` always takes the shared path (an atomic add of 4 to `ob_ref_shared`) and `Py_DECREF` always calls `_Py_DecRefShared`, an exported function. The pin count is therefore `ob_ref_shared >> 2`; `ob_ref_local` is 0, or `u32::MAX` for immortal objects (PEP 703's immortality test); `ob_mutex` is the per-object lock described above, moving from Coil's tail into the header; `ob_gc_bits` holds the regime and escape bits that cp312 keeps in side bitmaps. Every pin change on the t-ABI is function-mediated except the atomic increment, which only ever makes an object more pinned; the subtraction in 9.3 reads `ob_ref_shared` instead of `ob_refcnt`. The same `gc/` and `capi/` code serves both layouts through `coil-rt-abi` accessors.

## 9. Memory: allocator, collector, and C ownership

### 9.1 Heap

`coil-rt/src/alloc/`:

- Address space reserved in 4 MiB segments (`VirtualAlloc` MEM_RESERVE, committed in 64 KiB pages on demand). A page holds one size class. Size classes: 16-byte steps to 256, then coarser steps to 32 KiB (about 60 classes, mimalloc's table). Objects over 32 KiB are large objects: page runs on a first-fit free list, never moved.
- Each page has a header and side bitmaps indexed by 16-byte granule: `allocated`, `is_object`, `mark`, `regime` (0 = tracing, 1 = counted; 9.3), `c_created`, `escaped`, `counted_slots`, `has_prehdr`, `finalized`, `has_finalizer`. Go's heap bitmap and mimalloc's page metadata are the models. A two-level page table maps any address to its page in a few instructions; it answers "is this address in the GC heap" and returns the bitmap base. Objects of the counted regime get a 16-byte pre-header (9.3); CPython's `PyObject_GC_New` prepends `PyGC_Head` the same way, and no 3.12 extension reaches it except through functions.
- Each thread state owns a cache of current pages per size class; the global page pool is behind a lock. The compiled fast path: load the thread's free-list head for the class from the TLS thread state, pop, store `ob_type`, zero the rest. Refill and GC trigger are calls.
- `PyMem_Malloc`/`PyMem_RawMalloc` use a plain mimalloc-style allocator in the same crate (not the GC heap). `PyObject_Malloc` returns GC-heap memory with `is_object` clear until `PyObject_Init`. An object initialised with `PyObject_Init` on memory outside the GC heap (static types, `PyModuleDef`, extensions that `malloc` their own objects) is recorded in the foreign-object table and is never freed by Coil.

### 9.2 Roots

Compiled frames through stack maps (9.4; `rbp` is always kept, so a walk needs no unwind tables); the module table (globals, constant pool, interned strings); thread states (current exception, `exc_info` stack, context, saved frame pointers of detached threads); externally pinned objects (9.3); static types and `PyModuleDef`s registered by `PyType_Ready`/`PyModuleDef_Init` (`tp_dict`, `tp_mro`, `tp_bases`, `tp_base`, `tp_subclasses`, `tp_weaklist`, `m_copy`); the weak-reference table, the finalizer queue, the `atexit` list, `sys.modules`, the zero-count table of 9.3.

### 9.3 Two regimes

Decision 13 requires CPython's release timing for memory owned by C extensions, while compiled-code objects stay under tracing and compiled code never touches `ob_refcnt`. Those three constraints fix the design: objects are split into two regimes by the type of memory they own, and the compiled world contributes liveness information to the counted regime through a store barrier and a deferred stack check, never through `ob_refcnt`.

**Tracing regime (T)**: instances of Coil types (int, str, tuple, list, dict, compiled classes, …) whatever created them. Lifetime is reachability. `ob_refcnt` is the number of references C holds; a positive count after the subtraction below makes the object a root.

**Counted regime (R)**: instances of extension-defined types, that is any type whose `tp_dealloc` is not Coil's: static types from `.pyd` files, `PyType_FromSpec` types created by extensions, and compiled subclasses of either (section 8.3), since their `tp_dealloc` chain releases a C resource. Lifetime is a reference count with three contributions:

1. `ob_refcnt`: references held by C, maintained by the extension's inline `Py_INCREF`/`Py_DECREF` exactly as in CPython.
2. `heap_refs`: references held in compiled-code heap memory (instance slots, list items, dict entries, cells, module globals, generator frames). Kept in the object's 16-byte pre-header (`has_prehdr`), or in a hash map for foreign R objects. Maintained by the store barrier: every store of an R-typed or `Top`-typed value into heap memory, whether emitted by the compiler (`StoreField` with `BarrierKind::Ext` or `::Dynamic`) or performed by the runtime (`list.append`, `dict.__setitem__`, `setattr`), increments the new value's `heap_refs` if it is R and decrements the overwritten value's if it is R. Stores of statically T-typed values carry no barrier. The `Dynamic` barrier is a page-table lookup and a bit test; the `Ext` barrier is an unconditional `inc`/`dec` on the pre-header.
3. Stack references: slots in compiled frames and registers. Not counted (Deutsch and Bobrow, "An Efficient, Incremental, Automatic Garbage Collector", CACM 1976: deferred reference counting). When contributions 1 and 2 are both zero the object goes on the zero-count table (ZCT); it is freed if, when the ZCT is processed, no compiled frame of any thread holds it.

The ZCT is processed at safepoints when it holds 64 entries, at the eval-breaker tick (every switch interval, 5 ms), and before any full collection. Processing walks every thread's compiled frames with the stack maps, clears the entries found, and runs `tp_dealloc` on the rest, in insertion order. The walk is proportional to stack depth, not heap size.

Release timing, stated precisely:

| Situation | When the R object's `tp_dealloc` runs |
| --- | --- |
| C drops its last reference; compiled code never saw the object (`c_created`, not `escaped`) | immediately, inside `_Py_Dealloc`, as in CPython |
| C drops its last reference; compiled code holds it only in frame slots | at the first ZCT processing after the last frame holding it returns: at most 64 ZCT entries or 5 ms after CPython would have freed it |
| C drops its last reference; compiled code holds it in heap memory | immediately when the last heap holder overwrites or deletes the reference (barrier decrement to zero with `ob_refcnt == 0` enqueues to the ZCT; the next processing frees it), or when the last heap holder is itself collected (sweep decrements `heap_refs` of every R object a dead T object referenced). The second case is the only timing difference from CPython: extension memory held only through compiled-code objects waits for those objects to be collected. |
| Compiled code drops its references first; C still holds some | when C's last `Py_DECREF` reaches zero, immediately |
| Reachable from both C and compiled code | freed when both contributions are gone, whichever goes last, by the rules above |
| Escaped C→Python, C drops later | the escape itself changes nothing; the rules above apply from the moment C decrements |
| Cycles through C objects (R↔R through `ob_refcnt`, or R↔T) | by the full collection, which subtracts internal references (below) and runs `tp_clear`/`tp_dealloc` on the unreachable cycle. CPython also frees these only in its cycle collector. |
| R object with a compiled `__del__` (subclass of an extension type) | `tp_finalize` then `tp_dealloc` on the same path as CPython's `subtype_dealloc`, at the times above |

Objects C creates of Coil types (`PyTuple_New`, `PyList_New`, `PyCell_New`, `PyUnicode_FromString`) are T objects with `c_created` set. Tuples, lists and cells created this way also get `counted_slots`: C fills them through inline stores (`PyTuple_SET_ITEM`, `PyList_SET_ITEM`, `PyCell_SET`) that steal a counted reference, so their item references are counted references as in CPython. Compiled code storing into a counted list takes the slow path and increments the item's `ob_refcnt`; reading never changes anything; when a counted container dies the sweep decrements each item. Dicts and sets are never counted: all C access goes through functions that store without counting (the dict traces its contents) and return borrowed references. A C-created T object whose `ob_refcnt` reaches zero while `escaped` is clear is freed immediately by `_Py_Dealloc`, which keeps C's temporaries (argument tuples, intermediate ints, `Py_BuildValue` results) as prompt as in CPython. `escaped` is set for T objects when a pointer crosses into compiled code (a C return value, a callback argument, an item read from a counted container).

`_Py_Dealloc` is exported, as it must be: numpy's 19 `.pyd` files and Pillow's 8 import it. Its behaviour by regime: R object → run `tp_dealloc` if `heap_refs == 0` and the object is not on the ZCT-pending path, else enqueue to the ZCT; T object → clear the pin; if `c_created` and not `escaped`, run `tp_dealloc` now; otherwise the object is GC-owned.

Full collection:

1. Stop: with the GIL, the collecting thread is the only attached thread; detached threads (in C after `PyEval_SaveThread`, or C threads that never attached) have their last compiled frame recorded. Without the GIL (roadmap), a stop request in every eval-breaker word parks attached threads at their next safepoint.
2. Process the ZCT.
3. Subtract: for each counted container and each GC-tracked C object (`Py_TPFLAGS_HAVE_GC` with `tp_traverse`), visit referents and decrement a shadow copy of their `ob_refcnt`. This is `gcmodule.c`'s `gc_refs` subtraction. Objects whose shadow count stays positive are held from memory Coil cannot see and are roots. For R objects, `heap_refs` and ZCT presence also count as held.
4. Mark from all roots: compiled objects by shape or by type-specific trace functions the compiler emits, C objects by `tp_traverse` with a marking `visitproc`.
5. Finalize: unmarked objects with `__del__` or `tp_finalize` (PEP 442) and `finalized` clear run their finalizer once; anything resurrected is re-marked.
6. Clear weak references to dead objects and run their callbacks (CPython's `handle_weakrefs` order).
7. `tp_clear` on dead GC-tracked C objects. `Py_CLEAR` inside them calls `_Py_Dealloc`, which during a sweep only records.
8. `tp_dealloc` on every dead R object and every dead `c_created` T object exactly once; `tp_free` during a sweep queues the block. Dead T objects referencing R objects decrement their `heap_refs`; dead counted containers decrement their items; R objects brought to zero this way are freed in the same sweep.
9. Free every dead block. Nothing was reused before this point. Allocation during 5–8 is allowed; new objects are born marked.

A debug build (`--features debug-heap`) poisons freed blocks, quarantines freed R objects for one cycle, checks `heap_refs` and `ob_refcnt` underflow, verifies the barrier by recomputing `heap_refs` from a full heap walk at each collection, and asserts thread attachment at every API entry. The extension matrix runs against it.

### 9.4 Stack maps and safepoints

Safepoints are calls (every call into the runtime or C) and loop back-edges (a load-and-test of the thread's eval-breaker word). At a safepoint every live reference is in a frame slot; the allocator never keeps a reference in a register across one. The stack map for a return address is a bitmap over the frame's slots, sorted by address in `.rdata` (Go's `pclntab`). The frame also records its function descriptor, so a walk yields function, line and generator state.

Generational collection is not in the first design. Non-moving generational collection later uses sticky mark bits (Demers, Weiser, Hayes, Boehm, Bobrow, Shenker, PLDI 1990) with card marking on the existing `StoreField` barrier slot; C objects stay permanently in the remembered set, since C stores through macros carry no barrier.

### 9.5 Pacing

A full collection starts when bytes allocated since the last one exceed `max(8 MiB, live_bytes)` (Go's GOGC=100 rule), or on `gc.collect()`. Extension-owned memory is released by the counted regime, so pacing does not need to see it. As a backstop for memory held only through compiled-code objects, the runtime samples `GetProcessMemoryInfo` every 64 MiB of GC-heap allocation and starts a collection when private bytes have grown by more than the live GC heap since the last one.

### 9.6 Finalisation of tracing-regime objects

`__del__` of a compiled class instance with no extension base runs at collection, on the collecting thread, with the exception context cleared. `open(path).read()` closes the file at the next collection, as on PyPy; `with`, `close()` and `try/finally` behave exactly as in CPython. `sys.getrefcount(x)` returns the pin count plus one; `gc.disable()` only stops automatic pacing; `gc.get_referrers` is unsupported; `gc.get_objects` and `gc.get_referents` work.

## 10. Runtime (`coil-rt`)

### 10.1 Exports and DLLs

`coil-rt` builds `python312.dll` with `-C target-feature=+crt-static`, so the runtime needs no `vcruntime140.dll` (extensions still do; it ships from the embeddable zip as today). The DLL exports:

- the CPython C API: every symbol `python312.dll` exports in 3.12.10 (1,706; 1,349 are `PyAPI_FUNC` declarations, the rest data such as `PyFloat_Type` and `_Py_NoneStruct`, plus underscore functions inline helpers call: `_Py_Dealloc`, `_PyArg_ParseTuple_SizeT`, `_Py_BuildValue_SizeT`, `_PyObject_New`, `_Py_IncRef`/`_Py_DecRef`). `tools/gen_capi.py` keeps the list from the headers and diffs it against the real DLL's export table. Unimplemented entries export a stub that raises `SystemError("coil: <name> is not implemented")` naming the calling module; the extension matrix drives the stub count to zero for the supported set.
- `coil_rt_*`: the compiled-code ABI from `abi.toml`.

`python3.dll` is a forwarder DLL, 867 entries, written by `link/pe.rs` the way CPython's is; abi3 wheels import it. Wheels built with `Py_LIMITED_API >= 3.12` call `_Py_IncRef`/`_Py_DecRef`; wheels built for an older limited API (PyQt5's `cp38-abi3`, PyQt6's `cp310-abi3`, cryptography's `cp311-abi3`) use the inline `ob_refcnt` access of their era, which section 9.3 already handles.

`coil-rt-abi` holds per-version layout constants (8.2); cp313 is a second module and a second DLL build (`python313.dll`), selected by `--python 3.13`; cp313t/cp314t follow for the free-threaded roadmap (`python313t.dll`). `coil-capi-sys` is the generated Rust declaration of every C API type and function (`bindgen` at dev time, output checked in).

Measured surface: the embeddable's 21 `.pyd` files plus numpy 2.5.1 and Pillow 13.0 import 564 distinct `python312.dll` symbols; the v1 extension set in 15.3 raises the union to 610. `tools/capi_usage.py` produced those numbers and orders the C API work.

### 10.2 Thread state

`tstate.rs`: one `ThreadState` per thread that has run Python: attached/detached state (PEP 703's terms; "holds the GIL" in v1), current exception, `exc_info` stack, recursion depth, eval-breaker word, saved frame pointer when detached, `PyGILState` counter, context, allocation caches, ZCT. Compiled code reaches it through a TLS slot in the TEB (`gs:[0x1480 + 8*index]`, a slot below 64 allocated by `TlsAlloc` at runtime start; Go's Windows runtime does the same).

### 10.3 The GIL now, free-threading on the roadmap

v1: one attached thread at a time. The GIL is a mutex with a switch interval (5 ms, `sys.setswitchinterval`), a drop-request flag in every eval-breaker word, and the fairness of CPython's `ceval_gil.c`. Compiled code detaches around blocking runtime calls (`time.sleep`, socket and file I/O in native modules, lock acquisition with timeout); extensions detach with `Py_BEGIN_ALLOW_THREADS`; C threads attach with `PyGILState_Ensure`. The collector runs on the attached thread; detached threads may hold pinned objects and borrowed references to objects reachable from their recorded frames, and by the C API's rules may not touch unpinned objects or `ob_refcnt` while detached. Nothing moves, so what they hold stays valid.

Roadmap, designed for now: the free-threaded build targets the cp313t (later cp314t) ABI with the header mapping of 8.6. Attach/detach becomes a per-thread state with a stop-the-world request for collections and ZCT processing (ZCTs are per thread; processing happens at the pause); the allocator's per-thread caches and the per-object locks are already in place. At build time, `coilc` loads every `.pyd` the application bundles in a probe process (`coilc gil-probe`, the runtime plus a host that calls each `PyInit_*` under the t-runtime) and reads the PEP 489 `Py_mod_gil` slot. If every extension declares `Py_MOD_GIL_NOT_USED`, the build links the GIL-free runtime; a single-phase-init module or one without the slot forces the GIL, and the compile report names it, which is what CPython 3.13t does at import time. The two runtimes are two DLL builds of the same crate (`--features gil` / `--features free-threaded`); the compiled code is identical except for the barrier atomics selected by the ABI module.

### 10.4 Import and extension loading

`import.rs` owns `sys.modules` and a `meta_path` finder (`CoilFinder`) resolving names against the module table (compiled modules), the native table (Rust modules) and the `.pyd` search path (`_internal/` and `_internal/lib/**`, registered with `AddDllDirectory` as `_bootstrap.py` does today). `importlib` is compiled from the stdlib and sees Coil modules as built-ins with proper specs, so `importlib.resources`, `pkgutil` and `__file__`-relative data loading work. A compiled module's `__file__` is the path its source would have had in the bundle (`_internal/lib/numpy/__init__.py`); the package directory exists with its non-`.py` files, so `os.path.join(os.path.dirname(__file__), "pdfium.dll")` resolves.

Loading a `.pyd`: `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS`, `GetProcAddress("PyInit_<name>")`, call while attached. PEP 489 multi-phase init: a returned `PyModuleDef` runs `Py_mod_create` then `Py_mod_exec`; single-phase init returns a module fixed up with `m_copy`. `PyState_AddModule`, `PyModule_GetState`, `m_size` state work as in CPython. `_imp.create_dynamic`, `_imp.exec_dynamic`, `_imp.is_builtin` are provided so `importlib._bootstrap_external.ExtensionFileLoader` runs unchanged.

### 10.5 Exceptions across the boundary

Inside compiled code an exception is the thread state's `current_exception` plus a flag return (12.3). The C API sees exactly CPython's protocol: a function returns `NULL` (or -1) and `PyErr_Occurred()` is set; `PyErr_SetString`, `PyErr_Format`, `PyErr_Fetch`/`PyErr_Restore`, `PyErr_GetRaisedException`/`PyErr_SetRaisedException`, `PyErr_NormalizeException`, `PyErr_WriteUnraisable`, `PyErr_CheckSignals`, `PyErr_SetFromWindowsErr` operate on the same state. A Python callback invoked from C that raises returns `NULL` with the exception set; a C function returning `NULL` to compiled code becomes a `Raise` at the call site. `PyErr_SetObject` chaining follows `_PyErr_SetObject` in 3.12. Exception types are real type objects (`PyExc_ValueError` is the address of `ValueError`); `PyErr_NewException` creates heap types.

A C++ or SEH exception thrown through compiled frames (PyQt can) unwinds through them because every compiled function has unwind info (12.4) and reaches the runtime's vectored handler, which prints the Python traceback of the frames it crossed and aborts, as `faulthandler` would.

### 10.6 Native modules

The embeddable 3.12.10 runtime has 67 built-in modules and 21 separate `.pyd` files. The `.pyd` files load through the C API layer unchanged: `_asyncio`, `_bz2`, `_ctypes` (with `libffi-8.dll`), `_decimal`, `_elementtree`, `_hashlib` (OpenSSL), `_lzma`, `_msi`, `_multiprocessing`, `_overlapped`, `_queue`, `_socket`, `_sqlite3`, `_ssl`, `_uuid`, `_wmi`, `_zoneinfo`, `pyexpat`, `select`, `unicodedata`, `winsound`, with `libcrypto-3.dll`, `libssl-3.dll`, `sqlite3.dll`, `libffi-8.dll`, `vcruntime140*.dll`, all from the embeddable zip Coil already downloads, caches and verifies. `_ctypes` is on the critical path: half of the v1 extension set is pure Python over `ctypes` (15.3).

The 67 built-ins (`native/manifest/stdlib.toml` is the authoritative table):

| Provided by | Modules |
| --- | --- |
| Rust, in `coil-rt` | `builtins`, `sys`, `_imp`, `gc`, `marshal`, `_thread`, `_weakref`, `_codecs` (UTF-8/16/32, Latin-1, ASCII, charmap, `mbcs`/`oem` through `MultiByteToWideChar`, escape codecs), `_sre` (RustPython's `sre-engine`, MIT, is the candidate base), `_struct`, `_string`, `_locale`, `_signal`, `atexit`, `errno`, `faulthandler`, `math`, `cmath`, `time`, `_random`, `_contextvars`, `_collections` (`deque`, `defaultdict`, `_count_elements`, `_tuplegetter`), `itertools`, `array`, `binascii`, `zlib` (`miniz_oxide`), `mmap`, `nt`, `_winapi`, `msvcrt`, `winreg`, `_io` primitives (`FileIO`, `_WindowsConsoleIO`), `_ast` (node classes; bodies via `coil_dyn.dll`), `_typing`, `_tracemalloc` (stub), `audioop`, `_multibytecodec` and the `_codecs_*` CJK tables |
| Pure Python, compiled | the stdlib's own fallbacks: `_pyio` for `_io` (through `shims/_io.py`), `_pydatetime` for `_datetime`, `_py_abc` for `_abc`, the pure paths of `functools`, `operator`, `heapq`, `bisect`, `warnings`, `json`, `pickle`, `stat`, `statistics` for `_functools`, `_operator`, `_heapq`, `_bisect`, `_warnings`, `_json`, `_pickle`, `_stat`, `_statistics`; `shims/_csv.py` (PyPy's `lib_pypy/_csv.py`, MIT, is the reference); `shims/_tokenize.py` over `coil_dyn.dll`'s tokenizer; `shims/_opcode.py` minimal |
| Unsupported | `_lsprof` (`cProfile`), `_symtable`, `_xxsubinterpreters`, `_xxinterpchannels`, `xxsubtype`, `_md5`/`_sha1`/`_sha2`/`_sha3`/`_blake2` (`hashlib` uses `_hashlib.pyd` and tolerates their absence) |

`_pyio` is slow on CPython because it is interpreted; compiled natively it is the normal I/O stack. The benchmark tier measures it before this choice is final; the fallback is a Rust `_io`.

## 11. Standard library and semantics

### 11.1 Sources

Pure-Python stdlib modules compile from the `Lib/` tree of the pinned CPython source tarball (3.12.10, hash already in `tools/build_launcher.py`), downloaded once into `~/.coil/cache/stdlib-src/3.12.10/` with the same validation as the runtime zip. Per-module IR is cached in `~/.coil/cache/native/ir/<abi>/<optimize>/<module>.ir`; a build loads only reachable modules. The embeddable's `python312.zip` is not used in compiled builds. Modules the manifest marks `unsupported` fail with a message naming the module and the reason, at build time if statically imported.

### 11.2 Semantics in scope and deliberate deviations

In scope: the full 3.12 language. Statements and expressions including `match`, `:=`, PEP 701 f-strings, PEP 695 type parameters, `except*`, decorators, metaclasses, descriptors, `__slots__`, `__init_subclass__`, `__set_name__`, `__class_getitem__`, C3 MRO, `super()`, properties, closures and `nonlocal`, generators with `send`/`throw`/`close`, `yield from`, coroutines, async generators and comprehensions, context managers, star-unpacking, comprehension scopes, `__getattr__`/`__getattribute__`/`__setattr__`, `__class__` assignment with CPython's layout check, arbitrary-precision ints with the int/str conversion limit, IEEE floats with CPython's `repr` and `__format__`, `complex`, full `str` (every method; Unicode data through `unicodedata.pyd`), `bytes`/`bytearray`/`memoryview` and the buffer protocol, dict order and views, sets, slices, `__index__`, `__hash__`/`__eq__` contracts, `KeyboardInterrupt` from Ctrl-C, `RecursionError` at `sys.getrecursionlimit()`, `threading`, `multiprocessing` spawn, `ctypes`, `asyncio` (Proactor loop through `_overlapped.pyd`), `subprocess`, `socket`, `ssl`, `sqlite3`, `decimal`, `json`, `re`, `pickle`, `logging`, `argparse`, `dataclasses`, `enum`, `typing`, `tkinter` when `_tkinter.pyd` and Tcl/Tk are bundled.

| Area | Behaviour under `--compile` |
| --- | --- |
| Finalisation | tracing-regime objects: at collection (9.6). Extension-type objects: CPython timing (9.3), except when held only through compiled-code objects. |
| `sys.getrefcount` | pin count + 1 |
| `sys.settrace`, `sys.setprofile`, `sys.monitoring`, `pdb`, `coverage` | unsupported; `settrace` is a no-op that warns once |
| `frame.f_locals` | read-only snapshot; `f_lasti` is 0; `f_trace` ignored |
| `code.co_code`, `dis` | empty bytes; `dis` raises `TypeError("no bytecode")` |
| `exec`/`eval`/`compile`/`ast.parse` on non-constant source | correct, run on the IR interpreter in `coil_dyn.dll`, slower |
| `gc.get_referrers`, `gc.garbage` | unsupported / always empty |
| `sys.implementation.name`, `platform.python_implementation()` | `"coil"`, `"Coil"`; `sys.version_info` is the impersonated version; `sys.version` starts `"3.12.10 (coil …)"`; `sys.implementation.cache_tag` is `None` |
| `inspect.getsource` | works in default mode from the embedded source archive; `OSError` under `--secure` |
| `__import__` of names outside the module table | `ModuleNotFoundError` (as today's bundles for undeclared dynamic imports) |
| `id()` and `is` | stable addresses; CPython's small-int and interned-string identity |
| Grammar | the target version's, regardless of host Python |
| `PYTHON*` environment variables | ignored, as today |

### 11.3 Multiprocessing and the entry contract

`sys.frozen = True`, `sys.executable` is the exe, `sys.argv` is exact, `sys.orig_argv` is set, the working-directory contract is unchanged. `multiprocessing.spawn` runs with `WINEXE = True`: the child is the same exe with `--multiprocessing-fork`, `freeze_support()` dispatches, and `__main__` is reloaded from the module table as `__mp_main__` without reading a file. Today's `_bootstrap.py` sets `WINEXE = False` so CPython reloads the `.pyc`; compiled builds have no file to reload.

## 12. Code generation (`coilc`)

### 12.1 Structure

**Recommendation** adopted: one architecture-neutral middle layer with every optimisation (section 5), a thin per-architecture layer (`backend/<arch>/`: lowering rules, register set, encoder, ABI, unwind format) and a small per-architecture runtime piece (`coil-rt/src/arch/<arch>.rs`: TLS access, stack probe, Win64↔internal call shims, atomics). Go's `cmd/compile` is the precedent: `*.rules` per architecture, `rewritegeneric` shared.

`backend/lower.rs` turns Coil IR into machine-shaped IR: explicit `coil_rt_*` calls, boxing, barriers, safepoint polls, exception-flag tests, frame-slot assignment. `x86_64/rules.rs` matches machine-shaped instructions to patterns (`IAddOvf → add; jo`, `LoadField → mov reg,[base+off]`, `ShapeGuard → cmp [obj+16], imm; jne`), `isel.rs` applies them, `regalloc.rs` allocates (linear scan on SSA with live-range splitting, Wimmer & Franz, CGO 2010), `encode.rs` emits bytes.

### 12.2 x86-64 specifics

- Frame: `push rbp; mov rbp, rsp; sub rsp, N`, callee-saved pushes after. Frames over 4 KiB get a probe loop (no `__chkstk` import). `rbp` always valid.
- Encoder: table-driven (REX/VEX, ModRM/SIB) over the roughly 140 instruction forms the rules emit, including SSE2 scalar float, `cmov`, `setcc`, `bt`. Differential tests against the `iced-x86` crate (MIT) in `cargo test`.
- Addressing: RIP-relative for code and constants; absolute addresses only in data tables, which get base relocations so ASLR works.
- Inline caches are data: each dynamic site owns a 32-byte line in `.data` (shape or type pointer, slot or target) that the runtime updates on misses. Code is immutable after link.

### 12.3 Calling conventions

Internal ABI for Python-to-Python calls: integer/pointer arguments in `rdi, rsi, rdx, rcx, r8, r9, r10, r11`, floats in `xmm0-7`, the rest on the stack; result in `rax` or `xmm0`; exception state in `rdx` on return (0 = none; the exception is in the thread state). Boxed-only functions use `rax = NULL` as the flag, CPython's convention, so their vectorcall wrapper is a tail call. Go's ABIInternal/ABI0 split (Go 1.17) is the precedent: every function that escapes as a function object gets a Win64-ABI `vectorcall` wrapper `(callable, args*, nargsf, kwnames)` that maps arguments, calls the internal entry and converts the flag.

Calls into the runtime and into C use the Win64 ABI with shadow space; the shim records the frame pointer in the thread state when the callee may detach.

### 12.4 Exceptions and unwinding on Windows x64

Python exceptions do not use SEH or table-based unwinding. Python raises for control flow (`StopIteration`, `KeyError` behind `dict.get` idioms, `AttributeError` behind `hasattr`) at rates where a table-driven unwind (microseconds per throw) would dominate; CPython and Go propagate errors by return value. In compiled code the flag test after a may-raise call is `test rdx, rdx; jnz handler`, and a `try` costs nothing until an exception occurs.

Every compiled function still gets a `.pdata` entry (`RUNTIME_FUNCTION {BeginAddress, EndAddress, UnwindInfoAddress}`) and `UNWIND_INFO` in `.xdata` describing its prologue (`UWOP_PUSH_NONVOL rbp`, `UWOP_SET_FPREG`, `UWOP_ALLOC_SMALL/LARGE`, callee-saved pushes), with no handler flags. This is required for `RtlVirtualUnwind`/`RtlUnwindEx` when a C++ exception from an extension crosses compiled frames, for `RtlCaptureStackBackTrace` and ETW, for WinDbg and Visual Studio call stacks, and for crash dumps. Go 1.21 added the same SEH unwind information to Go functions on windows/amd64. `x86_64/unwind.rs` derives the codes from the emitted prologue; a test checks them against `RtlVirtualUnwind` on real frames.

The runtime installs one vectored exception handler: access violations and stack overflows in compiled or extension code produce a Python traceback from the frame walk and a Windows error code, then terminate. Recursion depth is checked in every prologue against `sys.getrecursionlimit()`; the PE's `SizeOfStackReserve` is 16 MiB for the main thread (`[compile] stack`), and `threading.stack_size()` applies to new threads.

### 12.5 Generators, coroutines, async

Generators are state machines, not stack switches: the body is a resumable function; locals live across a `yield` sit in a heap-allocated generator frame (Coil layout, shape-traced, barrier-covered); the entry switches on a state integer. `send`, `throw`, `close`, `yield from`, `gi_frame`, `gi_running`, `gi_yieldfrom`, return values into `StopIteration`, `GeneratorExit` on close, `try/finally` around a yield, and async generators' `asend`/`athrow`/`aclose` are expressed in the IR by `lower.rs`; the backend sees ordinary control flow. Coroutines are generators with `CO_COROUTINE`; `await` is `YieldFrom` on `__await__()`. C#'s and Rust's async transforms, Cython's generators and CPython's own heap frames are the precedents. Nothing is architecture-specific, so `arch/` has no stack switching and ARM64 inherits generators unchanged. Generators that escape analysis proves local are inlined into loops.

## 13. Integration with Coil

### 13.1 CLI and `coil.toml`

```
coil build ./proj --compile [--keep PATTERN]... [--emit ir|asm]
```

```toml
[build]
compile = true          # default false; profiles may override, as for secure/mode
python = "3.12"         # selects the impersonated ABI: 3.12 or 3.13

[compile]
keep = ["plugins.*"]    # section 6
report = true           # dist/<Entry>.compile-report.{txt,json} (default true)
opt = "speed"           # "speed" | "size"
stack = "16MiB"
```

Priority is unchanged: CLI > profile > `[build]` > defaults. `config.get_build_config` gains `compile`; `generate_toml` writes `compile = false` commented out; `cli._apply_toml_config` and `builder.build` pass it through; `coil inspect` runs `coilc inspect --json` and prints `Compile: yes (abi cp312, keep rules: 1)` with the closed-world counts; `coil doctor` reports whether `src/coil/_runtime/` has the runtime for the requested `--python` and whether `coilc.exe` runs.

`--compile` requires `--os windows` and `--python` 3.12 or 3.13; anything else is an error listing the supported versions. `--optimize`, `--secure`, `--gui`/`--console`, `--entry`, `--name`, `--icon`, `--exclude`/`--include`, `--requirements`, `--clean`, `--mode`, `--profile`, `--dry-run`, `--verbose` keep their meaning.

`install_dependencies` passes `--implementation cp --abi cp312 --platform win_amd64 --python-version 3.12` (or the 3.13 equivalents) for compiled builds unconditionally, so pip selects cp312 wheels whatever the host runs. `--only-binary=:all:` is not passed, because pyautogui, pygetwindow, pyscreeze, pytweening, mouseinfo and pyrect are published only as sdists; pip builds those into wheels locally. After installation the packager checks every wheel pip built from an sdist: a `py3-none-any` result is accepted, a result tagged for the host (`cp312-win_amd64` compiled on the build machine against the host's headers) is rejected with a message naming the package, since it was compiled against the host and not downloaded for the target.

### 13.2 Build pipeline

`builder.build(..., compile=True)`:

1. Resolve dependencies (unchanged).
2. Prepare runtime (unchanged call); the stage takes only `.pyd` and vendor `.dll` files from it.
3. Install dependencies (unchanged, with the pip flags above).
4. `native.driver.compile_app(CompileJob(...))` per entry: writes `job.json` (entries, source roots, dependency roots, stdlib source root, keep rules, ABI, optimize, secure, versioninfo, subsystem, icon, manifest, stack, output path) and runs `coilc.exe build job.json`. `coilc` produces `<Entry>.exe` with icon, VERSIONINFO, subsystem and manifest in `.rsrc`, plus the compile report. `COIL_COILC=<path>` overrides the executable for development.
5. `packager._build_app_directory` stages as in section 2: no `python312.zip`, no `.pyc`, no `_pth`, no launcher stub; `coil_dyn.dll` only when the report says so; `_internal/lib/` keeps every non-`.py` file of each dependency; `_remove_py_files` drops the sources (compiled into the exe).
6. Portable mode appends the zipped stage directory to the bootloader exactly as today; the inner executable is the compiled app. Cache keys, leases, repair and the certificate-aware trailer are untouched.

### 13.3 Stamping

`link/rsrc.rs` builds `RT_GROUP_ICON`/`RT_ICON` from the `.ico`, `RT_VERSION` from the `versioninfo` dict the CLI already resolves (the VS_VERSION_INFO byte layout in `platforms/windows.py:set_version_info` is the reference; a Python test asserts both produce identical bytes), and `RT_MANIFEST` from `templates/launcher.manifest`. The subsystem is a field in the optional header. Multi-entry bundled builds produce one compiled exe per entry (own subsystem and VERSIONINFO, as today) sharing `python312.dll` and `_internal/`.

Output is deterministic: sorted symbol order, `TimeDateStamp` = hash of the image (MSVC `/Brepro`), no absolute paths in default mode beyond bundle-relative `__file__` strings. Same inputs, same bytes (section 16).

### 13.4 Secure mode and decompile

Default mode writes `_internal/app/.coil_meta.json` (`"compiled": true`) and `.coil_source.zip` exactly as `obfuscate_default` does, so `coil decompile` recovers the source of a compiled default build with no change to `decompiler.py`; tracebacks show source lines from that archive through `linecache`. `--secure` omits the archive, strips docstrings (`--optimize 2`), reduces `co_filename` to basenames and keeps identifiers (needed for `getattr`, pickling and `__qualname__`). There is no bytecode in either mode; `coil decompile` on a secure compiled build refuses with the existing message.

## 14. Build order

1. Runtime core (Rust): allocator with thread caches, side bitmaps, object header, layouts with the layout oracle, type objects for the exact and prefix set, per-object locks, both regimes of section 9 with the ZCT, GIL and thread states, `builtins`/`sys`/`_imp` cores. Early check (decision 14): CPython 3.12.10's `_testcapi.pyd` is built in CI from the pinned source with MSVC (as `build_launcher.py` builds the launcher) and its C entry points are driven from `cargo test` through the C API, before any compiled Python exists.
2. `coil-front` and `coil-ir`: tokenizer, parser (differential test against `ast.parse` over all of `Lib/`), symtable, lowering, SSA, verifier, printer, interpreter; `coil-dyn` as a DLL.
3. `coilc` middle end: world, inference, optimisations, run through the interpreter for semantic checks before any machine code exists.
4. x86-64 backend and PE linker; `iced-x86` differential tests, `pefile` structure tests, `RtlVirtualUnwind` tests.
5. C API layer to completion for the extension set; stdlib shims; conformance runs (`test_capi`, `Lib/test`).
6. Coil integration: CLI, config, packager, inspect/doctor, portable/bundled, stamping.
7. Wheel packaging (`tools/build_native.py`), CI, reproducibility check.

Each step ends with its tests passing and the next step consuming its output.

## 15. Testing

### 15.1 Tiers

| Tier | What | Where |
| --- | --- | --- |
| Runtime unit | allocator, GC phases, both regimes (counted containers, barrier, ZCT, resurrection, weakref order, cycles through C), layouts, locks | `cargo test -p coil-rt`; `tools/layout_oracle.py` in CI |
| C API conformance | `_testcapi` from Rust (step 1); later `Lib/test/test_capi/*` compiled with `--compile`; expected-failure list checked in | `native/crates/coil-rt/tests/capi/`, `tests/native/conformance/capi/` |
| Compiler unit | parser differential against CPython over `Lib/`, IR construction and verification, inference on small programs, each pass with before/after IR text, encoder against `iced-x86`, PE structure with `pefile`, unwind codes against `RtlVirtualUnwind` | `cargo test -p coil-front -p coil-ir -p coilc`; `tests/native/` |
| Language conformance | CPython's `Lib/test/` modules that need no bytecode or tracing (`test_grammar`, `test_int`, `test_float`, `test_str`, `test_unicode`, `test_dict`, `test_list`, `test_set`, `test_class`, `test_descr`, `test_generators`, `test_coroutines`, `test_asyncgen`, `test_exceptions`, `test_except_star`, `test_patma`, `test_scope`, `test_itertools`, `test_re`, `test_json`, `test_pickle`, `test_threading`, `test_asyncio`, `test_io`, `test_ctypes`, …), each compiled to an exe and run; expected-failure list per module | `tests/native/conformance/lang/` |
| Differential | every existing executable integration test (`test_launcher_integration`, `test_portable_integration`, `test_pywin32_integration`, `test_subsystem_integration`, `test_versioninfo_integration`, `test_dependency_build_integration`) re-run with `COIL_TEST_COMPILE=1`; they assert on app-written JSON, so the compiled exe must produce the same results | existing files, one fixture switch |
| Extension matrix | 15.3, each with a small app exercising its C paths; release and debug-heap runtimes; `tools/capi_usage.py` coverage must be 100% per package | `tests/native/extensions/` |
| Reproducibility | two `coilc` runs of the same job on different machines produce byte-identical exes | `tools/build_native.py --verify` |
| Performance | pyperformance subset plus Coil cases (startup, import of a 200-module app, `_pyio` throughput, numpy-heavy loop, dataclass construction, PyQt widget churn for the R regime); CPython 3.12 numbers beside them; regressions reported, not gated | `tests/native/bench/` |
| CI | `tests.yml` gains a job that builds the workspace with cargo, runs the Rust tiers, builds the wheel with `_runtime/` and runs the Python tiers against it | `.github/workflows/` |

The existing audit tools (`tools/audit_launchers.py`, `audit_paths.py`, `audit_portable.py`, `audit_subsystems.py`) gain a `--compile` flag.

### 15.2 Memory-regime tests

`coil-rt/tests/regimes.rs` builds synthetic extension types with `tp_dealloc` counters and asserts the exact timing table of 9.3: immediate release on last C decref with no compiled holder; release at the next ZCT processing when held only on a stack; release on overwrite of the last heap holder; release at collection for cycles; no release while either side holds. The same table is run through compiled Python in `tests/native/extensions/test_timing.py` with a tiny C extension built in CI.

### 15.3 v1 extension set

Verified against PyPI on 2026-10-21 (`pip install --dry-run --report`, wheel contents inspected):

| Package | Kind | What it needs from Coil |
| --- | --- | --- |
| numpy 2.5 | C extensions, 19 `.pyd`, 396 symbols | full object-protocol and buffer API; `PyType_FromSpec` and static types; counted regime for `ndarray` |
| Pillow 13 | C extensions, 8 `.pyd`, 113 symbols | buffer API, `PyCapsule`, Windows GDI calls for `ImageGrab` (pyscreeze depends on it) |
| pywin32 312 | C extensions (`win32api`, `win32gui`, `pywintypes` …) | existing `.pth` and `pywin32_system32` DLL handling from `_bootstrap.py` carries over |
| PyQt6 6.11 + PyQt6-sip 13.13 | sip-generated C++ extensions; PyQt6 is `cp310-abi3` (imports `python3.dll`, inline refcounts), sip is cp312 | C metaclass (`sip.wrappertype`), compiled subclasses of C types, C++ exceptions crossing compiled frames, Qt threads attaching through `PyGILState_Ensure` |
| PyQt5 5.15.11 + PyQt5-sip 12.20 | same as PyQt6; PyQt5 is `cp38-abi3`, 38 `.pyd`, 151 symbols; sip 150 | same |
| cryptography 50 | PyO3 (Rust) `_rust.pyd`, `cp311-abi3`, 150 symbols; depends on cffi 2.1 (`_cffi_backend.pyd`, cp312, 178 symbols, libffi linked in) | PyO3's use of `PyType_FromSpec`, `PyObject_Vectorcall`, `_Py_Dealloc`; cffi's `PyCapsule`, buffer and callback trampolines |
| pydantic-core 2.50 | PyO3, cp312, 163 symbols | as above; heavy `PyObject_Vectorcall`, dict and str fast paths |
| orjson 3.13 | PyO3, cp312, 63 symbols | exact `dict`/`str`/`int` prefix layouts (orjson reads them) |
| lxml 6.1 | Cython | Cython's reads of `lv_tag`/`ob_digit`, `PyDict_GET_SIZE`, descriptor internals |
| psutil 7.2 | C, `cp37-abi3` | small surface; `PyGILState` |
| regex 2026.9 | C | `PyArg_ParseTuple` family, buffer API |
| markupsafe 3.0 | C speedups | small surface |
| charset-normalizer 3.5 | mypyc-compiled, 2 `.pyd`, 189 symbols | mypyc's generated code: native classes as C types with vtables, `PyType_FromSpec`-style creation, every dynamic operation through the C API |
| pdfplumber 0.11 | pure Python; depends on pdfminer.six (pure), pypdfium2, Pillow | nothing native of its own |
| pdfminer.six 20260107 | pure Python; depends on charset-normalizer, cryptography | nothing native of its own |
| pypdfium2 5.14 | pure Python; `pypdfium2_raw/bindings.py` is ctypesgen output loading `pdfium.dll` from the package directory | `_ctypes.pyd`, `libffi-8.dll`, `__file__`-relative DLL loading (10.4) |
| pyautogui 0.9.54 | pure Python, sdist only; Windows backend is `ctypes.windll.user32`; depends on pymsgbox 2.0 (pure, `ctypes` MessageBox), pytweening 1.2 (pure), pyscreeze 1.0 (pure; Pillow `ImageGrab`), pygetwindow 0.0.9 (pure, `ctypes`; depends on pyrect 0.2, pure), mouseinfo 0.1.3 (pure, `ctypes`; depends on pyperclip 1.11, pure, `ctypes` clipboard) | `_ctypes.pyd`, `ctypes.wintypes`, callback trampolines (`WINFUNCTYPE`), `ctypes.Structure` subclasses (C metaclass `_CDataMeta` with Python class bodies) |

What this does to the work order: `_ctypes.pyd` and its `_CDataMeta` metaclass path come first after the core, because pypdfium2, pyautogui and its five dependencies, pymsgbox and pyperclip are all ctypes over `_ctypes`; next the abi3 and inline-refcount paths (PyQt5/6, cryptography, psutil), since three of the set are limited-API wheels built for 3.8–3.11; then PyO3 and Cython surfaces; mypyc last, as its 189 symbols are mostly covered by the others (5 new over the stdlib/numpy/Pillow union). `tools/capi_usage.py` is run on exactly these wheels and its union (610 symbols) is the first implementation target.

## 16. Reproducibility, and moving runtime pieces into Python

The compiler is not self-hosted. What remains of the bootstrap idea is the reproducibility check: `coilc` output is a pure function of its inputs (section 13.3), verified in CI by compiling the conformance apps on two runners and comparing bytes. The Rust toolchain is pinned (`rust-toolchain.toml`) so `python312.dll` and `coilc.exe` are themselves reproducible across CI runs.

Later, pieces of the runtime that are naturally Python (parts of `builtins`, `_collections`, `itertools`, the import system's upper half) can be written in Python and compiled by `coilc` into the runtime image. The seam is `abi.toml`: a runtime function moves from Rust to compiled Python without changing its `coil_rt_*` signature. Not part of this design.

## 17. ARM64 and RISC-V readiness

| Piece | x86-64 | ARM64 (Windows) | RISC-V (Linux) |
| --- | --- | --- | --- |
| `backend/<arch>/` (rules, isel, regs, abi, encode, unwind) | first | AAPCS64 Windows variant (x18 reserved), ARM64 unwind codes (packed and unpacked `.xdata`), fixed 32-bit encoding | RV64GC, DWARF CFI |
| `coil-rt/src/arch/<arch>.rs` | TLS via TEB slot, probe loop, call shims | TLS via `x18` TEB, `stp/ldp` prologues | `tp` register, `.tdata` |
| Linker output | PE32+ `IMAGE_FILE_MACHINE_AMD64` | PE32+ `IMAGE_FILE_MACHINE_ARM64` | ELF64 (`link/elf.rs`), `libpython3.12.so` naming |
| Runtime DLL | `python312.dll` win_amd64 | `python312.dll` win_arm64 (cp312 ARM64 wheels exist) | `libpython3.12.so.1.0`, manylinux wheels |

Everything above `backend/lower.rs` and everything in the runtime outside `arch/` is shared. Generators as state machines (12.5) remove the one piece that usually needs per-architecture assembly. Cross-compilation follows from the linker writing files rather than calling Windows APIs: a Linux host can produce a Windows exe.

## 18. Risks

| Risk | Handling |
| --- | --- |
| C API breadth: 1,706 exports, 610 used by the v1 set | `tools/capi_usage.py` ranks symbols by real import tables; stubs raise rather than crash; the extension matrix is the acceptance test |
| Memory safety at the C boundary (pins, counted containers, barrier, ZCT, `tp_dealloc` order) | the nine-phase collection and the timing table in 9.3; the debug-heap runtime recomputes `heap_refs` from a heap walk and quarantines freed R objects; `_testcapi` conformance from step 1; the regime tests in 15.2 |
| Barrier cost on untyped heap stores | inference removes it for T-typed values; `Ext` barriers are one `inc`/`dec`; `Dynamic` barriers are a page-table lookup and a bit test on stores only, never on loads or stack moves; the benchmark tier measures a PyQt widget-churn case and a numpy-in-containers case |
| Extension memory held only through compiled-code objects waits for a collection | the one timing difference left (9.3); pacing backstop in 9.5; `gc.collect()` is available |
| Boxed-path performance below CPython 3.12's specialising interpreter | data inline caches, shapes, small-int cache, sealed-class direct calls; whole-program inference is what makes most code typed |
| Build time for whole-program compilation | per-module IR cache; parallel codegen per function in `coilc`; the compiler is native Rust |
| Grammar and stdlib drift between host and target | own parser from the pinned grammar; pinned `Lib/` sources; cp312 then cp313 only |
| Libraries that depend on bytecode or tracing (`pytest` assertion rewriting, `coverage`, debuggers, `dis`) | listed deviations; the compile report names the sites; tests run on CPython, the shipped app runs compiled |
| `exec`-generating libraries (`dataclasses`, `namedtuple`, `attrs`, `jinja2`, `mako`) | compile-time expansion for the two stdlib cases; `coil_dyn.dll` for the rest, bundled only when reachable |
| Inline-refcount abi3 wheels built for 3.8–3.11 (PyQt5, PyQt6, cryptography, psutil) | `ob_refcnt` semantics of 9.3 are the same for inline and function-mediated changes; the free-threaded roadmap build requires cp313t wheels, where every change but the atomic increment is a function call |
| Free-threaded roadmap | containers, allocator and thread states are built for it now (8.6, 10.3); the GIL build is a feature flag, not a different runtime |
| Scale of the work | `abi.toml`, the IR, the manifest and the regime split keep each part independently testable; no step in section 14 depends on an unfinished later step |
| Licensing of the embedded runtime | settled by decision 17: Apache-2.0 WITH LLVM-exception covers `coil-rt`, `coil_dyn.dll` and generated code, so compiled applications redistribute them without Coil's attribution and notice conditions |

## 19. Open

None. Every question raised while writing this design has been decided and is recorded in section 1.

//! The stdlib manifest (`native/manifest/stdlib.toml`, spec sections 10.6
//! and 11.1): every built-in of the pinned runtime is covered, the kinds are
//! the ones the spec assigns, and unsupported entries carry the reason the
//! build errors quote.

use coilc::world::manifest::{self, Kind, Manifest};

/// `sys.builtin_module_names` of the embeddable CPython 3.12.10 runtime: the
/// `_PyImport_Inittab` table of the pinned sources' PC/config.c, in table
/// order. Nothing may compile without every one of these resolvable.
const BUILTINS: [&str; 67] = [
    "_abc",
    "array",
    "_ast",
    "audioop",
    "binascii",
    "cmath",
    "errno",
    "faulthandler",
    "gc",
    "math",
    "nt",
    "_operator",
    "_signal",
    "_md5",
    "_sha1",
    "_sha2",
    "_sha3",
    "_blake2",
    "time",
    "_thread",
    "_tokenize",
    "_typing",
    "_statistics",
    "msvcrt",
    "_locale",
    "_tracemalloc",
    "_winapi",
    "_codecs",
    "_weakref",
    "_random",
    "_bisect",
    "_heapq",
    "_lsprof",
    "itertools",
    "_collections",
    "_symtable",
    "mmap",
    "_csv",
    "_sre",
    "winreg",
    "_struct",
    "_datetime",
    "_functools",
    "_json",
    "xxsubtype",
    "_xxsubinterpreters",
    "_xxinterpchannels",
    "zlib",
    "_multibytecodec",
    "_codecs_cn",
    "_codecs_hk",
    "_codecs_iso2022",
    "_codecs_jp",
    "_codecs_kr",
    "_codecs_tw",
    "marshal",
    "_imp",
    "builtins",
    "sys",
    "_warnings",
    "_string",
    "_io",
    "_pickle",
    "atexit",
    "_stat",
    "_opcode",
    "_contextvars",
];

/// CPython's own extension modules the embeddable zip ships, per spec
/// section 10.6.
const PYDS: [&str; 21] = [
    "_asyncio",
    "_bz2",
    "_ctypes",
    "_decimal",
    "_elementtree",
    "_hashlib",
    "_lzma",
    "_msi",
    "_multiprocessing",
    "_overlapped",
    "_queue",
    "_socket",
    "_sqlite3",
    "_ssl",
    "_uuid",
    "_wmi",
    "_zoneinfo",
    "pyexpat",
    "select",
    "unicodedata",
    "winsound",
];

fn manifest() -> Manifest {
    let (path, shims) = manifest::discover().unwrap();
    assert!(shims.is_dir(), "shims missing at {}", shims.display());
    manifest::Manifest::load(&path).unwrap()
}

#[test]
fn covers_every_pinned_builtin() {
    let m = manifest();
    for name in BUILTINS {
        assert!(
            m.get(name).is_some(),
            "built-in '{name}' is missing from the manifest"
        );
    }
}

#[test]
fn assigns_the_specified_kinds() {
    let m = manifest();
    let count = |kind: Kind| m.iter().filter(|(_, entry)| entry.kind == kind).count();
    assert_eq!(count(Kind::Native), 42, "native entries");
    assert_eq!(count(Kind::Shim), 15, "shim entries");
    assert_eq!(count(Kind::Compiled), 3, "compiled entries");
    assert_eq!(count(Kind::Pyd), 21, "pyd entries");
    assert_eq!(count(Kind::Unsupported), 10, "unsupported entries");
    assert_eq!(m.iter().count(), 91, "total entries");
    for name in PYDS {
        assert_eq!(
            m.get(name).map(|entry| entry.kind),
            Some(Kind::Pyd),
            "'{name}' is not a pyd entry"
        );
    }
    // The pure-Python fallbacks the stdlib itself ships, compiled from the
    // pinned Lib/ tree.
    for name in ["_py_abc", "_pydatetime", "_pyio"] {
        assert_eq!(
            m.get(name).map(|entry| entry.kind),
            Some(Kind::Compiled),
            "'{name}' is not a compiled entry"
        );
    }
}

#[test]
fn unsupported_entries_carry_a_reason() {
    let m = manifest();
    for (name, entry) in m.iter() {
        if entry.kind == Kind::Unsupported {
            let reason = entry.reason.as_deref().unwrap_or_default();
            assert!(!reason.is_empty(), "unsupported '{name}' has no reason");
        }
    }
    let symtable = m.get("_symtable").unwrap();
    assert_eq!(symtable.kind, Kind::Unsupported);
    assert!(
        symtable
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("symtable"),
        "reason: {:?}",
        symtable.reason
    );
}

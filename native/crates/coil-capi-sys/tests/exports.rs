//! The checked-in export lists: well-formed, sorted, and the size the spec
//! pins for 3.12.10 (native-compiler-spec.md section 10.1: 1,706 exports,
//! of which python3.dll forwards 867).

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn read(name: &str) -> Vec<(String, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    let text = fs::read_to_string(path).expect("export list is readable");
    let mut previous = String::new();
    text.lines()
        .enumerate()
        .map(|(number, line)| {
            let (symbol, kind) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("line {} is not '<name> <kind>': {line:?}", number + 1));
            assert!(matches!(kind, "func" | "data"), "unknown kind {kind:?}");
            assert!(
                symbol > previous.as_str(),
                "{name} is not sorted at {symbol:?}"
            );
            previous = symbol.to_owned();
            (symbol.to_owned(), kind.to_owned())
        })
        .collect()
}

#[test]
fn python312_exports() {
    let exports = read("exports.txt");
    assert_eq!(exports.len(), 1706);
    assert_eq!(
        exports.iter().filter(|(_, kind)| kind == "data").count(),
        211
    );
    let unique: HashSet<&String> = exports.iter().map(|(name, _)| name).collect();
    assert_eq!(unique.len(), exports.len(), "duplicate export names");
}

#[test]
fn python3_forwarder_exports() {
    let exports = read("python3-exports.txt");
    assert_eq!(exports.len(), 867);
    let python312_exports = read("exports.txt");
    let python312: HashSet<&String> = python312_exports.iter().map(|(name, _)| name).collect();
    // Every forwarder but PyOS_CheckStack, _Py_NegativeRefcount and
    // _Py_RefTotal (dropped from this python312.dll build, kinds resolved
    // through the pinned headers) names a python312.dll export.
    let missing: Vec<&String> = exports
        .iter()
        .map(|(name, _)| name)
        .filter(|name| !python312.contains(*name))
        .collect();
    assert_eq!(
        missing,
        ["PyOS_CheckStack", "_Py_NegativeRefcount", "_Py_RefTotal"]
    );
}

//! Oracle-driven tests for the `_sre` module (spec section 10.6) over
//! the regex corpus of CPython's `Lib/test/re_tests.py`. The table
//! under `tests/data` came from the pinned interpreter via
//! `tools/gen_sre_oracle.py`: every corpus row `re` compiles is
//! replayed through the Rust `_sre` surface with the exact
//! `_sre.compile` arguments CPython produced, and the `Pattern.search`
//! result is compared against the one CPython returned — the primary
//! run plus the variants `Lib/test/test_re.py`'s driver re-checks
//! (bytes and LOCALE on ASCII rows, IGNORECASE, UNICODE, and the
//! range-limited window). Corpus rows the parser rejects with
//! `re.error` never reach `_sre.compile` in CPython; they carry no
//! code in the table and nothing is replayed.

use std::fs;
use std::path::Path;
use std::sync::Once;

use core::ffi::c_char;

use coil_capi_sys::PyObject;
use python312::modules::_sre as sre;
use python312::object::singletons;
use python312::object::typeobject;
use python312::tstate;
use python312::types::{bytes as rt_bytes, dict, list, long, tuple, unicode};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

/// Undo the generator's escaping: backslash, tab, newline, carriage
/// return and lone surrogates' `\uXXXX`, in one decoder.
fn unescape(field: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut it = field.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c as u32);
            continue;
        }
        match it.next().expect("the generator escapes every backslash") {
            '\\' => out.push('\\' as u32),
            't' => out.push('\t' as u32),
            'n' => out.push('\n' as u32),
            'r' => out.push('\r' as u32),
            'u' => {
                let hex: String = it.by_ref().take(4).collect();
                out.push(u32::from_str_radix(&hex, 16).expect("four hex digits"));
            }
            other => panic!("unknown escape \\{other}"),
        }
    }
    out
}

/// Build a str object from code points.
fn str_object(chars: &[u32]) -> *mut PyObject {
    let text: String = chars
        .iter()
        .map(|&c| char::from_u32(c).expect("corpus strings are scalar"))
        .collect();
    // SAFETY: `text` lives across the call and the size is its byte
    // length.
    unsafe {
        unicode::PyUnicode_FromStringAndSize(
            if text.is_empty() {
                std::ptr::null()
            } else {
                text.as_ptr() as *const c_char
            },
            text.len() as isize,
        )
    }
}

/// Build a bytes object from code points (the corpus's ASCII re-encode).
fn bytes_object(chars: &[u32]) -> *mut PyObject {
    let data: Vec<u8> = chars.iter().map(|&c| c as u8).collect();
    // SAFETY: `data` lives across the call; NULL is passed for empty so
    // the shared empty object comes back.
    unsafe {
        rt_bytes::PyBytes_FromStringAndSize(
            if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr() as *const c_char
            },
            data.len() as isize,
        )
    }
}

/// The span table the oracle records: one optional `(start, end)` per
/// group.
type Spans = Vec<Option<(isize, isize)>>;

/// One `sre_re_tests.tsv` row.
struct Row {
    id: usize,
    variant: String,
    kind: String,
    is_bytes: bool,
    pattern: Vec<u32>,
    subject: Vec<u32>,
    code: Vec<u32>,
    groups: isize,
    flags: i32,
    groupindex: Vec<(Vec<u32>, isize)>,
    indexgroup: Vec<Option<Vec<u32>>>,
    /// The spans of group `0..=groups`, `None` for an unset group.
    spans: Option<Spans>,
    lastindex: Option<isize>,
    window: Option<(isize, isize)>,
}

fn parse_row(line: &str) -> Row {
    let f: Vec<&str> = line.split('\t').collect();
    assert_eq!(f.len(), 13, "every oracle row has 13 columns");
    let spans = if f[11].is_empty() {
        None
    } else {
        let (table, _lastindex) = f[11].split_once('|').expect("spans|lastindex");
        let spans: Vec<Option<(isize, isize)>> = table
            .split(',')
            .map(|entry| {
                if entry == "-" {
                    None
                } else {
                    let (a, b) = entry.split_once(':').expect("start:end");
                    Some((
                        a.parse::<isize>().expect("span start"),
                        b.parse::<isize>().expect("span end"),
                    ))
                }
            })
            .collect();
        Some(spans)
    };
    let lastindex_field = f[11].rsplit_once('|').map(|(_, li)| li);
    let lastindex = match lastindex_field {
        Some("-") => None,
        Some(li) => Some(li.parse::<isize>().expect("lastindex")),
        None => None,
    };
    Row {
        id: f[0].parse().expect("row id"),
        variant: f[1].to_string(),
        kind: f[2].to_string(),
        is_bytes: f[3] == "bytes",
        pattern: unescape(f[4]),
        code: if f[5].is_empty() {
            Vec::new()
        } else {
            f[5].split(',')
                .map(|w| w.parse().expect("code word"))
                .collect()
        },
        groups: if f[6].is_empty() {
            0
        } else {
            f[6].parse().expect("groups")
        },
        flags: if f[7].is_empty() {
            0
        } else {
            f[7].parse().expect("flags")
        },
        groupindex: if f[8].is_empty() {
            Vec::new()
        } else {
            f[8].split(';')
                .map(|entry| {
                    let (name, number) = entry.split_once('=').expect("name=number");
                    (unescape(name), number.parse().expect("group number"))
                })
                .collect()
        },
        indexgroup: if f[9].is_empty() {
            Vec::new()
        } else {
            f[9].split(';')
                .map(|name| {
                    if name.is_empty() {
                        None
                    } else {
                        Some(unescape(name))
                    }
                })
                .collect()
        },
        subject: unescape(f[10]),
        spans,
        lastindex,
        window: if f[12].is_empty() {
            None
        } else {
            let (pos, endpos) = f[12].split_once(',').expect("pos,endpos");
            Some((
                pos.parse().expect("window start"),
                endpos.parse().expect("window end"),
            ))
        },
    }
}

/// The pattern object of a row's recorded `_sre.compile` call.
fn compile_row(row: &Row) -> *mut PyObject {
    let pattern = if row.is_bytes {
        bytes_object(&row.pattern)
    } else {
        str_object(&row.pattern)
    };
    assert!(!pattern.is_null());
    let code = list::PyList_New(row.code.len() as isize);
    assert!(!code.is_null());
    // SAFETY: the steals target a fresh list.
    unsafe {
        for (i, &word) in row.code.iter().enumerate() {
            let item = long::PyLong_FromSsize_t(word as isize);
            assert!(!item.is_null());
            assert_eq!(list::PyList_SetItem(code, i as isize, item), 0);
        }
    }
    let groupindex = dict::PyDict_New();
    assert!(!groupindex.is_null());
    // SAFETY: the dict sets take owned keys and values.
    unsafe {
        for (name, number) in &row.groupindex {
            let key = str_object(name);
            let value = long::PyLong_FromSsize_t(*number);
            assert!(!key.is_null() && !value.is_null());
            assert_eq!(dict::PyDict_SetItem(groupindex, key, value), 0);
        }
    }
    let indexgroup = tuple::PyTuple_New(row.indexgroup.len() as isize);
    assert!(!indexgroup.is_null());
    // SAFETY: the steals target a fresh tuple.
    unsafe {
        for (i, name) in row.indexgroup.iter().enumerate() {
            let item = match name {
                Some(chars) => str_object(chars),
                None => singletons::none(),
            };
            assert!(!item.is_null());
            assert_eq!(tuple::PyTuple_SetItem(indexgroup, i as isize, item), 0);
        }
    }
    // SAFETY: the objects were built above from the row's fields.
    let compiled =
        unsafe { sre::compile(pattern, row.flags, code, row.groups, groupindex, indexgroup) };
    if compiled.is_null() {
        // SAFETY: the exception class's own name is its tp_name.
        let error = tstate::occurred();
        let name = unsafe {
            std::ffi::CStr::from_ptr(
                (*(error.cast::<coil_capi_sys::PyTypeObject>()))
                    .tp_name
                    .cast(),
            )
            .to_string_lossy()
            .into_owned()
        };
        panic!(
            "row {} ({}) failed to compile: {}",
            row.id, row.variant, name
        );
    }
    compiled
}

/// Replay one row's search through the Rust surface, encoded like the
/// oracle's span table. `groups` is the compiled pattern's group count:
/// the window rows that reuse the primary pattern carry no code of
/// their own.
fn replay(row: &Row, pattern: *mut PyObject, groups: isize) -> (Option<Spans>, Option<isize>) {
    let subject = if row.is_bytes {
        bytes_object(&row.subject)
    } else {
        str_object(&row.subject)
    };
    assert!(!subject.is_null());
    let (pos, endpos) = row.window.unwrap_or((0, isize::MAX));
    // SAFETY: the search reads the pattern's code and the subject only.
    let result = unsafe { sre::pattern_search(pattern, subject, pos, endpos) };
    assert!(
        tstate::occurred().is_null(),
        "row {} ({}) raised an error",
        row.id,
        row.variant
    );
    if result == singletons::none() {
        return (None, None);
    }
    let mut spans = Vec::new();
    for group in 0..=groups {
        let index = long::PyLong_FromSsize_t(group);
        assert!(!index.is_null());
        // SAFETY: the span read stays within the match.
        let span = unsafe { sre::match_span(result, index) };
        // SAFETY: our references from this harness.
        unsafe {
            let (start, end) = (
                long::PyLong_AsSsize_t(tuple::PyTuple_GetItem(span, 0)),
                long::PyLong_AsSsize_t(tuple::PyTuple_GetItem(span, 1)),
            );
            // An unset group is the oracle's "-", CPython's (-1, -1).
            spans.push(if start < 0 || end < 0 {
                None
            } else {
                Some((start, end))
            });
        }
    }
    // SAFETY: the lastindex read stays within the match.
    let lastindex = unsafe { sre::match_lastindex(result) };
    let lastindex = if lastindex == singletons::none() {
        None
    } else {
        // SAFETY: our reference.
        Some(unsafe { long::PyLong_AsSsize_t(lastindex) })
    };
    (Some(spans), lastindex)
}

/// One corpus tuple: its variant rows, keyed by variant.
struct CorpusRow {
    rows: Vec<Row>,
}

impl CorpusRow {
    /// Replay every variant and compare with the oracle. The primary
    /// row carries the pattern the `limited` window reuses.
    fn passes(&self) -> Result<(), String> {
        let primary = self
            .rows
            .iter()
            .find(|r| r.variant == "primary")
            .expect("every corpus row has a primary variant");
        if primary.kind == "syntax" {
            assert!(primary.code.is_empty(), "syntax rows carry no code");
            return Ok(());
        }
        let mut patterns: Vec<(String, *mut PyObject, isize)> = Vec::new();
        for row in &self.rows {
            if row.code.is_empty() {
                continue;
            }
            patterns.push((row.variant.clone(), compile_row(row), row.groups));
        }
        for row in &self.rows {
            let wanted = if row.code.is_empty() {
                "primary"
            } else {
                row.variant.as_str()
            };
            let (pattern, groups) = patterns
                .iter()
                .find(|(variant, _, _)| *variant == wanted)
                .map(|(_, pattern, groups)| (*pattern, *groups))
                .expect("the variant pattern exists");
            let (spans, lastindex) = replay(row, pattern, groups);
            let expected = match (row.kind.as_str(), &row.spans) {
                ("nomatch", None) => None,
                ("match", Some(table)) => Some((table.clone(), row.lastindex)),
                other => panic!("unknown oracle outcome {other:?}"),
            };
            let got = spans.map(|table| (table, lastindex));
            if got != expected {
                return Err(format!(
                    "row {} variant {}: oracle {:?} != replay {:?}",
                    row.id, row.variant, expected, got
                ));
            }
        }
        Ok(())
    }
}

#[test]
fn re_tests_corpus() {
    init();
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/sre_re_tests.tsv");
    let text = fs::read_to_string(&data).expect("the oracle table is checked in");
    let mut corpus: Vec<CorpusRow> = Vec::new();
    for line in text.lines() {
        let row = parse_row(line);
        if corpus.len() != row.id + 1 {
            assert_eq!(corpus.len(), row.id, "rows arrive grouped by id");
            corpus.push(CorpusRow { rows: Vec::new() });
        }
        corpus[row.id].rows.push(row);
    }
    let mut exercised = 0;
    let mut failures = Vec::new();
    for row in &corpus {
        if row.rows[0].kind == "syntax" {
            continue;
        }
        exercised += 1;
        if let Err(message) = row.passes() {
            failures.push(message);
        }
    }
    eprintln!(
        "re_tests.py corpus: {} of {} rows pass, {} fail",
        exercised - failures.len(),
        exercised,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} corpus rows diverge:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

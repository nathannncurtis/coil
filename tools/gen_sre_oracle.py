"""CPython 3.12 _sre oracle for the coil-rt tests (spec 10.6).

Captures the pinned interpreter's `re` pipeline over the regex corpus of
Lib/test/re_tests.py: for every corpus row that reaches `_sre.compile`,
the exact arguments Lib/re produced (pattern, flags, code array, group
count, group index) and the `Pattern.search` result CPython returns —
the primary run plus the variants test_re.py's driver re-checks (bytes
and LOCALE on ASCII-able rows, IGNORECASE, UNICODE, and the
range-limited window). Corpus rows the parser rejects with re.error
never reach `_sre.compile` in CPython and are recorded as such.

Writes tab-separated checked-in test data for
native/crates/coil-rt/tests/. This module refuses to run on anything
but CPython 3.12, the pinned target whose semantics the runtime ports.

Output file (under --out, default native/crates/coil-rt/tests/data):

  sre_re_tests.tsv    one row per (corpus row, variant)

Field encoding is shared with the Rust reader. Fields are separated by
literal tabs; inside a field a backslash escapes itself, tab, newline
and carriage return, and str fields escape lone surrogates as \\uXXXX.
Columns: id, variant, kind (syntax|nomatch|match), pattern flavour
(str|bytes), pattern, code words (comma-joined), groups, flags,
groupindex (name=nr;...), indexgroup names (;-joined, empty for None),
subject, spans (a:b,... with - for an unset group, then |lastindex or
-), and the search window (pos,endpos; empty for the full string).
"""

import argparse
import sys
from pathlib import Path

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_sre_oracle.py must run on CPython 3.12 (the pinned target)")

def esc(text):
    out = []
    for ch in text:
        if ch == "\\":
            out.append("\\\\")
        elif ch == "\t":
            out.append("\\t")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif 0xD800 <= ord(ch) <= 0xDFFF:
            out.append(f"\\u{ord(ch):04x}")
        else:
            out.append(ch)
    return "".join(out)


# ------------------------------------------------------------- capturing

class Recorder:
    """Wraps `_sre.compile` to record the arguments Lib/re produced."""

    def __init__(self):
        import _sre

        self._sre = _sre
        self._real = _sre.compile
        self.last = None
        _sre.compile = self._compile

    def close(self):
        self._sre.compile = self._real

    def _compile(self, pattern, flags, code, groups, groupindex, indexgroup):
        self.last = (pattern, flags, list(code), groups,
                     dict(groupindex), tuple(indexgroup))
        return self._real(pattern, flags, code, groups, groupindex, indexgroup)


def compile_pattern(recorder, pattern, flags):
    """`re._compiler.compile` with the `_sre.compile` call recorded."""
    from re import _compiler

    recorder.last = None
    return _compiler.compile(pattern, flags)


def search_spans(compiled, subject, pos=None, endpos=None):
    """The span table test_re.py's driver compares against."""
    if pos is None:
        result = compiled.search(subject)
    else:
        result = compiled.search(subject, pos, endpos)
    if result is None:
        return "nomatch", ""
    spans = []
    for index in range(compiled.groups + 1):
        begin, end = result.span(index)
        if begin < 0:
            spans.append("-")
        else:
            spans.append(f"{begin}:{end}")
    lastindex = result.lastindex
    return "match", ",".join(spans) + f"|{lastindex if lastindex is not None else '-'}"


def compile_fields(args):
    pattern, flags, code, groups, groupindex, indexgroup = args
    gix = ";".join(f"{esc(name)}={number}" for name, number in groupindex.items())
    ixg = ";".join("" if name is None else esc(name) for name in indexgroup)
    flavour = "bytes" if isinstance(pattern, bytes) else "str"
    text = pattern.decode("ascii") if isinstance(pattern, bytes) else pattern
    return (flavour, esc(text),
            ",".join(str(int(word)) for word in code), str(groups), str(int(flags)),
            gix, ixg)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, required=True,
                        help="path of Lib/test/re_tests.py")
    parser.add_argument("--out", type=Path,
                        default=Path("native/crates/coil-rt/tests/data"))
    args = parser.parse_args()

    namespace = {}
    source = args.corpus.read_text(encoding="utf-8")
    exec(compile(source, str(args.corpus), "exec"), namespace)
    tests = namespace["tests"]
    SUCCEED, FAIL, SYNTAX_ERROR = (namespace[name] for name in
                                   ("SUCCEED", "FAIL", "SYNTAX_ERROR"))

    import re

    recorder = Recorder()
    rows = []
    try:
        for row_id, t in enumerate(tests):
            pattern, subject, outcome = t[0], t[1], t[2]
            if outcome == SYNTAX_ERROR:
                try:
                    compile_pattern(recorder, pattern, 0)
                except re.error:
                    rows.append([str(row_id), "primary", "syntax", "str",
                                 esc(pattern), "", "", "", "", "", esc(subject), "", ""])
                    continue
                sys.exit(f"corpus row {row_id} expected a syntax error: {t!r}")

            compiled = compile_pattern(recorder, pattern, 0)
            primary_args = recorder.last
            kind, spans = search_spans(compiled, subject)
            rows.append([str(row_id), "primary", kind, *compile_fields(primary_args),
                         esc(subject), spans, ""])
            if kind == "nomatch":
                # The driver stops at the failed match.
                if outcome != FAIL:
                    sys.exit(f"corpus row {row_id} failed unexpectedly: {t!r}")
                continue
            if outcome != SUCCEED:
                sys.exit(f"corpus row {row_id} succeeded unexpectedly: {t!r}")

            # ASCII pattern and subject: the bytes and LOCALE recompiles.
            try:
                bpat = bytes(pattern, "ascii")
                bsubject = bytes(subject, "ascii")
            except UnicodeEncodeError:
                pass
            else:
                for variant, flags in (("bytes", 0), ("locale", re.LOCALE)):
                    bcompiled = compile_pattern(recorder, bpat, flags)
                    bkind, bspans = search_spans(bcompiled, bsubject)
                    rows.append([str(row_id), variant, bkind,
                                 *compile_fields(recorder.last),
                                 esc(bsubject.decode("ascii")), bspans, ""])

            # The range-limited window, unless \B sits at an edge.
            start, end = compiled.search(subject).span(0)
            if pattern[:2] != "\\B" and pattern[-2:] != "\\B":
                limited = compile_pattern(recorder, pattern, 0)
                lkind, lspans = search_spans(limited, subject, start, end + 1)
                rows.append([str(row_id), "limited", lkind, "str",
                             esc(pattern), "", "", "", "", "", esc(subject),
                             lspans, f"{start},{end + 1}"])

            for variant, flags in (("ignorecase", re.IGNORECASE),
                                   ("unicode", re.UNICODE)):
                vcompiled = compile_pattern(recorder, pattern, flags)
                vkind, vspans = search_spans(vcompiled, subject)
                rows.append([str(row_id), variant, vkind,
                             *compile_fields(recorder.last),
                             esc(subject), vspans, ""])
    finally:
        recorder.close()

    args.out.mkdir(parents=True, exist_ok=True)
    target = args.out / "sre_re_tests.tsv"
    with target.open("w", encoding="utf-8", newline="\n") as handle:
        for row in rows:
            handle.write("\t".join(row) + "\n")
    print(f"{target}: {len(rows)} rows over {len(tests)} corpus tuples")


if __name__ == "__main__":
    main()

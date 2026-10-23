"""Port CPython 3.12's character-type database to Rust for coil-rt.

Parses the pinned source tree's Objects/unicodetype_db.h (generated upstream
by Tools/unicode/makeunicodedata.py 3.3) and writes
native/crates/coil-rt/src/types/unicode_data.rs with the same records, the
two-level type index, the extended-case array and the numeric/whitespace/
linebreak switch tables, so the runtime's str methods classify and case-map
characters exactly as CPython 3.12 does (spec section 8.2).

The property masks come from Tools/unicode/makeunicodedata.py, which
assigns them; the source tree comes from tools/fetch_cpython_src.py; run:

    python3.12 tools/gen_unicode_data.py
"""

import re
import subprocess
import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
OUT = TOOLS.parent / "native/crates/coil-rt/src/types/unicode_data.rs"

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_unicode_data.py must run on CPython 3.12 (the pinned target)")


def source_root() -> Path:
    root = subprocess.run(
        [sys.executable, str(TOOLS / "fetch_cpython_src.py"), "--print-root"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return Path(root)


def parse_masks(source: Path) -> dict[str, int]:
    text = source.read_text(encoding="ascii")
    return {name: int(value, 16) for name, value in re.findall(
        r"^([A-Z_]+)_MASK = (0x[0-9A-Fa-f]+)$", text, re.M)}


def parse_db(path: Path) -> tuple:
    text = path.read_text(encoding="ascii")

    def array(name: str) -> list[str]:
        match = re.search(rf"{name}\[\] = \{{(.*?)\}};", text, re.S)
        if match is None:
            raise ValueError(f"{name} not found in {path}")
        items = match.group(1).replace("\n", " ").replace("{", " ").replace("}", " ")
        return [item.strip() for item in items.split(",") if item.strip()]

    records_body = re.search(r"_PyUnicode_TypeRecords\[\] = \{(.*?)\};", text, re.S).group(1)
    records = [tuple(int(f) for f in fields.split(","))
               for fields in re.findall(r"\{([^{}]*)\}", records_body)]
    extended = [int(item) for item in array("_PyUnicode_ExtendedCase")]
    shift = int(re.search(r"#define SHIFT (\d+)", text).group(1))
    index1 = [int(item) for item in array("index1")]
    index2 = [int(item) for item in array("index2")]

    # The switch tables at the tail of the header: `_PyUnicode_ToNumeric`
    # groups `case 0x...:` labels under `return (double) <expr>;`, where the
    # expression is a C double literal or division.
    numeric: list[tuple[int, str]] = []
    body = re.search(r"double _PyUnicode_ToNumeric\(Py_UCS4 ch\)\s*\{(.*?)\n\}", text, re.S).group(1)
    for block in re.finditer(r"((?:case 0x[0-9A-Fa-f]+:\s*)+)return \(double\) ([^;]+);", body):
        for label in re.finditer(r"case (0x[0-9A-Fa-f]+):", block.group(1)):
            numeric.append((int(label.group(1), 16), block.group(2).strip()))

    def switch_cases(function: str) -> list[int]:
        body = re.search(rf"int {function}\((?:const )?Py_UCS4 ch\)\s*\{{(.*?)\n\}}", text, re.S).group(1)
        return [int(label, 16) for label in re.findall(r"case (0x[0-9A-Fa-f]+):", body)]

    whitespace = switch_cases("_PyUnicode_IsWhitespace")
    linebreak = switch_cases("_PyUnicode_IsLinebreak")
    return records, extended, shift, index1, index2, numeric, whitespace, linebreak


def main() -> int:
    root = source_root()
    masks = parse_masks(root / "Tools" / "unicode" / "makeunicodedata.py")
    (records, extended, shift, index1, index2,
     numeric, whitespace, linebreak) = parse_db(root / "Objects" / "unicodetype_db.h")

    lines: list[str] = []
    w = lines.append
    w("//! CPython 3.12's character-type database, ported from the pinned")
    w("//! source tree's `Objects/unicodetype_db.h` by `tools/gen_unicode_data.py`")
    w("//! (regenerate with `python3.12 tools/gen_unicode_data.py`). The record")
    w("//! fields, masks, index split and extended-case encoding are upstream")
    w("//! `makeunicodedata.py` 3.3's; `types/unicode.rs` reads them through the")
    w("//! same accessors as CPython's `Objects/unicodectype.c`.")
    w("")
    w("// Numbers keep the exact C spelling of the generated tables.")
    w("#![allow(non_upper_case_globals)]")
    w("")
    w("/// Character-property bits, upstream's masks (`unicodectype.c`).")
    w("pub mod flags {")
    for name in sorted(masks):
        w(f"    pub const {name}: u32 = 0x{masks[name]:X};")
    w("}")
    w("")
    w("/// One `_PyUnicode_TypeRecord`: case deltas or extended-case offsets,")
    w("/// decimal/digit values and the property bits.")
    w("#[derive(Clone, Copy)]")
    w("pub struct TypeRecord {")
    w("    pub upper: i32,")
    w("    pub lower: i32,")
    w("    pub title: i32,")
    w("    pub decimal: u8,")
    w("    pub digit: u8,")
    w("    pub flags: u32,")
    w("}")
    w("")
    w(f"pub const TYPE_RECORDS: [TypeRecord; {len(records)}] = [")
    for upper, lower, title, decimal, digit, flag in records:
        w(f"    TypeRecord {{ upper: {upper}, lower: {lower}, title: {title}, "
          f"decimal: {decimal}, digit: {digit}, flags: {flag} }},")
    w("];")
    w("")
    w(f"pub const EXTENDED_CASE: [u32; {len(extended)}] = [")
    for row in range(0, len(extended), 16):
        w("    " + ", ".join(str(c) for c in extended[row:row + 16]) + ",")
    w("];")
    w("")
    w(f"pub const SHIFT: u32 = {shift};")
    for name, index in (("INDEX1", index1), ("INDEX2", index2)):
        w(f"pub const {name}: [u16; {len(index)}] = [")
        for row in range(0, len(index), 24):
            w("    " + ", ".join(str(c) for c in index[row:row + 24]) + ",")
        w("];")
    w("")
    w("/// `_PyUnicode_ToNumeric`'s switch: code point to the C double")
    w("/// expression (literals and divisions only), evaluated at the call site.")
    w(f"pub const NUMERIC: [(u32, f64); {len(numeric)}] = [")
    for code, expr in numeric:
        w(f"    ({code}, {expr.removeprefix('(double)').strip()}),")
    w("];")
    w("")
    for name, values in (("WHITESPACE", whitespace), ("LINEBREAK", linebreak)):
        w(f"pub const {name}: [u32; {len(values)}] = [")
        w("    " + ", ".join(f"0x{c:04X}" for c in values) + ",")
        w("];")
    w("")
    w("/// `gettyperecord`: the two-level index walk.")
    w("pub fn record(code: u32) -> &'static TypeRecord {")
    w("    if code >= 0x110000 {")
    w("        return &TYPE_RECORDS[0];")
    w("    }")
    w("    let index = INDEX1[(code >> SHIFT) as usize] as usize;")
    w("    let index = INDEX2[(index << SHIFT) + (code as usize & ((1 << SHIFT) - 1))];")
    w("    &TYPE_RECORDS[index as usize]")
    w("}")
    w("")
    OUT.write_text("\n".join(lines) + "\n", encoding="ascii")
    print(f"wrote {OUT} ({len(records)} records, {len(extended)} extended cases, "
          f"{len(numeric)} numeric code points)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

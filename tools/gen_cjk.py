"""Rust table generator for the CJK codecs (spec section 10.6).

Parses the pinned CPython source's mapping headers under
Modules/cjkcodecs/ — the same arrays Modules/_codecs_*.c compile
against — and emits the checked-in Rust tables the coil-rt multibyte
engine reads: flat value arrays plus the 256-entry index arrays of
`struct dbcs_index` / `struct unim_index` / `struct widedbcs_index`,
the JIS X 0213 pair encode table, the gb18030 four-byte range table,
and the big5hkscs plane hints. Values are copied verbatim, sentinels
included (U/N/M/D of cjkcodecs.h), so the engine tests the exact
membership decisions CPython makes.

Output files (under --out, default
native/crates/coil-rt/src/modules/cjk_data):

  cn.rs, jp.rs, kr.rs, tw.rs, hk.rs

Null index entries ({0,0,0} in the C source) are emitted as
(offset, bottom, top) triples with bottom > top, a shape a real entry
never has; the generator fails loudly if a source entry ever collides
with that encoding. A real single-entry bucket at low byte 0 reads
{__x+off,0,0} and stays valid because only the null rows are
rewritten. Standard library only.
"""

import argparse
import pathlib
import re
import sys

UNIINV = 0xFFFE
NOCHAR = 0xFFFF
MULTIC = 0xFFFE
DBCINV = 0xFFFD

SENTINELS = {"U": UNIINV, "N": NOCHAR, "M": MULTIC, "D": DBCINV}
SENTINEL_NAMES = {"U": "UNIINV", "N": "NOCHAR", "M": "MULTIC", "D": "DBCINV"}

FLAT_RE = re.compile(
    r"static const (ucs2_t|DBCHAR|Py_UCS4|unsigned char) "
    r"(_?[A-Za-z0-9_]+)\[(?:\d+)?\] *= *\{",
    re.DOTALL,
)
INDEX_RE = re.compile(
    r"static const struct (dbcs_index|unim_index|widedbcs_index) "
    r"([A-Za-z0-9_]+)\[256\] *= *\{",
    re.DOTALL,
)
PAIR_RE = re.compile(
    r"static const struct pair_encodemap ([A-Za-z0-9_]+)"
    r"\[(\d+|JISX0213_ENCPAIRS)\] *= *\{",
    re.DOTALL,
)
RANGES_RE = re.compile(
    r"static const struct _gb18030_to_unibmp_ranges *\{[^{}]*\} "
    r"([A-Za-z0-9_]+)\[\] *= *\{",
    re.DOTALL,
)
PAIRS_DEFINE_RE = re.compile(r"#define JISX0213_ENCPAIRS (\d+)")


def body_of(text, start):
    """The brace-balanced body starting at the '{' at `start`."""
    depth = 0
    for i in range(start, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1 : i]
    raise ValueError("unterminated initializer")


def flat_values(body):
    """The flat initializer's cells: ints, hex ints and U/N/M/D kept as
    their spelling so the emitted name matches the header's intent
    (U and M share the value 0xFFFE)."""
    tokens = [t for t in re.split(r"[,\s]+", body.strip()) if t]
    return [t if t in SENTINELS else int(t, 0) for t in tokens]


def index_entries(body):
    """The 256 {offset,bottom,top} (or null) entries of an index array."""
    groups = re.findall(r"\{([^{}]*)\}", body)
    if len(groups) != 256:
        raise ValueError(f"index array holds {len(groups)} entries, not 256")
    entries = []
    for group in groups:
        parts = [p.strip() for p in re.sub(r"\s+", "", group).split(",")]
        if parts == ["0", "0", "0"]:
            entries.append(None)
            continue
        m = re.fullmatch(r"__?[A-Za-z0-9_]+\+(\d+)", parts[0])
        if not m:
            raise ValueError(f"unexpected index entry {group!r}")
        offset = int(m.group(1))
        bottom, top = int(parts[1], 0), int(parts[2], 0)
        if bottom > top:
            raise ValueError(
                f"real index entry {group!r} collides with the null encoding"
            )
        entries.append((offset, bottom, top))
    return entries


def rust_values(values, per_line, rust_type="u16"):
    out = []
    for i in range(0, len(values), per_line):
        cells = []
        for v in values[i : i + per_line]:
            name = SENTINEL_NAMES.get(v) if v in SENTINELS else None
            if name and rust_type != "u16":
                # the wide (Py_UCS4) arrays hold the same sentinels
                cells.append(f"{name} as {rust_type}")
            else:
                cells.append(name if name else str(v))
        out.append("    " + ",".join(cells) + ",")
    return "\n".join(out)


def write_module(path, lines):
    """Write one generated file, importing the sentinel consts it uses."""
    text = "\n".join(lines) + "\n"
    names = [
        n
        for n in ("MULTIC", "NOCHAR", "UNIINV")
        if re.search(rf"(?<![A-Za-z0-9_]){n}(?![A-Za-z0-9_])", text)
    ]
    if names:
        pos = text.index("pub static")
        text = text[:pos] + "use super::{" + ",".join(names) + "};\n" + text[pos:]
    path.write_text(text)


def rust_index(entries):
    out = []
    for i in range(0, 256, 6):
        cells = []
        for entry in entries[i : i + 6]:
            if entry is None:
                cells.append("(0,1,0)")
            else:
                offset, bottom, top = entry
                cells.append(f"({offset},{bottom},{top})")
        out.append("    " + ",".join(cells) + ",")
    return "\n".join(out)


class Tables:
    """The parsed declarations of the mapping headers."""

    def __init__(self):
        self.flat = {}  # array name -> (ctype, values)
        self.index = {}  # table name -> (kind, entries)
        self.pairs = {}  # table name -> [(uniseq, code)]
        self.ranges = []  # [(first, last, base)]
        self.pairs_define = None

    def parse(self, text):
        self.pairs_define = PAIRS_DEFINE_RE.search(text)
        for m in FLAT_RE.finditer(text):
            self.flat[m.group(2)] = (
                m.group(1),
                flat_values(body_of(text, m.end() - 1)),
            )
        for m in INDEX_RE.finditer(text):
            self.index[m.group(2)] = (
                m.group(1),
                index_entries(body_of(text, m.end() - 1)),
            )
        for m in PAIR_RE.finditer(text):
            name, count = m.group(1), m.group(2)
            pairs = []
            for group in re.findall(r"\{([^{}]*)\}", body_of(text, m.end() - 1)):
                uni, code = (p.strip() for p in group.split(","))
                pairs.append((int(uni, 0), int(code, 0)))
            if count != "JISX0213_ENCPAIRS":
                declared = int(count)
                if len(pairs) != declared:
                    raise ValueError(f"{name}: {len(pairs)} pairs, declared {declared}")
            self.pairs[name] = pairs
        for m in RANGES_RE.finditer(text):
            for group in re.findall(r"\{([^{}]*)\}", body_of(text, m.end() - 1)):
                first, last, base = (p.strip() for p in group.split(","))
                self.ranges.append((int(first, 0), int(last, 0), int(base, 0)))


HEADER = """\
// Generated by tools/gen_cjk.py from Modules/cjkcodecs/{source}; do not
// edit. The values are the pinned 3.12.10 arrays verbatim, sentinels
// included, so membership tests match CPython exactly.
"""


def flat_decl(rust_name, c_name, tables, rust_type, per_line=14):
    _, values = tables.flat[c_name]
    return (
        f"pub static {rust_name}: &[{rust_type}] = &[\n"
        f"{rust_values(values, per_line, rust_type)}\n];"
    )


def index_decl(rust_table, rust_data, kind, entries):
    return "\n".join(
        [
            f"pub static {rust_table}: {kind} = {kind} {{",
            f"    data: {rust_data},",
            "    index: &[",
            rust_index(entries),
            "    ],",
            "};",
        ]
    )


def table_decls(tables, specs, lines):
    """`specs`: (index name, flat name, table kind, rust prefix) rows."""
    for c_index, c_flat, kind, prefix in specs:
        ctype, _ = tables.flat[c_flat]
        rust_type = "u32" if ctype == "Py_UCS4" else "u16"
        lines.append(flat_decl(prefix + "_DATA", c_flat, tables, rust_type))
        lines.append("")
        _, entries = tables.index[c_index]
        lines.append(index_decl(prefix, prefix + "_DATA", kind, entries))
        lines.append("")


def emit_cn(t, out):
    lines = [HEADER.format(source="mappings_cn.h"), "use super::{DecMap, EncMap};\n"]
    table_decls(
        t,
        [
            ("gb2312_decmap", "__gb2312_decmap", "DecMap", "GB2312_DEC"),
            ("gbkext_decmap", "__gbkext_decmap", "DecMap", "GBKEXT_DEC"),
            ("gbcommon_encmap", "__gbcommon_encmap", "EncMap", "GBCOMMON_ENC"),
            ("gb18030ext_decmap", "__gb18030ext_decmap", "DecMap", "GB18030EXT_DEC"),
            ("gb18030ext_encmap", "__gb18030ext_encmap", "EncMap", "GB18030EXT_ENC"),
        ],
        lines,
    )
    rows = ",\n".join(
        "    (" + ",".join(str(v) for v in row) + ",)" for row in t.ranges
    )
    lines.append(
        "/// `gb18030_to_unibmp_ranges` with its terminator row, which the\n"
        "/// four-byte decoder walks.\n"
        "pub static GB18030_RANGES: &[(u32, u32, u16)] = &[\n" + rows + ",\n];"
    )
    write_module(out / "cn.rs", lines)


def emit_jp(t, pair, out):
    lines = [
        HEADER.format(source="mappings_jp.h and mappings_jisx0213_pair.h"),
        "use super::{DecMap, EncMap, PairEncMap, WideDecMap};\n",
    ]
    table_decls(
        t,
        [
            ("jisx0208_decmap", "__jisx0208_decmap", "DecMap", "JISX0208_DEC"),
            ("jisx0212_decmap", "__jisx0212_decmap", "DecMap", "JISX0212_DEC"),
            ("jisxcommon_encmap", "__jisxcommon_encmap", "EncMap", "JISXCOMMON_ENC"),
            ("cp932ext_decmap", "__cp932ext_decmap", "DecMap", "CP932EXT_DEC"),
            ("cp932ext_encmap", "__cp932ext_encmap", "EncMap", "CP932EXT_ENC"),
            ("jisx0213_1_bmp_decmap", "__jisx0213_1_bmp_decmap", "DecMap", "JISX0213_1_BMP_DEC"),
            ("jisx0213_2_bmp_decmap", "__jisx0213_2_bmp_decmap", "DecMap", "JISX0213_2_BMP_DEC"),
            ("jisx0213_bmp_encmap", "__jisx0213_bmp_encmap", "EncMap", "JISX0213_BMP_ENC"),
            ("jisx0213_1_emp_decmap", "__jisx0213_1_emp_decmap", "DecMap", "JISX0213_1_EMP_DEC"),
            ("jisx0213_2_emp_decmap", "__jisx0213_2_emp_decmap", "DecMap", "JISX0213_2_EMP_DEC"),
            ("jisx0213_emp_encmap", "__jisx0213_emp_encmap", "EncMap", "JISX0213_EMP_ENC"),
        ],
        lines,
    )
    _, entries = pair.index["jisx0213_pair_decmap"]
    lines.append(flat_decl("JISX0213_PAIR_DEC_DATA", "__jisx0213_pair_decmap", pair, "u32", 8))
    lines.append("")
    lines.append(
        index_decl("JISX0213_PAIR_DEC", "JISX0213_PAIR_DEC_DATA", "WideDecMap", entries)
    )
    lines.append("")
    rows = ",\n".join(
        f"    (0x{uni:x},0x{code:x})" for uni, code in pair.pairs["jisx0213_pair_encmap"]
    )
    count = pair.pairs_define.group(1)
    lines.append(
        f"/// `JISX0213_ENCPAIRS` = {count}.\n"
        f"pub const JISX0213_ENCPAIRS: usize = {count};\n"
        "pub static JISX0213_PAIR_ENC: PairEncMap = PairEncMap { pairs: &[\n"
        + rows
        + ",\n] };"
    )
    write_module(out / "jp.rs", lines)


def emit_kr(t, out):
    lines = [HEADER.format(source="mappings_kr.h"), "use super::{DecMap, EncMap};\n"]
    table_decls(
        t,
        [
            ("ksx1001_decmap", "__ksx1001_decmap", "DecMap", "KSX1001_DEC"),
            ("cp949ext_decmap", "__cp949ext_decmap", "DecMap", "CP949EXT_DEC"),
            ("cp949_encmap", "__cp949_encmap", "EncMap", "CP949_ENC"),
        ],
        lines,
    )
    write_module(out / "kr.rs", lines)


def emit_tw(t, out):
    lines = [HEADER.format(source="mappings_tw.h"), "use super::{DecMap, EncMap};\n"]
    table_decls(
        t,
        [
            ("big5_decmap", "__big5_decmap", "DecMap", "BIG5_DEC"),
            ("big5_encmap", "__big5_encmap", "EncMap", "BIG5_ENC"),
            ("cp950ext_decmap", "__cp950ext_decmap", "DecMap", "CP950EXT_DEC"),
            ("cp950ext_encmap", "__cp950ext_encmap", "EncMap", "CP950EXT_ENC"),
        ],
        lines,
    )
    write_module(out / "tw.rs", lines)


def emit_hk(t, out):
    lines = [HEADER.format(source="mappings_hk.h"), "use super::{DecMap, EncMap};\n"]
    table_decls(
        t,
        [
            ("big5hkscs_decmap", "__big5hkscs_decmap", "DecMap", "BIG5HKSCS_DEC"),
            ("big5hkscs_bmp_encmap", "__big5hkscs_bmp_encmap", "EncMap", "BIG5HKSCS_BMP_ENC"),
            (
                "big5hkscs_nonbmp_encmap",
                "__big5hkscs_nonbmp_encmap",
                "EncMap",
                "BIG5HKSCS_NONBMP_ENC",
            ),
        ],
        lines,
    )
    for name in ("big5hkscs_phint_0", "big5hkscs_phint_12130", "big5hkscs_phint_21924"):
        _, values = t.flat[name]
        lines.append(f"pub static {name.upper()}: &[u8] = &[\n{rust_values(values, 24)}\n];")
        lines.append("")
    write_module(out / "hk.rs", lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("cjkcodecs", help="pinned Modules/cjkcodecs directory")
    ap.add_argument(
        "--out",
        default="native/crates/coil-rt/src/modules/cjk_data",
        help="output directory (default: %(default)s)",
    )
    args = ap.parse_args()
    src = pathlib.Path(args.cjkcodecs)
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    def table(header):
        t = Tables()
        t.parse((src / f"{header}.h").read_text())
        return t

    pair = Tables()
    pair.parse((src / "mappings_jisx0213_pair.h").read_text())
    emit_cn(table("mappings_cn"), out)
    emit_jp(table("mappings_jp"), pair, out)
    emit_kr(table("mappings_kr"), out)
    emit_tw(table("mappings_tw"), out)
    emit_hk(table("mappings_hk"), out)

    for f in ("cn", "jp", "kr", "tw", "hk"):
        path = out / f"{f}.rs"
        print(f"{path}: {path.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())

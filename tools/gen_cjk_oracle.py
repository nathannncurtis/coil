"""CPython 3.12 multibyte codec oracle for the coil-rt CJK tests
(spec 10.6).

Captures the pinned interpreter's behaviour for the `_codecs_*`
multibyte modules: the per-module `getcodec` lookups, the
`MultibyteCodec.encode`/`decode` one-shot methods (the `(output,
consumed)` tuples), and `MultibyteIncrementalEncoder`/`Decoder`
op sequences (encode/decode/getstate/setstate/reset). Writes
tab-separated checked-in test data for native/crates/coil-rt/tests/.
This module refuses to run on anything but CPython 3.12, the pinned
target whose semantics the runtime ports.

Output files (under --out, default native/crates/coil-rt/tests/data):

  cjk_getcodec.tsv      per-module getcodec hits and misses
  cjk_encode.tsv        MultibyteCodec.encode over text and handlers
  cjk_decode.tsv        MultibyteCodec.decode over byte inputs
  cjk_incremental.tsv   incremental encoder/decoder op sequences

Field encoding is shared with gen_codec_oracle.py (tabs separate
fields, a backslash escapes itself, tab, newline and carriage return,
str fields escape lone surrogates as \\uXXXX, bytes fields map each
remaining byte b to chr(b) when 32 <= b <= 126 else \\xHH, results are
tagged s:/B:/i:/b:/T:/L: or e:<Type>:<message>) with one addition:
`|` escapes as \\p, so the incremental op fields can split on `|`.
`namereplace` stays out of the handler lists: the runtime's registry
for it arrives with unicodedata, and its absence is recorded in the
PR's Choices instead of being asserted here.
"""

import os
import subprocess
import sys
from pathlib import Path

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_cjk_oracle.py must run on CPython 3.12 (the pinned target)")

# ---------------------------------------------------------------- escaping


def esc(text):
    out = []
    for ch in text:
        if ch == "\\":
            out.append("\\\\")
        elif ch == "|":
            out.append("\\p")
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


def enc_bytes(data):
    parts = []
    for b in data:
        if b == 0x5C:
            parts.append("\\\\")
        elif b == 0x7C:
            parts.append("\\p")
        elif b == 0x09:
            parts.append("\\t")
        elif b == 0x0A:
            parts.append("\\n")
        elif b == 0x0D:
            parts.append("\\r")
        elif 0x20 <= b <= 0x7E:
            parts.append(chr(b))
        else:
            parts.append(f"\\x{b:02x}")
    return "".join(parts)


def enc_result(value):
    if value is None:
        return "T:"
    if isinstance(value, bool):
        return "b:1" if value else "b:0"
    if isinstance(value, int):
        return f"i:{value}"
    if isinstance(value, bytes):
        return "B:" + enc_bytes(value)
    if isinstance(value, (list, tuple)):
        parts = [f"L:{len(value)}"]
        parts.extend(
            ("s:" + esc(item))
            if isinstance(item, str)
            else ("B:" + enc_bytes(item))
            if isinstance(item, bytes)
            else enc_result(item)
            if isinstance(item, (list, tuple))
            else f"i:{item}"
            for item in value
        )
        return "\t".join(parts)
    return "s:" + esc(value)


def decode_result(live):
    if live.startswith("E:"):
        name, _, message = live[2:].partition(":")
        return f"e:{name}:{esc(eval(message))}"
    return enc_result(eval(live))


# ------------------------------------------------------------------ codecs

# module key -> (import name, codec names)
MODULES = {
    "cn": ("_codecs_cn", ["gb2312", "gbk", "gb18030", "hz"]),
    "hk": ("_codecs_hk", ["big5hkscs"]),
    "iso2022": ("_codecs_iso2022", [
        "iso2022_jp", "iso2022_jp_1", "iso2022_jp_2", "iso2022_jp_2004",
        "iso2022_jp_3", "iso2022_jp_ext", "iso2022_kr",
    ]),
    "jp": ("_codecs_jp", [
        "cp932", "euc_jis_2004", "euc_jisx0213", "euc_jp",
        "shift_jis", "shift_jis_2004", "shift_jisx0213",
    ]),
    "kr": ("_codecs_kr", ["cp949", "euc_kr", "johab"]),
    "tw": ("_codecs_tw", ["big5", "cp950"]),
}

ALL_ENCODINGS = sorted(
    name for _, names in MODULES.values() for name in names
)

# ------------------------------------------------------------------ inputs

# Every codec sees these; unmappable characters and undecodable bytes
# are as valuable as hits, since the pinned behaviour is the oracle.
SHARED_TEXTS = [
    "",
    "a",
    "\u4e2d\u6587",
    "a\u4e2db",
    "\ufffd",
    "\U0001f600",
    "\U00020000",
    "\u00d7",
    "\u00a5\u203e",
    "\uff76",
    "\u304b\u309a",
    "\u9b1d",
    "\u9b1c",
    "\u20ac",
    "\ue000",
    "\u301c\uff5e",
    "\x0a\x0d\x00",
    "a\ud800b",
    "a\ufffdb",
]

EXTRA_TEXTS = {
    "gb2312": ["\u4e2d\u6587\u00d7"],
    "gbk": ["\u20ac\ue4c6", "\u4e02"],
    "gb18030": ["\U0001f600\u20ac", "\ue15f"],
    "hz": ["a~{\u4e2d\u6587}b", "~~a", "~", "~{", "x~}y"],
    "big5": ["\u4e2d\u9ec4", "\uc2dc"],
    "cp950": ["\u4e2d\u9ec4", "\u5011"],
    "big5hkscs": ["\u5024\u5026", "\u4e2d"],
    "euc_kr": ["\uac00\ub098", "\u3131"],
    "cp949": ["\uac00\ue000", "\u8138"],
    "johab": ["\uac00\ub098", "\u3131\u3132"],
    "shift_jis": ["\u3042\u3044", "\uff66\uff76", "\u00a5\u203e"],
    "cp932": ["\u3042\uff76", "\u2250", "\uf8f0"],
    "euc_jp": ["\u4e2d\uff76", "\u4e00"],
    "shift_jis_2004": ["\u4e2d\u3042", "\u9b1d"],
    "euc_jis_2004": ["\u4e2d\uff76", "\u9b1c"],
    "shift_jisx0213": ["\u4e2d\u304b\u309a", "\u9b1d"],
    "euc_jisx0213": ["\u4e2d\u304b\u309a", "\u9b1d"],
    "iso2022_jp": ["\u4e2d\u6587a1", "\uff76"],
    "iso2022_jp_1": ["\u4e2d\u6587a1", "\u9fa0"],
    "iso2022_jp_2": ["\u4e2d\u6587a1\u00e9", "\U00020000"],
    "iso2022_jp_2004": ["\u4e2d\u304b\u309a", "\u9b1d"],
    "iso2022_jp_3": ["\u4e2d\u304b\u309a", "\u9b1d"],
    "iso2022_jp_ext": ["\u4e2d\u4ff8", "\u00e9"],
    "iso2022_kr": ["\uac00\ub098a1", "\u20ac"],
}

SHARED_DATA = [
    b"",
    b"a",
    b"AB",
    b"\x80",
    b"\xa1\xa1",
    b"\xa4\xa4",
    b"\xd6\xd0",
    b"\xb0\xa1",
    b"\x88\x62",
    b"\x81\x30\x81\x30",
    b"\x81\x30",
    b"\x8f",
    b"\x8f\xa4\xa1",
    b"\x81",
    b"\x81\x40",
    b"\x1b$B\x24\x2B\x1b(B",
    b"\x1b$(D\x24\x2B\x1b(B",
    b"\x1b$(P\x7d\x3b\x1b(B",
    b"\x7e\x7b\xd6\xd0\x7e\x7d\x41",
    b"\x7e\x7b\xd6\xd0",
    b"\x1b$)C\x0e\xb0\xa1\x0f",
    b"\x1b.\x46\x8e\xe1",
    b"\x1b(B",
    b"\xff",
    b"\xfe\xfe",
    b"A\xffA",
    b"\x5c\x7e",
    b"\x0e\x41\x0f",
    b"\x7f",
]

EXTRA_DATA = {
    "gb2312": [b"\xd6\xd0\xd5\xe2"],
    "gbk": [b"\x80", b"\xa1\xa1\xff"],
    "gb18030": [b"\x80\x41", b"\x84\x31\xa4\x39"],
    "hz": [b"~~a", b"~!", b"~", b"~{a"],
    "big5": [b"\xa4\xa4\xa4\xe5", b"\xa4"],
    "cp950": [b"\xa4\xa4\xa4\xe5", b"\x87\x40"],
    "big5hkscs": [b"\x88\x62\x88\x63", b"\x88"],
    "euc_kr": [b"\xb0\xa1\xb3\xaa", b"\xb0"],
    "cp949": [b"\xb0\xa1\xc9\x41", b"\xc9"],
    "johab": [b"\x84A", b"\x84"],
    "shift_jis": [b"\x82\xa0\x82\xa2", b"\xa5"],
    "cp932": [b"\x82\xa0\x88\x70", b"\x81\x40", b"\xfa\x40"],
    "euc_jp": [b"\xa4\xa2\xa4\xa4", b"\x8e\xb6", b"\x8f\xa2\xf1"],
    "shift_jis_2004": [b"\x82\xa0", b"\x81\x60"],
    "euc_jis_2004": [b"\xa4\xa2\xa4\xa4", b"\xa1\xc0"],
    "shift_jisx0213": [b"\x82\xf5", b"\x86\x3d"],
    "euc_jisx0213": [b"\xa4\xf5", b"\x8f\xf5\xf5"],
    "iso2022_jp": [b"\x1b$Ba\x1b(B", b"\x1b$"],
    "iso2022_jp_1": [b"\x1b$(D\x24\x2B\x1b(B"],
    "iso2022_jp_2": [b"\x1b.A\x8e\x41", b"\x1b$(D\x24\x2B"],
    "iso2022_jp_2004": [b"\x1b$(Q\x24\x2B\x1b(B", b"\x1b$(Q\x7d\x3b\x1b(B"],
    "iso2022_jp_3": [b"\x1b$(O\x24\x2B\x1b(B", b"\x1b$(O\x7d\x3b\x1b(B"],
    "iso2022_jp_ext": [b"\x1b$(D\x24\x2B\x1b(B", b"\x1b$(Q\x24\x2B\x1b(B"],
    "iso2022_kr": [b"\x0e\xb0\xa1\x0f", b"\x1b$)C"],
}

ENCODE_ERRORS = [
    "", "strict", "ignore", "replace", "backslashreplace",
    "xmlcharrefreplace",
]
DECODE_ERRORS = ["", "strict", "ignore", "replace", "backslashreplace"]

# ---------------------------------------------------------------- probing


def run_probe(source):
    env = dict(os.environ)
    result = subprocess.run(
        [sys.executable, "-X", "utf8", "-S", "-c", source],
        capture_output=True,
        encoding="utf-8",
        env=env,
    )
    if result.returncode != 0:
        sys.exit(f"probe failed ({result.returncode}):\n{result.stderr}\n{source[:400]}")
    return result.stdout


def try_block(statements):
    return (
        "try:\n"
        + "".join(f"    {s}\n" for s in statements)
        + "except Exception as e:\n"
        + "    print('E:' + type(e).__name__ + ':' + repr(str(e)))\n"
    )


def probe_calls(calls, preamble=""):
    """Live results of one `r = <call>` per line, in batched processes."""
    results = []
    for chunk_start in range(0, len(calls), 1000):
        chunk = calls[chunk_start : chunk_start + 1000]
        source = "import _codecs_cn, _codecs_hk, _codecs_iso2022\n"
        source += "import _codecs_jp, _codecs_kr, _codecs_tw\n"
        source += "import codecs\n" + preamble
        source += "".join(try_block([f"r = {call}", "print(repr(r))"]) for call in chunk)
        lines = run_probe(source).splitlines()
        if len(lines) != len(chunk):
            sys.exit(
                f"probe returned {len(lines)} lines for {len(chunk)} calls "
                f"(at offset {chunk_start})"
            )
        results.extend(decode_result(line) for line in lines)
    return results


def py_errors_arg(errors):
    return "None" if errors == "" else repr(errors)


def py_bytes(data):
    body = "".join(
        chr(b) if 0x20 < b < 0x7F and chr(b) not in "'\\" else f"\\x{b:02x}"
        for b in data
    )
    return f"b'{body}'"


def module_of(encoding):
    for module, (_, names) in MODULES.items():
        if encoding in names:
            return module
    raise KeyError(encoding)


# ---------------------------------------------------------------- getcodec


def getcodec_rows():
    calls = []
    plan = []
    misses = {
        "cn": ["utf-8", "shift_jis"],
        "hk": ["utf-8", "big5"],
        "iso2022": ["utf-8", "gbk"],
        "jp": ["utf-8", "gbk"],
        "kr": ["utf-8", "euc_jp"],
        "tw": ["utf-8", "gb2312"],
    }
    for module, (import_name, names) in MODULES.items():
        for name in names:
            calls.append(f"{import_name}.getcodec({name!r}) is not None")
            plan.append((module, name))
        for name in misses[module]:
            calls.append(f"{import_name}.getcodec({name!r}) is not None")
            plan.append((module, name))
    results = probe_calls(calls)
    return [
        [module, name, result]
        for (module, name), result in zip(plan, results)
    ]


# ------------------------------------------------------- encode and decode


def encode_rows():
    calls = []
    plan = []
    for encoding in ALL_ENCODINGS:
        module = MODULES[module_of(encoding)][0]
        for text in SHARED_TEXTS + EXTRA_TEXTS[encoding]:
            for errors in ENCODE_ERRORS:
                calls.append(
                    f"{module}.getcodec({encoding!r}).encode("
                    f"{text!r}, {py_errors_arg(errors)})"
                )
                plan.append((encoding, text, errors))
    results = probe_calls(calls)
    return [
        [encoding, esc(text), errors, result]
        for (encoding, text, errors), result in zip(plan, results)
    ]


def decode_rows():
    calls = []
    plan = []
    for encoding in ALL_ENCODINGS:
        module = MODULES[module_of(encoding)][0]
        for data in SHARED_DATA + EXTRA_DATA[encoding]:
            for errors in DECODE_ERRORS:
                calls.append(
                    f"{module}.getcodec({encoding!r}).decode("
                    f"{py_bytes(data)}, {py_errors_arg(errors)})"
                )
                plan.append((encoding, data, errors))
    results = probe_calls(calls)
    return [
        [encoding, enc_bytes(data), errors, result]
        for (encoding, data, errors), result in zip(plan, results)
    ]


# ------------------------------------------------------------ incremental

# Op encoding, one field per step, each op a `|`-split token:
#   E|<0/1>|<text>      encoder.encode(text, final)
#   D|<0/1>|<data>      decoder.decode(data, final)
#   EG                  encoder.getstate()
#   ESR                 encoder.setstate(<last EG result>)
#   ES|<int>            encoder.setstate(int)
#   ER                  encoder.reset()
#   DG                  decoder.getstate()
#   DSR                 decoder.setstate(<last DG result>)
#   DR                  decoder.reset()
# Each row builds one encoder and one decoder for (encoding, errors).

SEQUENCES = [
    # encoder pending: a JIS X 0213 pair split across calls
    ("euc_jisx0213", "strict", ["E|0|\u304b", "EG", "E|1|\u309a", "EG", "E|0|a", "EG"]),
    ("shift_jisx0213", "strict", ["E|0|\u304b", "E|1|\u309a", "E|1|"]),
    ("euc_jisx0213", "strict", ["E|0|\u304b", "EG", "ESR", "E|1|\u309a", "EG"]),
    ("euc_jisx0213", "strict", ["E|0|\u304b", "ER", "E|0|\u309a"]),
    ("euc_jisx0213", "strict", ["E|0|\u304b", "E|1|a", "EG"]),
    ("euc_jisx0213", "replace", ["E|0|\u304b", "E|1|a"]),
    # decoder pending: split sequences for plain and stateful codecs
    ("gbk", "strict", ["D|0|\xd6", "DG", "D|1|\xd0", "DG"]),
    ("gbk", "strict", ["D|0|\xd6", "DG", "DSR", "D|1|\xd0"]),
    ("gb18030", "strict", ["D|0|\x81\x30\x81", "D|1|\x30", "DG"]),
    ("big5hkscs", "strict", ["D|0|\x88", "D|0|\x62", "D|1|"]),
    ("euc_jp", "strict", ["D|0|\x8f", "D|1|\xa4\xa1"]),
    ("cp932", "strict", ["D|0|\x82", "D|1|\xa0"]),
    ("johab", "strict", ["D|0|\x84", "D|1|A"]),
    # iso2022 state carry, reset and round trips
    ("iso2022_jp", "strict", ["E|0|\u4e2d", "E|0|a", "ER", "E|0|\u4e2d", "E|1|a"]),
    ("iso2022_jp", "strict", ["D|0|\x1b$B\x24\x2B", "DR", "D|0|a", "D|1|"]),
    ("iso2022_kr", "strict", ["E|0|\uac00", "EG", "ESR", "E|0|a", "E|0|\uac00", "E|1|"]),
    ("iso2022_jp_3", "strict", ["E|0|\u9b1d", "EG", "E|0|a", "E|1|"]),
    ("iso2022_jp_ext", "strict", ["E|0|\u4ff8", "E|0|a", "E|1|"]),
    ("iso2022_jp_2", "strict", ["D|0|\x1b$B\x24\x2B", "DG", "DSR", "D|1|\x1b(B"]),
    # hz shifting
    ("hz", "strict", ["E|0|\u4e2d", "E|0|a", "ER", "E|0|\u4e2d", "E|1|"]),
    ("hz", "strict", ["D|0|\x7e\x7b\xd6\xd0", "DG", "D|0|\x7e\x7d", "D|0|a", "D|1|"]),
    ("hz", "replace", ["D|0|~!", "D|1|~"]),
    # explicit small states
    ("iso2022_jp", "strict", ["ES|0", "E|0|a", "ES|1", "E|0|\u4e2d", "E|1|a"]),
    ("hz", "strict", ["ES|0", "E|0|a", "D|0|a", "D|1|"]),
    ("gbk", "strict", ["ES|0", "E|0|a", "EG"]),
    # errors under incremental surfaces
    ("gbk", "replace", ["D|0|\xd6\xd6\xd0", "D|1|"]),
    ("gbk", "ignore", ["D|0|\xd6\xd6\xd0", "D|1|"]),
    ("cp932", "backslashreplace", ["D|0|\x81\x40", "D|1|"]),
    ("euc_jisx0213", "backslashreplace", ["E|0|\ufffd", "E|1|"]),
    ("big5", "replace", ["D|0|\xa4", "D|1|\xa4\xa4"]),
    ("cp949", "strict", ["E|0|\U00020000", "E|0|a", "E|1|"]),
]


def op_field(op):
    """The op token with its payload in the table's field escaping."""
    parts = op.split("|")
    if parts[0] == "E":
        return "|".join([parts[0], parts[1], esc(parts[2])])
    if parts[0] == "D":
        return "|".join([parts[0], parts[1], enc_bytes(parts[2].encode("latin-1"))])
    return op


def incremental_rows():
    rows = []
    for encoding, errors, ops in SEQUENCES:
        statements = [
            f"enc = codecs.getincrementalencoder({encoding!r})"
            f"({py_errors_arg(errors)})",
            f"dec = codecs.getincrementaldecoder({encoding!r})"
            f"({py_errors_arg(errors)})",
            "estates = []",
            "dstates = []",
        ]
        for op in ops:
            parts = op.split("|")
            if parts[0] == "E":
                call = f"enc.encode({parts[2]!r}, {parts[1] == '1'})"
            elif parts[0] == "D":
                call = (
                    "dec.decode("
                    f"{py_bytes(parts[2].encode('latin-1'))}, {parts[1] == '1'})"
                )
            elif parts[0] == "EG":
                statements += ["estates.append(enc.getstate())"]
                call = "estates[-1]"
            elif parts[0] == "ESR":
                call = "enc.setstate(estates[-1])"
            elif parts[0] == "ES":
                call = f"enc.setstate({parts[1]})"
            elif parts[0] == "ER":
                call = "enc.reset()"
            elif parts[0] == "DG":
                statements += ["dstates.append(dec.getstate())"]
                call = "dstates[-1]"
            elif parts[0] == "DSR":
                call = "dec.setstate(dstates[-1])"
            elif parts[0] == "DR":
                call = "dec.reset()"
            else:
                raise ValueError(op)
            statements += [try_block([f"r = {call}", "print(repr(r))"])]
        lines = run_probe(
            "import codecs\n" + "\n".join(statements) + "\n"
        ).splitlines()
        if len(lines) != len(ops):
            sys.exit(
                f"sequence probe for {encoding}/{ops[0]} returned "
                f"{len(lines)} lines for {len(ops)} ops:\n"
                + "\n".join(lines)
            )
        row = [encoding, errors]
        for op, line in zip(ops, lines):
            row.append(op_field(op))
            row.append(decode_result(line))
        rows.append(row)
    return rows


# ------------------------------------------------------------------- main


def write_rows(path, header, rows):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(header)
        for row in rows:
            fh.write("\t".join(row) + "\n")


def main():
    args = sys.argv[1:]
    if "--out" in args:
        out = Path(args[args.index("--out") + 1])
    else:
        out = Path(__file__).resolve().parent.parent / "native" / "crates" / "coil-rt" / "tests" / "data"
    out.mkdir(parents=True, exist_ok=True)

    write_rows(
        out / "cjk_getcodec.tsv",
        "# module name result\n",
        getcodec_rows(),
    )
    write_rows(
        out / "cjk_encode.tsv",
        "# encoding text errors result\n",
        encode_rows(),
    )
    write_rows(
        out / "cjk_decode.tsv",
        "# encoding data errors result\n",
        decode_rows(),
    )
    write_rows(
        out / "cjk_incremental.tsv",
        "# encoding errors op result ...\n",
        incremental_rows(),
    )
    print("wrote cjk oracle tables under", out)


if __name__ == "__main__":
    main()

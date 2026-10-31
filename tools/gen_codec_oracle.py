"""CPython 3.12 codec oracle for the coil-rt _codecs tests (spec 10.6).

Captures the pinned interpreter's behaviour for the module-level codec
functions (`_codecs.*_decode`/`*_encode`), the registry paths
`_codecs.encode`/`_codecs.decode` resolve built-in names through, the
charmap codec over explicit tables, and the built-in codec error
handlers applied to exception instances. Writes tab-separated
checked-in test data for native/crates/coil-rt/tests/. This module
refuses to run on anything but CPython 3.12, the pinned target whose
semantics the runtime ports.

Output files (under --out, default native/crates/coil-rt/tests/data):

  codecs_decode.tsv     module decoder functions over byte inputs
  codecs_ex.tsv         utf_16_ex_decode/utf_32_ex_decode (byteorder arg)
  codecs_encode.tsv     module encoder functions over str/bytes inputs
  codecs_charmap.tsv    charmap_encode/charmap_decode/charmap_build
  codecs_registry.tsv   _codecs.encode/_codecs.decode over object types
  codecs_handlers.tsv   error handler C exports over exception instances

Field encoding is shared with the Rust reader. Fields are separated by
literal tabs; inside a field a backslash escapes itself, tab, newline
and carriage return, str fields escape lone surrogates as \\uXXXX, and
bytes fields map each remaining byte b to chr(b) when 32 <= b <= 126,
else to \\xHH. Result rows are tagged s:/B:/i:/b:/L:/T: or
e:<Type>:<message>; tuples encode as L:<n> followed by one field per
item.
"""

import os
import subprocess
import sys
from pathlib import Path

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_codec_oracle.py must run on CPython 3.12 (the pinned target)")

# ---------------------------------------------------------------- escaping


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


def enc_bytes(data):
    parts = []
    for b in data:
        if b == 0x5C:
            parts.append("\\\\")
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


# ------------------------------------------------------------------ inputs

DECODE_DATA = [
    b"",
    b"a",
    b"abc",
    b"\xff",
    b"\xc3\xa9",
    b"ab\xc3\xa9\xff",
    b"\xed\xa0\x80",
    b"\xf0\x9f\x98\x80",
    b"\xfe\xff",
    b"\xff\xfe",
    b"\xfe\xff\x00a\x00b",
    b"\xff\xfea\x00b\x00",
    b"\x00a\x00b",
    b"a\x00b\x00",
    b"\x00\x00\x00a\x00\x00\x00b",
    b"a\x00\x00\x00b\x00\x00\x00",
    b"\x00\x00\xfe\xff\x00\x00\x00a",
    b"\xff\xfe\x00\x00a\x00\x00\x00",
    b"+AGE-",
    b"a+b-+2AE-",
    b"ab+\\",
    b"\xc3",
    b"a\xc3",
    b"a\\x41b\\u00e9",
    b"a\\q\\x4",
    b"'\t'\n\\",
    b"\xe9\x00\x7f",
    b"\x3d\xd8\x00\xdd",
    b"\x00\xd8\x00a",
    b"\x00\xdc\x00a",
    b"\x00\xd8",
    b"\x00\xd8\x41",
    b"\x00\xd8\x00\x00",
    b"\x00\x00\x00\x41",
    b"\xff\xfe\x00\x00\x3d\xd8\x00\xdd",
]

ERRORS_DECODE = [
    "", "strict", "ignore", "replace", "backslashreplace", "surrogateescape",
    "surrogatepass", "xmlcharrefreplace",
]

ENCODE_TEXTS = [
    "",
    "a",
    "café",
    "ÿ",
    "\u0100",
    "€",
    "\U0001f600",
    "a\U0001d400c",
    "\u0080",
    "'\\\t\n\r",
    "a\udcffb",
    "\ud800",
    "a\u2028b",
    "\u0130",
]

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
        source = "import _codecs, codecs\n" + preamble
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


# ----------------------------------------------------------------- decode

# The decoder functions split by their trailing arguments: the UTF
# family is stateful, latin-1/ascii/escape take data and errors only.
STATEFUL_DECODERS = [
    "utf_7_decode", "utf_8_decode", "utf_16_decode", "utf_16_le_decode",
    "utf_16_be_decode", "utf_32_decode", "utf_32_le_decode", "utf_32_be_decode",
    "unicode_escape_decode", "raw_unicode_escape_decode",
]
PLAIN_DECODERS = ["latin_1_decode", "ascii_decode", "escape_decode"]


def decode_rows():
    rows = []
    calls = []
    plan = []
    for fn in STATEFUL_DECODERS:
        for data in DECODE_DATA:
            for errors in ERRORS_DECODE:
                for final in (False, True):
                    calls.append(
                        f"_codecs.{fn}({py_bytes(data)}, "
                        f"{py_errors_arg(errors)}, {final})"
                    )
                    plan.append((fn, data, errors, final))
    for fn in PLAIN_DECODERS:
        for data in DECODE_DATA:
            for errors in ERRORS_DECODE:
                calls.append(
                    f"_codecs.{fn}({py_bytes(data)}, {py_errors_arg(errors)})"
                )
                plan.append((fn, data, errors, None))
    results = probe_calls(calls)
    for (fn, data, errors, final), result in zip(plan, results):
        final_field = "-" if final is None else ("b:1" if final else "b:0")
        rows.append([fn, enc_bytes(data), errors, final_field, result])
    return rows


def ex_rows():
    rows = []
    calls = []
    plan = []
    for fn in ("utf_16_ex_decode", "utf_32_ex_decode"):
        for data in DECODE_DATA:
            for byteorder in (-1, 0, 1):
                for errors in ("", "strict", "ignore", "replace", "surrogateescape", "surrogatepass"):
                    for final in (False, True):
                        calls.append(
                            f"_codecs.{fn}({py_bytes(data)}, "
                            f"{py_errors_arg(errors)}, {byteorder}, {final})"
                        )
                        plan.append((fn, data, errors, byteorder, final))
    results = probe_calls(calls)
    for (fn, data, errors, byteorder, final), result in zip(plan, results):
        rows.append([
            fn, enc_bytes(data), errors, f"i:{byteorder}",
            "b:1" if final else "b:0", result,
        ])
    return rows


# ----------------------------------------------------------------- encode


def encode_rows():
    fns_no_arg = [
        "utf_7_encode", "utf_8_encode", "utf_16_le_encode", "utf_16_be_encode",
        "utf_32_le_encode", "utf_32_be_encode", "latin_1_encode", "ascii_encode",
        "unicode_escape_encode", "raw_unicode_escape_encode",
    ]
    errors_set = [
        "", "strict", "ignore", "replace", "backslashreplace", "surrogateescape",
        "surrogatepass", "xmlcharrefreplace",
    ]
    calls = []
    plan = []
    for fn in fns_no_arg:
        for text in ENCODE_TEXTS:
            for errors in errors_set:
                calls.append(f"_codecs.{fn}({text!r}, {py_errors_arg(errors)})")
                plan.append((fn, text, errors, None))
    for fn in ("utf_16_encode", "utf_32_encode"):
        for text in ENCODE_TEXTS:
            for byteorder in (-1, 0, 1):
                for errors in errors_set:
                    calls.append(
                        f"_codecs.{fn}({text!r}, {py_errors_arg(errors)}, {byteorder})"
                    )
                    plan.append((fn, text, errors, byteorder))
    for data in (b"", b"a", b"'\\\t\n\r", b"\x00\xff\x7f", b"abc"):
        for fn in ("escape_encode", "readbuffer_encode"):
            calls.append(f"_codecs.{fn}({py_bytes(data)}, None)")
            plan.append((fn, data, "", None))
    results = probe_calls(calls)
    rows = []
    for (fn, value, errors, byteorder), result in zip(plan, results):
        text = enc_bytes(value) if isinstance(value, bytes) else esc(value)
        order = "-" if byteorder is None else f"i:{byteorder}"
        rows.append([fn, text, errors, order, result])
    return rows


# ---------------------------------------------------------------- charmap

# byte -> value: i:<int> an int, s:<chars> a str, n None (undefined),
# B:<hex> bytes. The Rust reader builds the same mapping object.
CHARMAPS = [
    "97=i:65,98=i:9790,99=n,100=s:B,101=s:xy,102=i:1114112,103=i:-1",
    "0=n,1=i:49,65=s:\u20ac,128=i:8364,255=n",
    "97=B:4142,98=B:,99=i:200",
    "0=i:0,127=i:127,128=s:\u0080,160=s:xy",
]


def parse_mapping(spec):
    table = {}
    for item in spec.split(","):
        key, _, value = item.partition("=")
        if value == "n":
            table[int(key)] = None
        elif value.startswith("i:"):
            table[int(key)] = int(value[2:])
        elif value.startswith("s:"):
            table[int(key)] = value[2:]
        elif value.startswith("B:"):
            table[int(key)] = bytes.fromhex(value[2:])
        else:
            raise ValueError(spec)
    return table


def py_mapping(spec):
    parts = [f"{key}: {value!r}" for key, value in parse_mapping(spec).items()]
    return "{" + ", ".join(parts) + "}"


def charmap_rows():
    calls = []
    plan = []
    for spec in CHARMAPS:
        for data in (b"", b"abc", b"abcde", b"\x00\xff", b"ce", b"`ab"):
            for errors in ERRORS_DECODE:
                calls.append(
                    f"_codecs.charmap_decode({py_bytes(data)}, "
                    f"{py_errors_arg(errors)}, {py_mapping(spec)})"
                )
                plan.append(("charmap_decode", data, errors, spec))
        for text in ("a", "abcde", "ac", "\u20ac", "ÿ", "ce", "cff", "de"):
            for errors in ERRORS_DECODE:
                calls.append(
                    f"_codecs.charmap_encode({text!r}, "
                    f"{py_errors_arg(errors)}, {py_mapping(spec)})"
                )
                plan.append(("charmap_encode", text, errors, spec))
    results = probe_calls(calls)
    # charmap_build returns a mapping object whose items keep the
    # input string's order; probe the (key, value) pairs directly.
    build_texts = ("", "a", "abc", "\u00ff\u20ac", "\u0100\U0001f600", "aaab", "x" * 300)
    build_calls = [
        f"list(_codecs.charmap_build({text!r}).items())" for text in build_texts
    ]
    builds = [
        ["charmap_build", esc(text), "-", "-", result]
        for text, result in zip(build_texts, probe_calls(build_calls))
    ]
    rows = []
    for (kind, value, errors, spec), result in zip(plan, results):
        field = enc_bytes(value) if isinstance(value, bytes) else esc(value)
        rows.append([kind, field, errors, spec, result])
    rows.extend(builds)
    return rows


# --------------------------------------------------------------- registry

REGISTRY_NAMES = [
    "utf-8", "utf8", "UTF-8", "utf_8", "u8",
    "utf-7", "utf_7",
    "utf-16", "utf_16", "utf-16-le", "utf_16_le", "utf_16_be",
    "utf-32", "utf-32-be", "utf_32_le",
    "latin-1", "latin1", "latin_1", "iso8859-1", "iso_8859_1", "8859", "l1",
    "ascii", "us-ascii", "US-ASCII", "646", "ansi_x3.4-1968",
    "unicode_escape", "unicode-escape",
    "raw_unicode_escape", "raw-unicode-escape",
    "charmap",
]


def registry_rows():
    objects = [
        ("b", b"abc\xc3\xa9\xff"),
        ("b2", b"\xff\xfe\x00a"),
        ("s", "caf\u00e9 \u00ff \U0001f600"),
        ("s2", "abc"),
        ("B", bytearray(b"abc\xff")),
        ("i", 123),
    ]
    calls = []
    plan = []
    for name in REGISTRY_NAMES:
        for kind, obj in objects:
            for errors in ("", "strict", "ignore", "replace", "surrogateescape"):
                tail = "" if errors == "" else f", {errors!r}"
                calls.append(
                    f"_codecs.encode({obj!r}, {name!r}{tail})"
                )
                plan.append(("encode", name, kind, errors))
                calls.append(
                    f"_codecs.decode({obj!r}, {name!r}{tail})"
                )
                plan.append(("decode", name, kind, errors))
    results = probe_calls(calls)
    return [
        [op, kind, name, errors, result]
        for (op, name, kind, errors), result in zip(plan, results)
    ]


# --------------------------------------------------------------- handlers

HANDLER_NAMES = ["ignore", "replace", "backslashreplace", "xmlcharrefreplace", "strict"]

# Decode and translate exceptions are constructible from their fields;
# encode ones are triggered through the strict encoder (the C API has
# no constructor), so the Rust side reads the pending exception.
DECODE_EXCS = [
    ("utf-8", b"a\xff\xc3\x80b", 0, 1, "invalid start byte"),
    ("utf-8", b"a\xff\xc3\x80b", 1, 3, "invalid continuation byte"),
    ("latin-1", b"ab", 0, 0, "ordinal not in range"),
    ("ascii", b"a\xffb\xffc", 1, 4, "ordinal not in range(128)"),
]
ENCODE_EXCS = [
    ("ascii_encode", "aÿb€c", None),
    ("latin_1_encode", "aÿ€b", None),
    ("utf_8_encode", "a\ud800b", None),
]
TRANSLATE_EXCS = [
    ("a\u20acb", 0, 1, "character maps to <undefined>"),
    ("abc", 1, 3, "undefined"),
]


def handler_rows():
    # Columns: kind, then the exception's construction fields (p1..p5),
    # the handler, the result. Decode/cross rows carry
    # encoding/data/start/end/reason; encode rows the module function
    # and input whose strict failure raises the exception; translate
    # rows input/start/end/reason; value rows nothing (a plain
    # ValueError instance).
    preamble = (
        "def _enc_exc(fn, text):\n"
        "    try:\n"
        "        fn(text)\n"
        "    except Exception as e:\n"
        "        return e\n"
        "    raise AssertionError('expected an encode error')\n"
    )
    calls = []
    plan = []
    for encoding, data, start, end, reason in DECODE_EXCS:
        for handler in HANDLER_NAMES:
            calls.append(
                f"codecs.lookup_error({handler!r})("
                f"UnicodeDecodeError({encoding!r}, {py_bytes(data)}, "
                f"{start}, {end}, {reason!r}))"
            )
            plan.append(
                ("decode", encoding, enc_bytes(data), f"i:{start}", f"i:{end}", reason, handler)
            )
    for fn, text, errors in ENCODE_EXCS:
        for handler in HANDLER_NAMES:
            calls.append(
                f"codecs.lookup_error({handler!r})"
                f"(_enc_exc(_codecs.{fn}, {text!r}))"
            )
            plan.append(("encode", fn, esc(text), "-", "-", "-", handler))
    for text, start, end, reason in TRANSLATE_EXCS:
        for handler in HANDLER_NAMES:
            calls.append(
                f"codecs.lookup_error({handler!r})("
                f"UnicodeTranslateError({text!r}, {start}, {end}, {reason!r}))"
            )
            plan.append(("translate", esc(text), f"i:{start}", f"i:{end}", reason, "-", handler))
    # Wrong exception types: strict re-raises the instance, the rest
    # reject it.
    for handler in HANDLER_NAMES:
        calls.append(f"codecs.lookup_error({handler!r})(ValueError('boom'))")
        plan.append(("value", "-", "-", "-", "-", "-", handler))
    # an encode handler over a decode exception
    calls.append(
        "codecs.lookup_error('xmlcharrefreplace')("
        "UnicodeDecodeError('utf-8', b'\\xff', 0, 1, 'invalid start byte'))"
    )
    plan.append(
        ("cross", "utf-8", "\\xff", "i:0", "i:1", "invalid start byte", "xmlcharrefreplace")
    )
    results = probe_calls(calls, preamble)
    return [list(fields) + [result] for fields, result in zip(plan, results)]


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
        out / "codecs_decode.tsv",
        "# fn data errors final result\n",
        decode_rows(),
    )
    write_rows(
        out / "codecs_ex.tsv",
        "# fn data errors byteorder final result\n",
        ex_rows(),
    )
    write_rows(
        out / "codecs_encode.tsv",
        "# fn input errors byteorder result\n",
        encode_rows(),
    )
    write_rows(
        out / "codecs_charmap.tsv",
        "# fn input errors mapping result\n",
        charmap_rows(),
    )
    write_rows(
        out / "codecs_registry.tsv",
        "# op objtype encoding errors result\n",
        registry_rows(),
    )
    write_rows(
        out / "codecs_handlers.tsv",
        "# kind p1 p2 p3 p4 p5 handler result\n",
        handler_rows(),
    )
    print("wrote codec oracle tables under", out)


if __name__ == "__main__":
    main()

"""CPython 3.12 str/bytes/bytearray oracle for the coil-rt object tests (spec 8.2, 8.5).

Computes string, bytes and bytearray behaviour, %-formatting, the str
format-spec mini-language, repr quoting and SipHash values with the pinned
interpreter and writes them as checked-in test data (tab-separated, one case
per line) for native/crates/coil-rt/tests/. This module refuses to run on
anything but CPython 3.12, the pinned target whose semantics the runtime
ports.

Output files (under --out, default native/crates/coil-rt/tests/data):

  str_methods.tsv        str methods over ASCII, UCS2 and UCS4 inputs
  bytes_methods.tsv      bytes methods, byte values escaped as \\xNN
  str_decode.tsv         PyUnicode_Decode over the built-in codec set
  str_encode.tsv         PyUnicode_AsEncodedString over the same set
  str_format.tsv         str %-formatting with a single argument
  bytes_format.tsv       bytes %-formatting with a single argument
  str_format_spec.tsv    str.__format__ over the format-spec mini-language
  str_repr.tsv           repr() of str values (the quoting rules)
  bytes_repr.tsv         repr() of bytes and bytearray values
  str_hash.tsv           hash() of str values under pinned SipHash keys
  bytes_hash.tsv         hash() of bytes values under pinned SipHash keys

Field encoding is shared with the Rust reader. Fields are separated by
literal tabs; inside a field a backslash escapes itself, tab, newline and
carriage return, str fields escape lone surrogates as \\uXXXX (they arise
from the surrogateescape error handler), and bytes fields map each
remaining byte b to chr(b) when 32 <= b <= 126, else to \\xHH. Result rows
are tagged:

  s:<text>   str          B:<bytes>  bytes        i:<n>     int
  b:0|b:1    bool         L:<n> ...  n more fields, a list of items
  T:         None         e:<Type>:<message>   exception with its message

List items use the same escaping as their element type (str methods yield
str items, bytes methods bytes items); the reader knows which from the row.

The hash tables carry a `# key <k0> <k1>` header per section; the rows under
it hold the hash CPython 3.12 computes for that seed, and k0/k1 are the
process SipHash key CPython derives from `PYTHONHASHSEED` (0 for seed 0,
the LCG stream otherwise). The test pins the same key through the runtime's
test hook.
"""

import os
import struct
import subprocess
import sys
from pathlib import Path

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_str_oracle.py must run on CPython 3.12 (the pinned target)")

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


def enc_arg(value):
    """A method argument: empty for None, `b:1`/`b:0` for bools, `i:<n>`
    for ints, escaped text otherwise."""
    if value is None:
        return ""
    if isinstance(value, bool):
        return "b:1" if value else "b:0"
    if isinstance(value, int):
        return f"i:{value}"
    if isinstance(value, bytes):
        return enc_bytes(value)
    return esc(value)


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
            esc(item) if isinstance(item, str) else enc_bytes(item)
            for item in value
        )
        return "\t".join(parts)
    return "s:" + esc(value)


def decode_result(live):
    """The probe's `repr(r)` (or E: marker) to the shared encoding."""
    if live.startswith("E:"):
        name, _, message = live[2:].partition(":")
        return f"e:{name}:{esc(eval(message))}"
    return enc_result(eval(live))


# ------------------------------------------------------------ str inputs
# The pools deliberately cross the compact kinds: pure ASCII stays 1-byte,
# the latin-1 range forces 2-byte storage, and the astral plane forces 4.

STR_CORE = [
    "",
    "a",
    "abc",
    "hello world",
    "Hello, World!",
    "  spaced  out  ",
    "\t\ttabs\tand\nnewlines\n",
    "abcabcabc",
    "aaa",
    "xyz",
    "1234567890",
    "Ab1",
    "'quoted' and \"double\"",
    "back\\slash",
    "0123456789",
    "MiXeD CaSe StRiNg",
    "a b",
]

STR_LATIN1 = [
    "café",
    "ñ",
    "µ",
    "ÿ",
    "ÀÉÎÕÜ",
    "abcÿdef",
    "ééé",
    "\u0080\u009f",
]

STR_ASTRAL = [
    "\U0001f600",
    "a\U0001d400c",
    "\U00010000",
    "\U0010ffff",
    "x\U0001f600y\U0001f600z",
]

STR_CASES_POOL = [
    "ß",        # upper() expands to SS
    "İ",        # Turkish dotted I
    "ǅ",        # titlecase character
    "ﬃ",        # ligature, full lowercase mapping expands
    "ʼn",       # case ignorable
    "٣",        # Arabic-Indic digit three (decimal)
    "²",        # superscript two (digit, not decimal)
    "Ⅻ",        # Roman numeral twelve (numeric)
    "三",        # CJK numeric
    "\u00a0",        # NBSP, Unicode whitespace
    "\u2028",        # line separator
    "ᾈ",        # capital alpha with psili and prosgegrammeni
    "á",       # combining acute (case ignorable context)
]

STR_INPUTS = STR_CORE + STR_LATIN1 + STR_ASTRAL + STR_CASES_POOL

BYTE_INPUTS = [
    b"",
    b"a",
    b"abc",
    b"hello world",
    b"Hello, World!",
    b"  spaced  out  ",
    b"\t\ttabs\tand\nnewlines\n",
    b"abcabcabc",
    b"aaa",
    b"xyz",
    b"1234567890",
    b"Ab1",
    b"'quoted' and \"double\"",
    b"back\\slash",
    b"\x00\x01\x02",
    b"\x7f\x80\xff",
    b"\xe9\xe9\xe9",
    b"\xde\xad\xbe\xef",
    b"MiXeD CaSe ByTeS",
]

# ------------------------------------------------------------------ cases
# (method, args) per table; the generator executes input.method(*args) and
# the Rust harness dispatches on the same name with the same arity.

STR_METHOD_CASES = [
    ("upper", ()),
    ("lower", ()),
    ("casefold", ()),
    ("title", ()),
    ("capitalize", ()),
    ("swapcase", ()),
    ("strip", (None,)),
    ("strip", ("ab",)),
    ("lstrip", (None,)),
    ("lstrip", ("ab",)),
    ("rstrip", (None,)),
    ("rstrip", ("ab",)),
    ("center", (7,)),
    ("center", (7, "*")),
    ("center", (-1,)),
    ("center", (7, "**")),
    ("ljust", (7,)),
    ("ljust", (7, "*")),
    ("ljust", (2,)),
    ("rjust", (7,)),
    ("rjust", (7, "0")),
    ("zfill", (7,)),
    ("zfill", (2,)),
    ("zfill", (-7,)),
    ("expandtabs", ()),
    ("expandtabs", (0,)),
    ("expandtabs", (4,)),
    ("expandtabs", (-1,)),
    ("count", ("a", 0, -1)),
    ("count", ("ab", 1, 6)),
    ("count", ("", 0, -1)),
    ("count", ("z", 0, -1)),
    ("find", ("a", 0, -1)),
    ("find", ("ab", 1, 5)),
    ("find", ("z", 0, -1)),
    ("find", ("", 3, 3)),
    ("rfind", ("a", 0, -1)),
    ("rfind", ("z", 0, -1)),
    ("startswith", ("ab", 0, -1)),
    ("startswith", ("", 0, -1)),
    ("endswith", ("c", 0, -1)),
    ("endswith", ("", 0, -1)),
    ("replace", ("a", "X", -1)),
    ("replace", ("a", "X", 2)),
    ("replace", ("", "-", -1)),
    ("replace", ("abc", "", -1)),
    ("removeprefix", ("ab",)),
    ("removeprefix", ("zz",)),
    ("removesuffix", ("bc",)),
    ("removesuffix", ("zz",)),
    ("isalpha", ()),
    ("isdecimal", ()),
    ("isdigit", ()),
    ("isnumeric", ()),
    ("isspace", ()),
    ("islower", ()),
    ("isupper", ()),
    ("istitle", ()),
    ("isalnum", ()),
    ("isascii", ()),
    ("isprintable", ()),
    ("isidentifier", ()),
    ("split", (None, -1)),
    ("split", (None, 1)),
    ("split", ("a", -1)),
    ("split", ("a", 0)),
    ("split", ("bc", -1)),
    ("rsplit", (None, -1)),
    ("rsplit", (None, 1)),
    ("rsplit", ("a", -1)),
    ("rsplit", ("bc", 1)),
    ("splitlines", (False,)),
    ("splitlines", (True,)),
    ("partition", ("b",)),
    ("partition", ("zz",)),
    ("rpartition", ("b",)),
    ("rpartition", ("zz",)),
    # separator.join((x, x)): _PyUnicode_JoinArray over a fixed item array
    # (list objects are a later runtime layer)
    ("join_pair", ()),
]

BYTE_METHOD_CASES = [
    ("upper", ()),
    ("lower", ()),
    ("capitalize", ()),
    ("title", ()),
    ("swapcase", ()),
    ("strip", (None,)),
    ("strip", (b"ab",)),
    ("lstrip", (None,)),
    ("lstrip", (b"ab",)),
    ("rstrip", (None,)),
    ("rstrip", (b"ab",)),
    ("center", (7,)),
    ("center", (7, b"*")),
    ("center", (-1,)),
    ("center", (7, b"**")),
    ("ljust", (7,)),
    ("ljust", (7, b"*")),
    ("ljust", (2,)),
    ("rjust", (7,)),
    ("rjust", (7, b"0")),
    ("zfill", (7,)),
    ("zfill", (2,)),
    ("zfill", (-7,)),
    ("expandtabs", ()),
    ("expandtabs", (0,)),
    ("expandtabs", (4,)),
    ("expandtabs", (-1,)),
    ("count", (b"a", 0, -1)),
    ("count", (b"\xe9", 0, -1)),
    ("find", (b"a", 0, -1)),
    ("find", (b"\xff", 0, -1)),
    ("rfind", (b"a", 0, -1)),
    ("rfind", (b"z", 0, -1)),
    ("startswith", (b"ab", 0, -1)),
    ("startswith", (b"", 0, -1)),
    ("endswith", (b"c", 0, -1)),
    ("endswith", (b"", 0, -1)),
    ("replace", (b"a", b"X", -1)),
    ("replace", (b"a", b"X", 2)),
    ("replace", (b"", b"-", -1)),
    ("replace", (b"abc", b"", -1)),
    ("removeprefix", (b"ab",)),
    ("removeprefix", (b"zz",)),
    ("removesuffix", (b"bc",)),
    ("removesuffix", (b"zz",)),
    ("isalpha", ()),
    ("isdigit", ()),
    ("isalnum", ()),
    ("isascii", ()),
    ("isspace", ()),
    ("islower", ()),
    ("isupper", ()),
    ("istitle", ()),
    ("split", (None, -1)),
    ("split", (None, 1)),
    ("split", (b"a", -1)),
    ("split", (b"a", 0)),
    ("split", (b"\xe9", -1)),
    ("rsplit", (None, -1)),
    ("rsplit", (None, 1)),
    ("rsplit", (b"a", -1)),
    ("splitlines", (False,)),
    ("splitlines", (True,)),
    ("partition", (b"b",)),
    ("partition", (b"zz",)),
    ("rpartition", (b"b",)),
    ("rpartition", (b"zz",)),
    # bytes.join takes an iterable of bytes, so it waits for the container
    # runtime and is not probed here
    ("translate", (bytes(range(256)), b"")),
    ("translate", (bytes(range(255)), b"")),
    ("translate", (bytes(range(256)), bytes(range(0, 256, 2)))),
]

# x.translate(bytes.maketrans(frm, to, delete)) as one observable composite;
# maketrans itself returns a dict in CPython, which the runtime represents
# internally as the 256-byte table, so the probed result is the translate
# output rather than the table object.
MAKETRANS_CASES = [
    (b"abc", b"xyz", b""),
    (b"abc", b"xy", b""),
    (b"abc", b"xyz", b"ab"),
    (b"", b"", b"\x00\x05"),
    (b"\x00\xff", b"\xff\x00", b""),
]

FROMHEX_INPUTS = [
    "00 ff 10",
    "0ff",
    "0f 0f",
    "ff" * 8,
    "0 0 0",
    "000",
    "ff\tff",
    "f",
    "zz",
    "0x00",
    "aa bb cc",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "00  ff",
    "00 FF",
    "0 0 ",
]

HEX_CASES = [
    (b"", 0),
    (b"-", 1),
    (b"-", 2),
    (b"-", -2),
    (b"-", 100),
]

HEX_INPUTS = [b"", b"\x00", b"\x00\xff\x10", bytes(range(8)),
              b"\xde\xad\xbe\xef" * 3]

DECODE_CASES = [
    ("ascii", None),
    ("ascii", "strict"),
    ("ascii", "ignore"),
    ("ascii", "replace"),
    ("latin1", None),
    ("latin-1", "strict"),
    ("utf8", None),
    ("utf-8", "strict"),
    ("utf-8", "surrogateescape"),
    ("utf-16", None),
    ("utf-16le", None),
    ("utf-16be", None),
    ("utf-32", None),
    ("utf_32_le", None),
    ("unicodeescape", None),
    ("unicode_escape", None),
    ("unicode_escape", "ignore"),
    ("unicode_escape", "replace"),
    ("unicode_escape", "backslashreplace"),
    ("rawunicodeescape", None),
    ("raw_unicode_escape", None),
    # registry alias spellings and near misses
    ("u16", None),
    ("u32", None),
    ("646", None),
    ("l1", None),
    ("cp819", None),
    ("latin", None),
    ("utf-16-le", None),
    ("utf_16_le", None),
    ("utf-32-le", None),
    ("utf16le", None),
    ("utf32be", None),
    ("unicode-escape", None),
    ("raw-unicode-escape", None),
    ("mbcs", None),
]

DECODE_INPUTS = [
    b"hello",
    b"caf\xe9",
    b"\xff\xfeh\x00i\x00",
    b"h\x00i\x00",
    b"\x00\x00h\x00",
    b"h\x00\x00\x00i\x00\x00\x00",
    b"\\x41\\u00e9\\U0001f600",
    # \N{...} stays out until the unicodedata task ships the name
    # database; the runtime raises CPython's capsule-missing error.
    b"\\x4",
    b"\\",
    b"a\\qb",
    b"\xff\xfe\x00\x00i\x00\x00\x00",
    b"\xef\xbb\xbfhi",
    b"a\\U00110000b",
    b"\x80abc",
    b"\\u",
]

ENCODE_CASES = [
    ("ascii", None),
    ("ascii", "strict"),
    ("ascii", "ignore"),
    ("ascii", "replace"),
    ("ascii", "backslashreplace"),
    ("latin1", None),
    ("latin-1", "strict"),
    ("latin-1", "replace"),
    ("latin-1", "ignore"),
    ("utf8", None),
    ("utf-8", "strict"),
    ("utf-8", "surrogateescape"),
    ("utf-16", None),
    ("utf-16le", None),
    ("utf-16be", None),
    ("utf-32", None),
    ("utf_32_le", None),
    ("unicodeescape", None),
    ("unicode_escape", None),
    ("rawunicodeescape", None),
    ("charmap", None),
    ("l1", None),
    ("utf-16-le", None),
    ("utf16le", None),
    ("raw-unicode-escape", None),
]

STR_FORMAT_CASES = [
    ("no placeholders", 7),
    ("", 7),
    ("%%", 7),
    ("%d", 7),
    ("%d", -7),
    ("%d", 0),
    ("%5d", 7),
    ("%-5d|", 7),
    ("%05d", -7),
    ("% d", 7),
    ("%+d", 7),
    ("%+d", -7),
    ("%i", 42),
    ("%u", 42),
    ("%o", 8),
    ("%#o", 8),
    ("%x", 255),
    ("%X", 255),
    ("%#x", 255),
    ("%#X", 255),
    ("%8x", 255),
    ("%.4x", 255),
    ("%#010x", 255),
    ("%s", "text"),
    ("%s", ""),
    ("%10s", "hi"),
    ("%-10s|", "hi"),
    ("%.2s", "hello"),
    ("%r", "text"),
    ("%a", "text"),
    ("%r", "q'uo\"te"),
    ("%c", "A"),
    ("%c", "€"),
    ("%s%%", 100),
    ("%%%d", 9),
    ("%(name)s", 7),
    ("%s %s", 7),
    ("%c", "ab"),
    ("%d", "x"),
    ("%f", 1.5),
    ("%.2f", 1.005),
    ("%e", 12345.6789),
    ("%E", 12345.6789),
    ("%g", 0.0001),
    ("%G", 123456789.0),
    ("%g", 123456789.0),
    ("%.3g", 0.000999),
    ("%12.4f|", 3.14159265),
    ("%-12.4f|", 3.14159265),
    ("%+012.4f", 3.14159265),
    ("%f", float("inf")),
    ("%f", float("-inf")),
    ("%f", float("nan")),
    ("%e", 0.0),
    ("%a", 1.0),
    ("%r", 1.5),
    ("%d", 1.5),
]

BYTES_FORMAT_CASES = [
    (b"no placeholders", 7),
    (b"", 7),
    (b"%%", 7),
    (b"%d", 7),
    (b"%d", -7),
    (b"%05d", -7),
    (b"%x", 255),
    (b"%#x", 255),
    (b"%o", 8),
    (b"%s", b"bytes arg"),
    (b"%s", b""),
    (b"%10s|", b"hi"),
    (b"%.2s", b"hello"),
    (b"%c", 65),
    (b"%c", 255),
    (b"%c", 256),
    (b"%c", -1),
    (b"%z", 7),
    (b"%d", None),
    (b"%", 7),
    (b"% d", 7),
]

FORMAT_SPEC_CASES = [
    ("text", ""),
    ("text", "s"),
    ("text", ">10"),
    ("text", "<10"),
    ("text", "^10"),
    ("text", "*^12"),
    ("text", ".2"),
    ("text", "10.3"),
    ("text", "=10"),
    ("text", "+10"),
    ("text", "x"),
    ("text", ","),
    ("", "^7"),
    ("abc", "0>5"),
    ("abc", "0<5"),
    ("abc", "05"),
    ("ab", "c^6"),
    ("abc", ".10"),
    ("abcd", ".2s"),
]

STR_REPR_INPUTS = [
    "",
    "a",
    "abc",
    "'",
    '"',
    "'\"",
    "it's",
    'say "hi"',
    "both ' and \"",
    "\t\n\r",
    "\x00\x07\x1f",
    "\x7f",
    "café",
    "\U0001f600",
    "\x80",
    "a\\b",
    "\U0010ffff",
]

BYTE_REPR_INPUTS = [
    b"",
    b"a",
    b"'",
    b'"',
    b"'\x00",
    b'"\x00',
    b"\x00\x01\xff",
    b"hello world",
    b"\\",
    b"\t\n\r",
    b"\x7f\x80",
]

HASH_SEEDS = [0, 1250, 0xFFFFFFFF]

HASH_INPUTS_STR = [
    "",
    "a",
    "abc",
    "hello world",
    "The quick brown fox jumps over the lazy dog",
    "café",
    "\U0001f600",
    "x" * 100,
    "\x00\x00",
    "Ab3\tCd\n",
]

HASH_INPUTS_BYTES = [
    b"",
    b"a",
    b"abc",
    b"hello world",
    b"The quick brown fox jumps over the lazy dog",
    b"\x00\xff\x7f\x80",
    b"\xde\xad\xbe\xef" * 4,
    b"y" * 100,
]


# ---------------------------------------------------------------- probing


def run_probe(source, seed=None):
    env = dict(os.environ)
    if seed is not None:
        env["PYTHONHASHSEED"] = str(seed)
    result = subprocess.run(
        [sys.executable, "-X", "utf8", "-S", "-c", source],
        capture_output=True,
        encoding="utf-8",
        env=env,
    )
    if result.returncode != 0:
        sys.exit(f"probe failed ({result.returncode}):\n{result.stderr}")
    return result.stdout


def try_block(statements):
    return (
        "try:\n"
        + "".join(f"    {s}\n" for s in statements)
        + "except Exception as e:\n"
        + "    print('E:' + type(e).__name__ + ':' + repr(str(e)))\n"
    )


def probe_column(inputs, method, call_args):
    """Live results of `x.method(*args)` for each input, one process.

    repr() never contains a raw newline, so each result is one line.
    """
    arg_repr = ", ".join(repr(a) for a in call_args)
    source = ""
    for value in inputs:
        source += f"x = {value!r}\n"
        expr = "x.join((x, x))" if method == "join_pair" else f"x.{method}({arg_repr})"
        source += try_block([f"r = {expr}", "print(repr(r))"])
    lines = run_probe(source).splitlines()
    if len(lines) != len(inputs):
        sys.exit(f"probe for {method}{call_args!r} returned {len(lines)} lines")
    return [decode_result(line) for line in lines]


def fmt_arg(value):
    if value is None:
        return ""
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return f"f:{bits:016X}"
    if isinstance(value, int):
        return f"i:{value}"
    if isinstance(value, bytes):
        return "B:" + enc_bytes(value)
    return "s:" + esc(value)


def fmt_probe(fmt, arg):
    setup = ""
    arg_repr = repr(arg)
    if isinstance(arg, float):
        # repr(inf)/repr(nan) are not evaluable; name them explicitly
        if arg != arg:
            setup, arg_repr = "nan = float('nan')\n", "nan"
        elif arg == float("inf"):
            setup, arg_repr = "inf = float('inf')\n", "inf"
        elif arg == float("-inf"):
            setup, arg_repr = "inf = float('-inf')\n", "inf"
    return decode_result(
        run_probe(
            setup + try_block([f"r = {fmt!r} % ({arg_repr},)", "print(repr(r))"])
        ).strip("\n")
    )


def plain_probe(statement):
    return decode_result(
        run_probe(try_block([statement, "print(repr(r))"])).strip("\n")
    )


# ------------------------------------------------------------------ hashes
# CPython derives the 16-byte SipHash secret from PYTHONHASHSEED with an
# LCG (Python/bootstrap_hash.c: lcg_urandom); the key halves are the two
# little-endian u64 words of the secret. Seed 0 zeroes the secret.

MASK64 = (1 << 64) - 1


def lcg_urandom(seed):
    out = bytearray()
    state = seed & 0xFFFFFFFF
    for _ in range(16):
        state = (state * 214013 + 2531011) & 0xFFFFFFFF
        out.append((state >> 16) & 0xFF)
    return bytes(out)


def key_for_seed(seed):
    if seed == 0:
        return (0, 0)
    secret = lcg_urandom(seed)
    return (
        int.from_bytes(secret[0:8], "little"),
        int.from_bytes(secret[8:16], "little"),
    )


def siphash13(k0, k1, data):
    """The pyhash.c algorithm, for the generator's own cross-check."""

    def rot(x, b):
        return ((x << b) | (x >> (64 - b))) & MASK64

    def sipround():
        # pyhash.c SINGLE_ROUND: the second HALF_ROUND permutes the lanes
        # (v2,v1,v0,v3), unlike stock SipHash.
        nonlocal v0, v1, v2, v3
        v0 = (v0 + v1) & MASK64
        v1 = rot(v1, 13) ^ v0
        v2 = (v2 + v3) & MASK64
        v3 = rot(v3, 16) ^ v2
        v0 = rot(v0, 32)
        v2 = (v2 + v1) & MASK64
        v1 = rot(v1, 17) ^ v2
        v0 = (v0 + v3) & MASK64
        v3 = rot(v3, 21) ^ v0
        v2 = rot(v2, 32)

    v0 = k0 ^ 0x736F6D6570736575
    v1 = k1 ^ 0x646F72616E646F6D
    v2 = k0 ^ 0x6C7967656E657261
    v3 = k1 ^ 0x7465646279746573

    b = (len(data) & MASK64) << 56
    blocks = len(data) // 8
    for i in range(blocks):
        mi = int.from_bytes(data[i * 8 : (i + 1) * 8], "little")
        v3 ^= mi
        sipround()
        v0 ^= mi
    tail = data[blocks * 8 :]
    for i, byte in enumerate(tail):
        b |= byte << (8 * i)
    v3 ^= b
    sipround()
    v0 ^= b
    v2 ^= 0xFF
    for _ in range(3):
        sipround()
    return (v0 ^ v1 ^ v2 ^ v3) & MASK64


def write_hash_table(path, inputs, encode, storage_bytes):
    """One `# key <k0> <k1>` section per seed; rows hold input and the hash
    CPython 3.12 computes under that key. The generator's own SipHash port
    must reproduce every live value before a row is written."""
    lines = []
    for seed in HASH_SEEDS:
        k0, k1 = key_for_seed(seed)
        source = ""
        for value in inputs:
            source += f"print(hash({value!r}))\n"
        hashes = [int(line) for line in run_probe(source, seed=seed).splitlines()]
        if len(hashes) != len(inputs):
            sys.exit(f"hash probe for seed {seed} returned {len(hashes)} lines")
        for value, live in zip(inputs, hashes):
            raw = storage_bytes(value)
            # _Py_HashBytes returns 0 for the empty input before hashing
            own = 0 if not raw else siphash13(k0, k1, raw)
            own_signed = own - (1 << 64) if own >= (1 << 63) else own
            if own_signed != live:
                sys.exit(
                    f"siphash13 cross-check failed for {value!r} seed {seed}: "
                    f"own {own_signed} live {live}"
                )
        lines.append(f"# key {k0} {k1}\n")
        for value, live in zip(inputs, hashes):
            lines.append(f"{encode(value)}\t{live}\n")
    path.write_text("".join(lines), encoding="utf-8")


def str_storage_bytes(value):
    """The bytes CPython's string hash runs over: the compact PEP 393
    storage of a literal (1-byte, 2-byte or 4-byte kind, little-endian)."""
    if not value:
        return b""
    top = max(map(ord, value))
    if top < 0x100:
        return value.encode("latin-1")
    if top < 0x10000:
        return b"".join(bytes([ord(c) & 0xFF, ord(c) >> 8]) for c in value)
    return value.encode("utf-32-le")


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

    # str methods ------------------------------------------------------
    rows = []
    for method, call_args in STR_METHOD_CASES:
        results = probe_column(STR_INPUTS, method, call_args)
        for value, result in zip(STR_INPUTS, results):
            rows.append(
                [method] + [enc_arg(a) for a in call_args] + [esc(value), result]
            )
    write_rows(out / "str_methods.tsv", "# method [args...] input result\n", rows)

    # bytes methods ----------------------------------------------------
    rows = []
    for method, call_args in BYTE_METHOD_CASES:
        results = probe_column(BYTE_INPUTS, method, call_args)
        for value, result in zip(BYTE_INPUTS, results):
            rows.append(
                [method]
                + [enc_arg(a) for a in call_args]
                + [enc_bytes(value), result]
            )

    # fromhex (class call), maketrans+translate, hex, decode append
    for text in FROMHEX_INPUTS:
        result = probe_column([b""], "fromhex", (text,))[0]
        rows.append(["fromhex", enc_arg(text), enc_bytes(b""), result])
    for frm, to, delete in MAKETRANS_CASES:
        for data in BYTE_INPUTS:
            result = plain_probe(
                f"r = {data!r}.translate("
                f"bytes.maketrans({frm!r}, {to!r}), {delete!r})"
            )
            rows.append(
                [
                    "translate_maketrans",
                    enc_bytes(frm),
                    enc_bytes(to),
                    enc_bytes(delete),
                    enc_bytes(data),
                    result,
                ]
            )
    for data in HEX_INPUTS:
        results = probe_column([data], "hex", ())
        for sep, group in HEX_CASES:
            with_sep = probe_column([data], "hex", (sep, group))[0]
            rows.append(
                ["hex", enc_arg(sep), enc_arg(group), enc_bytes(data), with_sep]
            )
        rows.append(["hex", "", "", enc_bytes(data), results[0]])
    for data in DECODE_INPUTS:
        for encoding, errors in DECODE_CASES:
            # errors=None means "not passed": the C API takes NULL for the
            # default, and the method-level None check is arg parsing
            call = (
                f".decode({encoding!r})"
                if errors is None
                else f".decode({encoding!r}, {errors!r})"
            )
            result = plain_probe(f"r = {data!r}{call}")
            rows.append(
                ["decode", enc_arg(encoding), enc_arg(errors), enc_bytes(data), result]
            )
    write_rows(out / "bytes_methods.tsv", "# method [args...] input result\n", rows)

    # str decode (codec level: PyUnicode_Decode) ------------------------
    rows = []
    for data in DECODE_INPUTS:
        for encoding, errors in DECODE_CASES:
            call = (
                f".decode({encoding!r})"
                if errors is None
                else f".decode({encoding!r}, {errors!r})"
            )
            result = plain_probe(f"r = {data!r}{call}")
            rows.append([enc_bytes(data), encoding, enc_arg(errors), result])
    write_rows(out / "str_decode.tsv", "# data encoding errors result\n", rows)

    # str encode (PyUnicode_AsEncodedString) ----------------------------
    rows = []
    encode_inputs = STR_CORE + STR_LATIN1 + [" ", "\U0001f600", "ǅ", "ß"]
    for value in encode_inputs:
        for encoding, errors in ENCODE_CASES:
            call = (
                f".encode({encoding!r})"
                if errors is None
                else f".encode({encoding!r}, {errors!r})"
            )
            result = plain_probe(f"r = {value!r}{call}")
            rows.append([esc(value), encoding, enc_arg(errors), result])
    write_rows(out / "str_encode.tsv", "# value encoding errors result\n", rows)

    # %-formatting ------------------------------------------------------
    rows = [[esc(fmt), fmt_arg(arg), fmt_probe(fmt, arg)] for fmt, arg in STR_FORMAT_CASES]
    write_rows(out / "str_format.tsv", "# format arg result\n", rows)

    rows = [
        [enc_bytes(fmt), fmt_arg(arg), fmt_probe(fmt, arg)]
        for fmt, arg in BYTES_FORMAT_CASES
    ]
    write_rows(out / "bytes_format.tsv", "# format arg result\n", rows)

    # format-spec mini-language -----------------------------------------
    rows = []
    for value, spec in FORMAT_SPEC_CASES:
        result = plain_probe(f"r = format({value!r}, {spec!r})")
        rows.append([esc(value), esc(spec), result])
    write_rows(out / "str_format_spec.tsv", "# value spec result\n", rows)

    # repr ---------------------------------------------------------------
    def repr_of(value):
        printed = run_probe(f"print(repr(repr({value!r})))\n").strip("\n")
        return "s:" + esc(eval(printed))

    rows = [[esc(v), repr_of(v)] for v in STR_REPR_INPUTS]
    write_rows(out / "str_repr.tsv", "# value repr\n", rows)

    rows = [[enc_bytes(v), repr_of(v)] for v in BYTE_REPR_INPUTS]
    rows.append(["B:" + enc_bytes(b"\x00\xff a"), repr_of(bytearray(b"\x00\xff a"))])
    write_rows(out / "bytes_repr.tsv", "# value repr\n", rows)

    # hashes -------------------------------------------------------------
    write_hash_table(out / "str_hash.tsv", HASH_INPUTS_STR, esc, str_storage_bytes)
    write_hash_table(out / "bytes_hash.tsv", HASH_INPUTS_BYTES, enc_bytes, lambda v: v)

    print(f"wrote oracle tables to {out}")


if __name__ == "__main__":
    main()

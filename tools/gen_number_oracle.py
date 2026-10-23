"""CPython 3.12 number oracle for the coil-rt object tests (spec 8.1, 8.2, 8.5).

Computes int, float, complex and bool arithmetic, conversion, formatting and
hash values with the pinned interpreter and writes them as checked-in test
data (tab-separated, one case per line) for native/crates/coil-rt/tests/.
This module refuses to run on anything but CPython 3.12, the pinned target
whose semantics the runtime ports; it uses the C locale throughout so the
'n' presentation type matches the runtime's pre-locale behavior.

Output files (under --out, default native/crates/coil-rt/tests/data):

  int_arithmetic.tsv      int operations through the number slots
  int_convert.tsv         int<->str under the digit limit, int<->double
  int_hash.tsv            hash() of ints
  float_arithmetic.tsv    float operations, operands as u64 bit patterns
  float_repr.tsv          repr and the PyOS_double_to_string code table
  float_format.tsv        format(x, spec) and its errors
  float_hash.tsv          hash() of floats
  complex_arithmetic.tsv  complex operations, parts as u64 bit patterns
  complex_repr_format.tsv complex repr and format(x, spec)
  complex_hash.tsv        hash() of complex numbers

The last column encodes the result; the Rust reader uses the same encoding:

  i:<decimal>          int
  b:0 | b:1            bool
  f:<16 hex digits>    float as its u64 bit pattern
  c:<re>,<im>          complex, both parts as u64 bit patterns
  e:<Type>:<message>   exception with its exact message
  anything else        a formatted string result

Float and complex `pow` go through the platform libm (`pow`, plus
`hypot`, `atan2`, `exp`, `log`, `cos` and `sin` for complex exponents),
whose answers on half-way inputs can differ by one ulp between builds;
CPython's own results for them are not bit-reproducible across
platforms. The Rust harness compares those rows with a one-ulp
allowance per component and every other row exactly.
"""

import struct
import sys
from math import isnan

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_number_oracle.py must run on CPython 3.12 (the pinned target)")


def bits(x):
    return f"{struct.unpack('<Q', struct.pack('<d', x))[0]:016X}"


def f64(h):
    return struct.unpack("<d", struct.pack("<Q", int(h, 16)))[0]


# Ints crossing the 30-bit digit boundaries and the compact-tag limits.
INT_CORE = [
    "0", "1", "-1", "5", "-5", "256", "1073741823", "1073741824",
    "-1073741824", "1152921504606846976", str(7**11), str(-(7**11)),
]
INT_BIG = [
    "10", "-10", str(2**30 + 1), str(-(2**30 + 1)), str(2**60),
    str(2**61 - 1), str(2**61), str(-(2**61)), str(2**100),
    str(-(2**100)), str(10**30), str(7**50), str(-(7**50)),
    str(0xDEADBEEFCAFEF00D << 99), str(123456789 * 10**40),
    "1000000000000000000",
]
SHIFT_BY = ["0", "1", "29", "30", "31", "59", "60", "63", "64", "-1", "1000"]
POW_BY = ["0", "1", "2", "3", "5", "-1", "-2", "-5", "10"]
POW_MOD = ["5", "97", "1", "0", "-7", "6"]

FLOAT_CORE = [
    "0000000000000000",  # 0.0
    "8000000000000000",  # -0.0
    "3FF0000000000000",  # 1.0
    "BFF0000000000000",  # -1.0
    "3FE0000000000000",  # 0.5
    "4004000000000000",  # 2.5
    "400C000000000000",  # 3.5
    "3FB999999999999A",  # 0.1
    "3FD5555555555555",  # 1/3
    "4059000000000000",  # 100.0
    "4341C37937E08000",  # 1.2345678901234568e18
    "7FF0000000000000",  # inf
]
FLOAT_ALL = FLOAT_CORE + [
    "FFF0000000000000",  # -inf
    "7FF8000000000000",  # nan
    "0000000000000001",  # 5e-324
    "0010000000000000",  # min normal
    "7FEFFFFFFFFFFFFF",  # max double
    "3FF0000000000001",  # 1 + ulp
    "4340000000000000",  # 2**53
    "433FFFFFFFFFFFFF",  # 2**53 - 1
    "C1478E74B0B958B0",  # -(10**100) rounded
    "44B52D02C7E14AF6",  # 1e23
    "3F66666666666666",  # 0.006
    "4156BCC41E900000",  # 1234567.5
    "C004000000000000",  # -2.5
    "4237E48337EA7A50",  # 12345678.9012
]
FLOAT_POW_Y = [
    "0000000000000000", "3FF0000000000000", "4000000000000000",
    "BFF0000000000000", "3FE0000000000000", "BFE0000000000000",
    "4008000000000000", "4059000000000000", "7FF0000000000000",
    "FFF0000000000000", "7FF8000000000000", "3FD5555555555555",
]

FLOAT_FORMAT_VALUES = [
    "0000000000000000", "8000000000000000", "3FF0000000000000",
    "BFF0000000000000", "3FE0000000000000", "4004000000000000",
    "400C000000000000", "3FB999999999999A", "3FD5555555555555",
    "3FF8000000000000", "C004000000000000", "3F66666666666666",
    "4156BCC41E900000", "44B52D02C7E14AF6", "7FF0000000000000",
    "FFF0000000000000", "7FF8000000000000", "0000000000000001",
    "4341C37937E08000", "4059000000000000", "C059000000000000",
]
FLOAT_SPECS = [
    "", "d", "e", "E", "f", "F", "g", "G", "%",
    ".0f", ".1f", ".2f", ".17g", ".0e", ".3e", ".10f", ".0%",
    "z", "z.0f", "zf", "z.2f", "z,.3f", "z<8.2f",
    "+", "+f", "+.2e", "-", " ", " f", "+e", "+.0%",
    "#", "#g", "#.0f", "#e", "#.0e", "#o",
    "0>8", ">8", "<8", "^9", "08.3f", "+08.2f", "08", "0=8", "=8",
    ">+012.4f", "0>12.4e",
    ",", ",.2f", ",f", "_", "_f", "_,.4f", ",.2e", "_.3%",
    "n", ".2n", "10.2f", "10", ">10.3e", "^11.4g", "<15,.2f", "12,",
    "8.2f", ".#f", ".f", ">10,f", "x", "s", "q", "10.3q", "zz", "5=",
    "=<8", ">08", "000f", "+00.0f", "e<12", "*>12.4g", "025,", "033.20f",
]

COMPLEX_VALUES = [
    ("0000000000000000", "0000000000000000"),
    ("3FF0000000000000", "0000000000000000"),
    ("0000000000000000", "3FF0000000000000"),
    ("BFF0000000000000", "0000000000000000"),
    ("3FF8000000000000", "4004000000000000"),
    ("BFE0000000000000", "3FD0000000000000"),
    ("4004000000000000", "C000000000000000"),
    ("7FF0000000000000", "0000000000000000"),
    ("0000000000000000", "7FF8000000000000"),
    ("0000000000000000", "BFF0000000000000"),
    ("4341C37937E08000", "BFD3333333333333"),
]
COMPLEX_POW_Y = [
    ("4000000000000000", "0000000000000000"),
    ("BFF0000000000000", "0000000000000000"),
    ("3FE0000000000000", "0000000000000000"),
    ("0000000000000000", "3FE0000000000000"),
    ("4008000000000000", "0000000000000000"),
    ("C000000000000000", "0000000000000000"),
    ("4059000000000000", "0000000000000000"),
    ("0000000000000000", "0000000000000000"),
    ("3FE0000000000000", "3FE0000000000000"),
    ("0000000000000000", "3FF0000000000000"),
]
COMPLEX_SPECS = [
    "", "f", ".2f", ".0f", ">20", "^20", "<20", "+f", "e", ".3e", "g",
    "n", "0f", "=8", "d", "s", "%", ".1", "8.2f", "+^15.3g", "<10,.2f",
    "#f", "z.0f", "z", "0=10", "*>12", ".0e", "<+014.2f",
]


def encode(fn):
    try:
        value = fn()
        if isinstance(value, bool):
            return f"b:{int(value)}"
        if isinstance(value, int):
            return f"i:{value}"
        if isinstance(value, float):
            return "f:nan" if isnan(value) else f"f:{bits(value)}"
        if isinstance(value, complex):
            def part(x):
                return "nan" if isnan(x) else bits(x)
            return f"c:{part(value.real)},{part(value.imag)}"
        return value
    except Exception as exc:  # recorded verbatim, including its message
        return f"e:{type(exc).__name__}:{exc}"


def gen_int_arithmetic():
    rows = []
    a_core = [int(s) for s in INT_CORE]
    a_big = [int(s) for s in INT_BIG]
    partners = [int(s) for s in INT_CORE[:8]] + [int(s) for s in INT_BIG[:6]]
    for op, fn in [("+", lambda a, b: a + b), ("-", lambda a, b: a - b),
                   ("*", lambda a, b: a * b), ("&", lambda a, b: a & b),
                   ("|", lambda a, b: a | b), ("^", lambda a, b: a ^ b)]:
        for a in a_core:
            for b in partners:
                rows.append((op, str(a), str(b), encode(lambda a=a, b=b, fn=fn: fn(a, b))))
        for a in a_big[:6]:
            for b in a_core[:6]:
                rows.append((op, str(a), str(b), encode(lambda a=a, b=b, fn=fn: fn(a, b))))
    for op, fn in [("//", lambda a, b: a // b), ("%", lambda a, b: a % b)]:
        for a in a_core + a_big[:6]:
            for b in [0, 1, -1, 3, -3, 256, 1073741824, 1152921504606846976]:
                rows.append((op, str(a), str(b), encode(lambda a=a, b=b, fn=fn: fn(a, b))))
    for a in a_core:
        for e in [int(s) for s in POW_BY]:
            rows.append(("pow", str(a), str(e),
                         encode(lambda a=a, e=e: a ** e if e >= 0 else a ** e)))
        rows.append(("pow", str(a), "-1", encode(lambda a=a: a ** -1)))
    for a in ["2", "3", "10", "7", "12345"]:
        for e in ["2", "5", "10", "-2", "-5"]:
            rows.append(("pow", a, e, encode(lambda a=int(a), e=int(e): a ** e)))
    for a in ["2", "3", "7", "123456789", "5", "1152921504606846977"]:
        for m in POW_MOD:
            for e in ["1", "2", "5", "-1", "-3", "10"]:
                rows.append(("pow3", a, e, m,
                             encode(lambda a=int(a), e=int(e), m=int(m): pow(a, e, m))))
    for a in a_core + a_big[:6]:
        for k in SHIFT_BY:
            rows.append(("<<", str(a), k, encode(lambda a=a, k=int(k): a << k)))
            rows.append((">>", str(a), k, encode(lambda a=a, k=int(k): a >> k)))
    for a in a_core + a_big:
        rows.append(("~", str(a), "", encode(lambda a=a: ~a)))
        rows.append(("neg", str(a), "", encode(lambda a=a: -a)))
        rows.append(("abs", str(a), "", encode(lambda a=a: abs(a))))
    cmp_partners = ["0", "1", "-1", "5", "-5", str(2**61), str(-(2**61)), str(7**11)]
    for name, op in [("lt", "<"), ("le", "<="), ("eq", "=="),
                     ("ne", "!="), ("gt", ">"), ("ge", ">=")]:
        for a in a_core + a_big[:6]:
            for b in cmp_partners:
                rows.append((name, str(a), b,
                             encode(lambda a=a, b=int(b), op=op: eval(f"a {op} b"))))
    return rows


def gen_int_convert():
    rows = []
    for a in INT_CORE + INT_BIG[:8] + ["631", "-631", str(10**640)]:
        v = int(a)
        rows.append(("to_str", a, "10", "0", str(v)))
        rows.append(("to_str", a, "10", "1", str(v)))
        for base, withp, without in [(2, bin, format(v, "b")), (8, oct, format(v, "o")),
                                     (16, hex, format(v, "x"))]:
            rows.append(("to_str", a, str(base), "1", withp(v)))
            rows.append(("to_str", a, str(base), "0", without))
    rows.append(("to_str", str(10**4300 - 1), "10", "0",
                 encode(lambda: str(10**4300 - 1))))
    rows.append(("to_str", str(10**640), "10", "0", str(10**640)))
    rows.append(("to_str", str(-(10**640)), "10", "0", str(-(10**640))))
    rows.append(("to_str", hex(2**20000), "16", "0", format(2**20000, "x")))
    rows.append(("to_str", hex(2**20000), "16", "1", hex(2**20000)))
    rows.append(("to_str", hex(2**20000), "10", "0", encode(lambda: str(2**20000))))
    parse_cases = [
        ("10", "42"), ("10", " 42\t"), ("10", "\n42 "), ("10", "+42"), ("10", "-42"),
        ("10", "1_000"), ("10", "1__0"), ("10", "_10"), ("10", "10_"),
        ("10", "0x1f"), ("10", ""), ("10", " "), ("10", "-"), ("10", "12.5"),
        ("10", "1" * 4301), ("10", "9" * 640), ("10", "9" * 4300),
        ("16", "0x1f"), ("16", "1F"), ("16", " 0X1_f "), ("16", "0x"), ("16", "g"),
        ("8", "0o17"), ("8", "17"), ("8", "18"),
        ("2", "0b101"), ("2", "101"), ("2", "0b102"),
        ("0", "0x1f"), ("0", "0o17"), ("0", "0b101"), ("0", "17"), ("0", "0"),
        ("0", " 0x10 "), ("0", "0x"), ("0", "08"), ("0", "0b_1"), ("0", "1_0"),
        ("0", "f"), ("36", "zaza"), ("36", "Z9"), ("36", "z1z"),
        ("37", "1"), ("1", "1"), ("0", "1" * 4301),
        ("16", "f" * 5000), ("10", "١٢"),
    ]
    for base, text in parse_cases:
        rows.append(("from_str", base, text.replace("\t", "\\t").replace("\n", "\\n"),
                     from_str_expected(base, text)))
    fd_values = [
        "0000000000000000", "8000000000000000", "3FE0000000000000",
        "BFE0000000000000", "3FF8000000000000", "4004000000000000",
        "C004000000000000", "400C000000000000", "4340000000000000",
        "433FFFFFFFFFFFFF", "4340000000000001",
        "43E0000000000000", "7FF0000000000000", "FFF0000000000000",
        "7FF8000000000000", "C3E0000000000000", "43E158E4F209E000",
    ]
    for h in fd_values:
        rows.append(("from_double", h, encode(lambda v=f64(h): int(v))))
    for a in ["0", "1", "-1", "5", "256", "2**53", "-(2**53)", "2**53+1",
              "2**53+3", "2**60", "2**64", "10**30", "7**50", "2**1023",
              "2**1024", "-(2**1024)", "10**400", "1152921504606846977"]:
        v = eval(a)
        rows.append(("as_double", str(v), "", encode(lambda v=v: float(v))))
    return rows


def from_str_expected(base, text):
    """The expected result of CPython's PyLong_FromString (the byte API),
    not of the int() constructor: its error messages carry the sniffed
    working base for base 0, its own wording for the base range, and any
    non-ASCII byte fails the digit table.
    """
    base = int(base)
    if (base != 0 and base < 2) or base > 36:
        return "e:ValueError:int() arg 2 must be >= 2 and <= 36"
    if not text.isascii():
        return f"e:ValueError:invalid literal for int() with base {base}: '{text}'"
    if base == 0:
        body = text.lstrip(" \t\n\v\f\r")
        if body[:1] in ("+", "-"):
            body = body[1:]
        c0, c1 = (body[:2] + "\0\0")[:2]
        if c0 != "0":
            sniffed = 10
        elif c1 in "xX":
            sniffed = 16
        elif c1 in "oO":
            sniffed = 8
        elif c1 in "bB":
            sniffed = 2
        else:
            # C-style octal: the error, if any, reports base 0.
            return encode(lambda: int(text, 0))
        return encode(lambda: int(text, sniffed))
    return encode(lambda: int(text, base))


def gen_int_hash():
    values = [
        "0", "1", "-1", "2", "-2", "5", "-5", "16", "17", "256",
        str(2**61 - 1), str(2**61), str(-(2**61)), str(2**61 - 2), str(2**61 + 1),
        str(2**62), str(-(2**62)), str(2**100), str(-(2**100)), str(10**50),
        str(7**50), str(0xDEADBEEFCAFEF00D << 99), str(10**30),
        "1152921504606846976", "2305843009213693968", "6148914691236517205",
    ]
    return [("hash", v, str(hash(int(v)))) for v in values]


def gen_float_arithmetic():
    rows = []
    core = FLOAT_CORE
    for op, fn in [("+", lambda a, b: a + b), ("-", lambda a, b: a - b),
                   ("*", lambda a, b: a * b), ("/", lambda a, b: a / b),
                   ("%", lambda a, b: a % b), ("//", lambda a, b: a // b)]:
        for a in core:
            for b in core:
                rows.append((op, a, b, encode(lambda a=a, b=b, fn=fn: fn(f64(a), f64(b)))))
    extra = ["FFF0000000000000", "7FF8000000000000", "0000000000000001",
             "7FEFFFFFFFFFFFFF", "44B52D02C7E14AF6"]
    for op, fn in [("+", lambda a, b: a + b), ("*", lambda a, b: a * b),
                   ("/", lambda a, b: a / b), ("%", lambda a, b: a % b),
                   ("//", lambda a, b: a // b)]:
        for a in extra:
            for b in ["0000000000000000", "8000000000000000", "3FF0000000000000",
                      "7FF0000000000000", "FFF0000000000000", "7FF8000000000000"]:
                rows.append((op, a, b, encode(lambda a=a, b=b, fn=fn: fn(f64(a), f64(b)))))
    for x in FLOAT_ALL:
        for y in FLOAT_POW_Y:
            rows.append(("pow", x, y, encode(lambda x=x, y=y: f64(x) ** f64(y))))
    return rows


def gen_float_repr():
    rows = []
    for h in FLOAT_ALL:
        v = f64(h)
        rows.append(("repr", h, "r", "0", repr(v)))
        # The precision recorded is the one float.__format__ hands to
        # PyOS_double_to_string: after-the-point digits for e/f, total
        # significant digits for g.
        for code, prec in [("e", 7), ("e", 1), ("E", 3), ("f", 6), ("f", 0),
                           ("f", 1), ("F", 2), ("g", 6), ("g", 1), ("G", 4),
                           ("g", 17), ("e", 17), ("e", 0)]:
            spec = f".{prec}{code}"
            rows.append(("pyos", h, code, str(prec), format(v, spec)))
    return rows


def gen_float_format():
    return [("format", h, spec, encode(lambda v=f64(h), s=spec: format(v, s)))
            for h in FLOAT_FORMAT_VALUES for spec in FLOAT_SPECS]


def gen_float_hash():
    values = FLOAT_ALL + ["3FF0000000000000", "BFF8000000000000",
                          "40FE240000000000", "463021709E402823"]
    # hash(nan) is the object's pointer hash in 3.12 (pyhash.c), so it has
    # no reproducible value to freeze.
    values = [h for h in values if not isnan(f64(h))]
    return [("hash", h, str(hash(f64(h)))) for h in values]


def gen_complex_arithmetic():
    rows = []
    for op, fn in [("+", lambda a, b: a + b), ("-", lambda a, b: a - b),
                   ("*", lambda a, b: a * b), ("/", lambda a, b: a / b)]:
        for ar, ai in COMPLEX_VALUES:
            for br, bi in COMPLEX_VALUES:
                rows.append((op, ar, ai, br, bi,
                             encode(lambda ar=ar, ai=ai, br=br, bi=bi, fn=fn:
                                    fn(complex(f64(ar), f64(ai)),
                                       complex(f64(br), f64(bi))))))
    for ar, ai in COMPLEX_VALUES:
        for br, bi in COMPLEX_POW_Y:
            rows.append(("pow", ar, ai, br, bi,
                         encode(lambda ar=ar, ai=ai, br=br, bi=bi:
                                complex(f64(ar), f64(ai)) ** complex(f64(br), f64(bi)))))
    return rows


def gen_complex_repr_format():
    rows = []
    for ar, ai in COMPLEX_VALUES:
        v = complex(f64(ar), f64(ai))
        rows.append(("repr", ar, ai, "", repr(v)))
        for spec in COMPLEX_SPECS:
            rows.append(("format", ar, ai, spec, encode(lambda v=v, s=spec: format(v, s))))
    return rows


def gen_complex_hash():
    # A NaN part makes the hash identity-based (see gen_float_hash).
    values = [(ar, ai) for ar, ai in COMPLEX_VALUES
              if not (isnan(f64(ar)) or isnan(f64(ai)))]
    return [("hash", ar, ai, str(hash(complex(f64(ar), f64(ai)))))
            for ar, ai in values]


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else None
    if out_dir is None:
        import pathlib
        out_dir = pathlib.Path(__file__).resolve().parents[1] / \
            "native/crates/coil-rt/tests/data"
    tables = [
        ("int_arithmetic.tsv", "op [a] [b] [mod] result", gen_int_arithmetic),
        ("int_convert.tsv", "kind arg1 arg2 [arg3] result", gen_int_convert),
        ("int_hash.tsv", "op value result", gen_int_hash),
        ("float_arithmetic.tsv", "op a b result", gen_float_arithmetic),
        ("float_repr.tsv", "kind bits code precision result", gen_float_repr),
        ("float_format.tsv", "kind bits spec result", gen_float_format),
        ("float_hash.tsv", "op bits result", gen_float_hash),
        ("complex_arithmetic.tsv", "op ar ai br bi result", gen_complex_arithmetic),
        ("complex_repr_format.tsv", "kind ar ai spec result", gen_complex_repr_format),
        ("complex_hash.tsv", "op ar ai result", gen_complex_hash),
    ]
    for name, header, gen in tables:
        rows = gen()
        path = f"{out_dir}/{name}"
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write(f"# {header}\n")
            seen = set()
            for row in rows:
                if row in seen:
                    continue
                seen.add(row)
                f.write("\t".join(row) + "\n")
        print(f"{name}: {len(seen)} rows")


if __name__ == "__main__":
    main()

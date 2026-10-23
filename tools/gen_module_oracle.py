"""CPython 3.12 oracle for the coil-rt native module tests (spec 10.6).

Calls the built-in modules' functions with the pinned interpreter and
writes the results as checked-in test data (tab-separated, one case per
line) for native/crates/coil-rt/tests/. This module refuses to run on
anything but CPython 3.12, the pinned target whose semantics the runtime
ports. The runtime has no str type yet, so every argument and result is
one of the scalar/composite tokens below (no string arguments; keyword
arguments are exercised where the runtime's calling machinery can pass
them, which today is nowhere, so keyword-only parameters run at their
defaults).

Output files (under --out, default native/crates/coil-rt/tests/data):

  math_module.tsv       math functions, values and exact error messages
  cmath_module.tsv      cmath functions over the complex protocol
  binascii_module.tsv   binascii codecs and checksums over bytes

Columns: function name, one column per argument, the result. Composite
arguments are tuples; the encoding is shared with the Rust reader:

  i:<decimal>          int
  b:0 | b:1            bool
  y:<hex>              bytes
  f:<16 hex digits>    float as its u64 bit pattern (f:nan for NaN)
  c:<re>,<im>          complex from two float encodings
  T:<enc>,<enc>,...    tuple of the other encodings
  e:<Type>:<message>   exception with its exact message
  none                 None

Float functions whose C implementation is the platform libm (exp, log,
pow, the trigonometric and hyperbolic families, erf/erfc through their
internal exp, gamma/lgamma likewise) are not bit-reproducible across
platforms; the Rust harness compares those rows with a one-ulp allowance
per component and every other row exactly, as gen_number_oracle.py does.
"""

import math
import struct
import sys
from math import isnan

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_module_oracle.py must run on CPython 3.12 (the pinned target)")


def bits(x):
    return f"{struct.unpack('<Q', struct.pack('<d', x))[0]:016X}"


def f64(token):
    if token == "nan":
        return float("nan")
    return struct.unpack("<d", struct.pack("<Q", int(token, 16)))[0]


def fenc(x):
    return "nan" if isnan(x) else bits(x)


def enc(value):
    if value is None:
        return "none"
    if isinstance(value, bool):
        return f"b:{int(value)}"
    if isinstance(value, int):
        return f"i:{value}"
    if isinstance(value, float):
        return f"f:{fenc(value)}"
    if isinstance(value, complex):
        return f"c:{fenc(value.real)},{fenc(value.imag)}"
    if isinstance(value, bytes):
        return "y:" + value.hex()
    if isinstance(value, tuple):
        return "T:" + ",".join(enc(item) for item in value)
    raise AssertionError(f"unencodable value {value!r}")


def name_of(qualified):
    # "math.sqrt()" -> "sqrt()"
    return qualified.split(".", 1)[1]


def encode(fn, drop_prefix=None):
    try:
        return enc(fn())
    except Exception as exc:  # recorded verbatim, including its message
        text = str(exc)
        if drop_prefix is not None and text.startswith(drop_prefix):
            # The METH_O/METH_NOARGS arity errors render the function
            # through __qualname__: "math.sqrt() takes exactly one
            # argument (0 given)". The runtime's C function objects
            # carry no module name yet (no str or module objects, the
            # spec 10.6 unicode seam), so those messages keep only the
            # function name here and the harness compares them exactly.
            text = name_of(drop_prefix) + text[len(drop_prefix):]
        return f"e:{type(exc).__name__}:{text}"


# The float grid from gen_number_oracle.py plus branch-cut and pole
# neighbours for erf/erfc/gamma/lgamma.
FLOATS = [
    f64(h) for h in [
        "0000000000000000", "8000000000000000", "3FF0000000000000",
        "BFF0000000000000", "3FE0000000000000", "4004000000000000",
        "400C000000000000", "3FB999999999999A", "3FD5555555555555",
        "4059000000000000", "4341C37937E08000", "7FF0000000000000",
        "FFF0000000000000", "7FF8000000000000", "0000000000000001",
        "0010000000000000", "7FEFFFFFFFFFFFFF", "3FF0000000000001",
        "4340000000000000", "433FFFFFFFFFFFFF", "44B52D02C7E14AF6",
        "3F66666666666666", "4156BCC41E900000", "C004000000000000",
        "4237E48337EA7A50",
    ]
] + [
    -0.5, 0.5, 1.5, -1.5, 2.0, -2.0, 0.84375, 1.25, 5.9, 6.0, 28.0,
    27.9, 170.0, 170.5, 171.0, -170.5, -0.5, 0.1, 1e-10, 1e-300,
    -100.0, 100.0, 1e15, 0.25,
]

INTS = [
    0, 1, -1, 2, 3, 5, -5, 10, 13, 21, 22, 25, 26, 100, 1000,
    1073741823, 1073741824, 2**30, 2**61, 2**62, 2**63 - 1,
    10**18, 10**30, 7**11, 7**50, 2**100, -(2**100),
]


def rows_unary():
    one_arg = [
        "acos", "acosh", "asin", "asinh", "atan", "atanh", "cbrt", "cos",
        "cosh", "degrees", "erf", "erfc", "exp", "exp2", "expm1", "fabs",
        "floor", "gamma", "lgamma", "log1p", "log2", "log10", "radians",
        "sin", "sinh", "sqrt", "tan", "tanh", "trunc", "isfinite",
        "isinf", "isnan", "ulp",
    ]
    for name in one_arg:
        fn = getattr(math, name)
        for x in FLOATS:
            yield (name, f"f:{bits(x)}", encode(lambda fn=fn, x=x: fn(x)))
    # floor/ceil/trunc accept ints unchanged; isqrt is ints only.
    for name in ["floor", "ceil", "trunc", "isqrt"]:
        fn = getattr(math, name)
        for x in INTS:
            yield (name, enc(x), encode(lambda fn=fn, x=x: fn(x)))
    for x in [0, 1, 5, 12, 13, 20, 21, 22, 170, 171, 172, 2**63]:
        yield ("factorial", enc(x), encode(lambda x=x: math.factorial(x)))


def rows_binary():
    two_arg = ["atan2", "copysign", "fmod", "pow", "remainder"]
    partners = FLOATS[::3] + [-0.5, 0.5, 2.5, -2.5, 3.0, -3.0]
    for name in two_arg:
        fn = getattr(math, name)
        for x in FLOATS[::2]:
            for y in partners:
                yield (
                    name, f"f:{bits(x)}", f"f:{bits(y)}",
                    encode(lambda fn=fn, x=x, y=y: fn(x, y)),
                )
    # log takes one or two arguments.
    for x in FLOATS:
        yield ("log", f"f:{bits(x)}", encode(lambda x=x: math.log(x)))
    for x in FLOATS[::2]:
        for base in [0.5, 2.0, 3.0, 10.0, math.e, -1.0]:
            yield (
                "log", f"f:{bits(x)}", f"f:{bits(base)}",
                encode(lambda x=x, base=base: math.log(x, base)),
            )
    for x in [2, 10, 7, 2**100, 10**30]:
        for base in [2, 3, 10, 7]:
            yield (
                "log", enc(x), enc(base),
                encode(lambda x=x, base=base: math.log(x, base)),
            )
    # ldexp takes a float and an int exponent.
    for x in FLOATS[::2]:
        for e in [0, 1, -1, 53, 1000, -1000, 1023, 1024, -1074, -1100]:
            yield (
                "ldexp", f"f:{bits(x)}", enc(e),
                encode(lambda x=x, e=e: math.ldexp(x, e)),
            )
    # hypot and isclose take any number of floats; nextafter exactly two.
    for x in FLOATS[::2]:
        yield ("hypot", f"f:{bits(x)}", encode(lambda x=x: math.hypot(x)))
        for y in partners:
            yield (
                "hypot", f"f:{bits(x)}", f"f:{bits(y)}",
                encode(lambda x=x, y=y: math.hypot(x, y)),
            )
            yield (
                "isclose", f"f:{bits(x)}", f"f:{bits(y)}",
                encode(lambda x=x, y=y: math.isclose(x, y)),
            )
            yield (
                "nextafter", f"f:{bits(x)}", f"f:{bits(y)}",
                encode(lambda x=x, y=y: math.nextafter(x, y)),
            )
    for x in FLOATS[::2]:
        for y in FLOATS[1::2]:
            yield (
                "nextafter", f"f:{bits(x)}", f"f:{bits(y)}",
                encode(lambda x=x, y=y: math.nextafter(x, y)),
            )
    # dist takes two equal-length points.
    for p, q in [
        ((1.0,), (2.0,)), ((0.0, 0.0), (3.0, 4.0)),
        ((1, 2, 3), (4, 6, 3)), ((-1.5, 2.5), (0.5, -0.5)),
        ((1e308, 1e308), (1e308, 1e308)),
        ((0.0,) * 5, (1.0,) * 5), ((float("nan"),), (1.0,)),
        ((1.0,), (2.0, 3.0)),
    ]:
        yield (
            "dist", enc(tuple(p)), enc(tuple(q)),
            encode(lambda p=tuple(p), q=tuple(q): math.dist(p, q)),
        )


def rows_integer():
    # comb and perm over a spread of n and k.
    ns = [0, 1, 2, 5, 10, 13, 20, 21, 22, 30, 64, 65, 100, 2**61,
          10**18, 7**20]
    small_ks = [0, 1, 2, 3, 5, 10, 13, 20, 21, 30, 64, 100, -1]
    for n in ns:
        # Beyond n the answers collapse to 0/1, and huge (n, k) pairs
        # would compute numbers with billions of digits.
        ks = small_ks + ([n + 1] if n <= 2**62 else [10**18 + 1])
        for k in ks:
            yield (
                "comb", enc(n), enc(k),
                encode(lambda n=n, k=k: math.comb(n, k)),
            )
            yield (
                "perm", enc(n), enc(k),
                encode(lambda n=n, k=k: math.perm(n, k)),
            )
    for n in ns[:6]:
        yield ("perm", enc(n), encode(lambda n=n: math.perm(n)))
    for n in [1.5, -1]:
        yield ("comb", enc(n), enc(2), encode(lambda n=n: math.comb(n, 2)))
        yield ("perm", enc(n), enc(2), encode(lambda n=n: math.perm(n, 2)))
    for k in [1.5, -1]:
        yield ("comb", enc(5), enc(k), encode(lambda k=k: math.comb(5, k)))
        yield ("perm", enc(5), enc(k), encode(lambda k=k: math.perm(5, k)))
    # gcd and lcm over argument counts from zero up.
    arg_sets = [
        (), (0,), (12,), (0, 0), (0, 9), (12, 18), (-12, 18), (12, -18),
        (-12, -18), (2**100, 7**50), (0, 2**100, 7**50),
        (10**18, 27, 14, 21), (1.5, 2),
    ]
    for args in arg_sets:
        row_args = [enc(a) for a in args]
        yield (
            "gcd", *row_args, encode(lambda args=args: math.gcd(*args))
        )
        yield (
            "lcm", *row_args, encode(lambda args=args: math.lcm(*args))
        )
    # isqrt's error shape (the runtime has no str type, so the float
    # case carries the TypeError family).
    for x in [-1, 1.5]:
        yield ("isqrt", enc(x), encode(lambda x=x: math.isqrt(x)))
    for x in [-1, 1.5, True, 2**63]:
        yield ("factorial", enc(x), encode(lambda x=x: math.factorial(x)))


def rows_sequences():
    lists = [
        (), (0.0,), (0.1,) * 10, (1.0, 2.0, 3.0),
        (1e16, 1.0, -1e16), (1e308, 1e308), (float("inf"), float("-inf")),
        (float("nan"), 1.0), (-0.0, 0.0), (2.0**53, 1.0, 0.5),
        (1, 2, 3), (True, 2), (1.5, 2), (-1.5, -2.5),
        (2.0**100, 4.0), (0.1, 0.2, 0.3),
    ]
    for items in lists:
        yield ("fsum", enc(items), encode(lambda i=items: math.fsum(i)))
        yield ("prod", enc(items), encode(lambda i=items: math.prod(i)))
    pairs = [
        ((1, 2, 3), (4, 5, 6)), ((1.5, 2.5), (0.5, -0.5)),
        ((1, 2), (1.5, 2)), ((True, 3), (2, 4)), ((2.0**100,), (1.0,)),
        ((2**100,), (3,)), ((1, 2), (1,)),
        ((float("inf"), 1.0), (1.0, 1.0)), ((float("nan"),), (1.0,)),
    ]
    for p, q in pairs:
        yield (
            "sumprod", enc(p), enc(q),
            encode(lambda p=p, q=q: math.sumprod(p, q)),
        )
    # Argument-count and type error shapes for the calling conventions.
    shapes = [
        ("fmod", ()), ("fmod", (1.0,)), ("fmod", (1.0, 2.0, 3.0)),
        ("hypot", ()), ("dist", ((1.0,),)),
        ("prod", ()), ("prod", ((1,), 2)),
        ("prod", ((1,), 2, 3)),
        ("isclose", (1.0,)), ("isclose", ()),
        ("isclose", (1.0, 2.0, 3.0, 4.0, 5.0)),
        ("nextafter", (1.0,)), ("nextafter", (1.0, 2.0, 3.0)),
        ("log", ()), ("log", (1.0, 2.0, 3.0)), ("gcd", (1.5,)),
        ("sqrt", ()), ("sqrt", (1.0, 2.0)),
        ("ldexp", (1.0,)), ("frexp", (float("inf"),)),
        ("ldexp", (1.0, 2**63)),
    ]
    for name, args in shapes:
        fn = getattr(math, name)
        yield (
            name, *(enc(a) if not isinstance(a, tuple) else enc(a) for a in args),
            encode(lambda fn=fn, a=args: fn(*a), drop_prefix=f"math.{name}()"),
        )


def rows_cmath():
    import cmath
    # The Annex G lattice: parts chosen around the branch cuts, the
    # overflow thresholds (CM_LARGE_DOUBLE and friends) and the
    # subnormal rescaling paths, times themselves.
    parts = [
        "0000000000000000", "8000000000000000", "3FF0000000000000",
        "BFF0000000000000", "3FE0000000000000", "BFE0000000000000",
        "4000000000000000", "C000000000000000", "7FF0000000000000",
        "FFF0000000000000", "7FF8000000000000", "FFF8000000000000",
        "0000000000000001", "0000000000000002", "8000000000000001",
        "7FEFFFFFFFFFFFFF", "4341C37937E08000", "4237E48337EA7A50",
        "557F5FCED603E8E1", "7FEFFFFFFFFFFFFE",
        "0362659B2DB5B899", "8362659B2DB5B899",
        "43E1580850953E5B", "C3E1580850953E5B",
        "6974E718D7D7625A",  # 1e200, above CM_SQRT_LARGE_DOUBLE
        "16687E92154EF7AC",  # 1e-200
        "00005C0AB9347ED7",  # 5e-310, subnormal
        # Each magnitude above with its sign flipped.
        "BFF0000000000000", "BFE0000000000000", "C000000000000000",
        "E974E718D7D7625A", "96687E92154EF7AC", "80005C0AB9347ED7",
        "C341C37937E08000", "D57F5FCED603E8E1",
    ]
    zs = [complex(f64(re), f64(im)) for re in parts for im in parts]
    one_arg = [
        "acos", "acosh", "asin", "asinh", "atan", "atanh", "cos",
        "cosh", "exp", "isfinite", "isinf", "isnan", "log10", "phase",
        "polar", "sin", "sinh", "sqrt", "tan", "tanh",
    ]
    for name in one_arg:
        fn = getattr(cmath, name)
        for z in zs:
            yield (name, enc(z), encode(lambda fn=fn, z=z: fn(z)))
    # log with and without a base.
    for z in zs[::2]:
        yield ("log", enc(z), encode(lambda z=z: cmath.log(z)))
        for base in [zs[1], zs[3], zs[8], zs[10], zs[5], complex(2.0, 1.0)]:
            yield (
                "log", enc(z), enc(base),
                encode(lambda z=z, base=base: cmath.log(z, base)),
            )
    # rect takes two doubles.
    for re in parts:
        for im in parts[::4]:
            yield (
                "rect", f"f:{re}", f"f:{im}",
                encode(lambda re=re, im=im: cmath.rect(f64(re), f64(im))),
            )
    # isclose over pairs of lattice points.
    for a in zs[::29]:
        for b in zs[7::29]:
            yield (
                "isclose", enc(a), enc(b),
                encode(lambda a=a, b=b: cmath.isclose(a, b)),
            )
    # The complex protocol accepts ints, bools and floats.
    for x in [0, 1, -1, 2, True, False, 2**100, 1.5, -0.0]:
        yield ("acos", enc(x), encode(lambda x=x: cmath.acos(x)))
        yield ("phase", enc(x), encode(lambda x=x: cmath.phase(x)))
    # Argument-count and type error shapes for the calling conventions.
    shapes = [
        ("acos", ()), ("acos", (1j, 2j)), ("sqrt", ()),
        ("log", ()), ("log", (1j, 2j, 3j)), ("rect", ()),
        ("rect", (1.0,)), ("rect", (1.0, 2.0, 3.0)),
        ("isclose", (1j,)), ("isclose", (1j, 2j, 3j)),
        ("isclose", (1j, 2j, 3j, 4j, 5j)),
        ("phase", (None,)), ("polar", (None,)),
    ]
    for name, args in shapes:
        fn = getattr(cmath, name)
        yield (
            name, *(enc(a) for a in args),
            encode(lambda fn=fn, a=args: fn(*a), drop_prefix=f"cmath.{name}()"),
        )


def rows_binascii():
    import binascii

    datas = [
        b"", b"a", b"ab", b"abc", b"abcd", b"hello world", bytes(range(45)),
        bytes(range(46)), b" spaces  ", b"tab\there", b"\x00\x01\xfe\xff",
        b"\r\n", b"\n\n", b"line one\r\nline two\r\n", b"\x7f\x80\x81\xff",
        b"a" * 74, b"b" * 75, b"c" * 100, b"x" * 227, b"y" * 228,
        b"trailing ", b" lead", b"=3D", b"=e", b"=xy", b"=\n", b"=\r\n",
        b"=A", b"=41=42", b"=\nrest", b"a=b", b"_under_", b".\n..",
        b"\r\n.", b" .\r\n", b"quoted = sign", b"=  ", b"=00=ff=0a",
    ]
    for name, args in [
        ("a2b_uu", (data,)) for data in [
            b"", b"#86)C", b"'A`P``\n", b"&,2<SU4;\n", b" ", b"`", b"a",
            b"#86)C`", b"#86)C \n", b"#86)Cx\n", b"#", b"#0V%T\n",
        ]
    ] + [
        ("b2a_uu", (data,)) for data in datas[:10]
    ] + [
        ("a2b_base64", (data,)) for data in [
            b"", b"YQ==", b"YWI=", b"YWJj", b"YWJjZA==", b"YQ", b"YQ=",
            b"Y", b"=", b"==", b"==YQ==", b"=YQ==", b"YQ===", b"YQ==x",
            b"YQ== \n", b"YWJ j", b"YWJj\nYQ==", b"aGVsbG8gd29ybGQ=",
            b"!@#$", b"YWJj*", b"  \t\n", b"Zm9vYmFy", b"Zm9v\nYmFy\n",
            b"PDw/Pz8+Pg==", b"/w==", b"+w==",
        ]
    ] + [
        ("b2a_base64", (data,)) for data in [
            b"", b"a", b"ab", b"abc", b"abcd", b"abcde",
            b"hello world", b"\x00\xff\x10", b"a" * 57,
        ]
    ] + [
        ("a2b_hex", (data,)) for data in [
            b"", b"00", b"0f", b"414243", b"6162632e", b"FF00", b"fF0E",
            b"0", b"0g", b"zz", b"0 0", b"0\n1",
        ]
    ] + [
        ("b2a_hex", (data,)) for data in datas[:12]
    ] + [
        ("hexlify", (datas[3],)),
    ] + [
        ("unhexlify", (data,)) for data in [
            b"", b"00", b"736f6d65206279746573", b"0f1e2d3c", b"0",
            b"0g", b"10 ff",
        ]
    ] + [
        ("crc_hqx", (data, crc)) for data in [b"", b"check", b"123456789",
                                              b"The quick brown fox"]
        for crc in [0, 1, 0xFFFF, 0x1234, 4294967295, 2**32 + 5, 2**100,
                    -1, True]
    ] + [
        ("crc32", (data,)) for data in [b"", b"123456789", b"The quick brown",
                                        bytes(range(64))]
    ] + [
        ("crc32", (data, crc)) for data in [b"123456789", b"IETF"]
        for crc in [0, 1, 32, 0xFFFFFFFF, 2**32 - 1, 2**64 + 7, 2**100,
                    -1, -2**32, True]
    ] + [
        ("a2b_qp", (data,)) for data in [
            b"", b"hello", b"=41", b"=4", b"=", b"=\n", b"=\r\n", b"=a",
            b"=41=42", b"a=\nb", b"a=\r\nb", b"a=\rb", b"==41", b"_a_",
            b"=XY", b"=z9", b"end=", b"=00", b"=ff", b"=0\n",
        ]
    ] + [
        ("a2b_qp", (data, header))
        for data in [b"_a_", b"a_b", b"=5F", b"_", b"=20_"]
        for header in [True, False]
    ] + [
        ("b2a_qp", (data,)) for data in [
            b"", b"hello world", b" ", b"  ", b"\t", b"hello ",
            b"hello\t", b"foo bar", b"foo=bar", b"foo_bar",
            b"\x00\x01\x80\xff", b"\xe9\xe8", b"a.\nb", b".\nb", b"..\nb",
            b"\r\n", b"\n", b"a\r\nb", b"foo\nbar\n", b"x" * 76,
            b"x" * 77, b"a" * 74 + b" \n", b"q" * 78,
        ]
    ] + [
        ("b2a_qp", (data, quotetabs, istext, header))
        for data in [
            b"hello world", b"foo\tbar", b"foo bar", b"foo bar ",
            b"a\tb\t", b"\xe9 foo", b"foo\nbar", b"foo\r\nbar",
            b"quoted = and .\n", b"x" * 76 + b" ", b"foo_bar baz",
        ]
        for quotetabs in [False, True]
        for istext in [True, False]
        for header in [False, True]
    ] + [
        # sep and bytes_per_sep are positional-or-keyword.
        ("b2a_hex", (b"0ffc0ffee", sep, per))
        for sep in [None, b":", b"-", b"_", b"", b"::", 5]
        for per in [1, 0, -1, 2, -2, 3, -3, 100, -100]
    ] + [
        ("b2a_hex", (b"deadbeef", None, 2**31)),
        ("b2a_hex", (b"deadbeef", None, -2**31)),
        ("b2a_hex", (b"deadbeef", None, 2**100)),
        ("b2a_hex", (b"deadbeef", b":", 1.5)),
        ("b2a_hex", (b"deadbeef", b":", True)),
        # A length-1 non-bytes sep hits "sep must be str or bytes." in
        # CPython; this runtime's only other length-bearing types come
        # later, so the branch keeps no oracle row.
        ("b2a_hex", (b"", b":", 2)),
        ("b2a_hex", (b"a", b":", 1)),
        ("hexlify", (b"c0ffee", b".", -1)),
        # Argument-count and type shapes for the calling conventions.
        ("a2b_uu", ()), ("a2b_uu", (b"#86)C", b"extra")),
        ("b2a_uu", ()), ("b2a_uu", (b"abc",)), ("b2a_uu", (b"abc", True)),
        ("b2a_uu", (b"abc", True, 1)),
        ("a2b_base64", ()), ("a2b_base64", (b"YQ==", b"YQ==")),
        ("b2a_base64", ()), ("b2a_base64", (b"abc",)),
        ("b2a_base64", (b"abc", False)),
        ("b2a_base64", (b"abc", False, 1)),
        ("a2b_hex", ()), ("a2b_hex", (b"41", b"41")),
        ("b2a_hex", ()), ("b2a_hex", (b"41",)), ("b2a_hex", (b"41", None)),
        ("b2a_hex", (b"41", None, 1, 2)),
        ("unhexlify", ()), ("unhexlify", (b"41", b"41")),
        ("crc_hqx", ()), ("crc_hqx", (b"check",)),
        ("crc_hqx", (b"check", 0, 0)), ("crc_hqx", (5, 0)),
        ("crc_hqx", (b"check", 1.5)), ("crc_hqx", (b"check", None)),
        ("crc32", ()), ("crc32", (b"", 0, 0)), ("crc32", (5,)),
        ("crc32", (b"", 1.5)), ("crc32", (b"", None)),
        ("a2b_qp", ()), ("a2b_qp", (b"=41", True, 1)),
        ("b2a_qp", ()), ("b2a_qp", (b"a",)),
        ("b2a_qp", (b"a", True)), ("b2a_qp", (b"a", True, False)),
        ("b2a_qp", (b"a", True, False, True, None)),
        # Non-bytes data for the Py_buffer converter family.
        ("a2b_uu", (5,)), ("a2b_base64", (None,)), ("a2b_hex", (1.5,)),
        ("b2a_qp", (b"", None)), ("crc32", (True,)),
    ]:
        fn = getattr(binascii, name)
        yield (
            name, *(enc(a) for a in args),
            encode(lambda fn=fn, a=args: fn(*a), drop_prefix=f"binascii.{name}()"),
        )


def gen_cmath(out):
    path = out / "cmath_module.tsv"
    with open(path, "w") as f:
        for row in rows_cmath():
            print("\t".join(row), file=f)
    print(f"wrote {path}")


def gen_math(out):
    path = out / "math_module.tsv"
    with open(path, "w") as f:
        for row in (
            *rows_unary(),
            *rows_binary(),
            *rows_integer(),
            *rows_sequences(),
        ):
            print("\t".join(row), file=f)
    print(f"wrote {path}")


def gen_binascii(out):
    path = out / "binascii_module.tsv"
    with open(path, "w") as f:
        for row in rows_binascii():
            print("\t".join(row), file=f)
    print(f"wrote {path}")


def main():
    import argparse
    from pathlib import Path

    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--out", type=Path, default=Path("native/crates/coil-rt/tests/data")
    )
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    gen_math(args.out)
    gen_cmath(args.out)
    gen_binascii(args.out)


if __name__ == "__main__":
    main()

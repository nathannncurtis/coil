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

Columns: function name, one column per argument, the result. Composite
arguments are tuples; the encoding is shared with the Rust reader:

  i:<decimal>          int
  b:0 | b:1            bool
  f:<16 hex digits>    float as its u64 bit pattern (f:nan for NaN)
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


def enc(value):
    if value is None:
        return "none"
    if isinstance(value, bool):
        return f"b:{int(value)}"
    if isinstance(value, int):
        return f"i:{value}"
    if isinstance(value, float):
        return "f:nan" if isnan(value) else f"f:{bits(value)}"
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
        ("isclose", (1.0,)), ("isclose", ()),
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


if __name__ == "__main__":
    main()

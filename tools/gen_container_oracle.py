"""CPython 3.12 container oracle for the coil-rt container tests (spec 8.2,
8.5).

Computes tuple hashes, dict and set iteration order for scripted
insertion/deletion sequences, and frozenset hashes with the pinned
interpreter, and writes them as checked-in test data (tab-separated, one
case per line) for native/crates/coil-rt/tests/. This module refuses to
run on anything but CPython 3.12, the pinned target whose table mechanics
the runtime ports. Int keys keep the oracle deterministic: hash(int) is
the value itself, so PYTHONHASHSEED cannot change any row.

Output files (under --out, default native/crates/coil-rt/tests/data):

  tuple_hash.tsv       tuple element encoding (below), then hash(t)
  dict_order.tsv       ops (below), then the keys in iteration order
  set_order.tsv        ops (below), then the keys in iteration order
  frozenset_hash.tsv   space-separated ints, then hash(frozenset(...))

Element encoding (tuple_hash.tsv): space-separated tokens, a token being

  <decimal int> | f:<16 hex digits> float | ( and ) nesting a sub-tuple

NaN is excluded: CPython 3.12 hashes it from the object's address, so no
checked-in row could be stable.

Op encoding (dict_order.tsv, set_order.tsv): ';'-separated ops,

  i:<key>:<value>  dict insert/overwrite (dict only)
  d:<key>          dict delete (dict only)
  a:<key>          set add (set only)
  x:<key>          set discard (set only)
  p                set pop (set only)
  c                clear

and the result column: comma-separated keys in iteration order.
"""

import struct
import sys

if sys.version_info[:2] != (3, 12):
    sys.exit("gen_container_oracle.py must run on CPython 3.12 (the pinned target)")


def bits(x):
    return f"{struct.unpack('<Q', struct.pack('<d', x))[0]:016X}"


def f64(h):
    return struct.unpack("<d", struct.pack("<Q", int(h, 16)))[0]


# Ints whose small-table slots collide (all 1 mod 8), the two ints sharing
# hash -2, ints whose hash wraps to 0, plus digit-boundary values.
COLLIDE = [1, 9, 17, 25, 33, 41, 49]
NEG = [-1, -2]
WRAP = [2305843009213693951, 2305843009213693950]
EDGE = [0, 5, 256, 1073741823, 1073741824, -1073741824]


def enc_int(v):
    return str(v)


def tuple_cases():
    cases = [
        [()],
        [(0,)],
        [(2,)],
        [(-1,)],
        [(0, 0)],
        [(1, 2, 3)],
        [(0, 1, 2, 3, 4, 5, 6, 7, 8, 9)],
        [(-1, -2, 2305843009213693951)],
        [(1073741823, 1073741824, -1073741824)],
        [(0.5,)],
        [(-0.5, 1.5, 2.0)],
        [(0.0, -0.0, -1.0)],
        [(float("inf"), -float("inf"))],
        [(1, 0.5, -2, 1073741824)],
        [((),)],
        [((1, 2), (3,), ((4, 5.5),))],
        [((0.5,), 1, ((2,),))],
        [(1, 2), (2, 1), (1, 2, 0), (0, 1, 2)],
        [(i,) for i in COLLIDE],
        [tuple(COLLIDE)],
        [tuple(range(20))],
    ]
    rows = []
    for group in cases:
        for tup in group:
            tokens = []

            def emit(t):
                for item in t:
                    if isinstance(item, tuple):
                        tokens.append("(")
                        emit(item)
                        tokens.append(")")
                    elif isinstance(item, float):
                        tokens.append(f"f:{bits(item)}")
                    else:
                        tokens.append(enc_int(item))

            emit(tup)
            rows.append((" ".join(tokens), str(hash(tup))))
    return rows


def dict_cases():
    scripts = [
        # Plain growth through both resizes.
        [("i", k, k) for k in range(10)],
        # Overwrites keep position.
        [("i", k, k) for k in range(6)] + [("i", 2, 20), ("i", 0, 21)],
        # Colliding keys probe the full sequence.
        [("i", k, k) for k in COLLIDE],
        # Deletions leave dummies; refill reuses and then resizes.
        [("i", k, k) for k in range(8)]
        + [("d", k, None) for k in range(0, 8, 2)]
        + [("i", k, k) for k in (16, 17, 18, 19)],
        # hash(-1) == hash(-2) == -2: equal hash, different keys.
        [("i", k, k) for k in NEG + [2]],
        # hash wraps to 0 and 1.
        [("i", k, k) for k in WRAP + [0, 1]],
        # Delete everything, then reinsert: order by insertion, dummies
        # skipped until the resize compacts.
        [("i", k, k) for k in range(6)]
        + [("d", k, None) for k in range(6)]
        + [("i", k, k) for k in (3, 1, 5)],
        # Heavy deletion then growth: the resize compacts the table.
        [("i", k, k) for k in range(20)]
        + [("d", k, None) for k in range(0, 20, 2)]
        + [("i", k, k) for k in (30, 31, 32, 33, 34)],
        # Clear and refill.
        [("i", k, k) for k in range(7)] + [("c",)] + [("i", k, k) for k in (4, 2, 9)],
        # Misses do not disturb order.
        [("i", k, k) for k in range(5)] + [("d", 99, None), ("i", 5, 5)],
    ]
    rows = []
    for script in scripts:
        ops = []
        d = {}
        for op in script:
            if op[0] == "i":
                d[op[1]] = op[2]
                ops.append(f"i:{op[1]}:{op[2]}")
            elif op[0] == "d":
                d.pop(op[1], None)
                ops.append(f"d:{op[1]}")
            else:
                d.clear()
                ops.append("c")
        rows.append((";".join(ops), ",".join(str(k) for k in d)))
    return rows


def set_cases():
    scripts = [
        # Small-table fills (no resize yet).
        [("a", k) for k in range(5)],
        [("a", k) for k in range(8)],
        # The resize threshold (fill*5 >= mask*3) hits at the fifth add.
        [("a", k) for k in range(13)],
        # Colliding keys exercise the linear probes.
        [("a", k) for k in COLLIDE],
        # Discards leave dummies; the resize re-lays the table in slot
        # order, so the iteration order changes here.
        [("a", k) for k in range(8)]
        + [("x", k) for k in range(0, 8, 2)]
        + [("a", k) for k in (9, 17, 25, 33)],
        # Pops walk the search finger.
        [("a", k) for k in range(10)] + [("p",)] * 4,
        # Pop interleaved with add.
        [("a", k) for k in range(6)] + [("p",), ("a", 13), ("p",), ("a", 14)],
        # Discard and re-add: dummy reuse.
        [("a", k) for k in range(9)] + [("x", 4), ("a", 4), ("x", 4)],
        # Clear and refill.
        [("a", k) for k in range(11)] + [("c",)] + [("a", k) for k in (3, 1, 4, 1, 5)],
        # Missing discards change nothing.
        [("a", k) for k in range(4)] + [("x", 99)],
        # Empty everything by popping.
        [("a", k) for k in range(4)] + [("p",)] * 4,
    ]
    rows = []
    for script in scripts:
        ops = []
        s = set()
        for op in script:
            if op[0] == "a":
                s.add(op[1])
                ops.append(f"a:{op[1]}")
            elif op[0] == "x":
                s.discard(op[1])
                ops.append(f"x:{op[1]}")
            elif op[0] == "p":
                s.pop()
                ops.append("p")
            else:
                s.clear()
                ops.append("c")
        rows.append((";".join(ops), ",".join(str(k) for k in s)))
    return rows


def frozenset_cases():
    groups = [
        [[]],
        [[0]],
        [[1]],
        [[0, 1]],
        [[1, 0]],
        [COLLIDE[:k] for k in range(2, 8)],
        [[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]],
        [[12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]],
        [NEG + [2]],
        [WRAP],
        [EDGE],
        [[7, 14, 21, 28, 35, 42, 49, 56, 63]],
        [[63, 56, 49, 42, 35, 28, 21, 14, 7]],
        [list(range(30))],
        [[k * 3 + 1 for k in range(30)]],
    ]
    rows = []
    for group in groups:
        for items in group:
            fs = frozenset(items)
            rows.append((" ".join(str(k) for k in items), str(hash(fs))))
    return rows


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else None
    if out_dir is None:
        import pathlib
        out_dir = pathlib.Path(__file__).resolve().parents[1] / \
            "native/crates/coil-rt/tests/data"
    tables = [
        ("tuple_hash.tsv", "elements result", tuple_cases),
        ("dict_order.tsv", "ops result", dict_cases),
        ("set_order.tsv", "ops result", set_cases),
        ("frozenset_hash.tsv", "elements result", frozenset_cases),
    ]
    for name, header, gen in tables:
        rows = gen()
        path = f"{out_dir}/{name}"
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            f.write(f"# {header}\n")
            seen = set()
            for row in rows:
                if row in seen:
                    sys.exit(f"duplicate row in {name}: {row}")
                seen.add(row)
                f.write("\t".join(row) + "\n")
        print(f"{name}: {len(rows)} rows")


if __name__ == "__main__":
    main()

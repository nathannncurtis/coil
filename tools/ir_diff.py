"""Differential test driver for the IR corpus (task t13).

Runs every program under ``tests/native/ir_programs`` twice: once with
CPython 3.12, once with ``coil-front-dump --run`` (the Coil IR
interpreter over the test object model). A program matches when stdout
is identical and, if the program ends with an uncaught exception, the
exception type is the same.

Usage, from ``native/`` after ``cargo build --release -p coil-front``:

    python3.12 ../tools/ir_diff.py [name ...]

With no arguments every ``*.py`` in the corpus is checked. Names may be
bare (``010_int_arith``) or full paths. Exits nonzero on any mismatch.
"""

import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
CORPUS = REPO / "tests" / "native" / "ir_programs"
DUMPER = REPO / "native" / "target" / "release" / "coil-front-dump"
PYTHON = "python3.12"


def run(cmd, timeout=30):
    try:
        c = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return c.returncode, c.stdout, c.stderr
    except subprocess.TimeoutExpired:
        return None, "", "TIMEOUT"


def exc_type(stderr):
    """The type name of an uncaught exception, or None."""
    for line in reversed(stderr.splitlines()):
        if not line or line.startswith(" "):
            continue
        if line.startswith("Traceback"):
            return None
        return line.split(":", 1)[0].strip()
    return None


def check(path):
    rc, out, err = run([str(DUMPER), "--run", str(path)])
    prc, pout, perr = run([PYTHON, str(path)])
    if rc is not None and out == pout and exc_type(err) == exc_type(perr):
        return True, ""
    why = [f"coil exit={rc}, python exit={prc}"]
    if err.strip() and "Traceback" in err:
        why.append("coil err tail: " + " | ".join(err.strip().splitlines()[-2:]))
    po, co = pout.splitlines(), out.splitlines()
    for i in range(max(len(po), len(co))):
        a = po[i] if i < len(po) else "<missing>"
        b = co[i] if i < len(co) else "<missing>"
        if a != b:
            why.append(f"stdout line {i}: python={a!r} coil={b!r}")
            break
    return False, "; ".join(why)


def main():
    if not DUMPER.exists():
        print(f"error: {DUMPER} not built; run cargo build --release -p coil-front", file=sys.stderr)
        return 2
    names = sys.argv[1:]
    if names:
        paths = [
            pathlib.Path(n if n.endswith(".py") else f"{n}.py")
            if n.endswith(".py") or n.startswith("tests/") or "/" in n
            else CORPUS / f"{n}.py"
            for n in names
        ]
    else:
        paths = sorted(CORPUS.glob("*.py"))
    bad = 0
    for path in paths:
        ok, why = check(path)
        if ok:
            print(f"ok   {path.stem}")
        else:
            bad += 1
            print(f"BAD  {path.stem}: {why}")
    print(f"{len(paths) - bad}/{len(paths)} match")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

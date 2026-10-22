"""Tests for the CPython front-end oracle and its differential harness."""

from __future__ import annotations

import importlib.util
import shlex
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.version_info[:2] != (3, 12),
    reason="the front oracle is pinned to CPython 3.12 and refuses other interpreters",
)

TOOLS = Path(__file__).resolve().parents[2] / "tools"


def _load_tool(name: str):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


front_oracle = _load_tool("front_oracle")


def test_constant_encodings() -> None:
    cases = [
        ("hi", {"const": "str", "v": [104, 105]}),
        ("", {"const": "str", "v": []}),
        ("héllo", {"const": "str", "v": [104, 233, 108, 108, 111]}),
        (b"\x00\xff", {"const": "bytes", "v": "00ff"}),
        (b"", {"const": "bytes", "v": ""}),
        (0, {"const": "int", "v": "0"}),
        (-12, {"const": "int", "v": "-12"}),
        (10**30, {"const": "int", "v": "1000000000000000000000000000000"}),
        (1.5, {"const": "float", "v": "1.5"}),
        (float("inf"), {"const": "float", "v": "inf"}),
        (complex(1, 2), {"const": "complex", "real": "1.0", "imag": "2.0"}),
        (True, {"const": "bool", "v": True}),
        (False, {"const": "bool", "v": False}),
        (None, {"const": "None"}),
        (Ellipsis, {"const": "Ellipsis"}),
    ]
    for value, expected in cases:
        assert front_oracle.encode_constant(value) == expected


def test_small_node_encoding() -> None:
    import ast

    encoded = front_oracle.encode_node(ast.parse("x = 1\n", "t.py"))
    assert encoded == {
        "_type": "Module",
        "body": [
            {
                "_type": "Assign",
                "targets": [
                    {
                        "_type": "Name",
                        "id": "x",
                        "ctx": {"_type": "Store"},
                        "lineno": 1,
                        "col_offset": 0,
                        "end_lineno": 1,
                        "end_col_offset": 1,
                    }
                ],
                "value": {
                    "_type": "Constant",
                    "value": {"const": "int", "v": "1"},
                    "kind": None,
                    "lineno": 1,
                    "col_offset": 4,
                    "end_lineno": 1,
                    "end_col_offset": 5,
                },
                "type_comment": None,
                "lineno": 1,
                "col_offset": 0,
                "end_lineno": 1,
                "end_col_offset": 5,
            }
        ],
        "type_ignores": [],
    }
    # The canonical encoding fixes field order: _type, _fields, _attributes.
    assert list(encoded) == ["_type", "body", "type_ignores"]
    assert list(encoded["body"][0]["targets"][0]) == [
        "_type", "id", "ctx", "lineno", "col_offset", "end_lineno", "end_col_offset",
    ]


def test_symtable_encoding() -> None:
    import symtable

    table = symtable.symtable("def f(a):\n    return a\n", "t.py", "exec")
    assert front_oracle.encode_table(table) == {
        "name": "top",
        "type": "module",
        "lineno": 0,
        "is_optimized": False,
        "is_nested": False,
        "symbols": {"f": ["is_assigned", "is_global", "is_local", "is_namespace"]},
        "children": [
            {
                "name": "f",
                "type": "function",
                "lineno": 1,
                "is_optimized": True,
                "is_nested": False,
                "symbols": {"a": ["is_local", "is_parameter", "is_referenced"]},
                "children": [],
            }
        ],
    }


def _write_dumper(tmp_path: Path, name: str, body: str) -> str:
    script = tmp_path / name
    script.write_text(body, encoding="utf-8")
    return shlex.join([sys.executable, str(script)])


def _run_front_diff(mode: str, inputs: list[str], dumper: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(TOOLS / "front_diff.py"), f"--{mode}", *inputs, "--dumper", dumper],
        capture_output=True,
        text=True,
    )


ECHO_DUMPER = """\
import sys
sys.path.insert(0, {tools!r})
import front_oracle
raise SystemExit(front_oracle.main(sys.argv[1:]))
"""

ALTER_DUMPER = """\
import sys
sys.path.insert(0, {tools!r})
import front_oracle
mode = sys.argv[1].lstrip("-")
paths = [sys.argv[index] for index in range(2, len(sys.argv))]
for record in front_oracle.records(mode, paths):
    record["tokens"][0][1] += "!"
    print(front_oracle.dumps(record))
"""


def test_front_diff_match_with_echoing_dumper(tmp_path: Path) -> None:
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "a.py").write_text("x = 1\n", encoding="utf-8")
    (package / "b.py").write_text("def f(a):\n    return a\n", encoding="utf-8")
    (package / "notes.txt").write_text("not python\n", encoding="utf-8")
    dumper = _write_dumper(tmp_path, "echo_dumper.py", ECHO_DUMPER.format(tools=str(TOOLS)))
    result = _run_front_diff("tokens", [str(package)], dumper)
    assert result.returncode == 0, result.stderr
    assert "files 2" in result.stdout
    assert "match 2" in result.stdout
    assert "mismatch 0" in result.stdout
    assert "both_error 0" in result.stdout


def test_front_diff_reports_mismatch_with_pointer(tmp_path: Path) -> None:
    source = tmp_path / "a.py"
    source.write_text("x = 1\n", encoding="utf-8")
    dumper = _write_dumper(tmp_path, "alter_dumper.py", ALTER_DUMPER.format(tools=str(TOOLS)))
    result = _run_front_diff("tokens", [str(source)], dumper)
    assert result.returncode == 1, result.stderr
    assert f"mismatch {source} /tokens/0/1" in result.stdout
    assert "files 1" in result.stdout
    assert "match 0" in result.stdout
    assert "mismatch 1" in result.stdout


def test_front_diff_batches_bound_the_command_line() -> None:
    front_diff = _load_tool("front_diff")
    paths = [Path(f"/src/{'x' * 40}{index}.py") for index in range(10)]
    batches = front_diff.batches(paths, max_chars=150, max_files=4)
    assert [path for batch in batches for path in batch] == paths
    assert all(len(batch) <= 4 for batch in batches)
    assert all(sum(len(str(path)) + 3 for path in batch) <= 150 for batch in batches)
    assert front_diff.batches([Path("/" + "y" * 500)], max_chars=150) == [[Path("/" + "y" * 500)]]


def test_front_diff_compares_across_batches(tmp_path: Path) -> None:
    front_diff = _load_tool("front_diff")
    package = tmp_path / "pkg"
    package.mkdir()
    for index in range(5):
        (package / f"m{index}.py").write_text(f"x{index} = {index}\n", encoding="utf-8")
    dumper = _write_dumper(tmp_path, "echo_dumper.py", ECHO_DUMPER.format(tools=str(TOOLS)))
    paths = front_oracle.collect_paths([str(package)])
    result = front_diff.diff_batched("ast", dumper, paths, max_files=2)
    assert result["counts"] == {"files": 5, "match": 5, "mismatch": 0, "both_error": 0}
    altered = _write_dumper(tmp_path, "alter_dumper.py", ALTER_DUMPER.format(tools=str(TOOLS)))
    result = front_diff.diff_batched("tokens", altered, paths, max_files=2)
    assert result["counts"]["mismatch"] == 5
    assert [detail for _, detail in result["mismatches"]] == ["/tokens/0/1"] * 5

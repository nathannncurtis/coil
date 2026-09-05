"""Build and execute actual Windows bundles for dependency regressions."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import zipfile

import pytest

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows executable integration")


def _wheel(directory: Path, version: str):
    path = directory / f"coilauditdep-{version}-py3-none-any.whl"
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("coilauditdep/__init__.py", "from .docs import VERSION\n")
        archive.writestr("coilauditdep/docs/__init__.py", f"VERSION = {version!r}\n")
        info = f"coilauditdep-{version}.dist-info"
        archive.writestr(info + "/METADATA", f"Metadata-Version: 2.1\nName: coilauditdep\nVersion: {version}\n")
        archive.writestr(info + "/WHEEL", "Wheel-Version: 1.0\nGenerator: regression\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
        archive.writestr(info + "/RECORD", "")


def test_pinned_dependency_namespace_and_excluded_imports_execute(tmp_path, real_runtime, monkeypatch):
    from coil.builder import build

    wheels = tmp_path / "wheels"
    wheels.mkdir()
    _wheel(wheels, "1.0")
    _wheel(wheels, "2.0")
    monkeypatch.setenv("PIP_NO_INDEX", "1")
    monkeypatch.setenv("PIP_FIND_LINKS", str(wheels))
    monkeypatch.setenv("PIP_CACHE_DIR", str(tmp_path / "pip-cache"))
    monkeypatch.setattr("coil.builder.prepare_runtime", lambda **kwargs: real_runtime)
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text(
        "import json\nfrom localns import helper\nimport coilauditdep\n"
        "print(json.dumps([coilauditdep.VERSION, helper.VALUE]))\n"
    )
    (project / "localns").mkdir()
    (project / "localns/helper.py").write_text("VALUE = 'namespace works'\n")
    (project / "tests").mkdir()
    (project / "tests/test_unused.py").write_text("import nonexistent_test_only_package\n")
    (project / "ignored").mkdir()
    (project / "ignored/unused.py").write_text("import nonexistent_ignored_package\n")
    (project / ".coilignore").write_text("ignored/\n")
    (project / "pyproject.toml").write_text('[project]\ndependencies = ["coilauditdep==1.0"]\n')
    bundle = build(project, ["main.py"], mode="bundled", python_version="3.12.10",
                   output_dir=str(tmp_path / "dist"), name="dependencies")[0]
    result = subprocess.run([str(bundle / "dependencies.exe")], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == ["1.0", "namespace works"]


def test_cross_version_dependency_compilation_executes(tmp_path, monkeypatch):
    from coil.packager import package_bundled

    archive_path = os.environ.get("COIL_TEST_CROSS_RUNTIME")
    if not archive_path:
        pytest.skip("Set COIL_TEST_CROSS_RUNTIME to another Python minor's embed ZIP")
    runtime = tmp_path / "runtime"
    with zipfile.ZipFile(archive_path) as archive:
        archive.extractall(runtime)
    version = subprocess.check_output([str(runtime / "python.exe"), "-c", "import sys; print(sys.version_info[:2])"], text=True)
    if version.strip() == str(sys.version_info[:2]):
        pytest.skip("Cross runtime must differ from host Python")
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text("import customdep\nprint(customdep.VALUE)\n")
    dependencies = tmp_path / "deps"
    dependencies.mkdir()
    (dependencies / "customdep.py").write_text("VALUE = 'target bytecode works'\n")
    bundle = package_bundled(project, tmp_path / "dist", runtime, ["main.py"],
                             "cross", "windows", deps_dir=dependencies)
    result = subprocess.run([str(bundle / "cross.exe")], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "target bytecode works"

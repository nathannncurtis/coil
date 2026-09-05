"""Tests for native launchers, including real Windows interpreter embedding.

COIL_RUN_NATIVE_INTEGRATION=1 enables the runtime matrix. Official embeddable
runtimes are downloaded to COIL_NATIVE_RUNTIME_CACHE (or the test temp folder).
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request
import zipfile

import pytest

from coil._launcher_stubs import MANIFEST_SHA256, SOURCE_SHA256
from coil.launcher import create_launcher, get_launcher


VERSIONS = ("3.9.13", "3.10.11", "3.11.9", "3.12.10", "3.13.7")


def test_embedded_launchers_match_source():
    source = Path(__file__).resolve().parents[1] / "src/coil/templates/launcher.c"
    assert hashlib.sha256(source.read_bytes().replace(b"\r\n", b"\n")).hexdigest() == SOURCE_SHA256
    manifest = source.with_suffix(".manifest")
    assert hashlib.sha256(manifest.read_bytes().replace(b"\r\n", b"\n")).hexdigest() == MANIFEST_SHA256


@pytest.mark.parametrize("version", VERSIONS)
def test_launcher_is_x64_pe_with_bound_entry(version):
    import pefile
    launcher = get_launcher(version, "_boot_named_entry.py", 2)
    pe = pefile.PE(data=launcher)
    assert pe.FILE_HEADER.Machine == 0x8664
    assert pe.OPTIONAL_HEADER.Subsystem == 3
    assert "_boot_named_entry.py".encode("utf-16-le") in launcher
    imports = {entry.dll.lower() for entry in pe.DIRECTORY_ENTRY_IMPORT}
    assert all(not name.startswith(b"python") for name in imports)


@pytest.mark.parametrize("name", ("", "../escape.py", "a/b.py", "a\\b.py", "a:stream", "a\0b", "x" * 256, ".", ".."))
def test_invalid_boot_names_are_rejected(name):
    with pytest.raises(ValueError):
        get_launcher("3.12", name)


native_integration = pytest.mark.skipif(
    sys.platform != "win32" or os.environ.get("COIL_RUN_NATIVE_INTEGRATION") != "1",
    reason="Set COIL_RUN_NATIVE_INTEGRATION=1 on Windows to run real runtime matrix",
)


@pytest.fixture(params=VERSIONS)
def native_bundle(request, tmp_path):
    version = request.param
    cache = Path(os.environ.get("COIL_NATIVE_RUNTIME_CACHE", str(tmp_path / "downloads")))
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"python-{version}-embed-amd64.zip"
    if not archive.exists():
        urllib.request.urlretrieve(
            f"https://www.python.org/ftp/python/{version}/{archive.name}", archive)
    root = tmp_path / "bundle with spaces λ"
    internal = root / "_internal"
    internal.mkdir(parents=True)
    with zipfile.ZipFile(archive) as runtime:
        runtime.extractall(internal)
    for dll in internal.glob("*.dll"):
        if dll.name.startswith(("python", "vcruntime")):
            shutil.move(str(dll), root / dll.name)
    (internal / "app").mkdir()
    (internal / "lib").mkdir()
    exe = root / "Application λ.exe"
    create_launcher(exe, version, "_boot_fixed.py")
    boot = internal / "_boot_fixed.py"
    boot.write_text(
        "import sys, json, atexit\n"
        "print(json.dumps({'argv':sys.argv, 'orig_argv':sys.orig_argv, "
        "'executable':sys.executable, 'frozen':sys.frozen, 'path':sys.path, "
        "'isolated':sys.flags.isolated, 'no_site':sys.flags.no_site, "
        "'ignore_environment':sys.flags.ignore_environment}))\n"
        "atexit.register(lambda: print('ATEXIT'))\n",
        encoding="utf-8",
    )
    return exe, boot


@native_integration
def test_real_native_argv_shutdown_and_isolation(native_bundle, tmp_path):
    exe, _ = native_bundle
    for args in (["--flag"], ["bare"], ["path with spaces\\and λ"], [],
                 ["-X", "dev", "-c", "raise RuntimeError('not application code')"],
                 ["", 'a"b', "trailing\\", "one\\\"two"]):
        environment = dict(os.environ, PYTHONPATH=str(tmp_path / "untrusted"),
                           PYTHONHOME=str(tmp_path / "not-python"), PYTHONSTARTUP="missing.py",
                           PYTHONOPTIMIZE="2", PYTHONINSPECT="1")
        completed = subprocess.run([str(exe), *args], cwd=tmp_path, env=environment,
                                   capture_output=True, text=True, encoding="utf-8", timeout=20)
        assert completed.returncode == 0, completed.stderr
        lines = completed.stdout.splitlines()
        observed = json.loads(lines[0])
        assert observed["argv"] == [str(exe), *args]
        assert observed["orig_argv"] == [str(exe), *args]
        assert observed["executable"] == str(exe)
        assert observed["frozen"] is True
        assert observed["isolated"] == observed["no_site"] == observed["ignore_environment"] == 1
        assert str(tmp_path / "untrusted") not in observed["path"]
        assert lines[1:] == ["ATEXIT"]


@native_integration
def test_renamed_native_entry_and_python_exit_codes(native_bundle):
    exe, boot = native_bundle
    renamed = exe.with_name("Renamed.exe")
    exe.rename(renamed)
    for statement, code, stderr in (("raise SystemExit(7)", 7, ""),
                                   ("raise SystemExit('message')", 1, "message"),
                                   ("raise RuntimeError('failure')", 1, "RuntimeError: failure")):
        boot.write_text("print('flushed output')\n" + statement + "\n", encoding="utf-8")
        completed = subprocess.run([str(renamed)], capture_output=True, text=True, timeout=20)
        assert completed.returncode == code
        assert completed.stdout == "flushed output\n"
        assert stderr in completed.stderr


@native_integration
def test_gui_traceback_is_logged_only_when_stderr_is_absent(native_bundle, tmp_path):
    import pefile
    exe, boot = native_bundle
    version = next(version for version in VERSIONS
                   if (exe.parent / f"python{version.split('.')[0]}{version.split('.')[1]}.dll").exists())
    create_launcher(exe, version, boot.name, error_logging=True)
    pe = pefile.PE(str(exe))
    pe.OPTIONAL_HEADER.Subsystem = 2
    updated = pe.write()
    pe.close()
    exe.write_bytes(updated)
    environment = dict(os.environ, LOCALAPPDATA=str(tmp_path / "local"))
    (tmp_path / "local").mkdir()
    boot.write_text("pass\n", encoding="utf-8")
    successful = subprocess.run([str(exe)], env=environment, close_fds=True,
                                creationflags=subprocess.DETACHED_PROCESS, timeout=20)
    assert successful.returncode == 0
    assert not list((tmp_path / "local").rglob("*.log"))
    boot.write_text("raise RuntimeError('gui-visible-error')\n", encoding="utf-8")
    failed = subprocess.run([str(exe)], env=environment, close_fds=True,
                           creationflags=subprocess.DETACHED_PROCESS, timeout=20)
    assert failed.returncode == 1
    logs = list((tmp_path / "local").rglob("*.log"))
    assert len(logs) == 1
    assert "RuntimeError: gui-visible-error" in logs[0].read_text(encoding="utf-8")
    captured = subprocess.run([str(exe)], env=environment, capture_output=True,
                              text=True, timeout=20)
    assert captured.returncode == 1
    assert "RuntimeError: gui-visible-error" in captured.stderr
    assert list((tmp_path / "local").rglob("*.log")) == logs


@native_integration
def test_only_portable_launchers_accept_verified_parent_identity(native_bundle):
    import ctypes
    exe, boot = native_bundle
    version = next(version for version in VERSIONS
                   if (exe.parent / f"python{version.split('.')[0]}{version.split('.')[1]}.dll").exists())
    process_image = ctypes.create_unicode_buffer(32768)
    assert ctypes.windll.kernel32.GetModuleFileNameW(None, process_image, 32768)
    environment = dict(os.environ, _COIL_PORTABLE_EXE=process_image.value,
                       _COIL_PORTABLE_PARENT_PID=str(os.getpid()))
    for portable in (False, True):
        create_launcher(exe, version, boot.name, portable=portable)
        completed = subprocess.run([str(exe), "--flag"], env=environment,
                                   capture_output=True, text=True, timeout=20)
        assert completed.returncode == 0, completed.stderr
        observed = json.loads(completed.stdout.splitlines()[0])
        expected = process_image.value if portable else str(exe)
        assert observed["argv"] == observed["orig_argv"] == [expected, "--flag"]
        assert observed["executable"] == expected
    environment["_COIL_PORTABLE_PARENT_PID"] = "1"
    completed = subprocess.run([str(exe)], env=environment, capture_output=True, text=True, timeout=20)
    assert json.loads(completed.stdout.splitlines()[0])["executable"] == str(exe)


@native_integration
def test_launcher_supports_extended_unicode_paths(native_bundle, tmp_path):
    exe, _ = native_bundle
    destination = tmp_path / ("long folder λ " * 7).rstrip() / ("another folder " * 7).rstrip() / "bundle"
    shutil.copytree(exe.parent, destination)
    long_exe = "\\\\?\\" + str(destination / exe.name)
    assert len(long_exe) > 260
    completed = subprocess.run([long_exe, "--flag", "long path argument"], executable=long_exe,
                               capture_output=True, text=True, encoding="utf-8", timeout=20)
    assert completed.returncode == 0, completed.stderr
    observed = json.loads(completed.stdout.splitlines()[0])
    assert observed["argv"] == [long_exe, "--flag", "long path argument"]
    assert observed["executable"] == long_exe


@native_integration
def test_packaged_multiprocessing_for_each_runtime(native_bundle, tmp_path):
    from coil.packager import package_bundled
    exe, _ = native_bundle
    version = next(version for version in VERSIONS
                   if (exe.parent / f"python{version.split('.')[0]}{version.split('.')[1]}.dll").exists())
    cache = Path(os.environ.get("COIL_NATIVE_RUNTIME_CACHE", str(tmp_path / "downloads")))
    runtime = tmp_path / "packaging-runtime"
    with zipfile.ZipFile(cache / f"python-{version}-embed-amd64.zip") as archive:
        archive.extractall(runtime)
    project = tmp_path / "mp-project"
    project.mkdir()
    (project / "main.py").write_text(
        "import multiprocessing as mp\n"
        "def square(n): return n*n\n"
        "if __name__ == '__main__':\n"
        "    with mp.Pool(2) as pool: print(pool.map(square, [2, 3, 4]))\n")
    bundle = package_bundled(project, tmp_path / "dist", runtime, ["main.py"], "MP", "windows")
    result = subprocess.run([str(bundle / "MP.exe")], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "[4, 9, 16]"

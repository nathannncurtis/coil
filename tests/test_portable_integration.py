"""Execute real portable exes, including extraction and process forwarding."""
from __future__ import annotations

import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time
import zipfile
import zlib

import pytest

from coil.decompiler import decompile
from coil.packager import package_portable

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows portable executables")

SOURCE = '''import atexit, json, os, sys, time
from pathlib import Path
cache = Path(__file__).resolve().parents[2]
asset = cache / 'assets' / 'café-猫.txt'
result = dict(argv=sys.argv, executable=sys.executable,
              orig_argv=getattr(sys, 'orig_argv', sys.argv),
              frozen=getattr(sys, 'frozen', False), cache=str(cache),
              asset=asset.read_text(encoding='utf-8'),
              files=[p.name for p in asset.parent.iterdir()])
if os.environ.get('COIL_PORTABLE_TEST_STDIN'):
    result['stdin'] = sys.stdin.read()
Path(os.environ['COIL_PORTABLE_TEST_REPORT']).write_text(json.dumps(result), encoding='utf-8')
atexit.register(lambda: print('ATEXIT'))
print('BUFFERED_STDOUT', end='')
print('BUFFERED_STDERR', end='', file=sys.stderr)
if os.environ.get('COIL_PORTABLE_TEST_DELAY'):
    time.sleep(float(os.environ['COIL_PORTABLE_TEST_DELAY']))
if os.environ.get('COIL_PORTABLE_TEST_GATE'):
    while not Path(os.environ['COIL_PORTABLE_TEST_GATE']).exists():
        time.sleep(0.05)
if os.environ.get('COIL_PORTABLE_TEST_EXIT'):
    sys.exit(int(os.environ['COIL_PORTABLE_TEST_EXIT']))
'''


@pytest.fixture(scope="module")
def portable_builds(tmp_path_factory, real_runtime):
    root = tmp_path_factory.mktemp("portable-builds")
    project = root / "project"
    project.mkdir()
    (project / "main.py").write_text(SOURCE, encoding="utf-8")
    (project / "assets").mkdir()
    (project / "assets/café-猫.txt").write_text("UNICODE_ASSET", encoding="utf-8")
    normal = package_portable(project, root / "dist", real_runtime, ["main.py"],
                              "Portable Probe", "windows")[0]
    secure = package_portable(project, root / "secure", real_runtime, ["main.py"],
                              "Secure Probe", "windows", secure=True)[0]
    return {"root": root, "normal": normal, "secure": secure}


def _environment(tmp_path, label="report", overrides=None):
    local = tmp_path / "local"
    temp = tmp_path / "temp"
    local.mkdir(parents=True, exist_ok=True)
    temp.mkdir(parents=True, exist_ok=True)
    report = tmp_path / f"{label}.json"
    report.unlink(missing_ok=True)
    environment = dict(os.environ, LOCALAPPDATA=str(local), TEMP=str(temp), TMP=str(temp),
                       COIL_PORTABLE_TEST_REPORT=str(report))
    environment.update(overrides or {})
    return environment, report


def _communicate(process, timeout=40, input=None):
    try:
        return process.communicate(input=input, timeout=timeout)
    except subprocess.TimeoutExpired:
        # Only terminate the child tree we started. Every executable and its
        # cache is under the test's own output directory.
        subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                       capture_output=True, timeout=10, creationflags=subprocess.CREATE_NO_WINDOW)
        process.kill()
        process.communicate(timeout=10)
        raise


def _run(exe, tmp_path, args=(), overrides=None, input=None, label="report"):
    environment, report = _environment(tmp_path, label, overrides)
    process = subprocess.Popen([str(exe), *args], cwd=tmp_path, env=environment,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True)
    stdout, stderr = _communicate(process, input=input)
    observed = json.loads(report.read_text(encoding="utf-8")) if report.exists() else None
    return process.returncode, stdout, stderr, observed


@pytest.mark.parametrize("arguments", [[], ["--flag", "file"], ["import", "D:\\disc"],
    ["D:\\path with spaces\\café 猫"], ["", 'quoted"word', "trailing\\"],
    ["-X", "dev", "-c", "print('must not run')"]])
def test_portable_arguments_streams_and_unicode_assets(portable_builds, tmp_path, arguments):
    exe = portable_builds["normal"]
    code, stdout, stderr, observed = _run(exe, tmp_path, arguments)
    assert code == 0, stderr
    assert observed["argv"] == [str(exe), *arguments]
    assert observed["orig_argv"] == [str(exe), *arguments]
    assert observed["executable"] == str(exe)
    assert observed["frozen"] is True
    assert observed["asset"] == "UNICODE_ASSET"
    assert observed["files"] == ["café-猫.txt"]
    assert "BUFFERED_STDOUT" in stdout and "ATEXIT" in stdout
    assert "BUFFERED_STDERR" in stderr


def test_portable_stdin_and_exit_code(portable_builds, tmp_path):
    code, stdout, stderr, observed = _run(portable_builds["normal"], tmp_path,
        overrides={"COIL_PORTABLE_TEST_STDIN": "1", "COIL_PORTABLE_TEST_EXIT": "7"}, input="input through outer launcher")
    assert code == 7, stderr
    assert observed["stdin"] == "input through outer launcher"
    assert "BUFFERED_STDOUT" in stdout and "ATEXIT" in stdout


def test_portable_renaming_preserves_embedded_entry(portable_builds, tmp_path):
    renamed = tmp_path / "renamed café 猫 application.exe"
    shutil.copyfile(portable_builds["normal"], renamed)
    code, stdout, stderr, observed = _run(renamed, tmp_path, ["--flag"])
    assert code == 0, stderr
    assert observed["argv"] == [str(renamed), "--flag"]
    assert observed["executable"] == str(renamed)
    assert observed["asset"] == "UNICODE_ASSET"
    assert "ATEXIT" in stdout


@pytest.mark.parametrize("damage", ["missing_exe", "modified_bytecode", "missing_stdlib", "bad_marker"])
def test_portable_repairs_runtime_and_preserves_user_data(portable_builds, tmp_path, damage):
    exe = portable_builds["normal"]
    code, _, stderr, first = _run(exe, tmp_path)
    assert code == 0, stderr
    # The bootloader uses extended Windows paths for long-path support.
    cache = Path(first["cache"].removeprefix("\\\\?\\")).resolve()
    assert cache.is_relative_to(tmp_path.resolve())
    user_data = cache / "user-created.json"
    user_data.write_text("DO NOT DELETE")
    asset = cache / "assets/café-猫.txt"
    asset.write_text("USER MODIFIED ASSET", encoding="utf-8")
    if damage == "missing_exe":
        (cache / "Portable Probe.exe").unlink()
    elif damage == "modified_bytecode":
        (cache / "_internal/app/main.pyc").write_bytes(b"not valid bytecode")
    elif damage == "missing_stdlib":
        next((cache / "_internal").glob("python*.zip")).unlink()
    else:
        # Windows does not allow CREATE_ALWAYS on a hidden file without
        # matching attributes; opening the existing marker avoids that.
        with (cache / ".coil_ready").open("r+b") as marker:
            marker.write(b"invalid marker")
            marker.truncate()
    code, stdout, stderr, repaired = _run(exe, tmp_path)
    assert code == 0, stderr
    assert repaired["cache"] == first["cache"]
    assert repaired["asset"] == "USER MODIFIED ASSET"
    assert user_data.read_text() == "DO NOT DELETE"
    assert "ATEXIT" in stdout


def test_four_concurrent_first_launches_share_complete_cache(portable_builds, tmp_path):
    exe = portable_builds["normal"]
    processes = []
    try:
        for number in range(4):
            environment, report = _environment(tmp_path, f"concurrent-{number}", {"COIL_PORTABLE_TEST_DELAY": "0.25"})
            process = subprocess.Popen([str(exe), "--instance", str(number)], cwd=tmp_path,
                env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            processes.append((process, report, number))
        caches = set()
        for process, report, number in processes:
            stdout, stderr = _communicate(process)
            assert process.returncode == 0, stderr
            observed = json.loads(report.read_text(encoding="utf-8"))
            assert observed["argv"] == [str(exe), "--instance", str(number)]
            assert observed["asset"] == "UNICODE_ASSET"
            assert "ATEXIT" in stdout
            caches.add(observed["cache"])
        assert len(caches) == 1
    finally:
        for process, _, _ in processes:
            if process.poll() is None:
                _communicate(process, timeout=5)


def _append_certificate_table(exe: Path, output: Path):
    data = bytearray(exe.read_bytes())
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    optional = pe + 24
    assert struct.unpack_from("<H", data, optional)[0] == 0x20B
    security = optional + 112 + 4 * 8
    # This exercises the layout produced by signing, without pretending to
    # create an authentic signature or a trusted certificate.
    data.extend(b"\0" * (-len(data) % 8))
    offset = len(data)
    certificate = struct.pack("<IHH", 16, 0x200, 2) + b"CERTTEST"
    data.extend(certificate)
    struct.pack_into("<II", data, security, offset, len(certificate))
    output.write_bytes(data)


def test_certificate_overlay_executes_and_decompiles(portable_builds, tmp_path):
    exe = tmp_path / "certificate-table.exe"
    _append_certificate_table(portable_builds["normal"], exe)
    code, _, stderr, observed = _run(exe, tmp_path, ["--signed-layout"])
    assert code == 0, stderr
    assert observed["argv"] == [str(exe), "--signed-layout"]
    recovered = tmp_path / "recovered"
    assert decompile(exe, recovered) is True
    assert (recovered / "main.py").read_text(encoding="utf-8") == SOURCE


def test_true_portable_decompile_and_secure_refusal(portable_builds, tmp_path):
    recovered = tmp_path / "recovered"
    assert decompile(portable_builds["normal"], recovered) is True
    assert (recovered / "main.py").read_text(encoding="utf-8") == SOURCE
    secure_output = tmp_path / "secure-recovery"
    assert decompile(portable_builds["secure"], secure_output) is False
    assert not secure_output.exists()
    code, stdout, stderr, observed = _run(portable_builds["secure"], tmp_path, ["--secure"])
    assert code == 0, stderr
    assert observed["argv"] == [str(portable_builds["secure"]), "--secure"]
    assert "ATEXIT" in stdout


def test_cleanup_does_not_remove_a_running_old_cache(portable_builds, real_runtime, tmp_path):
    exe = portable_builds["normal"]
    gate = tmp_path / "release-running-app"
    environment, report = _environment(tmp_path, "held-instance", {"COIL_PORTABLE_TEST_GATE": str(gate)})
    running = subprocess.Popen([str(exe)], cwd=tmp_path, env=environment,
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + 20
        while not report.exists():
            if time.monotonic() >= deadline or running.poll() is not None:
                pytest.fail("First portable instance did not reach the hold point")
            time.sleep(0.05)
        old_cache = Path(json.loads(report.read_text())["cache"])
        project = tmp_path / "project"
        project.mkdir()
        (project / "assets").mkdir()
        (project / "assets/café-猫.txt").write_text("UNICODE_ASSET", encoding="utf-8")
        for generation in range(4):
            (project / "main.py").write_text(SOURCE + f"\n# generation {generation}\n", encoding="utf-8")
            updated = package_portable(project, tmp_path / f"dist-{generation}", real_runtime,
                                      ["main.py"], "Portable Probe", "windows")[0]
            code, _, stderr, observed = _run(updated, tmp_path, label=f"generation-{generation}")
            assert code == 0, stderr
            assert observed["cache"] != str(old_cache)
            assert old_cache.is_dir(), "Cleanup removed the cache of a running older executable"
            assert running.poll() is None
        # Three cache generations are retained, including the leased old one.
        assert len([p for p in old_cache.parent.iterdir() if p.is_dir()]) <= 3
    finally:
        gate.write_text("release")
        stdout, stderr = _communicate(running, timeout=10)
    assert running.returncode == 0, stderr
    assert "ATEXIT" in stdout


def test_unsafe_archive_name_is_rejected_before_extraction(portable_builds, tmp_path):
    original = portable_builds["normal"].read_bytes()
    offset, _, magic = struct.unpack_from("<III", original, len(original) - 12)
    archive_data = io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(original[offset:-12])) as source:
        with zipfile.ZipFile(archive_data, "w", zipfile.ZIP_DEFLATED) as archive:
            # Put the malicious member first so no legitimate file needs to
            # be extracted to observe a traversal attempt.
            archive.writestr("../escaped.txt", "must never leave its cache")
            for item in source.infolist():
                archive.writestr(item, source.read(item.filename))
    payload = archive_data.getvalue()
    exe = tmp_path / "unsafe-archive.exe"
    exe.write_bytes(original[:offset] + payload + struct.pack("<III", offset, zlib.crc32(payload), magic))
    code, _, stderr, observed = _run(exe, tmp_path)
    assert code != 0
    assert observed is None
    assert "archive" in stderr.lower()
    assert not list(tmp_path.rglob("escaped.txt"))

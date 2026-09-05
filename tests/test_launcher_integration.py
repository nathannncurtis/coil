"""Run real bundled executables with Windows-created command lines."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

from coil.packager import package_bundled

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows executables")

PROBE = '''import atexit, json, os, sys, threading, time
from pathlib import Path
result = dict(argv=sys.argv, orig_argv=getattr(sys, "orig_argv", sys.argv),
              executable=sys.executable, frozen=getattr(sys, "frozen", False),
              optimize=sys.flags.optimize, debug=__debug__, path=sys.path,
              isolated=sys.flags.isolated, ignore_environment=sys.flags.ignore_environment,
              utf8=sys.flags.utf8_mode, dont_write_bytecode=sys.dont_write_bytecode,
              xoptions=sys._xoptions, entry=Path(__file__).stem)
Path(os.environ["COIL_TEST_REPORT"]).write_text(json.dumps(result), encoding="utf-8")
atexit.register(lambda: print("ATEXIT"))
print("BUFFERED", end="")
mode = os.environ.get("COIL_TEST_CASE")
if mode == "thread":
    def finish():
        time.sleep(.15)
        print("THREAD")
    threading.Thread(target=finish).start()
elif mode == "exit_string":
    sys.exit("EXIT_EXPLANATION")
elif mode == "exit_int":
    sys.exit(7)
elif mode == "exception":
    raise RuntimeError("TRACEBACK_PROBE")
'''

MP_PROBE = '''import multiprocessing as mp
def square(n):
    return n * n
if __name__ == "__main__":
    mp.freeze_support()
    with mp.Pool(2) as pool:
        print(pool.map(square, [2, 3, 4]))
'''


@pytest.fixture(scope="module")
def launcher_bundle(tmp_path_factory, real_runtime):
    root = tmp_path_factory.mktemp("launcher-integration")
    project = root / "project"
    project.mkdir()
    entries = ["main.py", "Helper With Space.py", "gui.py", "mp_app.py"]
    for entry in entries:
        (project / entry).write_text(MP_PROBE if entry == "mp_app.py" else PROBE, encoding="utf-8")
    bundle = package_bundled(project, root / "dist", real_runtime, entries,
                             "Launcher App", "windows", optimize=1,
                             subsystems={"gui": "gui"})
    return bundle


def run_probe(exe, tmp_path, args=(), case="", overrides=None):
    report = tmp_path / "report.json"
    env = {**os.environ, "COIL_TEST_REPORT": str(report), "COIL_TEST_CASE": case,
           "LOCALAPPDATA": str(tmp_path / "local"), **(overrides or {})}
    result = subprocess.run([str(exe), *args], cwd=tmp_path, env=env,
                            capture_output=True, text=True, timeout=30)
    observed = json.loads(report.read_text(encoding="utf-8")) if report.exists() else None
    return result, observed


@pytest.mark.parametrize("entry", ["main", "Helper With Space", "gui"])
@pytest.mark.parametrize("args", [[], ["--flag", "file"], ["import", "D:\\disc"],
    ["D:\\quoted path with spaces\\café 猫"], ["--flag", "", 'embedded"quote', "ends in slash\\"],
    ["-X", "dev", "-c", "print('must not run')", "--help"]])
def test_exact_application_argv(launcher_bundle, tmp_path, entry, args):
    exe = launcher_bundle / (entry + ".exe")
    proc, observed = run_probe(exe, tmp_path, args)
    assert proc.returncode == 0, proc.stderr
    assert observed["argv"] == [str(exe), *args]
    assert observed["orig_argv"] == [str(exe), *args]
    assert observed["executable"] == str(exe)
    assert observed["frozen"] is True
    assert observed["optimize"] == 1 and observed["debug"] is False
    assert observed["xoptions"] == {}
    assert "BUFFERED" in proc.stdout and "ATEXIT" in proc.stdout


@pytest.mark.parametrize("case,code,message", [("thread", 0, "THREAD"),
    ("exit_int", 7, ""), ("exit_string", 1, "EXIT_EXPLANATION"),
    ("exception", 1, "Traceback (most recent call last)")])
def test_python_shutdown_and_diagnostics(launcher_bundle, tmp_path, case, code, message):
    proc, _ = run_probe(launcher_bundle / "main.exe", tmp_path, case=case)
    assert proc.returncode == code
    assert "BUFFERED" in proc.stdout and "ATEXIT" in proc.stdout
    assert message in proc.stdout + proc.stderr


def test_renamed_secondary_stays_bound_to_secondary(launcher_bundle, tmp_path):
    renamed = launcher_bundle / "renamed helper.exe"
    shutil.copyfile(launcher_bundle / "Helper With Space.exe", renamed)
    proc, observed = run_probe(renamed, tmp_path, ["--flag"])
    assert proc.returncode == 0, proc.stderr
    assert observed["entry"] == "Helper With Space"
    assert observed["argv"] == [str(renamed), "--flag"]
    assert not (launcher_bundle / "_internal/sitecustomize.py").exists()


def test_environment_does_not_configure_python(launcher_bundle, tmp_path):
    exe = launcher_bundle / "main.exe"
    _, clean = run_probe(exe, tmp_path)
    userbase = tmp_path / "userbase"
    usersite = userbase / "Python312" / "site-packages"
    usersite.mkdir(parents=True)
    marker = tmp_path / "outside-pth-executed"
    (usersite / "probe.pth").write_text(
        "import pathlib; pathlib.Path(" + repr(str(marker)) + ").touch()\n")
    proc, dirty = run_probe(exe, tmp_path, overrides={
        "PYTHONPATH": str(tmp_path / "foreign-modules"), "PYTHONHOME": "Z:\\missing",
        "PYTHONSAFEPATH": "1", "PYTHONUTF8": "1", "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONSTARTUP": str(tmp_path / "startup.py"), "PYTHONINSPECT": "1",
        "PYTHONOPTIMIZE": "2", "PYTHONWARNINGS": "error",
        "PYTHONUSERBASE": str(userbase),
    })
    assert proc.returncode == 0, proc.stderr
    assert clean == dirty
    assert dirty["isolated"] == 1 and dirty["ignore_environment"] == 1
    assert not marker.exists()


def test_multiprocessing_spawn(launcher_bundle, tmp_path):
    proc = subprocess.run([str(launcher_bundle / "mp_app.exe")], cwd=tmp_path,
                          capture_output=True, text=True, timeout=40)
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == "[4, 9, 16]"

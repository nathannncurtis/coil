"""Build and run launcher probes. Select Coil with PYTHONPATH for baseline runs.

    python tools/audit_launchers.py --output build/audit/launcher-baseline

All generated files, runtime extraction and reports stay beneath --output.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile

from coil.packager import package_bundled


PROBE = '''import atexit, json, os, sys, threading, time
from pathlib import Path
result = {
    "argv": sys.argv, "orig_argv": getattr(sys, "orig_argv", None),
    "executable": sys.executable, "frozen": getattr(sys, "frozen", False),
    "flags": repr(sys.flags), "path": sys.path, "cwd": os.getcwd(),
    "entry": Path(__file__).stem,
}
Path(os.environ["COIL_AUDIT_REPORT"]).write_text(json.dumps(result), encoding="utf-8")
atexit.register(lambda: print("ATEXIT"))
print("BUFFERED_OUTPUT", end="")
mode = os.environ.get("COIL_AUDIT_CASE")
if mode == "thread":
    def finish():
        time.sleep(.2)
        Path(os.environ["COIL_AUDIT_REPORT"] + ".thread").write_text("DONE")
    threading.Thread(target=finish).start()
elif mode == "exit_string":
    sys.exit("EXPLANATION")
elif mode == "exit_int":
    sys.exit(7)
elif mode == "exception":
    raise RuntimeError("TRACEBACK_PROBE")
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        parser.error("Use a new output directory; audit evidence is never overwritten.")
    output.mkdir(parents=True)
    runtime = output / "runtime"
    cached = Path.home() / ".coil/cache/runtimes/python-3.12.10-embed-amd64.zip"
    with zipfile.ZipFile(cached) as zf:
        zf.extractall(runtime)
    project = output / "project"
    project.mkdir()
    for name in ("main.py", "Helper With Space.py"):
        (project / name).write_text(PROBE, encoding="utf-8")
    bundle = package_bundled(project, output / "dist", runtime,
                             ["main.py", "Helper With Space.py"], "Probe", "windows")
    unrelated = output / "caller with spaces"
    unrelated.mkdir()
    rows = []
    for exe in (bundle / "main.exe", bundle / "Helper With Space.exe"):
        cases = [("none", []), ("flag", ["--flag", "file"]),
                 ("bare", ["import", "D:\\disc"]),
                 ("spaces", ["D:\\path with spaces\\café 猫"]),
                 ("interpreter_options", ["-X", "dev", "-c", "ignored"]),
                 ("exit_int", []), ("exit_string", []), ("exception", []), ("thread", [])]
        for label, argv in cases:
            report = output / f"{exe.stem}-{label}.json"
            env = {**os.environ, "COIL_AUDIT_REPORT": str(report), "COIL_AUDIT_CASE": label}
            proc = subprocess.run([str(exe), *argv], cwd=unrelated, env=env,
                                  capture_output=True, text=True, timeout=20)
            rows.append({"exe": exe.name, "case": label, "input": argv,
                         "returncode": proc.returncode, "stdout": proc.stdout,
                         "stderr": proc.stderr,
                         "observed": json.loads(report.read_text()) if report.exists() else None,
                         "thread_finished": Path(str(report) + ".thread").exists()})
    renamed = bundle / "Renamed.exe"
    shutil.copy2(bundle / "Helper With Space.exe", renamed)
    report = output / "renamed.json"
    proc = subprocess.run([str(renamed)], cwd=unrelated,
                          env={**os.environ, "COIL_AUDIT_REPORT": str(report)},
                          capture_output=True, text=True, timeout=20)
    rows.append({"case": "renamed_helper", "returncode": proc.returncode,
                 "observed": json.loads(report.read_text()) if report.exists() else None})
    # Site isolation includes executable user-site .pth files, not just PYTHONPATH.
    usersite = output / "userbase/Python312/site-packages"
    usersite.mkdir(parents=True)
    marker = output / "external-pth-ran"
    (usersite / "outside.pth").write_text(
        "import pathlib; pathlib.Path(" + repr(str(marker)) + ").touch()\n")
    report = output / "environment.json"
    proc = subprocess.run([str(bundle / "main.exe")], cwd=unrelated,
        env={**os.environ, "COIL_AUDIT_REPORT": str(report),
             "PYTHONUSERBASE": str(output / "userbase"), "PYTHONHOME": "Z:\\missing",
             "PYTHONPATH": str(usersite), "PYTHONUTF8": "1", "PYTHONDONTWRITEBYTECODE": "1"},
        capture_output=True, text=True, timeout=20)
    rows.append({"case": "python_environment", "returncode": proc.returncode,
                 "external_pth_ran": marker.exists(),
                 "observed": json.loads(report.read_text()) if report.exists() else None})
    mp_project = output / "mp-project"
    mp_project.mkdir()
    (mp_project / "main.py").write_text(
        "import multiprocessing as mp, os\n"
        "def worker(): print('WORKER', flush=True)\n"
        "if __name__ == '__main__':\n"
        "    mp.freeze_support()\n"
        "    if not os.environ.get('COIL_CHILD_GUARD'):\n"
        "        os.environ['COIL_CHILD_GUARD'] = '1'\n"
        "        p = mp.Process(target=worker); p.start(); p.join(10)\n"
        "        if p.is_alive(): p.terminate(); p.join()\n"
        "        print('CHILD_EXIT', p.exitcode, flush=True)\n"
        "    else: print('WRONG_APPLICATION_REENTRY', flush=True)\n")
    mp_bundle = package_bundled(mp_project, output / "dist", runtime,
                                ["main.py"], "Multiprocessing", "windows")
    proc = subprocess.run([str(mp_bundle / "Multiprocessing.exe")], cwd=unrelated,
                          capture_output=True, text=True, timeout=20)
    rows.append({"case": "multiprocessing", "returncode": proc.returncode,
                 "stdout": proc.stdout, "stderr": proc.stderr})
    (output / "results.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
    for row in rows:
        observed = row.get("observed") or {}
        print(row["case"], row["returncode"], ascii(observed.get("argv")),
              ascii(row.get("stdout")), ascii(row.get("stderr")))
    print("Evidence:", output / "results.json")


if __name__ == "__main__":
    main()

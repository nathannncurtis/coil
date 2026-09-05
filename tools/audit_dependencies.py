"""Real-build adversarial probes; run with baseline or current as argument."""
from pathlib import Path
import contextlib, io, json, os, shutil, subprocess, sys, zipfile

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "build/audit/dependencies"
VARIANT = sys.argv[1] if len(sys.argv) > 1 else "baseline"
sys.path.insert(0, str(ROOT / ("build/audit/published" if VARIANT == "baseline" else "src")))
WORK = BASE / VARIANT
WORK.mkdir(parents=True, exist_ok=True)
TEMP = WORK / "tmp"
TEMP.mkdir(exist_ok=True)
os.environ.update(TMP=str(TEMP), TEMP=str(TEMP), PIP_NO_INDEX="1", PIP_FIND_LINKS=str(BASE / "wheels"), PIP_CACHE_DIR=str(BASE / "pip-cache"))
import tempfile
tempfile.tempdir = str(TEMP)
import coil.builder as builder
import coil.runtime as runtime
import coil.resolver as resolver

runtime.CACHE_DIR = BASE / "runtime-cache"
runtime.CACHE_DIR.mkdir(exist_ok=True)
cached = Path.home() / ".coil/cache/runtimes/python-3.12.10-embed-amd64.zip"
if not (runtime.CACHE_DIR / cached.name).exists() and cached.exists():
    shutil.copy2(cached, runtime.CACHE_DIR / cached.name)
for version in ("3.12.10", "3.13.7"):
    runtime.download_runtime(version)
builder.get_cache_dir = lambda: WORK / "clean-cache"
original_resolve_version = runtime.resolve_full_version
runtime.resolve_full_version = lambda value, **kwargs: {"3.12": "3.12.10", "3.13": "3.13.7"}.get(value) or original_resolve_version(value, **kwargs)

WHEELS = BASE / "wheels"
WHEELS.mkdir(exist_ok=True)
for version in ("1.0", "2.0"):
    dist = f"coilauditdep-{version}.dist-info"
    path = WHEELS / f"coilauditdep-{version}-py3-none-any.whl"
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("coilauditdep/__init__.py", f"VERSION = {version!r}\n")
        z.writestr(dist + "/METADATA", f"Metadata-Version: 2.1\nName: coilauditdep\nVersion: {version}\n")
        z.writestr(dist + "/WHEEL", "Wheel-Version: 1.0\nGenerator: audit\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
        z.writestr(dist + "/RECORD", "")

for tag in ("cp312", "cp313"):
    dist = "coilauditabi-1.0.dist-info"
    with zipfile.ZipFile(WHEELS / f"coilauditabi-1.0-{tag}-{tag}-win_amd64.whl", "w") as z:
        z.writestr("coilauditabi/__init__.py", f"ABI = {tag!r}\n")
        z.writestr(dist + "/METADATA", "Metadata-Version: 2.1\nName: coilauditabi\nVersion: 1.0\n")
        z.writestr(dist + "/WHEEL", f"Wheel-Version: 1.0\nGenerator: audit\nRoot-Is-Purelib: false\nTag: {tag}-{tag}-win_amd64\n")
        z.writestr(dist + "/RECORD", "")

with zipfile.ZipFile(WHEELS / "coilauditstructure-1.0-py3-none-any.whl", "w") as z:
    z.writestr("coilauditstructure/__init__.py", "from .docs import VALUE\nfrom .tests import HELPERS\n")
    z.writestr("coilauditstructure/docs/__init__.py", "VALUE = 'runtime documentation parser'\n")
    z.writestr("coilauditstructure/tests/__init__.py", "HELPERS = 'runtime fixtures'\n")
    dist = "coilauditstructure-1.0.dist-info"
    z.writestr(dist + "/METADATA", "Metadata-Version: 2.1\nName: coilauditstructure\nVersion: 1.0\n")
    z.writestr(dist + "/WHEEL", "Wheel-Version: 1.0\nGenerator: audit\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
    z.writestr(dist + "/RECORD", "")

RESULTS = {}

def case(name, files, *, secure=False, auto=True, version="3.12", clean=False):
    project = WORK / "projects" / name
    project.mkdir(parents=True, exist_ok=True)
    for rel, content in files.items():
        p = project / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")
    result = {}
    log = io.StringIO()
    try:
        result["resolved"] = resolver.resolve_dependencies(project, version, auto=auto)
        with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
            outputs = builder.build(project, ["main.py"], mode="bundled", python_version=version,
                output_dir=str(WORK / "dist"), name=name, secure=secure, verbose=True, deps_auto=auto, clean=clean)
        bundle = outputs[0]
        report = WORK / (name + ".json")
        report.unlink(missing_ok=True)
        env = dict(os.environ, AUDIT_RESULT=str(report))
        proc = subprocess.run([str(bundle / (name + ".exe"))], cwd=BASE, env=env,
            capture_output=True, text=True, timeout=20)
        result.update(exit=proc.returncode, stdout=proc.stdout, stderr=proc.stderr,
            report=json.loads(report.read_text()) if report.exists() else None,
            bundle=str(bundle), source_files=[str(p.relative_to(bundle)) for p in bundle.rglob("*.py")])
    except Exception as exc:
        result.update(error=f"{type(exc).__name__}: {exc}")
    (WORK / (name + ".log")).write_text(log.getvalue(), encoding="utf-8")
    RESULTS[name] = result
    (WORK / "results.json").write_text(json.dumps(RESULTS, indent=2), encoding="utf-8")
    print(name, json.dumps(result), flush=True)

REPORT = "import os, json\nfrom pathlib import Path\nPath(os.environ['AUDIT_RESULT']).write_text(json.dumps(result))\n"

case("pin", {"main.py": "import coilauditdep\nresult = {'version': coilauditdep.VERSION}\n" + REPORT,
    "requirements.txt": "coilauditdep==1.0\n"})
case("excluded", {"main.py": "result = 'application reached'\n" + REPORT,
    "tests/test_unused.py": "import coilaudit_missing_test_dep\n",
    ".coilignore": "ignored/\n", "ignored/helper.py": "import coilaudit_missing_ignored_dep\n"})
case("assets_secure", {"main.py": "import pkg\nresult = pkg.probe()\n" + REPORT,
    "pkg/__init__.py": "from pathlib import Path\ndef probe():\n return {'file': __file__, 'resource_exists': Path(__file__).with_name('data.json').is_file()}\n",
    "pkg/data.json": "{}", "pkg/private/secret.txt": "PRIVATE_NOT_FOR_BUNDLE",
    "pkg/private/helper.py": "SOURCE_SECRET = 'PLAINTEXT'\n", ".coilignore": "pkg/private/\n", "app.ico": "icon bytes"}, secure=True)
case("stdlib", {"main.py": "import importlib\nresult = {}\nfor name in ['pydoc', 'venv', 'lib2to3', 'xml', 'json']:\n try:\n  importlib.import_module(name)\n  result[name] = 'OK'\n except Exception as exc:\n  result[name] = type(exc).__name__ + ': ' + str(exc)\n" + REPORT})
case("stdlib_direct", {"main.py": "import pydoc\nresult = 'application reached'\n" + REPORT})
case("dynamic", {"main.py": "import importlib\nname = 'coilauditdep'\ntry:\n module = importlib.import_module(name)\n result = {'version': module.VERSION}\nexcept Exception as exc:\n result = type(exc).__name__ + ': ' + str(exc)\n" + REPORT})
case("function_literal", {"main.py": "import importlib\ndef check():\n module = importlib.import_module('coilauditdep')\n return module.VERSION\nresult = check()\n" + REPORT})
case("namespace", {"main.py": "from localns import helper\nresult = helper.VALUE\n" + REPORT,
    "localns/helper.py": "VALUE = 'namespace import works'\n"})
case("src_layout", {"main.py": "import srcpkg\nresult = srcpkg.VALUE\n" + REPORT,
    "src/srcpkg/__init__.py": "VALUE = 'src package'\n"})
case("optimized", {"main.py": "def func():\n 'Function docs'\n return 'OK'\nresult = {'debug': __debug__, 'docstring': func.__doc__}\ntry:\n assert False, 'assertion must execute'\n result['assert'] = 'skipped'\nexcept AssertionError:\n result['assert'] = 'raised'\n" + REPORT})
case("stdlib_cgi", {"main.py": "import cgi\nresult = 'cgi exists in 3.12'\n" + REPORT})
case("cross_version", {"main.py": "import coilauditdep\nresult = coilauditdep.VERSION\n" + REPORT,
    "requirements.txt": "coilauditdep==1.0\n"}, version="3.13")
for name, version in (("cache312", "3.12"), ("cache313", "3.13")):
    case(name, {"main.py": "import coilauditabi\nresult = coilauditabi.ABI\n" + REPORT,
        "requirements.txt": "coilauditabi\n"}, version=version, clean=True)
case("full_version", {"main.py": "result = 'full patch version accepted'\n" + REPORT}, version="3.12.10")
case("dependency_subdirectories", {"main.py": "import coilauditstructure\nresult = [coilauditstructure.VALUE, coilauditstructure.HELPERS]\n" + REPORT})

runtime.CACHE_DIR = WORK / "corrupt-cache"
runtime.CACHE_DIR.mkdir(exist_ok=True)
(runtime.CACHE_DIR / "python-3.12.10-embed-amd64.zip").write_bytes(b"interrupted download")
case("corrupt_runtime_cache", {"main.py": "result = 'runtime repaired'\n" + REPORT,
    "requirements.txt": ""}, version="3.12.10")
runtime.CACHE_DIR = BASE / "runtime-cache"

project = WORK / "projects/pin"
env = dict(os.environ, PYTHONPATH=str(ROOT / ("build/audit/published" if VARIANT == "baseline" else "src")))
p = subprocess.run([sys.executable, "-m", "coil", "build", str(project), "--dry-run", "--python", "3.12.10"], env=env, capture_output=True, text=True)
RESULTS["dry_run"] = dict(exit=p.returncode, stdout=p.stdout, stderr=p.stderr)

preview = WORK / "projects/preview"
preview.mkdir(parents=True, exist_ok=True)
(preview / "runner.py").write_text("import os\nfrom pathlib import Path\nif False:\n import coilaudit_absent_optional\nPath(os.environ['AUDIT_RESULT']).write_text('configured entry reached')\n")
(preview / "coil.toml").write_text('[project]\nentry = "runner.py"\nname = "preview"\n[build]\npython = "3.12.10"\nmode = "bundled"\n[build.dependencies]\nauto = false\n[profile.release]\ninclude = ["coilauditdep==1.0"]\n')
for command, arguments in (("inspect", ["--python", "3.12.10"]), ("build", ["--output", str(WORK / "preview-dist")])):
    p = subprocess.run([sys.executable, "-m", "coil", command, str(preview), "--profile", "release", *arguments],
        env=dict(env, AUDIT_RESULT=str(WORK / "preview.json")), capture_output=True, text=True, timeout=60)
    RESULTS["preview_" + command] = dict(exit=p.returncode, stdout=p.stdout, stderr=p.stderr)
    if command == "build" and p.returncode == 0:
        exe = WORK / "preview-dist/preview/preview.exe"
        p = subprocess.run([str(exe)], env=dict(env, AUDIT_RESULT=str(WORK / "preview.json")), capture_output=True, text=True, timeout=20)
        RESULTS["preview_execute"] = dict(exit=p.returncode, report=(WORK / "preview.json").read_text() if (WORK / "preview.json").exists() else None)
(WORK / "results.json").write_text(json.dumps(RESULTS, indent=2), encoding="utf-8")
print("dry_run", json.dumps(RESULTS["dry_run"]), flush=True)

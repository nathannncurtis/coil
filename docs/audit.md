# Coil launcher and tool audit — 2026-09-05

The bundled launcher defect is fixed with native Windows x64 launchers using CPython's `PyConfig` API and `parse_argv = 0`. Each executable contains its own entry identity. Python runs after initialization and performs normal shutdown. Portable applications use the same inner launcher and preserve the outer executable's identity.

## Baseline and scope

The starting checkout was branch `default-excludes-tests-and-ai-state`, commit `be6845a`, with package metadata 0.2.2. PyPI's published version was **0.2.4**. The downloaded wheel SHA-256 is `64cb026622ba57b7e066e0985177b82eef9a556b9baf4cd2b7ee0e7d6929d2f6`. Its Python sources match tag `v0.2.4` after newline normalization, except `coil.__version__`: the release workflow injects 0.2.4 while the repository still contains 0.1.1. That existing source-version inconsistency is reported, not bumped here.

Work began on `fix/native-launcher-and-audit` from current `origin/master` (`aa26cd9`), which also contains the post-release atexit fix. Published-wheel reproductions use a separate extracted copy. No version was bumped and nothing was published.

Reviewed CLI/configuration, scanner/resolver, runtime acquisition, build/cache orchestration, compiler/obfuscator, both packagers and native code, Windows resource stamping, source recovery, diagnostics/preview, tests, packaging and release workflow. macOS/Linux and ARM64 remain unsupported. Findings below are actual built-executable observations, or actual build/CLI failures where no executable could be produced. Source-only concerns are distinguished in the detailed reports.

## Reproduced findings, ranked

| Impact | Finding and real reproduction | Result |
| --- | --- | --- |
| High: data loss | `tools/audit_paths.py`: `name='../victim'` deletes an owned sibling sentinel directory, then the build fails creating the bootstrap. | Reject path/device/ambiguous output names and source-overwriting destinations before packaging. Sentinel survives. |
| High: source/data disclosure | A secure build containing package data also copies raw package source and explicitly ignored nested files. The app imports that loose source instead of the compiled package. | Apply exclusions per file, keep project source compiled, and supply assets beside compiled modules as well as at the existing bundle root. |
| High: application never runs | `--flag file` exits 2 inside CPython; bare `import D:\disc` loses the conventional argv slot; no args produce `['']`. | Native launcher preserves exact arguments, including empty strings, Unicode, embedded quotes and trailing backslashes. Both console/GUI and all entries execute correctly. |
| High: wrong dependencies | An app requiring a controlled local wheel `==1.0` actually runs version 2.0. | Preserve pins, extras, markers and named URLs; autodetection cannot replace declared constraints. |
| High: broken cross-version builds | A 3.12-host/3.13-target app fails with bad dependency bytecode magic; a clean 3.13 build reuses a cached cp312 wheel. | Compile dependency bytecode with the target runtime; separate dependency caches by target version/OS/architecture and publish complete cache trees atomically. |
| High: premature shutdown | A non-daemon thread never finishes; `SystemExit('EXPLANATION')` loses its message; exceptions lose the traceback. Published 0.2.4 also skips atexit handlers. | `Py_RunMain` supplies normal exit codes, thread joining, atexit, flushing and tracebacks. Existing newer-master atexit work is superseded by normal shutdown. |
| High: incorrect subprocess startup | A real `multiprocessing.Process` re-enters application startup, reports success, and never executes its worker. | Frozen spawn dispatch plus normal `__main__` reloading; actual Pools work with embedded Python 3.9–3.13. |
| High: ambient Python code | `PYTHONUSERBASE` points to an owned user-site `.pth`; the published exe executes it despite reporting isolated mode. `PYTHONUTF8` and `PYTHONDONTWRITEBYTECODE` also change observed flags. | Explicit isolated initialization disables environment/user-site configuration. Tests cover the listed variables, `PYTHONHOME`, `PYTHONPATH`, and Python-looking command-line options. |
| High: missing runtime code | Functional dependency `docs`/`tests` subpackages are deleted; even explicit `import pydoc` is stripped from the stdlib. | Preserve functional dependency directories. Retain explicitly detected stdlib use, dependency imports and the pydoc helper. Arbitrary dynamic stdlib stripping remains a pending policy choice. |
| High: portable I/O/assets | A portable app loses captured stdout/stderr even with flush=True, and Unicode filenames are corrupted during extraction. | Forward standard handles and decode ZIP names correctly; actual stdin, streams, atexit and exit-code tests pass. |
| High for signing | Both a synthetic certificate overlay and **actual SignTool signing** make the published portable exe exit 1 before the app runs. | Locate the payload trailer before certificate data. A self-signed corrected executable runs with exit 0 and source recovery succeeds. Trust/reputation was not asserted. |
| Medium: wrong entry | Rename a secondary bundled exe: it silently runs the primary entry. Duplicate `a/main.py` and `b/main.py` produce one exe running only b. Rename a portable exe: it exits 1. | Bind entry identity inside each launcher; reject colliding entry stems; portable selection uses its embedded entry. |
| Medium: cache damage | Delete the cached inner exe but leave `.coil_ready`: every subsequent launch fails. | Validate and repair runtime files and markers while preserving modified non-code assets and app-created files. |
| Medium: incomplete extraction/cleanup | Native extraction and cleanup had unchecked operations and races. | Four simultaneous first launches, damaged-cache repair, traversal rejection, and keeping an old app running across four newer generations pass actual-process tests. A deterministic baseline race frequency was not measured. |
| Medium: unwanted pip attempts | Imports only in excluded test/ignored folders trigger pip and fail the build. Namespace packages are mistaken for distributions; `cgi` is misclassified on 3.12; full patch-version scanning crashes. | Align scanning with exclusions, recognize local namespace packages, and correct version classification/parsing. Custom in-project output is excluded from repeat-build dependency scans. |
| Medium: incorrect console choice | Real CLI auto build makes both main and GUI entries GUI; `--console` leaves the secondary GUI. | Resolve every entry consistently; explicit per-entry overrides still win. Icon, VERSIONINFO and subsystem stamping are verified on executed native exes. |
| Medium: recovery fails | `coil decompile` cannot recover a genuine default portable app and searches unrelated neighboring metadata. | Read the requested executable's appended archive and bound bundle layout; validate recovery destinations. |
| Medium: tool compatibility/cache | Advertised Python 3.9 cannot import CLI annotations and lacks declared TOML support. A corrupt runtime-cache ZIP is accepted indefinitely. | Postpone annotations, declare the existing TOML dependency on older Python, validate runtime ZIPs and atomically replace corrupt downloads. |
| Low: misleading tooling | `--dry-run` crashes; inspect disagrees with a configured entry/profile whose actual build runs. Runtime license.txt is dropped. | Fix preview configuration/optimization and retain the supplied runtime license file. |

Detailed evidence, exact case names, commands and remaining limits: [dependency/runtime/configuration audit](audit-dependencies.md), [portable audit](audit-portable.md).

## Reproduce launcher and path findings

Install the development test dependencies in an environment of your choice. Extract the published 0.2.4 wheel to `build/audit/published`; keep the source checkout separate. Harnesses require a cached official Python 3.12.10 embedded runtime; integration tests can instead use `COIL_TEST_RUNTIME_ZIP`.

```powershell
$env:PYTHONPATH = "$PWD/build/audit/published"
python tools/audit_launchers.py --output build/audit/baseline-launchers
python tools/audit_paths.py --output build/audit/baseline-paths
python tools/audit_subsystems.py --output build/audit/baseline-subsystems
python tools/audit_portable.py --output build/audit/baseline-portable
$env:PYTHONPATH = "$PWD/src"
python tools/audit_launchers.py --output build/audit/fixed-launchers
python tools/audit_paths.py --output build/audit/fixed-paths
python tools/audit_subsystems.py --output build/audit/fixed-subsystems
python tools/audit_portable.py --output build/audit/fixed-portable
```

Every harness retains its fixtures, actual exes and JSON results beneath its output directory. Use new output paths for repeat runs. Regression tests build actual executables; they do not fake application argv. Windows CI explicitly prepares the runtime and fails instead of silently skipping required executable tests. The native runtime matrix also packages and executes multiprocessing applications against 3.9.13, 3.10.11, 3.11.9, 3.12.10 and 3.13.7.

Native source, manifests, reproducible build commands and encoded binaries are checked in. `tools/build_launcher.py` pins CPython header archive hashes and builds one stub per minor-version PyConfig ABI; `scripts/build_bootloader.py --update` rebuilds the portable extractor. Applications need no C compiler. Legacy `templates/bootloader.exe`/`.pdb` files are retained untouched; packaging uses the rebuilt encoded stub, not those historical artifacts.

Final validation: **416 tests passed, zero skipped**, with `COIL_RUN_NATIVE_INTEGRATION=1`, all five native runtime versions, and the cross-version dependency fixture enabled (`build/audit/pytest-release-review.log`). Wheel and source-distribution builds succeeded. Installing the wheel into a separate target directory, importing Coil from that installation, then building and running a bundled app preserved `[absolute_exe_path, '--flag', 'a path']` exactly. No package upload was performed.

## Choices left unchanged

- **Working directory:** Coil still changes cwd to the bundle/cache root. Caller-relative file paths therefore resolve there. Changing this default needs an explicit decision; argv itself is preserved exactly.
- **GUI diagnostics:** without inherited stderr a GUI exe can still be silent. Persistent logs versus dialogs was asked and left pending. Native logging is implemented as an explicit low-level `error_logging=True` option, disabled by the standard build path.
- **Stdlib stripping:** the remaining candidates are `test`, `tests`, `idlelib`, `turtledemo`, `ensurepip`, `lib2to3`, `venv`, `_pyrepl`, plus tkinter/turtle when not detected. Explicit detected use is retained. Variable-driven `venv`/`lib2to3` imports still fail in the reproduced fixture. Retaining the complete supplied stdlib is the recommended future default; it was not silently imposed.
- **Dynamic imports/source roots:** function-body and literal dynamic imports work; arbitrary variable/plugin imports require declared dependencies. `src/` is not automatically a new import root. See the real failing fixtures in the dependency report.
- **Executable identity:** `sys.executable` is the application, not a general-purpose Python CLI. `subprocess([sys.executable, '-c', ...])` relaunches the application with those arguments. `sys.frozen=True`, `sys.orig_argv`, and optimization flags now reflect the executable contract; Coil does not promise every private PyInstaller attribute.
- **Optimization/security:** default optimize=1 still removes asserts; secure mode also strips docstrings. Bytecode is inspectable; secure mode is not encryption. Default recovery contains source. No new secrecy guarantee is made.
- **Caches/reproducibility:** three-generation retention remains. Mutable cache data survives repair but can disappear with old-generation cleanup; durable data belongs elsewhere. Legacy caches lack safe leases and are left for explicit cleanup. Unpinned dependency caching and timestamped archives do not guarantee reproducible artifacts.

## Signing and EDR evidence

`Get-AuthenticodeSignature` reports the official cached python.exe as **Valid**, the stamped published bundled exe as **NotSigned**, and the new native exe as **NotSigned**. Resource stamping never transfers Python's signer identity to the application. Windows sees a native launcher that loads the bundled Python DLL; portable mode also extracts and launches a child. Sign final artifacts after all resource/subsystem/payload changes. The [SignTool reference](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool) documents the signing and verification operations.

The actual signing test used a one-day local test certificate/PFX under `build/audit/signing`, with `signtool sign /f test.pfx /fd SHA256`. Baseline signed launch returned 1 with no application report; corrected signed launch returned 0 with exact argv and output. SignTool verification rejected the self-signed certificate's untrusted root, as expected; no trust store was changed. This establishes real signature-layout compatibility, not trusted-publisher acceptance, SmartScreen reputation, or EDR approval. No application was uploaded to a public scanning service.

This audit does not certify every third-party extension, Windows release, network share, scanner, ZIP size boundary or dynamic import. Source-only follow-ups and gaps are listed explicitly in the detailed reports rather than presented as reproduced failures.

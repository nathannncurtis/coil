# Portable executable audit

Windows x64, 2026-09-05. Baseline observations come from a real portable executable built with the published `coil-compiler==0.2.4` wheel, using CPython 3.12.10. The original probe, executable, app reports, and result summary remain under `build/audit/portable`; `baseline-results.json` records exit codes and actual captured streams.

The corrected native stub was rebuilt from the checked-in C source and tested with real portable builds. **17 executable integration tests passed in 38.03 seconds**, including concurrent launches, runtime repair, active-cache retention, and recovery from the appended source archive. Test outputs are under `build/audit/pytest-portable-verified`.

The final combined suite passed 416 tests without skips. In addition to synthetic certificate-table regression tests, actual SignTool signing with an isolated local test PFX reproduced baseline exit 1 and corrected exit 0; see the [signing evidence](audit.md#signing-and-edr-evidence). The test certificate was not trusted, and no trusted-publisher or EDR acceptance claim is made.

## Reproduce the current checks

```powershell
$env:PYTHONPATH = 'src'
build/audit/venv/Scripts/python.exe -m pytest tests/test_portable_integration.py -q --basetemp build/audit/pytest-portable-verification
```

The fixture uses the cached Windows embedded runtime and builds genuine `package_portable` executables. Each test sets `LOCALAPPDATA`, `TEMP`, and `TMP` to its own output directory. It runs the executable with Windows-created command lines; assertions inspect app-written JSON and captured pipes. No test hand-constructs application `sys.argv`. Timeouts terminate only the subprocess tree started by that test.

The baseline probe can be rerun with:

```powershell
$env:PYTHONPATH = 'build/audit/published'
build/audit/venv/Scripts/python.exe tools/audit_portable.py --output build/audit/portable-baseline-reproduction
$env:PYTHONPATH = 'build/audit/published'
build/audit/venv/Scripts/python.exe -c "from pathlib import Path; from coil.decompiler import decompile; print(decompile(Path('build/audit/portable/baseline/PortableProbe.exe'), Path('build/audit/portable/baseline-recovered')))"
```

That baseline script confines its extraction cache, synthetic certificate overlay, and intentional missing-inner-executable damage to `build/audit/portable`. It does not change the published wheel.

## Reproduced defects

| Severity | Baseline observation | Corrected behavior and executable evidence |
| --- | --- | --- |
| High | Portable mode inherits the copied-interpreter defect: fresh no-argument launch reports `sys.argv == ['']`, and `sys.executable` points into the extraction cache. | The portable archive contains the native application launcher. Tests pass flags, bare words, spaces, Unicode, embedded quotes, empty arguments, trailing backslashes, and CPython-looking `-X`/`-c` options. Exact argv/orig_argv start with the outer executable path; sys.executable is the outer executable; sys.frozen is true. |
| High | Fresh launch returns 0 while captured stdout and stderr are both empty, even though the application explicitly prints and flushes distinct markers to each. | Inherited standard handles reach the inner process. Tests observe buffered stdout, stderr, and `atexit` output; stdin reaches the app; application exit 7 reaches the parent unchanged. |
| High | Asset `assets/café-猫.txt` becomes mojibake during extraction. The real app reports `asset_exists: false` and a mangled filename containing UTF-8 bytes interpreted as individual characters. | ZIP names decode to Unicode and extraction uses Windows wide-character APIs. Real app reads the correctly named asset and verifies its exact name/content. |
| High for signed distribution | Appending a structurally valid PE certificate table causes exit 1. The baseline assumes the Coil trailer is at the physical end of the file, where a signature changes the layout. | Both native launch and source recovery locate the trailer before certificate-table padding/overlay. A synthetic certificate table passes actual launch and decompilation tests. This is a layout test; it does **not** constitute a cryptographically valid Authenticode signature or signing-service certification. |
| Medium | Copying `PortableProbe.exe` to `Renamed.exe` causes exit 1: the outer filename is incorrectly used to select a nonexistent inner executable. | The inner entry comes from the embedded boot script; the visible executable identity still reflects the renamed outer file. A renamed outer filename containing spaces and Unicode launches correctly with exact argv. |
| Medium | Delete only the cached inner exe while leaving `.coil_ready`: subsequent launches return 1 indefinitely, because the marker is trusted without checking the payload. | Validate the marker and executable/runtime contents; repair missing or modified runtime members. Tests cover a missing inner exe, overwritten main bytecode, missing stdlib ZIP, and corrupt marker. All recover, while preserving both user-created JSON and deliberately edited non-code assets. |
| Medium | `coil decompile` rejects a genuine default-mode portable exe with `No Coil metadata found`, although the executable contains the source archive. | Read the embedded archive belonging to the requested executable, including certificate overlays. The recovered source equals the original source exactly. A secure portable executable runs but is refused by Coil's recovery command, as intended. |

## Additional adversarial checks on the fixes

Four processes were launched simultaneously against an empty shared cache. Each application received its own complete argv, read its asset successfully, flushed output, and exited 0. All four used the same complete cache directory. This validates the repaired extraction/launch coordination; it does not establish an exact baseline race failure frequency.

For cleanup, an old application was held running while four distinct newer builds with the same application name were built and executed. The old cache remained present and usable throughout. Cleanup retained at most three generations, including the leased running generation. Releasing the held app produced normal exit and `atexit` output. This checks active-file retention with actual processes, rather than a filesystem-only mock.

A rebuilt portable archive whose first member was `../escaped.txt` was executed. The launcher returned a failure with an archive diagnostic before extraction, and no escaped file appeared anywhere in the test output. Native extraction also checks sizes, decompression results, and ZIP CRCs. This test is intentionally limited to an owned output tree; no path outside it was targeted.

Cache identity now uses SHA-256 of the archive. Runtime repair verifies the launcher and executable runtime files while preserving mutable non-code assets and files created by the app. These checks address accidental corruption and stale cache reuse. They are not a trust boundary against a user who can replace the executable itself or modify the process.

## Retained choices and coverage limits

Portable apps execute from an extraction directory, and the current working directory remains that directory to preserve Coil's existing asset behavior. Durable user data should not be stored there: old inactive cache generations are eligible for cleanup. Changing the working-directory/data-location contract is a product decision and was not done as part of the launcher correction.

The cache location falls back from local application data to the temporary directory and then beside the executable when necessary. The successful tests used a writable `LOCALAPPDATA` under their own output; the fallback paths, network shares, and permissions-denied matrix were not exhaustively exercised.

Windows extended paths may appear in `__file__` within the extracted runtime. The tests normalize that prefix only when checking ownership before deliberate cache damage. The public `sys.executable`/argv identity remains the outer executable path.

The default GUI diagnostic policy is retained. Captured stderr works and is tested. An unattached GUI launch can still lack visible error output; no new dialog or logging policy is silently imposed. EDR verdicts, SmartScreen reputation, trusted signing, arbitrary native dependencies, every Windows version, and archives near the format's size limits were not certified by these tests.

The retained three-generation cleanup policy does not remove legacy cache directories that lack the new lease format. This avoids deleting caches that may belong to running applications from older Coil builds; legacy disk usage may require explicit cache management.

# Coil

![Python](https://img.shields.io/badge/python-3.9%2B-blue)
![License](https://img.shields.io/badge/license-Apache%202.0-green)
![Platform](https://img.shields.io/badge/platform-Windows-lightgrey)
![Version](https://img.shields.io/badge/version-0.1.0-orange)

**A Python-to-executable compiler that just works.**

---

## Why Coil?

Existing tools for turning Python projects into executables — PyInstaller, cx_Freeze, Nuitka, py2exe — all share the same problem: they're complicated. Hidden imports, missing DLLs, spec files, hook scripts, cryptic errors. You spend more time fighting the tool than building your app.

Coil takes a different approach: **directory in, executable out.** Point it at your project folder, and it handles the rest. No spec files. No hook scripts. No per-file configuration.

- Auto-detects entry points
- Auto-detects dependencies (or reads your requirements.txt)
- Bundles an embedded Python runtime — no Python installation needed on the target machine
- Produces a single portable .exe or a clean bundled directory
- Built-in decompiler to recover your own source if you need it

## Quick Start

```bash
pip install coil-compiler
coil init ./myproject        # Generate coil.toml config
coil build ./myproject       # Build your executable
```

Or, if `coil` isn't on your PATH:

```bash
python -m coil init ./myproject
python -m coil build ./myproject
```

That's it. Your executable is in `./dist/`.

## Installation

```bash
pip install coil-compiler
```

**Requirements:**
- Python 3.9 or later
- Windows (macOS and Linux support planned)
- pip (for dependency installation during builds)

> **Windows note:** If `coil` isn't recognized after install, your pip Scripts directory may not be on PATH. You can either:
> - Use `python -m coil` instead (works the same way — all examples below apply)
> - Or add Python's Scripts directory to your PATH (`python -m site --user-site` will show you where it is)

## Usage

### Basic Build

```bash
# Auto-detect entry point, build portable exe
coil build ./myproject
```

### Specify Entry Point

```bash
coil build ./myproject --entry app.py
```

### Portable Mode (Default)

Single standalone .exe. Copy it anywhere and run it. No installation needed.

```bash
coil build ./myproject --mode portable
```

### Bundled Mode

Directory containing compiled application files. Multiple scripts become multiple compiled files.

```bash
coil build ./myproject --mode bundled
```

### GUI Application

Coil auto-detects GUI frameworks. If your project imports `tkinter`, `PyQt5`, `PyQt6`, `PySide2`, `PySide6`, `wx`, `kivy`, `pygame`, `pyglet`, `dearpygui`, `customtkinter`, `flet`, `pystray`, `infi.systray`, or `plyer`, the console window is hidden automatically.

```bash
# No --gui needed — Coil detects tkinter/PyQt5/etc. and hides the console
coil build ./myproject

# Force console window even with GUI imports
coil build ./myproject --console

# Explicitly set GUI mode (redundant if auto-detected, but works)
coil build ./myproject --gui
```

### Specify Python Version

```bash
coil build ./myproject --python 3.12
```

### Dependency Control

```bash
# Exclude packages
coil build ./myproject --exclude numpy,pandas

# Force-include packages
coil build ./myproject --include extra-lib

# Use a specific requirements file
coil build ./myproject --requirements ./reqs.txt
```

### Multiple Entry Points

Each entry point produces its own executable:

```bash
coil build ./myproject --entry cli.py --entry gui.py --mode bundled
```

### Secure Build

Heavy obfuscation. Cannot be reversed by `coil decompile`:

```bash
coil build ./myproject --secure
```

### Decompile

Recover source from a default (non-secure) Coil build:

```bash
coil decompile ./dist/MyApp.exe --output ./recovered
```

Secure builds cannot be decompiled.

### Clean Build

Build in an isolated environment with only declared dependencies. Guarantees reproducible builds:

```bash
coil build ./myproject --clean
```

The clean environment is cached — subsequent builds reuse it if dependencies haven't changed.

### Project Setup

Generate a `coil.toml` config file for your project:

```bash
coil init ./myproject
```

This asks a few questions (entry point, console/GUI, icon) and writes a `coil.toml` with sensible defaults. After that, `coil build ./myproject` uses the config automatically — no flags needed.

### Build Profiles

Define named profiles in `coil.toml` for different build scenarios:

```toml
[profile.dev]
mode = "bundled"
secure = false
verbose = true

[profile.release]
mode = "portable"
secure = true
```

Switch between them with `--profile`:

```bash
coil build ./myproject --profile dev
coil build ./myproject --profile release
```

CLI flags always override profile settings.

### Pre-Build Diagnostics

Check for problems before building:

```bash
coil doctor ./myproject
```

Checks Python version, runtime availability, write permissions, config validity, and known package issues.

### Build Preview

See what Coil will include in a build without building:

```bash
coil inspect ./myproject
```

Shows entry point, dependencies (stdlib vs third-party), estimated output size, and config.

### Dry Run

See what would be built without building:

```bash
coil build ./myproject --dry-run --verbose
```

### Full Example

```bash
coil build ./myproject \
  --entry main.py \
  --mode portable \
  --os windows \
  --python 3.12 \
  --gui \
  --secure \
  --exclude numpy,pandas \
  --output ./dist \
  --name MyApp \
  --icon ./assets/icon.ico
```

## How It Works

1. **Dependency Resolution** — Coil checks for a `requirements.txt` or `pyproject.toml`. If neither exists, it scans every `.py` file using Python's `ast` module to find all imports, separates stdlib from third-party, and resolves import names to PyPI packages.

2. **Runtime Bundling** — Downloads the official Windows embeddable Python distribution matching your target version. No C compiler needed.

3. **Compilation** — All `.py` files are compiled to `.pyc` bytecode. No loose `.py` files in the output.

4. **Packaging** — In portable mode, everything is packed into a single `.exe` file. In bundled mode, a clean directory with the runtime, compiled code, and dependencies.

### Portable Mode Details

The portable `.exe` is a single file you can copy anywhere and run. Here's what happens under the hood:

- **You distribute one file.** The `.exe` contains a lightweight native launcher (~23 KB) with the full application payload appended.
- **First launch** extracts the runtime to a local cache (`%LOCALAPPDATA%\coil\<app>\<build_hash>\`). This is a one-time operation.
- **Subsequent launches** detect the cache and start instantly — no extraction needed.
- **Each build gets a unique hash.** Rebuilding your app creates a new cache entry. Old cache entries are automatically cleaned up (only the 3 most recent are kept).
- **Cache safety:** Extraction uses file locking to prevent corruption if multiple instances launch simultaneously. A marker file ensures only fully-extracted caches are used — if extraction is interrupted, it will restart cleanly.

To manage the cache manually:

```bash
coil cache info     # Show cache location and size
coil cache clear    # Delete all cached runtimes
```

> **Tip:** If you have a `requirements.txt` or `pyproject.toml`, Coil uses that instead of scanning imports. This is faster and more reliable.

## Configuration (`coil.toml`)

Run `coil init` to generate a config file, or create one manually:

```toml
[project]
entry = "main.py"
name = "MyApp"

[build]
mode = "portable"
os = "windows"
console = true
python = "3.12"
clean = false

[build.dependencies]
# Coil resolves deps automatically from requirements.txt or AST scan
auto = true
exclude = []
include = []

[build.output]
dir = "./dist"
icon = ""

# Profiles override [build] settings
# [profile.dev]
# mode = "bundled"
# secure = false
# verbose = true

# [profile.release]
# mode = "portable"
# secure = true
```

**Priority order:** CLI flags > profile values > `[build]` values > defaults.

## CLI Reference

### Subcommands

| Command | Description |
|---------|-------------|
| `coil build <project>` | Build a Python project into an executable |
| `coil init [project]` | Generate a coil.toml config file |
| `coil doctor [project]` | Run pre-build diagnostics |
| `coil inspect [project]` | Preview what Coil will include in a build |
| `coil decompile <exe>` | Recover source from a default Coil build |
| `coil cache info` | Show cache location and size |
| `coil cache clear` | Delete all cached runtimes |

### Build Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--entry` | Auto-detect (`__main__.py` then `main.py`) | Entry point script relative to project dir |
| `--mode` | `portable` | `portable` (single exe) or `bundled` (directory) |
| `--os` | Current OS | `windows`, `macos`, `linux` |
| `--python` | Auto-detect | Target Python version |
| `--gui` | `false` | Suppress console window |
| `--console` | `true` | Show console window (default) |
| `--secure` | `false` | Heavy obfuscation, not reversible |
| `--clean` | `false` | Build in clean environment with only declared deps |
| `--profile` | None | Build profile from coil.toml |
| `--exclude` | None | Comma-separated packages to exclude |
| `--include` | None | Comma-separated packages to force-include |
| `--output` | `./dist` | Output directory |
| `--name` | Project dir name | Output executable name |
| `--icon` | None | Icon file path (.ico) |
| `--requirements` | Auto-detect | Path to requirements.txt or pyproject.toml |
| `--verbose` | `false` | Detailed build output |
| `--dry-run` | `false` | Preview build without executing |

## Obfuscation

**Default mode:** Source is compiled to bytecode and packaged with metadata that `coil decompile` can use to recover the original `.py` files. This is a safety net — not cryptographic security, but your source isn't casually visible.

**Secure mode (`--secure`):** Bytecode only, debug info stripped, no recovery metadata. `coil decompile` will refuse to process it. This is stronger but not impenetrable — no bytecode obfuscation is NSA-proof. It's meant to raise the bar, not guarantee secrecy.

## FAQ

**Q: Does Coil require Python on the target machine?**
No. Coil bundles an embedded Python runtime. The resulting executable is fully standalone.

**Q: What about C extensions / native modules?**
Coil bundles `.pyd` / `.dll` files from installed packages. Most packages with C extensions work out of the box.

**Q: How big are the executables?**
A minimal project produces an exe around 15-20 MB (mostly the Python runtime). Dependencies add to that. Coil strips unnecessary files to keep size reasonable.

**Q: Can I cross-compile for other platforms?**
Not yet. Currently Coil only builds Windows executables on Windows. Cross-platform and cross-compilation are on the roadmap.

**Q: What's the difference between portable and bundled?**
Portable = single `.exe` file you can copy anywhere. On first launch, it extracts the runtime to a local cache and runs from there. Later launches reuse the cache and start instantly. Bundled = directory with the executable and its supporting files. No extraction step — it runs directly from the directory.

**Q: Where does the portable exe store its cache?**
`%LOCALAPPDATA%\coil\<AppName>\<build_hash>\`. Run `coil cache info` to see details, or `coil cache clear` to remove it. If `%LOCALAPPDATA%` isn't available, it falls back to `%TEMP%` or the exe's own directory.

**Q: Is the portable exe really a single file?**
Yes. The build output is a single `.exe`. On first run, it extracts a cached copy of the runtime to a local directory. This is not visible to the user as a separate step — the app just starts. Subsequent runs skip extraction entirely.

## Roadmap

- macOS .app support
- Linux ELF binary support
- ARM64 bootloader for Windows on ARM
- Cross-compilation
- Optional runtime signing (helps with AV false positives)

## Contributing

Contributions welcome. Open an issue or submit a pull request.

```bash
# Development setup
git clone https://github.com/nathannncurtis/coil.git
cd coil
pip install -e .
python -m pytest
```

## License

Apache License 2.0. See [LICENSE](LICENSE) for the full text.

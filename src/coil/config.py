"""Configuration management for Coil.

Handles reading and writing coil.toml files, and merging config
with CLI arguments (CLI > profile > [build] > defaults).
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any, Optional

if sys.version_info >= (3, 11):
    import tomllib
else:
    try:
        import tomllib
    except ImportError:
        import tomli as tomllib  # type: ignore[no-redef]


# Default config values
DEFAULTS = {
    "mode": "portable",
    "os": "windows",
    "console": True,
    "secure": False,
    "verbose": False,
    "clean": False,
    "output_dir": "./dist",
    "icon": "",
    "python": "",
    "entry": "",
    "name": "",
    "exclude": [],
    "include": [],
    "deps_auto": True,
}


def load_config(project_dir: Path) -> Optional[dict[str, Any]]:
    """Load coil.toml from a project directory.

    Returns None if no coil.toml exists. Raises on parse errors.
    """
    toml_path = project_dir / "coil.toml"
    if not toml_path.is_file():
        return None

    with open(toml_path, "rb") as f:
        return tomllib.load(f)


def _resolve_project_version(raw: dict[str, Any], project_dir: Optional[Path]) -> str:
    """Return the project version string.

    Priority: [project].version > version.txt at project root > "".
    """
    project = raw.get("project", {})
    version = project.get("version", "")
    if version:
        return str(version).strip()
    if project_dir is not None:
        version_txt = project_dir / "version.txt"
        if version_txt.is_file():
            return version_txt.read_text(encoding="utf-8").strip()
    return ""


def _pad_version(version: str) -> str:
    """Pad a version string to 4 dot-separated parts (e.g. "1.2" -> "1.2.0.0").

    Accepts hyphen as separator. Non-numeric parts become 0.
    """
    if not version:
        return "1.0.0.0"
    parts = version.replace("-", ".").split(".")
    nums = []
    for p in parts[:4]:
        try:
            nums.append(str(int(p)))
        except ValueError:
            nums.append("0")
    while len(nums) < 4:
        nums.append("0")
    return ".".join(nums)


def get_versioninfo_config(
    raw: dict[str, Any],
    entry_name: str,
    project_name: str,
    project_dir: Optional[Path] = None,
) -> dict[str, str]:
    """Resolve VERSIONINFO fields for a single entry.

    Merges shared [build.versioninfo] with per-entry
    [build.versioninfo.entries.<stem>] overrides. Falls back to sensible
    defaults derived from the entry name and project metadata.

    Args:
        raw: Parsed coil.toml.
        entry_name: Entry point stem (e.g. "main" for "main.py"). For stems
            with spaces, TOML requires quoted keys: ["With Spaces"].
        project_name: Project name (from [project].name or directory name).
        project_dir: Project directory, used for version.txt fallback.

    Returns:
        Dict with keys matching set_version_info kwargs: product_name,
        file_description, file_version, product_version, company_name,
        legal_copyright, internal_name, original_filename, comments.
    """
    build = raw.get("build", {})
    vi = build.get("versioninfo", {}) or {}
    per_entry = (vi.get("entries", {}) or {}).get(entry_name, {}) or {}

    version = _resolve_project_version(raw, project_dir)
    default_version = _pad_version(version) if version else "1.0.0.0"

    def pick(key: str, default: str) -> str:
        if key in per_entry and per_entry[key] not in (None, ""):
            return str(per_entry[key])
        if key in vi and vi[key] not in (None, ""):
            return str(vi[key])
        return default

    product_name = pick("product_name", project_name or entry_name)
    file_description = pick("file_description", entry_name)
    internal_name = pick("internal_name", entry_name)
    original_filename = pick("original_filename", f"{entry_name}.exe")
    file_version = _pad_version(pick("file_version", default_version))
    product_version = _pad_version(pick("product_version", file_version))
    company_name = pick("company_name", "")
    legal_copyright = pick("legal_copyright", "")
    comments = pick("comments", "")

    return {
        "product_name": product_name,
        "file_description": file_description,
        "internal_name": internal_name,
        "original_filename": original_filename,
        "file_version": file_version,
        "product_version": product_version,
        "company_name": company_name,
        "legal_copyright": legal_copyright,
        "comments": comments,
    }


def get_subsystem_config(raw: dict[str, Any], entry_name: str) -> Optional[str]:
    """Resolve the explicit PE subsystem for a single entry.

    Reads [build.entries.<stem>].subsystem. Returns "gui" or "console" when
    set, None when absent (callers fall through to the existing hybrid
    default: top-level --gui flag for the primary entry, per-file GUI-import
    autodetect for extras).

    Args:
        raw: Parsed coil.toml.
        entry_name: Entry point stem (e.g. "main" for "main.py").

    Returns:
        "gui", "console", or None.

    Raises:
        ValueError: If the configured value is not "gui" or "console".
    """
    build = raw.get("build", {})
    entries = build.get("entries", {}) or {}
    per_entry = entries.get(entry_name, {}) or {}
    value = per_entry.get("subsystem")

    if value is None:
        return None
    if value not in ("gui", "console"):
        raise ValueError(
            f"Invalid subsystem for entry {entry_name!r}: "
            f"expected 'console' or 'gui', got {value!r}"
        )
    return value


def get_build_config(
    raw: dict[str, Any],
    profile: Optional[str] = None,
) -> dict[str, Any]:
    """Extract a flat build config from parsed toml, optionally applying a profile.

    Priority: profile values > [build] values > defaults.

    Returns a flat dict with keys matching CLI argument names.
    """
    project = raw.get("project", {})
    build = raw.get("build", {})
    deps = build.get("dependencies", {})
    output = build.get("output", {})

    config: dict[str, Any] = {}

    # From [project]
    config["entry"] = project.get("entry", "")
    config["name"] = project.get("name", "")

    # From [build]
    config["mode"] = build.get("mode", DEFAULTS["mode"])
    config["os"] = build.get("os", DEFAULTS["os"])
    config["console"] = build.get("console", DEFAULTS["console"])
    config["secure"] = build.get("secure", DEFAULTS["secure"])
    config["verbose"] = build.get("verbose", DEFAULTS["verbose"])
    config["clean"] = build.get("clean", DEFAULTS["clean"])
    config["python"] = build.get("python", DEFAULTS["python"])

    # From [build.dependencies]
    config["deps_auto"] = deps.get("auto", DEFAULTS["deps_auto"])
    config["exclude"] = deps.get("exclude", [])
    config["include"] = deps.get("include", [])

    # From [build.output]
    config["output_dir"] = output.get("dir", DEFAULTS["output_dir"])
    config["icon"] = output.get("icon", DEFAULTS["icon"])

    # Apply profile overrides
    if profile:
        profiles = raw.get("profile", {})
        if profile not in profiles:
            available = ", ".join(sorted(profiles.keys())) if profiles else "none"
            raise ValueError(
                f"Profile '{profile}' not found in coil.toml. "
                f"Available profiles: {available}"
            )
        prof = profiles[profile]

        # Profile can override any build-level key
        for key in ("mode", "os", "console", "secure", "verbose", "clean", "python"):
            if key in prof:
                config[key] = prof[key]
        if "icon" in prof:
            config["icon"] = prof["icon"]
        if "output_dir" in prof:
            config["output_dir"] = prof["output_dir"]
        if "entry" in prof:
            config["entry"] = prof["entry"]
        if "name" in prof:
            config["name"] = prof["name"]
        if "exclude" in prof:
            config["exclude"] = prof["exclude"]
        if "include" in prof:
            config["include"] = prof["include"]

    return config


def generate_toml(
    entry: str,
    name: str,
    console: bool = True,
    icon: str = "",
    python_version: str = "",
    has_requirements: bool = False,
    has_pyproject: bool = False,
) -> str:
    """Generate a coil.toml file content string."""
    gui_line = "console = true" if console else "console = false"

    deps_comment = ""
    if has_requirements:
        deps_comment = "# Coil will use your requirements.txt for dependency resolution\n"
    elif has_pyproject:
        deps_comment = "# Coil will use your pyproject.toml for dependency resolution\n"

    icon_line = f'icon = "{icon}"' if icon else 'icon = ""'

    python_line = f'python = "{python_version}"' if python_version else '# python = "3.12"'

    return f'''\
[project]
entry = "{entry}"
name = "{name}"

[build]
mode = "portable"
os = "windows"
{gui_line}
{python_line}

[build.dependencies]
{deps_comment}auto = true
exclude = []
include = []

[build.output]
dir = "./dist"
{icon_line}

# Profiles override [build] settings. Use with: coil build --profile <name>
# [profile.dev]
# mode = "bundled"
# secure = false
# verbose = true
#
# [profile.release]
# mode = "portable"
# secure = true
# verbose = false
# icon = "./assets/icon.ico"
'''

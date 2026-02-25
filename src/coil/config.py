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

"""The [compile] table and the compile keys of [build].

Spec section 13.1. Parsing and validation only; running coilc is the
driver's job (section 13.2).
"""

from __future__ import annotations

import re
from typing import Any

# Python versions with an impersonated ABI (spec decision 9: cp312
# first, then cp313).
SUPPORTED_PYTHON_VERSIONS = ("3.12", "3.13")

OPT_LEVELS = ("speed", "size")
DEFAULT_STACK_BYTES = 16 * 1024 * 1024

COMPILE_DEFAULTS: dict[str, Any] = {
    "keep": [],
    "report": True,
    "opt": "speed",
    "stack": DEFAULT_STACK_BYTES,
}

_STACK_PATTERN = re.compile(r"(?i)^(\d+)\s*(b|kib|mib|gib)?$")
_STACK_MULTIPLIERS = {
    "": 1,
    "b": 1,
    "kib": 1024,
    "mib": 1024 * 1024,
    "gib": 1024 * 1024 * 1024,
}


def parse_stack_size(value: Any, where: str = "[compile] stack") -> int:
    """Parse a stack size into bytes.

    Accepts a positive byte count or a string with a B, KiB, MiB or
    GiB unit, e.g. "16MiB".
    """
    if isinstance(value, int) and not isinstance(value, bool):
        if value <= 0:
            raise ValueError(f"{where} must be a positive size, got {value}")
        return value
    if isinstance(value, str):
        match = _STACK_PATTERN.match(value.strip())
        if match is not None:
            size = int(match.group(1)) * _STACK_MULTIPLIERS[(match.group(2) or "").lower()]
            if size > 0:
                return size
    raise ValueError(f'{where} must be a size like "16MiB", got {value!r}')


def format_stack_size(size: int) -> str:
    """Render a byte count back to its shortest exact unit form."""
    for unit, factor in (("GiB", 1024 ** 3), ("MiB", 1024 ** 2), ("KiB", 1024)):
        if size >= factor and size % factor == 0:
            return f"{size // factor}{unit}"
    return f"{size}B"


def validate_compile_flag(value: Any, where: str = "[build] compile") -> bool:
    """Validate a compile on/off setting from coil.toml."""
    if not isinstance(value, bool):
        raise ValueError(f"{where} must be a boolean, got {value!r}")
    return value


def require_supported_python(python_version: Any, setting: str) -> None:
    """Raise unless python_version has an impersonated ABI.

    The error lists the supported versions (spec section 13.1).
    """
    if python_version not in SUPPORTED_PYTHON_VERSIONS:
        supported = " or ".join(SUPPORTED_PYTHON_VERSIONS)
        raise ValueError(
            f"{setting} must be {supported} to compile natively; got {python_version!r}"
        )


def parse_compile_table(
    table: Any,
    base: dict[str, Any] | None = None,
    where: str = "[compile]",
) -> dict[str, Any]:
    """Validate a raw [compile] table into keep/report/opt/stack.

    Keys absent from the table keep their value from base, the
    lower-priority layer; base defaults to COMPILE_DEFAULTS. Unknown
    keys are ignored, as elsewhere in coil.toml.
    """
    if table is None:
        table = {}
    if not isinstance(table, dict):
        raise ValueError(f"{where} must be a table")

    config = dict(COMPILE_DEFAULTS if base is None else base)

    if "keep" in table:
        keep = table["keep"]
        if not isinstance(keep, list) or not all(isinstance(p, str) for p in keep):
            raise ValueError(f"{where} keep must be a list of patterns, got {keep!r}")
        config["keep"] = list(keep)

    if "report" in table:
        report = table["report"]
        if not isinstance(report, bool):
            raise ValueError(f"{where} report must be a boolean, got {report!r}")
        config["report"] = report

    if "opt" in table:
        opt = table["opt"]
        if opt not in OPT_LEVELS:
            raise ValueError(f'{where} opt must be "speed" or "size", got {opt!r}')
        config["opt"] = opt

    if "stack" in table:
        config["stack"] = parse_stack_size(table["stack"], where=f"{where} stack")

    return config

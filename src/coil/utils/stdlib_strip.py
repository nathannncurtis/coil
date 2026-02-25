"""Stdlib modules that can be safely stripped from the embedded runtime.

These are never needed at runtime for typical Coil-built applications.
"""

# Top-level modules/packages to remove from the stdlib zip.
# These are large and unused by virtually all applications.
ALWAYS_STRIP: set[str] = {
    "test",
    "tests",
    "unittest",
    "idlelib",
    "turtledemo",
    "pydoc.pyc",
    "pydoc_data",
    "doctest.pyc",
    "ensurepip",
    "lib2to3",
    "venv",
    "_pyrepl",
}

# Modules stripped unless the project actually imports them.
STRIP_UNLESS_USED: dict[str, str] = {
    "tkinter": "tkinter",
    "turtle.pyc": "turtle",
}

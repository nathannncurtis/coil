"""Standard library module lists per Python version."""

# Common stdlib modules present across Python 3.9-3.13.
# This is the authoritative list used to distinguish stdlib from third-party.
_COMMON_STDLIB = frozenset({
    "__future__", "_thread", "abc", "aifc", "argparse", "array", "ast",
    "asynchat", "asyncio", "asyncore", "atexit", "audioop", "base64",
    "bdb", "binascii", "binhex", "bisect", "builtins", "bz2", "calendar",
    "cgi", "cgitb", "chunk", "cmath", "cmd", "code", "codecs", "codeop",
    "collections", "colorsys", "compileall", "concurrent", "configparser",
    "contextlib", "contextvars", "copy", "copyreg", "cProfile", "crypt",
    "csv", "ctypes", "curses", "dataclasses", "datetime", "dbm", "decimal",
    "difflib", "dis", "distutils", "doctest", "email", "encodings",
    "enum", "errno", "faulthandler", "fcntl", "filecmp", "fileinput",
    "fnmatch", "formatter", "fractions", "ftplib", "functools", "gc",
    "getopt", "getpass", "gettext", "glob", "grp", "gzip", "hashlib",
    "heapq", "hmac", "html", "http", "idlelib", "imaplib", "imghdr",
    "imp", "importlib", "inspect", "io", "ipaddress", "itertools", "json",
    "keyword", "lib2to3", "linecache", "locale", "logging", "lzma",
    "mailbox", "mailcap", "marshal", "math", "mimetypes", "mmap",
    "modulefinder", "multiprocessing", "netrc", "nis", "nntplib",
    "numbers", "operator", "optparse", "os", "ossaudiodev", "parser",
    "pathlib", "pdb", "pickle", "pickletools", "pipes", "pkgutil",
    "platform", "plistlib", "poplib", "posix", "posixpath", "pprint",
    "profile", "pstats", "pty", "pwd", "py_compile", "pyclbr",
    "pydoc", "queue", "quopri", "random", "re", "readline", "reprlib",
    "resource", "rlcompleter", "runpy", "sched", "secrets", "select",
    "selectors", "shelve", "shlex", "shutil", "signal", "site", "smtpd",
    "smtplib", "sndhdr", "socket", "socketserver", "spwd", "sqlite3",
    "sre_compile", "sre_constants", "sre_parse", "ssl", "stat",
    "statistics", "string", "stringprep", "struct", "subprocess",
    "sunau", "symtable", "sys", "sysconfig", "syslog", "tabnanny",
    "tarfile", "telnetlib", "tempfile", "termios", "test", "textwrap",
    "threading", "time", "timeit", "tkinter", "token", "tokenize",
    "tomllib", "trace", "traceback", "tracemalloc", "tty", "turtle",
    "turtledemo", "types", "typing", "unicodedata", "unittest", "urllib",
    "uu", "uuid", "venv", "warnings", "wave", "weakref", "webbrowser",
    "winreg", "winsound", "wsgiref", "xdrlib", "xml", "xmlrpc",
    "zipapp", "zipfile", "zipimport", "zlib", "_abc", "_ast",
    "_bisect", "_codecs", "_collections", "_csv", "_datetime",
    "_decimal", "_elementtree", "_functools", "_heapq", "_io", "_json",
    "_locale", "_markupbase", "_operator", "_osx_support", "_pickle",
    "_py_abc", "_pydecimal", "_pyio", "_queue", "_random", "_signal",
    "_socket", "_sqlite3", "_sre", "_ssl", "_stat", "_string",
    "_strptime", "_struct", "_symtable", "_thread", "_threading_local",
    "_tracemalloc", "_uuid", "_warnings", "_weakref", "_weakrefset",
    "ntpath", "nt", "posixpath", "posix", "msvcrt", "msilib",
    "winreg", "winsound",
})

# Modules added in specific versions
_ADDED_IN: dict[str, set[str]] = {
    "3.11": {"tomllib"},
    "3.12": set(),
    "3.13": set(),
}

# Modules removed in specific versions
_REMOVED_IN: dict[str, set[str]] = {
    "3.12": {
        "asynchat", "asyncore", "distutils", "imp", "smtpd",
        "aifc", "audioop", "cgi", "cgitb", "chunk", "crypt",
        "imghdr", "mailcap", "msilib", "nis", "nntplib",
        "ossaudiodev", "pipes", "sndhdr", "spwd", "sunau",
        "telnetlib", "uu", "xdrlib",
    },
    "3.13": set(),
}


def get_stdlib_modules(python_version: str) -> frozenset[str]:
    """Get the set of stdlib module names for a given Python version.

    Args:
        python_version: Version string like "3.12" or "3.9".

    Returns:
        Frozenset of stdlib module names for that version.
    """
    major, minor = python_version.split(".")
    minor = int(minor)

    modules = set(_COMMON_STDLIB)

    # Add version-specific modules
    for ver_str, added in _ADDED_IN.items():
        ver_minor = int(ver_str.split(".")[1])
        if minor >= ver_minor:
            modules.update(added)

    # Remove version-specific modules
    for ver_str, removed in _REMOVED_IN.items():
        ver_minor = int(ver_str.split(".")[1])
        if minor >= ver_minor:
            modules -= removed

    return frozenset(modules)


def is_stdlib(module_name: str, python_version: str) -> bool:
    """Check if a module name is part of the standard library."""
    return module_name in get_stdlib_modules(python_version)

"""Runtime support copied into each bundle as _internal/_coil_runtime.py."""

import importlib.util
import os
import site
import sys


# Keep DLL search registrations alive for extensions imported later by the app.
_dll_directories = []


def initialize():
    # Supply normal site conveniences without discovering user/site packages
    # or executing sitecustomize during interpreter initialization.
    site.setquit()
    site.setcopyright()
    site.sethelper()
    here = os.path.dirname(os.path.abspath(__file__))
    lib = os.path.join(here, "lib")
    # pywin32's frozen loader searches sys.path for its versioned DLLs;
    # os.add_dll_directory alone only affects the Windows loader.
    pywin32_dlls = os.path.join(lib, "pywin32_system32")
    if os.path.isdir(pywin32_dlls) and pywin32_dlls not in sys.path:
        sys.path.append(pywin32_dlls)
    if hasattr(os, "add_dll_directory") and os.path.isdir(lib):
        seen = set()
        for directory, _, files in os.walk(lib):
            if any(name.lower().endswith((".dll", ".pyd")) for name in files):
                real = os.path.realpath(directory)
                if real not in seen:
                    seen.add(real)
                    _dll_directories.append(os.add_dll_directory(real))
    # Register DLL paths first: executable .pth lines may import extensions.
    if os.path.isdir(lib):
        site.addsitedir(lib)


def run(entry_point):
    initialize()
    here = os.path.dirname(os.path.abspath(__file__))
    # Preserve Coil's existing asset-relative working directory contract.
    os.chdir(os.path.dirname(here))
    import multiprocessing.spawn
    # CPython assumes a frozen Windows exe restores __main__ itself. Coil
    # stores normal script bytecode, so let spawn reload that file as it does
    # for python script.py; command-line generation still uses sys.frozen.
    multiprocessing.spawn.WINEXE = False
    if len(sys.argv) > 1 and sys.argv[1] == "--multiprocessing-fork":
        import multiprocessing
        multiprocessing.freeze_support()
    entry = os.path.join(here, "app", entry_point)
    if not os.path.isfile(entry):
        raise FileNotFoundError("Application entry point is missing: " + entry)
    spec = importlib.util.spec_from_file_location("__main__", entry)
    if spec is None or spec.loader is None:
        raise ImportError("Cannot load application entry point: " + entry)
    module = importlib.util.module_from_spec(spec)
    # A script has no module spec. Windows multiprocessing must reload its
    # __file__ as __mp_main__, rather than trying to import a module '__main__'.
    module.__spec__ = None
    sys.modules["__main__"] = module
    spec.loader.exec_module(module)

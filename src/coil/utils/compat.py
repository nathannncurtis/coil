"""Known package compatibility issues for Coil builds.

Maintains a list of packages that have known issues when bundled
into Coil executables. Used by `coil doctor` for pre-build warnings.
"""

# Map of package name -> warning message.
# These are packages that may need special handling or have known
# issues when bundled with embedded Python.
KNOWN_ISSUES: dict[str, str] = {
    "opencv-python": "may require additional DLLs (opencv_videoio_ffmpeg*.dll)",
    "opencv-contrib-python": "may require additional DLLs (opencv_videoio_ffmpeg*.dll)",
    "torch": "very large (~2 GB); may exceed portable exe limits",
    "tensorflow": "very large; requires specific DLL versions",
    "scipy": "includes large native extensions; ensure all .pyd files are included",
    "matplotlib": "requires font files and backends; test GUI rendering",
    "tkinter": "not included in embeddable Python; use --include if needed",
    "pygame": "requires SDL2 DLLs alongside the executable",
    "pyaudio": "requires portaudio DLLs alongside the executable",
    "cryptography": "requires OpenSSL DLLs (usually bundled automatically)",
}

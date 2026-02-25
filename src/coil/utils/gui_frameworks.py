"""Known GUI framework imports for auto-detection.

If any of these modules are imported anywhere in a project, Coil
defaults to GUI mode (console=false) to hide the console window.
"""

# Set of top-level import names that indicate a GUI application.
GUI_IMPORTS: set[str] = {
    # Tk
    "tkinter",
    "_tkinter",
    "customtkinter",
    # Qt
    "PyQt5",
    "PyQt6",
    "PySide2",
    "PySide6",
    # wx
    "wx",
    # Other GUI frameworks
    "kivy",
    "pygame",
    "pyglet",
    "dearpygui",
    "flet",
    # System tray
    "pystray",
    "infi",  # infi.systray imports as "infi"
    # Desktop notification / GUI-adjacent
    "plyer",
}

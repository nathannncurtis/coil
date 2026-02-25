"""Platform-specific build handlers."""

from coil.platforms.base import PlatformHandler
from coil.platforms.windows import WindowsHandler
from coil.platforms.macos import MacOSHandler
from coil.platforms.linux import LinuxHandler


def get_handler(target_os: str) -> PlatformHandler:
    """Get the platform handler for the target OS.

    Args:
        target_os: Target operating system ("windows", "macos", "linux").

    Returns:
        Platform handler instance.
    """
    handlers = {
        "windows": WindowsHandler,
        "macos": MacOSHandler,
        "linux": LinuxHandler,
    }

    handler_cls = handlers.get(target_os)
    if handler_cls is None:
        raise ValueError(f"Unsupported target OS: {target_os}")

    return handler_cls()

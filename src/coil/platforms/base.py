"""Abstract base class for platform-specific build handlers."""

from abc import ABC, abstractmethod
from pathlib import Path


class PlatformHandler(ABC):
    """Base class for platform-specific executable builders.

    Each platform implements how to:
    - Embed/bundle the Python runtime
    - Produce the final executable format
    - Handle GUI vs console mode
    - Deal with OS-specific quirks
    """

    @abstractmethod
    def create_launcher(
        self,
        output_dir: Path,
        entry_point: str,
        name: str,
        gui: bool = False,
        icon: str | None = None,
    ) -> Path:
        """Create a platform-specific launcher executable.

        Args:
            output_dir: Directory containing the runtime and app files.
            entry_point: Entry point script name (e.g. "main.pyc").
            name: Name for the output executable.
            gui: Whether to suppress the console window.
            icon: Path to icon file.

        Returns:
            Path to the created launcher executable.
        """
        ...

    @abstractmethod
    def get_runtime_arch(self) -> str:
        """Get the architecture string for the embeddable runtime."""
        ...

    @abstractmethod
    def get_executable_extension(self) -> str:
        """Get the file extension for executables on this platform."""
        ...

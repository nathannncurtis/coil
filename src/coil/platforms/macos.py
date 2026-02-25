"""macOS platform handler stub."""

from pathlib import Path

from coil.platforms.base import PlatformHandler


class MacOSHandler(PlatformHandler):
    """macOS .app builder (not yet implemented)."""

    def create_launcher(
        self,
        output_dir: Path,
        entry_point: str,
        name: str,
        gui: bool = False,
        icon: str | None = None,
    ) -> Path:
        raise NotImplementedError("macOS support coming soon")

    def get_runtime_arch(self) -> str:
        raise NotImplementedError("macOS support coming soon")

    def get_executable_extension(self) -> str:
        raise NotImplementedError("macOS support coming soon")

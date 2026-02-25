"""Linux platform handler stub."""

from pathlib import Path

from coil.platforms.base import PlatformHandler


class LinuxHandler(PlatformHandler):
    """Linux ELF builder (not yet implemented)."""

    def create_launcher(
        self,
        output_dir: Path,
        entry_point: str,
        name: str,
        gui: bool = False,
        icon: str | None = None,
    ) -> Path:
        raise NotImplementedError("Linux support coming soon")

    def get_runtime_arch(self) -> str:
        raise NotImplementedError("Linux support coming soon")

    def get_executable_extension(self) -> str:
        raise NotImplementedError("Linux support coming soon")

"""Build output and progress UI for Coil.

Provides cargo-style build output using the rich library.
Non-verbose: one line per step with spinner/progress bar.
Verbose: additional detail lines under each step.
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any, Callable, Optional

from rich.console import Console
from rich.progress import (
    BarColumn,
    DownloadColumn,
    Progress,
    SpinnerColumn,
    TaskProgressColumn,
    TextColumn,
    TransferSpeedColumn,
)
from rich.theme import Theme


COIL_THEME = Theme({
    "step": "bold cyan",
    "info": "dim",
    "success": "bold green",
    "warning": "bold yellow",
    "path": "dim cyan",
})


class BuildUI:
    """Manages build output and progress display.

    Args:
        verbose: Show detailed output under each step.
        console: Optional Console instance (for testing).
    """

    def __init__(
        self,
        verbose: bool = False,
        console: Optional[Console] = None,
    ) -> None:
        self.verbose = verbose
        self.console = console or Console(theme=COIL_THEME, highlight=False)
        self._start_time = time.monotonic()

    # --- Step-level output ---

    def step(self, message: str) -> None:
        """Print a build step header. Always visible."""
        self.console.print(f"  [step]{message}[/step]")

    def detail(self, message: str) -> None:
        """Print a detail line. Only visible in verbose mode."""
        if self.verbose:
            self.console.print(f"    [info]{message}[/info]")

    def success(self, message: str) -> None:
        """Print a success message. Always visible."""
        self.console.print(f"  [success]{message}[/success]")

    def warning(self, message: str) -> None:
        """Print a warning. Always visible."""
        self.console.print(f"  [warning]warning:[/warning] {message}")

    # --- Build header and summary ---

    def build_header(self, name: str, mode: str) -> None:
        """Print the build start header."""
        self.console.print(f"[step]Compiling[/step] {name} ({mode})")

    def build_summary(self, outputs: list[Path]) -> None:
        """Print the final build summary with paths, sizes, elapsed time."""
        elapsed = time.monotonic() - self._start_time
        self.console.print()
        self.console.print(f"[success]Finished[/success] in {elapsed:.1f}s")
        for output in outputs:
            if output.is_file():
                size = output.stat().st_size
                self.console.print(f"     [path]->[/path] {output} ({format_size(size)})")
            elif output.is_dir():
                total = sum(
                    f.stat().st_size for f in output.rglob("*") if f.is_file()
                )
                self.console.print(
                    f"     [path]->[/path] {output} ({format_size(total)})"
                )

    # --- Progress contexts ---

    def file_progress(self, description: str, total: int) -> Progress:
        """Create a progress bar for file-count operations (compile, zip).

        Usage:
            with ui.file_progress("Compiling", total=12) as progress:
                task = progress.add_task("", total=12)
                for f in files:
                    do_work(f)
                    progress.advance(task)
        """
        return Progress(
            SpinnerColumn(),
            TextColumn(f"  {description}"),
            BarColumn(bar_width=30),
            TaskProgressColumn(),
            TextColumn("{task.completed}/{task.total} files"),
            console=self.console,
            transient=not self.verbose,
        )

    def download_progress(self) -> Progress:
        """Create a progress bar for download operations (bytes)."""
        return Progress(
            SpinnerColumn(),
            TextColumn("  Downloading runtime"),
            BarColumn(bar_width=30),
            DownloadColumn(),
            TransferSpeedColumn(),
            console=self.console,
            transient=not self.verbose,
        )

    def spinner(self, description: str) -> Any:
        """Create a spinner for indeterminate operations."""
        from rich.status import Status

        return Status(
            f"  {description}",
            console=self.console,
            spinner="dots",
        )

    def make_download_hook(
        self, progress: Progress, task_id: Any
    ) -> Callable[[int, int, int], None]:
        """Create a urllib reporthook that updates a rich progress bar."""

        def reporthook(block_count: int, block_size: int, total_size: int) -> None:
            if block_count == 0 and total_size > 0:
                progress.update(task_id, total=total_size)
            downloaded = block_count * block_size
            if total_size > 0:
                progress.update(task_id, completed=min(downloaded, total_size))
            else:
                progress.update(task_id, completed=downloaded)

        return reporthook


def format_size(size_bytes: int) -> str:
    """Format bytes into human-readable string."""
    if size_bytes < 1024:
        return f"{size_bytes} B"
    elif size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.1f} KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"

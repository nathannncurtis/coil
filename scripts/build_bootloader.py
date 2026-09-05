"""Rebuild the portable x64 stub with the MSVC x64 Native Tools prompt.

Run `python scripts/build_bootloader.py --update` from that prompt. All compiler
outputs stay in build/bootloader; --update refreshes the checked-in base64 blob.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import re
import subprocess
import zlib
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    output = root / "build" / "bootloader"
    output.mkdir(parents=True, exist_ok=True)
    source = root / "src" / "coil" / "templates" / "bootloader.c"
    executable = output / "bootloader.exe"
    subprocess.run(
        [
            "cl", "/nologo", "/O1", "/MT", "/W4", "/GS", "/guard:cf",
            "/D_WIN32_WINNT=0x0A00", str(source),
            f"/Fo{output / 'bootloader.obj'}", f"/Fe{executable}",
            "/link", "/SUBSYSTEM:WINDOWS", "/DYNAMICBASE", "/NXCOMPAT",
            "/HIGHENTROPYVA", "/guard:cf", "/Brepro", "bcrypt.lib", "user32.lib",
        ], cwd=output, check=True,
    )
    data = executable.read_bytes()
    digest = hashlib.sha256(source.read_bytes().replace(b"\r\n", b"\n") +
                            source.with_name("tinfl.h").read_bytes().replace(b"\r\n", b"\n")).hexdigest()
    print(f"{executable}: {len(data)} bytes; source SHA-256 {digest}")
    if args.update:
        module = root / "src" / "coil" / "bootloader.py"
        text = module.read_text(encoding="utf-8")
        encoded = base64.b64encode(zlib.compress(data, 9)).decode("ascii")
        block = f'# Source SHA-256: {digest}\n_STUBS["x86_64"] = ({len(data)}, zlib.decompress(base64.b64decode(\n'
        block += "".join(f'    b"{encoded[i:i + 88]}"\n' for i in range(0, len(encoded), 88))
        block += ")))"
        text = re.sub(r'_STUBS\["x86_64"\] = .*?\n\)\)\)?', lambda _: block, text, count=1, flags=re.S)
        text = re.sub(r"BOOTLOADER_VERSION = \d+", "BOOTLOADER_VERSION = 4", text)
        text = re.sub(r"# Source SHA-256: [a-f0-9]+\n(?=# Source SHA-256:)", "", text)
        module.write_text(text, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()

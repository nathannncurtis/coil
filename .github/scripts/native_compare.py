import hashlib
import re
import sys
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()


templates = Path("src/coil/templates")
ok = True

stubs = Path("src/coil/_launcher_stubs.py").read_text(encoding="utf-8")
for name, source in (("SOURCE_SHA256", "launcher.c"), ("MANIFEST_SHA256", "launcher.manifest")):
    recorded = re.search(rf'^{name} = "([0-9a-f]+)"', stubs, re.M).group(1)
    current = digest(templates / source)
    print(f"{name}: recorded {recorded}, {source} is {current}")
    ok &= recorded == current

module = Path("src/coil/bootloader.py").read_text(encoding="utf-8")
recorded = re.findall(r"^# Source SHA-256: ([0-9a-f]+)$", module, re.M)[-1]
current = hashlib.sha256((templates / "bootloader.c").read_bytes().replace(b"\r\n", b"\n") +
                         (templates / "tinfl.h").read_bytes().replace(b"\r\n", b"\n")).hexdigest()
print(f"bootloader: recorded {recorded}, source is {current}")
ok &= recorded == current

sys.exit(0 if ok else 1)

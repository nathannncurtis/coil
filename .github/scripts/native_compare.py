import ast
import base64
import hashlib
import re
import sys
import zlib
from pathlib import Path

ok = True

built = Path("build/native/_launcher_stubs.py").read_text(encoding="utf-8")
checked = Path("src/coil/_launcher_stubs.py").read_text(encoding="utf-8")
same = built == checked
ok &= same
print(f"launcher stubs identical to checked-in: {same}")
if not same:
    for label, text in (("built", built), ("checked-in", checked)):
        for line in text.splitlines():
            if line.startswith("# CPython"):
                print(f"  {label}: {line}")

exe = Path("build/bootloader/bootloader.exe").read_bytes()
module = Path("src/coil/bootloader.py").read_text(encoding="utf-8")
block = re.search(r'_STUBS\["x86_64"\] = \((\d+), zlib\.decompress\(base64\.b64decode\(\n(.*?)\n\)\)\)', module, re.S)
encoded = b"".join(ast.literal_eval(line) for line in block.group(2).splitlines())
stub = zlib.decompress(base64.b64decode(encoded))
same = exe == stub
ok &= same
print(f"bootloader identical to checked-in: {same} (built {len(exe)} bytes {hashlib.sha256(exe).hexdigest()}, checked-in {len(stub)} bytes {hashlib.sha256(stub).hexdigest()})")
sys.exit(0 if ok else 1)

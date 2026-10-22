import os

import psutil

process = psutil.Process()
assert process.pid == os.getpid()
assert process.memory_info().rss > 0
assert process.name().lower().endswith(".exe")
print("COMPAT_OK psutil", process.name())

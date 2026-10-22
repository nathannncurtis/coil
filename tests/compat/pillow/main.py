import os
import tempfile

from PIL import Image

image = Image.new("RGB", (8, 4), (10, 20, 30))
image.putpixel((1, 2), (200, 100, 50))
path = os.path.join(tempfile.mkdtemp(), "probe.png")
image.save(path)
with Image.open(path) as again:
    assert again.size == (8, 4), again.size
    assert again.getpixel((1, 2)) == (200, 100, 50)
print("COMPAT_OK pillow", Image.__version__)

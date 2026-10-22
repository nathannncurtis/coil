from pathlib import Path

import pdfplumber

sample = Path(__file__).resolve().parents[2] / "sample.pdf"
with pdfplumber.open(sample) as pdf:
    text = pdf.pages[0].extract_text()
    assert "Coil compat sample" in text, text
    image = pdf.pages[0].to_image(resolution=50)
    assert image.original.size[0] > 0
print("COMPAT_OK pdfplumber")

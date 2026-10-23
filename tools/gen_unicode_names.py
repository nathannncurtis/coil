#!/usr/bin/env python3.12
"""Generate src/parser/unicode_names.rs from CPython's unicodename_db.h.

The parser needs CPython's exact ``\\N{name}`` table (spec section 4): every
character name plus the formal aliases, looked up case-insensitively, with the
Hangul-syllable and CJK-unified-ideograph names generated algorithmically at
runtime (like ``Modules/unicodedata.c``). This script decodes the generated
header's phrasebook -- the same tables ``_getucname`` walks -- and emits the
sorted table the Rust lookup binary-searches.

Usage: gen_unicode_names.py [<path to unicodename_db.h>]  (default: the
extracted CPython 3.12.10 sources under ~/.coil/cache, or any argument of
fetch_cpython_src.py --print-root plus Modules/unicodename_db.h).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def parse_arrays(header: str) -> dict[str, list[int]]:
    arrays: dict[str, list[int]] = {}
    pattern = re.compile(r"static const (?:unsigned \w+|named_sequence) (\w+)\[\] = \{(.*?)\};", re.S)
    for name, body in pattern.findall(header):
        numbers: list[int] = []
        for token in body.split(","):
            token = token.strip()
            if not token or token.startswith("/*") or token.startswith("//"):
                continue
            if token.startswith("{") or token.startswith("}"):
                token = token.strip("{} \n")
                if not token:
                    continue
            match = re.match(r"^-?(0[xX][0-9a-fA-F]+|\d+)$", token)
            if match:
                text = match.group(1)
                numbers.append(int(text, 0))
        arrays[name] = numbers
    return arrays


def main() -> int:
    if len(sys.argv) > 1:
        header = Path(sys.argv[1]).read_text()
    else:
        member = "Python-3.12.10/Modules/unicodename_db.h"
        tarball = Path.home() / ".coil/cache/stdlib-src/Python-3.12.10.tar.xz"
        if not tarball.exists():
            print(f"error: {tarball} not found; fetch CPython first", file=sys.stderr)
            return 2
        import tarfile

        with tarfile.open(tarball, "r:xz") as archive:
            header = archive.extractfile(member).read().decode()
    arrays = parse_arrays(header)
    lexicon = arrays["lexicon"]
    lexicon_offset = arrays["lexicon_offset"]
    phrasebook = arrays["phrasebook"]
    phrasebook_offset1 = arrays["phrasebook_offset1"]
    phrasebook_offset2 = arrays["phrasebook_offset2"]
    shift = int(re.search(r"#define phrasebook_shift (\d+)", header).group(1))
    short = int(re.search(r"#define phrasebook_short (\d+)", header).group(1))
    aliases_start = int(re.search(r"aliases_start = (0x[0-9a-f]+);", header).group(1), 16)
    aliases_end = int(re.search(r"aliases_end = (0x[0-9a-f]+);", header).group(1), 16)

    def name_of(code: int) -> str | None:
        """_getucname without the Hangul/CJK special cases (handled in Rust)."""
        offset = phrasebook_offset1[code >> shift]
        offset = phrasebook_offset2[(offset << shift) + (code & ((1 << shift) - 1))]
        if not offset:
            return None
        parts: list[str] = []
        index = 0
        while True:
            word = phrasebook[offset] - short
            if word >= 0:
                word = (word << 8) + phrasebook[offset + 1]
                offset += 2
            else:
                word = phrasebook[offset]
                offset += 1
            w = lexicon_offset[word]
            chars: list[str] = []
            while lexicon[w] < 128:
                chars.append(chr(lexicon[w]))
                w += 1
            # A byte >= 0x80 terminates the word; 0x80 itself is the end-of-name
            # sentinel (its NUL), anything else is a real final character.
            if lexicon[w] != 128:
                chars.append(chr(lexicon[w] & 127))
            parts.append("".join(chars))
            if lexicon[w] == 128:
                break
            index += 1
        return " ".join(parts)

    entries: dict[str, int] = {}
    for code in range(0x110000):
        name = name_of(code)
        if name is None or not name:
            continue
        if aliases_start <= code < aliases_end:
            name = "ALIAS:" + name  # temporary marker, resolved below
            entries[name] = code
        else:
            entries[name.upper()] = code

    # Formal aliases map to their target character (name_aliases).
    name_aliases = arrays["name_aliases"]
    final: dict[str, int] = {}
    for name, code in entries.items():
        if name.startswith("ALIAS:"):
            target = name_aliases[code - aliases_start]
            final[name[6:].upper()] = target
        else:
            final[name] = code

    ordered = sorted(final.items())
    blob = "\n".join(name for name, _ in ordered) + "\n"
    offsets: list[int] = []
    position = 0
    for name, _ in ordered:
        offsets.append(position)
        position += len(name) + 1
    codepoints = [cp for _, cp in ordered]

    out_path = Path(__file__).resolve().parent.parent / "native/crates/coil-front/src/parser/unicode_names.rs"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w") as out:
        out.write("//! Generated by tools/gen_unicode_names.py from CPython 3.12.10's\n")
        out.write("//! Modules/unicodename_db.h. Do not edit: rerun the generator.\n")
        out.write("//! One name per line in NAMES (uppercase, sorted); CODEPOINTS is parallel.\n")
        out.write("//! Formal aliases are included, named sequences are not (CPython's\n")
        out.write("//! \\\\N{} lookup passes with_named_seq=0).\n\n")
        out.write("pub static NAMES: &str = \"")
        out.write("".join("\\n" if ch == "\n" else ch for ch in blob))
        out.write("\";\n\n")
        out.write("pub static OFFSETS: [u32; %d] = [" % len(offsets))
        for i in range(0, len(offsets), 16):
            out.write("\n    " + ",".join(str(v) for v in offsets[i:i + 16]) + ",")
        out.write("\n];\n\n")
        out.write("pub static CODEPOINTS: [u32; %d] = [" % len(codepoints))
        for i in range(0, len(codepoints), 16):
            out.write("\n    " + ",".join(hex(v) for v in codepoints[i:i + 16]) + ",")
        out.write("\n];\n")
    print(f"{len(ordered)} names, blob {len(blob)} bytes -> {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

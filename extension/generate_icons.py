#!/usr/bin/env python3
"""Rasterize Sidekar brand PNGs into extension toolbar sizes (requires Pillow).

Source: ../www/public/sidekar-icon-light-512.png (default; Chrome UI is light) or dark variant.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
WWW = REPO / "www" / "public"
ICONS = HERE / "icons"

SOURCES = {
    "dark": WWW / "sidekar-icon-dark-512.png",
    "light": WWW / "sidekar-icon-light-512.png",
}


def main() -> None:
    variant = os.environ.get("SIDEKAR_EXT_ICON", "light").lower()
    if variant not in SOURCES:
        print(f"Unknown SIDEKAR_EXT_ICON={variant!r}, use dark or light", file=sys.stderr)
        sys.exit(1)
    src = SOURCES[variant]
    if not src.is_file():
        print(f"Missing source icon: {src}", file=sys.stderr)
        sys.exit(1)

    ICONS.mkdir(parents=True, exist_ok=True)
    im = Image.open(src).convert("RGBA")
    for s in (16, 48, 128):
        out = ICONS / f"icon-{s}.png"
        im.resize((s, s), Image.Resampling.LANCZOS).save(out, "PNG")
        print(out)


if __name__ == "__main__":
    main()

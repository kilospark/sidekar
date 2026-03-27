#!/usr/bin/env python3
"""Rasterize Sidekar SVG icons into extension toolbar PNGs.

Requires rsvg-convert (brew install librsvg) or Pillow as fallback from 512px PNGs.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
WWW = REPO / "www" / "public"
ICONS = HERE / "icons"

SOURCES_SVG = {
    "dark": WWW / "sidekar-icon-dark.svg",
    "light": WWW / "sidekar-icon-light.svg",
}
SOURCES_PNG = {
    "dark": WWW / "sidekar-icon-dark-512.png",
    "light": WWW / "sidekar-icon-light-512.png",
}

SIZES = (16, 48, 128)


def has_rsvg() -> bool:
    try:
        subprocess.run(["rsvg-convert", "--version"], capture_output=True, check=True)
        return True
    except (FileNotFoundError, subprocess.CalledProcessError):
        return False


def rasterize_svg(svg_path: Path, variant: str) -> None:
    ICONS.mkdir(parents=True, exist_ok=True)
    for s in SIZES:
        out = ICONS / f"icon-{variant}-{s}.png"
        subprocess.run(
            ["rsvg-convert", "-w", str(s), "-h", str(s), "-o", str(out), str(svg_path)],
            check=True,
        )
        print(out)


def rasterize_png(png_path: Path, variant: str) -> None:
    from PIL import Image

    ICONS.mkdir(parents=True, exist_ok=True)
    im = Image.open(png_path).convert("RGBA")
    for s in SIZES:
        out = ICONS / f"icon-{variant}-{s}.png"
        im.resize((s, s), Image.Resampling.LANCZOS).save(out, "PNG")
        print(out)


def main() -> None:
    use_rsvg = has_rsvg()

    for variant in ("dark", "light"):
        svg = SOURCES_SVG[variant]
        png = SOURCES_PNG[variant]

        if svg.is_file() and use_rsvg:
            print(f"Rasterizing {svg.name} via rsvg-convert")
            rasterize_svg(svg, variant)
        elif png.is_file():
            print(f"Resizing {png.name} via Pillow (install librsvg for SVG source)")
            rasterize_png(png, variant)
        else:
            print(f"No source icon found for {variant!r}", file=sys.stderr)
            sys.exit(1)


if __name__ == "__main__":
    main()

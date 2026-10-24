#!/usr/bin/env python3
"""Renders the MSIX visual assets in packaging/windows/msix/Assets from the RustPy logo.

Source: packaging/linux/rustpy.svg rendered by ImageMagick (`magick`) when it is installed,
otherwise the 256 px image of packaging/windows/rustpy.ico. Needs Pillow. The outputs are
committed, so the package build (Linux or the Windows CI runner) does not need either tool;
run this again after the logo changes:

    python3 packaging/windows/msix/generate-assets.py

Only unqualified file names are written (no .scale-200 / .targetsize-* variants): those
need a resources.pri built by makepri.exe, which the Linux packer cannot run. The square
logo is drawn on a transparent background, so tiles use the manifest's BackgroundColor.
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SVG = ROOT / "packaging/linux/rustpy.svg"
ICO = ROOT / "packaging/windows/rustpy.ico"
OUT = HERE / "Assets"

# name -> (width, height, logo share of the shorter side)
ASSETS = {
    "StoreLogo.png": (50, 50, 1.0),
    "Square44x44Logo.png": (44, 44, 1.0),
    "Square71x71Logo.png": (71, 71, 0.8),
    "Square150x150Logo.png": (150, 150, 0.66),
    "Square310x310Logo.png": (310, 310, 0.66),
    "Wide310x150Logo.png": (310, 150, 0.66),
    "SplashScreen.png": (620, 300, 0.66),
}


def master() -> Image.Image:
    magick = shutil.which("magick")
    if magick and SVG.exists():
        with tempfile.TemporaryDirectory() as tmp:
            png = Path(tmp) / "logo.png"
            subprocess.run(
                [magick, "-background", "none", "-density", "1536", str(SVG),
                 "-resize", "1024x1024", "-depth", "8", str(png)],
                check=True,
            )
            return Image.open(png).convert("RGBA").copy()
    ico = Image.open(ICO)
    ico.size = max(ico.ico.sizes())
    return ico.convert("RGBA")


def main() -> int:
    logo = master()
    OUT.mkdir(parents=True, exist_ok=True)
    for name, (w, h, share) in ASSETS.items():
        side = max(1, round(min(w, h) * share))
        icon = logo.resize((side, side), Image.LANCZOS)
        canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))
        canvas.paste(icon, ((w - side) // 2, (h - side) // 2), icon)
        canvas.save(OUT / name, optimize=True)
        print(f"{name}: {w}x{h}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

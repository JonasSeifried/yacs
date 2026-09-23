"""Draws the YACS icons with no dependencies beyond the standard library.

    python3 apps/desktop/icon/generate.py
    pnpm --filter @yacs/desktop tauri icon icon/icon.png -o src-tauri/icons

Writes icon.png (1024², app icon source) next to this file and the macOS menu
bar template icon to src-tauri/icons/tray-template.png. Shapes are signed
distance fields, so edges are anti-aliased analytically.
"""

import math
import struct
import zlib
from pathlib import Path

HERE = Path(__file__).parent


def rounded_rect(px, py, cx, cy, hw, hh, r):
    qx, qy = abs(px - cx) - (hw - r), abs(py - cy) - (hh - r)
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    return outside + min(max(qx, qy), 0.0) - r


def coverage(d):
    return min(max(0.5 - d, 0.0), 1.0)


def clipboard(px, py, s, ox=0.0, oy=0.0):
    """Coverage of the clipboard glyph (board + clip, minus text lines) at size s."""
    x, y = (px - ox) / s, (py - oy) / s
    board = rounded_rect(x, y, 0.5, 0.56, 0.205, 0.245, 0.04) * s
    clip = rounded_rect(x, y, 0.5, 0.315, 0.095, 0.045, 0.022) * s
    notch = rounded_rect(x, y, 0.5, 0.300, 0.038, 0.013, 0.013) * s
    lines = min(
        rounded_rect(x, y, 0.5, 0.49, 0.125, 0.016, 0.016),
        rounded_rect(x, y, 0.5, 0.565, 0.125, 0.016, 0.016),
        rounded_rect(x, y, 0.455, 0.64, 0.08, 0.016, 0.016),
    ) * s
    shape = max(min(board, clip), -notch, -lines)
    return coverage(shape)


def write_png(path, size, pixel):
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        for x in range(size):
            rows.extend(pixel(x + 0.5, y + 0.5))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b""))


def app_icon(x, y, size=1024):
    # macOS-style squircle-ish tile with a diagonal indigo → violet gradient.
    tile = coverage(rounded_rect(x, y, size / 2, size / 2, size * 0.40, size * 0.40, size * 0.18))
    t = (x + y) / (2 * size)
    top, bottom = (79, 70, 229), (124, 58, 237)
    bg = [a + (b - a) * t for a, b in zip(top, bottom)]
    glyph = clipboard(x, y, size)
    rgb = [c + (255 - c) * glyph for c in bg]
    return bytes([round(c) for c in rgb] + [round(255 * tile)])


def web_icon(x, y, size=512):
    # Full-bleed and opaque: Android masks it to its own shape (the glyph stays
    # inside the 80% safe zone) and iOS would show transparency as black.
    t = (x + y) / (2 * size)
    top, bottom = (79, 70, 229), (124, 58, 237)
    bg = [a + (b - a) * t for a, b in zip(top, bottom)]
    glyph = clipboard(x, y, size)
    rgb = [c + (255 - c) * glyph for c in bg]
    return bytes([round(c) for c in rgb] + [255])


def tray_icon(x, y, size=44):
    # Template images: only alpha matters, macOS tints them for light/dark menu bars.
    glyph = clipboard(x, y, size * 1.25, ox=-size * 0.125, oy=-size * 0.16)
    return bytes([0, 0, 0, round(255 * glyph)])


if __name__ == "__main__":
    write_png(HERE / "icon.png", 1024, app_icon)
    write_png(HERE.parent / "src-tauri/icons/tray-template.png", 44, tray_icon)
    web = HERE.parents[2] / "ui/public/icons"
    write_png(web / "icon-512.png", 512, web_icon)
    print("wrote icon.png, src-tauri/icons/tray-template.png and ui/public/icons/icon-512.png")

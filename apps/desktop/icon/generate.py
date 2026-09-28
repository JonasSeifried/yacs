"""Draws the YACS icons with no dependencies beyond the standard library.

    python3 apps/desktop/icon/generate.py
    pnpm --filter @yacs/desktop tauri icon icon/icon.png -o src-tauri/icons

Writes icon.png (1024², app icon source) next to this file, the macOS menu
bar template icon to src-tauri/icons/tray-template.png and the web app's icons
to ui/public/icons/. Shapes are signed
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


def smooth_max(a, b, k):
    # Like max(), but blends within k of the seam, which rounds the corners an intersection leaves.
    h = max(k - abs(a - b), 0.0) / k
    return max(a, b) + h * h * k / 4


def coverage(d):
    return min(max(0.5 - d, 0.0), 1.0)


def copy_across(px, py, s, ox=0.0, oy=0.0):
    """Coverage of the copy-across glyph (a card stacked on another) in an s-sized box at (ox, oy).

    The front card is cut out of the back one with a gap around it, so the two
    stay apart even at menu bar size.
    """
    x, y = (px - ox) / s, (py - oy) / s
    back = rounded_rect(x, y, 0.346, 0.346, 0.346, 0.346, 0.154) * s
    gap = rounded_rect(x, y, 0.712, 0.712, 0.365, 0.365, 0.192) * s
    front = rounded_rect(x, y, 0.712, 0.712, 0.288, 0.288, 0.135) * s
    shape = min(smooth_max(back, -gap, 0.08 * s), front)
    return coverage(shape)


def glyph(x, y, size):
    # Centred, about 47% of the canvas: inside the app tile and Android's 80% safe zone.
    g = size * 0.47
    return copy_across(x, y, g, ox=(size - g) / 2, oy=(size - g) / 2)


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


CORAL = (226, 86, 111)


def app_icon(x, y, size=1024):
    # macOS-style squircle-ish tile, flat coral.
    tile = coverage(rounded_rect(x, y, size / 2, size / 2, size * 0.40, size * 0.40, size * 0.18))
    g = glyph(x, y, size)
    rgb = [c + (255 - c) * g for c in CORAL]
    return bytes([round(c) for c in rgb] + [round(255 * tile)])


def web_icon(x, y, size):
    # Full-bleed and opaque: Android masks it to its own shape and iOS would
    # show transparency as black.
    g = glyph(x, y, size)
    rgb = [c + (255 - c) * g for c in CORAL]
    return bytes([round(c) for c in rgb] + [255])


def tray_icon(x, y, size=44):
    # Template images: only alpha matters, macOS tints them for light/dark menu bars.
    g = size * 0.8
    return bytes([0, 0, 0, round(255 * copy_across(x, y, g, ox=(size - g) / 2, oy=(size - g) / 2))])


if __name__ == "__main__":
    write_png(HERE / "icon.png", 1024, app_icon)
    write_png(HERE.parent / "src-tauri/icons/tray-template.png", 44, tray_icon)
    web = HERE.parents[2] / "ui/public/icons"
    for name, size in (("icon-512.png", 512), ("icon-192.png", 192), ("apple-touch-icon.png", 180)):
        write_png(web / name, size, lambda x, y, size=size: web_icon(x, y, size))
    print("wrote icon.png, src-tauri/icons/tray-template.png and ui/public/icons/{icon-512,icon-192,apple-touch-icon}.png")

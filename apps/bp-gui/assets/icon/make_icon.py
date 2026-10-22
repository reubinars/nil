"""Draws the nil app icon: the crossed-loops clover, white on a dark rounded square.

Writes nil.ico (16-256 px, each size drawn on its own) and nil-256.png next to this
file. Run it again only when the mark changes: python3 make_icon.py
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).parent
TILE = (0x16, 0x16, 0x16, 255)
BORDER = (0x2A, 0x2A, 0x2A, 255)
MARK = (0xF2, 0xF2, 0xF2, 255)
SUPERSAMPLE = 8

# One loop of the figure-eight, as cubic Béziers in a 100-unit box (same path as the logo).
LOOP = [
    ((50, 50), (60, 34), (90, 30), (90, 50)),
    ((90, 50), (90, 70), (60, 66), (50, 50)),
    ((50, 50), (40, 34), (10, 30), (10, 50)),
    ((10, 50), (10, 70), (40, 66), (50, 50)),
]


def bezier(p0, p1, p2, p3, steps=64):
    for i in range(steps + 1):
        t = i / steps
        u = 1 - t
        yield tuple(
            u**3 * a + 3 * u * u * t * b + 3 * u * t * t * c + t**3 * d
            for a, b, c, d in zip(p0, p1, p2, p3)
        )


def rotated(points, degrees):
    a = math.radians(degrees)
    ca, sa = math.cos(a), math.sin(a)
    for x, y in points:
        x, y = x - 50, y - 50
        yield (50 + x * ca - y * sa, 50 + x * sa + y * ca)


def figure_eight():
    pts = []
    for seg in LOOP:
        pts.extend(bezier(*seg))
    return pts


def draw(size, mark_scale, stroke_units):
    """One icon of `size` px: the tile, and the mark at `mark_scale` of the tile with a
    stroke of `stroke_units` (in the mark's 100-unit box)."""
    big = size * SUPERSAMPLE
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    radius = round(big * 0.22)
    d.rounded_rectangle((0, 0, big - 1, big - 1), radius, fill=TILE)
    if size >= 48:
        d.rounded_rectangle(
            (0, 0, big - 1, big - 1), radius, outline=BORDER, width=SUPERSAMPLE
        )
    side = big * mark_scale
    origin = (big - side) / 2
    unit = side / 100
    width = max(1, round(stroke_units * unit))
    base = figure_eight()
    for turn in (45, 135):
        pts = [(origin + x * unit, origin + y * unit) for x, y in rotated(base, turn)]
        d.line(pts, fill=MARK, width=width, joint="curve")
    return img.resize((size, size), Image.LANCZOS)


# Small sizes get a bigger mark and a heavier stroke so the loops stay legible.
SIZES = {
    16: (0.86, 11),
    24: (0.82, 10),
    32: (0.78, 9),
    48: (0.74, 8),
    64: (0.72, 7),
    256: (0.68, 6),
}


def main():
    icons = {s: draw(s, *p) for s, p in SIZES.items()}
    largest = icons[256]
    largest.save(HERE / "nil-256.png")
    largest.save(
        HERE / "nil.ico",
        sizes=[(s, s) for s in SIZES],
        append_images=[icons[s] for s in SIZES if s != 256],
    )


if __name__ == "__main__":
    main()

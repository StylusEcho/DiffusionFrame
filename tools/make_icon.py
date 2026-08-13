#!/usr/bin/env python3
"""Regenerate the app icon from the upstream Material Symbols glyph.

Source: the "iframe" icon from https://fonts.google.com/icons (Material
Symbols Outlined), fetched verbatim and rasterized here, so the committed
assets are reproducible rather than hand-traced.

    python3 tools/make_icon.py

Writes assets/icon.ico (the Windows executable icon) and assets/icon-32.rgba
(raw RGBA for the tao window icon, so the app needs no PNG decoder at run
time). Uses only the standard library.
"""

import math
import re
import struct
import urllib.request
import zlib
from pathlib import Path

SVG_URL = (
    "https://fonts.gstatic.com/s/i/short-term/release/"
    "materialsymbolsoutlined/iframe/default/24px.svg"
)

# The app's own surface and text colours, so the icon matches the frame.
TILE = (0x16, 0x18, 0x1D)
GLYPH = (0xD8, 0xDB, 0xE2)

ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
SUPERSAMPLE = 4
ASSETS = Path(__file__).resolve().parent.parent / "assets"


# --- SVG path parsing -------------------------------------------------------

NUMBER = re.compile(r"[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?")
COMMAND = re.compile(r"[MmLlHhVvQqTtZz]")


def parse_path(d):
    """Flatten a path into a list of closed subpaths of (x, y) points."""
    tokens = []
    index = 0
    while index < len(d):
        char = d[index]
        if COMMAND.match(char):
            tokens.append(char)
            index += 1
        elif char in " ,\t\r\n":
            index += 1
        else:
            match = NUMBER.match(d, index)
            if not match:
                raise ValueError(f"unparsable path at offset {index}: {d[index:index+12]!r}")
            tokens.append(float(match.group()))
            index = match.end()

    subpaths = []
    current = []
    x = y = 0.0
    start_x = start_y = 0.0
    # Reflection point for smooth quadratics (T), per the SVG spec.
    last_control = None
    command = None
    position = 0

    def take(count):
        nonlocal position
        values = tokens[position:position + count]
        if len(values) != count or any(isinstance(v, str) for v in values):
            raise ValueError(f"{command} wants {count} numbers, got {values!r}")
        position += count
        return values

    def quadratic(cx, cy, nx, ny):
        # Flatten finely; the glyph is rendered supersampled anyway.
        steps = 24
        for step in range(1, steps + 1):
            t = step / steps
            u = 1 - t
            current.append((
                u * u * x + 2 * u * t * cx + t * t * nx,
                u * u * y + 2 * u * t * cy + t * t * ny,
            ))

    while position < len(tokens):
        token = tokens[position]
        if isinstance(token, str):
            command = token
            position += 1
        elif command in ("M", "m"):
            # A repeated coordinate pair after M is an implicit lineto.
            command = "L" if command == "M" else "l"
            continue

        if command in ("M", "m"):
            if current:
                subpaths.append(current)
            dx, dy = take(2)
            x, y = (dx, dy) if command == "M" else (x + dx, y + dy)
            start_x, start_y = x, y
            current = [(x, y)]
            last_control = None
        elif command in ("L", "l"):
            dx, dy = take(2)
            x, y = (dx, dy) if command == "L" else (x + dx, y + dy)
            current.append((x, y))
            last_control = None
        elif command in ("H", "h"):
            (dx,) = take(1)
            x = dx if command == "H" else x + dx
            current.append((x, y))
            last_control = None
        elif command in ("V", "v"):
            (dy,) = take(1)
            y = dy if command == "V" else y + dy
            current.append((x, y))
            last_control = None
        elif command in ("Q", "q"):
            cx, cy, nx, ny = take(4)
            if command == "q":
                cx, cy, nx, ny = x + cx, y + cy, x + nx, y + ny
            quadratic(cx, cy, nx, ny)
            last_control = (cx, cy)
            x, y = nx, ny
        elif command in ("T", "t"):
            nx, ny = take(2)
            if command == "t":
                nx, ny = x + nx, y + ny
            cx, cy = (2 * x - last_control[0], 2 * y - last_control[1]) if last_control else (x, y)
            quadratic(cx, cy, nx, ny)
            last_control = (cx, cy)
            x, y = nx, ny
        elif command in ("Z", "z"):
            if current:
                current.append((start_x, start_y))
                subpaths.append(current)
            current = []
            x, y = start_x, start_y
            last_control = None
        else:
            raise ValueError(f"unsupported path command {command!r}")

    if current:
        subpaths.append(current)
    return subpaths


# --- Rasterizing ------------------------------------------------------------

def coverage(subpaths, size, scale, offset):
    """Nonzero-winding scanline fill, supersampled, returning 0..1 coverage."""
    high = size * SUPERSAMPLE
    edges = []
    for points in subpaths:
        transformed = [(px * scale + offset, py * scale + offset) for px, py in points]
        for (x0, y0), (x1, y1) in zip(transformed, transformed[1:]):
            if y0 != y1:
                edges.append((x0, y0, x1, y1))

    counts = [0.0] * (size * size)
    for row in range(high):
        sample_y = (row + 0.5) / SUPERSAMPLE
        # Winding-signed crossings, sorted; nonzero rule spans where depth != 0.
        crossings = []
        for x0, y0, x1, y1 in edges:
            if (y0 <= sample_y < y1) or (y1 <= sample_y < y0):
                t = (sample_y - y0) / (y1 - y0)
                crossings.append((x0 + t * (x1 - x0), 1 if y1 > y0 else -1))
        if not crossings:
            continue
        crossings.sort()

        depth = 0
        span_start = 0.0
        target_row = row // SUPERSAMPLE
        for cross_x, winding in crossings:
            if depth == 0:
                span_start = cross_x
            depth += winding
            if depth == 0:
                # Accumulate this covered span into the output pixels.
                left = max(0.0, span_start * SUPERSAMPLE)
                right = min(float(high), cross_x * SUPERSAMPLE)
                if right <= left:
                    continue
                for column in range(int(left), min(int(math.ceil(right)), high)):
                    covered = min(right, column + 1.0) - max(left, float(column))
                    if covered > 0:
                        counts[target_row * size + column // SUPERSAMPLE] += covered

    limit = float(SUPERSAMPLE * SUPERSAMPLE)
    return [min(1.0, value / limit) for value in counts]


def rounded_tile(size):
    """Squircle-ish tile matching the Windows app-icon convention."""
    radius = size * 0.22
    alpha = [0.0] * (size * size)
    for row in range(size):
        for column in range(size):
            total = 0.0
            for sub_row in range(SUPERSAMPLE):
                for sub_column in range(SUPERSAMPLE):
                    px = column + (sub_column + 0.5) / SUPERSAMPLE
                    py = row + (sub_row + 0.5) / SUPERSAMPLE
                    dx = max(radius - px, px - (size - radius), 0.0)
                    dy = max(radius - py, py - (size - radius), 0.0)
                    if dx * dx + dy * dy <= radius * radius:
                        total += 1.0
            alpha[row * size + column] = total / (SUPERSAMPLE * SUPERSAMPLE)
    return alpha


def render(subpaths, view, size):
    """Composite the glyph over the tile, returning RGBA bytes."""
    # Inset the glyph so it does not crowd the tile's edges.
    inset = size * 0.18
    scale = (size - 2 * inset) / view
    glyph = coverage(subpaths, size, scale, inset)
    tile = rounded_tile(size)

    out = bytearray()
    for index in range(size * size):
        tile_alpha = tile[index]
        glyph_alpha = glyph[index] * tile_alpha
        if tile_alpha == 0:
            out += bytes(4)
            continue
        # Glyph over tile, both premultiplied by their own coverage.
        pixel = [
            round(GLYPH[channel] * glyph_alpha + TILE[channel] * (tile_alpha - glyph_alpha))
            for channel in range(3)
        ]
        # Un-premultiply against the tile's alpha for straight-alpha output.
        out += bytes([min(255, round(value / tile_alpha)) for value in pixel])
        out.append(round(tile_alpha * 255))
    return bytes(out)


# --- Encoding ---------------------------------------------------------------

def png(rgba, size):
    raw = b"".join(
        b"\x00" + rgba[row * size * 4:(row + 1) * size * 4] for row in range(size)
    )

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    """ICO with PNG-compressed entries, supported by Windows Vista onwards."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, payloads = b"", b""
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII", size if size < 256 else 0, size if size < 256 else 0,
            0, 0, 1, 32, len(data), offset,
        )
        payloads += data
        offset += len(data)
    return header + entries + payloads


def main():
    ASSETS.mkdir(exist_ok=True)
    svg = urllib.request.urlopen(SVG_URL, timeout=30).read().decode()

    view_box = re.search(r'viewBox="([^"]+)"', svg).group(1).split()
    min_x, min_y, view_w, view_h = (float(value) for value in view_box)
    path = re.search(r'\sd="([^"]+)"', svg).group(1)

    subpaths = [
        [(px - min_x, py - min_y) for px, py in points]
        for points in parse_path(path)
    ]
    view = max(view_w, view_h)

    images = [(size, png(render(subpaths, view, size), size)) for size in ICO_SIZES]
    (ASSETS / "icon.ico").write_bytes(ico(images))
    (ASSETS / "icon-32.rgba").write_bytes(render(subpaths, view, 32))
    print(f"wrote {ASSETS/'icon.ico'} ({len(ico(images))} bytes) and icon-32.rgba")


if __name__ == "__main__":
    main()

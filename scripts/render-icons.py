#!/usr/bin/env python3
"""Render the app's SVG icon to the PNG sizes the bundles carry.

The AppImage's hicolor tree and the Tauri bundle's icon set both need real
PNGs at fixed sizes, and this host has no rsvg-convert or ImageMagick. It does
have gdk-pixbuf (it is the app's own image stack, and its SVG loader is what
draws every icon in the UI), so the rendering happens through gdk-pixbuf's
own loader -- no new dependency, no second SVG renderer to disagree with the
one the app draws with.

Usage: render-icons.py <source.svg> <output-dir> <size> [<size> ...]
Writes <output-dir>/<size>.png for each size, square.
"""

import ctypes
import ctypes.util
import os
import sys

LIBRARY = ctypes.util.find_library("gdk_pixbuf-2.0") or "libgdk_pixbuf-2.0.so.0"


def load():
    pixbuf = ctypes.CDLL(LIBRARY)
    pixbuf.gdk_pixbuf_new_from_file_at_size.restype = ctypes.c_void_p
    pixbuf.gdk_pixbuf_new_from_file_at_size.argtypes = [
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_void_p,
    ]
    pixbuf.gdk_pixbuf_save.restype = ctypes.c_int
    return pixbuf


def render(pixbuf, source, destination, size):
    scaled = pixbuf.gdk_pixbuf_new_from_file_at_size(
        source.encode(), size, size, None
    )
    if not scaled:
        raise SystemExit(f"gdk-pixbuf could not load {source}")
    # gdk_pixbuf_save is variadic: (pixbuf, path, type, &error, ...) -- the
    # trailing NULL ends the options list.
    saved = pixbuf.gdk_pixbuf_save(
        ctypes.c_void_p(scaled), destination.encode(), b"png", None, None
    )
    if not saved:
        raise SystemExit(f"gdk-pixbuf could not write {destination}")


def main(argv):
    if len(argv) < 4:
        raise SystemExit(__doc__)
    source, outdir = argv[1], argv[2]
    sizes = [int(size) for size in argv[3:]]
    if not os.path.isfile(source):
        raise SystemExit(f"no such icon: {source}")
    os.makedirs(outdir, exist_ok=True)
    pixbuf = load()
    for size in sizes:
        if size <= 0:
            raise SystemExit(f"bad size: {size}")
        destination = os.path.join(outdir, f"{size}.png")
        render(pixbuf, source, destination, size)
        print(f"{destination} ({size}x{size})")


if __name__ == "__main__":
    main(sys.argv)

"""Capture one X11 window to a PNG, with libX11 and the stdlib only.

XGetImage returns a BadMatch on a window that is not viewable, and under
Xwayland the *root* window can never be read at all -- so this takes a window
id, checks first that it is mapped, and says which of the two went wrong
instead of dying on an X error. Use scripts/muse-gui.sh to get an id that is
viewable.

    DISPLAY=:1 python3 scripts/x11-shot.py 0x3000004 out.png
"""
import ctypes
import ctypes.util
import struct
import sys
import zlib

x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XGetWindowAttributes.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p]
x11.XGetGeometry.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(ctypes.c_ulong),
    ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int),
    ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint),
    ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint),
]
x11.XGetImage.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
    ctypes.c_uint, ctypes.c_uint, ctypes.c_ulong, ctypes.c_int,
]
x11.XGetImage.restype = ctypes.c_void_p

VIEWABLE = 2


class WindowAttributes(ctypes.Structure):
    _fields_ = [
        ("x", ctypes.c_int), ("y", ctypes.c_int),
        ("width", ctypes.c_int), ("height", ctypes.c_int),
        ("border_width", ctypes.c_int), ("depth", ctypes.c_int),
        ("visual", ctypes.c_void_p), ("root", ctypes.c_ulong),
        ("class_", ctypes.c_int), ("bit_gravity", ctypes.c_int),
        ("win_gravity", ctypes.c_int), ("backing_store", ctypes.c_int),
        ("backing_planes", ctypes.c_ulong), ("backing_pixel", ctypes.c_ulong),
        ("save_under", ctypes.c_int), ("colormap", ctypes.c_ulong),
        ("map_installed", ctypes.c_int), ("map_state", ctypes.c_int),
        ("all_event_masks", ctypes.c_long), ("your_event_mask", ctypes.c_long),
        ("do_not_propagate_mask", ctypes.c_long),
        ("override_redirect", ctypes.c_int), ("screen", ctypes.c_void_p),
    ]


class XImage(ctypes.Structure):
    _fields_ = [
        ("width", ctypes.c_int),
        ("height", ctypes.c_int),
        ("xoffset", ctypes.c_int),
        ("format", ctypes.c_int),
        ("data", ctypes.c_void_p),
        ("byte_order", ctypes.c_int),
        ("bitmap_unit", ctypes.c_int),
        ("bitmap_bit_order", ctypes.c_int),
        ("bitmap_pad", ctypes.c_int),
        ("depth", ctypes.c_int),
        ("bytes_per_line", ctypes.c_int),
        ("bits_per_pixel", ctypes.c_int),
        ("red_mask", ctypes.c_ulong),
        ("green_mask", ctypes.c_ulong),
        ("blue_mask", ctypes.c_ulong),
    ]


def channel(value, mask):
    if not mask:
        return 0
    shift = (mask & -mask).bit_length() - 1
    width = bin(mask).count("1")
    raw = (value & mask) >> shift
    if width == 8:
        return raw
    if width == 0:
        return 0
    return raw * 255 // ((1 << width) - 1)


def png(path, width, height, rows):
    raw = bytearray()
    for row in rows:
        raw.append(0)
        raw.extend(row)

    def chunk(kind, payload):
        return (struct.pack(">I", len(payload)) + kind + payload
                + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF))

    with open(path, "wb") as handle:
        handle.write(b"\x89PNG\r\n\x1a\n")
        handle.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        handle.write(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
        handle.write(chunk(b"IEND", b""))


def main():
    if len(sys.argv) < 3:
        raise SystemExit("usage: x11-shot.py <window-id> <output.png>")
    window = int(sys.argv[1], 0)
    out = sys.argv[2]
    display = x11.XOpenDisplay(None)
    if not display:
        raise SystemExit("cannot open display")
    display = ctypes.c_void_p(display)

    attributes = WindowAttributes()
    if not x11.XGetWindowAttributes(display, window, ctypes.byref(attributes)):
        raise SystemExit(f"window {hex(window)} does not exist on this display")
    if attributes.map_state != VIEWABLE:
        raise SystemExit(
            f"window {hex(window)} is not viewable (map_state={attributes.map_state}); "
            "raise or unminimise it first (scripts/x11-window.py raise|unminimize <id>)"
        )

    root = ctypes.c_ulong()
    gx = ctypes.c_int(); gy = ctypes.c_int()
    gw = ctypes.c_uint(); gh = ctypes.c_uint()
    border = ctypes.c_uint(); depth = ctypes.c_uint()
    if not x11.XGetGeometry(display, window,
                            ctypes.byref(root), ctypes.byref(gx), ctypes.byref(gy),
                            ctypes.byref(gw), ctypes.byref(gh),
                            ctypes.byref(border), ctypes.byref(depth)):
        raise SystemExit("XGetGeometry failed")
    image_ptr = x11.XGetImage(display, window, 0, 0, gw, gh, 0xFFFFFFFF, 2)
    if not image_ptr:
        raise SystemExit("XGetImage failed")
    image = ctypes.cast(image_ptr, ctypes.POINTER(XImage)).contents
    width, height = image.width, image.height
    bpp = image.bits_per_pixel
    stride = image.bytes_per_line
    data = ctypes.string_at(image.data, stride * height)
    rmask, gmask, bmask = image.red_mask, image.green_mask, image.blue_mask
    step = bpp // 8
    rows = []
    for y in range(height):
        line = data[y * stride:(y + 1) * stride]
        row = bytearray()
        for x in range(width):
            offset = x * step
            if step >= 3:
                value = int.from_bytes(line[offset:offset + step], "little")
            else:
                value = line[offset]
            row.append(channel(value, rmask))
            row.append(channel(value, gmask))
            row.append(channel(value, bmask))
        rows.append(row)
    png(out, width, height, rows)
    print("captured %dx%d bpp=%d depth=%d -> %s" % (width, height, bpp, image.depth, out))


main()

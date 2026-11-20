"""Click at absolute screen coordinates with XTEST (libXtst), no xdotool.

Coordinates are screen coordinates. Screenshots are window-relative, so add the
window's absolute upper-left (xwininfo -id <id> | grep Absolute) before clicking.

    DISPLAY=:1 python3 scripts/x11-click.py 409 281
"""
import ctypes
import ctypes.util
import sys
import time

x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
xtst = ctypes.CDLL(ctypes.util.find_library("Xtst"))
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XFlush.argtypes = [ctypes.c_void_p]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
                                      ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int,
                                      ctypes.c_ulong]

if len(sys.argv) < 3:
    raise SystemExit("usage: x11-click.py <x> <y>")

display = x11.XOpenDisplay(None)
if not display:
    raise SystemExit("cannot open display")
display = ctypes.c_void_p(display)
x, y = int(sys.argv[1]), int(sys.argv[2])
xtst.XTestFakeMotionEvent(display, 0, x, y, 0)
x11.XFlush(display)
time.sleep(0.25)
xtst.XTestFakeButtonEvent(display, 1, 1, 0)
x11.XFlush(display)
time.sleep(0.08)
xtst.XTestFakeButtonEvent(display, 1, 0, 0)
x11.XFlush(display)
time.sleep(0.4)
print("clicked", x, y)

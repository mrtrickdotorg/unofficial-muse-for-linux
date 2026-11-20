"""Window control with libX11 and the stdlib only: no xdotool needed.

    x11-window.py close <win>        ask the client to close (WM_DELETE_WINDOW)
    x11-window.py raise <win>        activate and raise it (EWMH _NET_ACTIVE_WINDOW)
    x11-window.py unminimize <win>   ICCCM WM_CHANGE_STATE -> NormalState, then map/raise

A window the window manager kept hidden cannot be clicked or captured; close
also matters for the web views, which open on top of the app's sidebar.

    DISPLAY=:1 python3 scripts/x11-window.py raise 0x3000004
"""
import ctypes
import ctypes.util
import sys
import time

x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XInternAtom.restype = ctypes.c_ulong
x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x11.XDefaultRootWindow.restype = ctypes.c_ulong
x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                           ctypes.c_long, ctypes.c_void_p]
x11.XMapRaised.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XRaiseWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]


class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong),
                ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("l", ctypes.c_long * 5)]


class Event(ctypes.Union):
    _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]


CLIENT_MESSAGE = 33
SUBSTRUCTURE_REDIRECT = 1 << 20
SUBSTRUCTURE_NOTIFY = 1 << 19


def send(display, target, event_mask, message_type, values):
    event = Event()
    event.xclient.type = CLIENT_MESSAGE
    event.xclient.window = target
    event.xclient.message_type = message_type
    event.xclient.format = 32
    for index, value in enumerate(values):
        event.xclient.l[index] = value
    x11.XSendEvent(display, target, False, event_mask, ctypes.byref(event))
    x11.XFlush(display)


def main():
    if len(sys.argv) < 3:
        raise SystemExit("usage: x11-window.py close|raise|unminimize <window-id>")
    action = sys.argv[1]
    window = int(sys.argv[2], 0)
    display = x11.XOpenDisplay(None)
    if not display:
        raise SystemExit("cannot open display")
    root = x11.XDefaultRootWindow(display)
    atom = lambda name: x11.XInternAtom(display, name, False)
    if action == "close":
        send(display, window, 0, atom(b"WM_PROTOCOLS"),
             [atom(b"WM_DELETE_WINDOW"), 0, 0, 0, 0])
    elif action == "raise":
        send(display, root, SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
             atom(b"_NET_ACTIVE_WINDOW"), [2, 0, 0, 0, 0])
        x11.XRaiseWindow(display, window)
        x11.XFlush(display)
    elif action == "unminimize":
        # ICCCM: WM_CHANGE_STATE NormalState de-iconifies a managed window.
        send(display, root, SUBSTRUCTURE_REDIRECT | SUBSTRUCTURE_NOTIFY,
             atom(b"WM_CHANGE_STATE"), [1, 0, 0, 0, 0])
        x11.XMapRaised(display, window)
        x11.XFlush(display)
    else:
        raise SystemExit("usage: x11-window.py close|raise|unminimize <window-id>")
    time.sleep(0.3)
    print(action, hex(window))


main()

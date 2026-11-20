"""Type a string into the focused X11 window with XTEST (libXtst), no xdotool.

Click the field first: the keys go wherever focus is. Only the characters the
live checks need are mapped (letters, digits, space and a few punctuation marks);
anything else is skipped rather than guessed at.

    DISPLAY=:1 python3 scripts/x11-type.py "parking"
"""
import ctypes
import ctypes.util
import sys
import time

x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
xtst = ctypes.CDLL(ctypes.util.find_library("Xtst"))
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.restype = ctypes.c_uint

NAMES = {
    " ": "space",
    "-": "minus",
    ".": "period",
    ",": "comma",
    "'": "apostrophe",
    "/": "slash",
    ":": "colon",
    "_": "underscore",
    "?": "question",
}
SHIFTED = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ:?_")


def press(display, code: int, modifiers: list[int]) -> None:
    for modifier in modifiers:
        xtst.XTestFakeKeyEvent(display, modifier, True, 0)
    xtst.XTestFakeKeyEvent(display, code, True, 0)
    xtst.XTestFakeKeyEvent(display, code, False, 0)
    for modifier in reversed(modifiers):
        xtst.XTestFakeKeyEvent(display, modifier, False, 0)
    x11.XFlush(display)


def main(words: str, clear: bool = False) -> None:
    display = x11.XOpenDisplay(None)
    if not display:
        raise SystemExit("no display")
    shift = x11.XKeysymToKeycode(display, x11.XStringToKeysym(b"Shift_L"))
    if clear:
        control = x11.XKeysymToKeycode(display, x11.XStringToKeysym(b"Control_L"))
        select_all = x11.XKeysymToKeycode(display, x11.XStringToKeysym(b"a"))
        press(display, select_all, [control])
        time.sleep(0.05)
    typed = 0
    for char in words:
        keysym = x11.XStringToKeysym(NAMES.get(char, char).encode())
        if not keysym:
            continue
        code = x11.XKeysymToKeycode(display, keysym)
        if not code:
            continue
        shifted = char in SHIFTED
        if shifted:
            xtst.XTestFakeKeyEvent(display, shift, True, 0)
        xtst.XTestFakeKeyEvent(display, code, True, 0)
        xtst.XTestFakeKeyEvent(display, code, False, 0)
        if shifted:
            xtst.XTestFakeKeyEvent(display, shift, False, 0)
        x11.XFlush(display)
        typed += 1
        time.sleep(0.02)
    x11.XFlush(display)
    print(f"typed {typed} keys: {words!r}")


if __name__ == "__main__":
    arguments = sys.argv[1:]
    clear = False
    if arguments and arguments[0] == "--clear":
        clear = True
        arguments = arguments[1:]
    if len(arguments) != 1:
        raise SystemExit("usage: python3 scripts/x11-type.py [--clear] <words>")
    main(arguments[0], clear)

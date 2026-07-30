#!/usr/bin/env python3

import ctypes
import os
import sys


def fail(message: str) -> None:
    raise SystemExit(message)


if len(sys.argv) != 3 or sys.argv[2] not in {"marker", "close"}:
    fail("usage: x11-x-test-input.py <window-id> <marker|close>")

window_id = int(sys.argv[1], 0)
x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

display = x11.XOpenDisplay(os.environ.get("DISPLAY", "").encode())
if not display:
    fail("failed to open X11 display")


def keycode(name: str) -> int:
    keysym = x11.XStringToKeysym(name.encode())
    code = x11.XKeysymToKeycode(display, keysym)
    if not code:
        fail(f"missing X11 keycode: {name}")
    return code


def key(name: str, pressed: bool) -> None:
    if not xtst.XTestFakeKeyEvent(display, keycode(name), int(pressed), 0):
        fail(f"XTEST rejected key event: {name}")


x11.XSetInputFocus(display, window_id, 1, 0)
if sys.argv[2] == "marker":
    key("t", True)
    key("t", False)
else:
    key("Alt_L", True)
    key("F4", True)
    key("F4", False)
    key("Alt_L", False)
x11.XFlush(display)
x11.XCloseDisplay(display)

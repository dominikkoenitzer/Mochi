"""Three processes in the layout a Chromium window has: one owns a top level
window, one owns a child window on a thread of its own and resizes it from
another thread, and this one minimizes and moves the top level window.

Runs on a CI runner only. It moves real windows.
"""
import argparse
import ctypes
import json
import subprocess
import sys
import threading
import time
from ctypes import wintypes as w

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, SW_SHOWMINNOACTIVE = 6, 9, 4, 7
SWP_NOSIZE, SWP_NOMOVE, SWP_NOZORDER, SWP_NOREDRAW, SWP_NOACTIVATE = 0x1, 0x2, 0x4, 0x8, 0x10
SWP_NOCOPYBITS, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING = 0x100, 0x200, 0x400
RESIZE_FLAGS = (SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_NOMOVE | SWP_NOOWNERZORDER
                | SWP_NOREDRAW | SWP_NOSENDCHANGING | SWP_NOZORDER)
WS_POPUP, WS_VISIBLE, WS_CHILD, WS_DISABLED = 0x80000000, 0x10000000, 0x40000000, 0x08000000
WS_OVERLAPPEDWINDOW = 0x00CF0000
EX_TOOLWINDOW, EX_LAYERED, EX_TRANSPARENT = 0x80, 0x80000, 0x20
EX_NOPARENTNOTIFY, EX_NOREDIRECTIONBITMAP = 0x4, 0x200000


class WNDCLASS(ctypes.Structure):
    _fields_ = [("style", w.UINT), ("proc", ctypes.c_void_p), ("extra", ctypes.c_int),
                ("wnd", ctypes.c_int), ("instance", w.HINSTANCE), ("icon", w.HICON),
                ("cursor", w.HANDLE), ("brush", w.HBRUSH), ("menu", w.LPCWSTR), ("name", w.LPCWSTR)]


kernel32.GetProcAddress.restype = ctypes.c_void_p
kernel32.GetProcAddress.argtypes = [w.HMODULE, ctypes.c_char_p]
kernel32.GetModuleHandleW.restype = w.HMODULE
kernel32.GetModuleHandleW.argtypes = [w.LPCWSTR]
user32.RegisterClassW.argtypes = [ctypes.POINTER(WNDCLASS)]
user32.CreateWindowExW.restype = w.HWND
user32.CreateWindowExW.argtypes = [w.DWORD, w.LPCWSTR, w.LPCWSTR, w.DWORD] + [ctypes.c_int] * 4 + [
    w.HWND, w.HMENU, w.HINSTANCE, ctypes.c_void_p]
user32.GetMessageW.argtypes = [ctypes.POINTER(w.MSG), w.HWND, w.UINT, w.UINT]
user32.DispatchMessageW.argtypes = [ctypes.POINTER(w.MSG)]
user32.TranslateMessage.argtypes = [ctypes.POINTER(w.MSG)]
user32.SetWindowPos.argtypes = [w.HWND, w.HWND] + [ctypes.c_int] * 4 + [w.UINT]
user32.SetParent.restype = w.HWND
user32.SetParent.argtypes = [w.HWND, w.HWND]
user32.ShowWindow.argtypes = [w.HWND, ctypes.c_int]
user32.GetWindowRect.argtypes = [w.HWND, ctypes.POINTER(w.RECT)]
user32.ClientToScreen.argtypes = [w.HWND, ctypes.POINTER(w.POINT)]
user32.IsWindow.argtypes = [w.HWND]


def register(name):
    # No callback of ours: the default procedure answers directly, so nothing
    # here is held up by the interpreter.
    module = kernel32.GetModuleHandleW("user32.dll")
    cls = WNDCLASS(0x20, kernel32.GetProcAddress(module, b"DefWindowProcW"), 0, 0,
                   kernel32.GetModuleHandleW(None), None, None, None, None, name)
    if not user32.RegisterClassW(ctypes.byref(cls)):
        raise ctypes.WinError(ctypes.get_last_error())


def create(ex, name, style, x, y, width, height, parent=None):
    hwnd = user32.CreateWindowExW(ex, name, "", style, x, y, width, height, parent, None,
                                  kernel32.GetModuleHandleW(None), None)
    if not hwnd:
        raise ctypes.WinError(ctypes.get_last_error())
    return hwnd


def pump():
    message = w.MSG()
    while user32.GetMessageW(ctypes.byref(message), None, 0, 0) > 0:
        user32.TranslateMessage(ctypes.byref(message))
        user32.DispatchMessageW(ctypes.byref(message))


def role_child(pause):
    """Owns the child on one thread, resizes it from another."""
    made = threading.Event()
    box = {}

    def owner():
        register("StandinChild")
        register("StandinHidden")
        hidden = create(EX_TOOLWINDOW, "StandinHidden", WS_POPUP, 0, 0, 0, 0)
        box["child"] = create(EX_NOPARENTNOTIFY | EX_LAYERED | EX_TRANSPARENT | EX_NOREDIRECTIONBITMAP,
                              "StandinChild", WS_CHILD | WS_DISABLED | WS_VISIBLE, 0, 0, 1, 1, hidden)
        made.set()
        pump()

    threading.Thread(target=owner, daemon=True).start()
    made.wait()
    print(box["child"], flush=True)
    i = 0
    while user32.IsWindow(box["child"]):
        i += 1
        user32.SetWindowPos(box["child"], None, 0, 0, 700 + (i & 1), 500, RESIZE_FLAGS)
        if pause:
            time.sleep(pause)


def role_parent(child):
    """Owns the top level window and adopts the child, as a browser does."""
    register("StandinTop")
    top = create(0, "StandinTop", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 60, 40, 800, 600)
    user32.SetParent(child, top)
    print(top, flush=True)
    pump()


def origin_rel(child, top):
    r, p = w.RECT(), w.POINT(0, 0)
    user32.GetWindowRect(child, ctypes.byref(r))
    user32.ClientToScreen(top, ctypes.byref(p))
    return (r.left - p.x, r.top - p.y)


def spawn(*args):
    return subprocess.Popen([sys.executable, "-I", __file__, *args], stdout=subprocess.PIPE, text=True)


def act(top, mode, i):
    if mode == "minimize":
        user32.ShowWindow(top, SW_MINIMIZE)
        user32.ShowWindow(top, SW_RESTORE)
    elif mode == "noactivate":
        user32.ShowWindow(top, SW_SHOWMINNOACTIVE)
        user32.ShowWindow(top, SW_SHOWNOACTIVATE)
    elif mode == "move":
        user32.SetWindowPos(top, None, 60 + 300 * (i & 1), 40, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
    elif mode != "idle":
        raise SystemExit(mode)


def run(mode, resizing, cycles, pause):
    args = ["--role", "child", "--pause", str(pause)]
    if not resizing:
        args = ["--role", "child", "--pause", "1000"]
    child_process = spawn(*args)
    child = int(child_process.stdout.readline())
    parent_process = spawn("--role", "parent", "--child", str(child))
    top = int(parent_process.stdout.readline())
    time.sleep(0.5)
    seen = []
    start = time.time()
    try:
        for i in range(cycles):
            act(top, mode, i)
            time.sleep(0.03)
            rel = origin_rel(child, top)
            if rel != (0, 0):
                seen.append(rel)
                user32.SetWindowPos(child, None, 0, 0, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
                time.sleep(0.01)
    finally:
        parent_process.kill()
        child_process.kill()
    print(json.dumps({"mode": mode, "child_resizing": resizing, "cycles": cycles,
                      "displaced": len(seen), "distinct": sorted(set(seen))[:12],
                      "seconds": round(time.time() - start, 1)}), flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--role", choices=("driver", "child", "parent"), default="driver")
    parser.add_argument("--child", type=int)
    parser.add_argument("--pause", type=float, default=0.0)
    parser.add_argument("--cycles", type=int, default=2000)
    args = parser.parse_args()
    if args.role == "child":
        role_child(args.pause)
    elif args.role == "parent":
        role_parent(args.child)
    else:
        for mode in ("minimize", "noactivate", "move"):
            run(mode, False, args.cycles, 0)
            run(mode, True, args.cycles, 0)
        run("minimize", True, args.cycles, 0.016)
        run("idle", True, 200, 0)


if __name__ == "__main__":
    sys.exit(main())

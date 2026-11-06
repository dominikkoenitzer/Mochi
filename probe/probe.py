"""Drives a throwaway Chromium window through minimize and restore and reads
where its compositor child window ends up.

Runs on a CI runner only. It moves real windows.
"""
import argparse
import ctypes
import glob
import json
import multiprocessing
import os
import random
import subprocess
import sys
import tempfile
import threading
import time
import winreg
from ctypes import wintypes as w

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, SW_SHOWMINNOACTIVE = 6, 9, 4, 7
SWP_NOSIZE, SWP_NOMOVE, SWP_NOZORDER, SWP_NOACTIVATE = 0x1, 0x2, 0x4, 0x10
SWP_FRAMECHANGED = 0x20
MOVE_FLAGS = SWP_NOACTIVATE | SWP_FRAMECHANGED
CHILD_CLASS = "Intermediate D3D Window"
ENUM = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)

for fn in ("GetWindowRect", "GetClientRect"):
    getattr(user32, fn).argtypes = [w.HWND, ctypes.POINTER(w.RECT)]
user32.ClientToScreen.argtypes = [w.HWND, ctypes.POINTER(w.POINT)]
user32.ShowWindow.argtypes = [w.HWND, ctypes.c_int]
user32.IsIconic.argtypes = [w.HWND]
user32.GetClassNameW.argtypes = [w.HWND, w.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, ctypes.c_int]
user32.SetWindowPos.argtypes = [w.HWND, w.HWND] + [ctypes.c_int] * 4 + [w.UINT]
user32.GetParent.restype = w.HWND
user32.GetParent.argtypes = [w.HWND]
user32.GetWindowThreadProcessId.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]


def cls(hwnd):
    buf = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, buf, 256)
    return buf.value


def title(hwnd):
    buf = ctypes.create_unicode_buffer(256)
    user32.GetWindowTextW(hwnd, buf, 256)
    return buf.value


def rect(hwnd):
    r = w.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(r)):
        raise ctypes.WinError(ctypes.get_last_error())
    return (r.left, r.top, r.right, r.bottom)


def client_origin(hwnd):
    p = w.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(p))
    return (p.x, p.y)


def client_size(hwnd):
    r = w.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    return (r.right, r.bottom)


def children(hwnd):
    found = []

    def visit(child, _):
        found.append(child)
        return True

    user32.EnumChildWindows(hwnd, ENUM(visit), 0)
    return found


def compositor_children(hwnd):
    return [c for c in children(hwnd) if cls(c) == CHILD_CLASS]


def find_window(marker, timeout):
    end = time.time() + timeout
    while time.time() < end:
        hits = []

        def visit(hwnd, _):
            if user32.IsWindowVisible(hwnd) and marker in title(hwnd):
                hits.append(hwnd)
            return True

        user32.EnumWindows(ENUM(visit), 0)
        if hits:
            return hits[0]
        time.sleep(0.2)
    raise SystemExit(f"no window with {marker!r} after {timeout} s")


def relative(child, parent):
    x, y = client_origin(parent)
    l, t, r, b = rect(child)
    return (l - x, t - y, r - x, b - y)


def outside(rel, size):
    l, t, r, b = rel
    return r <= 0 or b <= 0 or l >= size[0] or t >= size[1]


def colours(hwnd):
    """Distinct colours inside the window's client area as the screen shows it."""
    x, y = client_origin(hwnd)
    width, height = client_size(hwnd)
    if width <= 0 or height <= 0:
        return 0
    screen = user32.GetDC(None)
    dc = gdi32.CreateCompatibleDC(screen)
    bitmap = gdi32.CreateCompatibleBitmap(screen, width, height)
    gdi32.SelectObject(dc, bitmap)
    gdi32.BitBlt(dc, 0, 0, width, height, screen, x, y, 0x00CC0020)

    class Header(ctypes.Structure):
        _fields_ = [
            ("size", w.DWORD), ("width", w.LONG), ("height", w.LONG),
            ("planes", w.WORD), ("bits", w.WORD), ("compression", w.DWORD),
            ("image", w.DWORD), ("xppm", w.LONG), ("yppm", w.LONG),
            ("used", w.DWORD), ("important", w.DWORD),
        ]

    header = Header(ctypes.sizeof(Header), width, -height, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = (ctypes.c_uint32 * (width * height))()
    gdi32.GetDIBits(dc, bitmap, 0, height, pixels, ctypes.byref(header), 0)
    gdi32.DeleteObject(bitmap)
    gdi32.DeleteDC(dc)
    user32.ReleaseDC(None, screen)
    return len(set(pixels[::7]))


PAGE = """<!doctype html><title>{marker}</title>
<body style="margin:0;background:#121212;color:#eee;font:20px sans-serif">
<canvas id=c width=900 height=700></canvas>
<script>
const g=document.getElementById('c').getContext('2d');let n=0;
(function f(){{n++;for(let i=0;i<40;i++){{g.fillStyle=`hsl(${{(n+i*9)%360}},70%,50%)`;
g.fillRect((n*3+i*23)%900,(i*17)%700,22,22)}}requestAnimationFrame(f)}})();
</script>"""


def tile(hwnd, i):
    """One move the way a window manager makes it, frame recalculation included."""
    user32.SetWindowPos(hwnd, None, 40 + 300 * (i % 2), 30 + 40 * (i % 2),
                        1000 - 60 * (i % 2), 760 - 40 * (i % 2), MOVE_FLAGS)


def animate(hwnd, stop_event):
    """Frames in flight, independent of whatever else happens to the window."""
    i = 0
    while not stop_event.is_set():
        i += 1
        tile(hwnd, i)
        time.sleep(0.016)


def launch(browser, flags, marker, workdir):
    page = os.path.join(workdir, "page.html")
    with open(page, "w", encoding="ascii") as out:
        out.write(PAGE.format(marker=marker))
    profile = os.path.join(workdir, "profile")
    if os.path.basename(browser).lower().startswith("electron"):
        app = os.path.join(os.path.dirname(os.path.abspath(__file__)), "app")
        return subprocess.Popen([browser, app, f"--user-data-dir={profile}", "--mute-audio", *flags, page])
    command = [
        browser, f"--user-data-dir={profile}", "--no-first-run",
        "--no-default-browser-check", "--disable-sync", "--mute-audio",
        "--window-position=120,80", "--window-size=1000,760",
        f"--app=file:///{page.replace(os.sep, '/')}",
    ] + flags
    return subprocess.Popen(command)


def stop(process):
    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                   capture_output=True, check=False)
    process.wait(timeout=30)


def installed_chromium():
    """The first registered browser that is built on Chromium."""
    root = r"SOFTWARE\Clients\StartMenuInternet"
    for hive in (winreg.HKEY_LOCAL_MACHINE, winreg.HKEY_CURRENT_USER):
        try:
            key = winreg.OpenKey(hive, root)
        except OSError:
            continue
        for i in range(winreg.QueryInfoKey(key)[0]):
            name = winreg.EnumKey(key, i)
            try:
                command = winreg.QueryValue(key, name + r"\shell\open\command")
            except OSError:
                continue
            exe = command.strip().strip('"').split('"')[0]
            if glob.glob(os.path.join(os.path.dirname(exe), "*", "*_elf.dll")):
                return exe
    raise SystemExit("no Chromium browser registered")


def place(hwnd, x, y):
    user32.SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)


def burn():
    while True:
        pass


def resize(hwnd, i):
    user32.SetWindowPos(hwnd, None, 0, 0, 1000 + 40 * (i % 2), 760 + 30 * (i % 2),
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE)


def cycle(hwnd, style, down, settle, move=None, jitter=None, iconic=0, step=0):
    if move:
        place(hwnd, *move[0])
    if jitter is not None:
        # A layout change right before the minimize, so the GPU process is
        # resizing its child while the parent goes away.
        resize(hwnd, jitter[0])
        time.sleep(jitter[1])
    if style == "noactivate":
        user32.ShowWindow(hwnd, SW_SHOWMINNOACTIVE)
        time.sleep(down)
        user32.ShowWindow(hwnd, SW_SHOWNOACTIVATE)
    else:
        user32.ShowWindow(hwnd, SW_MINIMIZE)
        for n in range(iconic):
            tile(hwnd, step + n)
        time.sleep(down)
        user32.ShowWindow(hwnd, SW_RESTORE)
    if move:
        place(hwnd, *move[1])
    if jitter is not None:
        resize(hwnd, jitter[0] + 1)
    time.sleep(settle)


def run(args):
    args.browser = args.browser or installed_chromium()
    report = {"browser": os.path.basename(args.browser), "flags": args.flag, "style": args.style,
              "cycles": args.cycles, "iconic": args.iconic, "animate": args.animate, "resize": args.resize}
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as workdir:
        marker = f"probe-{os.getpid()}"
        process = launch(args.browser, args.flag, marker, workdir)
        try:
            hwnd = find_window(marker, 40)
            time.sleep(4)
            size = client_size(hwnd)
            healthy = colours(hwnd)
            found = compositor_children(hwnd)
            report.update(
                parent_rect=rect(hwnd), client_origin=client_origin(hwnd), client=size,
                healthy_colours=healthy,
                child_classes=sorted({cls(c) for c in children(hwnd)}),
                compositor_children=[relative(c, hwnd) for c in found],
            )
            print(json.dumps(report), flush=True)
            displaced = []
            rng = random.Random(args.seed)
            frames_stop = threading.Event()
            if args.animate:
                threading.Thread(target=animate, args=(hwnd, frames_stop), daemon=True).start()
            for i in range(args.cycles):
                down = args.down if args.down >= 0 else rng.uniform(0.0, 0.7)
                move = ((40 + 200 * (i % 2), 30), (240 - 200 * (i % 2), 90)) if args.move else None
                jitter = (i, rng.uniform(0.0, 0.03)) if args.resize else None
                cycle(hwnd, args.style, down, args.settle, move, jitter, args.iconic, i)
                now = compositor_children(hwnd)
                rels = [relative(c, hwnd) for c in now]
                if rels and all(outside(r, client_size(hwnd)) for r in rels):
                    seen = colours(hwnd)
                    displaced.append({"cycle": i, "down": round(down, 3), "rels": rels, "colours": seen})
                    print(json.dumps(displaced[-1]), flush=True)
                    time.sleep(1)
                    frozen = [colours(hwnd), [relative(c, hwnd) for c in now]]
                    for c in now:
                        user32.SetWindowPos(c, None, 0, 0, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
                    time.sleep(0.7)
                    print(json.dumps({"one_second_later": frozen, "put_back": [relative(c, hwnd) for c in now],
                                      "colours": colours(hwnd)}), flush=True)
            frames_stop.set()
            report["displaced"] = len(displaced)
            report["end_colours"] = colours(hwnd)
            if args.displace:
                # The state itself, made on purpose: is it a blank, and does
                # putting the child back bring the picture back?
                now = compositor_children(hwnd)
                before = colours(hwnd)
                for c in now:
                    left, top, _, _ = relative(c, hwnd)
                    user32.SetWindowPos(c, None, left + 32000, top + 32000, 0, 0,
                                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
                time.sleep(1)
                moved = colours(hwnd)
                moved_rels = [relative(c, hwnd) for c in now]
                time.sleep(3)
                later_rels = [relative(c, hwnd) for c in now]
                later = colours(hwnd)
                for c in now:
                    user32.SetWindowPos(c, None, 0, 0, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
                time.sleep(1)
                report["displace"] = {
                    "before": before, "moved": moved, "moved_rels": moved_rels,
                    "after_3s_rels": later_rels, "after_3s": later,
                    "put_back": colours(hwnd), "put_back_rels": [relative(c, hwnd) for c in now],
                }
        finally:
            stop(process)
    print("RESULT " + json.dumps(report), flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser")
    parser.add_argument("--flag", action="append", default=[])
    parser.add_argument("--style", choices=("restore", "noactivate"), default="restore")
    parser.add_argument("--cycles", type=int, default=100)
    parser.add_argument("--down", type=float, default=0.6, help="seconds minimized; negative picks at random")
    parser.add_argument("--settle", type=float, default=0.4)
    parser.add_argument("--seed", type=int, default=11)
    parser.add_argument("--resize", action="store_true", help="resize the window around each cycle")
    parser.add_argument("--iconic", type=int, default=0, help="moves to make while minimized")
    parser.add_argument("--animate", action="store_true", help="keep making moves on another thread")
    parser.add_argument("--load", type=int, default=0, help="busy processes to run alongside")
    parser.add_argument("--displace", action="store_true")
    parser.add_argument("--move", action="store_true", help="move the window around each cycle")
    args = parser.parse_args()
    burners = [multiprocessing.Process(target=burn, daemon=True) for _ in range(args.load)]
    for burner in burners:
        burner.start()
    try:
        run(args)
    finally:
        for burner in burners:
            burner.terminate()


if __name__ == "__main__":
    sys.exit(main())

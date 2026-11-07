"""Puts a bug back into a checkout of the fix, so its tests can be seen to fail."""
import subprocess
import sys


def swap(path, old, new):
    with open(path, encoding="utf-8", newline="") as source:
        text = source.read()
    old = old.replace("\n", "\r\n") if "\r\n" in text else old
    new = new.replace("\n", "\r\n") if "\r\n" in text else new
    if old not in text:
        sys.exit(f"{path}: missing {old[:60]}")
    with open(path, "w", encoding="utf-8", newline="") as out:
        out.write(text.replace(old, new))


def everything():
    """The whole of the old behaviour, under the tests of the new."""
    subprocess.run(
        ["git", "checkout", "origin/main", "--", "crates/mochi/src/wm.rs",
         "crates/mochi/src/visuals.rs", "crates/mochi/src/events/mod.rs"],
        check=True,
    )
    posted()


def posted():
    swap("crates/mochi/src/platform/win32.rs",
         "    window_class(h).starts_with(mochi_core::layering::CHROMIUM_CLASS_PREFIX)\n",
         "    let _ = h;\n    false\n")


def activating():
    swap("crates/mochi/src/wm.rs",
         "self.platform.show(hwnd, ShowState::MinimizeNoActivate)",
         "self.platform.show(hwnd, ShowState::Minimize)")
    swap("crates/mochi/src/wm.rs",
         "self.platform.show(hwnd, ShowState::ShowNoActivate) {\n"
         "                tracing::debug!(%hwnd, error = %e, \"could not restore a redrawn window\");",
         "self.platform.show(hwnd, ShowState::Restore) {\n"
         "                tracing::debug!(%hwnd, error = %e, \"could not restore a redrawn window\");")


def no_second_pass():
    swap("crates/mochi/src/wm.rs",
         "                self.minimized.remove(&hwnd);\n                self.window_appeared(hwnd);\n",
         "                self.minimized.remove(&hwnd);\n")


def parked_start():
    swap("crates/mochi/src/visuals.rs", "Ok(info) if info.minimized => {", "Ok(info) if false && info.minimized => {")


{"everything": everything, "posted": posted, "activating": activating, "parked_start": parked_start,
 "no_second_pass": no_second_pass}[sys.argv[1]]()
print("put back:", sys.argv[1])

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
         "self.platform.show(hwnd, ShowState::RestoreNoActivate) {\n"
         "                tracing::debug!(%hwnd, error = %e, \"could not restore a redrawn window\");",
         "self.platform.show(hwnd, ShowState::Restore) {\n"
         "                tracing::debug!(%hwnd, error = %e, \"could not restore a redrawn window\");")


def move_overtakes():
    """State changes stay posted, moves go straight to the window."""
    swap("crates/mochi/src/platform/win32.rs",
         ".partition(|p| holds_gpu_child(hwnd(p.hwnd)));",
         ".partition(|_| false);")


def parked_border():
    """A move measured while minimized goes by the border it shows then."""
    swap("crates/mochi/src/platform/win32.rs",
         "let frame = if look.iconic {",
         "let frame = if false {")


def parked_start():
    swap("crates/mochi/src/visuals.rs", "Ok(info) if info.minimized => {", "Ok(info) if false && info.minimized => {")


def late_minimize():
    """A minimize that reports in late reads as the user's."""
    swap("crates/mochi/src/wm.rs",
         "let expected = self.minimize_unseen.remove(&hwnd);",
         "let expected = false;")


def stale_read():
    """The time alone says when a move is over."""
    swap("crates/mochi/src/visuals.rs",
         "move_.until > now || move_.to != from",
         "move_.until > now")


def torn_read():
    """One look is taken for the truth."""
    swap("crates/mochi/src/platform/win32.rs",
         "    let mut last = look();\n",
         "    let mut last = look();\n"
         "    if LOOKS_TO_SETTLE > 0 {\n"
         "        return outer_rect_at(last, target);\n"
         "    }\n")


def posted_hide():
    """Hiding and showing go through the queue like the rest."""
    swap("crates/mochi/src/platform/win32.rs",
         "let posted = !matches!(state, ShowState::ShowNoActivate | ShowState::Hide);",
         "let posted = true;")


def holder_minimize():
    """The minimize of a redraw hands the keyboard on."""
    swap("crates/mochi/src/wm.rs",
         "self.platform.show(hwnd, ShowState::MinimizeNoActivate)",
         "self.platform.show(hwnd, ShowState::Minimize)")


def zoom_lost():
    """The restore of a redraw goes back to the normal size."""
    swap("crates/mochi/src/platform/win32.rs",
         "ShowState::RestoreNoActivate | ShowState::ShowNoActivate => SW_SHOWNOACTIVATE,",
         "ShowState::RestoreNoActivate => windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,\n"
         "            ShowState::ShowNoActivate => SW_SHOWNOACTIVATE,")


{"late_minimize": late_minimize, "stale_read": stale_read, "torn_read": torn_read,
 "posted_hide": posted_hide, "holder_minimize": holder_minimize, "zoom_lost": zoom_lost,
 "everything": everything, "posted": posted, "activating": activating, "parked_start": parked_start,
 "move_overtakes": move_overtakes,
 "parked_border": parked_border}[sys.argv[1]]()
print("put back:", sys.argv[1])

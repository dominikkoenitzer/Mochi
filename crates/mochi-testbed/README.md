# mochi-testbed

Disposable Win32 windows for testing Mochi's tiling end to end, on a desktop
that is already running another window manager and the user's real apps.

The trick is one extended style: every test window carries `WS_EX_TOOLWINDOW`
and the class `MochiTestWindow`. Tool windows are skipped by every window
manager that follows the usual manageability rules, and they
show up neither in the taskbar nor in Alt-Tab. So a batch of them can sit on a
working desktop, be moved around by Mochi alone, and disappear again without
having touched anything the user cares about.

That protects the user's windows from the tests. It does not protect the tests
from the user's window manager, and the two are not the same promise. Another
manager that does take these windows moves and fades them underneath the
assertions, and what comes out is overlapping tiles, focus commands that look
broken and windows still faded after a stop, none of which is a defect in Mochi.
Give the suite the desktop to itself. `SpawnOptions::taskbar` gives up the tool
window bit deliberately, to be cloakable by the shell, and a window spawned that
way is visible to everything: only a test that knows it is alone asks for one.

Apart from that one bit the windows are ordinary: `WS_OVERLAPPEDWINDOW`, so DWM
draws a normal Windows 11 frame with rounded corners, a caption and the
invisible resize border that tiling has to compensate for. Each one paints a
big centred label with its title, its index and its current rect on a pastel
background, so a screenshot of a tiling run can be read at a glance.

The crate depends on `windows`, `serde`, `serde_json` and `clap` and on nothing
else in the workspace, so it keeps working while the crate under test does not
compile.

## Contract with the daemon

| | |
|---|---|
| Window class | `MochiTestWindow` |
| Extended style | `WS_EX_TOOLWINDOW`, never `WS_EX_APPWINDOW` |
| Title | `MochiTest 1`, `MochiTest 2`, … (`--title-prefix` changes the prefix) |
| Daemon test flag | `--manage-class MochiTestWindow` |
| Control pipe | `\\.\pipe\mochi-testbed` |
| Session file | `%TEMP%\mochi-testbed\session.json` |

The daemon side of the flag is the lead's to wire. What the testbed needs from
it:

* `--manage-class <CLASS>` may be repeated, and makes the daemon treat a window
  of that class as manageable **even though** `WS_EX_TOOLWINDOW` is set. It is
  the only rule that is allowed to override the tool window check.
* Nothing else changes: the class still has to pass the remaining checks
  (visible, not cloaked, not a child, large enough), and every other window on
  the desktop keeps being judged as before.
* Without the flag the daemon must leave these windows alone. That is what
  makes it safe to run `mochi-testwin` next to a normal daemon session.

The constants are exported so a test never has to spell them out:
`mochi_testbed::TEST_WINDOW_CLASS`, `DEFAULT_TITLE_PREFIX`,
`DAEMON_MANAGE_FLAG`.

## Command line

```
mochi-testwin spawn [--count N] [--monitor I] [--title-prefix X]
                    [--x X --y Y] [--w W --h H] [--min-size W H]
                    [--owned] [--no-title]
mochi-testwin list
mochi-testwin monitors
mochi-testwin close --all | --hwnd H
mochi-testwin focus --hwnd H
mochi-testwin resize --hwnd H --w W --h H
mochi-testwin move --hwnd H --x X --y Y
mochi-testwin minimize --hwnd H
mochi-testwin restore --hwnd H
mochi-testwin rename --hwnd H --title T
mochi-testwin cloak --hwnd H --on | --off
mochi-testwin alpha --hwnd H --value N | --clear
```

Defaults: `--count 3`, `--monitor 0`, `--title-prefix MochiTest`. Handles are
accepted as `4660` or `0x1234`, which is what `list` prints and what the daemon
logs show.

The spawn options that exist for the manageability rules and for deterministic
tests:

| | |
|---|---|
| `--x --y` | Exact top left corner instead of the staggered placement. Taken as given, off the monitor included. Both or neither. |
| `--w --h` | Exact size in physical pixels. Both or neither. |
| `--min-size W H` | The size the window defends, on `WM_GETMINMAXINFO` against a user drag and on `WM_WINDOWPOSCHANGING` against the `SetWindowPos` a tiling manager uses. Without the flag nothing is defended and a layout may make the window as small as it likes; with it the window simulates an application that refuses to shrink. |
| `--owned` | Each window becomes an owned popup: a hidden owner window of the class `MochiTestOwnerWindow`, never shown and never listed, owns the visible one. The usual manageability rules skip a window with an owner. |
| `--no-title` | The windows are created with an empty title, which the usual manageability rules also skip. |

`spawn` waits until every window of the batch is visible and its extended frame
bounds have stopped moving before it returns or prints, so the first thing a
test reads is never a half created window.

`cloak` and `alpha` drive from the command line the two states a window manager
sets from the outside, so a manual session can put a window into the same state
a test does:

| | |
|---|---|
| `cloak --on` / `--off` | `DWMWA_CLOAKED`: the window disappears from the screen while `IsWindowVisible` still says true. This is how a manager hides a workspace, and `list` reports it as `cloaked`. |
| `alpha --value N` | A transparency from 0, invisible, to 255, opaque. The window becomes layered, and `list` reports the value as `alpha`. |
| `alpha --clear` | Takes the transparency away again, layered bit included, so `alpha` reads `null`. |

Beside the state a window is in, `list` reports what a tiling test asserts on:

| Field | Source |
|---|---|
| `visible` | `IsWindowVisible` |
| `minimized` | `IsIconic` |
| `cloaked` | `DWMWA_CLOAKED`, how a manager hides a workspace |
| `foreground` | `GetForegroundWindow() == hwnd` |
| `alpha` | The layered alpha, `null` when nothing made the window layered |
| `owner` | `GetWindow(GW_OWNER)`, `null` for an ordinary top level window |

Every command prints JSON on stdout:

* `list` and `monitors` print a pretty JSON array.
* The single-window commands print `{"ok":true,"command":…,"window":{…}}`,
  with the state of the window *after* the call, so a script can check the
  effect without a second invocation.
* Failures print `{"ok":false,"error":"…"}` on stderr and exit with 1.

### How the two invocations talk to each other

`spawn` becomes the **host**: one process, one thread and one message loop per
window, alive until `close --all` or Ctrl+C. A second invocation reaches it two
ways, on purpose:

* Over the **named pipe** `\\.\pipe\mochi-testbed` for the things only the host
  can do: `spawn` into the running process and `close --all`, which shuts the
  host down. One request is one line of JSON, one response is one line of JSON.
  The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a
  second host refuses to start rather than serving half the requests. The
  listener does nothing but accept: every connection is handed to a short-lived
  worker thread, so a client that connects and then says nothing stalls only
  itself and the next client is still served.
* Straight through **Win32 on the window handle** for everything else: `list`,
  `move`, `resize`, `focus`, `minimize`, `restore`, `rename` and
  `close --hwnd`. Those are cross-process calls by nature and are exactly the
  calls a window manager makes, so routing them through a pipe would only test
  the pipe. It also means they work against windows this shell never spawned.

`%TEMP%\mochi-testbed\session.json` records the host pid, the pipe name and the
class. It is how a second invocation tells a live host from a dead one without
blocking on a connect; a file left behind by a crashed host fails the ping and
is deleted. Nothing is ever written next to the repository.

`focus` is best effort: Windows refuses `SetForegroundWindow` for a process
that does not own the foreground, which a console usually does not. The window
is raised either way and the JSON says `"focused": false` when the foreground
change was refused.

## Event stream

While the host runs it prints one JSON object per line. Three shapes, told
apart by their first key:

```jsonc
// a window event, on WM_WINDOWPOSCHANGED and at spawn, DPI change and destroy
{"event":"pos","hwnd":67852,"index":1,"title":"MochiTest 1",
 "rect":{"left":48,"top":48,"right":1128,"bottom":768},
 "frame":{"left":57,"top":48,"right":1119,"bottom":759},
 "minimized":false,"dpi":144,"ts_ms":1789500047468}

// host lifecycle
{"host":"ready","pid":15192,"pipe":"\\\\.\\pipe\\mochi-testbed","class":"MochiTestWindow","windows":[67852,67860]}
{"host":"exit","pid":15192,"reason":"close all or Ctrl+C"}

// a command result, from the other subcommands
{"ok":true,"command":"move","window":{ … }}
```

`event` is one of `spawned`, `pos`, `dpi`, `closed`. `rect` is `GetWindowRect`,
the invisible border included; `frame` is `DWMWA_EXTENDED_FRAME_BOUNDS`, what
the user perceives. The difference between the two is the amount a tiling
manager has to compensate for: on this machine it is 9 physical pixels left,
right and bottom at 144 DPI, and 0 at the top. `ts_ms` is Unix milliseconds, so
a test can measure how long a retile took.

A test window never repositions itself, not even on `WM_DPICHANGED`, so every
rect change in the stream came from the window manager under test.

## A manual tiling session

```powershell
# 1. Where can the windows go?
mochi-testwin monitors

# 2. Four windows on the main monitor. This blocks and prints the stream.
mochi-testwin spawn --count 4 --monitor 0

# 3. In a second shell, tile exactly those windows.
mochi --manage-class MochiTestWindow

# 4. In a third shell, watch or poke at them.
mochi-testwin list
mochi-testwin close --hwnd 0x10914      # does the layout close the gap?
mochi-testwin minimize --hwnd 0x1090c   # does the slot survive?

# 5. Done.
mochi-testwin close --all
```

Every rect change appears as a `pos` line in the shell from step 2 as it
happens; step 4 is only for the questions the stream does not answer.

## From a Rust test

```rust
use std::time::Duration;
use mochi_testbed::{TestWindows, layout_assert};

let windows = TestWindows::spawn(2, 0)?;          // Drop closes them, panic included
let first = windows.handles()[0];
windows.wait_for_frame(first, |r| r.width() > 0, Duration::from_secs(5))?;

let frames: Vec<_> = windows.windows().iter().map(|w| w.frame).collect();
let work_area = mochi_testbed::monitor_at(0)?.work_area;
layout_assert::assert_no_overlap(&frames);
layout_assert::assert_all_within(&frames, work_area);
layout_assert::assert_covers(&frames, work_area, 12);   // 12 px of seams allowed
```

`layout_assert` has a `check_*` form of each assertion that returns the
`Violation` instead of panicking, for a test that wants to retry while the
layout settles. Feed them extended frame bounds, not window rects: the raw
rects of two perfectly tiled windows always overlap by the invisible border.

`tests/test_windows.rs` is the worked example and runs on any desktop.

## Caveats

* **Tool windows are not normal windows.** They have no taskbar button and no
  Alt-Tab entry, and a manager that filters on `WS_EX_TOOLWINDOW` drops them.
  That is the point, but it also means a green tiling run here does not prove
  that the daemon's manageability rules are right for real applications.
* **They do not fight back.** A test window accepts any size down to 120x80,
  ignores `WM_DPICHANGED` and never moves itself. Real applications have
  minimum sizes, snap their client area to a character grid, and some of them
  reposition themselves; none of that is exercised here.
* **DPI.** The binary and the test process switch themselves to per-monitor v2
  awareness before the first window is created, because they have no embedded
  manifest of their own. Under a debugger that re-launches the executable, or
  from a host process that is already DPI unaware, `GetWindowRect` will report
  scaled coordinates and every number in the stream will be wrong. All sizes
  are physical pixels and scale with the monitor: 720 logical pixels is 1080
  physical at 144 DPI.
* **Monitor indices** are `EnumDisplayMonitors` order, which is neither the
  Windows display number nor the order in the Settings app, and it changes when
  a monitor is attached, detached or woken. Run `mochi-testwin monitors` and use
  the index it prints rather than assuming 0 is the main display.
* **Ctrl+C** closes the windows through the normal path. `Ctrl+Break` and a
  console window closed with the mouse do the same, but Windows gives the
  handler only a few seconds; killing the process instead is still safe, since
  the windows die with it.
* **Elevation.** Everything here runs unelevated. A daemon running elevated can
  move these windows, but not the other way round.

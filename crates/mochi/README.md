# mochi

The daemon. One thread owns all state; everything else is a producer on a single
`std::sync::mpsc` channel, so there is not a lock in the crate.

```
main.rs          argument parsing, startup order, shutdown order, Ctrl-C
cli.rs           --dry-run, --config, --hotkeys, --no-hotkeys, --manage-class
logging.rs       stderr (RUST_LOG, default info) + %LOCALAPPDATA%\mochi\mochi.log, daily rotation
single_instance.rs   named mutex Local\mochi-single-instance
safety.rs        panic hook, RestoreGuard, the restore hook the daemon installs
config.rs        path resolution for mochi.json and the hotkey file, reading them plus applications.json, quickstart stub, file watcher
state.rs         the session facts, Settings, and snapshot(), the JSON `mochic state` prints
wm.rs            the event loop, the mochi-core model, command handling, Changes to Win32

platform/
  mod.rs         the Platform trait, WindowPlacement, ZOrder, ShowState
  types.rs       Hwnd, MonitorId, MonitorInfo, WindowInfo, style bits, is_manageable
  win32.rs       the real implementation, plus compensate_invisible_border
  dry_run.rs     reads through, writes become log lines
  dpi.rs         SetProcessDpiAwarenessContext fallback behind the manifest
  wide.rs        UTF-16 helpers

events/
  mod.rs         the Event enum, WindowEventKind, Reply
  winevent.rs    SetWinEventHook on its own thread with a message loop
  message_window.rs  hidden top-level window: display, work area, DPI, session, power
  mouse.rs       focus-follows-mouse, polling by default, WH_MOUSE_LL behind `mouse-hook`
  hotkey.rs      WH_KEYBOARD_LL on its own thread: the bindings, the gate, the shell spawn

ipc/
  mod.rs
  server.rs      \\.\pipe\mochi, one thread per connection
  subscribers.rs notification fan-out on its own thread
```

## Running it

```
cargo run -p mochi -- --dry-run          # safe next to another window manager
RUST_LOG=debug cargo run -p mochi        # every window event on stderr
mochic state                             # what it can see
mochic stop                              # clean shutdown
```

`--dry-run` keeps every read and turns every write into an `info` log line. It
is the only safe way to run Mochi while something else is managing the desktop.

## Why the message window is not a message-only window

`WM_DISPLAYCHANGE` and `WM_SETTINGCHANGE` are broadcast to top-level windows.
A `HWND_MESSAGE` child never receives them. So `events/message_window.rs`
creates a real top-level window that is zero sized, `WS_EX_TOOLWINDOW` and never
given `WS_VISIBLE`. It has no pixels, no taskbar button, and Mochi's own
`is_manageable` skips tool windows, so it cannot tile itself.

## DPI

`build.rs` embeds a manifest with `PerMonitorV2`. That is what makes
`GetWindowRect` return real pixels on a mixed DPI desktop, and it has to happen
before the first window exists, so the manifest is the real mechanism.
`platform::dpi::ensure_per_monitor_v2` calls `SetProcessDpiAwarenessContext` as
a fallback for the case where the manifest is lost, and logs what it ended up
with.

## How a turn works

`wm.rs` owns one `mochi_core::State`. Everything follows the same three steps:

1. an event or a command becomes one call on the model,
2. the model hands back a `Changes`,
3. `apply_changes` turns that into Win32 calls, in this order: hide, restore,
   retile, show, minimize, maximize, close, focus, warp the mouse.

`restore` comes before the retile on purpose: a window that is still maximized
cannot be given a tile, and restoring it afterwards would undo the move.

| Piece | Where | What it does |
|---|---|---|
| the model | `wm.rs`, field `core` | monitors, workspaces, containers, windows; every command is a method on it |
| `apply_changes` | `wm.rs` | the only place a `Changes` becomes a window move |
| `apply_layout` | `wm.rs` | batches into `DeferWindowPos`, honours pause and `--dry-run` |
| `Hidden` | `wm.rs` | every window Mochi took off screen, and how; the list `restore` works from |
| `wm::restore` | `wm.rs` | uncloaks, shows or un-minimizes all of them and clears any alpha |
| `safety::set_restore_hook` | `safety.rs` | installed once, runs from `Drop` and from the panic hook |
| `platform::Platform` | `platform/mod.rs` | the whole Win32 surface, real and dry-run |
| `platform::types::is_manageable` | `platform/types.rs` | the static half of the decision; the config rules are applied on top |

Every command in `docs/cli.md` reaches the model. `border`, `border-width`,
`border-offset`, `border-style`, `border-colour`, `toggle-transparency`,
`animation`, `animation-duration`, `animation-style` and `animation-fps` all
show up in `mochic state` under `settings`, and the *configuration file* keys
of the same name are drawn by [`visuals`](src/visuals.rs): borders, unfocused
transparency and move/resize animation. Each of those commands takes effect
where it lands: it writes the live configuration, hands it back to the managers
and redraws the workspace, with no reload in between. There is no stackbar and
`visuals` never builds one: Mochi draws borders and nothing else, on purpose.

`visuals` owns an optional `mochi-render` `BorderManager`, `TransparencyManager`
and `Animator`, one per setting, created only while that setting is on. It
translates the daemon's own `Hwnd`/`mochi_core::Rect` into the render crate's
`WindowHandle`/`Rect` (the same type) and decides the desired end state; every
Win32 call stays inside `mochi-render`. `WindowManager::apply_workspace` calls
it after every retile with the focused window, every visible window's rect and
`BorderKind` (`Single`, `Stack`, `Monocle`, `Floating` or `Unfocused`), and the
unfocused set; `WindowManager::apply_layout` routes position changes through it
too, so an animated move is one `Platform::set_positions` batch per frame with
borders following through `BorderManager::follow_frame`. Pausing, reload and
stop all call `Visuals::clear`/`Visuals::stop`, which destroy the border frames
and put every faded window back to opaque; any window `TransparencyManager`
fades is mirrored into `Hidden` with `Hidden::fade` (and un-mirrored with
`Hidden::unfade` once it is put back), so a crash mid-fade is still undone by
`wm::restore` and the panic hook.

Manual check against real windows, since a border cannot be asserted from the
testbed: start the daemon with `--manage-class MochiTestWindow` and a config
with `border`, `transparency` and `animation` all on, spawn a few
`mochi-testwin` windows, and confirm with a screenshot that the focused window
has a rounded pink border and the rest a dark one, that `mochi-testwin list`
reports a non-null `alpha` on every unfocused window and none on the focused
one, and that `mochic stop` leaves every window with no alpha and no border.

## Testing against real windows

`--manage-class <CLASS>` makes the daemon manage exactly the given classes and
ignore every other window on the desktop. It is the only switch that lifts the
tool window rejection, which is what `crates/mochi-testbed` needs, and it is the
only safe way to exercise the tiling path on a machine where another window
manager and the user's real applications are running:

```
mochi-testwin spawn --count 4 --monitor 0
mochi --manage-class MochiTestWindow --config %TEMP%\mochi-e2e\mochi.json
mochic state
```

A window that is cloaked at startup and would otherwise be managed is uncloaked
first: a daemon killed with `taskkill /F` never runs its restore hook, and its
windows have to come back on the next start.

## The end-to-end tests

`tests/e2e_testbed.rs` starts the real daemon against real windows spawned by
`mochi-testbed`. It needs an interactive desktop, so it is opt-in and skips
with a message everywhere else: without `MOCHI_E2E=1`, or on a session that
enumerates no monitor, or when the `mochi` binary was not built.

```
set MOCHI_E2E=1
set MOCHI_E2E_CONFIG=%TEMP%\mochi-e2e\mochi.json   # optional, one is written if absent
cargo test -p mochi --test e2e_testbed -- --test-threads 1 --nocapture
```

`--test-threads 1` is not optional: there is one daemon, one named pipe and one
desktop. For the same reason nothing else may be spawning `MochiTestWindow`
windows at the same time; another testbed run on the same machine is adopted by
the daemon under test and every count assertion drifts.

Each test writes `RUST_LOG=debug` to `%TEMP%\mochi-e2e\mochi-*.log` and names
that file in its failure message. Every step is reported on its own line, so one
run tells you about all of them rather than stopping at the first surprise.

## The hotkeys

`events/hotkey.rs` owns a `WH_KEYBOARD_LL` hook on its own thread and matches
every key press against the [`mochi-hotkey`](../mochi-hotkey) bindings the
daemon read from the hotkey file. A match is swallowed, its release with it, and
sent to the loop as `Event::Hotkey`; everything else goes straight on.

Two rules the hook lives by. The callback runs inside the raw input path for
every key on the desktop, so it does one hash lookup and one non-blocking send
and nothing else: Windows removes a hook whose callback overruns
`LowLevelHooksTimeout`. And nothing is ever withheld from the desktop that did
not match a binding exactly, because the failure mode of getting that wrong is a
keyboard that eats keys.

The bindings live in thread-local storage on that thread and a reload is posted
to it as a thread message, so there is nothing to lock. The one exception is the
gate, which decides whether all bindings are live, only the one that leaves game
mode, or none: that is a single atomic, because `mochic set-hotkeys disable` has
to be in force by the time it answers, not once a thread got round to a message.

```
mochi --no-hotkeys                       # bind nothing, for a second daemon
mochi --hotkeys %TEMP%\keys             # bind a scratch file instead
mochic hotkeys                           # what it made of the file
```

The end-to-end test injects real key presses on F13 to F16. No keyboard has
those keys and no layout produces them, which is what makes it safe to run on a
desktop someone is sitting at; no test in this crate may ever press a key a
person or another program could mean.

## Protocol

See [`docs/ipc.md`](../../docs/ipc.md).

# mochi

The daemon. One thread owns all state; everything else is a producer on a single
`std::sync::mpsc` channel, so there is not a lock in the crate.

```
main.rs          argument parsing, startup order, shutdown order, Ctrl-C
cli.rs           --dry-run, --config, --manage-class
logging.rs       stderr (RUST_LOG, default info) + %LOCALAPPDATA%\mochi\mochi.log, daily rotation
single_instance.rs   named mutex Local\mochi-single-instance
safety.rs        panic hook, RestoreGuard, the restore hook the daemon installs
config.rs        config path resolution, reading mochi.json plus applications.json, quickstart stub, file watcher
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

Every command in `docs/cli.md` reaches the model. What is still only *stored*:
`border`, `border-width`, `border-offset`, `border-style`, `border-colour`,
`toggle-transparency`, `animation`, `animation-duration`, `animation-style` and
`animation-fps`. They show up in `mochic state` under `settings`; nothing draws
them until milestone 5.

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

## Protocol

See [`docs/ipc.md`](../../docs/ipc.md).

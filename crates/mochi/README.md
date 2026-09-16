# mochi

The daemon. One thread owns all state; everything else is a producer on a single
`std::sync::mpsc` channel, so there is not a lock in the crate.

```
main.rs          argument parsing, startup order, shutdown order, Ctrl-C
cli.rs           --dry-run, --config
logging.rs       stderr (RUST_LOG, default info) + %LOCALAPPDATA%\mochi\mochi.log, daily rotation
single_instance.rs   named mutex Local\mochi-single-instance
safety.rs        panic hook, RestoreGuard, the restore_all hook point
config.rs        config path resolution, quickstart stub, notify-based file watcher
state.rs         State, TrackedWindow, Settings, the JSON `mochic state` prints
wm.rs            the event loop, command handling, the tiling hook points

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

## Hook points for mochi-core

Everything below is written, logged and unit tested; the bodies that need a
monitor / workspace / container tree say so and do nothing else.

| Hook | Where | What it should become |
|---|---|---|
| `WindowManager::on_window_event` | `wm.rs` | feed the tree: add on show/uncloak, remove on destroy/cloak, re-home on move |
| `WindowManager::retile` | `wm.rs` | ask `mochi-core` for the rectangles of the visible workspace on each monitor, then call `apply_layout` |
| `WindowManager::apply_layout` | `wm.rs` | already complete: batches into `DeferWindowPos`, honours pause and `--dry-run` |
| `WindowManager::reload_config` | `wm.rs` | parse `mochi.json` with `mochi-core`, apply it, retile |
| `WindowManager::cloaked` | `wm.rs` | push every handle Mochi cloaks, remove it on uncloak |
| `WindowManager::restore_all` | `wm.rs` | already complete: uncloaks everything in `cloaked()` |
| `safety::set_restore_hook` | `safety.rs` | installed once by `install_restore_hook`, runs from `Drop` and from the panic hook |
| `wm::pending` | `wm.rs` | the single place that answers "not wired up yet"; delete a match arm from `handle_command` as each command lands |
| `platform::Platform` | `platform/mod.rs` | the whole Win32 surface, already implemented for real and for dry runs |
| `platform::types::is_manageable` | `platform/types.rs` | static half of the decision; user rules from the config are applied on top |
| `state::State` | `state.rs` | the flat `windows` map is what the tree replaces |

The command handlers that still return `pending` are: `focus`, `move`,
`resize-axis`, `promote`, `toggle-float`, `toggle-maximize`, `toggle-monocle`,
`minimize`, `close`, `manage`, `unmanage`, `stack`, `unstack`, `cycle-stack`,
`cycle-layout`, `change-layout`, `flip-layout`, `focus-workspace`,
`move-to-workspace`, `cycle-workspace`, `focus-last-workspace`,
`workspace-padding`, `container-padding`, `focus-monitor`, `move-to-monitor`,
`cycle-monitor`, `border-colour`, `border-style`, `animation-style`,
`float-rule`, `ignore-rule`, and the three workspace-shaped `query` targets.

## Protocol

See [`docs/ipc.md`](../../docs/ipc.md).

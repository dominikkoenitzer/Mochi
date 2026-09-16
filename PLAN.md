# Plan

Goal: a full featured tiling window manager for Windows in Rust, built from scratch.
No code is taken from any existing window manager.

## Decisions

- Name: Mochi. Crates `mochi`, `mochic`, `mochi-core`, `mochi-client`.
- License: none yet, all rights reserved. Repo private for now.
- Config: JSON, key names follow the common tiling window manager conventions so an existing config imports with a rename.
- Hotkeys: whkd for now, own daemon later. whkdrc bindings point at `mochic`.

## Architecture

- `mochi-core` is platform independent. Monitor > Workspace > Container > Window,
  each level a ring with a focus index. Containers are stacks. Floating list per
  workspace. Layouts are pure functions from area, count and resize deltas to rects.
- `mochi` runs one `WindowManager` thread that consumes a single channel. Producers:
  SetWinEventHook (show, destroy, focus, move, minimize, cloak), a hidden message
  window for display and DPI changes, and the IPC named pipe. No shared locks.
- Windows are hidden by DWM cloaking, positioned with DeferWindowPos, frame borders
  compensated via extended frame bounds. Per monitor DPI awareness in the manifest.
- Rules: ignore, float and manage on exe, class, title and path with Equals, Contains,
  StartsWith, EndsWith and Regex.
- Visuals (borders, transparency, animations) are separate modules so tiling works
  without them.
- Safety net: every managed window is uncloaked and restored on exit and in the
  panic hook. A crash that leaves windows hidden is the worst failure mode.

## Milestones

1. Scaffold. Workspace, CI, private repo, README. Done.
2. Observe. Enumerate monitors and windows, log every WinEvent, `mochic state`
   prints JSON. Moves nothing, safe to run next to another window manager. Done.
3. Tile. Core model, BSP first, rules, apply layout to real windows, restore on exit.
   From here only one window manager runs at a time. Done. Missing: the daemon
   never exercises the tiling path against real windows in CI, so the end to end
   run with `--manage-class MochiTestWindow` is still a manual step.
4. Control. IPC and CLI for everything whkdrc uses: focus, move, resize axis,
   workspaces, monitors, float, maximize, monocle, minimize, close, cycle and flip
   layout, retile, pause, reload, stop. Done. Missing: `stack`, `unstack` and
   `cycle-stack` are wired but have no stackbar, so a stacked container is only
   visible in `mochic state`.
5. Polish. Borders, transparency and animations are wired: `mochi/src/visuals.rs`
   owns an optional `mochi-render` `BorderManager`, `TransparencyManager` and
   `Animator`, translates the daemon's `Hwnd`/`Rect` into the render crate's own
   types, and the window manager drives it after every retile and on pause,
   reload and stop. Borders follow focus with `Single`/`Stack`/`Monocle`/
   `Floating`/`Unfocused`, transparency fades exactly the unfocused managed
   windows, and animated moves batch through `Platform::set_positions` with
   borders following each frame. Verified against real testbed windows with the
   user's own rice settings (pink `#ffbbdf` focused border, dark `#313244`
   unfocused, 235 alpha, 250 ms EaseOutQuad). No stackbar by default and none is
   built by this module: Mochi draws borders and nothing else, on purpose.
   Missing: cross monitor move behaviour, game mode; individual `border`/
   `transparency`/`animation-*` CLI commands still only update `mochic state`
   and take effect on the next config reload rather than immediately.
   Later: stackbar (opt-in, off by default), event subscriptions for bars, own
   hotkey daemon, releases.

## Layouts to support

BSP, Columns, Rows, VerticalStack, HorizontalStack, UltrawideVerticalStack, Grid.

## Known limits

- Elevated windows cannot be managed from a non admin process.
- Mixed DPI (4K main plus 1080p portrait) is the primary test bed.
- Electron, UWP and game windows each have quirks; ignore rules and a cloak check
  handle most of them.

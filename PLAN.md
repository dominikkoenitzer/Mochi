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
   From here only one window manager runs at a time.
4. Control. IPC and CLI for everything whkdrc uses: focus, move, resize axis,
   workspaces, monitors, float, maximize, monocle, minimize, close, cycle and flip
   layout, retile, pause, reload, stop. Then swap whkdrc over and daily drive it.
5. Polish. Borders, transparency, animations, cross monitor move behaviour, game mode.
   Later: stackbar, event subscriptions for bars, own hotkey daemon, releases.

## Layouts to support

BSP, Columns, Rows, VerticalStack, HorizontalStack, UltrawideVerticalStack, Grid.

## Known limits

- Elevated windows cannot be managed from a non admin process.
- Mixed DPI (4K main plus 1080p portrait) is the primary test bed.
- Electron, UWP and game windows each have quirks; ignore rules and a cloak check
  handle most of them.

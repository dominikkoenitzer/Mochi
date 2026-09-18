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
   layout, retile, pause, reload, stop. Done. `stack`, `unstack` and
   `cycle-stack` have no stackbar by design, so the proof that they work is a
   testbed run: the stack shows one window on the container's tile, cycling
   brings the other one forward onto that same tile, unstack gives both their
   own again. A stack in a direction with no container there is a silent no-op,
   like every other movement command.
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
   The `border*`, `transparency` and `animation-*` commands apply the moment
   they land: each one writes the live configuration, hands it back to the
   managers and redraws the workspace, proven end to end by a testbed window
   fading from one `toggle-transparency` with no reload in between.
   Cross monitor moves, an unplugged screen and a screen that comes back are
   covered against a simulated second monitor at a different DPI, including the
   rule that a window is never lost with the screen it was on; they have still
   not run against real hardware, since the portrait screen was detached.
   Game mode is covered too: the script pauses tiling, writes a minimal hotkey
   config holding nothing but the toggle, and resumes. The whkd restart inside
   it is the one step no test takes, because the user's own hotkey daemon is
   running while the tests are.
   Later: a hotkey daemon of Mochi's own, so whkd is no longer needed, and
   releases. Never: a stackbar, or any other strip drawn above a window.
   Subscriptions already exist for anything that wants to follow the state.

## Layouts to support

BSP, Columns, Rows, VerticalStack, HorizontalStack, UltrawideVerticalStack, Grid.

## Known limits

- Elevated windows cannot be managed from a non admin process. One that refuses
  to move no longer takes the rest of the layout with it: the batch falls back
  to one window at a time and reports only the windows it could not place.
- Mixed DPI (4K main plus 1080p portrait) is the primary test bed.
- UWP windows belong to `ApplicationFrameHost.exe`, which would make every UWP
  app the same program to an `exe` rule. `read_window` walks the host's children
  for the process the application actually runs in, so Calculator reports
  `CalculatorApp.exe`.
- An application that defends a minimum size gets the tile it cannot fill; it
  overflows its neighbour rather than shrinking, and the daemon leaves it there
  instead of fighting it.
- Electron and game windows each have quirks; ignore rules and a cloak check
  handle most of them.

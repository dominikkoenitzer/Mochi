# Plan

Goal: a full featured tiling window manager for Windows in Rust, built from scratch.
No code is taken from any existing window manager.

## Decisions

- Name: Mochi. Crates `mochi`, `mochic`, `mochi-core`, `mochi-client`,
  `mochi-hotkey`, `mochi-render`.
- License: none yet, all rights reserved. Repo private for now.
- Config: JSON, key names follow the common tiling window manager conventions so an existing config imports with a rename.
- Hotkeys: Mochi's own, in process. The file format follows the same
  conventions, so an existing hotkey file is read as it stands.

## Architecture

- `mochi-core` is platform independent. Monitor > Workspace > Container > Window,
  each level a ring with a focus index. Containers are stacks. Floating list per
  workspace. Layouts are pure functions from area, count and resize deltas to rects.
- `mochi` runs one `WindowManager` thread that consumes a single channel. Producers:
  SetWinEventHook (show, destroy, focus, move, minimize, cloak), a hidden message
  window for display and DPI changes, a `WH_KEYBOARD_LL` hook for the hotkeys,
  and the IPC named pipe. No shared locks: the hook thread owns its bindings and
  is given new ones by a thread message, not by sharing them.
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
4. Control. IPC and CLI for everything a hotkey file binds: focus, move, resize axis,
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
   Game mode moved into the daemon with the hotkeys, see milestone 6; the
   script that used to fake it is gone.
   Never: a stackbar, or any other strip drawn above a window. Subscriptions
   already exist for anything that wants to follow the state.
6. Hotkeys. Done. Mochi binds its own keys and nothing else is needed to drive
   it. `mochi-hotkey` parses the file and answers a key press with one hash
   lookup; the daemon owns a `WH_KEYBOARD_LL` hook on its own thread and turns a
   match straight into the command, with no process spawned per press. The
   grammar on the right of the colon is the `mochic` grammar itself, taken from
   `mochi-client`, so a binding cannot drift away from the command line it was
   copied from, and a `mochic` prefix is optional. A press is swallowed only on
   an exact match, and its release with it; AltGr is not read as Ctrl+Alt, so a
   Swiss or German layout keeps its `@` and its brackets. One bad line costs
   only itself and is reported by `mochic hotkeys` with its line number. Saving
   the file rebinds at once, and the reload reinstalls the hook, which is the
   answer to a hook Windows dropped for overrunning its timeout.
   Game mode is one command rather than a script: `toggle-game-mode` pauses
   tiling and suspends every binding but its own. The gate is the whole state,
   so a daemon killed in game mode comes back normal instead of in a state
   nobody can leave.
   Proven end to end by injecting real key presses on F13 to F16, keys no
   keyboard has and no layout produces: a bound key acts, an unbound one and a
   bound one with an extra modifier do not, game mode suspends and restores the
   set, a shell binding starts its program, saving the file rebinds, a broken
   line costs only itself, and `set-hotkeys disable` and `enable` turn the whole
   thing off and on.

## Layouts to support

BSP, Columns, Rows, VerticalStack, HorizontalStack, UltrawideVerticalStack, Grid.

## Known limits

- Elevated windows cannot be managed from a non admin process. One that refuses
  to move no longer takes the rest of the layout with it: the batch falls back
  to one window at a time and reports only the windows it could not place. The
  same boundary applies to the keyboard: a key pressed over an elevated window
  is not delivered to a hook in a process that is not.
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

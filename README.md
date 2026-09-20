# Mochi

A tiling window manager for Windows, written in Rust.

Mochi is a clean-room project. It takes its feature set from the tiling window
managers that came before it, but shares no code with any of them.

## Layout

| Crate | Purpose |
|---|---|
| `mochi-core` | Pure logic: geometry, monitor/workspace/container tree, layouts, rules. No Win32, fully unit tested. |
| `mochi-client` | IPC wire types and the command grammar, shared by the daemon, the CLI and external tools. |
| `mochi-hotkey` | The hotkey file: key names, parsing, lookup. No Win32 either. |
| `mochi-render` | Borders, transparency and animation. |
| `mochi` | The daemon. One thread owns all state and talks to Win32. |
| `mochic` | Command-line client. |
| `mochi-testbed` | Throwaway windows the end-to-end tests drive. |

## Status

All five milestones are done, see [PLAN.md](PLAN.md). Mochi tiles real windows
and binds its own keys, and the proof is a test suite that drives throwaway
windows and injects real key presses on a real desktop rather than only checking
a model in memory. CI runs it: the end to end job fails if those tests skip,
because a test that returns early still reports as passed.

What is still owed is real hardware for the cross monitor and unplugged screen
paths, which have only ever run against a simulated second monitor, and time on
a desktop someone actually works in. The first supervised trial found a defect
no test could have: a window Mochi was hiding could be forgotten without being
uncloaked, and the shell cloak that every real application takes had never once
run in a test, because a tool window is given no application view and every
cloak in every test fell back to hiding. Both are fixed and both are covered
now, which is the argument for running it rather than only testing it.

What works: BSP, columns, rows, the two stacks, ultrawide and grid layouts;
focus, move and resize by direction; workspaces and monitors, including a screen
that is unplugged and comes back; float, monocle, maximize and minimize; stack,
unstack and cycle-stack; ignore, float and workspace rules; borders,
transparency and animated moves; hotkeys, pause, reload, and a game mode that
gives a game every key. Every visual setting has a command, and a command takes
effect the moment it lands rather than on the next reload.

Mochi draws borders and nothing else. There is no bar, no tab strip and none is
planned.

## Hotkeys

Mochi binds keys itself, in process. There is no second program to install and
no subprocess per key press.

```
alt + h                 : focus left
alt + shift + q         : close
alt + shift + g         : toggle-game-mode
```

The file lives at `%USERPROFILE%\.config\mochi\hotkeys` and is reloaded the
moment it is saved. `mochic hotkeys` prints what Mochi made of it. Game mode is
one command: it pauses tiling and suspends every binding except the one that
turns it off again, so the game in front gets the whole keyboard.

The full reference is [docs/hotkeys.md](docs/hotkeys.md).

## Build

```
cargo build --release
cargo test --workspace
```

## Install

Build and install for the current user, no admin rights needed:

```
.\scripts\install.ps1
```

This puts `mochi.exe` and `mochic.exe` in `%LOCALAPPDATA%\Programs\Mochi\bin`,
adds that folder to the user PATH and writes a default `%USERPROFILE%\mochi.json`
and hotkey file when there is none. `-Version v0.1.10` downloads that release
instead of building, `-Uninstall` reverses everything.

Start it at login, and see what is registered today:

```
.\scripts\autostart.ps1 -Enable
.\scripts\autostart.ps1
```

Config keys are in [docs/configuration.md](docs/configuration.md), commands in
[docs/cli.md](docs/cli.md), keys in [docs/hotkeys.md](docs/hotkeys.md). The log
is `%LOCALAPPDATA%\mochi\mochi.log`.

## Importing an existing setup

Config keys, command names and the hotkey file format follow the common tiling
window manager conventions, so an existing JSON config and hotkey file carry over
with a rename. `.\scripts\import-config.ps1` shows the diff and `-Apply` writes
the new files next to the old ones. Nothing the old setup owns is changed, and
two commands switch back at any time.

The whole path is in [docs/import.md](docs/import.md).

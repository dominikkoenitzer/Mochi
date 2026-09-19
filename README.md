# Mochi

A tiling window manager for Windows, written in Rust.

Mochi is a clean-room project. It takes its feature set from the tiling window
managers that came before it, but shares no code with any of them.

## Layout

| Crate | Purpose |
|---|---|
| `mochi-core` | Pure logic: geometry, monitor/workspace/container tree, layouts, rules. No Win32, fully unit tested. |
| `mochi-client` | IPC wire types shared by the daemon, the CLI and external tools. |
| `mochi` | The daemon. One thread owns all state and talks to Win32. |
| `mochic` | Command-line client, the thing your hotkey daemon calls. |

## Status

Milestones 1 to 4 are done, milestone 5 is not, see [PLAN.md](PLAN.md). Borders,
transparency and animations are in and working; what polish still owes is real
hardware for the cross monitor and unplugged screen paths, which so far have only
run against a simulated second monitor, and the whkd restart in game mode. Mochi
tiles real windows, and the proof is a test suite that drives throwaway windows
on a real desktop rather than only a model in memory: run it with `MOCHI_E2E=1`.

What works: BSP, columns, rows, the two stacks, ultrawide and grid layouts;
focus, move and resize by direction; workspaces and monitors, including a screen
that is unplugged and comes back; float, monocle, maximize and minimize; stack,
unstack and cycle-stack; ignore, float and workspace rules; borders,
transparency and animated moves; pause, reload, and a game mode that gives a
game every key. Every visual setting has a command, and a command takes effect
the moment it lands rather than on the next reload.

Mochi draws borders and nothing else. There is no bar, no tab strip and none is
planned. A hotkey daemon of its own is the one piece still missing; whkd does
that job for now.

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
when there is none. `-Version v0.1.0` downloads that release instead of
building, `-Uninstall` reverses everything.

Start it at login, and see what is registered today:

```
.\scripts\autostart.ps1 -Enable
.\scripts\autostart.ps1
```

Config keys are in [docs/configuration.md](docs/configuration.md), commands in
[docs/cli.md](docs/cli.md). The log is `%LOCALAPPDATA%\mochi\mochi.log`.

## Importing an existing setup

Config keys and command names follow the common tiling window manager
conventions, so an existing JSON config and whkdrc carry over with a rename.
`.\scripts\import-config.ps1` shows the diff and `-Apply` writes the new files
next to the old ones. Nothing the old setup owns is changed, and two commands
switch back at any time.

The whole path is in [docs/import.md](docs/import.md).

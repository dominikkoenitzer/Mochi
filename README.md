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

Milestones 1 and 2 of 5 are done, see [PLAN.md](PLAN.md): the daemon observes
monitors, windows and events and answers over IPC, but nothing manages windows yet.

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

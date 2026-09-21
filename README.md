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
alt + shift + q         : close the focused window
alt + shift + e         : stop mochi and give the desktop back
```

**To turn tiling off and on, press `Pause`.** One key. Off means Mochi stops
touching windows and leaves them exactly where they are; on puts them back in
their tiles. The daemon keeps running either way, which is why the key still
works while it is off. `alt + f12` does the same on a keyboard with no Pause
key.

**If you want out for good, press `alt + shift + e`.** It stops Mochi, puts every window
it was hiding back, takes the borders down and unbinds the keys. Nothing about
the desktop is left changed. It is worth knowing before you start it for the
first time, because a tiling window manager rearranges every window on screen
the moment it comes up.

The arrow keys are deliberately left alone. `alt + left` and `alt + right` are
Back and Forward in every browser, and `alt + up` is the parent folder in
Explorer; a window manager that binds them takes those away everywhere with
nothing on screen to say why. The shipped file says how to add them if you
would rather have them.

The file lives at `%USERPROFILE%\.config\mochi\hotkeys` and is reloaded the
moment it is saved. `mochic hotkeys` prints what Mochi made of it. Game mode is
one command: it pauses tiling and suspends every binding except the one that
turns it off again, so the game in front gets the whole keyboard.

The full reference is [docs/hotkeys.md](docs/hotkeys.md).

## Build

On a machine that has never built Rust for Windows you need two things, and
neither is Mochi's to install: [rustup](https://rustup.rs), and the Microsoft
C++ build tools, which is the "Desktop development with C++" workload in the
Visual Studio Installer. Rust on Windows links with `link.exe` and rustup does
not bring it. Without it the build stops at `error: linker 'link.exe' not
found`, which says nothing about what to go and install.

```
git clone https://github.com/dominikkoenitzer/Mochi
cd Mochi
cargo build --release
cargo test --workspace
```

The first build takes a few minutes. Every one after that is seconds.

## Install

Build and install for the current user, no admin rights needed:

```
.\scripts\install.ps1
```

If that fails before it prints anything, with "running scripts is disabled on
this system" or "is not digitally signed", that is Windows and not the script.
A repo downloaded as a zip arrives with every file marked as coming from the
internet, and Windows PowerShell 5.1 refuses to run scripts at all by default.
Either way this runs it without changing any setting on the machine:

```
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

**Open a new terminal afterwards.** The install adds its folder to the user
PATH, and a shell that is already running does not see a PATH that changed
under it. Without a new one the next command answers that `mochic` is not
recognized, which reads like the install failed when it did not.

This puts `mochi.exe` and `mochic.exe` in `%LOCALAPPDATA%\Programs\Mochi\bin`,
adds that folder to the user PATH and writes a default `%USERPROFILE%\mochi.json`
and hotkey file when there is none. `-Version v0.1.13` downloads that release
instead of building, `-Uninstall` reverses everything.

Start it at login, and see what is registered today:

```
.\scripts\autostart.ps1 -Enable
.\scripts\autostart.ps1
```

The Run value only fires at login, so a daemon that stops during the day
leaves the desktop untiled until the next one. `-Watchdog` registers a task
that asks every five minutes whether Mochi is up, and starts it when it is
not. `mochic start` answers that it is already running in about thirty
milliseconds, so the check costs nothing.

```
.\scripts\autostart.ps1 -Watchdog
```

## Running it

Look before you leap. A dry run reads the desktop and writes nothing at all: no
window is moved, cloaked, focused or closed, it takes no control pipe and it
binds no keys, so it is safe to run while another window manager, or another
Mochi, is managing the screen.

```
mochi --dry-run
```

Every line it prints starting `dry-run:` is something it would have done. When
that looks right, start it for real, and stop it when you want your desktop
back:

```
mochic start
mochic stop
```

`mochic stop` is the way out. It puts every window it was hiding back on screen,
takes the borders down, restores transparency and unbinds the keys, so stopping
Mochi leaves the desktop the way it found it. Nothing is left behind even if it
is killed outright: every window taken off screen is written down first, and the
next start puts back anything the record still owes. While it is running,
`mochic restore-windows` does the same on demand.

No admin rights, and none are wanted. Mochi runs as a normal user, which means
Windows will not let it move a window belonging to a program running as
administrator. It notices, says so once in the log, and leaves that window alone
rather than tiling around a hole it cannot fill.

When a window is not being tiled and it is not obvious why, ask:

```
mochic why
```

It explains what Mochi makes of the window in front and what can be done about
it, in words rather than in the terms the log is written in. Every answer it can
give names either a fix or the reason there is nothing to fix.

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

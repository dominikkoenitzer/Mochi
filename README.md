# Mochi

A tiling window manager for Windows, written in Rust.

Mochi is a clean-room project. It takes its feature set from the tiling window
managers that came before it, but shares no code with any of them.

## Install

Know the way out before you start. The moment Mochi starts, every window on
screen is rearranged into tiles. `Pause` turns tiling off and on again, or
`alt + f12` on a keyboard without a Pause key. `alt + shift + e` stops Mochi and
puts every window back. To start it again, open Mochi from the Start menu or run
`mochic start`.

Then open PowerShell (Start menu, type PowerShell, Enter) and paste this line:

```
irm https://github.com/dominikkoenitzer/Mochi/releases/latest/download/install.ps1 | iex
```

No admin rights are needed, and it works in Windows PowerShell 5.1 and
PowerShell 7.

It downloads the [latest release](https://github.com/dominikkoenitzer/Mochi/releases/latest)
and checks its SHA256. It puts `mochi.exe` and `mochic.exe` in
`%LOCALAPPDATA%\Programs\Mochi\bin` and adds that folder to your user PATH. If
you have no `%USERPROFILE%\mochi.json` or `%USERPROFILE%\.config\mochi\hotkeys`
yet, it writes a default one, and it never overwrites yours. It makes Mochi
start at every login with a HKCU Run value called Mochi, adds Mochi to the Start
menu, and then starts Mochi right away, which tiles every window on screen.

**Open a new terminal afterwards** before you type `mochic` yourself. A shell
that is already running does not see a PATH that changed under it, and answers
that `mochic` is not recognized, which reads like the install failed when it did
not.

### Options

The same line in this form takes switches:

```
& ([scriptblock]::Create((irm https://github.com/dominikkoenitzer/Mochi/releases/latest/download/install.ps1))) -SkipStart -SkipAutostart
```

`-SkipStart` installs without starting Mochi now. `-SkipAutostart` does not
register the login start. `-Version v0.1.21` installs that release instead of
the latest. `-SkipPath` leaves your PATH alone, and `-SkipQuickstart` does not
write the default `mochi.json` and hotkey file.

### Updating

Run the one line again. It refuses while Mochi is running, so stop Mochi first
with `alt + shift + e` or `mochic stop`.

### Watchdog

The Run value only fires at login, so a daemon that stops during the day leaves
the desktop untiled until the next one. `scripts\autostart.ps1` can register a
task that asks every five minutes whether Mochi is up, and starts it when it is
not. `mochic start` answers that it is already running in about thirty
milliseconds, so the check costs nothing. The script needs a clone of the
repository (see [Build from source](#build-from-source)), not a build:

```
powershell -ExecutionPolicy Bypass -File .\scripts\autostart.ps1 -Watchdog
```

With the watchdog on, `alt + shift + e` stops Mochi only until the next check:
the task starts it again within five minutes. `-Disable` removes the watchdog
along with the Run value, and that is the way out for good. Without a switch the
script shows what is registered today, and `-DisableStartupItem <name>` moves
another program's startup item aside until `-EnableStartupItem <name>` puts it
back.

## Uninstall

```
& ([scriptblock]::Create((irm https://github.com/dominikkoenitzer/Mochi/releases/latest/download/install.ps1))) -Uninstall
```

It stops Mochi, which puts every window back, then removes the login start, the
watchdog task if there is one, the Start menu entry, the binaries and the PATH
entry. Your `mochi.json`, the hotkey file and the log in `%LOCALAPPDATA%\mochi`
stay. Delete them by hand if you want them gone.

From a checkout:

```
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -Uninstall
```

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

`alt` plus an arrow is deliberately left alone. `alt + left` and `alt + right` are
Back and Forward in every browser, and `alt + up` is the parent folder in
Explorer; a window manager that binds them takes those away everywhere with
nothing on screen to say why. The shipped file says how to add them if you
would rather have them.

The file lives at `%USERPROFILE%\.config\mochi\hotkeys` and is reloaded the
moment it is saved. `mochic hotkeys` prints what Mochi made of it. Game mode is
one command: it pauses tiling and suspends every binding except the one that
turns it off again, so the game in front gets the whole keyboard. A `mode`
block holds keys that only work while that mode is on: `alt + shift + s`
enters the shipped resize mode, where `h`, `j`, `k` and `l` resize and `Esc`
leaves.

The full reference is [docs/hotkeys.md](docs/hotkeys.md).

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

## Status

All six milestones are done, see [PLAN.md](PLAN.md). Mochi tiles real windows
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
transparency and animated moves; hotkeys with binding modes, pause, reload, and
a game mode that gives a game every key. Every visual setting has a command, and a command takes
effect the moment it lands rather than on the next reload.

Mochi draws borders and nothing else. There is no bar, no tab strip and none is
planned.

## Build from source

On a fresh Windows PC, install Git, rustup and the Microsoft C++ build tools:

```
winget install --id Git.Git -e
winget install --id Rustlang.Rustup -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

The VCTools workload with its recommended components brings the MSVC linker and
the Windows SDK. Rust on Windows needs both and rustup does not bring them.
Without them the build stops at `error: linker 'link.exe' not found`, which says
nothing about what to go and install.

Open a new terminal so the new PATH is picked up, then:

```
git clone https://github.com/dominikkoenitzer/Mochi
cd Mochi
cargo build --release --locked
cargo test --workspace
```

`rust-toolchain.toml` pins the toolchain, so rustup fetches the right one on the
first `cargo` call. The first build takes a few minutes. Every one after that is
seconds. The tests are optional and safe to run on the desktop you work in: the
end to end tests, which drive real windows, only run when `MOCHI_E2E` is set.

To install what you built:

```
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1
```

It builds with cargo and installs the same way the one line does, login start
and first start included, and takes the same switches.

`-ExecutionPolicy Bypass` is there because Windows PowerShell 5.1 refuses to run
scripts by default, and a repo downloaded as a zip arrives with every file
marked as coming from the internet. Without it the script fails before it
prints anything, with "running scripts is disabled on this system" or "is not
digitally signed". It changes no setting on the machine, and the same goes for
the other scripts in `scripts\`. The one line under [Install](#install) is not
affected by execution policy, because it never runs a script file.

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

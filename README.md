# Mochi

[![CI](https://github.com/dominikkoenitzer/Mochi/actions/workflows/ci.yml/badge.svg)](https://github.com/dominikkoenitzer/Mochi/actions/workflows/ci.yml)

A tiling window manager for Windows, written in Rust: it arranges every open
window into tiles that you focus, move and resize from the keyboard.

## Download

[Download Mochi](https://get-mochi.vercel.app),
then double-click it.

Know the way out before you click Install. The moment Mochi starts, every window
on screen is rearranged into tiles. `Pause` turns tiling off and on again, or
`alt + f12` on a keyboard without a Pause key. `alt + shift + e` stops Mochi and
puts every window back. To start it again, open Mochi from the Start menu or run
`mochic start`.

Windows may say "Windows protected your PC"; click More info, then Run anyway.
No admin rights are needed. Setup installs Mochi for your user and starts it,
now and at every login. The PowerShell line and the zip are under
[Other ways to install](#other-ways-to-install).

## What it does

- Tiles windows in BSP, columns, rows, vertical stack, horizontal stack,
  ultrawide and grid layouts.
- Moves the focus, moves windows and resizes them by direction.
- Gives every monitor its own workspaces, and copes with a screen that is
  unplugged and comes back.
- Floats, maximizes, minimizes or monocles a window, and stacks several windows
  into one tile you cycle through.
- Ignores, floats, manages or places windows by rules on their exe, class,
  title or path.
- Draws window borders and nothing else on screen, and can fade unfocused
  windows and animate moves. Commands turn borders, transparency and
  animations on and off at once, without a reload.
- Binds its own keys, with binding modes, a pause key and a game mode that
  gives a game every key.

The keys the default hotkey file binds:

| Key | Does |
|---|---|
| `alt + h`, `j`, `k`, `l` | Focus left, down, up, right |
| `alt + shift + h`, `j`, `k`, `l` | Move the window left, down, up, right |
| `alt + 1` to `alt + 9` | Switch to that workspace |
| `alt + shift + 1` to `alt + shift + 9` | Move the window to that workspace and follow it |
| `alt + shift + s` | Resize mode: `h`, `j`, `k`, `l` resize, `Esc` leaves |
| `alt + shift + space` | Float the window, or tile it again |
| `alt + shift + q` | Close the window |
| `Pause` or `alt + f12` | Turn tiling off and on |
| `alt + shift + e` | Stop Mochi and put every window back |

The rest of the file and how to change it are under [Hotkeys](#hotkeys).

## Use it with an AI assistant

`mochic mcp` is an MCP server. An AI assistant that speaks MCP can use it to
see your monitors, workspaces and windows and to arrange them, through the same
local pipe `mochic` uses. Add it to the client's MCP configuration with the full
path to `mochic.exe`:

```json
{
  "mcpServers": {
    "mochi": {
      "command": "C:\\Users\\you\\AppData\\Local\\Programs\\Mochi\\bin\\mochic.exe",
      "args": ["mcp"]
    }
  }
}
```

Replace `you` with your Windows user name. With it, an assistant can:

- read the state: every monitor, workspace and window, and which one has the
  focus;
- say why a window is or is not tiled;
- list the hotkeys, and check `mochi.json` for errors even when Mochi will not
  start;
- focus a window, move one to another workspace, switch workspaces and set the
  layout;
- float a window or put it in monocle, retile, pause and resume tiling, and
  reload the configuration.

Nothing leaves the computer: the server makes no network connection and needs
no account. No tool can stop Mochi, close a window or write
a file. Every tool and what it answers is in [docs/mcp.md](docs/mcp.md).

Without MCP, call `mochic` itself: every command that reports something takes
`--json` and prints one JSON document, on success and on failure, as
[docs/json-output.md](docs/json-output.md) describes.

## Your data, updates and removal

Mochi makes no network connection and keeps everything on your PC:

| What | Where |
|---|---|
| `mochi.exe` and `mochic.exe` | `%LOCALAPPDATA%\Programs\Mochi\bin`, added to your user PATH |
| The copy of `Mochi-Setup.exe` that removes Mochi | `%LOCALAPPDATA%\Programs\Mochi` |
| Settings | `%USERPROFILE%\mochi.json` |
| Hotkeys | `%USERPROFILE%\.config\mochi\hotkeys` |
| The log, and the record of windows Mochi has taken off screen | `%LOCALAPPDATA%\mochi` |
| The start at login | the HKCU Run value `Mochi` |

Setup writes a default `mochi.json` and hotkey file only when there are none,
and never overwrites yours. A terminal that was already open does not see the
new PATH: open a new one before you type `mochic`, or it answers that `mochic`
is not recognized, which reads like the install failed when it did not.

To update, download and open `Mochi-Setup.exe` again. It stops the running
Mochi first and leaves your settings and hotkeys alone.

To remove Mochi, open Settings, Apps, Installed apps, and uninstall Mochi. It
removes the login start and the watchdog task if there is one, stops Mochi,
which puts every window back, and then removes the Start menu entry, the
program files, the PATH entry and the Apps & features entry. Your settings, hotkeys and log stay.
To remove those too, delete `%USERPROFILE%\mochi.json`,
`%USERPROFILE%\.config\mochi` and `%LOCALAPPDATA%\mochi` afterwards.

## Other ways to install

### Quiet install

`Mochi-Setup.exe --quiet` installs without asking, and
`Mochi-Setup.exe --uninstall --quiet` removes Mochi the same way. Both exit with
0 when everything worked. Setup is a windowed program, so a script that needs
the exit code starts it with `Start-Process -Wait -PassThru`.

### PowerShell

For a scripted install, open PowerShell (Start menu, type PowerShell, Enter) and
paste this line:

```
irm https://get-mochi.vercel.app/install.ps1 | iex
```

It works in Windows PowerShell 5.1 and PowerShell 7. It downloads the
[latest release](https://github.com/dominikkoenitzer/Mochi/releases/latest),
checks its SHA256 and installs the same way Setup does: the same folder, PATH
entry, default files, login start and Start menu entry, and then it starts
Mochi right away, which tiles every window on screen. It adds no Apps & features
entry; `-Uninstall` below removes what it installed.

The same line in this form takes switches:

```
& ([scriptblock]::Create((irm https://get-mochi.vercel.app/install.ps1))) -SkipStart -SkipAutostart
```

`-SkipStart` installs without starting Mochi now. `-SkipAutostart` does not
register the login start. `-Version v0.1.26` installs that release instead of
the latest. `-SkipPath` leaves your PATH alone, and `-SkipQuickstart` does not
write the default `mochi.json` and hotkey file. `-Uninstall` removes Mochi the
way the Apps & features entry does.

To update this way, run the line again. It refuses while Mochi is running, so
stop Mochi first with `alt + shift + e` or `mochic stop`.

### Zip

Every release also carries `Mochi-x86_64-pc-windows-msvc.zip` with `mochi.exe`,
`mochic.exe` and `mochi.schema.json`, and a `.sha256` file beside it. Unpack it
anywhere and run `mochic start`, which starts the `mochi.exe` next to it.
Nothing is registered: no PATH entry, no login start, no Start menu entry.

### Watchdog

The Run value only fires at login, so a daemon that stops during the day leaves
the desktop untiled until the next one. `scripts\autostart.ps1` can register a
task that asks every five minutes whether Mochi is up, and starts it when it is
not. When Mochi is already up, `mochic start` only says so, which makes the
check cheap. The script needs a clone of the repository (see
[Build from source](#build-from-source)), not a build:

```
powershell -ExecutionPolicy Bypass -File .\scripts\autostart.ps1 -Watchdog
```

With the watchdog on, `alt + shift + e` stops Mochi only until the next check:
the task starts it again within five minutes. `-Disable` removes the watchdog
along with the Run value, and that is the way out for good. Without a switch the
script shows what is registered today, and `-DisableStartupItem <name>` moves
another program's startup item aside until `-EnableStartupItem <name>` puts it
back.

## Hotkeys

Mochi binds keys itself, in process. There is no second program to install and
no subprocess per key press.

The file lives at `%USERPROFILE%\.config\mochi\hotkeys` and is reloaded the
moment it is saved. Each line is a key, a colon and a `mochic` command without
the `mochic`; a line that does not start with a Mochi command is run by the
shell:

```
alt + h                 : focus left
alt + shift + q         : close
alt + shift + e         : stop
```

`Pause` turns tiling off and on. Off means Mochi stops touching windows and
leaves them exactly where they are; on puts them back in their tiles. The daemon
keeps running either way, which is why the key still works while it is off.
`alt + f12` does the same on a keyboard with no Pause key.

`alt + shift + e` is the way out for good. It stops Mochi, puts every window it
was hiding back, takes the borders down and unbinds the keys. Nothing about the
desktop is left changed.

`alt` plus an arrow is deliberately left alone. `alt + left` and `alt + right`
are Back and Forward in every browser, and `alt + up` is the parent folder in
Explorer; a window manager that binds them takes those away everywhere with
nothing on screen to say why. The shipped file says how to add them if you
would rather have them.

`mochic hotkeys` prints what Mochi made of the file. Game mode, on
`alt + shift + g`, pauses tiling and suspends every binding except the one that
turns it off again, so the game in front gets the whole keyboard. A `mode`
block holds keys that only work while that mode is on: `alt + shift + s` enters
the shipped resize mode, where `h`, `j`, `k` and `l` resize and `Esc` leaves.

The full reference is [docs/hotkeys.md](docs/hotkeys.md).

## Running it from a terminal

Look before you leap. A dry run reads the desktop and changes nothing on it: no
window is moved, cloaked, focused or closed, it takes no control pipe and it
binds no keys, so it is safe to run while another window manager, or another
Mochi, is managing the screen. It only writes its log.

```
mochi --dry-run
```

Every line it prints with `dry-run:` in it is something it would have done. When
that looks right, start it for real, and stop it when you want your desktop
back:

```
mochic start
mochic stop
```

`mochic stop` puts every window it was hiding back on screen, takes the borders
down, restores transparency and unbinds the keys. The windows keep the size and
place of their last tile. Nothing is left off screen even if Mochi is killed
outright: every window taken off screen is written down first, and the next
start puts back anything the record still owes. While it is running,
`mochic restore-windows` puts back any window that is off screen with nothing
in Mochi's state to explain it; the windows of hidden workspaces stay hidden.

Mochi runs as a normal user and wants no admin rights. Windows therefore will
not let it move a window that belongs to a program running as administrator. It
notices, says so once in the log, and leaves that window alone instead of
tiling around a hole it cannot fill. To tile those windows too, set
`"elevated": true` in `mochi.json`: `mochic start` then starts the daemon as
administrator after the usual Windows prompt, once per start and so at every
logon. Declining the prompt starts Mochi with normal rights as before. The
details are under `elevated` in [docs/configuration.md](docs/configuration.md).

When a window is not being tiled and it is not obvious why, ask:

```
mochic why
```

It explains what Mochi makes of the window in front and what can be done about
it, in plain words. Every answer names either a fix or the reason there is
nothing to fix.

The log is in `%LOCALAPPDATA%\mochi`, one `mochi.log.<date>` file per day, and
the last 14 days are kept.

## Importing an existing setup

Config keys, command names and the hotkey file format follow the common tiling
window manager conventions, so an existing JSON config and hotkey file carry over
with a rename. Given the old config file, the old hotkey file and the old
command name, `scripts\import-config.ps1` shows the diff, and `-Apply` writes
the new files next to the old ones. Nothing the old setup owns is changed, and
two commands switch back at any time.

The whole path is in [docs/import.md](docs/import.md).

## Status

All six milestones in [PLAN.md](PLAN.md) are done. The test suite drives
throwaway windows and injects real key presses on a real desktop, and CI runs
it: the end to end job fails if those tests skip, because a test that returns
early still reports as passed.

Cross monitor moves and an unplugged screen have only run against a simulated
second monitor so far, and still need real hardware. Supervised use on a real
desktop has already found one defect no test could have, a hidden window that
could be forgotten without being shown again; it is fixed and covered by a test.

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
```

`rust-toolchain.toml` selects the stable toolchain with rustfmt and clippy, and
rustup fetches it on the first `cargo` call. The oldest Rust that builds Mochi
is 1.96, the `rust-version` in `Cargo.toml`. The first build takes a few
minutes.

Both exes are self-contained: the C runtime is linked in, so they run on any
Windows PC without installing a runtime.

To build `Mochi-Setup.exe` around the two exes you just built, in PowerShell:

```
$env:MOCHI_SETUP_PAYLOAD = "$PWD\target\release"
cargo build --release --locked -p mochi-setup
```

It lands in `target\release\mochi-setup.exe`. Without `MOCHI_SETUP_PAYLOAD` the
setup builds too, but carries nothing and says so when it is opened.

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
the other scripts in `scripts\`. The PowerShell line under
[Other ways to install](#powershell) is not affected by execution policy,
because it never runs a script file.

### Tests

The tests are optional, and `cargo test --workspace` touches the desktop you
work in. The `mochi-testbed` tests open real windows and try to move the focus
between them. The `mochi` tests start the built `mochi.exe --dry-run` for about
ten seconds and install global keyboard and window event hooks. None of that
waits for an environment variable. Only the end to end tests, which drive real
windows through the daemon, wait for `MOCHI_E2E`. To run just the tests that
touch no window, leave those two crates out:

```
cargo test --locked -p mochi-core -p mochi-hotkey -p mochi-render -p mochi-client -p mochic
```

### Layout

| Crate | Purpose |
|---|---|
| `mochi-core` | Pure logic: geometry, monitor/workspace/container tree, layouts, rules. No Win32, fully unit tested. |
| `mochi-client` | IPC wire types and the command grammar, shared by the daemon, the CLI and external tools. |
| `mochi-hotkey` | The hotkey file: key names, parsing, lookup. No Win32 either. |
| `mochi-render` | Borders, transparency and animation. |
| `mochi` | The daemon. One thread owns all state and talks to Win32. |
| `mochic` | Command-line client and MCP server. |
| `mochi-setup` | `Mochi-Setup.exe`, which installs and removes Mochi for the current user. |
| `mochi-testbed` | Throwaway windows the end-to-end tests drive. |

## Docs

- [docs/configuration.md](docs/configuration.md): every key in `mochi.json`
- [docs/cli.md](docs/cli.md): every `mochic` command and exit code
- [docs/hotkeys.md](docs/hotkeys.md): the hotkey file
- [docs/json-output.md](docs/json-output.md): `--json` documents
- [docs/mcp.md](docs/mcp.md): the MCP server's tools
- [docs/ipc.md](docs/ipc.md): the pipe protocol between `mochic` and the daemon
- [docs/import.md](docs/import.md): moving an existing setup over
- [PLAN.md](PLAN.md): decisions, architecture and milestones

## License

GPL-3.0-only. See [LICENSE](LICENSE).

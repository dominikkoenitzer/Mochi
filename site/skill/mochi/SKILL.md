---
name: mochi
description: Install, configure and troubleshoot Mochi, a tiling window manager for Windows, with its command-line client mochic. Use it when the user asks to install, update or remove Mochi, edit mochi.json or the hotkey file, rebind keys, run mochic commands, connect an MCP client to Mochi with mochic mcp, or get back a window or layout that Mochi left stuck.
---

# Mochi

Mochi is a tiling window manager for Windows. The moment it starts it arranges every window on screen into tiles, and it keeps doing so as windows open and close. It draws borders and nothing else: no bar, no panel.

It ships two programs:

- `mochi.exe` is the daemon that does the tiling and binds the keys itself.
- `mochic.exe` is the client. It starts and stops the daemon and sends it commands over a named pipe.

Mochi runs as a normal user and needs no admin rights. Windows does not let it move windows of programs that run as administrator, so it leaves those alone. The opt-in key `"elevated": true` in `mochi.json` changes that: `mochic start` then starts the daemon as administrator after the Windows prompt (once per start, so at every logon too), and those windows are tiled as well; declining the prompt starts Mochi with normal rights as before.

## Rules for the assistant

1. Before you start Mochi or run the installer, tell the user the way out: `Pause` (or `alt + f12`) turns tiling off and on, and `alt + shift + e` stops Mochi and puts every window back. Starting Mochi rearranges every window on screen.
2. Never start Mochi from a terminal that runs as administrator. The daemon inherits the elevation. Use a normal terminal or the Mochi entry in the Start menu. The one intended way to an elevated daemon is `"elevated": true` in `mochi.json` and `mochic start` from a normal terminal, which asks through the Windows prompt.
3. Before you edit `mochi.json` or the hotkey file, copy it to a backup next to it.
4. After every edit, run `mochic check` and fix every error it reports before the change is used.
5. Stop Mochi only with `mochic stop` or `alt + shift + e`. Never end `mochi.exe` with Task Manager, `Stop-Process` or `taskkill` unless the daemon no longer answers, and then tell the user first how the windows come back (see [Recover](#recover-a-stuck-layout-or-a-missing-window)).
6. Stop Mochi before you run the PowerShell installer to update it. That installer refuses to replace files while Mochi runs. `Mochi-Setup.exe` and `-Uninstall` stop Mochi by themselves.
7. Prefer machine-readable output when you need to parse it. `state`, `query`, `check`, `schema`, `doctor`, `why` and `hotkeys` take `--json` after the subcommand, for example `mochic query monitor-count --json`. Each then prints exactly one JSON document on stdout, whether it succeeds or fails. A failure is `{"error": {"code": ..., "message": ...}}`; the codes are `daemon_not_running`, `ipc`, `daemon`, `config`, `io`, `usage` and `internal`. The exit code is 0 on success, 1 on a failure at run time (or an unusable file for `check`), 2 when the command line did not parse. Details in [references/commands.md](references/commands.md#json-output).
8. When the Mochi MCP tools are available, prefer them to running `mochic` in a shell. See [MCP server](#mcp-server).

## Install

The usual way is `Mochi-Setup.exe`, one file with both programs inside. Download it from `https://mochi.punds.ch/download/windows` and open it as the normal user. If Windows says it protected the PC, click More info, then Run anyway. It shows the way out and asks once, needs no admin rights, and then:

- stops a running Mochi, which puts every window back,
- puts `mochi.exe` and `mochic.exe` in `%LOCALAPPDATA%\Programs\Mochi\bin`, adds that folder to the user PATH and keeps a copy of itself in `%LOCALAPPDATA%\Programs\Mochi`,
- runs `mochic quickstart`, which writes a default `mochi.json` and hotkey file only where none exists,
- registers the HKCU Run value `Mochi` so Mochi starts at login, adds Mochi to the Start menu and adds a Mochi entry under Settings, Apps, Installed apps,
- starts Mochi, which tiles every window on screen.

`Mochi-Setup.exe --quiet` installs without asking, and `Mochi-Setup.exe --uninstall --quiet` removes Mochi the same way. Both exit with 0 when everything worked. Setup is a windowed program, so a script that needs the exit code starts it with `Start-Process -Wait -PassThru` and reads `ExitCode`.

### PowerShell

For a scripted install, open PowerShell (Windows PowerShell 5.1 or PowerShell 7) as the normal user and paste:

```
irm https://mochi.punds.ch/install.ps1 | iex
```

This installer:

- downloads the latest release and checks its SHA256,
- puts `mochi.exe` and `mochic.exe` in `%LOCALAPPDATA%\Programs\Mochi\bin` and adds that folder to the user PATH,
- runs `mochic quickstart`, which writes a default `%USERPROFILE%\mochi.json` and `%USERPROFILE%\.config\mochi\hotkeys` only where none exists,
- registers the HKCU Run value `Mochi` so Mochi starts at login, and adds Mochi to the Start menu,
- starts Mochi, which tiles every window on screen.

It adds no entry under Installed apps; its `-Uninstall` switch removes what it installed. Release builds exist for x64 only. On other processors the installer stops and asks for a build from source.

After the install, open a new terminal before you type `mochic`. A terminal that was already open does not see the new PATH.

To pass switches, run the script as a script block:

```
& ([scriptblock]::Create((irm https://mochi.punds.ch/install.ps1))) -SkipStart -SkipAutostart
```

| Switch | Effect |
|---|---|
| `-Version v0.1.20` | Install that release. `latest` installs the newest. |
| `-InstallRoot <dir>` | Install into `<dir>\bin` instead of `%LOCALAPPDATA%\Programs\Mochi\bin`. |
| `-SkipStart` | Do not start Mochi at the end. |
| `-SkipAutostart` | Do not register the start at login. |
| `-SkipPath` | Leave the user PATH alone. |
| `-SkipQuickstart` | Do not write a default `mochi.json` or hotkey file. |
| `-Uninstall` | Remove Mochi, see below. |

### Update

Download and open `Mochi-Setup.exe` again. It stops the running Mochi first, replaces the files and starts Mochi again. With the PowerShell line, stop Mochi with `mochic stop` or `alt + shift + e`, then run the install line again. Config and hotkey files are never overwritten.

### Uninstall

Open Settings, Apps, Installed apps, and uninstall Mochi. It removes the Run value and the `MochiWatchdog` scheduled task if one exists, stops Mochi, which puts every window back, and then removes the Start menu entry, the PATH entry, the Installed apps entry, the binaries and the copy of `Mochi-Setup.exe`.

After the PowerShell install:

```
& ([scriptblock]::Create((irm https://mochi.punds.ch/install.ps1))) -Uninstall
```

It stops Mochi, which puts every window back, then removes the Run value, the `MochiWatchdog` scheduled task if one exists, the Start menu entry, the binaries and the PATH entry, and in the default folder also a copy of `Mochi-Setup.exe` and its Installed apps entry.

Both keep `mochi.json`, the hotkey file and the logs in `%LOCALAPPDATA%\mochi`; delete those by hand if the user wants them gone.

## Files

| What | Where |
|---|---|
| Binaries | `%LOCALAPPDATA%\Programs\Mochi\bin\mochi.exe` and `mochic.exe` |
| Setup copy | `%LOCALAPPDATA%\Programs\Mochi\Mochi-Setup.exe`, which the Installed apps entry runs to remove Mochi |
| Config | `%USERPROFILE%\mochi.json`. Overridden by `MOCHI_CONFIG`, which is overridden by `--config <path>`. |
| Hotkeys | `%USERPROFILE%\.config\mochi\hotkeys`. Overridden by `MOCHI_HOTKEYS`, which is overridden by `--hotkeys <path>`. |
| Logs | `%LOCALAPPDATA%\mochi\`, one file per day named `mochi.log.<date>`, the last 14 kept |
| Off-screen record | `%LOCALAPPDATA%\mochi\off-screen.json`, the windows Mochi has hidden. Do not delete it. |
| Config schema | `mochic schema --output mochi.schema.json` writes it; the download site serves it at `/schema.json` |

`--config` and `--hotkeys` are flags of both `mochi` and `mochic start`.

## Keys

The full default table, the file grammar and binding modes are in [references/hotkeys.md](references/hotkeys.md). The keys every user should know:

| Key | Command | Effect |
|---|---|---|
| `Pause` or `alt + f12` | `toggle-pause` | Tiling off: Mochi stops touching windows and leaves them where they are. Press again to put them back in their tiles. The daemon keeps running. |
| `alt + shift + e` | `stop` | Stops Mochi, shows every window it was hiding, removes borders and transparency, unbinds the keys. Windows keep the size and place of their last tile. |
| `alt + shift + g` | `toggle-game-mode` | Pauses tiling and frees every key except this one, for games. |
| `alt + shift + r` | `reload-configuration` | Re-reads `mochi.json` and the hotkey file. |
| `alt + shift + w` | `retile` | Recomputes and re-applies every layout. |
| `alt + h`, `j`, `k`, `l` | `focus left`, `down`, `up`, `right` | Moves focus. |
| `alt + shift + h`, `j`, `k`, `l` | `move left`, `down`, `up`, `right` | Moves the focused window. |
| `alt + 1` to `alt + 9` | `focus-workspace 0` to `8` | Switches workspace. |
| `alt + shift + space` | `toggle-float` | Floats or tiles the focused window. |
| `alt + shift + q` | `close` | Closes the focused window. |

The escape keys are ordinary lines in the hotkey file. If the user removes or rebinds them, they are gone. Keep a binding for `toggle-pause` and for `stop` in every hotkey file you write. Both also work from a terminal: `mochic toggle-pause`, `mochic stop`.

## Commands

`mochic <command>`. The full list with arguments is in [references/commands.md](references/commands.md). The ones you will use most:

| Command | Does |
|---|---|
| `mochic start` | Starts the daemon detached and waits until it answers. Says so if it already runs. `mochic start --dry-run` instead runs a dry run in this terminal, which changes nothing, until Ctrl-C. |
| `mochic stop` | Stops the daemon and puts back every window it took off screen. Windows keep the size and place of their last tile. |
| `mochic check [file]` | Reads the config and the hotkey file without a daemon and reports errors with line and column. Exits 0 when the config is usable, 1 when it is not. `--json` for the findings as one document. |
| `mochic reload-configuration` | Re-reads both files. |
| `mochic state` | The daemon state as JSON. |
| `mochic doctor` | Compares the daemon's picture of the desktop with the real one. `--json` for raw findings. |
| `mochic why` | Explains what Mochi makes of the window in front and what to do about it. `--json` for raw output. |
| `mochic hotkeys` | Lists the bindings the daemon holds and every line that did not parse. `--json` for raw output. |
| `mochic restore-windows` | Puts back every window that is off screen with nothing in Mochi's state to explain it, without stopping Mochi. The windows of hidden workspaces stay hidden. |
| `mochic quickstart` | Writes a default `mochi.json` and hotkey file where none exists. |
| `mochic schema --output <file>` | Writes the JSON schema of the config. Use `--output`, not `>`: Windows PowerShell 5.1 redirects as UTF-16. |
| `mochic mcp` | Runs the MCP server for an MCP client. See [MCP server](#mcp-server). |

## MCP server

`mochic mcp` runs a local MCP server on stdin and stdout (the stdio transport). With it an MCP client reads Mochi's state and drives it through tools instead of `mochic` in a shell. It runs until the client leaves. It cannot be bound to a key.

### Set up

The client starts the server itself. Give it the full path to `mochic.exe` as the command and `mcp` as the only argument. The installer puts `mochic.exe` in `%LOCALAPPDATA%\Programs\Mochi\bin` (in `<dir>\bin` after `-InstallRoot <dir>`). Most clients take this JSON form:

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

Replace `you` with the user's folder name. JSON needs every backslash doubled.

### What it can and cannot do

- It talks to the running Mochi over the same local named pipe `mochic` uses. Nothing leaves the computer: no network, no account, no telemetry.
- Stdout carries only the protocol. Its own messages go to stderr.
- No tool can stop Mochi, close a window or write a file. No tool starts Mochi either; follow rule 1 before you start it.
- The server tells the client to call `get_state` first. Window ids are the `hwnd` numbers from `get_state` (the `hwnd` field of each window in the state document). Workspace and monitor indexes are zero-based.
- When Mochi is not running, every tool that needs it answers with a tool error: "Mochi is not running. Start it from the Start menu, or run `mochic start` in a terminal, then try again." `check_config` needs no running Mochi, so it is the tool to use when Mochi will not start.

### Tools

| Tool | Arguments | Does |
|---|---|---|
| `get_state` | none | Returns the `mochic state --json` document. Reads only. |
| `why_not_tiled` | `window` (hwnd, optional) | Returns a document shaped like `mochic why --json`. Without `window` it explains the window in front. A window Mochi manages comes back as `managed` true with its `monitor` and `workspace`, taken from the state. Mochi can explain an unmanaged window only while it is in front, so for any other one the tool answers with an error: bring that window to the front and call again without `window`. Reads only. |
| `list_hotkeys` | none | Returns the `mochic hotkeys --json` document. Reads only. |
| `check_config` | `path` (string, optional) | Returns the `mochic check --json` document. Without `path` it checks the file Mochi would load. Reads only. Needs no running Mochi. |
| `focus_window` | `window` (hwnd, required) | Switches to the window's monitor and workspace and focuses its container, and its place in a stack. Mochi has no command that focuses one floating window, so for a floating window that does not have the focus already the tool shows its workspace and says so in an error. |
| `move_window_to_workspace` | `window` (hwnd, optional, default the focused window); `workspace` (integer, zero-based, on the window's monitor) or `workspace_name` (string, any monitor); `follow` (boolean, default false) | Moves the window to that workspace. With `follow` true the view follows the window. |
| `switch_workspace` | `workspace` (integer, zero-based) with optional `monitor` (integer, default the focused monitor), or `name` (string) | Shows that workspace. |
| `set_layout` | `layout`: `bsp`, `columns`, `rows`, `vertical-stack`, `horizontal-stack`, `ultrawide-vertical-stack` or `grid` | Sets the layout of the focused workspace. |
| `toggle_float` | `window` (hwnd, optional) | Switches that window, or the focused one, between tiled and floating. |
| `toggle_monocle` | `window` (hwnd, optional) | Switches monocle for that window, or the focused one. |
| `retile` | none | Recomputes and re-applies every layout. |
| `pause_tiling` | none | Pauses tiling. Windows stay where they are. |
| `resume_tiling` | none | Resumes tiling and retiles. |
| `reload_config` | none | Mochi re-reads its configuration file and hotkey file. A refusal, for example a broken file, comes back as a tool error with Mochi's message; `check_config` shows what is wrong. |

The action tools answer with a JSON object that holds `"ok": true` and `sent`, the list of commands sent to Mochi. The read-only tools carry the MCP read-only hint.

Given a `window`, `move_window_to_workspace`, `toggle_float` and `toggle_monocle` first focus it the way `focus_window` does, which shows its workspace, and then act on it. A tool that fails after it has sent commands ends its message with `(already sent: ...)`, so you know what changed. Read the state again after every change.

## Configure

The config is JSON. Keys are `snake_case`; enum values in the file are `PascalCase` (`"BSP"`, `"Equals"`). Every key has a default, so a short file works. All keys with examples are in [references/configuration.md](references/configuration.md).

Change it in this order:

1. Back up the file:
   ```
   Copy-Item "$env:USERPROFILE\mochi.json" "$env:USERPROFILE\mochi.json.bak"
   ```
2. Edit the file. Keep the `$schema` line; editors use it for completion.
3. Run `mochic check`. Fix everything it reports and run it again until it is clean.
4. Save. Mochi watches the file and applies a saved change at once. `mochic reload-configuration` forces a re-read.
5. Confirm with `mochic state` or the newest log file. A file that does not parse is not applied: Mochi keeps the previous configuration and writes the error to the log.
6. To undo, copy the backup back over the file. Mochi picks it up on save.

A key the file names replaces a value set with a `mochic` command since the last load, such as borders or padding; a key the file leaves out keeps the command's value. Rules added with `mochic ignore-rule` and the other rule commands survive a reload but are never written to the file; put permanent rules in `mochi.json`.

Short example:

```json
{
  "$schema": "https://mochi.punds.ch/schema.json",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "border": true,
  "border_width": 4,
  "monitors": [
    { "workspaces": [ { "name": "web", "layout": "BSP" }, { "name": "code", "layout": "Columns" } ] }
  ],
  "ignore_rules": [
    { "kind": "Exe", "id": "Taskmgr.exe", "matching_strategy": "Equals" }
  ]
}
```

## Rebind a key

1. Back up `%USERPROFILE%\.config\mochi\hotkeys`.
2. Edit the line. Syntax: `modifier + modifier + key : command`, where the command is any `mochic` command without the `mochic`. A line whose first word is not a Mochi command runs in the shell.
3. Run `mochic check`, then `mochic hotkeys` to see what Mochi made of the file.
4. Save. The file is reloaded on save.

Avoid new bindings on `alt` with a bare letter (Windows uses those for menus) and on `alt` with an arrow (browser Back and Forward, Explorer parent folder) unless the user asks for them.

## Recover a stuck layout or a missing window

Work down the list and stop at the first step that fixes it.

1. Ask Mochi about the window in front: `mochic why`. Its answer names a fix or the reason there is none.
2. Retile: `mochic retile` (`alt + shift + w`).
3. Check the bookkeeping: `mochic doctor`, then `mochic retile`, then `mochic doctor` again. Findings that disappear after the retile were stale.
4. A window should not be tiled: `mochic toggle-float` or `mochic unmanage` for now, then add an `ignore_rules` or `floating_applications` entry to `mochi.json` to make it stick.
5. A window is missing:
   - It may sit on another workspace, which Mochi hides. `mochic state` lists every monitor, workspace and window; switch there with `alt + 1` to `alt + 9` or `mochic focus-workspace <index>`.
   - Mochi may have hidden it and lost track of it. `mochic restore-windows` shows every such window without stopping Mochi.
6. Keys do nothing:
   - A window that runs as administrator has the focus. Windows does not pass its key presses to Mochi. Click a normal window, or set `"elevated": true` in `mochi.json` and restart with `mochic start`, accepting the prompt; `mochic doctor` reports `not-elevated` while the key is on but the running daemon has normal rights.
   - A binding mode is on. Press `Esc` to leave the shipped resize mode.
   - Game mode is on. Press `alt + shift + g` or run `mochic toggle-game-mode`.
   - Keys were disabled: `mochic set-hotkeys enable`.
   - The file has an error: `mochic hotkeys` lists every line that did not parse. Fix it and save.
   - Windows dropped the keyboard hook: `mochic reload-configuration` installs it again.
7. Turn tiling off: `Pause` or `mochic toggle-pause`. Windows stay where they are.
8. Stop Mochi: `mochic stop` or `alt + shift + e`. Every window it was hiding comes back.
9. Mochi was killed or crashed and windows are still missing: run `mochic start` from a normal terminal or open Mochi from the Start menu. On start it first puts back every window listed in `off-screen.json`. Then `mochic stop` if the user wants Mochi off.
10. The daemon does not answer (`mochic state` fails) but `Get-Process mochi` shows it: tell the user that ending it leaves the windows of other workspaces hidden until Mochi starts again, then end it and do step 9.
11. Mochi comes back by itself a few minutes after a stop: a watchdog task named `MochiWatchdog` is registered. The uninstall removes it.

`mochic` is not recognized: open a new terminal, or call `%LOCALAPPDATA%\Programs\Mochi\bin\mochic.exe` directly.

## Logs

Read the newest file in `%LOCALAPPDATA%\mochi\`. It holds every hotkey that fired, every reload error and every window Mochi took on or let go. A line `a window went off screen` with `ours=true` means Mochi hid the window; `ours=false` means something else did. The file gets `debug` level by default; the environment variable `MOCHI_LOG` takes a filter such as `trace` for the next start.

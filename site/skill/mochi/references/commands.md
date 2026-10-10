# mochic commands

`mochic` sends one command to the running Mochi daemon over the named pipe `\\.\pipe\mochi` and prints the answer. Four subcommands never touch the pipe and work without a daemon: `start`, `quickstart`, `check` and `schema`. Every other subcommand needs a running daemon and otherwise fails with exit code 1 and the message ``mochi is not running. Start it with `mochic start`.`` (with `--json`, an error document with the code `daemon_not_running`). `mcp` also starts without a daemon; its tools that need one answer with an error until Mochi runs.

## Basics

- Subcommands and enum values are kebab-case: `focus-workspace`, `ease-out-quad`, `no-op`.
- Indexes are zero-based: monitor `0` is the first monitor, workspace `0` the first workspace.
- `mochic --help`, `mochic <subcommand> --help` and `mochic --version` are available.
- A command the daemon accepts prints nothing and exits 0, unless the table below says it prints something.
- Errors go to stderr as one line starting with `mochic:`. With `--json` they come out on stdout as an error document instead; see [JSON output](#json-output).
- "The focused window" is the window in front. When `mochic` is typed in a terminal, that is usually the terminal itself.

| Exit code | Meaning |
|---|---|
| `0` | Accepted. For `check`: the file is usable. |
| `1` | The daemon answered with an error, the daemon is not running, or (for `check`) the file is not usable. |
| `2` | The command line did not parse: unknown subcommand, missing argument, value not in the list. Nothing was sent. With `--json` on the line, it prints an error document with the code `usage`. |

## Output at a glance

| Command | Output |
|---|---|
| `state` | JSON (pretty-printed), always; with `--json` a failure is an error document too |
| `query <target>` | One bare value per line: strings without quotes, numbers, `true`/`false`, `null`; with `--json` a document with `version`, `target` and `value` |
| `doctor` | Text report; JSON with `--json` |
| `why` | Text paragraph; JSON with `--json` |
| `hotkeys` | Text table; JSON with `--json` |
| `schema` | JSON schema on stdout; with `--output` a `wrote <path>` line, or with `--output` and `--json` a document with `version` and `path` |
| `subscribe <name>` | One JSON object per line on stdout (status lines on stderr) |
| `check [PATH]` | Text report; JSON with `--json`. The exit code is the result either way |
| `mcp` | MCP protocol messages on stdout for an MCP client, until it leaves; its own messages on stderr |
| `start` | Text line: `started <path> (pid N)` or `mochi is already running`; with `--dry-run`, the dry run's own output in the same terminal until it ends |
| `quickstart` | Text lines: `wrote <path>` or `<path> already exists, leaving it alone` |
| every other command | Nothing on success; error text on stderr on failure |

## JSON output

`state`, `query`, `check`, `schema`, `doctor`, `why` and `hotkeys` take `--json`. It belongs to the command, so it goes after the subcommand: `mochic query monitor-count --json`, `mochic check C:\path\to\mochi.json --json`. The command then prints exactly one JSON document on stdout and nothing else, whether it succeeds or fails. The document is pretty-printed; parse it and do not depend on the whitespace.

`subscribe`, `mcp` and the action commands (`focus`, `move`, `stop`, `retile` and the rest) have no `--json`.

The documents `mochic` builds itself (`query`, `check`, `schema --output`) have a top-level `version`, the version of `mochic`. The daemon's documents (`state`, `doctor`, `why`, `hotkeys`) and the schema are printed as they are, without it; `state` has its own `version`, the daemon's. The error document has no `version`.

A failure:

```json
{
  "error": {
    "code": "daemon_not_running",
    "message": "mochi is not running. Start it with `mochic start`."
  }
}
```

`message` is the text `mochic` prints to stderr without `--json`. For `usage` it is the first line of the parser's message.

| Code | When |
|---|---|
| `daemon_not_running` | No daemon is running. |
| `ipc` | The daemon could not be reached, or answered with something that does not fit the command. |
| `daemon` | The daemon understood the command and refused it. |
| `config` | `check` found no configuration path. |
| `io` | A file could not be written. |
| `usage` | The command line did not parse and contained `--json`. `--help` and `--version` keep their normal output. |
| `internal` | Anything else. |

Within a major version, fields are added, never renamed or removed. Error codes are never renamed. Treat a code you do not know like `internal`. The exit codes are the same with `--json` as without.

## Daemon control

| Syntax | What it does |
|---|---|
| `mochic start [--config PATH] [--hotkeys PATH] [--no-hotkeys] [--dry-run]` | Starts `mochi.exe` detached from the console and waits up to 10 seconds for its pipe. No daemon needed. With `--dry-run` it runs `mochi --dry-run` in this terminal instead and returns when that ends. |
| `mochic stop` | Stops the daemon. Before it exits, every window it took off screen is shown again and the transparency it set is cleared. Windows keep the size and place of their last tile. |
| `mochic toggle-pause` | Pauses window management, or resumes it. |
| `mochic toggle-game-mode` | Pauses tiling and suspends every hotkey except the one bound to `toggle-game-mode`. Run again to undo. |
| `mochic reload-configuration` | Re-reads `mochi.json` and the hotkey file, then retiles. |
| `mochic retile` | Recomputes and re-applies every layout. |
| `mochic restore-windows` | Puts back every window that is off screen with nothing in the daemon state to explain it. Leaves windows on hidden workspaces alone. Does not stop the daemon. |

`start` options:

| Option | Value | Effect |
|---|---|---|
| `--config PATH` | file path | Passed to the daemon as its configuration file. |
| `--hotkeys PATH` | file path | Passed to the daemon as its hotkey file. |
| `--no-hotkeys` | flag | The daemon binds no keys. Cannot be combined with `--hotkeys`. |
| `--dry-run` | flag | Runs a dry run in this terminal instead of a background daemon. It moves, hides, focuses and closes nothing, opens no pipe and binds no keys, and every line it prints with `dry-run:` in it is something it would have done. Ctrl-C or closing the terminal ends it. |

Notes on these commands:

- `start` looks for `mochi.exe` next to `mochic.exe` first, then on `PATH`. If a daemon is already running it prints `mochi is already running` and exits 0. `start --dry-run` skips that check, because a dry run is safe beside a running daemon.
- If the daemon exits during `start`, the error says so and suggests `mochic check` and the log folder `%LOCALAPPDATA%\mochi`.
- Without a config path the daemon reads `MOCHI_CONFIG`, then `%USERPROFILE%\mochi.json`.
- A dry run opens no pipe, so no other `mochic` command, `mochic stop` included, can reach it. End it with Ctrl-C. If it exits with an error, `start --dry-run` reports its exit code and exits 1.
- While paused, commands that change the layout fail with `mochi is paused, nothing was changed` (exit 1). Resuming adopts windows opened during the pause and retiles.
- `toggle-game-mode` and `set-hotkeys` fail on a daemon started with `--no-hotkeys`.
- `reload-configuration` keeps the old configuration if the file cannot be loaded and answers with an error. Run `mochic check` to see why.

## Configuration files (no daemon needed)

| Syntax | What it does |
|---|---|
| `mochic quickstart` | Writes `%USERPROFILE%\mochi.json` and `%USERPROFILE%\.config\mochi\hotkeys`, each only if it does not exist yet. If that folder already holds a hotkey file under the other name Mochi reads, no hotkey file is written. |
| `mochic check [PATH] [--json]` | Validates a configuration file without applying anything. |
| `mochic schema [--output PATH] [--json]` | Prints the JSON schema of `mochi.json`. `-o` is the short form of `--output`. With `--output`, `--json` prints a document with `version` and `path` instead of `wrote <path>`; a file that cannot be written is an error document with the code `io`. Without `--output`, `--json` prints the schema unchanged. |

`check` details:

- No `PATH` means the file the daemon would load: `MOCHI_CONFIG`, then `%USERPROFILE%\mochi.json`.
- Reports parse errors with line and column, rules that cannot work, top-level keys that are ignored, and the file named by `app_specific_configuration_path` (problems there are warnings).
- Also reads the hotkey file and reports the number of bindings and every line that did not parse.
- Given an application rules file instead of a `mochi.json`, it checks that file as one.
- The last line is `ok: <path> is usable` (optionally `, with N warning(s)`) or `not usable: <reason>`. Exit 0 when usable, 1 when not. Warnings never change the exit code.
- `--json` prints one document with the same exit code: `version`, `path`, `source` (the command line, `MOCHI_CONFIG` or the default path), `kind` (`config` or `app_rules`), `usable`, `headline` (one sentence on what to fix first, null when usable), `errors`, `warnings` and `notes` (arrays of strings). With no configuration path at all it prints an error document with the code `config` instead.

`schema --output PATH` writes UTF-8. Prefer it over redirecting stdout, because Windows PowerShell 5.1 redirection writes UTF-16.

## Diagnostics

| Syntax | What it does | Output |
|---|---|---|
| `mochic state [--json]` | Prints the whole daemon state. | JSON; with `--json` a failure is an error document too |
| `mochic query <target> [--json]` | Prints one value. | bare value, or JSON with `--json` |
| `mochic doctor [--json]` | Compares the daemon's picture of the desktop with the real one and lists what disagrees. | text, or JSON with `--json` |
| `mochic why [--json]` | Explains what Mochi makes of the window in front: tiled and where, or why it is left alone and what would change that. | text, or JSON with `--json` |
| `mochic hotkeys [--json]` | Lists the bindings the daemon holds and the lines of the hotkey file that did not parse. | text table, or JSON with `--json` |

`query` targets: `focused-monitor-index`, `focused-workspace-index`, `focused-container-index`, `focused-window-index`, `focused-workspace-name`, `monitor-count`, `window-count`, `paused`, `dry-run`, `config-path`, `version`. `query --json` prints `version` (of `mochic`), `target` (as typed) and `value` (the daemon's answer).

Top-level keys of `state`: `version`, `dry_run`, `paused`, `config_path`, `app_config_path`, `manage_classes`, `focused_monitor`, `focused_workspace`, `focused_window`, `foreground_window`, `window_count`, `monitors`, `settings`, `behaviour`, `rules`, `subscribers`, `fullscreen`. Window handles in `state` are numbers, the `HWND` value; `why` and `doctor` write them as hex strings.

`doctor` text starts with `N window(s) managed, M taken off screen by Mochi.`, then either `Nothing wrong: ...` or one entry per finding (`title  [exe hwnd]` and a detail line). Its JSON has `managed`, `off_screen` and `findings` (each with `kind`, `title`, `exe`, `hwnd`, `detail`; `findings` is empty when nothing is wrong). `kind` is one of `gone`, `unreachable`, `hole`, `stale-record`, `wrong-record`, `cannot-restore`, `hotkeys-blocked`. A stale layout is fixed by `mochic retile`.

`why --json` has `hwnd` (hex, for example `0x1a2b`), `title`, `exe`, `class` and `managed`. A managed window adds `monitor` and `workspace`; one that is not managed adds `reason`. `fullscreen_monitor`, `scratchpad`, `overridable` and `hotkeys_blocked` are there only when they apply. `reason` is one of `scratchpad`, `paused`, `manage-class`, `rule`, `unknown`, or a trait of the window itself: `not visible`, `cloaked`, `child window`, `tool window`, `no-activate window`, `owned window`, `no title`, `too small`, `shell class`, `shell process`, `elevated, out of reach`, `always on top`, `click-through overlay`.

`hotkeys --json` has `path`, `gate` (`all`, `off` or `game-mode`), `bindings` (each `keys`, `command`, optional `mode`) and `errors`.

## Notifications

| Syntax | What it does |
|---|---|
| `mochic subscribe <name>` | Creates the pipe `\\.\pipe\<name>`, registers it and prints every event as one JSON line until Ctrl-C or until the daemon stops. |
| `mochic subscribe-pipe <name>` | Registers an existing pipe `\\.\pipe\<name>` for events. |
| `mochic unsubscribe-pipe <name>` | Stops sending events to that pipe. |

`<name>` is given without the `\\.\pipe\` prefix.

## MCP server

| Syntax | What it does |
|---|---|
| `mochic mcp` | Runs a local MCP server on stdin and stdout (stdio transport) for an MCP client, until the client leaves. It talks to the running daemon over the same named pipe as every other command. Nothing leaves the computer. No tool can stop Mochi, close a window or write a file. |

An MCP client starts it itself: the command is the full path to `mochic.exe` (`%LOCALAPPDATA%\Programs\Mochi\bin\mochic.exe` after a default install), the arguments are `["mcp"]`. Stdout carries only the protocol; messages go to stderr. The tools, their arguments and the client config are in the MCP server section of [SKILL.md](../SKILL.md#mcp-server).

## Focus and movement

| Syntax | What it does |
|---|---|
| `mochic focus <direction>` | Moves focus to the neighbour in that direction. |
| `mochic cycle-focus <cycle>` | Moves focus one container along the ring, by position. |
| `mochic move <direction>` | Moves the focused window in that direction. |
| `mochic cycle-move <cycle>` | Moves the focused window one place along the ring, by position. |
| `mochic promote` | Swaps the focused window with the first window of the workspace. |
| `mochic promote-focus` | Focuses the first window of the workspace, moves nothing. |

`<direction>` is one of `left`, `right`, `up`, `down`. `<cycle>` is `next` or `previous`.

## Resize and padding

| Syntax | What it does |
|---|---|
| `mochic resize-axis <axis> <sizing>` | Grows or shrinks the focused window along an axis. |
| `mochic resize-edge <direction> <sizing>` | Grows or shrinks the focused window by moving that one edge. |
| `mochic workspace-padding <monitor> <workspace> <size>` | Sets the outer padding of one workspace, in logical pixels. |
| `mochic container-padding <monitor> <workspace> <size>` | Sets the padding between containers of one workspace, in logical pixels. |

`<axis>` is `horizontal` or `vertical`. `<sizing>` is `increase` or `decrease`. `<monitor>` and `<workspace>` are zero-based indexes. `<size>` is an integer and may be negative.

## Window state

| Syntax | What it does |
|---|---|
| `mochic toggle-float` | Switches the focused window between tiled and floating. |
| `mochic toggle-float-override` | Switches whether every new window floats. Windows already open do not move. Lasts until the daemon stops, or until a reload of a `mochi.json` that sets `float_override`. |
| `mochic toggle-maximize` | Switches the focused window between tiled and maximized. |
| `mochic toggle-monocle` | Switches monocle mode (whole work area) for the focused window. |
| `mochic minimize` | Minimizes the focused window. |
| `mochic close` | Asks the focused window to close. |
| `mochic manage` | Manages the focused window even if a rule would skip it. |
| `mochic unmanage` | Stops managing the focused window. |

## Stacks

| Syntax | What it does |
|---|---|
| `mochic stack <direction>` | Stacks the focused window onto its neighbour in that direction. |
| `mochic unstack` | Pulls the focused window out of its stack. |
| `mochic stack-all` | Collapses the whole workspace into one stack. |
| `mochic unstack-all` | Gives every stacked window its own container again. |
| `mochic cycle-stack <cycle>` | Steps through the windows of the focused stack. |
| `mochic focus-stack-window <index>` | Focuses the window at that zero-based position in the focused stack. |

## Layouts

| Syntax | What it does |
|---|---|
| `mochic change-layout <layout>` | Sets the layout of the focused workspace. |
| `mochic cycle-layout <cycle>` | Steps through the layout ring. |
| `mochic flip-layout <axis>` | Mirrors the layout along `horizontal` or `vertical`. |
| `mochic toggle-tiling` | Stops arranging the focused workspace, or resumes. Windows stay managed and stay where they are. |

`<layout>` is one of `bsp`, `columns`, `rows`, `vertical-stack`, `horizontal-stack`, `ultrawide-vertical-stack`, `grid`.

## Workspaces

| Syntax | What it does |
|---|---|
| `mochic focus-workspace <index>` | Focuses a workspace by zero-based index. |
| `mochic move-to-workspace <index>` | Moves the focused window to a workspace and follows it. |
| `mochic send-to-workspace <index>` | Moves the focused window to a workspace and stays where you are. |
| `mochic cycle-workspace <cycle>` | Steps through the workspaces of the focused monitor. |
| `mochic focus-last-workspace` | Goes back to the previously focused workspace. |
| `mochic focus-named-workspace <name>` | Focuses the workspace with that name from the configuration. |
| `mochic move-to-named-workspace <name>` | Moves the focused window to the named workspace and follows it. |
| `mochic send-to-named-workspace <name>` | Moves the focused window to the named workspace and stays where you are. |

`<index>` must not be negative.

## Monitors

| Syntax | What it does |
|---|---|
| `mochic focus-monitor <index>` | Focuses a monitor by zero-based index. |
| `mochic move-to-monitor <index>` | Moves the focused window to a monitor and follows it. |
| `mochic send-to-monitor <index>` | Moves the focused window to a monitor and stays where you are. |
| `mochic cycle-monitor <cycle>` | Steps through the monitor ring. |

## Scratchpads

`<name>` is a name from the `scratchpads` list in `mochi.json`.

| Syntax | What it does |
|---|---|
| `mochic toggle-scratchpad <name>` | Shows the scratchpad on the focused monitor, or hides it again. |
| `mochic scratchpad-claim <name>` | Makes the focused window the scratchpad's window. |
| `mochic scratchpad-release <name>` | Gives the scratchpad's window back to tiling. |

## Input behaviour

| Syntax | Values | What it does |
|---|---|---|
| `mochic focus-follows-mouse <state>` | `enable`, `disable` | Focuses whatever window the mouse moves over. |
| `mochic mouse-follows-focus <state>` | `enable`, `disable` | Moves the mouse to a newly focused window. |
| `mochic window-container-behaviour <behaviour>` | `create`, `append` | Whether a new window gets its own container or stacks onto the focused one. |
| `mochic toggle-window-container-behaviour` | none | Switches between `create` and `append`. |
| `mochic cross-monitor-move-behaviour <behaviour>` | `swap`, `insert`, `no-op` | What moving a container past a monitor edge does. |
| `mochic window-hiding-behaviour <behaviour>` | `hide`, `minimize`, `cloak` | How a window on an inactive workspace is taken off screen. |
| `mochic unmanaged-window-operation-behaviour <behaviour>` | `op`, `no-op` | Whether a command aimed at an unmanaged window runs or is refused. |
| `mochic set-hotkeys <state>` | `enable`, `disable` | Turns the hotkeys on or off. Tiling continues either way. |

## Visuals

| Syntax | Values | What it does |
|---|---|---|
| `mochic border <state>` | `enable`, `disable` | Turns the focus border on or off. |
| `mochic border-width <width>` | integer, physical pixels | Sets the border thickness (not scaled by DPI). |
| `mochic border-offset <offset>` | integer, physical pixels, may be negative | Sets how far the border sits outside the window frame. Negative pulls it inwards. |
| `mochic border-colour <r> <g> <b> [--window-kind <kind>]` | each channel 0 to 255; kind `single` (default), `stack`, `monocle`, `floating`, `unfocused` | Sets the border colour for one kind of window. |
| `mochic border-style <style>` | `system`, `rounded`, `square` | Sets the border shape. |
| `mochic toggle-transparency` | none | Toggles transparency for unfocused windows. |
| `mochic animation <state>` | `enable`, `disable` | Turns move and resize animations on or off. |
| `mochic animation-duration <ms>` | non-negative integer | Sets the animation duration in milliseconds. |
| `mochic animation-style <style>` | see below | Sets the easing curve. |
| `mochic animation-fps <fps>` | non-negative integer | Sets the animation frame rate. |

Animation styles: `linear`, `ease-in-sine`, `ease-out-sine`, `ease-in-out-sine`, `ease-in-quad`, `ease-out-quad`, `ease-in-out-quad`, `ease-in-cubic`, `ease-out-cubic`, `ease-in-out-cubic`.

## Rules

| Syntax | What it does |
|---|---|
| `mochic float-rule <identifier> <id> [--matching-strategy <strategy>]` | Floats windows that match. |
| `mochic ignore-rule <identifier> <id> [--matching-strategy <strategy>]` | Ignores windows that match. |
| `mochic manage-rule <identifier> <id> [--matching-strategy <strategy>]` | Manages matching windows Mochi would otherwise skip. |
| `mochic workspace-rule <identifier> <id> <monitor> <workspace> [--initial-only] [--matching-strategy <strategy>]` | Opens matching windows on that monitor and workspace. `--initial-only` routes only the first window the rule ever matches. |

- `<identifier>` is one of `exe`, `class`, `title`, `path`.
- `<id>` is the value to match. Quote it if it contains spaces.
- `<strategy>` is one of `equals` (default), `contains`, `starts-with`, `ends-with`, `regex`. Negated strategies exist only in `mochi.json`.
- A rule that does not validate (for example a bad regex) is refused with an error.
- Rules added this way are not written to `mochi.json`. They survive `reload-configuration` and are gone when the daemon stops. Permanent rules belong in `mochi.json`.

## Hotkey file

A binding in the hotkey file uses the same words as `mochic` without the program name, for example `focus left`. `start`, `quickstart`, `check`, `schema`, `subscribe` and `mcp` cannot be bound to a key. `stop` can.

## Safety notes

- `mochic stop` ends window management. On the way out the daemon shows, uncloaks or un-minimizes every window it took off screen, including windows on other workspaces, clears transparency it set, removes borders and unbinds its keys. Windows keep the size and place of their last tile. Ctrl-C, closing its console, logoff and a panic run the same restore.
- Ending `mochi.exe` hard (Task Manager, `Stop-Process`, `taskkill /f`) skips that restore: windows on hidden workspaces stay hidden. The next `mochic start` puts them back from its record. Use `mochic stop` instead of killing the process.
- To recover invisible windows while the daemon keeps running, use `mochic restore-windows`. It puts back windows that nothing in the daemon state accounts for; windows on hidden workspaces stay hidden until their workspace is shown. It is safe to run at any time.
- `mochic close` closes the focused window, which may be the terminal running the command. `minimize`, `unmanage` and the move commands also act on that window.
- `mochic set-hotkeys disable` and `mochic toggle-game-mode` take the keyboard bindings away. Bring them back with `mochic set-hotkeys enable` or `mochic toggle-game-mode`.
- After `mochic toggle-pause` the desktop stays untiled until it is run again. `mochic query paused` answers `true` or `false`.
- Settings changed with `mochic` (borders, behaviours, rules, padding) are not saved to `mochi.json`.
- Run `mochic check` before `mochic reload-configuration` or `mochic start` after editing `mochi.json` or the hotkey file.

# MCP server

`mochic mcp` lets an AI assistant see and arrange your windows. It is an MCP
server on stdin and stdout, the stdio transport. It reaches the daemon over the
same local named pipe as every other `mochic` command. An MCP client starts it
and talks to it; there is no reason to run it by hand.

## Setup

The client needs the full path to `mochic.exe` and the argument `mcp`. The
setup and the one line install put `mochic.exe` in
`%LOCALAPPDATA%\Programs\Mochi\bin`. Most clients take this JSON form:

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

Replace `you` with your Windows user name. In PowerShell,
`"$env:LOCALAPPDATA\Programs\Mochi\bin\mochic.exe"` prints the path for your
account. JSON wants every backslash written twice.

Give the full path rather than `mochic`. A client that was already running
when Mochi was installed does not see the new PATH, the same way an open
terminal does not.

The server starts whether Mochi is running or not, and runs until the client
goes away. Stdout carries only the protocol. Everything else the server has to
say goes to stderr.

## What it can and cannot do

Nothing leaves the computer. The server makes no network connection, needs no
account and sends no telemetry. It talks to the daemon over the local pipe and
to its client over stdin and stdout, and to nothing else.

No tool can stop Mochi, close a window or write a file.

The tools that only read carry the MCP read-only hint. The others do not.

## Window ids

A tool that takes a window takes its handle: the `hwnd` number of the window in
the `get_state` document. See the window table in
[json-output.md](json-output.md#window). It is the number, not the hex string
`why` writes.

Monitor and workspace indexes are zero-based, as in the state document and on
the command line.

The server tells its client the same when it connects: call `get_state` first,
use the `hwnd` numbers as window ids, count monitors and workspaces from zero,
and read the state again after a change.

## Tools

| Tool | Reads only | Does |
|---|---|---|
| `get_state` | yes | The whole daemon state. |
| `why_not_tiled` | yes | Whether a window is tiled, and why not when it is not. |
| `list_hotkeys` | yes | Every binding, and every line of the hotkey file that did not parse. |
| `check_config` | yes | What is wrong with a configuration file. Needs no running Mochi. |
| `focus_window` | no | Focus one window, wherever it is. |
| `move_window_to_workspace` | no | Move a window to another workspace. |
| `switch_workspace` | no | Show another workspace. |
| `set_layout` | no | Set the layout of the focused workspace. |
| `toggle_float` | no | Switch a window between tiled and floating. |
| `toggle_monocle` | no | Let one container fill the workspace, and back. |
| `retile` | no | Recompute and apply every layout. |
| `pause_tiling` | no | Pause window management. |
| `resume_tiling` | no | Resume window management. |
| `reload_config` | no | Re-read the configuration file and the hotkey file. |

Every tool answers with one JSON document. The four that read answer with the
document the matching `--json` command prints, described in
[json-output.md](json-output.md). The others answer with a small document of
their own, and every one of those has these two fields:

| Field | Type | Meaning |
|---|---|---|
| `ok` | boolean | Always true. A failure is a tool error instead, see [Errors](#errors). |
| `sent` | array of strings | The commands the tool sent to the daemon, in order, by name and without their arguments, for example `focus-workspace` or `cycle-focus`. |

`move_window_to_workspace`, `toggle_float` and `toggle_monocle` take an
optional `window`. Given one, the tool first focuses it the way `focus_window`
does, which shows its workspace, and then acts on it. Without one, it acts on
the focused window.

Tool calls run one at a time, so another call cannot move the focus between
those two steps.

### get_state

No arguments. Answers with the document `mochic state --json` prints: the
monitors, their workspaces with the layout of each, the containers, the windows
with `hwnd`, `title`, `exe` and `class`, the focused monitor, workspace and
window, and whether management is paused. Every field is in
[json-output.md](json-output.md#state).

### why_not_tiled

| Argument | Type | Meaning |
|---|---|---|
| `window` | number, optional | The window to explain. Without it, the window in front. |

Answers with a document shaped like the one `mochic why --json` prints: `hwnd`
as a hex string,
`title`, `exe`, `class`, `managed`, `monitor` and `workspace` when it is
managed, `reason` when it is not, and the other fields when they apply. Every
field and every reason is in [json-output.md](json-output.md#why).

For a window Mochi manages, the answer is `managed` true with its monitor and
workspace, taken from the state. Mochi can explain an unmanaged window only
while it is in front. Asked about an unmanaged window that is not, the tool
answers with an error that says so: bring the window to the front and call it
again without `window`.

### list_hotkeys

No arguments. Answers with the document `mochic hotkeys --json` prints: the
hotkey file in use, the gate, every binding and every line that did not parse.
See [json-output.md](json-output.md#hotkeys).

### check_config

| Argument | Type | Meaning |
|---|---|---|
| `path` | string, optional | The file to check. Without it, the file Mochi would load. |

Answers with the document `mochic check --json` prints: `usable`, `headline`,
`errors`, `warnings`, `notes` and the rest, in
[json-output.md](json-output.md#check). Like `mochic check`, it needs no
running Mochi, which makes it the tool to call when Mochi will not start.

### focus_window

| Argument | Type | Meaning |
|---|---|---|
| `window` | number | The window to focus. |

Switches to the window's monitor and workspace and steps the focus to its
container, and to its place when the container is a stack.

| Field | Type | Meaning |
|---|---|---|
| `ok` | boolean | True. |
| `window` | number | The window focused. |
| `monitor` | number | The monitor it is on. |
| `workspace` | number | The workspace it is on. |
| `sent` | array of strings | The commands sent. |

Mochi has no command that focuses one floating window. For a floating window
the tool shows its workspace and then answers with an error that says so.

### move_window_to_workspace

| Argument | Type | Meaning |
|---|---|---|
| `window` | number, optional | The window to move. Without it, the focused window. |
| `workspace` | number | The workspace index on the window's monitor, zero-based. |
| `workspace_name` | string | The workspace with that name, on any monitor. |
| `follow` | boolean, optional | False by default: the screen stays as it is. True: the view follows the window to its new workspace. |

Give `workspace` or `workspace_name`, not both.

| Field | Type | Meaning |
|---|---|---|
| `ok` | boolean | True. |
| `window` | number or null | The window moved. Null when the tool moved the focused window. |
| `workspace` | number or string | The workspace it was moved to, as given. |
| `followed` | boolean | True when the view followed the window. |
| `sent` | array of strings | The commands sent. |

### switch_workspace

| Argument | Type | Meaning |
|---|---|---|
| `workspace` | number | The workspace index, zero-based. |
| `monitor` | number, optional | The monitor of that workspace. Without it, the focused monitor. |
| `name` | string | The workspace with that name, on whichever monitor it is. |

Give `workspace`, with `monitor` when it is on another monitor, or `name`, not
both. A name already says which monitor, so `name` takes no `monitor`. Answers
with `ok` and `sent`.

### set_layout

| Argument | Type | Meaning |
|---|---|---|
| `layout` | string | `bsp`, `columns`, `rows`, `vertical-stack`, `horizontal-stack`, `ultrawide-vertical-stack` or `grid`. |

Sets the layout of the focused workspace. To change another workspace, switch
to it first. Answers with `ok`, `layout` (the layout set, spelled as given, for
example `vertical-stack`) and `sent`.

### toggle_float

| Argument | Type | Meaning |
|---|---|---|
| `window` | number, optional | The window. Without it, the focused window. |

Switches the window between tiled and floating. Answers with `ok`, `window`
(the window given, or null for the focused one) and `sent`.

### toggle_monocle

| Argument | Type | Meaning |
|---|---|---|
| `window` | number, optional | The window. Without it, the focused window. |

Turns monocle on or off for the window's container: in monocle it fills the
workspace. Answers like `toggle_float`.

### retile

No arguments. Recomputes and applies every layout. Answers with `ok` and
`sent`, which is `["retile"]`.

### pause_tiling

No arguments. Pauses window management, unless it is paused already. While it
is paused, Mochi leaves every window where it is.

| Field | Type | Meaning |
|---|---|---|
| `ok` | boolean | True. |
| `paused` | boolean | True. |
| `changed` | boolean | False when management was paused already. |
| `sent` | array of strings | The commands sent. |

### resume_tiling

No arguments. Resumes window management and retiles. Answers like
`pause_tiling`, with `paused` false; `changed` is false when
management was not paused.

### reload_config

No arguments. Mochi re-reads its configuration file and its hotkey file.
Answers with `ok` and `sent`, which is `["reload-configuration"]`. When Mochi
refuses, for example because the file is broken, the tool answers with an
error that carries Mochi's message. `check_config` shows what is wrong.

## Errors

A failure is a tool error: the result is marked as an error, and its text says
what went wrong and what to do.

When Mochi is not running, every tool that needs it answers:

```
Mochi is not running. Start it from the Start menu, or run `mochic start` in a terminal, then try again.
```

`check_config` is the one tool that needs no running Mochi.

| When | The tool answers |
|---|---|
| Mochi is not running. | The message above. |
| Mochi refuses a command. | An error that names the command and carries Mochi's message. |
| A window id names no window Mochi manages, for example one that has closed. | An error that says so and asks for `get_state` again. |
| Both or neither of `workspace` and `workspace_name` are given, or of `workspace` and `name`. | An error that says which to give. |
| `why_not_tiled` is asked about an unmanaged window that is not in front. | An error: bring the window to the front and call again without `window`. |
| `focus_window` is given a floating window. | Its workspace is shown, then an error says that Mochi has no command that focuses one floating window. |
| A monocle, a maximized window or a floating window keeps the focus from reaching the window. | An error that says so and points to `get_state`. |
| `check_config` finds no configuration path at all. | An error, where `mochic check --json` prints one with the code `config`. |

A tool that fails after it has sent commands ends its message with
`(already sent: ...)` and their names, so the client knows what has changed.

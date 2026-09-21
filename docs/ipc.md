# The Mochi IPC protocol

Everything that talks to the daemon speaks this. `mochic` is only a thin
translation from a command line to one JSON object, so anything `mochic` can do,
a three line PowerShell script can do too.

## Transport

| | |
|---|---|
| Command pipe | `\\.\pipe\mochi` |
| Pipe mode | byte type, byte read mode, blocking, remote clients rejected |
| Instances | unlimited |
| Framing | newline delimited JSON (NDJSON) |
| Encoding | UTF-8, no BOM |

One JSON value per line, terminated by a single `\n`. Blank lines are ignored,
so a `\n` works as a keepalive. A line may not exceed 8 MiB. `\r\n` is accepted
on input and never produced on output.

Newline framing was chosen over a length prefix because it survives being typed
by hand, piped through `Get-Content`, and read in a log.

## Commands

A command is one object. The `cmd` field holds the command name in kebab-case
and the arguments sit next to it:

```json
{"cmd":"focus","direction":"left"}
{"cmd":"resize-axis","axis":"horizontal","sizing":"increase"}
{"cmd":"focus-workspace","index":3}
{"cmd":"workspace-padding","monitor":0,"workspace":1,"size":14}
{"cmd":"state"}
```

Fields with a sensible default may be left out: a rule without
`matching_strategy` uses `equals`.

The exchange is one round trip:

1. connect to `\\.\pipe\mochi`,
2. write exactly one command line,
3. read exactly one response line,
4. close, or send another command on the same connection.

A connection may carry any number of commands; each one is answered in order.
A command that takes longer than five seconds is answered with an error so a
hotkey never hangs.

## Responses

Exactly one response per command, tagged by a `response` field:

```json
{"response":"ok"}
{"response":"error","message":"no such monitor"}
{"response":"state","state":{ ... }}
{"response":"query","answer":2}
{"response":"hotkeys","hotkeys":{ ... }}
```

`mochic` exits non-zero and prints `message` on `error`.

## Notifications

Subscriptions run in the opposite direction, because that lets a script and
the daemon start in any order.

1. The subscriber creates `\\.\pipe\<name>` itself, inbound only, one instance.
2. It sends `{"cmd":"subscribe-pipe","name":"<name>"}` on the command pipe.
3. The daemon opens the subscriber pipe as a client and writes one JSON
   notification per line until the pipe breaks.
4. `{"cmd":"unsubscribe-pipe","name":"<name>"}` ends it. So does closing the
   pipe: the daemon drops any subscriber whose write fails.

A notification carries a flattened event and an optional state snapshot:

```json
{"event":"focus-change","window":{"hwnd":852368,"title":"Cargo.toml","exe":"Code.exe"}}
{"event":"manage","window":{"hwnd":852368,"title":"Cargo.toml","exe":"Code.exe"}}
{"event":"unmanage","window":{"hwnd":852368,"title":"","exe":"Code.exe"}}
{"event":"workspace-change","monitor":0,"workspace":2,"name":"3"}
{"event":"layout-change","monitor":0,"workspace":2,"layout":"BSP"}
{"event":"monitors-changed","count":2}
{"event":"reload","path":"C:\\Users\\domin\\mochi.json"}
{"event":"pause","paused":true}
{"event":"session-change","kind":"lock"}
{"event":"stop"}
```

`{"event":"stop"}` is always the last line on a pipe.

A `layout` in a notification, and in the `state` document, is the file's
PascalCase spelling: `BSP`, `VerticalStack`, `UltrawideVerticalStack`. Only the
argument of a `change-layout` command is kebab-case. A subscriber that compares
against `"bsp"` never matches.

`window` is null on a `focus-change` for a window that had already gone by the
time the event was read, and `name` is null on a `workspace-change` for a
workspace that was never named.

## Errors on the wire

| Situation | What the client sees |
|---|---|
| No daemon | opening the pipe fails with `ERROR_FILE_NOT_FOUND`, `mochic` says "mochi is not running" |
| All instances busy | `ERROR_PIPE_BUSY`, the client retries for two seconds |
| Malformed line | `{"response":"error","message":"malformed message: ..."}`, the connection stays open |
| Daemon shutting down | `{"response":"error","message":"mochi is shutting down"}` |

## Talking to it without mochic

```powershell
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream '.', 'mochi', 'InOut'
$pipe.Connect(2000)
$writer = New-Object System.IO.StreamWriter $pipe
$reader = New-Object System.IO.StreamReader $pipe
$writer.WriteLine('{"cmd":"query","target":"monitor-count"}')
$writer.Flush()
$reader.ReadLine()
```

## Command reference

Names match the commands a hotkey file uses, so an existing `whkdrc` imports
with a search and replace.

| Command | Arguments |
|---|---|
| `start` | |
| `stop` | |
| `quickstart` | |
| `toggle-pause`, `reload-configuration`, `retile` | |
| `hotkeys` | |
| `set-hotkeys` | `state`: `enable` `disable` |
| `toggle-game-mode` | |
| `state` | |
| `query` | `target` |
| `focus`, `move` | `direction`: `left` `right` `up` `down` |
| `resize-axis` | `axis`: `horizontal` `vertical`, `sizing`: `increase` `decrease` |
| `resize-edge` | `direction`: `left` `right` `up` `down`, `sizing`: `increase` `decrease` |
| `promote`, `promote-focus` | |
| `toggle-float`, `toggle-float-override`, `toggle-maximize`, `toggle-monocle`, `minimize`, `close` | |
| `manage`, `unmanage` | |
| `stack` | `direction` |
| `unstack` | |
| `stack-all`, `unstack-all` | |
| `focus-stack-window` | `index` |
| `cycle-focus`, `cycle-move`, `cycle-stack`, `cycle-layout`, `cycle-workspace`, `cycle-monitor` | `direction`: `next` `previous` |
| `change-layout` | `layout`: `bsp` `columns` `rows` `vertical-stack` `horizontal-stack` `ultrawide-vertical-stack` `grid` |
| `flip-layout` | `axis` |
| `toggle-tiling` | |
| `focus-workspace`, `move-to-workspace`, `send-to-workspace` | `index` |
| `focus-last-workspace` | |
| `focus-named-workspace`, `move-to-named-workspace`, `send-to-named-workspace` | `name` |
| `workspace-padding`, `container-padding` | `monitor`, `workspace`, `size` |
| `focus-monitor`, `move-to-monitor`, `send-to-monitor` | `index` |
| `focus-follows-mouse`, `mouse-follows-focus`, `border`, `animation` | `state`: `enable` `disable` |
| `window-container-behaviour` | `behaviour`: `create` `append` |
| `toggle-window-container-behaviour` | |
| `cross-monitor-move-behaviour` | `behaviour`: `swap` `insert` `no-op` |
| `window-hiding-behaviour` | `behaviour`: `hide` `minimize` `cloak` |
| `unmanaged-window-operation-behaviour` | `behaviour`: `op` `no-op` |
| `toggle-transparency` | |
| `border-width` | `width` |
| `border-offset` | `offset` |
| `border-colour` | `kind`, `r`, `g`, `b` |
| `border-style` | `style`: `system` `rounded` `square` |
| `animation-duration` | `duration` in ms |
| `animation-style` | `style` |
| `animation-fps` | `fps` |
| `float-rule`, `ignore-rule`, `manage-rule` | `identifier`: `exe` `class` `title` `path`, `id`, `matching_strategy`: `equals` `contains` `starts-with` `ends-with` `regex` |
| `workspace-rule` | the same three, plus `monitor`, `workspace` and `initial_only` |
| `restore-windows` | |
| `subscribe-pipe`, `unsubscribe-pipe` | `name` |

`query` targets: `focused-monitor-index`, `focused-workspace-index`,
`focused-container-index`, `focused-window-index`, `focused-workspace-name`,
`monitor-count`, `window-count`, `paused`, `dry-run`, `config-path`, `version`.

## What `state` answers with

Every command in the table above reaches the model. `state` returns one
document built by the daemon, not `mochi-core`'s internal serialisation, so its
shape is stable across changes to the model:

```json
{
  "version": "0.1.1", "dry_run": false, "paused": false,
  "config_path": "C:\\Users\\you\\mochi.json",
  "app_config_path": "C:\\Users\\you\\applications.json",
  "manage_classes": [],
  "focused_monitor": 0, "focused_workspace": 0, "focused_window": 852368,
  "foreground_window": 852368,
  "window_count": 4,
  "monitors": [{
    "index": 0, "id": 65539, "name": "DISPLAY1", "device": "\\\\.\\DISPLAY1",
    "device_id": "DISPLAY1-SAM7301-0",
    "size": {"left":0,"top":0,"right":3840,"bottom":2160,"width":3840,"height":2160},
    "work_area": {"...": 0}, "dpi": 144, "scale": 1.5,
    "focused_workspace": 0, "last_focused_workspace": 2,
    "workspaces": [{
      "index": 0, "name": "1", "layout": "BSP",
      "flip": {"horizontal": false, "vertical": false},
      "tile": true, "monocle": false, "maximized": false, "visible": true,
      "work_area": {"...": 0},
      "workspace_padding": 14, "container_padding": 10,
      "focused_container": 0, "focused_window": 852368,
      "containers": [{
        "index": 0, "stack": false, "focused_window": 852368,
        "rect": {"left":24,"top":24,"right":1920,"bottom":2088,"width":1896,"height":2064},
        "windows": [{"hwnd":852368,"title":"Cargo.toml","exe":"Code.exe","class":"Chrome_WidgetWin_1","path":"C:\\Program Files\\Code\\Code.exe","rect":{"...":0},"actual_rect":{"...":0},"visible":true,"on_screen":true}]
      }],
      "monocle_container": null, "maximized_window": null, "floating_windows": []
    }]
  }],
  "settings": {
    "transparency": false, "transparency_alpha": 200,
    "border": true, "border_width": 6, "border_offset": -1,
    "border_style": "system",
    "border_colours": {"single":"#ffbbdf","stack":null,"monocle":null,"floating":null,"unfocused":null},
    "animation": false, "animation_duration": 250, "animation_fps": 60,
    "animation_style": "linear"
  },
  "behaviour": {
    "window_hiding_behaviour": "Cloak",
    "cross_monitor_move_behaviour": "Insert",
    "unmanaged_window_operation_behaviour": "Op",
    "window_container_behaviour": "Create",
    "focus_follows_mouse": null,
    "mouse_follows_focus": true,
    "float_override": false,
    "resize_delta": 50,
    "default_workspace_padding": 10,
    "default_container_padding": 10
  },
  "rules": 312, "subscribers": []
}
```

`settings` has exactly those eleven keys and `behaviour` exactly those ten;
both objects are written out in full every time, so a missing key means an
older daemon rather than an unset value. The two use different spellings on
purpose: `settings` echoes back what a `mochic` command set, so its enums are
the kebab-case the command line takes, while `behaviour` mirrors `mochi.json`,
so its enums are the PascalCase the file uses.

What can be null:

| Field | Null when |
|---|---|
| `app_config_path` | the configuration names no `app_specific_configuration_path` |
| `focused_workspace`, `focused_window` | nothing is focused, which is the case before the first window appears |
| `foreground_window` | Windows reports no foreground window, during a switch or while the lock screen is up |
| `monitors[].last_focused_workspace` | only one workspace has been focused since the daemon started, so `focus-last-workspace` has nowhere to go |
| `workspaces[].focused_window`, `containers[].focused_window` | that workspace or container is empty |
| `containers[].rect`, `windows[].rect` | the last layout did not place them: a container added since the last run, a workspace with `tile` off, or a floating window, which has no layout rectangle at all |
| `windows[].actual_rect`, `windows[].on_screen` | the window could not be read, which is what one that has just died looks like |
| `monocle_container`, `maximized_window` | that mode is not on for the workspace, which is the normal case |

A `rect` that is present is the rectangle the last layout gave the container,
in physical pixels, describing the perceived frame: the daemon compensates for
the invisible resize border itself, so these are the numbers a screenshot
shows. A window carries the rectangle of its container, so every window of a
stack reports the same one.

`rect` and `actual_rect` answer two different questions and it is worth being
clear about which is which. `rect` is where the model says the window BELONGS;
`actual_rect` is where it is, measured off the desktop when the document was
built. They normally agree. When they do not, the window is not where Mochi
believes it is, and that is the interesting case: a window it was not allowed
to move, one that ignored the rectangle it was given, or one that has been
dragged since the last layout. `visible` and `on_screen` split the same way:
`visible` is the model's answer, whether the container is showing this window
rather than another of its stack, and `on_screen` is the desktop's, false for a
window that is cloaked or hidden however that came about.

The visual commands (`border*`, `animation*`, `toggle-transparency`) change the
live configuration, reach the border, transparency and animation managers and
redraw the workspace before the response comes back. `settings` reports the
result, so a command and the document never disagree.

## What `hotkeys` answers with

`{"cmd":"hotkeys"}` hands back what the daemon made of the hotkey file, which is
also what `mochic hotkeys` renders as a table:

```json
{
  "path": "C:\\Users\\you\\.config\\mochi\\hotkeys",
  "gate": "all",
  "bindings": [
    {"keys":"alt + h","command":"focus left"},
    {"keys":"alt + shift + g","command":"toggle-game-mode"}
  ],
  "errors": ["line 12: `wiggle` is not a key name"]
}
```

`path` is the file that was loaded, picked the way
[hotkeys.md](hotkeys.md) describes, and null when the daemon was started with
`--no-hotkeys` and so never looked for one. `gate` is `off` then as well, and
`bindings` and `errors` are empty. A daemon whose hotkey file simply does not
exist still reports the path it would have read. `keys` is the chord as Mochi
normalised it,
so a binding written `Shift+ALT+H` comes back as `alt + shift + h` and a typo in
a modifier is visible. `command` is the right hand side of the line, a Mochi
command or a shell line.

`gate` says which bindings fire: `all` normally, `game-mode` while
`{"cmd":"toggle-game-mode"}` is holding everything but its own key for a game,
and `off` after `{"cmd":"set-hotkeys","state":"disable"}`. Only `all` means the
document and the keyboard agree.

`errors` holds one string per line that did not parse, each one starting with
its line number. Those lines are the only ones lost: the rest of the file is
bound, so a broken line never costs the whole keyboard.

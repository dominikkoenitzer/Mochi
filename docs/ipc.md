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

Fields with a sensible default may be left out: `{"cmd":"stop"}` is
`{"cmd":"stop","whkd":false}`, and a rule without `matching_strategy` uses
`equals`.

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
{"event":"layout-change","monitor":0,"workspace":2,"layout":"bsp"}
{"event":"monitors-changed","count":2}
{"event":"reload","path":"C:\\Users\\domin\\mochi.json"}
{"event":"pause","paused":true}
{"event":"session-change","kind":"lock"}
{"event":"stop"}
```

`{"event":"stop"}` is always the last line on a pipe.

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

Names match the commands a whkdrc hotkey file uses, so an existing `whkdrc` imports
with a search and replace.

| Command | Arguments |
|---|---|
| `start` | `whkd` |
| `stop` | `whkd` |
| `quickstart` | |
| `toggle-pause`, `reload-configuration`, `retile` | |
| `state` | |
| `query` | `target` |
| `focus`, `move` | `direction`: `left` `right` `up` `down` |
| `resize-axis` | `axis`: `horizontal` `vertical`, `sizing`: `increase` `decrease` |
| `promote` | |
| `toggle-float`, `toggle-maximize`, `toggle-monocle`, `minimize`, `close` | |
| `manage`, `unmanage` | |
| `stack` | `direction` |
| `unstack` | |
| `cycle-stack`, `cycle-layout`, `cycle-workspace`, `cycle-monitor` | `direction`: `next` `previous` |
| `change-layout` | `layout`: `bsp` `columns` `rows` `vertical-stack` `horizontal-stack` `ultrawide-vertical-stack` `grid` |
| `flip-layout` | `axis` |
| `focus-workspace`, `move-to-workspace` | `index` |
| `focus-last-workspace` | |
| `workspace-padding`, `container-padding` | `monitor`, `workspace`, `size` |
| `focus-monitor`, `move-to-monitor` | `index` |
| `focus-follows-mouse`, `mouse-follows-focus`, `border`, `animation` | `state`: `enable` `disable` |
| `toggle-transparency` | |
| `border-width` | `width` |
| `border-offset` | `offset` |
| `border-colour` | `kind`, `r`, `g`, `b` |
| `border-style` | `style`: `system` `rounded` `square` |
| `animation-duration` | `duration` in ms |
| `animation-style` | `style` |
| `animation-fps` | `fps` |
| `float-rule`, `ignore-rule` | `identifier`: `exe` `class` `title` `path`, `id`, `matching_strategy`: `equals` `contains` `starts-with` `ends-with` `regex` |
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
  "version": "0.1.0", "dry_run": false, "paused": false,
  "config_path": "C:\Users\you\mochi.json",
  "manage_classes": [],
  "focused_monitor": 0, "focused_workspace": 0, "focused_window": 852368,
  "window_count": 4,
  "monitors": [{
    "index": 0, "name": "DISPLAY1", "device": "\\.\DISPLAY1",
    "size": {"left":0,"top":0,"right":3840,"bottom":2160,"width":3840,"height":2160},
    "work_area": {"...": 0}, "dpi": 144, "scale": 1.5,
    "focused_workspace": 0,
    "workspaces": [{
      "index": 0, "name": "1", "layout": "BSP", "visible": true,
      "monocle": false, "maximized": false,
      "workspace_padding": 14, "container_padding": 10,
      "focused_container": 0,
      "containers": [{
        "index": 0, "stack": false,
        "rect": {"left":24,"top":24,"right":1920,"bottom":2088,"width":1896,"height":2064},
        "windows": [{"hwnd":852368,"title":"Cargo.toml","exe":"Code.exe","class":"Chrome_WidgetWin_1","visible":true}]
      }],
      "monocle_container": null, "maximized_window": null, "floating_windows": []
    }]
  }],
  "settings": { "border": true, "transparency": true },
  "behaviour": { "window_hiding_behaviour": "Cloak", "cross_monitor_move_behaviour": "Insert" },
  "rules": 312, "subscribers": []
}
```

A `rect` is always the rectangle the last layout gave the container, in physical
pixels, describing the perceived frame: the daemon compensates for the invisible
resize border itself, so these are the numbers a screenshot shows.

The visual commands (`border*`, `animation*`, `toggle-transparency`) change the
live configuration, reach the border, transparency and animation managers and
redraw the workspace before the response comes back. `settings` reports the
result, so a command and the document never disagree.

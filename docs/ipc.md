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

Subscriptions run in the opposite direction, because that lets a status bar and
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

## State so far

Until the tiling model lands, the daemon answers everything that needs a
monitor / workspace / container tree with

```json
{"response":"error","message":"`focus` is accepted by the protocol but the tiling model from mochi-core is not wired up yet"}
```

The protocol itself is final; only the handlers behind it are still empty.

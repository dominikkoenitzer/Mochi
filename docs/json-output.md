# JSON output

`mochic` prints text for a person. Scripts and other programs can ask for JSON
instead: give a command `--json` and it prints exactly one JSON document on
stdout and nothing else, whether it succeeds or fails.

`--json` belongs to the command, so it goes after the subcommand:

```
mochic query monitor-count --json
mochic check --json
mochic check C:\path\to\mochi.json --json
```

The document is pretty printed and followed by a newline. Parse it as JSON and
do not depend on the whitespace.

## Stability

Within a major version, fields are added, never renamed or removed. Error codes
are never renamed. New codes may be added, so treat a code you do not know like
`internal`.

Every success document `mochic` builds itself has a top level `version` field
with the version of `mochic`. The documents that come from the daemon (`state`,
`doctor`, `why`, `hotkeys`) and the schema are printed as they are, without it.
The error document has no `version` either.

## Commands

| Command | Prints |
|---|---|
| `state --json` | The daemon's state document. |
| `query TARGET --json` | One value in a small document. |
| `check [PATH] --json` | The findings for a configuration file. |
| `schema --json` | The schema, or where it was written. |
| `doctor --json` | The daemon's findings document. |
| `why --json` | The daemon's explanation document. |
| `hotkeys --json` | The daemon's bindings document. |

`subscribe` has no `--json`. It is not a single document: it already prints one
JSON notification per line until it is interrupted. The action commands
(`focus`, `move`, `stop`, `retile` and the rest) have no `--json` either; when
they succeed they print nothing.

### state

The same document `mochic state` prints without the flag. The flag only makes a
failure come out as an error document. The top level fields:

| Field | Type | Meaning |
|---|---|---|
| `version` | string | The daemon's version. |
| `dry_run` | boolean | True when the daemon was started with `--dry-run`. |
| `paused` | boolean | True while management is paused. |
| `config_path` | string | The configuration file the daemon loaded. |
| `app_config_path` | string or null | The application rules file, when the configuration names one. |
| `manage_classes` | array of strings | The classes given with `--manage-class`. |
| `focused_monitor` | number | Index of the focused monitor. |
| `focused_workspace` | number or null | Index of the focused workspace on that monitor. |
| `focused_window` | number or null | Handle of the focused managed window. |
| `foreground_window` | number or null | Handle of the window in front, managed or not. |
| `window_count` | number | Number of managed windows. |
| `monitors` | array of monitors | One per monitor, in index order. See below. |
| `settings` | object | The border, transparency and animation settings in force. See below. |
| `behaviour` | object | The behaviour settings in force. See below. |
| `rules` | number | Number of window rules loaded, across every rule list. |
| `subscribers` | array of strings | The pipes that receive notifications. |
| `fullscreen` | array | One object per fullscreen window: `window` (number, its handle), `monitor` (number, the monitor it holds) and `frozen` (boolean, true while the window is on screen and so holds its monitor). |
| `elevated` | object | `configured` (boolean, what the `elevated` key in the file says) and `running` (boolean, whether this daemon process runs as administrator). The two differ after the prompt was declined, or when the key was changed while the daemon runs. |

A window handle is a number here, the `HWND` value. `why` and `doctor` write
handles as hex strings instead.

#### Monitor

| Field | Type | Meaning |
|---|---|---|
| `index` | number | Zero-based monitor index, as the commands take it. |
| `id` | number | The `HMONITOR` handle. |
| `name` | string | The friendly name, for example `DISPLAY1`. |
| `device` | string | The GDI device name, for example `\\.\DISPLAY1`. |
| `device_id` | string | The display's model name. Two panels of the same model have the same one. |
| `size` | rect | The whole display, in virtual desktop coordinates. |
| `work_area` | rect | The part of `size` the taskbar does not cover. |
| `dpi` | number | The display's DPI. 96 is 100 percent scaling. |
| `scale` | number | `dpi` divided by 96. |
| `focused_workspace` | number | Index of the workspace shown on this monitor. |
| `last_focused_workspace` | number or null | The workspace shown before it, which `focus-last-workspace` goes back to. |
| `workspaces` | array of workspaces | Every workspace of this monitor, in index order. |

#### Workspace

| Field | Type | Meaning |
|---|---|---|
| `index` | number | Zero-based workspace index on its monitor. |
| `name` | string | The name from the configuration, or the index plus one when it has none. |
| `layout` | string | The layout in use: `BSP`, `Columns`, `Rows`, `VerticalStack`, `HorizontalStack`, `UltrawideVerticalStack` or `Grid`. |
| `flip` | object | `horizontal` and `vertical`, both booleans: whether the layout is mirrored along that axis. |
| `tile` | boolean | False when tiling is off for this workspace. |
| `monocle` | boolean | True while one container fills the workspace. |
| `maximized` | boolean | True while one window is maximized over it. |
| `visible` | boolean | True for the workspace its monitor is showing. |
| `work_area` | rect | The area this workspace tiles into. |
| `workspace_padding` | number | Outer padding in logical pixels. |
| `container_padding` | number | Padding between containers in logical pixels. |
| `focused_container` | number | Index of the focused container. |
| `focused_window` | number or null | Handle of the focused window. |
| `containers` | array of containers | The tiled containers, in order. |
| `monocle_container` | container or null | The container in monocle, which is not in `containers` while it is. |
| `maximized_window` | window or null | The maximized window. |
| `floating_windows` | array of windows | The floating windows. Their `rect` is null. |

#### Container

| Field | Type | Meaning |
|---|---|---|
| `index` | number | Zero-based container index on its workspace. |
| `rect` | rect or null | The tile the last layout gave it. |
| `focused_window` | number or null | Handle of the window it is showing. |
| `stack` | boolean | True when it holds more than one window. |
| `windows` | array of windows | Its windows. A stack shows one and hides the rest. |

#### Window

| Field | Type | Meaning |
|---|---|---|
| `hwnd` | number | The window handle. |
| `title` | string | The title. |
| `exe` | string | The executable name. |
| `class` | string | The window class. |
| `path` | string | The full path of the executable. |
| `rect` | rect or null | Where Mochi says it belongs: its container's tile, shared by every window of a stack. Null for a floating window. |
| `actual_rect` | rect or null | Where it really is on screen. Null when it could not be read. |
| `min_size` | object or null | The smallest size the window said it accepts, as `width` and `height` in logical pixels. Null until it has answered. |
| `visible` | boolean | True when its container is showing it. |
| `on_screen` | boolean or null | What the desktop says: false when it is cloaked or hidden. Null when it could not be read. |

When `rect` and `actual_rect` differ, the window is not where Mochi thinks it
is.

#### Rect

`left`, `top`, `right`, `bottom`, `width` and `height`, all numbers, in
physical pixels.

#### Settings

| Field | Type | Meaning |
|---|---|---|
| `transparency` | boolean | Unfocused windows are faded. |
| `transparency_alpha` | number | The alpha unfocused windows get, 0 to 255. |
| `border` | boolean | A border is drawn around the focused window. |
| `border_width` | number | Border thickness in physical pixels, not scaled by DPI. |
| `border_offset` | number | How far the border sits outside the frame. Negative pulls it inwards. |
| `border_style` | string | `system`, `rounded` or `square`. |
| `border_colours` | object | `single`, `stack`, `monocle`, `floating` and `unfocused`, each a `#rrggbb` string, or null when neither the file nor a command set it, and the border draws its own default. |
| `animation` | boolean | Move and resize animations are on. |
| `animation_duration` | number | Animation length in milliseconds. |
| `animation_fps` | number | Animation frame rate. |
| `animation_style` | string | The easing curve, spelled as `animation-style` takes it, for example `linear` or `ease-in-out-cubic`. |

#### Behaviour

These values are spelled the way `mochi.json` spells them, not the way the
command line does.

| Field | Type | Meaning |
|---|---|---|
| `window_hiding_behaviour` | string | `Hide`, `Minimize` or `Cloak`. |
| `cross_monitor_move_behaviour` | string | `Swap`, `Insert` or `NoOp`. |
| `unmanaged_window_operation_behaviour` | string | `Op` or `NoOp`. |
| `window_container_behaviour` | string | `Create` or `Append`. |
| `focus_follows_mouse` | string or null | `Windows` or `Mochi`, or null when it is off. |
| `mouse_follows_focus` | boolean | The cursor moves to a newly focused window. |
| `float_override` | boolean | Every new window floats. |
| `resize_delta` | number | How many pixels one `resize-axis` step moves a boundary. |
| `default_workspace_padding` | number | Outer padding of a workspace that sets none, in logical pixels. |
| `default_container_padding` | number | Container padding of a workspace that sets none, in logical pixels. |

### query

```json
{
  "version": "0.1.0",
  "target": "focused-workspace-index",
  "value": 2
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | string | The version of `mochic`. |
| `target` | string | The target as typed. |
| `value` | number, string, boolean or null | The daemon's answer. |

| Target | Value |
|---|---|
| `focused-monitor-index` | number |
| `focused-workspace-index` | number |
| `focused-container-index` | number |
| `focused-window-index` | number, the index inside the focused container |
| `focused-workspace-name` | string |
| `monitor-count` | number |
| `window-count` | number |
| `paused` | boolean |
| `dry-run` | boolean |
| `config-path` | string |
| `version` | string, the daemon's version |

### check

`check` needs no daemon. The exit code is the same as without `--json`: 0 when
the file is usable, 1 when it is not.

```json
{
  "version": "0.1.0",
  "path": "C:\\Users\\you\\mochi.json",
  "source": "given on the command line",
  "kind": "config",
  "usable": true,
  "headline": null,
  "errors": [],
  "warnings": [],
  "notes": []
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | string | The version of `mochic`. |
| `path` | string | The file that was checked. |
| `source` | string | Where the path came from: the command line, `MOCHI_CONFIG`, or the default path. |
| `kind` | string | `config` for a `mochi.json`, `app_rules` for an application rules file. |
| `usable` | boolean | True when the daemon could start with this file. Warnings do not make a file unusable. |
| `headline` | string or null | One sentence saying what to fix first. Null when the file is usable. `the configuration could not be read` when the file could not be read. |
| `errors` | array of strings | What makes the file unusable. An entry may span several lines. When the file could not be read, its reason is the only entry. |
| `warnings` | array of strings | What the daemon would carry on past, the hotkey file findings included. |
| `notes` | array of strings | Plain facts about what was read, one line each. |

When there is no configuration path at all, `check --json` prints an error
document with the code `config` instead.

### schema

Without `--output`, `schema --json` prints the schema exactly as `schema` does.
With `--output PATH --json` it prints this instead of `wrote PATH`:

```json
{
  "version": "0.1.0",
  "path": "mochi.schema.json"
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | string | The version of `mochic`. |
| `path` | string | The file the schema was written to. |

A file that cannot be written is an error document with the code `io`.

### doctor

| Field | Type | Meaning |
|---|---|---|
| `managed` | number | Number of managed windows. |
| `off_screen` | number | Number of windows Mochi has taken off screen. |
| `elevated` | boolean | True when this daemon process runs as administrator. |
| `findings` | array | One object per finding. Empty when nothing is wrong. |

Each finding:

| Field | Type | Meaning |
|---|---|---|
| `kind` | string | `gone`, `unreachable`, `hole`, `stale-record`, `wrong-record`, `cannot-restore`, `hotkeys-blocked` or `not-elevated`. |
| `hwnd` | string | The window handle. |
| `title` | string | The window title. |
| `exe` | string | The executable name. |
| `detail` | string | What is wrong, in a sentence. |

`hotkeys-blocked` and the `elevated, out of reach` reason of `why` only occur
while the daemon is not elevated. `not-elevated` is the other way round: the
`elevated` key in the file is on and this daemon runs with normal rights, either
because the prompt was declined or because the key was switched on while the
daemon was running. It names no window, so `hwnd`, `title` and `exe` are empty
strings, and `detail` says that windows running as administrator stay out of
reach until the daemon is stopped and started again with `mochic start` and the
prompt is accepted.

### why

Fields marked "sometimes" are only there when they apply.

| Field | Type | Meaning |
|---|---|---|
| `hwnd` | string | The handle of the window in front, in hex, for example `0x1a2b`. |
| `title` | string | Its title. |
| `exe` | string | Its executable name. |
| `class` | string | Its window class. |
| `managed` | boolean | True when Mochi is tiling it. |
| `monitor` | number, sometimes | The monitor it is on, when managed. |
| `workspace` | number, sometimes | The workspace it is on, when managed. |
| `fullscreen_monitor` | number, sometimes | The monitor it holds fullscreen, when it does. |
| `reason` | string, sometimes | Why it is left alone, when not managed. One of the values below. |
| `scratchpad` | string, sometimes | The scratchpad that holds it, when `reason` is `scratchpad`. |
| `overridable` | boolean, sometimes | Only with a reason from the second table: whether a `manage_rules` entry or `manage-rule` may overrule it. |
| `hotkeys_blocked` | boolean, sometimes | Only there when true: the window runs with more rights than Mochi, so no hotkey works while it has the focus. |

The daemon asks in this order and gives the first reason that holds.

| `reason` | Meaning |
|---|---|
| `scratchpad` | A scratchpad holds the window. |
| `paused` | Management is paused. |
| `manage-class` | The daemon was started with `--manage-class` and the window's class is not one of them. |
| `rule` | A rule in the configuration ignores it. |
| `unknown` | Nothing rules it out. It has only just appeared, or an earlier attempt to manage it failed. |

Or the window itself is not something Mochi tiles. These come before `rule`
and `unknown`. A `manage_rules` entry overrules the ones marked overridable,
and when one does, `why` goes on to `rule` or `unknown`.

| `reason` | `overridable` | Meaning |
|---|---|---|
| `not visible` | false | The window is not visible. |
| `cloaked` | false | Windows is hiding it: another virtual desktop, a suspended app, or another program cloaked it. |
| `child window` | false | It is not a top-level window. |
| `tool window` | true | A tool window that does not ask for a taskbar button. |
| `no-activate window` | true | It refuses to be activated, like an overlay or an on-screen keyboard. |
| `owned window` | true | A dialog, palette or popup owned by another window. |
| `no title` | true | It has no title and asks for no taskbar button. |
| `too small` | false | It is too small in at least one direction. |
| `shell class` | false | Its class belongs to the Windows shell. |
| `shell process` | false | Its process belongs to the Windows shell. |
| `elevated, out of reach` | false | It runs as administrator and Mochi does not (the `elevated` setting is off, or the daemon was started with normal rights anyway), so Windows refuses every call against it. |
| `always on top` | true | An always-on-top overlay that does not ask for a taskbar button. |
| `click-through overlay` | true | Mouse input falls straight through it. |

### hotkeys

| Field | Type | Meaning |
|---|---|---|
| `path` | string or null | The hotkey file in use. |
| `gate` | string | `all`, `game-mode` or `off`. |
| `bindings` | array | One object per binding: `keys` (string), `command` (string), and `mode` (string) for a binding inside a `mode` block. |
| `errors` | array of strings | The lines of the file that did not parse. |

## Errors

With `--json`, a failure prints exactly one document on stdout and nothing
else:

```json
{
  "error": {
    "code": "daemon_not_running",
    "message": "mochi is not running. Start it with `mochic start`."
  }
}
```

`message` is the same text `mochic` prints to stderr without `--json`. For
`usage` it is the first line of the parser's message.

| Code | When |
|---|---|
| `daemon_not_running` | No daemon is running. |
| `ipc` | The daemon could not be reached, or answered with something that does not fit the command. |
| `daemon` | The daemon understood the command and refused it. |
| `config` | `check` found no configuration path. |
| `io` | A file could not be written. |
| `usage` | The command line did not parse and contained `--json`. `--help` and `--version` keep their normal output. |
| `internal` | Anything else. |

## Exit codes

| Code | When |
|---|---|
| `0` | Success, or `check` found the file usable. |
| `1` | A failure at run time, or `check` found the file unusable. |
| `2` | The command line did not parse (code `usage`). |

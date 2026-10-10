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
| `monitors` | array | One object per monitor, with its workspaces, their containers and the windows in each. |
| `settings` | object | The visual settings in force. |
| `behaviour` | object | The behaviour settings in force, such as `window_hiding_behaviour` and `focus_follows_mouse`. |
| `rules` | number | Number of window rules loaded. |
| `subscribers` | array of strings | The pipes that receive notifications. |
| `fullscreen` | array | One object per fullscreen window: `window` (handle), `monitor` (index), `frozen` (boolean). |

The nested fields follow the daemon's model and are best read from a live
document.

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
| `findings` | array | One object per finding. Empty when nothing is wrong. |

Each finding:

| Field | Type | Meaning |
|---|---|---|
| `kind` | string | `gone`, `unreachable`, `hole`, `stale-record`, `wrong-record`, `cannot-restore` or `hotkeys-blocked`. |
| `hwnd` | string | The window handle. |
| `title` | string | The window title. |
| `exe` | string | The executable name. |
| `detail` | string | What is wrong, in a sentence. |

### why

Fields marked "sometimes" are only there when they apply.

| Field | Type | Meaning |
|---|---|---|
| `hwnd` | string | The handle of the window in front. |
| `title` | string | Its title. |
| `exe` | string | Its executable name. |
| `class` | string | Its window class. |
| `managed` | boolean | True when Mochi is tiling it. |
| `monitor` | number, sometimes | The monitor it is on, when managed. |
| `workspace` | number, sometimes | The workspace it is on, when managed. |
| `fullscreen_monitor` | number, sometimes | The monitor it holds fullscreen, when it does. |
| `reason` | string, sometimes | Why it is left alone, when not managed: `scratchpad`, `paused`, `manage-class`, `rule`, `unknown`, or the reason the window itself cannot be tiled. |
| `scratchpad` | string, sometimes | The scratchpad that holds it, when `reason` is `scratchpad`. |
| `overridable` | boolean, sometimes | Whether a `manage-rule` could change that, when the reason is the window itself. |
| `hotkeys_blocked` | boolean, sometimes | True when the window runs with more rights than Mochi, so no hotkey works while it has the focus. |

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

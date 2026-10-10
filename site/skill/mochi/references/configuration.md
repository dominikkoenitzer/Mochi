# Configuration

## Where it lives

Mochi reads one JSON file. It takes the first of these that applies:

1. `--config <path>`, on `mochi` or on `mochic start`
2. the environment variable `MOCHI_CONFIG`
3. `%USERPROFILE%\mochi.json`

`mochic quickstart` (and the installer) writes this starting file when there is none:

```json
{
  "$schema": "https://get-mochi.vercel.app/schema.json",
  "window_hiding_behaviour": "Cloak",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "monitors": []
}
```

Every key is optional and has a default. The file must be a JSON object.

## Format

- Keys are `snake_case`.
- Enum values in the file are `PascalCase`: `"BSP"`, `"EaseOutQuad"`, `"Equals"`. On the `mochic` command line the same values are kebab-case: `bsp`, `ease-out-quad`, `equals`.
- `$schema` is only read by editors. Point it at `/schema.json` on the download site, or write a local copy with `mochic schema --output mochi.schema.json` and point at that. Use `--output`, not `>`: Windows PowerShell 5.1 writes a redirect as UTF-16, which is not valid JSON.

## Validate and reload

1. Back up: `Copy-Item "$env:USERPROFILE\mochi.json" "$env:USERPROFILE\mochi.json.bak"`
2. Edit.
3. `mochic check` (or `mochic check <path>` for another file). It needs no daemon. It reports parse errors with line and column, rules that cannot work, keys that are read but ignored, and problems in the hotkey file. The last line is `ok: <path> is usable` or `not usable: <reason>`; the exit code is 0 or 1.
4. Save. Mochi watches the file and applies the change at once. `mochic reload-configuration` forces a re-read of this file and the hotkey file.
5. A file that does not parse is not applied. Mochi keeps the previous configuration and writes the error to the log in `%LOCALAPPDATA%\mochi`.

A key the file names replaces a value set with a `mochic` command since the last load; a key the file leaves out keeps the command's value. Rules added with `mochic ignore-rule`, `float-rule`, `manage-rule` or `workspace-rule` survive a reload but are never written to the file and end when Mochi stops.

## Top level keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `$schema` | string | none | Schema for the editor. |
| `app_specific_configuration_path` | string | none | Path to an `applications.json` rule file merged into the rule lists. `%USERPROFILE%` and `$Env:USERPROFILE` are expanded. |
| `window_hiding_behaviour` | `Cloak`, `Minimize`, `Hide` | `Cloak` | How windows of inactive workspaces are taken off screen. |
| `cross_monitor_move_behaviour` | `Swap`, `Insert`, `NoOp` | `Swap` | What a move past a monitor edge does. |
| `window_container_behaviour` | `Create`, `Append` | `Create` | A new window gets its own container, or stacks onto the focused one. |
| `unmanaged_window_operation_behaviour` | `Op`, `NoOp` | `Op` | Whether commands act on an unmanaged focused window. |
| `mouse_follows_focus` | boolean | `true` | Move the cursor into a window when it takes focus. |
| `focus_follows_mouse` | `Windows`, `Mochi` | unset (off) | Focus the window under the cursor, by the Windows setting or by Mochi itself. Not a boolean. |
| `float_override` | boolean | `false` | Every new window floats. |
| `fullscreen_passthrough` | boolean | `true` | Leave a borderless fullscreen window alone; its monitor is not retiled until it leaves fullscreen. |
| `resize_delta` | integer | `50` | Pixels one `resize-axis` step moves. |
| `default_workspace_padding` | integer | `10` | Gap between a workspace and the screen edge. |
| `default_container_padding` | integer | `10` | Gap between tiles. |
| `minimum_window_width` | integer | `64` | Tiles are never made narrower than this. |
| `minimum_window_height` | integer | `64` | Tiles are never made shorter than this. |
| `work_area_offset` | offset | none | Pixels taken off every monitor's work area. |
| `global_work_area_offset` | offset | none | Same as `work_area_offset`, which wins when both are set. |
| `border` | boolean | `false` | Draw a border around the focused window. |
| `border_width` | integer | `6` | Border thickness in physical pixels. |
| `border_offset` | integer | `-1` | Distance outside the window frame; negative pulls it in. |
| `border_style` | `System`, `Rounded`, `Square` | `System` | Corner shape. |
| `border_colours` | object | none | Colours per window kind, see below. |
| `transparency` | boolean | `false` | Make unfocused windows translucent. Chromium, Electron and UWP windows always stay opaque. |
| `transparency_alpha` | integer 0 to 255 | `200` | Opacity of unfocused windows; 255 is opaque. |
| `transparency_ignore_rules` | rule list | `[]` | Windows that stay opaque. |
| `animation` | object | none | Move and resize animation, see below. |
| `stackbar` | object | none | Accepted so copied configs validate. Nothing is drawn. |
| `ignore_rules` | rule list | `[]` | Windows Mochi never touches. |
| `manage_rules` | rule list | `[]` | Windows Mochi manages although its checks would skip them. |
| `floating_applications` | rule list | `[]` | Windows that are managed but never tiled. |
| `tray_and_multi_window_applications` | rule list | `[]` | Applications that keep a hidden window alive in the tray. |
| `object_name_change_applications` | rule list | `[]` | Applications that reuse one window and only change its title. |
| `slow_application_identifiers` | rule list | `[]` | Applications that need extra time before their window is ready. |
| `fullscreen_passthrough_ignore_rules` | rule list | `[]` | Windows tiled even when they go fullscreen. |
| `border_overflow_applications` | rule list | `[]` | Read, not acted on. |
| `layered_whitelist` | rule list | `[]` | Read, not acted on. |
| `scratchpads` | list | `[]` | Windows shown and hidden by name, see below. |
| `monitors` | list | `[]` | Per monitor settings in physical order, see below. |
| `monitor_index_preferences` | object | none | Monitor index to rectangle; pins an entry to the screen at that position. |
| `display_index_preferences` | object | none | Monitor index to display id; pins an entry to a physical display. |

Padding and minimum sizes are in logical pixels. Border width and offset are physical pixels.

## Rules

Every rule list takes the same object:

| Key | Values | Default |
|---|---|---|
| `kind` | `Exe`, `Class`, `Title`, `Path` | required |
| `id` | string to compare with | required |
| `matching_strategy` | `Legacy`, `Equals`, `DoesNotEqual`, `StartsWith`, `DoesNotStartWith`, `EndsWith`, `DoesNotEndWith`, `Contains`, `DoesNotContain`, `Regex` | `Legacy` |

`Exe` is the file name, `Path` the full path of the executable. `Legacy` treats `id` as a regular expression when it contains regex characters and as an exact match otherwise; write an explicit strategy. An entry may be a list of rule objects, and then all of them must match.

```json
"ignore_rules": [
  { "kind": "Exe", "id": "Taskmgr.exe", "matching_strategy": "Equals" },
  { "kind": "Title", "id": " - Peek", "matching_strategy": "EndsWith" }
],
"floating_applications": [
  { "kind": "Title", "id": "Picture-in-Picture", "matching_strategy": "Equals" }
]
```

To find the exe, class and title of a window, focus it and run `mochic why --json`.

## monitors

One entry per physical monitor, in order. Each entry:

| Key | Type | Meaning |
|---|---|---|
| `workspaces` | list | Workspaces of this monitor, in order. |
| `work_area_offset` | offset | Overrides the top level offset for this monitor. |
| `window_based_work_area_offset` | offset | Extra offset for workspaces that set `apply_window_based_work_area_offset`. |
| `window_based_work_area_offset_limit` | integer, default `1` | The extra offset stops once a workspace holds more containers than this. |

Each workspace:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | index | Name for `focus-named-workspace` and `mochic state`. |
| `layout` | layout | `BSP` | Starting layout. |
| `workspace_padding` | integer | `default_workspace_padding` | Gap to the screen edge. |
| `container_padding` | integer | `default_container_padding` | Gap between tiles. |
| `layout_rules` | object | none | Window count to layout, for example `{ "1": "BSP", "3": "VerticalStack" }`. |
| `workspace_rules` | rule list | `[]` | Windows sent here every time they appear. |
| `initial_workspace_rules` | rule list | `[]` | Windows sent here the first time they appear. |
| `apply_window_based_work_area_offset` | boolean | `false` | Apply the monitor's window based offset here. |
| `float_override` | boolean | `false` | Every window that opens here floats. |

Layouts: `BSP`, `Columns`, `Rows`, `VerticalStack`, `HorizontalStack`, `UltrawideVerticalStack`, `Grid`.

```json
"monitors": [
  {
    "workspaces": [
      { "name": "web", "layout": "BSP" },
      { "name": "chat", "layout": "Rows",
        "workspace_rules": [ { "kind": "Exe", "id": "thunderbird.exe", "matching_strategy": "Equals" } ] }
    ]
  }
]
```

## offset

Pixels taken off each side. Positive shrinks the area, negative grows it.

```json
{ "left": 0, "top": 40, "right": 0, "bottom": 0 }
```

## border_colours

Keys: `single` (focused window alone), `stack`, `monocle`, `floating`, `unfocused`. Each takes a hex string (`"#ffbbdf"` or `"#fbd"`), a channel object (`{ "r": 255, "g": 187, "b": 223 }`) or a Win32 `COLORREF` integer (`0x00bbggrr`, so `16711680` is blue).

```json
"border": true,
"border_colours": { "single": "#ffbbdf", "unfocused": "#313244" }
```

## animation

| Key | Type | Default |
|---|---|---|
| `enabled` | boolean | `false` |
| `duration` | integer, milliseconds | `250` |
| `style` | easing style | `Linear` |
| `fps` | integer | `60` |

Easing styles that work in the file and on the command line: `Linear`, `EaseInSine`, `EaseOutSine`, `EaseInOutSine`, `EaseInQuad`, `EaseOutQuad`, `EaseInOutQuad`, `EaseInCubic`, `EaseOutCubic`, `EaseInOutCubic`. The file also accepts the Quart, Quint, Expo, Circ, Back, Elastic and Bounce curves named in the schema.

```json
"animation": { "enabled": true, "duration": 250, "style": "EaseOutQuad", "fps": 60 }
```

## scratchpads

A window kept out of the layout and shown with one command, `mochic toggle-scratchpad <name>`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Name the commands use. |
| `match` | rule object or list | required | The window that belongs to it. Match on title or class, not on the exe alone. |
| `command` | string | none | Run through `cmd.exe` when no window matches. |
| `width` | number 0.1 to 1 | `0.6` | Fraction of the work area. |
| `height` | number 0.1 to 1 | `0.5` | Fraction of the work area. |
| `hide_on_focus_loss` | boolean | `true` | Hide it when another application takes focus. |

```json
"scratchpads": [
  {
    "name": "term",
    "match": { "kind": "Title", "id": "scratch", "matching_strategy": "Equals" },
    "command": "wt.exe -w new --title scratch --suppressApplicationTitle"
  }
]
```

## A full example

```json
{
  "$schema": "https://get-mochi.vercel.app/schema.json",
  "window_hiding_behaviour": "Cloak",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "mouse_follows_focus": true,
  "border": true,
  "border_width": 4,
  "border_colours": { "single": "#ffbbdf", "unfocused": "#313244" },
  "animation": { "enabled": true, "duration": 200, "style": "EaseOutQuad" },
  "monitors": [
    {
      "workspaces": [
        { "name": "1", "layout": "BSP" },
        { "name": "2", "layout": "Columns" },
        { "name": "3", "layout": "Grid" }
      ]
    }
  ],
  "ignore_rules": [
    { "kind": "Exe", "id": "Taskmgr.exe", "matching_strategy": "Equals" }
  ],
  "floating_applications": [
    { "kind": "Exe", "id": "Calculator", "matching_strategy": "Contains" }
  ]
}
```

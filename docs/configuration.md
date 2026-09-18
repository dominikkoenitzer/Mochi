# Configuration

Mochi reads `%USERPROFILE%\mochi.json`. A `--config` argument wins over the
`MOCHI_CONFIG` environment variable, which wins over that default path.

The key names follow the common tiling window manager conventions, so an existing JSON config works after the
`$schema` line is swapped. See [import.md](import.md).

Values in the file use `PascalCase` for enums
(`"BSP"`, `"EaseOutQuad"`, `"Equals"`). The same values on the `mochic` command
line are kebab-case (`bsp`, `ease-out-quad`, `equals`).

Write the current schema next to the config and point `$schema` at it:

```
mochic schema > mochi.schema.json
```

## Top level

| Key | Type | Default | Meaning |
|---|---|---|---|
| `$schema` | string | none | Schema URL or path, only used by the editor. |
| `app_specific_configuration_path` | string | none | Path to a community style `applications.json` rule file, merged into the rule sets below. `$Env:USERPROFILE` is expanded. |
| `window_hiding_behaviour` | `Cloak`, `Minimize`, `Hide` | `Cloak` | How windows of inactive workspaces disappear. |
| `cross_monitor_move_behaviour` | `Swap`, `Insert` | `Swap` | What happens when a window is moved past the edge of a monitor. |
| `mouse_follows_focus` | boolean | `true` | Warp the cursor into a window when it takes focus. |
| `focus_follows_mouse` | boolean | `false` | Focus whatever window the cursor moves over. |
| `default_workspace_padding` | integer | `10` | Gap between a workspace and the screen edge, in logical pixels. |
| `default_container_padding` | integer | `10` | Gap between tiled containers, in logical pixels. |
| `border` | boolean | `false` | Draw a border around the focused window. |
| `border_width` | integer | `6` | Border thickness in physical pixels, not scaled by DPI. |
| `border_offset` | integer | `-1` | How far the border sits outside the window frame, negative pulls it in. |
| `border_style` | `System`, `Rounded`, `Square` | `System` | Border corner shape. |
| `transparency` | boolean | `false` | Make unfocused windows transparent. |
| `transparency_alpha` | integer 0 to 255 | `200` | Opacity of unfocused windows, 255 is opaque. |
| `transparency_ignore_rules` | rule array | `[]` | Windows that stay opaque. They still get a border. |
| `border_colours` | object | none | Border colour per window kind, see below. |
| `animation` | object | none | Move and resize animation, see below. |
| `stackbar` | object | none | Tab bar for stacked containers, see below. Parsed so a copied config keeps validating; nothing is ever drawn above a window. |
| `ignore_rules` | rule array | `[]` | Windows Mochi never touches. |
| `manage_rules` | rule array | `[]` | Windows Mochi manages even though the usual checks say no. |
| `floating_applications` | rule array | `[]` | Windows that are managed but never tiled. |
| `monitors` | array | `[]` | Per monitor settings in physical order, see below. |
| `work_area_offset` | offset object | none | Pixels taken off every monitor's work area, to leave room for something else on screen. |
| `unmanaged_window_operation_behaviour` | `Op`, `NoOp` | `Op` | Whether commands still act when the focused window is not managed. |
| `monitor_index_preferences` | object | none | Monitor index to rect, pins an index to the screen at that position. |
| `display_index_preferences` | object | none | Monitor index to display id, pins an index to a physical display. |

## border_colours

Hex strings such as `"#ffbbdf"`. Every key is optional.

| Key | Applies to |
|---|---|
| `single` | Focused window on its own. |
| `stack` | Focused window inside a stack. |
| `monocle` | Focused window in monocle mode. |
| `floating` | Focused floating window. |
| `unfocused` | Everything that is not focused. |

## animation

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enabled` | boolean | `false` | Animate moves and resizes. |
| `duration` | integer | `250` | Length of one animation in milliseconds. |
| `style` | easing style | `Linear` | Easing curve, see the list below. |
| `fps` | integer | `60` | Frames per second while animating. |

## stackbar

Off by default and not drawn at all: nothing is ever painted above a window
unless the configuration asks for it. The shape is already fixed, so a copied config keeps
validating.

| Key | Type | Meaning |
|---|---|---|
| `height` | integer | Bar height in logical pixels. |
| `mode` | `Always`, `Never`, `OnStack` | When the bar is shown. |
| `label` | `Process`, `Title` | What a tab is labelled with. |
| `tabs` | object | `width`, `focused_text`, `unfocused_text`, `background`, `font_family`, `font_size`. |

## monitors

`monitors` is an array, one entry per physical monitor.

| Key | Type | Meaning |
|---|---|---|
| `workspaces` | array | The workspaces of this monitor, in order. |
| `work_area_offset` | offset object | Overrides the global `work_area_offset` for this monitor. |

Each entry of `workspaces`:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | index | Workspace name, shown by bars and `mochic state`. |
| `layout` | layout | `BSP` | Layout this workspace starts with. |
| `workspace_padding` | integer | `default_workspace_padding` | Gap to the screen edge. |
| `container_padding` | integer | `default_container_padding` | Gap between containers. |
| `layout_rules` | object | none | Window count to layout, for example `{"1": "BSP", "3": "VerticalStack"}`. The highest count that is not above the current one wins. |
| `initial_workspace_rules` | rule array | `[]` | Windows sent here the first time they appear. |
| `workspace_rules` | rule array | `[]` | Windows sent here every time they appear. |

## offset object

Pixels taken off each side. Positive shrinks the area, negative grows it.

```json
{ "left": 0, "top": 40, "right": 0, "bottom": 0 }
```

## Rule object

Every rule list takes the same object.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `kind` | `Exe`, `Class`, `Title`, `Path` | required | Which part of the window to look at. `Exe` is the file name, `Path` the full path of the executable. |
| `id` | string | required | Value to compare against. |
| `matching_strategy` | see below | `Legacy` | How to compare. |

Strategies: `Legacy`, `Equals`, `DoesNotEqual`, `StartsWith`, `DoesNotStartWith`,
`EndsWith`, `DoesNotEndWith`, `Contains`, `DoesNotContain`, `Regex`.
`Legacy` treats the id as a regular expression when it contains regex
characters and as an exact comparison otherwise. Prefer an explicit strategy.

A UWP window is hosted by `ApplicationFrameHost.exe`, which would make every
UWP app the same program. Mochi reports the process the application itself runs
in instead, so `Calculator` matches `CalculatorApp.exe` and a rule for one UWP
app leaves the others alone.

An element of a rule array may also be an array of rule objects. Then every
object in it has to match, which is how `applications.json` expresses
conditions like "class equals X and title does not contain Y".

```json
"ignore_rules": [
  { "kind": "Exe", "id": "wallpaper64.exe", "matching_strategy": "Equals" },
  { "kind": "Title", "id": " - Peek", "matching_strategy": "EndsWith" },
  [
    { "kind": "Class", "id": "Ableton Live Window Class", "matching_strategy": "Equals" },
    { "kind": "Title", "id": "Ableton", "matching_strategy": "DoesNotContain" }
  ]
]
```

## Layouts

`BSP`, `Columns`, `Rows`, `VerticalStack`, `HorizontalStack`,
`UltrawideVerticalStack`, `Grid`.

`mochic cycle-layout next` walks through them in that order.

## Easing styles

`Linear`, `EaseInSine`, `EaseOutSine`, `EaseInOutSine`, `EaseInQuad`,
`EaseOutQuad`, `EaseInOutQuad`, `EaseInCubic`, `EaseOutCubic`, `EaseInOutCubic`.

## Reloading

`mochic reload-configuration` re-reads the file. Anything set with a `mochic`
command in the meantime is overwritten by what the file says.

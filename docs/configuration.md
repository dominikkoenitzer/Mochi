# Configuration

Mochi reads `%USERPROFILE%\mochi.json`. A `--config` argument wins over the
`MOCHI_CONFIG` environment variable, which wins over that default path.

The key names follow the common tiling window manager conventions, so an existing JSON config works after the
`$schema` line is swapped. See [import.md](import.md).

Values in the file use `PascalCase` for enums
(`"BSP"`, `"EaseOutQuad"`, `"Equals"`). Where the same value exists on the
`mochic` command line it is kebab-case there (`bsp`, `ease-out-quad`,
`equals`). Not every value exists on both sides: five of the ten matching
strategies are file only, and `focus_follows_mouse` has a different shape
altogether. Both are spelled out below.

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
| `cross_monitor_move_behaviour` | `Swap`, `Insert`, `NoOp` | `Swap` | What happens when a window is moved past the edge of a monitor. `NoOp` leaves it where it is, so a monitor edge behaves like a screen edge. |
| `window_container_behaviour` | `Create`, `Append` | `Create` | Whether a new window gets a container of its own or is stacked onto the focused one. |
| `mouse_follows_focus` | boolean | `true` | Warp the cursor into a window when it takes focus. |
| `focus_follows_mouse` | `Windows`, `Mochi` | unset | Focus whatever window the cursor moves over, and with which implementation. See below: this is not a boolean. |
| `float_override` | boolean | `false` | Make every new window float. |
| `resize_delta` | integer | `50` | How many pixels one `mochic resize-axis` step moves a boundary. |
| `default_workspace_padding` | integer | `10` | Gap between a workspace and the screen edge, in pixels. See the note on DPI below. |
| `default_container_padding` | integer | `10` | Gap between tiled containers, in pixels. See the note on DPI below. |
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
| `tray_and_multi_window_applications` | rule array | `[]` | Applications that keep a hidden window alive in the tray. Parsed, not acted on, see below. |
| `object_name_change_applications` | rule array | `[]` | Applications that reuse one window and only change its title. Parsed, not acted on, see below. |
| `border_overflow_applications` | rule array | `[]` | Applications whose own border sits outside their window rectangle. Parsed, not acted on, see below. |
| `layered_whitelist` | rule array | `[]` | Layered windows to manage anyway. Parsed, not acted on, see below. |
| `slow_application_identifiers` | rule array | `[]` | Applications that need an extra beat before their window is ready. Parsed, not acted on, see below. |
| `monitors` | array | `[]` | Per monitor settings in physical order, see below. |
| `work_area_offset` | offset object | none | Pixels taken off every monitor's work area, to leave room for something else on screen. |
| `global_work_area_offset` | offset object | none | The other spelling the existing config format uses for the same thing. `work_area_offset` wins when both are present. |
| `unmanaged_window_operation_behaviour` | `Op`, `NoOp` | `Op` | Whether commands still act when the focused window is not managed. |
| `monitor_index_preferences` | object | none | Monitor index to rect, pins an index to the screen at that position. |
| `display_index_preferences` | object | none | Monitor index to display id, pins an index to a physical display. |

## focus_follows_mouse

This is not a boolean. It is optional and names an implementation:

| Value | Meaning |
|---|---|
| omitted | Off. This is the default. |
| `"Windows"` | The Windows accessibility setting does the focusing. |
| `"Mochi"` | Mochi tracks the cursor itself. |

`"focus_follows_mouse": true` is a type error, and a type error anywhere in
`mochi.json` fails the whole file, not just that key, so one wrong value here
costs every other setting as well.

The command line half of the same feature does use `enable` and `disable`:
`mochic focus-follows-mouse enable` selects `Mochi`, `disable` turns it off.
The two spellings differ because the command has only a switch to offer, and
the file is where the implementation is chosen.

## Padding and DPI

`default_workspace_padding`, `default_container_padding` and the per workspace
`workspace_padding` and `container_padding` are multiplied by the scale factor
of the monitor they land on, so `10` is ten pixels at 100 percent and fifteen
at 150 percent and the gap looks the same on both screens. There is no key to
turn that off: what the file carries is the value at 100 percent.

The border is the exception. `border_width` and `border_offset` are physical
pixels and are never scaled, so a border is the same thickness everywhere.

## Rules that are read but not acted on

`tray_and_multi_window_applications`, `object_name_change_applications`,
`border_overflow_applications`, `layered_whitelist` and
`slow_application_identifiers` parse, merge with the community rule file and
are counted by `mochic state`, and nothing in the daemon asks them anything
yet. They are here so a config copied over from another window manager keeps
validating and keeps its rules; they change no behaviour today.

`ignore_rules`, `manage_rules`, `floating_applications` and
`transparency_ignore_rules` are the four that are acted on.

## border_colours

Every key is optional and takes a colour in one of three forms:

| Form | Example | Notes |
|---|---|---|
| Hex string | `"#ffbbdf"`, `"#fbd"` | Three digits or six, with or without the `#`. |
| Channel object | `{ "r": 255, "g": 187, "b": 223 }` | Each channel 0 to 255. |
| Integer | `16711680` | A Win32 `COLORREF`, so it is `0x00bbggrr` and not `0xrrggbb`. `16711680` is `0xff0000`, which is blue. |

`mochic state` and a rewritten config always give the hex string back,
whichever form went in.

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
| `window_based_work_area_offset` | offset object | A second offset, applied on top of the one above, and only to workspaces that ask for it with `apply_window_based_work_area_offset`. |
| `window_based_work_area_offset_limit` | integer, default `1` | The offset above stops applying once a workspace holds more containers than this. One window gets the narrower area, a second one takes the whole screen back. |

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
| `apply_window_based_work_area_offset` | boolean | `false` | Apply this monitor's `window_based_work_area_offset` to this workspace. |
| `float_override` | boolean | `false` | Make every window that opens here float. It adds to the top level `float_override` rather than replacing it: either one being true is enough, so this cannot bring floating back to one workspace when the global key is on. |
| `window_container_behaviour` | `Create`, `Append` | none | Accepted and then dropped: the per workspace value is never read, and the top level key decides for every workspace. |

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

`mochic float-rule` and `mochic ignore-rule` take only five of them:
`equals`, `contains`, `starts-with`, `ends-with` and `regex`. A rule that needs
`Legacy` or one of the four negative strategies has to be written here.

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

The file is watched, so saving it applies at once. `mochic
reload-configuration` re-reads it on demand, and the hotkey file with it.
Anything set with a `mochic` command in the meantime is overwritten by what the
file says.

Keys are not configured here. They live in their own file, next to this one in
spirit but with a syntax of its own, and they reload the same way: see
[hotkeys.md](hotkeys.md).

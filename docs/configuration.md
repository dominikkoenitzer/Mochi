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
mochic schema --output mochi.schema.json
```

Use `--output` rather than redirecting with `>`. Windows PowerShell 5.1, which
is what `powershell.exe` still is on Windows 11, writes a redirect as UTF-16
with a byte order mark, and the result is not JSON: every editor and every
parser rejects it. `--output` writes the file itself, as UTF-8, whatever shell
you are in.

## The smallest file that works

Everything below has a default, so a config can be as short as this. Copy it
into `%USERPROFILE%\mochi.json` and change one thing at a time:

```json
{
  "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "border": true,
  "border_width": 4,
  "monitors": [
    {
      "workspaces": [
        { "name": "web", "layout": "BSP" },
        { "name": "code", "layout": "BSP" }
      ]
    }
  ],
  "ignore_rules": [
    { "kind": "Exe", "id": "Taskmgr.exe", "matching_strategy": "Equals" }
  ]
}
```

`mochic check` reads a file and says what is wrong with it, without starting
anything, and it names the line and the column. Run it after every edit.

## Words this file uses

| Word | Means |
|---|---|
| daemon | The `mochi.exe` process that does the tiling. `mochic` only talks to it. |
| container | One slot in the layout. It holds one window, or several as a stack. Padding and resizing are about containers, not windows. |
| workspace | One set of containers on one monitor. Switching workspace takes every window of the old one off screen. Nine per monitor. |
| stack | Several windows sharing one container, one visible at a time, cycled with a key. |
| BSP | Binary space partitioning, the default layout: each new window splits the space of the one it lands next to, so windows halve as you add them. |
| the ring | The containers of a workspace in creation order. `cycle-focus` steps along it by position, which is what a layout with no obvious left and right needs. |
| monocle | One window filling the whole workspace, the others still there underneath. |
| float | A window left at its own size and position, ignored by the layout, drawn above the tiled ones. |
| cloak | Hiding a window the way Windows hides the windows of another virtual desktop. The default way Mochi takes a window off screen; it is invisible but not minimized, so the application does not know. |
| work area | The part of a monitor left after the taskbar. Tiles never cover the taskbar. |



## Top level

| Key | Type | Default | Meaning |
|---|---|---|---|
| `$schema` | string | none | Schema URL or path, only used by the editor. |
| `app_specific_configuration_path` | string | none | Path to a community style `applications.json` rule file, merged into the rule sets below. `$Env:USERPROFILE` is expanded. |
| `window_hiding_behaviour` | `Cloak`, `Minimize`, `Hide` | `Cloak` | How windows of inactive workspaces disappear. |
| `cross_monitor_move_behaviour` | `Swap`, `Insert`, `NoOp` | `Swap` | What happens when a window is moved past the edge of a monitor. `NoOp` leaves it where it is, so a monitor edge behaves like a screen edge. |
| `window_container_behaviour` | `Create`, `Append` | `Create` | Whether a new window gets a container of its own or is stacked onto the focused one. |
| `mouse_follows_focus` | boolean | `true` | Warp the cursor into a window when it takes focus. |
| `fullscreen_passthrough` | boolean | `true` | Leave a window alone while it covers its whole monitor with no title bar: a borderless game, a video in F11, a slideshow. That monitor is not retiled and loses its borders and fading until the window leaves fullscreen. See below. |
| `focus_follows_mouse` | `Windows`, `Mochi` | unset | Focus whatever window the cursor moves over, and with which implementation. See below: this is not a boolean. |
| `float_override` | boolean | `false` | Make every new window float. |
| `resize_delta` | integer | `50` | How many pixels one `mochic resize-axis` step moves a boundary. |
| `default_workspace_padding` | integer | `10` | Gap between a workspace and the screen edge, in pixels. See the note on DPI below. |
| `default_container_padding` | integer | `10` | Gap between tiled containers, in pixels. See the note on DPI below. |
| `minimum_window_width` | integer | `64` | Width below which a tile is never shrunk, in pixels. See the note on DPI below and the note on minimum window size. |
| `minimum_window_height` | integer | `64` | Height below which a tile is never shrunk, in pixels. See the note on DPI below and the note on minimum window size. |
| `border` | boolean | `false` | Draw a border around the focused window. |
| `border_width` | integer | `6` | Border thickness in physical pixels, not scaled by DPI. |
| `border_offset` | integer | `-1` | How far the border sits outside the window frame, negative pulls it in. |
| `border_style` | `System`, `Rounded`, `Square` | `System` | Border corner shape. |
| `transparency` | boolean | `false` | Make unfocused windows transparent. Windows that present through DirectComposition (Chromium and Electron apps, UWP frames) cannot be layered without going blank and always stay opaque, with no rule needed. |
| `transparency_alpha` | integer 0 to 255 | `200` | Opacity of unfocused windows, 255 is opaque. |
| `transparency_ignore_rules` | rule array | `[]` | Windows that stay opaque. They still get a border. |
| `border_colours` | object | none | Border colour per window kind, see below. |
| `animation` | object | none | Move and resize animation, see below. |
| `stackbar` | object | none | Tab bar for stacked containers, see below. Parsed so a copied config keeps validating; nothing is ever drawn above a window. |
| `ignore_rules` | rule array | `[]` | Windows Mochi never touches. |
| `manage_rules` | rule array | `[]` | Windows Mochi manages even though the usual checks say no. |
| `floating_applications` | rule array | `[]` | Windows that are managed but never tiled. |
| `tray_and_multi_window_applications` | rule array | `[]` | Applications that keep a hidden window alive in the tray. |
| `object_name_change_applications` | rule array | `[]` | Applications that reuse one window and only change its title. |
| `border_overflow_applications` | rule array | `[]` | Applications whose own border sits outside their window rectangle. Parsed, not acted on, see below. |
| `layered_whitelist` | rule array | `[]` | Layered windows to manage anyway. Parsed, not acted on, see below. |
| `slow_application_identifiers` | rule array | `[]` | Applications that need an extra beat before their window is ready. |
| `scratchpads` | array | `[]` | Windows kept out of the layout and shown by name with one key, see below. |
| `fullscreen_passthrough_ignore_rules` | rule array | `[]` | Windows that are tiled like any other even when they go fullscreen. |
| `monitors` | array | `[]` | Per monitor settings in physical order, see below. |
| `work_area_offset` | offset object | none | Pixels taken off every monitor's work area, to leave room for something else on screen. |
| `global_work_area_offset` | offset object | none | The other spelling the existing config format uses for the same thing. `work_area_offset` wins when both are present. |
| `unmanaged_window_operation_behaviour` | `Op`, `NoOp` | `Op` | Whether commands still act when the focused window is not managed. |
| `monitor_index_preferences` | object | none | Monitor index to rect, pins an entry to the screen at that position. See below. |
| `display_index_preferences` | object | none | Monitor index to display id, pins an entry to a physical display. See below. |

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

`default_workspace_padding`, `default_container_padding`,
`minimum_window_width`, `minimum_window_height` and the per workspace
`workspace_padding` and `container_padding` are multiplied by the scale factor
of the monitor they land on, so `10` is ten pixels at 100 percent and fifteen
at 150 percent and the gap looks the same on both screens. There is no key to
turn that off: what the file carries is the value at 100 percent.

The border is the exception. `border_width` and `border_offset` are physical
pixels and are never scaled, so a border is the same thickness everywhere.

## Minimum window size

`minimum_window_width` and `minimum_window_height` are the floor a tile is
never shrunk below, whether the squeeze comes from the number of windows on the
workspace or from leaning on `mochic resize-axis`. Both default to `64`, which
is the floor every layout used before the keys existed, so a file that sets
neither tiles exactly as it always did.

They are in the same units as the paddings: logical pixels at 100 percent,
multiplied by the scale factor of the monitor the workspace lands on. A floor
of `300` is 300 physical pixels on a display at 100 percent and 450 on one at
150 percent, so a window stays the same size on the desk on both screens.

The two are independent. Every cut a layout makes divides exactly one axis and
takes the floor belonging to that axis, so a width of `300` with a height of
`250` means no tile narrower than 300 and none shorter than 250. A width floor
is usually the one worth setting: it is what keeps a column of text readable.

A value the screen cannot possibly honour is not an error and cannot break a
layout. When a workspace has more containers than the floor leaves room for,
the layout scales the minimum back down for that cut and the tiles still cover
the whole area exactly, with nothing inverted and nothing off screen. On a 4K
screen with twelve BSP containers a floor of `300` is met exactly; a floor of
`400` gives 400 pixel wide tiles that are 360 tall, because twelve of them do
not fit into 2160 pixels any other way.

## Rules that are read but not acted on

Two lists parse, merge with the community rule file and are counted by
`mochic state`, and nothing in the daemon asks them anything:
`border_overflow_applications` and `layered_whitelist`. They are here so a
config copied over from another window manager keeps validating and keeps its
rules; they change no behaviour today, and `mochic check` says so when it finds
entries in them.

Everything else in the rules is acted on. `ignore_rules`, `manage_rules`,
`floating_applications` and `transparency_ignore_rules` decide whether a window
is managed, floated or faded. `tray_and_multi_window_applications` decides
whether a window that closes to the tray is really gone,
`object_name_change_applications` decides whether a title change is worth
re-reading the window for, and `slow_application_identifiers` gives an
application an extra beat before its window is judged. Those three were wired up
after this page was first written and it said otherwise until 2026-09-21, which
was worth correcting: acting on it would have meant deleting rules that work.

## Fullscreen windows

A window counts as fullscreen when it has no title bar and its rectangle
covers the whole monitor, taskbar included, unless that is the rectangle Mochi
gave it (a captionless window in monocle with no padding). It stays in the
model, but its monitor is left alone: no retile moves anything there, and no
border or fading is drawn on it. The other monitors carry on as usual.

The monitor comes back a quarter of a second after the window leaves
fullscreen, is minimized or closed, or after a display change, and only that
monitor is retiled. Pausing and game mode do not change any of this, so leaving
game mode never pulls a game that is still fullscreen back into a tile.
`mochic state` lists the frozen monitors and the window holding each one, and
`mochic why` says so for the focused window.

## Pinning a monitor

Without one of these, the first `monitors` entry configures whatever screen
Windows enumerated first. That order is not stable: a DisplayPort renegotiation,
a monitor waking in a different sequence, or a cable in another socket can swap
it, and then every workspace, padding and layout lands on the wrong screen.

```json
"monitor_index_preferences": {
  "0": { "left": 0, "top": 0, "right": 3840, "bottom": 2160 },
  "1": { "left": 3840, "top": 0, "right": 4920, "bottom": 1920 }
}
```

Read the rectangles out of `mochic state` with every screen attached, and use
each monitor's own `size`. A rectangle survives a reboot and a renegotiation; it
does not survive moving a screen in the display settings or changing its
resolution, so update it if you do either.

`display_index_preferences` maps the same indices to a display identifier
instead, which is preferable when it works, because it survives a move as well.
Check yours first: it is the `device_id` field in `mochic state`, and it comes
from the display driver. Windows very often reports `Generic PnP Monitor` for
every panel, and two panels reporting the same string cannot be told apart, so
if that is what you see, pin by rectangle.

An entry whose display or rectangle is not attached configures nothing at all
rather than falling back to the screen that happens to be there. That is the
point: applying a portrait panel's workspaces to a landscape one is the failure
being prevented, not a lesser outcome. The entry takes effect at the next
configuration load once its screen is back.

An entry that names neither key keeps the old behaviour and takes the screen at
its own position, among those no pin has claimed.

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

Never drawn, whatever the configuration says: the block parses so a copied
config keeps validating.

| Key | Type | Meaning |
|---|---|---|
| `height` | integer | Bar height in logical pixels. |
| `mode` | `Always`, `Never`, `OnStack` | When the bar is shown. |
| `label` | `Process`, `Title` | What a tab is labelled with. |
| `tabs` | object | `width`, `focused_text`, `unfocused_text`, `background`, `font_family`, `font_size`. |

## scratchpads

A scratchpad is one window that never takes a tile. `mochic toggle-scratchpad
term` shows it centred on the monitor you are looking at and gives it the
keyboard, and the same command takes it off screen again. Bound to a key it is
a terminal, a notes window or a calculator one press away, and the layout
underneath never moves.

```json
"scratchpads": [
  {
    "name": "term",
    "match": { "kind": "Title", "id": "scratch", "matching_strategy": "Equals" },
    "command": "wt.exe -w new --title scratch --suppressApplicationTitle",
    "width": 0.6,
    "height": 0.5,
    "hide_on_focus_loss": true
  }
]
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | What `toggle-scratchpad`, `scratchpad-claim` and `scratchpad-release` are given. Case does not matter. A second entry with the same name is dropped. |
| `match` | rule object | required | The window that belongs to this scratchpad, written like an entry of any rule list. An array of rule objects works too, and then all of them have to match. |
| `command` | string | none | Run through `cmd.exe` when no window matches, the way a hotkey `.shell` line is. Without it, a toggle with nothing to show is an error. |
| `width` | number | `0.6` | Width as a fraction of the focused monitor's work area, from `0.1` to `1`. |
| `height` | number | `0.5` | Height as a fraction of the work area, from `0.1` to `1`. |
| `hide_on_focus_loss` | boolean | `true` | Take it off screen as soon as another application takes the focus. A dialog of the scratchpad's own program does not count. |

What a toggle does:

- Nothing held yet: the first open window that matches is taken out of the
  layout and shown. With none open, `command` is started and the first window
  that matches in the next ten seconds is taken. Pressing the key again while it
  starts does not start a second copy.
- Off screen: shown on the monitor you are looking at.
- On screen, on this monitor: taken off screen, and the keyboard goes back to
  the focused window of the layout.
- On screen, on another monitor: moved over here rather than hidden.

Match on a title or a class, never on the executable alone. `WindowsTerminal.exe`
owns every terminal window, so a rule naming it takes whichever one opens
first. `wt.exe --title scratch --suppressApplicationTitle` gives the new window
a title of its own that the shell cannot change, and Mochi waits for it:
Windows Terminal opens under its own name and only then takes the title.
`mochic check` warns about a scratchpad that matches on the executable alone.

A scratchpad stays on screen across workspace switches and is never set always
on top, so a window you click can still cover it. `hide_on_focus_loss` reacts
to you: clicking or switching to another application hides it, and a Mochi
command that moves the keyboard, a workspace switch among them, does not. It is taken off screen the
same way a hidden workspace is, and written into the same record, so `mochic
stop`, `mochic restore-windows` and the next start after a crash all give it
back. After a restart it is an ordinary tiled window until the next toggle
takes it again; no second copy is started. When the window is closed the next
toggle starts `command` again. Its border has the `floating` colour.

`scratchpad-claim <name>` makes the focused window the scratchpad's, which is
the way to use one without a `command`. `scratchpad-release <name>` gives the
window back to the layout. All three commands are refused while Mochi is paused
or in game mode, and for a window that runs as administrator, which Windows does
not let Mochi move or hide.

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

The four rule commands, `mochic float-rule`, `ignore-rule`, `manage-rule` and
`workspace-rule`, take only five of them: `equals`, `contains`, `starts-with`,
`ends-with` and `regex`. A rule that needs `Legacy` or one of the four negative
strategies has to be written here.

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

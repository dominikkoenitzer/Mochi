# mochic

`mochic` sends one command to the running daemon and prints its answer. The
command names follow the common tiling window manager conventions, so an existing whkdrc imports by replacing the program
name. See [import.md](import.md).

Enum arguments are kebab-case on the command line (`bsp`, `ease-out-quad`)
while the same values are PascalCase in `mochi.json`.

The groups below follow the order of the hotkey file.

## Focus

| Command | Arguments | Does |
|---|---|---|
| `focus` | `left` \| `right` \| `up` \| `down` | Move focus to the neighbour in that direction. |
| `focus-monitor` | index | Focus a monitor by zero based index. |
| `focus-workspace` | index | Focus a workspace of the current monitor by zero based index. |
| `focus-last-workspace` | none | Go back to the workspace focused before this one. |
| `cycle-monitor` | `next` \| `previous` | Step through the monitors. |
| `cycle-workspace` | `next` \| `previous` | Step through the workspaces of the current monitor. |
| `cycle-stack` | `next` \| `previous` | Step through the windows of the focused stack. |

## Move

| Command | Arguments | Does |
|---|---|---|
| `move` | `left` \| `right` \| `up` \| `down` | Move the focused window. At the screen edge `cross_monitor_move_behaviour` decides what happens. |
| `move-to-workspace` | index | Move the focused window to a workspace and follow it. |
| `move-to-monitor` | index | Move the focused window to a monitor and follow it. |
| `promote` | none | Swap the focused window with the first window of the workspace. |
| `stack` | `left` \| `right` \| `up` \| `down` | Stack the focused window onto the neighbour in that direction. |
| `unstack` | none | Pull the focused window out of its stack. |

## Resize

| Command | Arguments | Does |
|---|---|---|
| `resize-axis` | `horizontal` \| `vertical`, `increase` \| `decrease` | Grow or shrink the focused window along an axis. |
| `workspace-padding` | monitor, workspace, size | Set the outer padding of one workspace. |
| `container-padding` | monitor, workspace, size | Set the padding between containers of one workspace. |

## Window state

| Command | Arguments | Does |
|---|---|---|
| `toggle-float` | none | Switch the focused window between tiled and floating. |
| `toggle-maximize` | none | Switch the focused window between tiled and maximized. |
| `toggle-monocle` | none | Give the focused window the whole work area. |
| `minimize` | none | Minimize the focused window. |
| `close` | none | Ask the focused window to close. |
| `manage` | none | Start managing the focused window even if a rule would skip it. |
| `unmanage` | none | Stop managing the focused window and leave it where it is. |

## Layouts

| Command | Arguments | Does |
|---|---|---|
| `cycle-layout` | `next` \| `previous` | Step through the layout list. |
| `change-layout` | `bsp` \| `columns` \| `rows` \| `vertical-stack` \| `horizontal-stack` \| `ultrawide-vertical-stack` \| `grid` | Set the layout of the focused workspace. |
| `flip-layout` | `horizontal` \| `vertical` | Mirror the layout of the focused workspace. |

## State

| Command | Arguments | Does |
|---|---|---|
| `state` | none | Print the whole daemon state as JSON. |
| `query` | target | Print one value. Targets: `focused-monitor-index`, `focused-workspace-index`, `focused-container-index`, `focused-window-index` (the index inside the focused container), `focused-workspace-name`, `monitor-count`, `window-count`, `paused`, `dry-run`, `config-path`, `version`. |
| `subscribe-pipe` | name | Send every event to a named pipe the subscriber created, for scripts and integrations. |
| `unsubscribe-pipe` | name | Stop sending events to that pipe. |

## Control

| Command | Arguments | Does |
|---|---|---|
| `start` | `--whkd`, `--config`, `--dry-run` | Start the daemon, and whkd with it when `--whkd` is given. |
| `stop` | `--whkd` | Restore every managed window, then exit. Also stops whkd with `--whkd`. |
| `toggle-pause` | none | Stop and resume management without exiting. |
| `retile` | none | Recompute and apply every layout. |
| `reload-configuration` | none | Re-read `mochi.json`. |

## Config

| Command | Arguments | Does |
|---|---|---|
| `quickstart` | none | Write a default `mochi.json` when there is none. |
| `schema` | none | Print the JSON schema of the config file. |
| `focus-follows-mouse` | `enable` \| `disable` | Focus whatever the cursor moves over. |
| `mouse-follows-focus` | `enable` \| `disable` | Warp the cursor to a newly focused window. |
| `border` | `enable` \| `disable` | Turn the focus border on or off. |
| `border-width` | width | Border thickness in logical pixels. |
| `border-offset` | offset | How far the border sits outside the frame. |
| `border-style` | `system` \| `rounded` \| `square` | Border corner shape. |
| `border-colour` | `--kind` `single` \| `stack` \| `monocle` \| `floating` \| `unfocused`, r, g, b | Border colour for one kind of window. |
| `toggle-transparency` | none | Turn transparency for unfocused windows on or off. |
| `animation` | `enable` \| `disable` | Turn move and resize animations on or off. |
| `animation-duration` | milliseconds | Length of one animation. |
| `animation-style` | easing style | Easing curve, kebab-case, for example `ease-out-quad`. |
| `animation-fps` | fps | Frames per second while animating. |
| `float-rule` | `exe` \| `class` \| `title` \| `path`, id, [`--matching-strategy`] | Add a rule that floats matching windows. |
| `ignore-rule` | `exe` \| `class` \| `title` \| `path`, id, [`--matching-strategy`] | Add a rule that ignores matching windows. |

Rules added with `float-rule` and `ignore-rule` live until the daemon stops or
the configuration is reloaded. Put the permanent ones in `mochi.json`.

## Daemon switches

`mochic` talks to a running daemon; these are arguments to `mochi` itself.

| Switch | Does |
|---|---|
| `--config PATH` | Use this file instead of `$MOCHI_CONFIG` or `%USERPROFILE%\mochi.json`. |
| `--dry-run` | Read everything, move nothing: every write becomes a log line. The only safe way to run Mochi next to another window manager. |
| `--manage-class CLASS` | Manage only windows of that class, even when they are tool windows, and leave every other window alone. May be repeated. This is the switch `crates/mochi-testbed` needs; see its README. |

## Exit codes

`0` when the daemon accepted the command, `1` when it answered with an error or
was not reachable. The error message goes to stderr.

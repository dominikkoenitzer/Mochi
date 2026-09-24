# mochic

`mochic` sends one command to the running daemon and prints its answer. The
command names follow the common tiling window manager conventions, so an
existing whkdrc imports by replacing the program name. See
[import.md](import.md).

Enum arguments are kebab-case on the command line (`bsp`, `ease-out-quad`)
while the same values are PascalCase in `mochi.json`. The two sets are not
always the same size: the file accepts ten matching strategies and the command
line five, and `focus-follows-mouse` is `enable`/`disable` here and an
implementation name in the file. Each of those is called out where it appears.

The groups below follow the order of the hotkey file.

## Focus

| Command | Arguments | Does |
|---|---|---|
| `focus` | `left` \| `right` \| `up` \| `down` | Move focus to the neighbour in that direction. |
| `focus-monitor` | index | Focus a monitor by zero based index. |
| `focus-workspace` | index | Focus a workspace of the current monitor by zero based index. |
| `focus-last-workspace` | none | Go back to the workspace focused before this one. |
| `focus-named-workspace` | name | Focus the workspace with that `name` in `mochi.json`, on whichever monitor it is. The search starts at the screen you are looking at, so a name used on both resolves to the near one. Case does not matter. |
| `cycle-focus` | `next` \| `previous` | Step the focus one container along the ring. It goes by position rather than by geometry, so it is the one that still makes sense on a layout where "left" is ambiguous. |
| `promote-focus` | none | Focus the first window of the workspace, the one most layouts give the biggest tile. Moves nothing; `promote` is the half that does. |
| `cycle-monitor` | `next` \| `previous` | Step through the monitors. |
| `cycle-workspace` | `next` \| `previous` | Step through the workspaces of the current monitor. |
| `cycle-stack` | `next` \| `previous` | Step through the windows of the focused stack. |

## Move

| Command | Arguments | Does |
|---|---|---|
| `move` | `left` \| `right` \| `up` \| `down` | Move the focused window. At the screen edge `cross_monitor_move_behaviour` decides what happens. |
| `cycle-move` | `next` \| `previous` | Swap the focused window with its neighbour in the ring, by position rather than by geometry. The counterpart to `cycle-focus`. |
| `move-to-workspace` | index | Move the focused window to a workspace and follow it. |
| `send-to-workspace` | index | Move the focused window to a workspace and stay where you are. The same command without the following, which is often exactly what you want: park something and carry on. |
| `move-to-named-workspace` | name | Move the focused window to the workspace with that name and follow it. |
| `send-to-named-workspace` | name | Move the focused window to the workspace with that name and stay where you are. |
| `move-to-monitor` | index | Move the focused window to a monitor and follow it. |
| `send-to-monitor` | index | Move the focused window to a monitor and stay where you are. |
| `promote` | none | Swap the focused window with the first window of the workspace. |
| `stack` | `left` \| `right` \| `up` \| `down` | Stack the focused window onto the neighbour in that direction. |
| `unstack` | none | Pull the focused window out of its stack. |
| `stack-all` | none | Collapse the whole workspace into one stack, keeping the focused window in front. Floating windows are left alone. |
| `unstack-all` | none | Give every stacked window its own container again, in the order they were stacked. The window you were looking at keeps the focus. |
| `focus-stack-window` | index | Focus the window at that position in the focused stack, counting from zero. What `cycle-stack` does one step at a time. An index past the end does nothing rather than wrapping. |

## Resize

| Command | Arguments | Does |
|---|---|---|
| `resize-axis` | `horizontal` \| `vertical`, `increase` \| `decrease` | Grow or shrink the focused window along an axis. Mochi picks the edge: the far one where there is a boundary, the near one where there is not. |
| `resize-edge` | `left` \| `right` \| `up` \| `down`, `increase` \| `decrease` | Grow or shrink the focused window by moving that one edge. Nothing happens when the edge named is the edge of the screen, which is the difference from `resize-axis`: a key per edge never moves the other one instead. |
| `workspace-padding` | monitor, workspace, size | Set the outer padding of one workspace. |
| `container-padding` | monitor, workspace, size | Set the padding between containers of one workspace. |

## Window state

| Command | Arguments | Does |
|---|---|---|
| `toggle-float` | none | Switch the focused window between tiled and floating. |
| `toggle-float-override` | none | Float every window that appears from now on, and again to stop. Nothing already on screen moves. It lasts until the daemon stops or the configuration is reloaded; `float_override` in `mochi.json` is the permanent version. |
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
| `toggle-tiling` | none | Stop arranging the focused workspace, and again to arrange it once more. Every window stays managed and stays where it is, so this is the one to reach for while dragging things around by hand; `unmanage` and `toggle-pause` are the bigger hammers. |

## State

| Command | Arguments | Does |
|---|---|---|
| `state` | none | Print the whole daemon state as JSON. |
| `why` | `--json` | Explain what Mochi makes of the window in front and what to do about it: whether it is being tiled and where, or the reason it is being left alone and the command that would change that. The one thing to run when a window is not tiling and it is not obvious why. `--json` prints the document the paragraph is made of. |
| `doctor` | `--json` | Check the daemon's picture of the desktop against the real one. Each finding names a window and says what is wrong the way you would see it on screen, for example a tile reserved for a window that is not there. `--json` prints the raw findings document. |
| `query` | target | Print one value. Targets: `focused-monitor-index`, `focused-workspace-index`, `focused-container-index`, `focused-window-index` (the index inside the focused container), `focused-workspace-name`, `monitor-count`, `window-count`, `paused`, `dry-run`, `config-path`, `version`. |
| `subscribe` | name | Create a pipe of that name, register it, and print every event to stdout as one JSON line until Ctrl-C. The one command to watch what the daemon is doing. |
| `subscribe-pipe` | name | Send every event to a named pipe the subscriber created itself, for a bar or a service that owns its own pipe. |
| `unsubscribe-pipe` | name | Stop sending events to that pipe. |

## Control

| Command | Arguments | Does |
|---|---|---|
| `start` | `--config PATH`, `--hotkeys PATH`, `--no-hotkeys`, `--dry-run` | Start the daemon, hotkeys and all. Every switch is handed straight to `mochi`; they mean what the table under "Daemon switches" says. |
| `stop` | none | Restore every managed window, then exit. |
| `toggle-pause` | none | Stop and resume management without exiting. |
| `retile` | none | Recompute and apply every layout. |
| `reload-configuration` | none | Re-read `mochi.json` and the hotkey file. |
| `restore-windows` | none | Put back every window that is off screen with nothing in the daemon state to explain it. The escape hatch for a window that has gone invisible and unreachable: windows that belong to a hidden workspace are left where they are, so this is safe to run at any time. |

## Hotkeys

Mochi binds the keys itself. The file, its syntax and where it is looked for are
in [hotkeys.md](hotkeys.md).

| Command | Arguments | Does |
|---|---|---|
| `hotkeys` | `--json` | Print every binding, and every line of the file that did not parse. `--json` prints the document those tables are made of. |
| `set-hotkeys` | `enable` \| `disable` | Bind keys, or stop binding them and leave the keyboard alone. Tiling carries on either way. |
| `toggle-game-mode` | none | Pause tiling and suspend every binding except the one bound to `toggle-game-mode`, so the game in front gets the rest of the keyboard. Again to come back. |

## Config

| Command | Arguments | Does |
|---|---|---|
| `quickstart` | none | Write a default `mochi.json` and a default hotkey file, each only when there is none. |
| `check` | `[PATH]` | Read a configuration file and say what is wrong with it, without a daemon and without applying anything. No path means the file the daemon would load. Reports a parse failure with its line, column and the offending text; every rule that cannot do what it says, with its list, its line and the compiler's complaint; every top level key that parses and is then ignored; and, when the file names an `app_specific_configuration_path`, the same for that file, whose problems are warnings because the daemon carries on past them. Exits 0 when the file is usable, 1 when it is not; warnings do not fail. It reads the hotkey file at the same time and reports how many bindings it found, every line that did not parse, and every line that starts with a Mochi command but does not parse as one, because the daemon hands those to the shell and they then fail silently on every key press. Hand it an `applications.json` and it checks that instead. |
| `schema` | `[-o PATH]` | Print the JSON schema of the config file, or write it to `PATH` in UTF-8. |
| `focus-follows-mouse` | `enable` \| `disable` | Focus whatever the cursor moves over. |
| `mouse-follows-focus` | `enable` \| `disable` | Warp the cursor to a newly focused window. |
| `window-container-behaviour` | `create` \| `append` | Whether a new window gets a container of its own or stacks onto the focused one. |
| `toggle-window-container-behaviour` | none | Switch between the two, for a key that turns auto-stacking on while you fill a workspace and off again. |
| `cross-monitor-move-behaviour` | `swap` \| `insert` \| `no-op` | What moving a container past a monitor edge does. |
| `window-hiding-behaviour` | `hide` \| `minimize` \| `cloak` | How a window on an inactive workspace is taken off screen. Windows already off screen keep the method they were hidden with, so changing this mid-session cannot strand one. |
| `unmanaged-window-operation-behaviour` | `op` \| `no-op` | Whether a command aimed at a window Mochi does not manage runs anyway or is refused. |
| `border` | `enable` \| `disable` | Turn the focus border on or off. |
| `border-width` | width | Border thickness in physical pixels. It is not scaled by DPI, so 6 is six pixels on the 4K monitor and on the 1080p one. |
| `border-offset` | offset | How far the border sits outside the frame, in physical pixels too. |
| `border-style` | `system` \| `rounded` \| `square` | Border corner shape. |
| `border-colour` | r, g, b, `--window-kind` `single` \| `stack` \| `monocle` \| `floating` \| `unfocused` | Border colour for one kind of window. The three channels are positional, the kind is the option and defaults to `single`. |
| `toggle-transparency` | none | Turn transparency for unfocused windows on or off. |
| `animation` | `enable` \| `disable` | Turn move and resize animations on or off. |
| `animation-duration` | milliseconds | Length of one animation. |
| `animation-style` | easing style | Easing curve, kebab-case, for example `ease-out-quad`. |
| `animation-fps` | fps | Frames per second while animating. |
| `float-rule` | `exe` \| `class` \| `title` \| `path`, id, [`--matching-strategy`] | Add a rule that floats matching windows. |
| `ignore-rule` | `exe` \| `class` \| `title` \| `path`, id, [`--matching-strategy`] | Add a rule that ignores matching windows. An ignore rule you type here is your own word, so a later `manage-rule` cannot cancel it. |
| `manage-rule` | `exe` \| `class` \| `title` \| `path`, id, [`--matching-strategy`] | Add a rule that manages matching windows Mochi would otherwise skip, and adopt the ones already on screen. It can only overrule the soft reasons for skipping a window, never the ones that describe something nothing could tile. |
| `workspace-rule` | `exe` \| `class` \| `title` \| `path`, id, monitor, workspace, [`--initial-only`], [`--matching-strategy`] | Open matching windows on that monitor and workspace. The workspace is created if it is not there yet. `--initial-only` routes only the first window the rule ever matches, so a second window of the same application opens where you are. |

Rules added with `float-rule`, `ignore-rule`, `manage-rule` and
`workspace-rule` live until the daemon stops or the configuration is reloaded.
Put the permanent ones in `mochi.json`.

`--matching-strategy` takes `equals` (the default), `contains`, `starts-with`,
`ends-with` or `regex`. The file knows ten strategies and the command line only
these five: `Legacy`, `DoesNotEqual`, `DoesNotStartWith`, `DoesNotEndWith` and
`DoesNotContain` have no spelling here, so a rule that needs one of them belongs
in `mochi.json`. [configuration.md](configuration.md) lists all ten.

## Daemon switches

`mochic` talks to a running daemon; these are arguments to `mochi` itself.

| Switch | Does |
|---|---|
| `--config PATH` | Use this file instead of `$MOCHI_CONFIG` or `%USERPROFILE%\mochi.json`. |
| `--hotkeys PATH` | Bind this hotkey file instead of `$MOCHI_HOTKEYS` or the default path. See [hotkeys.md](hotkeys.md). |
| `--no-hotkeys` | Bind no keys at all, for a desktop that drives Mochi from something else. Conflicts with `--hotkeys`. |
| `--dry-run` | Read everything, move nothing: every write becomes a log line. The only safe way to run Mochi next to another window manager. |
| `--manage-class CLASS` | Manage only windows of that class, even when they are tool windows, and leave every other window alone. May be repeated. This is the switch `crates/mochi-testbed` needs; see its README. |

## Exit codes

| Code | When |
|---|---|
| `0` | The daemon accepted the command. |
| `1` | The daemon answered with an error, or was not reachable. The message goes to stderr. |
| `2` | The command line itself was wrong: an unknown command, a missing argument, a value outside the list. This one comes from the argument parser before anything is sent, so the message is a usage hint rather than an answer from the daemon. |

# Hotkeys

Mochi binds its keys itself, inside the daemon. There is no second program to install.

## The file

```
%USERPROFILE%\.config\mochi\hotkeys
```

Mochi takes the first of these that applies:

1. `--hotkeys <path>`, on `mochi` or on `mochic start`
2. the environment variable `MOCHI_HOTKEYS`
3. `%USERPROFILE%\.config\mochi\hotkeys`

`mochic quickstart` (and the installer) writes the default file when there is none, and never overwrites one. `mochi --no-hotkeys` or `mochic start --no-hotkeys` starts the daemon with no keys bound.

Saving the file reloads it at once. `mochic reload-configuration` (`alt + shift + r`) re-reads it on demand. Every reload returns to the default mode.

## Syntax

```
.shell pwsh                      # cmd, powershell, pwsh or bash. Default: cmd

# Comments start with a hash.
alt + h                 : focus left
alt + shift + q         : close
alt + shift + return    : pwsh -NoProfile -Command "code ."
```

Left of the colon: modifiers and exactly one key, in any order, any case, any spacing. Modifiers: `alt`, `ctrl` (or `control`), `shift`, `win` (or `super`, `meta`).

Right of the colon: a `mochic` command without the `mochic`, or a command line for the shell named by `.shell`. A line is a Mochi command when its first word is one `mochic` knows; anything else runs in the shell. Writing `mochic` in front is allowed and makes a command that does not parse an error at load time instead of a shell line that fails later.

`start`, `quickstart`, `check`, `schema` and `subscribe` cannot be bound. `stop` can.

### Groups

One bracket group on each side expands pairwise:

```
alt + [1,2,3]           : focus-workspace [0,1,2]
alt + shift + [1,2,3]   : move-to-workspace [0,1,2]
```

Without a group on the left, the right side is taken exactly as written, brackets included.

### Modes

A `mode` block holds keys that work only while that mode is on. `mode <name>` on the right of a binding switches to it; `mode default` switches back.

```
alt + shift + s         : mode resize

mode resize {
    h                   : resize-axis horizontal decrease
    j                   : resize-axis vertical increase
    k                   : resize-axis vertical decrease
    l                   : resize-axis horizontal increase
    esc                 : mode default
    enter               : mode default
}
```

- While a mode is on, only its own keys belong to Mochi. Every other key reaches the application, so typing still types.
- A mode name is letters, digits, `-` and `_`. `default` is taken.
- Every mode needs a key that leads back to `default`, directly or through another mode. A mode without one is dropped together with every key into it.
- Blocks do not nest. Close a block with `}` on its own line. A key is bound once per block.
- Game mode, `mochic set-hotkeys disable` and every reload return to `default`.
- Nothing on screen shows the active mode. `mochic hotkeys` lists mode keys as `resize: h`, and the log records each switch as a `hotkey mode` line.

## Key names

| Group | Names |
|---|---|
| Letters and digits | `a` to `z`, `0` to `9` |
| Function keys | `f1` to `f24` |
| Arrows | `left`, `right`, `up`, `down` |
| Editing and navigation | `space`, `enter`, `tab`, `esc`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown` |
| Locks and system | `pause`, `capslock`, `numlock`, `printscreen`, `scrolllock`, `apps` |
| Numeric keypad | `numpad0` to `numpad9`, `multiply`, `add`, `subtract`, `decimal`, `divide` |
| Punctuation | `semicolon`, `plus`, `comma`, `minus`, `period`, `slash`, `backtick`, `lbracket`, `backslash`, `rbracket`, `quote`, `oem_8`, `oem_102` |
| Media and volume | `volumemute`, `volumedown`, `volumeup`, `medianext`, `mediaprev`, `mediastop`, `mediaplaypause` |

Aliases: `return` = `enter`, `escape` = `esc`, `pgup` = `pageup`, `pgdn` = `pagedown`, `del` = `delete`, `grave` = `backtick`.

## Default bindings

This is the file `mochic quickstart` writes.

| Keys | Command |
|---|---|
| `alt + h` / `j` / `k` / `l` | `focus left` / `down` / `up` / `right` |
| `alt + shift + h` / `j` / `k` / `l` | `move left` / `down` / `up` / `right` |
| `alt + shift + left` / `down` / `up` / `right` | `move left` / `down` / `up` / `right` |
| `alt + shift + u` | `resize-axis horizontal decrease` |
| `alt + shift + p` | `resize-axis horizontal increase` |
| `alt + shift + i` | `resize-axis vertical decrease` |
| `alt + shift + o` | `resize-axis vertical increase` |
| `alt + shift + s` | `mode resize` (then `h`, `j`, `k`, `l` resize; `esc` or `enter` leave) |
| `alt + shift + space` | `toggle-float` |
| `alt + plus` | `toggle-maximize` |
| `alt + shift + f` | `toggle-monocle` |
| `alt + minus` | `minimize` |
| `alt + shift + q` | `close` |
| `alt + shift + v` | `cycle-layout next` |
| `alt + shift + x` | `flip-layout horizontal` |
| `alt + shift + y` | `flip-layout vertical` |
| `alt + shift + return` | `promote` |
| `alt + comma` | `cycle-workspace previous` |
| `alt + period` | `cycle-workspace next` |
| `alt + grave` | `focus-last-workspace` |
| `alt + 1` to `alt + 9` | `focus-workspace 0` to `8` |
| `alt + shift + 1` to `alt + shift + 9` | `move-to-workspace 0` to `8` |
| `alt + shift + g` | `toggle-game-mode` |
| `pause` | `toggle-pause` |
| `alt + f12` | `toggle-pause` |
| `alt + shift + r` | `reload-configuration` |
| `alt + shift + w` | `retile` |
| `alt + shift + e` | `stop` |

`alt` with an arrow is left unbound on purpose: it is Back and Forward in browsers and the parent folder in Explorer. The default file shows how to add those bindings if the user wants them.

## Escape keys

| Keys | Command | Effect |
|---|---|---|
| `pause`, `alt + f12` | `toggle-pause` | Tiling off: Mochi stops touching windows and leaves them where they are, and the borders go. The daemon keeps running, so the same key turns tiling on again, which adopts windows opened meanwhile and retiles. |
| `alt + shift + e` | `stop` | Stops the daemon. It shows every window it was hiding, including those on other workspaces, removes borders and transparency and unbinds every key. |
| `alt + shift + g` | `toggle-game-mode` | Pauses tiling and suspends every binding except this one. Press again to come back. |

These are ordinary lines of the hotkey file and can be changed or removed like any other. When you edit a user's file, keep a binding for `toggle-pause` and one for `stop`. From a terminal the same actions are `mochic toggle-pause`, `mochic stop` and `mochic toggle-game-mode`.

If a scheduled watchdog task is registered (`MochiWatchdog`), it starts Mochi again within five minutes of a stop.

## Change a binding

1. Back up the file: `Copy-Item "$env:USERPROFILE\.config\mochi\hotkeys" "$env:USERPROFILE\.config\mochi\hotkeys.bak"`
2. Edit or add the line, for example `alt + shift + t : toggle-scratchpad term` (the scratchpad `term` must exist in `mochi.json`).
3. Run `mochic check`. It reads the hotkey file too and names each line that does not parse.
4. Save. Run `mochic hotkeys` to see what Mochi bound.

## When a key does nothing

- Run `mochic hotkeys`. A typo shows up as a missing binding or a parse error with its line number.
- A mode may be on. In the shipped resize mode, `esc` leaves.
- A window that runs as administrator has the focus. Windows does not deliver its key presses to Mochi, which runs as a normal user unless `"elevated": true` is set in `mochi.json` and the daemon was started with `mochic start` and the Windows prompt accepted.
- Another program may hold the same key. Between two keyboard hooks on one key, the one installed last wins.
- Windows may have dropped the hook after a long freeze. A reload (`mochic reload-configuration`) installs it again.
- `numpad0` to `numpad9` only fire while NumLock is on. With it off the keys report as `insert`, `end`, arrows and so on.
- AltGr on layouts such as Swiss or German counts as neither `ctrl` nor `alt`, so a `ctrl + alt` binding cannot take `@`, `[`, `]`, `{`, `}` or `|`.
- The log in `%LOCALAPPDATA%\mochi` records every hotkey that fired and every shell command that failed to start.

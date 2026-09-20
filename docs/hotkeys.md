# Hotkeys

Mochi binds keys itself. There is no second program to install, start or keep
alive, and a bound key does not spawn anything: the press is matched inside the
daemon and turned straight into the command, so it acts on the frame it was
pressed.

## The file

```
%USERPROFILE%\.config\mochi\hotkeys
```

Mochi looks for it in this order and takes the first hit:

1. `--hotkeys <path>`, on `mochi` or on `mochic start`,
2. `$MOCHI_HOTKEYS`,
3. `%USERPROFILE%\.config\mochi\hotkeys`,
4. `%USERPROFILE%\.config\mochi\whkdrc`, for a desktop that came from a
   standalone hotkey daemon and still uses that file name.

`mochic quickstart` writes a starting file: focus, moving windows, resizing,
workspaces, layouts and game mode, one key each. Saving the file reloads it;
nothing has to be restarted. `mochi --no-hotkeys` binds no keys at all, for a
setup that drives Mochi from something else.

## Syntax

```
.shell pwsh                      # cmd, powershell, pwsh or bash. Default: cmd

# Comments start with a hash.
alt + h                 : focus left
alt + shift + q         : close
alt + shift + return    : pwsh -NoProfile -Command "code ."
```

Left of the colon: modifiers and exactly one key, in any order, in any case,
with any spacing. The modifiers are `alt`, `ctrl` (`control`), `shift` and `win`
(`super`, `meta`).

Right of the colon: a Mochi command, or a command line for the shell. A line is
a Mochi command when its first word is one `mochic` knows; everything else is
handed to the shell. `mochic` may be written in front of a command and changes
nothing, so a file written for a separate hotkey daemon works unchanged:

```
alt + h : focus left
alt + h : mochic focus left
```

With the `mochic` prefix a command that does not parse is an error at load time
instead of a shell line that fails later.

### Groups

One bracket group on each side expands pairwise:

```
alt + [1,2,3]           : focus-workspace [0,1,2]
alt + shift + [1,2,3]   : move-to-workspace [0,1,2]
```

The trigger decides. Without a group on the left, the right-hand side is taken
exactly as written, brackets and all, because a command line is allowed to
contain them:

```
alt + b                 : [console]::beep(440,200)
```

### Key names

Letters and digits by themselves; `f1` to `f24`; the arrows as `left`, `right`,
`up`, `down`; `space`, `return` (`enter`), `tab`, `esc`, `backspace`, `delete`,
`insert`, `home`, `end`, `pageup`, `pagedown`; `numpad0` to `numpad9` and the
numpad operators; the punctuation keys by name (`comma`, `period`, `slash`,
`semicolon`, `quote`, `backtick`, `lbracket`, `rbracket`, `minus`, `plus`); the
media and volume keys; `apps`. `mochic hotkeys` prints what Mochi made of the
file, which is the fastest way to check a name.

## What a key press does

The press is swallowed only when it matches a binding exactly, and its release
is swallowed with it so no application sees half a keystroke. Everything else,
including a chord that merely starts with the right modifiers, goes to the
desktop untouched.

An Alt binding does not leave the application in a menu. A window that sees Alt
go down and come back up with nothing in between opens its menu bar, and
swallowing the key would have created exactly that gap, so one harmless Ctrl
goes through in its place. It is tagged as Mochi's own, so it cannot come back
round as a binding.

AltGr is not Ctrl+Alt. On a Swiss, German or any other layout where AltGr
reports itself as right Alt plus left Ctrl, that pair counts as neither, so a
`ctrl + alt` binding cannot eat `@`, `[`, `]`, `{`, `}` or `|`.

Windows will not deliver a key press that happened over a window running as
administrator to a program that is not. That is the same limit that stops Mochi
managing those windows, and there is no way around it short of running Mochi
elevated.

## Commands

| Command | What it does |
|---|---|
| `mochic hotkeys` | Print every binding, and every line that did not parse |
| `mochic hotkeys --json` | The same as a document, for a script |
| `mochic set-hotkeys enable` | Bind keys again |
| `mochic set-hotkeys disable` | Stop binding keys. The daemon keeps tiling |
| `mochic toggle-game-mode` | Game mode, see below |
| `mochic reload-configuration` | Re-read `mochi.json` and the hotkey file |

## Game mode

`toggle-game-mode` pauses tiling and suspends every binding except the one
bound to `toggle-game-mode` itself, so the game in front gets every other key on
the keyboard. Pressing it again resumes tiling and brings the bindings back. It
is one command inside the daemon: nothing is restarted, no second config file is
written, and there is no window in which the keyboard belongs to nobody.

```
alt + shift + g : toggle-game-mode
```

## When a key does nothing

- `mochic hotkeys` shows what is bound, so a typo shows up as a missing line or
  as a parse error with its line number.
- The log at `%LOCALAPPDATA%\mochi\mochi.log` records every hotkey that fired
  and every shell command that failed to start.
- Another program may own the key. Windows calls the most recently installed
  keyboard hook first, so between Mochi and another hotkey daemon on the same
  key, whichever started last wins and the other one never sees the press. Two
  of them running is not a crash, it is a coin toss decided at startup: stop the
  old one.
- Windows drops a hook whose callback is too slow. Mochi's does a hash lookup
  and nothing else, but a machine that was frozen long enough (a debugger on the
  daemon, for instance) can still lose it. Reloading the file installs it again.

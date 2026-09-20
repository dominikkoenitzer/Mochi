# Importing an existing setup

Mochi uses the config key names and the command names that are common among
tiling window managers on Windows, so moving a working setup over is mechanical.
Nothing here changes a file of your current setup: the scripts write new files
next to the old ones and the switch is reversible.

## What maps to what

| your current tiling window manager | Mochi |
|---|---|
| its JSON config, usually in `%USERPROFILE%` | `%USERPROFILE%\mochi.json` |
| its daemon | `mochi.exe` |
| its command line client | `mochic.exe` |
| its separate hotkey daemon | nothing to install, `mochi.exe` binds the keys |
| `%USERPROFILE%\.config\whkdrc` | `%USERPROFILE%\.config\mochi\hotkeys` |
| `applications.json` | same file, keep `app_specific_configuration_path` |
| its log file | `%LOCALAPPDATA%\mochi\mochi.log` |

Anything that read the old daemon's event pipe has no counterpart. It goes
dark while Mochi is the window manager. Mochi's own event subscriptions are
`mochic subscribe-pipe`, so a script can follow along.

## 1. Install

```
git clone git@github.com:dominikkoenitzer/Mochi.git
cd Mochi
.\scripts\install.ps1
```

This builds the release binaries, copies `mochi.exe` and `mochic.exe` to
`%LOCALAPPDATA%\Programs\Mochi\bin` and puts that folder on the user PATH. Open
a new shell afterwards so the PATH is picked up.

## 2. Import the configuration and the hotkeys

`scripts\import-config.ps1` needs three things, it has no defaults that guess at
your setup:

- `-Config`, the JSON configuration you use today
- `-Hotkeys`, the hotkey file you use today
- `-Command`, the name of the command line program that hotkey file calls today,
  without the `.exe`

Look at what would change:

```
.\scripts\import-config.ps1 -Config $env:USERPROFILE\wm.json -Hotkeys $env:USERPROFILE\.config\whkdrc -Command wmc
```

Then write it:

```
.\scripts\import-config.ps1 -Config $env:USERPROFILE\wm.json -Hotkeys $env:USERPROFILE\.config\whkdrc -Command wmc -Apply
```

That produces:

- `%USERPROFILE%\mochi.json`, a copy of your JSON config with the `$schema` line
  rewritten. Everything else is kept, including
  `app_specific_configuration_path`: Mochi reads the usual `applications.json`
  format as it is.
- `%USERPROFILE%\.config\mochi\hotkeys`, the hotkey file with every standalone
  call of the old CLI name replaced by `mochic`. Mochi binds that file itself,
  reads the syntax the old file already uses, and rebinds every time it is
  saved. `-HotkeyDir` writes it somewhere else, in which case start the daemon
  with `--hotkeys <path>`.

The script leaves both source files untouched and refuses to overwrite an
existing `mochi.json` unless `-Force` is given. Lines that still point at the
old setup, for example a launcher name that is not a standalone token, are
printed so you can fix them by hand.

Every key `mochi.json` reads is listed in [configuration.md](configuration.md),
every command in [cli.md](cli.md), and the hotkey file down to the key names in
[hotkeys.md](hotkeys.md).

## 3. Game mode

Game mode is the built in `mochic toggle-game-mode`: it pauses tiling and
suspends every binding except the one bound to `toggle-game-mode` itself, so the
game in front gets the rest of the keyboard, and the same key brings both back.
Nothing is restarted and there is no second hotkey file to keep in step.

The import turns a line that ran a game mode script into that command and keeps
its keys, so a setup that had game mode on `alt + shift + g` still has it there:

```
alt + shift + g : mochic toggle-game-mode
```

## 4. Switch over

Stop your current window manager and its hotkey daemon, then:

```
mochic start
```

Only one window manager may run at a time. Stopping the old one first also
restores the windows it managed before Mochi takes over. The hotkey daemon
matters as much: Mochi binds the keys itself, so a key left bound in both places
fires both bindings.

## 5. Autostart

`scripts\autostart.ps1` registers a HKCU Run value called `Mochi` that runs
`mochic start` through a hidden PowerShell launcher, so no console flashes at
login.

If your current setup starts at login from a shortcut in the Startup folder,
`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\<name>.lnk`, the same
script moves it aside for you. A HKCU Run value of that name is backed up and
removed the same way.

```
.\scripts\autostart.ps1                                # show what is registered today
.\scripts\autostart.ps1 -DisableStartupItem <name>     # renames <name>.lnk to <name>.lnk.disabled
.\scripts\autostart.ps1 -Enable                        # adds the Mochi Run value
```

## Going back

```
mochic stop
```

then start your previous window manager again, and whatever it used for its
hotkeys. In that order: while Mochi is running it is binding the keys itself.
The old file is where it always was and still calls the old CLI name, because
the import only ever wrote a copy.

For the autostart:

```
.\scripts\autostart.ps1 -Disable
.\scripts\autostart.ps1 -EnableStartupItem <name>
```

And to remove Mochi completely:

```
.\scripts\install.ps1 -Uninstall
```

That deletes the binaries and the PATH entry. `mochi.json`, the hotkey file and
the log stay until they are deleted by hand. Nothing that belongs to your old
setup is touched by the installer.

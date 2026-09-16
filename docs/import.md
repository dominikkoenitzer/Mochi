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
| `%USERPROFILE%\.config\whkdrc` | `%USERPROFILE%\.config\mochi\whkdrc` |
| `applications.json` | same file, keep `app_specific_configuration_path` |
| its log file | `%LOCALAPPDATA%\mochi\mochi.log` |

A status bar that reads the old daemon's event pipe has no counterpart. It goes
dark while Mochi is the window manager. Mochi's own event subscriptions are
`mochic subscribe-pipe`, a bar that speaks them comes later.

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
- `-Hotkeys`, the whkdrc you use today
- `-Command`, the name of the command line program your whkdrc calls today,
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
- `%USERPROFILE%\.config\mochi\whkdrc`, the hotkey file with every standalone
  call of the old CLI name replaced by `mochic`. whkd only ever loads a file
  called `whkdrc` from `$env:WHKD_CONFIG_HOME`, so this is the copy it reads.

The script leaves both source files untouched and refuses to overwrite an
existing `mochi.json` unless `-Force` is given. Lines that still point at the
old setup, for example a launcher name that is not a standalone token, are
printed so you can fix them by hand.

Every key `mochi.json` reads is listed in [configuration.md](configuration.md),
every command in [cli.md](cli.md).

## 3. Game mode

`scripts\game-mode.ps1` is the game mode toggle: it pauses tiling and restarts
whkd with a minimal config in which only the toggle key works, then back. Point
the hotkey at it, for example by running the import with

```
.\scripts\import-config.ps1 -Config <config> -Hotkeys <whkdrc> -Command <old CLI name> -Apply -Force -GameModeScript C:\path\to\Mochi\scripts\game-mode.ps1
```

which repoints hotkey lines that call another `game-mode.ps1`. The minimal
hotkey file `%USERPROFILE%\.config\mochi\gamemode\whkdrc` is written the first
time game mode runs. whkd has to be on the PATH, or given with `-WhkdPath`,
otherwise only the tiling pause is toggled.

## 4. Switch over

Stop your current window manager and its hotkey daemon, then:

```
$env:WHKD_CONFIG_HOME = "$env:USERPROFILE\.config\mochi"; mochic start --whkd
```

Only one window manager may run at a time. Stopping the old one first also
restores the windows it managed before Mochi takes over.

## 5. Autostart

`scripts\autostart.ps1` registers a HKCU Run value called `Mochi` that runs
`mochic start --whkd` with `WHKD_CONFIG_HOME` set, through a hidden PowerShell
launcher so no console flashes at login.

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
mochic stop --whkd
Remove-Item Env:WHKD_CONFIG_HOME -ErrorAction SilentlyContinue
```

then start your previous window manager and its hotkey daemon again. Without
`WHKD_CONFIG_HOME`, whkd reads `%USERPROFILE%\.config\whkdrc` again, which still
calls the old CLI name.

For the autostart:

```
.\scripts\autostart.ps1 -Disable
.\scripts\autostart.ps1 -EnableStartupItem <name>
```

And to remove Mochi completely:

```
.\scripts\install.ps1 -Uninstall
```

That deletes the binaries and the PATH entry. `mochi.json` and the log stay
until they are deleted by hand. Nothing that belongs to your old setup is
touched by the installer.

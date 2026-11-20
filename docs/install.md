# Other ways to install

## Quiet install

`Mochi-Setup.exe --quiet` installs without asking, and
`Mochi-Setup.exe --uninstall --quiet` removes Mochi the same way. Both exit with
0 when everything worked. Setup is a windowed program, so a script that needs
the exit code starts it with `Start-Process -Wait -PassThru`.

## PowerShell

For a scripted install, open PowerShell (Start menu, type PowerShell, Enter) and
paste this line:

```
irm https://mochi.punds.ch/install.ps1 | iex
```

It works in Windows PowerShell 5.1 and PowerShell 7. It downloads the
[latest release](https://github.com/dominikkoenitzer/Mochi/releases/latest),
checks its SHA256 and installs the same way Setup does: the same folder, PATH
entry, default files, login start and Start menu entry, and then it starts
Mochi right away, which tiles every window on screen. It adds no Apps & features
entry; `-Uninstall` below removes what it installed.

The same line in this form takes switches:

```
& ([scriptblock]::Create((irm https://mochi.punds.ch/install.ps1))) -SkipStart -SkipAutostart
```

`-SkipStart` installs without starting Mochi now. `-SkipAutostart` does not
register the login start. `-Version v0.1.32` installs that release instead of
the latest. `-SkipPath` leaves your PATH alone, and `-SkipQuickstart` does not
write the default `mochi.json` and hotkey file. `-Uninstall` removes Mochi the
way the Apps & features entry does.

To update this way, run the line again. It refuses while Mochi is running, so
stop Mochi first with `alt + shift + e` or `mochic stop`.

## Zip

Every release also carries `Mochi-x86_64-pc-windows-msvc.zip` with `mochi.exe`,
`mochic.exe` and `mochi.schema.json`, and a `.sha256` file beside it. Unpack it
anywhere and run `mochic start`, which starts the `mochi.exe` next to it.
Nothing is registered: no PATH entry, no login start, no Start menu entry.

## Watchdog

The Run value only fires at login, so a daemon that stops during the day leaves
the desktop untiled until the next one. `scripts\autostart.ps1` can register a
task that asks every five minutes whether Mochi is up, and starts it when it is
not. When Mochi is already up, `mochic start` only says so, which makes the
check cheap. The script needs a clone of the repository (see
[Build from source](../README.md#build-from-source)), not a build:

```
powershell -ExecutionPolicy Bypass -File .\scripts\autostart.ps1 -Watchdog
```

With the watchdog on, `alt + shift + e` stops Mochi only until the next check:
the task starts it again within five minutes. `-Disable` removes the watchdog
along with the Run value, and that is the way out for good. Without a switch the
script shows what is registered today, and `-DisableStartupItem <name>` moves
another program's startup item aside until `-EnableStartupItem <name>` puts it
back.

## Your data, updates and removal

Mochi makes no network connection and keeps everything on your PC:

| What | Where |
|---|---|
| `mochi.exe` and `mochic.exe` | `%LOCALAPPDATA%\Programs\Mochi\bin`, added to your user PATH |
| The copy of `Mochi-Setup.exe` that removes Mochi | `%LOCALAPPDATA%\Programs\Mochi` |
| Settings | `%USERPROFILE%\mochi.json` |
| Hotkeys | `%USERPROFILE%\.config\mochi\hotkeys` |
| The log, and the record of windows Mochi has taken off screen | `%LOCALAPPDATA%\mochi` |
| The start at login | the HKCU Run value `Mochi` |

Setup writes a default `mochi.json` and hotkey file only when there are none,
and never overwrites yours. A terminal that was already open does not see the
new PATH: open a new one before you type `mochic`, or it answers that `mochic`
is not recognized, which reads like the install failed when it did not.

To remove Mochi, open Settings, Apps, Installed apps, and uninstall Mochi. It
removes the login start and the watchdog task if there is one, stops Mochi,
which puts every window back, and then removes the Start menu entry, the
program files, the PATH entry and the Apps & features entry. Your settings, hotkeys and log stay.
To remove those too, delete `%USERPROFILE%\mochi.json`,
`%USERPROFILE%\.config\mochi` and `%LOCALAPPDATA%\mochi` afterwards.

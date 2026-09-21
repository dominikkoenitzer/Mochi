<#
.SYNOPSIS
    Turns Mochi's autostart on and off, and another program's autostart with it.

.DESCRIPTION
    -Enable writes a HKCU Run value called Mochi that runs `mochic start` at
    login, which brings the hotkeys up with the daemon. A program that starts
    from a shortcut in the Startup folder is moved aside by
    -DisableStartupItem <name>, which renames <name>.lnk to <name>.lnk.disabled,
    and put back by -EnableStartupItem <name>. A HKCU Run value of the same name
    is backed up and removed the same way, and restored on the way back. A Run
    value called Mochi that somebody else wrote is backed up before -Enable
    replaces it and restored by -Disable, and -Disable never removes a value
    that does not run mochic. Nothing else is touched.

    Without a switch the script only reports the current state.

    Switching window managers at login is two commands:
        .\scripts\autostart.ps1 -DisableStartupItem <name>
        .\scripts\autostart.ps1 -Enable

    And back:
        .\scripts\autostart.ps1 -Disable
        .\scripts\autostart.ps1 -EnableStartupItem <name>

.PARAMETER Enable
    Register the Mochi Run value.

.PARAMETER Disable
    Remove the Mochi Run value.

.PARAMETER DisableStartupItem
    Name of a startup item to move aside, reversibly. The name is the file name
    of the Startup folder shortcut without .lnk, and also the name of a Run
    value if there is one.

.PARAMETER EnableStartupItem
    Name of a startup item to put back.

.PARAMETER MochicPath
    mochic.exe to start. Default %LOCALAPPDATA%\Programs\Mochi\bin\mochic.exe.

.PARAMETER ShowConsole
    Start mochic directly instead of through a hidden launcher. Simpler, but a
    console window flashes at login until mochic ships a no-console build.

.EXAMPLE
    .\scripts\autostart.ps1

.EXAMPLE
    .\scripts\autostart.ps1 -Enable

.EXAMPLE
    .\scripts\autostart.ps1 -DisableStartupItem myoldwm
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [switch] $Enable,
    [switch] $Disable,
    [string] $DisableStartupItem,
    [string] $EnableStartupItem,
    [string] $MochicPath = (Join-Path $env:LOCALAPPDATA 'Programs\Mochi\bin\mochic.exe'),
    [switch] $Watchdog,
    [switch] $ShowConsole
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$script:BackupKey = 'HKCU:\Software\Mochi'
$script:BackupPrefix = 'RunBackup_'
$script:MochiValue = 'Mochi'
$script:WatchdogTask = 'MochiWatchdog'
# Asked for, not assumed. Group Policy folder redirection on a managed machine
# moves the Start Menu elsewhere, and a hardcoded path then reports "found no
# autostart to disable" while the shortcut sits where it always was, so the user
# believes their old window manager is off, logs in, and two of them fight over
# the desktop.
$script:StartupDir = [Environment]::GetFolderPath('Startup')
if ([string]::IsNullOrWhiteSpace($script:StartupDir)) {
    $script:StartupDir = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Startup'
}

# Where install.ps1 recorded the install, so -Enable finds a non-default one.
function Get-RecordedInstallRoot {
    try {
        $value = (Get-ItemProperty -Path $script:BackupKey -Name 'InstallRoot' -ErrorAction Stop).InstallRoot
        if ([string]::IsNullOrWhiteSpace($value)) { return $null }
        return $value
    } catch {
        return $null
    }
}

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "==> $Message"
}

function Write-Detail {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "    $Message"
}

function Get-RunValue {
    param([Parameter(Mandatory)][string] $Name)
    $item = Get-ItemProperty -Path $script:RunKey -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$Name
}

# The raw value and its registry kind.
#
# Get-ItemProperty EXPANDS a REG_EXPAND_SZ, so backing up through it and
# restoring as a plain string rewrites another program's autostart with the
# path frozen to whatever %LOCALAPPDATA% happened to be that day. It keeps
# working until the profile or the drive layout changes, and then that program
# silently stops starting with nothing pointing back at Mochi. install.ps1
# already takes this care with PATH; this is the same care for Run values.
function Get-RunValueRaw {
    param([Parameter(Mandatory)][string] $Name)
    try {
        $key = Get-Item -LiteralPath $script:RunKey -ErrorAction Stop
        $raw = $key.GetValue($Name, $null, 'DoNotExpandEnvironmentNames')
        if ($null -eq $raw) { return $null }
        return [PSCustomObject]@{
            Value = [string] $raw
            Kind  = [string] $key.GetValueKind($Name)
        }
    } catch {
        return $null
    }
}

function Get-BackupValue {
    param([Parameter(Mandatory)][string] $Name)
    $value = "$script:BackupPrefix$Name"
    $item = Get-ItemProperty -Path $script:BackupKey -Name $value -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$value
}

function Get-StartupLink {
    param([Parameter(Mandatory)][string] $Name)
    return (Join-Path $script:StartupDir "$Name.lnk")
}

function Get-MochiCommand {
    if ($ShowConsole) {
        return "`"$MochicPath`" start"
    }

    # A single quote in the path ends the string early and leaves a command
    # line PowerShell cannot parse. A Windows account called O'Brien is enough,
    # and because the launcher is hidden nobody ever sees the error: Mochi just
    # never starts at login. Doubling it is how a single quoted string escapes
    # one.
    $quoted = $MochicPath.Replace("'", "''")
    $inner = "Start-Process -FilePath '$quoted' -ArgumentList 'start' -WindowStyle Hidden"
    return "powershell.exe -NoProfile -WindowStyle Hidden -Command `"$inner`""
}

# Mochi's own Run value is a name like any other: somebody may already own it.
# Only a command that runs mochic is ours to overwrite or to remove.
function Test-MochiRunValue {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Value)
    # CultureInvariant is load bearing, not decoration. Case-insensitive
    # matching without it follows the machine's culture, and Turkish and
    # Azeri do not case 'I' the way the rest of the world does: an upper
    # case MOCHIC lowercases to a dotless letter and stops matching. On
    # those machines -Disable would decide Mochi's own Run value belonged
    # to somebody else and refuse to remove it, leaving the user unable to
    # turn off an autostart they own.
    return [regex]::IsMatch(
        $Value,
        '\bmochic(\.exe)?\b',
        [Text.RegularExpressions.RegexOptions]'IgnoreCase, CultureInvariant')
}

function Backup-RunValue {
    param(
        [Parameter(Mandatory)][string] $Name,
        [Parameter(Mandatory)][AllowEmptyString()][string] $Value
    )
    $backupValue = "$script:BackupPrefix$Name"
    if (-not (Test-Path $script:BackupKey)) { New-Item -Path $script:BackupKey -Force | Out-Null }

    # The raw text and the kind, so the value goes back exactly as it was.
    $raw = Get-RunValueRaw -Name $Name
    $text = if ($null -ne $raw) { $raw.Value } else { $Value }
    $kind = if ($null -ne $raw) { $raw.Kind } else { 'String' }

    New-ItemProperty -Path $script:BackupKey -Name $backupValue -Value $text -PropertyType String -Force | Out-Null
    New-ItemProperty -Path $script:BackupKey -Name "${backupValue}_Kind" -Value $kind -PropertyType String -Force | Out-Null
    Write-Detail "Run value backed up to $script:BackupKey\$backupValue ($kind)"
}

function Restore-RunValue {
    param([Parameter(Mandatory)][string] $Name)
    $backup = Get-BackupValue -Name $Name
    if ($null -eq $backup) { return $false }
    $backupValue = "$script:BackupPrefix$Name"

    # Back under its own kind. An ExpandString written as String keeps its
    # %VARIABLES% as dead literal text.
    $kind = 'String'
    try {
        $recorded = (Get-ItemProperty -Path $script:BackupKey -Name "${backupValue}_Kind" -ErrorAction Stop)."${backupValue}_Kind"
        if ($recorded -eq 'ExpandString') { $kind = 'ExpandString' }
    } catch {
        # Backed up by an older version of this script, which only ever wrote
        # plain strings. String is what it was.
    }

    New-ItemProperty -Path $script:RunKey -Name $Name -Value $backup -PropertyType $kind -Force | Out-Null
    Remove-ItemProperty -Path $script:BackupKey -Name $backupValue
    Remove-ItemProperty -Path $script:BackupKey -Name "${backupValue}_Kind" -ErrorAction SilentlyContinue
    return $true
}

function Enable-MochiAutostart {
    # A missing binary is a refusal, not a note. The launcher runs hidden, so a
    # Run value pointing at nothing fails silently at every single login: Mochi
    # never starts and there is nothing on screen to say why. Registering it
    # anyway, after printing one buried line, was the worst of both.
    if (-not (Test-Path -LiteralPath $MochicPath)) {
        $recorded = Get-RecordedInstallRoot
        if ($recorded) {
            $candidate = Join-Path $recorded 'bin\mochic.exe'
            if (Test-Path -LiteralPath $candidate) {
                Write-Detail "using the recorded install location $recorded"
                $script:MochicPath = $candidate
                $MochicPath = $candidate
            }
        }
    }
    if (-not (Test-Path -LiteralPath $MochicPath)) {
        Write-Step 'nothing registered'
        Write-Detail "$MochicPath does not exist"
        Write-Detail 'run scripts\install.ps1 first, or pass -MochicPath'
        Write-Detail 'a Run entry pointing at a missing file fails silently at every login'
        return
    }
    $command = Get-MochiCommand
    $current = Get-RunValue -Name $script:MochiValue

    # Every other name this script touches is backed up before it is moved
    # aside and put back afterwards. Mochi's own name gets the same treatment
    # rather than -Force writing over whatever a stranger left there.
    $foreign = ($null -ne $current) -and -not (Test-MochiRunValue -Value "$current")
    if ((-not $foreign) -and ($current -eq $command)) {
        Write-Detail 'already registered with the same command'
        Write-Detail "$script:RunKey\$script:MochiValue = $command"
        return
    }
    if ($foreign) {
        Write-Detail "$script:RunKey\$script:MochiValue is already taken by something else:"
        Write-Detail "  $current"
        Write-Detail 'it is backed up first and -Disable puts it back'
    }

    Write-Detail "$script:RunKey\$script:MochiValue = $command"
    if ($PSCmdlet.ShouldProcess("$script:RunKey\$script:MochiValue", 'set')) {
        if ($foreign) { Backup-RunValue -Name $script:MochiValue -Value "$current" }
        New-ItemProperty -Path $script:RunKey -Name $script:MochiValue -Value $command -PropertyType String -Force | Out-Null
        Write-Detail 'registered'
    }
}

function Disable-MochiAutostart {
    $current = Get-RunValue -Name $script:MochiValue
    if ($null -eq $current) {
        Write-Detail 'no Mochi Run value, nothing to do'
    } elseif (-not (Test-MochiRunValue -Value "$current")) {
        # Not ours, so not ours to delete.
        Write-Detail "$script:RunKey\$script:MochiValue does not run mochic, leaving it alone:"
        Write-Detail "  $current"
        return
    } elseif ($PSCmdlet.ShouldProcess("$script:RunKey\$script:MochiValue", 'remove')) {
        Remove-ItemProperty -Path $script:RunKey -Name $script:MochiValue
        Write-Detail 'removed'
    } else {
        return
    }

    if ($null -ne (Get-BackupValue -Name $script:MochiValue)) {
        if ($PSCmdlet.ShouldProcess("$script:RunKey\$script:MochiValue", 'restore from backup')) {
            [void] (Restore-RunValue -Name $script:MochiValue)
            Write-Detail 'the Run value that was there before Mochi was restored'
        }
    }
}

function Disable-OtherAutostart {
    param([Parameter(Mandatory)][string] $Name)

    $found = $false
    $link = Get-StartupLink -Name $Name
    $linkOff = "$link.disabled"

    if (Test-Path $link) {
        $found = $true
        if ($PSCmdlet.ShouldProcess($link, 'rename to .disabled')) {
            Move-Item -LiteralPath $link -Destination $linkOff -Force
            Write-Detail "startup shortcut moved to $linkOff"
        }
    } elseif (Test-Path $linkOff) {
        Write-Detail 'startup shortcut is already disabled'
        $found = $true
    }

    $run = Get-RunValue -Name $Name
    if ($null -ne $run) {
        $found = $true
        if ($PSCmdlet.ShouldProcess("$script:RunKey\$Name", 'back up and remove')) {
            Backup-RunValue -Name $Name -Value "$run"
            Remove-ItemProperty -Path $script:RunKey -Name $Name
            Write-Detail 'Run value removed'
        }
    }

    if (-not $found) { Write-Detail "found no autostart called $Name to disable" }
    Write-Detail 'a program that is already running is not stopped, stop it yourself'
}

function Enable-OtherAutostart {
    param([Parameter(Mandatory)][string] $Name)

    $found = $false
    $link = Get-StartupLink -Name $Name
    $linkOff = "$link.disabled"

    if (Test-Path $linkOff) {
        $found = $true
        if ($PSCmdlet.ShouldProcess($linkOff, "rename back to $Name.lnk")) {
            Move-Item -LiteralPath $linkOff -Destination $link -Force
            Write-Detail "startup shortcut restored to $link"
        }
    } elseif (Test-Path $link) {
        Write-Detail 'startup shortcut is already in place'
        $found = $true
    }

    $backup = Get-BackupValue -Name $Name
    if ($null -ne $backup) {
        $found = $true
        if ($PSCmdlet.ShouldProcess("$script:RunKey\$Name", 'restore from backup')) {
            [void] (Restore-RunValue -Name $Name)
            Write-Detail 'Run value restored'
        }
    }

    if (-not $found) { Write-Detail "found no autostart called $Name to restore" }
}

function Show-AutostartStatus {
    Write-Step 'Mochi'
    $mochi = Get-RunValue -Name $script:MochiValue
    if ($null -eq $mochi) {
        Write-Detail "no Run value called $script:MochiValue"
        Write-Detail "-Enable would register: $script:RunKey\$script:MochiValue = $(Get-MochiCommand)"
    } else {
        Write-Detail "$script:RunKey\$script:MochiValue = $mochi"
    }

    Show-WatchdogStatus

    Write-Step 'startup items moved aside by this script'
    $anyDisabled = $false
    $off = @(Get-ChildItem -LiteralPath $script:StartupDir -Filter '*.lnk.disabled' -ErrorAction SilentlyContinue)
    foreach ($file in $off) {
        Write-Detail "startup shortcut disabled: $($file.FullName)"
        $anyDisabled = $true
    }
    $backups = Get-ItemProperty -Path $script:BackupKey -ErrorAction SilentlyContinue
    if ($null -ne $backups) {
        foreach ($property in $backups.PSObject.Properties) {
            # Every backup is stored as two values, the data and a companion
            # `_Kind` holding the registry type it has to be restored as.
            # Counting both reported one backed up Run value as two and sent
            # the reader looking for a "<name>_Kind" entry that was never
            # theirs.
            if ($property.Name.StartsWith($script:BackupPrefix) -and
                -not $property.Name.EndsWith('_Kind')) {
                $name = $property.Name.Substring($script:BackupPrefix.Length)
                Write-Detail "Run value backed up: $name = $($property.Value)"
                $anyDisabled = $true
            }
        }
    }
    if (-not $anyDisabled) { Write-Detail 'none' }

    Write-Step 'switches'
    Write-Detail '-Enable  -Disable  -DisableStartupItem <name>  -EnableStartupItem <name>'
}

if ($Enable -and $Disable) { throw 'pick either -Enable or -Disable' }
if ($DisableStartupItem -and $EnableStartupItem) { throw 'pick either -DisableStartupItem or -EnableStartupItem' }

$acted = $false

# Mochi exits and nothing brings it back. The Run value only fires at login,
# so a daemon that stops at eleven in the morning leaves the desktop untiled
# until the machine is restarted, and the user is given no sign that anything
# has happened at all.
#
# `mochic start` answers 'mochi is already running' in about thirty
# milliseconds when it is, so asking every few minutes costs nothing and needs
# no watchdog process of its own. LIMITED keeps the task at normal rights: a
# window manager started with highest privileges can move every window on the
# machine, which is not a trade worth making to restart one.
# schtasks writes to stderr when a task is not there, and this script runs
# with ErrorActionPreference = Stop, so a plain query for a watchdog that was
# never registered printed a red error at the user in the ordinary case. The
# absence of a task is an answer, not a fault.
function Test-WatchdogTask {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'SilentlyContinue'
    try {
        schtasks /query /tn $script:WatchdogTask 2>$null | Out-Null
        return ($LASTEXITCODE -eq 0)
    } finally {
        $ErrorActionPreference = $previous
    }
}
function Enable-MochiWatchdog {
    if (-not (Test-Path -LiteralPath $MochicPath)) {
        Write-Detail "no mochic at $MochicPath, not registering the watchdog"
        return
    }
    if (-not $PSCmdlet.ShouldProcess($script:WatchdogTask, 'register a scheduled task')) { return }
    $out = schtasks /create /tn $script:WatchdogTask /tr "`"$MochicPath`" start" `
        /sc minute /mo 5 /rl LIMITED /f 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Detail "could not register the watchdog: $out"
        return
    }
    Write-Detail 'watchdog registered, checks every 5 minutes'
}

function Disable-MochiWatchdog {
    if (-not (Test-WatchdogTask)) { return }
    if (-not $PSCmdlet.ShouldProcess($script:WatchdogTask, 'remove the scheduled task')) { return }
    $out = schtasks /delete /tn $script:WatchdogTask /f 2>&1
    if ($LASTEXITCODE -ne 0) { Write-Detail "could not remove the watchdog: $out"; return }
    Write-Detail 'watchdog removed'
}

function Show-WatchdogStatus {
    if (Test-WatchdogTask) {
        Write-Detail "watchdog: registered, restarts Mochi within 5 minutes if it stops"
    } else {
        Write-Detail 'watchdog: not registered (-Watchdog turns it on)'
    }
}
if ($DisableStartupItem) { Write-Step "disabling the $DisableStartupItem autostart"; Disable-OtherAutostart -Name $DisableStartupItem; $acted = $true }
if ($EnableStartupItem) { Write-Step "restoring the $EnableStartupItem autostart"; Enable-OtherAutostart -Name $EnableStartupItem; $acted = $true }
if ($Enable) { Write-Step 'enabling Mochi autostart'; Enable-MochiAutostart; $acted = $true }
if ($Disable) { Write-Step 'disabling Mochi autostart'; Disable-MochiAutostart; $acted = $true }
if ($Disable) { Disable-MochiWatchdog }
if ($Watchdog) { Write-Step 'enabling the Mochi watchdog'; Enable-MochiWatchdog; $acted = $true }

if (-not $acted) {
    Write-Output 'Autostart status, nothing changed.'
    Show-AutostartStatus
}

<#
.SYNOPSIS
    Turns Mochi's autostart on and off, and another program's autostart with it.

.DESCRIPTION
    -Enable writes a HKCU Run value called Mochi that runs
    `mochic start --whkd` at login. A program that starts from a shortcut in
    the Startup folder is moved aside by -DisableStartupItem <name>, which
    renames <name>.lnk to <name>.lnk.disabled, and put back by
    -EnableStartupItem <name>. A HKCU Run value of the same name is backed up
    and removed the same way, and restored on the way back. Nothing else is
    touched.

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

.PARAMETER WhkdConfigHome
    Directory whose whkdrc whkd should load. Default %USERPROFILE%\.config\mochi
    when it exists, otherwise whkd's own default is used.

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
    [string] $WhkdConfigHome = (Join-Path $env:USERPROFILE '.config\mochi'),
    [switch] $ShowConsole
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$script:BackupKey = 'HKCU:\Software\Mochi'
$script:BackupPrefix = 'RunBackup_'
$script:MochiValue = 'Mochi'
$script:StartupDir = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Startup'

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
        return "`"$MochicPath`" start --whkd"
    }

    $prelude = ''
    if (Test-Path $WhkdConfigHome) {
        $prelude = "`$env:WHKD_CONFIG_HOME='$WhkdConfigHome'; "
    }
    $inner = "$prelude" + "Start-Process -FilePath '$MochicPath' -ArgumentList 'start','--whkd' -WindowStyle Hidden"
    return "powershell.exe -NoProfile -WindowStyle Hidden -Command `"$inner`""
}

function Enable-MochiAutostart {
    if (-not (Test-Path $MochicPath)) {
        Write-Detail "$MochicPath does not exist yet, run scripts\install.ps1 first"
    }
    $command = Get-MochiCommand
    $current = Get-RunValue -Name $script:MochiValue
    if ($current -eq $command) {
        Write-Detail 'already registered with the same command'
        return
    }
    Write-Detail "command: $command"
    if ($PSCmdlet.ShouldProcess("$script:RunKey\$script:MochiValue", 'set')) {
        New-ItemProperty -Path $script:RunKey -Name $script:MochiValue -Value $command -PropertyType String -Force | Out-Null
        Write-Detail 'registered'
    }
}

function Disable-MochiAutostart {
    if ($null -eq (Get-RunValue -Name $script:MochiValue)) {
        Write-Detail 'no Mochi Run value, nothing to do'
        return
    }
    if ($PSCmdlet.ShouldProcess("$script:RunKey\$script:MochiValue", 'remove')) {
        Remove-ItemProperty -Path $script:RunKey -Name $script:MochiValue
        Write-Detail 'removed'
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
        $backupValue = "$script:BackupPrefix$Name"
        if ($PSCmdlet.ShouldProcess("$script:RunKey\$Name", 'back up and remove')) {
            if (-not (Test-Path $script:BackupKey)) { New-Item -Path $script:BackupKey -Force | Out-Null }
            New-ItemProperty -Path $script:BackupKey -Name $backupValue -Value $run -PropertyType String -Force | Out-Null
            Remove-ItemProperty -Path $script:RunKey -Name $Name
            Write-Detail "Run value backed up to $script:BackupKey\$backupValue and removed"
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
        $backupValue = "$script:BackupPrefix$Name"
        if ($PSCmdlet.ShouldProcess("$script:RunKey\$Name", 'restore from backup')) {
            New-ItemProperty -Path $script:RunKey -Name $Name -Value $backup -PropertyType String -Force | Out-Null
            Remove-ItemProperty -Path $script:BackupKey -Name $backupValue
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
    } else {
        Write-Detail "$script:RunKey\$script:MochiValue"
        Write-Detail $mochi
    }

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
            if ($property.Name.StartsWith($script:BackupPrefix)) {
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
if ($DisableStartupItem) { Write-Step "disabling the $DisableStartupItem autostart"; Disable-OtherAutostart -Name $DisableStartupItem; $acted = $true }
if ($EnableStartupItem) { Write-Step "restoring the $EnableStartupItem autostart"; Enable-OtherAutostart -Name $EnableStartupItem; $acted = $true }
if ($Enable) { Write-Step 'enabling Mochi autostart'; Enable-MochiAutostart; $acted = $true }
if ($Disable) { Write-Step 'disabling Mochi autostart'; Disable-MochiAutostart; $acted = $true }

if (-not $acted) {
    Write-Output 'Autostart status, nothing changed.'
    Show-AutostartStatus
}

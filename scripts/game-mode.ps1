<#
.SYNOPSIS
    Game mode toggle for Mochi. Same key both ways.

.DESCRIPTION
    The usual game mode behaviour:

      ON  pauses tiling and restarts whkd with a minimal config in which only
          the toggle key survives, so a game gets every other key.
      OFF restarts whkd with the full config and resumes tiling.

    Bind it in whkdrc, for example:
        alt + shift + g : powershell -NoProfile -WindowStyle Hidden -File "<path>\game-mode.ps1"

    The minimal config is written on first use to <GameModeConfigHome>\whkdrc
    and contains nothing but the toggle binding.

.PARAMETER ConfigHome
    Directory holding the full whkdrc. Default %USERPROFILE%\.config\mochi.
    When it does not exist, WHKD_CONFIG_HOME is cleared instead and whkd falls
    back to %USERPROFILE%\.config\whkdrc.

.PARAMETER GameModeConfigHome
    Directory holding the minimal whkdrc.
    Default %USERPROFILE%\.config\mochi\gamemode.

.PARAMETER MochiBin
    Directory holding mochic.exe.

.PARAMETER WhkdPath
    whkd.exe to restart. Looked up on the PATH when not given. Without it and
    without whkd on the PATH only the tiling pause is toggled.
#>
[CmdletBinding()]
param(
    [string] $ConfigHome = (Join-Path $env:USERPROFILE '.config\mochi'),
    [string] $GameModeConfigHome = (Join-Path $env:USERPROFILE '.config\mochi\gamemode'),
    [string] $MochiBin = (Join-Path $env:LOCALAPPDATA 'Programs\Mochi\bin'),
    [string] $WhkdPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (Test-Path $MochiBin) { $env:Path = "$env:Path;$MochiBin" }

function Get-Paused {
    try {
        $answer = (& mochic query paused) 2>$null
        if ($LASTEXITCODE -eq 0 -and $answer) {
            $value = $answer | ConvertFrom-Json
            if ($value -is [string]) { return ($value -eq 'true') }
            return [bool]$value
        }
    } catch {
        Write-Verbose "mochic query paused failed: $($_.Exception.Message)"
    }

    $state = (& mochic state) | ConvertFrom-Json
    foreach ($name in @('is_paused', 'paused')) {
        if ($state.PSObject.Properties.Name -contains $name) { return [bool]$state.$name }
    }
    throw 'cannot tell whether Mochi is paused, is the daemon running?'
}

function Get-WhkdPath {
    if ($WhkdPath) { return $WhkdPath }
    $onPath = Get-Command whkd -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    return $null
}

function Initialize-GameModeConfig {
    $file = Join-Path $GameModeConfigHome 'whkdrc'
    if (Test-Path $file) { return }

    $self = $PSCommandPath
    $lines = @(
        '.shell cmd',
        '',
        '# GAME MODE ACTIVE',
        '# Every other hotkey is off so games get all the keys.',
        '# Only the toggle lives here, press it again to leave game mode.',
        "alt + shift + g         : powershell -NoProfile -WindowStyle Hidden -File `"$self`""
    )
    New-Item -ItemType Directory -Force -Path $GameModeConfigHome | Out-Null
    [System.IO.File]::WriteAllText($file, ($lines -join "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
    Write-Output "created $file"
}

$paused = Get-Paused
$desired = -not $paused

Initialize-GameModeConfig

$whkd = Get-WhkdPath
if ($whkd) {
    Get-Process whkd -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400

    if ($paused) {
        # leaving game mode: full hotkeys come back
        if (Test-Path $ConfigHome) {
            $env:WHKD_CONFIG_HOME = $ConfigHome
        } else {
            Remove-Item Env:WHKD_CONFIG_HOME -ErrorAction SilentlyContinue
        }
    } else {
        # entering game mode: toggle key only
        $env:WHKD_CONFIG_HOME = $GameModeConfigHome
    }

    Start-Process $whkd -WindowStyle Hidden
} else {
    Write-Output 'whkd.exe not found, only the tiling pause is toggled'
}

& mochic toggle-pause | Out-Null

# Confirm the pause state flipped, nudge once if the daemon missed it.
for ($i = 0; $i -lt 6; $i++) {
    Start-Sleep -Milliseconds 300
    if ((Get-Paused) -eq $desired) { break }
    if ($i -eq 3) { & mochic toggle-pause | Out-Null }
}

if ($desired) {
    Write-Output 'game mode: ON (tiling paused, hotkeys minimal)'
} else {
    Write-Output 'game mode: OFF (tiling and hotkeys restored)'
}

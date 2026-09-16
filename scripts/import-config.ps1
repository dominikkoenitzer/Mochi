<#
.SYNOPSIS
    Imports an existing tiling window manager setup into Mochi. Dry run by
    default.

.DESCRIPTION
    Mochi uses the config key names and command names that are common among
    tiling window managers on Windows, so importing a working setup is
    mechanical:

      1. The JSON configuration given with -Config is copied to mochi.json with
         the $schema line rewritten. Everything else stays as it is, including
         app_specific_configuration_path: the daemon reads the usual
         applications.json format.
      2. The whkdrc given with -Hotkeys is copied to <WhkdConfigHome>\whkdrc
         with every standalone call of -Command replaced by `mochic`. whkd only
         ever loads a file called whkdrc from $env:WHKD_CONFIG_HOME, so that is
         the copy it reads. The original file is never touched.

    Without -Apply nothing is written, the script only prints what would change.

.PARAMETER Config
    The JSON configuration in use today. Required, there is no default.

.PARAMETER Hotkeys
    The whkdrc in use today. Required, there is no default.

.PARAMETER Command
    Name of the command line program the whkdrc calls today, without the .exe.
    Every standalone occurrence of it becomes `mochic`. Required.

.PARAMETER MochiConfig
    Target configuration. Default %USERPROFILE%\mochi.json.

.PARAMETER WhkdConfigHome
    Directory that gets the imported hotkeys as a loadable whkdrc.
    Default %USERPROFILE%\.config\mochi.

.PARAMETER SchemaUrl
    Value for the $schema key in mochi.json. A local path works too, for
    example the file produced by `mochic schema > mochi.schema.json`.

.PARAMETER GameModeScript
    When given, hotkey lines pointing at another game-mode.ps1 are repointed at
    this path. Without it those lines are reported and left alone.

.PARAMETER Apply
    Write the files. Without it the script is a dry run.

.PARAMETER Force
    Overwrite target files that already exist.

.EXAMPLE
    .\scripts\import-config.ps1 -Config $env:USERPROFILE\wm.json -Hotkeys $env:USERPROFILE\.config\whkdrc -Command wmc

.EXAMPLE
    .\scripts\import-config.ps1 -Config $env:USERPROFILE\wm.json -Hotkeys $env:USERPROFILE\.config\whkdrc -Command wmc -Apply
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $Config,
    [Parameter(Mandatory)][string] $Hotkeys,
    [Parameter(Mandatory)][string] $Command,
    [string] $MochiConfig = (Join-Path $env:USERPROFILE 'mochi.json'),
    [string] $WhkdConfigHome = (Join-Path $env:USERPROFILE '.config\mochi'),
    [string] $SchemaUrl = 'https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json',
    [string] $GameModeScript,
    [switch] $Apply,
    [switch] $Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Config keys Mochi reads. Anything else in the source file is carried over
# unchanged but is reported so nothing silently stops working.
$script:KnownKeys = @(
    '$schema', 'app_specific_configuration_path', 'window_hiding_behaviour',
    'cross_monitor_move_behaviour', 'mouse_follows_focus', 'focus_follows_mouse',
    'default_workspace_padding', 'default_container_padding', 'border', 'border_width',
    'border_offset', 'border_style', 'transparency', 'transparency_alpha',
    'transparency_ignore_rules', 'border_colours', 'animation', 'stackbar',
    'ignore_rules', 'manage_rules', 'floating_applications', 'monitors',
    'work_area_offset', 'unmanaged_window_operation_behaviour',
    'monitor_index_preferences', 'display_index_preferences'
)

$script:Changes = 0

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output ''
    Write-Output "==> $Message"
}

function Write-Detail {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "    $Message"
}

function Get-Newline {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Text)
    if ($Text -match "`r`n") { return "`r`n" }
    return "`n"
}

function Split-Lines {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Text)
    return @($Text -split "`r?`n")
}

function Show-LineDiff {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]] $Before,
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]] $After
    )
    $count = [Math]::Max($Before.Count, $After.Count)
    $shown = 0
    for ($i = 0; $i -lt $count; $i++) {
        $old = if ($i -lt $Before.Count) { $Before[$i] } else { '' }
        $new = if ($i -lt $After.Count) { $After[$i] } else { '' }
        if ($old -ne $new) {
            Write-Output "    - $old"
            Write-Output "    + $new"
            $shown++
        }
    }
    if ($shown -eq 0) { Write-Detail 'no lines changed' }
}

function Write-Target {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][AllowEmptyString()][string] $Content
    )

    if ((Test-Path $Path) -and -not $Force) {
        Write-Detail "$Path exists, not overwriting it (use -Force)"
        return
    }
    if (-not $Apply) {
        Write-Detail "dry run, would write $Path"
        return
    }

    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    [System.IO.File]::WriteAllText($Path, $Content, (New-Object System.Text.UTF8Encoding($false)))
    Write-Detail "wrote $Path"
    $script:Changes++
}

function Convert-Config {
    if (-not (Test-Path $Config)) {
        Write-Detail "$Config not found, skipping the configuration"
        return
    }

    $raw = Get-Content -LiteralPath $Config -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        Write-Detail "$Config is empty, skipping the configuration"
        return
    }
    $newline = Get-Newline -Text $raw
    $before = Split-Lines -Text $raw

    $pattern = '("\$schema"\s*:\s*)"[^"]*"'
    if ($raw -match $pattern) {
        $text = $raw -replace $pattern, ('$1"' + $SchemaUrl + '"')
    } else {
        Write-Detail 'no $schema key in the source, nothing to rewrite'
        $text = $raw
    }

    $after = Split-Lines -Text $text
    Write-Detail "$Config -> $MochiConfig"
    Show-LineDiff -Before $before -After $after

    $json = $raw | ConvertFrom-Json
    if ($null -eq $json) {
        Write-Detail "$Config holds no JSON object, skipping the configuration"
        return
    }
    $keys = @($json.PSObject.Properties.Name)
    $unknown = @($keys | Where-Object { $script:KnownKeys -notcontains $_ })
    if ($unknown.Count -gt 0) {
        Write-Detail "carried over but not read by Mochi yet: $($unknown -join ', ')"
    }

    if ($keys -contains 'app_specific_configuration_path') {
        $asc = $json.app_specific_configuration_path
        Write-Detail "app_specific_configuration_path kept: $asc"
        $expanded = $asc -replace '\$Env:USERPROFILE', $env:USERPROFILE -replace '/', '\'
        if (Test-Path $expanded) {
            Write-Detail "   the file is there, the applications.json format is read as is"
        } else {
            Write-Detail "   the file is missing, drop the key or fix the path"
        }
    }

    Write-Target -Path $MochiConfig -Content ($after -join $newline)
}

function Convert-Hotkeys {
    if (-not (Test-Path $Hotkeys)) {
        Write-Detail "$Hotkeys not found, skipping the hotkeys"
        return
    }

    $raw = Get-Content -LiteralPath $Hotkeys -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        Write-Detail "$Hotkeys is empty, skipping the hotkeys"
        return
    }
    $newline = Get-Newline -Text $raw
    $before = Split-Lines -Text $raw

    # Standalone token only, so a name like <command>-no-console.exe and paths
    # that merely contain the old name are left for the human to look at.
    $name = [regex]::Escape($Command)
    $token = "(?<![\w.-])$name(?![\w-])"
    $hits = ([regex]::Matches($raw, $token)).Count
    $text = [regex]::Replace($raw, $token, 'mochic')

    if ($GameModeScript) {
        $found = [regex]::Matches($text, '(?i)[A-Za-z]:\\[^"]*\\game-mode\.ps1')
        foreach ($hit in $found) {
            if ($hit.Value -ine $GameModeScript) { $text = $text.Replace($hit.Value, $GameModeScript) }
        }
    }

    $after = Split-Lines -Text $text
    $loadable = Join-Path $WhkdConfigHome 'whkdrc'

    Write-Detail "$Hotkeys -> $loadable"
    Write-Detail "$hits $Command call(s) become mochic"
    Show-LineDiff -Before $before -After $after

    $leftovers = @($after | Where-Object { $_ -match "(?i)$name" -or $_ -match '(?i)game-mode\.ps1' })
    if ($leftovers.Count -gt 0) {
        Write-Detail 'lines that still point at the old setup:'
        foreach ($line in $leftovers) { Write-Output "      $($line.Trim())" }
        if (-not $GameModeScript) {
            Write-Detail 'repoint a game mode hotkey with -GameModeScript <path to scripts\game-mode.ps1>'
        }
    }

    Write-Target -Path $loadable -Content ($after -join $newline)
}

function Show-SwitchCommands {
    $loadable = Join-Path $WhkdConfigHome 'whkdrc'
    Write-Output ''
    Write-Output 'Switch to Mochi:'
    Write-Output '    stop your current window manager and its hotkey daemon, then run'
    Write-Output "    `$env:WHKD_CONFIG_HOME = '$WhkdConfigHome'; mochic start --whkd"
    Write-Output ''
    Write-Output 'Go back:'
    Write-Output '    mochic stop --whkd'
    Write-Output '    Remove-Item Env:WHKD_CONFIG_HOME -ErrorAction SilentlyContinue'
    Write-Output '    then start your previous window manager and its hotkey daemon again'
    Write-Output ''
    Write-Output "whkd loads $loadable while WHKD_CONFIG_HOME points at $WhkdConfigHome,"
    Write-Output "and $Hotkeys again once the variable is gone."
    Write-Output 'For the same switch at login use scripts\autostart.ps1.'
}

Write-Output 'Mochi config import'
Write-Output ("mode: " + $(if ($Apply) { 'apply' } else { 'dry run, nothing is written' }))

Write-Step 'configuration'
Convert-Config

Write-Step 'hotkeys'
Convert-Hotkeys

Write-Step 'switch over'
Show-SwitchCommands

if (-not $Apply) {
    Write-Output ''
    Write-Output 'Run it again with -Apply to write the files.'
} else {
    Write-Output ''
    Write-Output "$script:Changes file(s) written."
}

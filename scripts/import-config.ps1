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
      2. The hotkey file given with -Hotkeys is copied to <HotkeyDir>\hotkeys
         with every standalone call of -Command replaced by `mochic`, and every
         line that ran a game mode script replaced by `mochic toggle-game-mode`,
         which is what game mode is now. Mochi binds that file itself and reads
         the syntax the old file already uses, see docs\hotkeys.md. The original
         file is never touched.

    Without -Apply nothing is written, the script only prints what would change.

.PARAMETER Config
    The JSON configuration in use today. Required, there is no default.

.PARAMETER Hotkeys
    The hotkey file in use today. Required, there is no default.

.PARAMETER Command
    Name of the command line program the hotkey file calls today, without the
    .exe. Every standalone occurrence of it becomes `mochic`. Required.

.PARAMETER MochiConfig
    Target configuration. Default %USERPROFILE%\mochi.json.

.PARAMETER HotkeyDir
    Directory the imported hotkeys are written to, as a file called hotkeys.
    Default %USERPROFILE%\.config\mochi, where Mochi looks for it.

.PARAMETER SchemaUrl
    Value for the $schema key in mochi.json. A local path works too, for
    example the file produced by `mochic schema > mochi.schema.json`.

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
    [string] $HotkeyDir = (Join-Path $env:USERPROFILE '.config\mochi'),
    [string] $SchemaUrl = 'https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json',
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

function Resolve-FullPath {
    param([Parameter(Mandatory)][string] $Path)
    try {
        $text = $Path
        if (-not [System.IO.Path]::IsPathRooted($text)) {
            $text = Join-Path (Get-Location -PSProvider FileSystem).ProviderPath $text
        }
        return [System.IO.Path]::GetFullPath($text)
    } catch {
        return $Path
    }
}

function Write-Target {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][AllowEmptyString()][string] $Content,
        [string] $Source
    )

    # -HotkeyDir can point at the directory the source file already lives in,
    # and then the target is the source. This script promises the original is
    # never touched, so it is not written over, not even with -Force.
    if ($Source -and ((Resolve-FullPath -Path $Path) -ieq (Resolve-FullPath -Path $Source))) {
        Write-Detail "$Path is the source file itself, refusing to write it"
        Write-Detail 'point -HotkeyDir somewhere else, the original is left as it is'
        return
    }

    if ((Test-Path $Path) -and -not $Force) {
        Write-Detail "$Path exists, not overwriting it (use -Force)"
        return
    }
    if (-not $Apply) {
        Write-Detail "dry run, would write $Path"
        return
    }

    $dir = Split-Path -Parent (Resolve-FullPath -Path $Path)
    if ($dir -and -not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    # Resolved first. .NET resolves a relative path against the process
    # working directory, which is not PowerShell's location, so `-MochiConfig
    # out.json` reported "wrote out.json" and put it somewhere the user was not
    # looking. Split-Path above uses PowerShell's location, so the two also
    # disagreed about which directory to create.
    $full = Resolve-FullPath -Path $Path
    [System.IO.File]::WriteAllText($full, $Content, (New-Object System.Text.UTF8Encoding($false)))
    Write-Detail "wrote $full"
    $script:Changes++
}

function Convert-Config {
    if (-not (Test-Path $Config)) {
        Write-Detail "$Config not found, skipping the configuration"
        return
    }

    # -Encoding UTF8 is load bearing. Windows PowerShell 5.1, which is what
    # `powershell.exe` still is on Windows 11, reads a file with no byte order
    # mark as ANSI. The file is written back as UTF-8 further down, so an
    # umlaut in a path or a rule went in as one encoding and came out as
    # another, silently, with no error anywhere.
    $raw = Get-Content -LiteralPath $Config -Raw -Encoding UTF8
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

    # Advisory only: this parse decides what gets said about the keys, it does
    # not decide what gets written. Windows PowerShell 5.1 tolerates neither a
    # comment nor a trailing comma in ConvertFrom-Json, and every tiling window
    # manager config in the wild has one or the other, so under
    # $ErrorActionPreference = 'Stop' the run would end here: after the diff
    # was printed and before the hotkeys were written. A warning is the right
    # size for it.
    $json = $null
    try {
        $json = $raw | ConvertFrom-Json
    } catch {
        $reason = @($_.Exception.Message -split "`r?`n")[0]
        Write-Detail "could not read $Config as JSON: $reason"
        Write-Detail 'the key check is skipped, the file itself is copied unchanged'
    }

    if ($json -is [System.Management.Automation.PSCustomObject]) {
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
    } elseif ($null -ne $json) {
        Write-Detail "$Config holds no JSON object, the key check is skipped"
    }

    Write-Target -Path $MochiConfig -Content ($after -join $newline)
}

function Convert-Hotkeys {
    if (-not (Test-Path $Hotkeys)) {
        Write-Detail "$Hotkeys not found, skipping the hotkeys"
        return
    }

    $raw = Get-Content -LiteralPath $Hotkeys -Raw -Encoding UTF8
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

    # Game mode used to be a script that restarted the hotkey daemon with a
    # cut down file. Mochi has the whole thing as one command, so the line
    # keeps its keys and loses everything else.
    $lines = Split-Lines -Text $text
    $gameModeLines = 0
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match '^\s*#') { continue }
        $parts = $line -split ':', 2
        if ($parts.Count -ne 2) { continue }
        if ($parts[1] -notmatch '(?i)game[-_ ]?mode[^\\/]*\.ps1') { continue }
        $lines[$i] = "$($parts[0]): mochic toggle-game-mode"
        $gameModeLines++
    }

    $after = $lines
    $target = Join-Path $HotkeyDir 'hotkeys'

    Write-Detail "$Hotkeys -> $target"
    Write-Detail "$hits $Command call(s) become mochic"
    if ($gameModeLines -gt 0) {
        Write-Detail "$gameModeLines game mode line(s) become mochic toggle-game-mode"
    }
    Show-LineDiff -Before $before -After $after

    $leftovers = @($after | Where-Object { $_ -match "(?i)$name" })
    if ($leftovers.Count -gt 0) {
        Write-Detail 'lines that still point at the old setup:'
        foreach ($line in $leftovers) { Write-Output "      $($line.Trim())" }
    }

    Write-Target -Path $target -Content ($after -join $newline) -Source $Hotkeys
}

function Show-SwitchCommands {
    $target = Join-Path $HotkeyDir 'hotkeys'
    Write-Output ''
    Write-Output 'Switch to Mochi:'
    Write-Output '    stop your current window manager and its hotkey daemon, then run'
    Write-Output '    mochic start'
    Write-Output ''
    Write-Output 'Go back:'
    Write-Output '    mochic stop'
    Write-Output '    then start your previous window manager and its hotkey daemon again'
    Write-Output ''
    Write-Output "Mochi binds $target itself, and rebinds it every time the file is saved."
    Write-Output "$Hotkeys is left as it is, so the old setup finds it where it always was."
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

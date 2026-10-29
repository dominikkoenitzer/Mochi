<#
.SYNOPSIS
    Installs Mochi for the current user. No admin rights needed.

.DESCRIPTION
    Without a checkout, for example run as
        irm https://get-mochi.vercel.app/install.ps1 | iex
    it downloads the latest release from GitHub and verifies the SHA256 file
    that ships with it. From a checkout it builds the release binaries with
    cargo instead. Either way mochi.exe and mochic.exe end up in
    %LOCALAPPDATA%\Programs\Mochi\bin.

    The install directory is added to the user PATH when it is missing, and
    `mochic quickstart` runs when there is no %USERPROFILE%\mochi.json or no
    hotkey file in %USERPROFILE%\.config\mochi yet. It writes whichever of the
    two is missing and never overwrites one that is there.

    Then it registers the HKCU Run value Mochi that scripts\autostart.ps1
    -Enable writes, adds Mochi to the Start menu and starts it. A shell running
    as administrator starts it through Explorer, so Mochi itself never runs
    elevated.

    The script is idempotent: running it twice leaves the same result. It only
    ever touches its own files, nothing that another program owns.

    Switches need the script as a script block when it comes from the web:
        & ([scriptblock]::Create((irm <url>))) -Uninstall

.PARAMETER Version
    Release tag to download, for example v0.1.27, or `latest` for the newest
    release. Without it the repository is built from source with cargo, or,
    when the script was downloaded on its own and has no checkout around it,
    the latest release is installed.

.PARAMETER InstallRoot
    Install directory. The binaries land in <InstallRoot>\bin.

.PARAMETER SkipPath
    Do not touch the user PATH.

.PARAMETER SkipQuickstart
    Do not create a default mochi.json or hotkey file.

.PARAMETER SkipAutostart
    Do not register the Run value that starts Mochi at login.

.PARAMETER SkipStart
    Do not start Mochi at the end.

.PARAMETER Uninstall
    Stop Mochi, remove its Run value, the MochiWatchdog task, the Start menu
    shortcut, the binaries, the <InstallRoot>\bin directory Mochi created, and
    the PATH entry unless -SkipPath is given. <InstallRoot> itself is left
    alone, it is a directory you named and it may be older than Mochi. In the
    default folder the copy of Mochi-Setup.exe and its Apps & features entry
    go too.

.EXAMPLE
    .\scripts\install.ps1

.EXAMPLE
    .\scripts\install.ps1 -Version v0.1.27

.EXAMPLE
    .\scripts\install.ps1 -Version latest

.EXAMPLE
    .\scripts\install.ps1 -Uninstall
#>

# Everything runs inside this script block. `irm ... | iex` runs a script in
# the caller's own scope with no $PSCmdlet, so a top level param block would
# overwrite the caller's variables of the same name, ShouldProcess would fail,
# and strict mode and Stop would stay behind in the user's shell. A script
# block invoked with & gets a scope of its own; @args hands it the switches
# when the script is run as a file.
& {
[CmdletBinding(SupportsShouldProcess)]
param(
    [string] $Version,
    [string] $InstallRoot = (Join-Path $env:LOCALAPPDATA 'Programs\Mochi'),
    [switch] $SkipPath,
    [switch] $SkipQuickstart,
    [switch] $SkipAutostart,
    [switch] $SkipStart,
    [switch] $Uninstall
)

# Every download goes through this address: /install.ps1 is this script,
# /latest names the newest release, /download/<tag>/<file> is one of its
# files.
$BaseUrl = 'https://get-mochi.vercel.app'

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 redraws the progress bar for every chunk a download
# receives, which makes a few megabytes take minutes.
$ProgressPreference = 'SilentlyContinue'

$BinDir = Join-Path $InstallRoot 'bin'
$Binaries = @('mochi.exe', 'mochic.exe')
$ConfigPath = Join-Path $env:USERPROFILE 'mochi.json'
$HotkeyDir = Join-Path $env:USERPROFILE '.config\mochi'
$HotkeyPath = Join-Path $HotkeyDir 'hotkeys'
$LegacyHotkeyPath = Join-Path $HotkeyDir 'whkdrc'
$LogPath = Join-Path $env:LOCALAPPDATA 'mochi\mochi.log'
$EnvKey = 'HKCU:\Environment'
$EnvValueName = 'Path'
# What the acquisition step fetched, filled in by the functions below. A
# table rather than separate variables, because a function can change an
# entry of a table it can see but cannot assign a variable of this scope.
$acquired = @{ SourceDir = $null; TempWork = $null }

# Mochi reads either name from that directory, so a summary that always
# printed the first one would point at a file that is not there.
function Get-HotkeyPath {
    if ((-not (Test-Path -LiteralPath $HotkeyPath)) -and (Test-Path -LiteralPath $LegacyHotkeyPath)) {
        return $LegacyHotkeyPath
    }
    return $HotkeyPath
}

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "==> $Message"
}

function Write-Detail {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "    $Message"
}

# The user PATH is read and written through the registry on purpose.
# [Environment]::GetEnvironmentVariable('Path', 'User') expands %VAR% before it
# hands the value over, and SetEnvironmentVariable always writes REG_SZ, so a
# round trip through the pair bakes every %VAR% into a literal path and turns a
# REG_EXPAND_SZ PATH into a plain one. Both are silent and neither is ours to
# do. Reading raw and writing back with the kind the value already had leaves
# everything we did not come for exactly as it was.
function Get-UserPathRaw {
    $key = Get-Item -LiteralPath $EnvKey -ErrorAction SilentlyContinue
    if ($null -eq $key) { return '' }
    $raw = $key.GetValue($EnvValueName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if ($null -eq $raw) { return '' }
    return [string] $raw
}

# REG_EXPAND_SZ is what Windows itself writes, so it is also the kind to create
# the value with when there is none yet.
function Get-UserPathKind {
    $key = Get-Item -LiteralPath $EnvKey -ErrorAction SilentlyContinue
    if ($null -eq $key) { return 'ExpandString' }
    if (@($key.GetValueNames()) -notcontains $EnvValueName) { return 'ExpandString' }
    $kind = [string] $key.GetValueKind($EnvValueName)
    if ($kind -eq 'ExpandString') { return 'ExpandString' }
    return 'String'
}

# SetEnvironmentVariable broadcasts WM_SETTINGCHANGE for us; a registry write
# does not, so Explorer and every shell started after it would keep the old
# PATH until the next logon unless we send it ourselves.
function Publish-EnvironmentChange {
    try {
        if (-not ([System.Management.Automation.PSTypeName]'Mochi.NativeMethods').Type) {
            Add-Type -Namespace 'Mochi' -Name 'NativeMethods' -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out System.UIntPtr lpdwResult);
'@
        }
        $result = [UIntPtr]::Zero
        # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG, 5 s
        [void] [Mochi.NativeMethods]::SendMessageTimeout([IntPtr] 0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 0x0002, 5000, [ref] $result)
    } catch {
        Write-Detail "could not broadcast the PATH change ($($_.Exception.Message)), a new logon picks it up"
    }
}

function Set-UserPathRaw {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string] $Value,
        [Parameter(Mandatory)][string] $Kind
    )
    New-ItemProperty -Path $EnvKey -Name $EnvValueName -Value $Value -PropertyType $Kind -Force | Out-Null
    Publish-EnvironmentChange
}

# An empty entry is a real entry: it means the current directory. Dropping it
# would change what the PATH does, so the split keeps every field.
function Get-UserPathEntries {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Raw)
    # The leading comma keeps an empty result an empty array instead of
    # letting the pipeline unroll it into nothing.
    if ($Raw.Length -eq 0) { return , @() }
    return , @($Raw -split ';')
}

# Comparison only. The expanded, unquoted, slash trimmed form is what decides
# whether an entry is ours; the raw text is what gets written back.
function ConvertTo-ComparablePath {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Path)
    $text = $Path.Trim().Trim('"')
    if ($text.Length -eq 0) { return '' }
    try { $text = [Environment]::ExpandEnvironmentVariables($text) } catch { }
    return $text.TrimEnd('\')
}

function Test-PathEntry {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]] $Entries,
        [Parameter(Mandatory)][string] $Directory
    )
    $wanted = ConvertTo-ComparablePath -Path $Directory
    foreach ($entry in $Entries) {
        if ((ConvertTo-ComparablePath -Path $entry) -ieq $wanted) { return $true }
    }
    return $false
}

function Update-UserPath {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string] $Directory,
        [switch] $Remove
    )

    $raw = Get-UserPathRaw
    $kind = Get-UserPathKind
    $entries = Get-UserPathEntries -Raw $raw
    $wanted = ConvertTo-ComparablePath -Path $Directory

    if ($Remove) {
        $kept = @($entries | Where-Object { (ConvertTo-ComparablePath -Path $_) -ine $wanted })
        if ($kept.Count -eq $entries.Count) {
            Write-Detail 'user PATH is already clean'
            return
        }
        if ($PSCmdlet.ShouldProcess('user PATH', "remove $Directory")) {
            Set-UserPathRaw -Value ($kept -join ';') -Kind $kind
            Write-Detail "removed from the user PATH, the rest of the value is unchanged"
        }
        return
    }

    if (Test-PathEntry -Entries $entries -Directory $Directory) {
        Write-Detail 'already on the user PATH'
        return
    }

    # Append the one entry and leave the rest of the value as it stands, rather
    # than splitting and rejoining a value that belongs to the user.
    if ($raw.Length -eq 0) {
        $updated = $Directory
    } elseif ($raw.EndsWith(';')) {
        $updated = "$raw$Directory"
    } else {
        $updated = "$raw;$Directory"
    }
    # Said out loud rather than discovered later. Writing through the registry
    # rather than `setx` already avoids the 1024 character truncation that is
    # the worst version of this, but a user PATH past about 2047 characters is
    # still silently cut short by the legacy System Properties dialog and by
    # some older tools, and the symptom is programs that stop being found for
    # no visible reason. Mochi is not going to be the one that pushed it over
    # without a word.
    if ($updated.Length -gt 2047) {
        Write-Detail "warning: the user PATH would become $($updated.Length) characters"
        Write-Detail '  values past ~2047 are truncated by the old System Properties dialog'
        Write-Detail '  add it yourself, or use -SkipPath and the full path to mochic.exe'
    }
    if ($PSCmdlet.ShouldProcess('user PATH', "add $Directory")) {
        Set-UserPathRaw -Value $updated -Kind $kind
        Write-Detail 'added to the user PATH, open a new shell to pick it up'
    }
}

# The checkout this script sits in, or $null. Run through `irm | iex` there is
# no script file at all and $PSScriptRoot is empty, which Split-Path refuses.
function Find-RepoRoot {
    if ([string]::IsNullOrEmpty($PSScriptRoot)) { return $null }
    $root = Split-Path -Parent $PSScriptRoot
    if (-not (Test-Path -LiteralPath (Join-Path $root 'Cargo.toml'))) { return $null }
    return $root
}

function Get-RepoRoot {
    $root = Find-RepoRoot
    if (-not $root) {
        throw "no Cargo.toml above the script, run this from a checkout or pass -Version"
    }
    return $root
}

# The acquisition functions print progress, so they hand the result over in
# $acquired.SourceDir instead of returning it.
function Invoke-CargoBuild {
    $root = Get-RepoRoot
    Write-Step "building release binaries in $root"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo is not on the PATH, install Rust from https://rustup.rs'
    }
    Push-Location $root
    try {
        & cargo build --release --workspace
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    $acquired.SourceDir = Join-Path $root 'target\release'
}

# `latest` is resolved to the newest published release tag here, so every
# later step works with a real tag and the download names stay exact.
function Resolve-ReleaseTag {
    param([Parameter(Mandatory)][string] $Tag)

    if ($Tag -ne 'latest') { return $Tag }
    try {
        $release = Invoke-RestMethod -Uri "$BaseUrl/latest" -UseBasicParsing
        return $release.tag_name
    } catch {
        throw "could not find the latest Mochi release ($($_.Exception.Message)). Pass a tag such as -Version v0.1.27."
    }
}

function Save-ReleaseAsset {
    param([Parameter(Mandatory)][string] $Tag)

    $Tag = Resolve-ReleaseTag -Tag $Tag
    Write-Detail "release $Tag"
    if (-not $Tag.StartsWith('v')) { $Tag = "v$Tag" }
    # Only x64 is released. Windows on ARM can run the x64 build under
    # emulation, and a window manager is close to the worst thing to emulate:
    # it is all Win32 hooks and per-frame window calls, on the hot path of
    # everything the user does. Saying so and pointing at a source build is
    # more honest than installing something that will be slow in a way nobody
    # can diagnose, and far better than a 404 on an asset that was never
    # built. Cross-building it needs the ARM64 MSVC tools, which is why there
    # is no asset yet rather than an oversight.
    if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw ("Mochi has no release build for $env:PROCESSOR_ARCHITECTURE yet. " +
               'Build it from source instead: run this script with no -Version.')
    }
    # Releases from now on carry a zip whose name does not change between
    # versions, so that a releases/latest/download link always works. Older
    # releases carry the version in the name; -Version on one of those still
    # finds it.
    $names = @('Mochi-x86_64-pc-windows-msvc', "Mochi-$Tag-x86_64-pc-windows-msvc")
    $base = "$BaseUrl/download/$Tag"
    $work = Join-Path $env:TEMP "mochi-install-$Tag"

    Write-Step "downloading Mochi $Tag"

    # A download is megabytes on disk and a request to GitHub, so it is an
    # action, not a preparation: -WhatIf and a declined -Confirm stop here
    # rather than after the bytes have already landed in %TEMP%.
    if (-not $PSCmdlet.ShouldProcess("Mochi $Tag", 'download and extract')) {
        Write-Detail 'skipped, nothing was downloaded'
        $acquired.SourceDir = $null
        return
    }

    New-Item -ItemType Directory -Force -Path $work | Out-Null

    $zip = $null
    $sum = $null
    $haveSum = $false
    $failures = @()
    foreach ($name in $names) {
        $candidate = Join-Path $work "$name.zip"
        $candidateSum = "$candidate.sha256"
        try {
            Invoke-WebRequest -Uri "$base/$name.zip" -OutFile $candidate -UseBasicParsing
        } catch {
            $failures += "$name.zip: $($_.Exception.Message)"
            continue
        }
        Write-Detail "$name.zip"
        $zip = $candidate
        $sum = $candidateSum
        try {
            Invoke-WebRequest -Uri "$base/$name.zip.sha256" -OutFile $sum -UseBasicParsing
            $haveSum = $true
        } catch {
            $haveSum = $false
        }
        break
    }
    if (-not $zip) {
        throw ("could not download Mochi $Tag. Check the tag exists, or build from source by leaving -Version off.`n" +
               ($failures -join "`n"))
    }
    if (-not $haveSum) {
        Write-Detail 'no SHA256 file published for this release, skipping the checksum'
    }

    if ($haveSum) {
        $expected = ((Get-Content $sum -Raw).Trim() -split '\s+')[0]
        $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash
        if ($expected -ine $actual) {
            throw "checksum mismatch: expected $expected, got $actual"
        }
        Write-Detail 'checksum ok'
    }

    $extract = Join-Path $work 'extract'
    if (Test-Path -LiteralPath $extract) { Remove-Item -LiteralPath $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    $acquired.SourceDir = $extract
    $acquired.TempWork = $work
}

# The archive and what came out of it are ours and are of no use once the
# binaries are copied, so a real run does not leave them behind in %TEMP%.
function Remove-TempWork {
    if (-not $acquired.TempWork) { return }
    if (Test-Path -LiteralPath $acquired.TempWork) {
        try {
            Remove-Item -LiteralPath $acquired.TempWork -Recurse -Force
            Write-Detail "cleaned up $($acquired.TempWork)"
        } catch {
            Write-Detail "could not clean up $($acquired.TempWork) ($($_.Exception.Message))"
        }
    }
    $acquired.TempWork = $null
}

# Where an install went, so that an uninstall can find it.
#
# -InstallRoot is a parameter with a default, so -Uninstall on its own always
# recomputed the DEFAULT location. A user who installed to D:\Apps\Mochi and
# later ran -Uninstall got "mochi.exe was not there", "user PATH is already
# clean" and a tidy summary, having removed nothing at all, with the binaries
# and the PATH entry still live.
$StateKey = 'HKCU:\Software\Mochi'

function Save-InstallRoot {
    param([Parameter(Mandatory)] [string] $Root)
    try {
        if (-not (Test-Path $StateKey)) {
            New-Item -Path $StateKey -Force | Out-Null
        }
        New-ItemProperty -Path $StateKey -Name 'InstallRoot' -Value $Root `
            -PropertyType String -Force | Out-Null
    } catch {
        # Not fatal: the install itself worked, only the uninstall hint is lost.
        Write-Detail "could not record the install location: $($_.Exception.Message)"
    }
}

function Get-RecordedInstallRoot {
    try {
        $value = (Get-ItemProperty -Path $StateKey -Name 'InstallRoot' -ErrorAction Stop).InstallRoot
        if ([string]::IsNullOrWhiteSpace($value)) { return $null }
        return $value
    } catch {
        return $null
    }
}

function Clear-InstallRoot {
    try {
        Remove-ItemProperty -Path $StateKey -Name 'InstallRoot' -ErrorAction Stop
        $key = Get-Item -Path $StateKey -ErrorAction Stop
        if ($key.ValueCount -eq 0 -and $key.SubKeyCount -eq 0) {
            Remove-Item -Path $StateKey -Force
        }
    } catch {
        # Nothing recorded, or already gone.
    }
}

$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$RunValue = 'Mochi'
$WatchdogTask = 'MochiWatchdog'

function Get-ShortcutPath {
    $programs = [Environment]::GetFolderPath('Programs')
    if ([string]::IsNullOrWhiteSpace($programs)) {
        $programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    }
    return (Join-Path $programs 'Mochi.lnk')
}

# The same command scripts\autostart.ps1 -Enable writes, so that script reports
# it as already registered and its -Disable removes it.
function Get-AutostartCommand {
    param([Parameter(Mandatory)][string] $Mochic)
    $quoted = $Mochic.Replace("'", "''")
    $inner = "Start-Process -FilePath '$quoted' -ArgumentList 'start' -WindowStyle Hidden"
    return "powershell.exe -NoProfile -WindowStyle Hidden -Command `"$inner`""
}

function Test-MochiCommand {
    param([AllowEmptyString()][string] $Value)
    return [regex]::IsMatch("$Value", '\bmochic(\.exe)?\b',
        [Text.RegularExpressions.RegexOptions]'IgnoreCase, CultureInvariant')
}

function Get-MochiRunValue {
    $item = Get-ItemProperty -Path $RunKey -Name $RunValue -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$RunValue
}

function Register-Autostart {
    param([Parameter(Mandatory)][string] $Mochic)
    $command = Get-AutostartCommand -Mochic $Mochic
    $current = Get-MochiRunValue
    if (($null -ne $current) -and -not (Test-MochiCommand -Value "$current")) {
        # Somebody else's value under the same name is not ours to replace.
        Write-Detail "$RunKey\$RunValue belongs to something else, left alone"
        Write-Detail 'scripts\autostart.ps1 -Enable backs it up and registers Mochi'
        return
    }
    if ("$current" -eq $command) {
        Write-Detail 'already registered'
        return
    }
    if ($PSCmdlet.ShouldProcess("$RunKey\$RunValue", 'set')) {
        New-ItemProperty -Path $RunKey -Name $RunValue -Value $command -PropertyType String -Force | Out-Null
        Write-Detail "registered $RunKey\$RunValue"
    }
}

function Unregister-Autostart {
    $current = Get-MochiRunValue
    if ($null -eq $current) {
        Write-Detail 'no Run value'
    } elseif (-not (Test-MochiCommand -Value "$current")) {
        Write-Detail "$RunKey\$RunValue does not run mochic, left alone"
    } elseif ($PSCmdlet.ShouldProcess("$RunKey\$RunValue", 'remove')) {
        Remove-ItemProperty -Path $RunKey -Name $RunValue
        Write-Detail "removed $RunKey\$RunValue"
        # scripts\autostart.ps1 -Enable backs up a value it replaced under this
        # name. It goes back exactly as it was, under its own registry kind.
        $backup = "RunBackup_$RunValue"
        $saved = Get-ItemProperty -Path $StateKey -Name $backup -ErrorAction SilentlyContinue
        if ($null -ne $saved) {
            $kind = 'String'
            $recorded = Get-ItemProperty -Path $StateKey -Name "${backup}_Kind" -ErrorAction SilentlyContinue
            if (($null -ne $recorded) -and ($recorded."${backup}_Kind" -eq 'ExpandString')) { $kind = 'ExpandString' }
            New-ItemProperty -Path $RunKey -Name $RunValue -Value $saved.$backup -PropertyType $kind -Force | Out-Null
            Remove-ItemProperty -Path $StateKey -Name $backup
            Remove-ItemProperty -Path $StateKey -Name "${backup}_Kind" -ErrorAction SilentlyContinue
            Write-Detail 'the Run value that was there before Mochi is back'
        }
    }

    # The watchdog scripts\autostart.ps1 -Watchdog registers. Left behind, it
    # would try to start a deleted mochic every five minutes.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'SilentlyContinue'
    try {
        schtasks /query /tn $WatchdogTask 2>$null | Out-Null
        $haveTask = ($LASTEXITCODE -eq 0)
    } finally {
        $ErrorActionPreference = $previous
    }
    if ($haveTask -and $PSCmdlet.ShouldProcess($WatchdogTask, 'remove the scheduled task')) {
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $out = schtasks /delete /tn $WatchdogTask /f 2>&1
        } finally {
            $ErrorActionPreference = $previous
        }
        if ($LASTEXITCODE -eq 0) {
            Write-Detail "removed the $WatchdogTask task"
        } else {
            Write-Detail "could not remove the $WatchdogTask task: $out"
        }
    }
}

# conhost --headless gives mochic a console nobody sees. A shortcut that named
# mochic.exe or powershell.exe directly would open a terminal window on every
# click, because both are console programs.
function New-StartMenuShortcut {
    param([Parameter(Mandatory)][string] $Mochic)
    $path = Get-ShortcutPath
    if (-not $PSCmdlet.ShouldProcess($path, 'create shortcut')) { return }
    try {
        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($path)
        $link.TargetPath = Join-Path $env:SystemRoot 'System32\conhost.exe'
        $link.Arguments = "--headless `"$Mochic`" start"
        $link.WorkingDirectory = Split-Path -Parent $Mochic
        $link.IconLocation = "$(Join-Path (Split-Path -Parent $Mochic) 'mochi.exe'),0"
        $link.Description = 'Start Mochi'
        $link.Save()
        Write-Detail $path
    } catch {
        Write-Detail "could not create $path ($($_.Exception.Message))"
    }
}

function Remove-StartMenuShortcut {
    $path = Get-ShortcutPath
    if (-not (Test-Path -LiteralPath $path)) {
        Write-Detail 'no Start menu shortcut'
        return
    }
    if ($PSCmdlet.ShouldProcess($path, 'remove')) {
        Remove-Item -LiteralPath $path -Force
        Write-Detail "removed $path"
    }
}

# By the token: an elevated shell hands its token to every child it starts,
# and Mochi must not run elevated.
function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Write-WayOut {
    Write-Detail 'Pause            turns tiling off and on (alt + f12 without a Pause key)'
    Write-Detail 'alt + shift + e  stops Mochi and puts every window back'
}

function Start-Mochi {
    param([Parameter(Mandatory)][string] $Mochic)
    if (-not $PSCmdlet.ShouldProcess('Mochi', 'start')) { return }
    if (Test-Elevated) {
        # Explorer runs at the user's normal rights whatever this shell runs
        # at, so a program it opens does too.
        $shortcut = Get-ShortcutPath
        if (-not (Test-Path -LiteralPath $shortcut)) {
            Write-Detail 'this shell runs as administrator and there is no shortcut to start Mochi through'
            Write-Detail 'start it from a normal PowerShell with: mochic start'
            return
        }
        Write-Detail 'this shell runs as administrator, so Mochi is started through Explorer without it'
        Start-Process -FilePath (Join-Path $env:SystemRoot 'explorer.exe') -ArgumentList "`"$shortcut`""
        for ($i = 0; $i -lt 50; $i++) {
            if (Get-Process -Name mochi -ErrorAction SilentlyContinue) {
                Write-Detail 'Mochi is running'
                return
            }
            Start-Sleep -Milliseconds 200
        }
        Write-Detail 'Mochi did not come up within 10 seconds, start it from the Start menu'
        Write-Detail "if it still does not start, run mochic check and read $LogPath"
        return
    }
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $Mochic start 2>&1
    } finally {
        $ErrorActionPreference = $previous
    }
    foreach ($line in @($output)) {
        if ("$line".Trim()) { Write-Detail "$line" }
    }
    if ($LASTEXITCODE -ne 0) {
        Write-Detail "Mochi did not start, run mochic check and read $LogPath"
    }
}

# Asked to stop, not killed: a stop puts back every window Mochi was hiding.
function Stop-Mochi {
    $running = @(Get-Process -Name mochi -ErrorAction SilentlyContinue)
    if ($running.Count -eq 0) {
        Write-Detail 'Mochi is not running'
        return
    }
    if (-not $PSCmdlet.ShouldProcess('Mochi', 'stop')) { return }
    $mochic = Join-Path $BinDir 'mochic.exe'
    if (-not (Test-Path -LiteralPath $mochic)) {
        $found = Get-Command mochic -ErrorAction SilentlyContinue
        if ($found) { $mochic = $found.Source } else { $mochic = $null }
    }
    if (-not $mochic) {
        throw 'Mochi is running and there is no mochic to stop it with. Press alt + shift + e, then run this again.'
    }
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $mochic stop 2>&1 | Out-Null
    } finally {
        $ErrorActionPreference = $previous
    }
    for ($i = 0; $i -lt 50; $i++) {
        if (-not (Get-Process -Name mochi -ErrorAction SilentlyContinue)) {
            Write-Detail 'stopped, every window is back'
            return
        }
        Start-Sleep -Milliseconds 200
    }
    throw 'Mochi did not stop within 10 seconds. Press alt + shift + e, then run this again.'
}

# Answers the names it copied into $BinDir.
function Copy-Binaries {
    param([Parameter(Mandatory)][string] $Source)
    foreach ($binary in $Binaries) {
        if ($PSCmdlet.ShouldProcess($binary, "copy to $BinDir")) {
            Copy-Item -LiteralPath (Join-Path $Source $binary) -Destination $BinDir -Force
            $binary
        }
    }
}

# Only bin, and only when it is empty. <InstallRoot> is a directory the user
# named and may have had long before Mochi: -InstallRoot C:\Tools must not
# turn an uninstall into deleting C:\Tools.
function Remove-EmptyBinDir {
    $dir = $BinDir
    if ((Test-Path $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force)) {
        if ($PSCmdlet.ShouldProcess($dir, 'remove empty directory')) {
            Remove-Item -LiteralPath $dir -Force
        }
    }
}

function Install-Mochi {
    if ($Version) {
        Save-ReleaseAsset -Tag $Version
    } elseif (-not (Find-RepoRoot)) {
        # Downloaded on its own rather than run from a checkout: there is
        # nothing to build, so take the newest release instead of failing.
        Write-Detail 'no checkout around this script, installing the latest release'
        Save-ReleaseAsset -Tag 'latest'
    } else {
        Invoke-CargoBuild
    }
    $source = $acquired.SourceDir

    # Nothing was acquired, because the download was declined or dry run. There
    # is nothing to check for and nothing to copy, and pretending otherwise is
    # how a stale extract directory from an earlier run gets mistaken for this
    # one's binaries.
    if (-not $source) {
        Write-Step 'nothing was installed'
        Write-Detail 'the binaries were not fetched, so nothing was copied, verified or configured'
        return
    }

    foreach ($binary in $Binaries) {
        if (-not (Test-Path -LiteralPath (Join-Path $source $binary))) {
            throw "$binary not found in $source"
        }
    }

    Write-Step "installing to $BinDir"
    if (-not (Test-Path -LiteralPath $BinDir)) {
        if ($PSCmdlet.ShouldProcess($BinDir, 'create directory')) {
            New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
        }
    }
    # Whether the copies actually happened is the only thing that says the rest
    # of the run makes sense: -WhatIf is not the only way to answer no, a
    # declined -Confirm is the other one.
    # A running daemon holds its own image open for reading only, so the copy
    # below fails with a raw sharing violation. Since autostart exists, that is
    # the *normal* upgrade path, and the error it produced said nothing about
    # what to do. Refuse early and say it, rather than throwing halfway through
    # and leaving one new binary beside one old one.
    $running = @(Get-Process -Name mochi, mochic -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        $names = ($running | ForEach-Object { "$($_.ProcessName) (pid $($_.Id))" }) -join ', '
        throw "Mochi is running: $names. Stop it first with 'mochic stop', then run this again. Its files cannot be replaced while it is using them."
    }

    $placed = @(Copy-Binaries -Source $source)
    foreach ($binary in $placed) { Write-Detail $binary }
    $copied = $placed.Count -eq $Binaries.Count
    Write-Step 'checking that the installed binaries run'
    if (-not $copied) {
        Write-Detail 'skipped, the binaries were not copied'
    } else {
        foreach ($binary in $Binaries) {
            $path = Join-Path $BinDir $binary
            # Windows PowerShell 5.1 turns a native command writing to stderr
            # into a NativeCommandError, which $ErrorActionPreference = 'Stop'
            # makes terminating: one informational line from the binary would
            # abort the install with the copies already done. The redirect
            # still collects that output, it just must not throw.
            $previous = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                $output = & $path --version 2>&1
            } finally {
                $ErrorActionPreference = $previous
            }
            if ($LASTEXITCODE -ne 0) {
                throw "$path --version exited with $LASTEXITCODE, the install is not usable"
            }
            $line = @($output | Where-Object { "$_".Trim() }) | Select-Object -First 1
            if (-not $line) {
                throw "$path --version printed nothing, the install is not usable"
            }
            Write-Detail "$line"
        }
    }
    # Only now. Throwing the download away before the installed binaries had
    # proven they run left a broken install in place with nothing to retry
    # from, and autostart pointing straight at it.
    Remove-TempWork

    if ($SkipPath) {
        Write-Step 'user PATH left alone (-SkipPath)'
    } else {
        Write-Step 'user PATH'
        Update-UserPath -Directory $BinDir
    }
    if (Test-PathEntry -Entries (Get-UserPathEntries -Raw (Get-UserPathRaw)) -Directory $BinDir) {
        $env:Path = "$env:Path;$BinDir"
    }

    # Mochi reads hotkeys or whkdrc from that directory and quickstart leaves
    # either name alone, so either one means the hotkeys are there already.
    $haveHotkeys = (Test-Path -LiteralPath $HotkeyPath) -or (Test-Path -LiteralPath $LegacyHotkeyPath)

    if (-not $copied) {
        Write-Step 'configuration left alone'
        Write-Detail 'the binaries were not copied, so mochic quickstart was not run'
    } elseif ($SkipQuickstart) {
        Write-Step 'configuration left alone (-SkipQuickstart)'
    } elseif ((Test-Path -LiteralPath $ConfigPath) -and $haveHotkeys) {
        Write-Step "configuration is already there: $ConfigPath"
        Write-Detail "hotkeys are already there: $(Get-HotkeyPath)"
    } else {
        Write-Step 'creating a default configuration and hotkey file with mochic quickstart'
        if ($PSCmdlet.ShouldProcess("$ConfigPath and $HotkeyPath", 'mochic quickstart')) {
            try {
                & (Join-Path $BinDir 'mochic.exe') quickstart
                if ($LASTEXITCODE -ne 0) { throw "mochic quickstart exited with $LASTEXITCODE" }
            } catch {
                Write-Detail "quickstart failed: $($_.Exception.Message)"
                Write-Detail "write $ConfigPath by hand, see docs/configuration.md"
                Write-Detail "and $HotkeyPath, see docs/hotkeys.md"
            }
        }
    }

    # The summary reports what is on disk now, not what the happy path would
    # have written: a dry run and a quickstart that failed both end here too.
    if (-not $copied) {
        Write-Step 'nothing was installed'
        Write-Detail "binaries   would go to $BinDir"
        return
    }

    Save-InstallRoot -Root $InstallRoot

    $mochic = Join-Path $BinDir 'mochic.exe'
    if ($SkipAutostart) {
        Write-Step 'start at login left alone (-SkipAutostart)'
    } else {
        Write-Step 'start at login'
        Register-Autostart -Mochic $mochic
    }
    Write-Step 'Start menu shortcut'
    New-StartMenuShortcut -Mochic $mochic

    Write-Step 'done'
    Write-Detail "binaries   $BinDir"
    if (Test-Path -LiteralPath $ConfigPath) {
        Write-Detail "config     $ConfigPath"
    } else {
        Write-Detail "config     not written, see docs/configuration.md for $ConfigPath"
    }
    if ((Test-Path -LiteralPath $HotkeyPath) -or (Test-Path -LiteralPath $LegacyHotkeyPath)) {
        Write-Detail "hotkeys    $(Get-HotkeyPath)"
    } else {
        Write-Detail "hotkeys    not written, see docs/hotkeys.md for $HotkeyPath"
    }
    Write-Detail "log        $LogPath"
    Write-Detail "uninstall  & ([scriptblock]::Create((irm $BaseUrl/install.ps1))) -Uninstall"

    Write-Step 'the way out, before Mochi rearranges your windows'
    Write-WayOut
    if ($SkipStart) {
        Write-Step 'not started (-SkipStart), start it from the Start menu or with: mochic start'
    } else {
        Write-Step 'starting Mochi'
        Start-Mochi -Mochic $mochic
    }
}

function Uninstall-Mochi {
    # First, while mochic is still there to ask, and before the files go: a
    # running mochi.exe cannot be deleted.
    Write-Step 'stopping Mochi'
    Stop-Mochi

    Write-Step 'start at login'
    Unregister-Autostart

    Write-Step 'Start menu shortcut'
    Remove-StartMenuShortcut

    Write-Step "removing binaries from $BinDir"
    foreach ($binary in $Binaries) {
        $path = Join-Path $BinDir $binary
        if (Test-Path -LiteralPath $path) {
            if ($PSCmdlet.ShouldProcess($path, 'remove')) {
                Remove-Item -LiteralPath $path -Force
                Write-Detail "removed $binary"
            }
        } else {
            Write-Detail "$binary was not there"
        }
    }

    Remove-EmptyBinDir

    # What Mochi-Setup.exe adds on top: its own copy, which Apps & features
    # runs to uninstall, and that Apps & features entry. The setup always
    # installs to the default folder, so only an uninstall of that folder
    # touches them; a folder the user named may hold a Mochi-Setup.exe of
    # their own.
    $setupRoot = Join-Path $env:LOCALAPPDATA 'Programs\Mochi'
    if ($InstallRoot.TrimEnd('\') -ieq $setupRoot) {
        $setupCopy = Join-Path $setupRoot 'Mochi-Setup.exe'
        if ((Test-Path -LiteralPath $setupCopy) -and $PSCmdlet.ShouldProcess($setupCopy, 'remove')) {
            Remove-Item -LiteralPath $setupCopy -Force
            Write-Detail 'removed Mochi-Setup.exe'
        }
        $appsEntry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Mochi'
        if ((Test-Path -LiteralPath $appsEntry) -and $PSCmdlet.ShouldProcess($appsEntry, 'remove')) {
            Remove-Item -LiteralPath $appsEntry -Recurse -Force
            Write-Detail 'removed the Apps & features entry'
        }
    }

    if ($SkipPath) {
        Write-Step 'user PATH left alone (-SkipPath)'
    } else {
        Write-Step 'user PATH'
        Update-UserPath -Directory $BinDir -Remove
    }

    Write-Step 'left in place on purpose'
    Write-Detail "config     $ConfigPath"
    Write-Detail "hotkeys    $(Get-HotkeyPath)"
    Write-Detail "log        $LogPath"
}

if ($Uninstall) {
    # Only when the user did not say where: an explicit -InstallRoot always wins.
    if (-not $PSBoundParameters.ContainsKey('InstallRoot')) {
        $recorded = Get-RecordedInstallRoot
        if ($recorded -and $recorded -ne $InstallRoot) {
            Write-Step "using the recorded install location $recorded"
            $InstallRoot = $recorded
            $BinDir = Join-Path $InstallRoot 'bin'
        }
    }
    Uninstall-Mochi
    Clear-InstallRoot
} else {
    Install-Mochi
}
} @args

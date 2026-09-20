<#
.SYNOPSIS
    Installs Mochi for the current user. No admin rights needed.

.DESCRIPTION
    By default the script builds the release binaries from this repository and
    copies mochi.exe and mochic.exe to %LOCALAPPDATA%\Programs\Mochi\bin. With
    -Version it downloads that release from GitHub instead and verifies the
    SHA256 file that ships with it.

    The install directory is added to the user PATH when it is missing, and
    `mochic quickstart` runs when there is no %USERPROFILE%\mochi.json or no
    hotkey file in %USERPROFILE%\.config\mochi yet. It writes whichever of the
    two is missing and never overwrites one that is there.

    The script is idempotent: running it twice leaves the same result. It only
    ever touches its own files, nothing that another program owns.

.PARAMETER Version
    Release tag to download, for example v0.1.1. Without it the repository is
    built from source with cargo. While the repository is private the download
    needs the GitHub CLI, signed in as someone who can see it.

.PARAMETER Repo
    GitHub repository to download releases from.

.PARAMETER InstallRoot
    Install directory. The binaries land in <InstallRoot>\bin.

.PARAMETER SkipPath
    Do not touch the user PATH.

.PARAMETER SkipQuickstart
    Do not create a default mochi.json or hotkey file.

.PARAMETER Uninstall
    Remove the binaries, the <InstallRoot>\bin directory Mochi created, and the
    PATH entry unless -SkipPath is given. <InstallRoot> itself is left alone,
    it is a directory you named and it may be older than Mochi.

.EXAMPLE
    .\scripts\install.ps1

.EXAMPLE
    .\scripts\install.ps1 -Version v0.1.1

.EXAMPLE
    .\scripts\install.ps1 -Uninstall
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string] $Version,
    [string] $Repo = 'dominikkoenitzer/Mochi',
    [string] $InstallRoot = (Join-Path $env:LOCALAPPDATA 'Programs\Mochi'),
    [switch] $SkipPath,
    [switch] $SkipQuickstart,
    [switch] $Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:BinDir = Join-Path $InstallRoot 'bin'
$script:Binaries = @('mochi.exe', 'mochic.exe')
$script:SourceDir = $null
$script:ConfigPath = Join-Path $env:USERPROFILE 'mochi.json'
$script:HotkeyDir = Join-Path $env:USERPROFILE '.config\mochi'
$script:HotkeyPath = Join-Path $script:HotkeyDir 'hotkeys'
$script:LegacyHotkeyPath = Join-Path $script:HotkeyDir 'whkdrc'
$script:LogPath = Join-Path $env:LOCALAPPDATA 'mochi\mochi.log'
$script:EnvKey = 'HKCU:\Environment'
$script:EnvValueName = 'Path'
$script:TempWork = $null

# Mochi reads either name from that directory, so a summary that always
# printed the first one would point at a file that is not there.
function Get-HotkeyPath {
    if ((-not (Test-Path $script:HotkeyPath)) -and (Test-Path $script:LegacyHotkeyPath)) {
        return $script:LegacyHotkeyPath
    }
    return $script:HotkeyPath
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
    $key = Get-Item -LiteralPath $script:EnvKey -ErrorAction SilentlyContinue
    if ($null -eq $key) { return '' }
    $raw = $key.GetValue($script:EnvValueName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if ($null -eq $raw) { return '' }
    return [string] $raw
}

# REG_EXPAND_SZ is what Windows itself writes, so it is also the kind to create
# the value with when there is none yet.
function Get-UserPathKind {
    $key = Get-Item -LiteralPath $script:EnvKey -ErrorAction SilentlyContinue
    if ($null -eq $key) { return 'ExpandString' }
    if (@($key.GetValueNames()) -notcontains $script:EnvValueName) { return 'ExpandString' }
    $kind = [string] $key.GetValueKind($script:EnvValueName)
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
    New-ItemProperty -Path $script:EnvKey -Name $script:EnvValueName -Value $Value -PropertyType $Kind -Force | Out-Null
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
    if ($PSCmdlet.ShouldProcess('user PATH', "add $Directory")) {
        Set-UserPathRaw -Value $updated -Kind $kind
        Write-Detail 'added to the user PATH, open a new shell to pick it up'
    }
}

function Get-RepoRoot {
    $root = Split-Path -Parent $PSScriptRoot
    if (-not (Test-Path (Join-Path $root 'Cargo.toml'))) {
        throw "no Cargo.toml above $PSScriptRoot, run this from a checkout or pass -Version"
    }
    return $root
}

# The acquisition functions print progress, so they hand the result over in
# $script:SourceDir instead of returning it.
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
    $script:SourceDir = Join-Path $root 'target\release'
}

function Save-ReleaseAsset {
    param([Parameter(Mandatory)][string] $Tag)

    if (-not $Tag.StartsWith('v')) { $Tag = "v$Tag" }
    $name = "Mochi-$Tag-x86_64-pc-windows-msvc"
    $base = "https://github.com/$Repo/releases/download/$Tag"
    $work = Join-Path $env:TEMP "mochi-install-$Tag"
    $zip = Join-Path $work "$name.zip"
    $sum = "$zip.sha256"

    Write-Step "downloading $name.zip from $Repo"

    # A download is megabytes on disk and a request to GitHub, so it is an
    # action, not a preparation: -WhatIf and a declined -Confirm stop here
    # rather than after the bytes have already landed in %TEMP%.
    if (-not $PSCmdlet.ShouldProcess("$name.zip from $Repo", 'download and extract')) {
        Write-Detail 'skipped, nothing was downloaded'
        $script:SourceDir = $null
        return
    }

    New-Item -ItemType Directory -Force -Path $work | Out-Null

    # A release on a private repository is not there for an anonymous request:
    # GitHub answers 404 rather than 403, so the plain download looks like a
    # missing file. The GitHub CLI carries the credentials that make it visible,
    # so it goes first whenever it is installed and signed in.
    $gh = (Get-Command gh -ErrorAction SilentlyContinue)
    $haveSum = $true
    if ($gh) {
        Write-Detail "using $($gh.Source)"
        & $gh.Source release download $Tag --repo $Repo --pattern "$name.zip*" --dir $work --clobber
        if ($LASTEXITCODE -ne 0) {
            throw "gh release download failed with exit code $LASTEXITCODE. Is $Tag published, and are you signed in with ``gh auth login``?"
        }
        $haveSum = Test-Path $sum
        if (-not $haveSum) {
            Write-Detail 'no SHA256 file published for this release, skipping the checksum'
        }
    } else {
        try {
            Invoke-WebRequest -Uri "$base/$name.zip" -OutFile $zip -UseBasicParsing
        } catch {
            throw "could not download $name.zip from $Repo ($($_.Exception.Message)). A private repository needs the GitHub CLI: install it, run ``gh auth login`` and try again, or build from source by leaving -Version off."
        }

        try {
            Invoke-WebRequest -Uri "$base/$name.zip.sha256" -OutFile $sum -UseBasicParsing
        } catch {
            Write-Detail 'no SHA256 file published for this release, skipping the checksum'
            $haveSum = $false
        }
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
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    $script:SourceDir = $extract
    $script:TempWork = $work
}

# The archive and what came out of it are ours and are of no use once the
# binaries are copied, so a real run does not leave them behind in %TEMP%.
function Remove-TempWork {
    if (-not $script:TempWork) { return }
    if (Test-Path $script:TempWork) {
        try {
            Remove-Item -LiteralPath $script:TempWork -Recurse -Force
            Write-Detail "cleaned up $script:TempWork"
        } catch {
            Write-Detail "could not clean up $script:TempWork ($($_.Exception.Message))"
        }
    }
    $script:TempWork = $null
}

function Install-Mochi {
    if ($Version) {
        Save-ReleaseAsset -Tag $Version
    } else {
        Invoke-CargoBuild
    }
    $source = $script:SourceDir

    # Nothing was acquired, because the download was declined or dry run. There
    # is nothing to check for and nothing to copy, and pretending otherwise is
    # how a stale extract directory from an earlier run gets mistaken for this
    # one's binaries.
    if (-not $source) {
        Write-Step 'nothing was installed'
        Write-Detail 'the binaries were not fetched, so nothing was copied, verified or configured'
        return
    }

    foreach ($binary in $script:Binaries) {
        if (-not (Test-Path (Join-Path $source $binary))) {
            throw "$binary not found in $source"
        }
    }

    Write-Step "installing to $script:BinDir"
    if (-not (Test-Path $script:BinDir)) {
        if ($PSCmdlet.ShouldProcess($script:BinDir, 'create directory')) {
            New-Item -ItemType Directory -Force -Path $script:BinDir | Out-Null
        }
    }
    # Whether the copies actually happened is the only thing that says the rest
    # of the run makes sense: -WhatIf is not the only way to answer no, a
    # declined -Confirm is the other one.
    $copied = $true
    foreach ($binary in $script:Binaries) {
        if ($PSCmdlet.ShouldProcess($binary, "copy to $script:BinDir")) {
            Copy-Item -Path (Join-Path $source $binary) -Destination $script:BinDir -Force
            Write-Detail $binary
        } else {
            $copied = $false
        }
    }
    Remove-TempWork

    Write-Step 'checking that the installed binaries run'
    if (-not $copied) {
        Write-Detail 'skipped, the binaries were not copied'
    } else {
        foreach ($binary in $script:Binaries) {
            $path = Join-Path $script:BinDir $binary
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

    if ($SkipPath) {
        Write-Step 'user PATH left alone (-SkipPath)'
    } else {
        Write-Step 'user PATH'
        Update-UserPath -Directory $script:BinDir
    }
    if (Test-PathEntry -Entries (Get-UserPathEntries -Raw (Get-UserPathRaw)) -Directory $script:BinDir) {
        $env:Path = "$env:Path;$script:BinDir"
    }

    # Mochi reads hotkeys or whkdrc from that directory and quickstart leaves
    # either name alone, so either one means the hotkeys are there already.
    $haveHotkeys = (Test-Path $script:HotkeyPath) -or (Test-Path $script:LegacyHotkeyPath)

    if (-not $copied) {
        Write-Step 'configuration left alone'
        Write-Detail 'the binaries were not copied, so mochic quickstart was not run'
    } elseif ($SkipQuickstart) {
        Write-Step 'configuration left alone (-SkipQuickstart)'
    } elseif ((Test-Path $script:ConfigPath) -and $haveHotkeys) {
        Write-Step "configuration is already there: $script:ConfigPath"
        Write-Detail "hotkeys are already there: $(Get-HotkeyPath)"
    } else {
        Write-Step 'creating a default configuration and hotkey file with mochic quickstart'
        if ($PSCmdlet.ShouldProcess("$script:ConfigPath and $script:HotkeyPath", 'mochic quickstart')) {
            try {
                & (Join-Path $script:BinDir 'mochic.exe') quickstart
                if ($LASTEXITCODE -ne 0) { throw "mochic quickstart exited with $LASTEXITCODE" }
            } catch {
                Write-Detail "quickstart failed: $($_.Exception.Message)"
                Write-Detail "write $script:ConfigPath by hand, see docs/configuration.md"
                Write-Detail "and $script:HotkeyPath, see docs/hotkeys.md"
            }
        }
    }

    # The summary reports what is on disk now, not what the happy path would
    # have written: a dry run and a quickstart that failed both end here too.
    if (-not $copied) {
        Write-Step 'nothing was installed'
        Write-Detail "binaries   would go to $script:BinDir"
        Write-Detail 'autostart  scripts\autostart.ps1 -Enable, after a real run'
        return
    }

    Write-Step 'done'
    Write-Detail "binaries   $script:BinDir"
    if (Test-Path $script:ConfigPath) {
        Write-Detail "config     $script:ConfigPath"
    } else {
        Write-Detail "config     not written, see docs/configuration.md for $script:ConfigPath"
    }
    if ((Test-Path $script:HotkeyPath) -or (Test-Path $script:LegacyHotkeyPath)) {
        Write-Detail "hotkeys    $(Get-HotkeyPath)"
    } else {
        Write-Detail "hotkeys    not written, see docs/hotkeys.md for $script:HotkeyPath"
    }
    Write-Detail "log        $script:LogPath"
    Write-Detail 'autostart  scripts\autostart.ps1 -Enable'
}

function Uninstall-Mochi {
    Write-Step "removing binaries from $script:BinDir"
    foreach ($binary in $script:Binaries) {
        $path = Join-Path $script:BinDir $binary
        if (Test-Path $path) {
            if ($PSCmdlet.ShouldProcess($path, 'remove')) {
                Remove-Item $path -Force
                Write-Detail "removed $binary"
            }
        } else {
            Write-Detail "$binary was not there"
        }
    }

    # Only bin, and only when the loop above left it empty. <InstallRoot> is a
    # directory the user named and may have had long before Mochi: -InstallRoot
    # C:\Tools must not turn an uninstall into deleting C:\Tools.
    $dir = $script:BinDir
    if ((Test-Path $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force)) {
        if ($PSCmdlet.ShouldProcess($dir, 'remove empty directory')) {
            Remove-Item $dir -Force
        }
    }

    if ($SkipPath) {
        Write-Step 'user PATH left alone (-SkipPath)'
    } else {
        Write-Step 'user PATH'
        Update-UserPath -Directory $script:BinDir -Remove
    }

    Write-Step 'left in place on purpose'
    Write-Detail "config     $script:ConfigPath"
    Write-Detail "hotkeys    $(Get-HotkeyPath)"
    Write-Detail "log        $script:LogPath"
    Write-Detail 'autostart  remove it with scripts\autostart.ps1 -Disable'
}

if ($Uninstall) {
    Uninstall-Mochi
} else {
    Install-Mochi
}

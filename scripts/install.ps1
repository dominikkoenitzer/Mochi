<#
.SYNOPSIS
    Installs Mochi for the current user. No admin rights needed.

.DESCRIPTION
    By default the script builds the release binaries from this repository and
    copies mochi.exe and mochic.exe to %LOCALAPPDATA%\Programs\Mochi\bin. With
    -Version it downloads that release from GitHub instead and verifies the
    SHA256 file that ships with it.

    The install directory is added to the user PATH when it is missing, and
    `mochic quickstart` runs when there is no %USERPROFILE%\mochi.json yet.

    The script is idempotent: running it twice leaves the same result. It only
    ever touches its own files, nothing that another program owns.

.PARAMETER Version
    Release tag to download, for example v0.1.0. Without it the repository is
    built from source with cargo.

.PARAMETER Repo
    GitHub repository to download releases from.

.PARAMETER InstallRoot
    Install directory. The binaries land in <InstallRoot>\bin.

.PARAMETER SkipPath
    Do not touch the user PATH.

.PARAMETER SkipQuickstart
    Do not create a default mochi.json.

.PARAMETER Uninstall
    Remove the binaries, the install directory and the PATH entry.

.EXAMPLE
    .\scripts\install.ps1

.EXAMPLE
    .\scripts\install.ps1 -Version v0.1.0

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
$script:LogPath = Join-Path $env:LOCALAPPDATA 'mochi\mochi.log'

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "==> $Message"
}

function Write-Detail {
    param([Parameter(Mandatory)][string] $Message)
    Write-Output "    $Message"
}

function Get-UserPathEntries {
    $raw = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($raw)) { return @() }
    return @($raw -split ';' | Where-Object { $_ -ne '' })
}

function Test-PathEntry {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]] $Entries,
        [Parameter(Mandatory)][string] $Directory
    )
    $wanted = $Directory.TrimEnd('\')
    foreach ($entry in $Entries) {
        if ($entry.TrimEnd('\') -ieq $wanted) { return $true }
    }
    return $false
}

function Update-UserPath {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string] $Directory,
        [switch] $Remove
    )

    $entries = Get-UserPathEntries
    $present = Test-PathEntry -Entries $entries -Directory $Directory
    $wanted = $Directory.TrimEnd('\')

    if ($Remove) {
        if (-not $present) {
            Write-Detail 'user PATH is already clean'
            return
        }
        $kept = @($entries | Where-Object { $_.TrimEnd('\') -ine $wanted })
        if ($PSCmdlet.ShouldProcess('user PATH', "remove $Directory")) {
            [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
            Write-Detail 'removed from the user PATH'
        }
        return
    }

    if ($present) {
        Write-Detail 'already on the user PATH'
        return
    }
    $updated = @($entries) + $Directory
    if ($PSCmdlet.ShouldProcess('user PATH', "add $Directory")) {
        [Environment]::SetEnvironmentVariable('Path', ($updated -join ';'), 'User')
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
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    Invoke-WebRequest -Uri "$base/$name.zip" -OutFile $zip -UseBasicParsing

    $haveSum = $true
    try {
        Invoke-WebRequest -Uri "$base/$name.zip.sha256" -OutFile $sum -UseBasicParsing
    } catch {
        Write-Detail 'no SHA256 file published for this release, skipping the checksum'
        $haveSum = $false
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
}

function Install-Mochi {
    if ($Version) {
        Save-ReleaseAsset -Tag $Version
    } else {
        Invoke-CargoBuild
    }
    $source = $script:SourceDir

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
    foreach ($binary in $script:Binaries) {
        if ($PSCmdlet.ShouldProcess($binary, "copy to $script:BinDir")) {
            Copy-Item -Path (Join-Path $source $binary) -Destination $script:BinDir -Force
            Write-Detail $binary
        }
    }

    Write-Step 'checking that the installed binaries run'
    if ($WhatIfPreference) {
        Write-Detail 'skipped, nothing was copied under -WhatIf'
    } else {
        foreach ($binary in $script:Binaries) {
            $path = Join-Path $script:BinDir $binary
            $output = & $path --version 2>&1
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
    if (Test-PathEntry -Entries (Get-UserPathEntries) -Directory $script:BinDir) {
        $env:Path = "$env:Path;$script:BinDir"
    }

    if ($SkipQuickstart) {
        Write-Step 'configuration left alone (-SkipQuickstart)'
    } elseif (Test-Path $script:ConfigPath) {
        Write-Step "configuration is already there: $script:ConfigPath"
    } else {
        Write-Step 'creating a default configuration with mochic quickstart'
        if ($PSCmdlet.ShouldProcess($script:ConfigPath, 'mochic quickstart')) {
            try {
                & (Join-Path $script:BinDir 'mochic.exe') quickstart
                if ($LASTEXITCODE -ne 0) { throw "mochic quickstart exited with $LASTEXITCODE" }
            } catch {
                Write-Detail "quickstart failed: $($_.Exception.Message)"
                Write-Detail "write $script:ConfigPath by hand, see docs/configuration.md"
            }
        }
    }

    Write-Step 'done'
    Write-Detail "binaries   $script:BinDir"
    Write-Detail "config     $script:ConfigPath"
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

    foreach ($dir in @($script:BinDir, $InstallRoot)) {
        if ((Test-Path $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force)) {
            if ($PSCmdlet.ShouldProcess($dir, 'remove empty directory')) {
                Remove-Item $dir -Force
            }
        }
    }

    Write-Step 'user PATH'
    Update-UserPath -Directory $script:BinDir -Remove

    Write-Step 'left in place on purpose'
    Write-Detail "config     $script:ConfigPath"
    Write-Detail "log        $script:LogPath"
    Write-Detail 'autostart  remove it with scripts\autostart.ps1 -Disable'
}

if ($Uninstall) {
    Uninstall-Mochi
} else {
    Install-Mochi
}

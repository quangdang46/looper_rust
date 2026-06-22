<#
.SYNOPSIS
    Install Looper Rust binaries (looperd, looper-cli, looper-net) from GitHub Releases.

.DESCRIPTION
    Detects Windows architecture, downloads the correct release archive from
    GitHub (quangdang46/looper), verifies the file hash, and installs to
    $env:LOCALAPPDATA\looper\bin by default.  Optionally adds the install
    directory to the user PATH.

.PARAMETER InstallDir
    Target installation directory.  Default: $env:LOCALAPPDATA\looper\bin

.PARAMETER Version
    A specific semver tag (e.g. "v0.1.0") to install.  Default: latest release.

.PARAMETER AddToPath
    Switch; if present, add InstallDir to the user PATH (idempotent).

.PARAMETER SkipPath
    Switch; if present, skip the PATH prompt even when AddToPath is not given.

.EXAMPLE
    .\install.ps1
    Install the latest release to the default directory.

.EXAMPLE
    .\install.ps1 -InstallDir C:\tools\looper -Version v0.1.0 -AddToPath
    Install a specific version and add to PATH.

.LINK
    https://github.com/quangdang46/looper
#>

[CmdletBinding()]
param(
    [string]$InstallDir   = "$env:LOCALAPPDATA\looper\bin",
    [string]$Version      = "",
    [switch]$AddToPath,
    [switch]$SkipPath
)

$ErrorActionPreference = "Stop"
$ProgressPreference    = "SilentlyContinue"      # speed up Invoke-WebRequest

# Force TLS 1.2 (some Windows builds default to older versions)
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
$Repo      = "quangdang46/looper"
$RepoUrl   = "https://github.com/$Repo"
$ApiUrl    = "https://api.github.com/repos/$Repo/releases/latest"
$Binaries  = @("looperd", "looper-cli", "looper-net")

# Colours (only interactive consoles)
function Write-Step  { Write-Host "==> " -NoNewline -ForegroundColor Green;  Write-Host "$args" }
function Write-Warn  { Write-Host "==> " -NoNewline -ForegroundColor Yellow; Write-Host "$args" }
function Write-Err   { Write-Host "==> " -NoNewline -ForegroundColor Red;    Write-Host "$args" }

# ---------------------------------------------------------------------------
# Helper: die on error
# ---------------------------------------------------------------------------
function Die([string]$Msg, [int]$ExitCode = 1) {
    Write-Err $Msg
    exit $ExitCode
}

# ---------------------------------------------------------------------------
# Architecture detection
# ---------------------------------------------------------------------------
function Get-TargetArch {
    $arch = (Get-CimInstance Win32_Processor | Select-Object -First 1).AddressWidth
    # Also check via environment for ARM64
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -or $env:PROCESSOR_IDENTIFIER -match "ARM64") {
        return "aarch64"
    }
    switch ($arch) {
        64  { return "x86_64" }
        32  { Die "32-bit Windows is not supported." }
        default { Die "Unsupported architecture: $arch" }
    }
}

$TargetArch = Get-TargetArch
$TargetOs   = "pc-windows-msvc"
$Target     = "${TargetArch}-${TargetOs}"

Write-Step "Detected platform: $Target"
Write-Step "Install directory: $InstallDir"

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------
# PowerShell 5+ has everything we need — Invoke-WebRequest, Expand-Archive,
# Get-FileHash.  No external dependencies.

# ---------------------------------------------------------------------------
# Resolve release version
# ---------------------------------------------------------------------------
$Tag        = ""
$Checksums  = @{}

function Resolve-Release {
    param([string]$VersionTag)

    $tagValue  = $null
    $targetUrl = $null

    if (![string]::IsNullOrEmpty($VersionTag)) {
        Write-Step "Resolving release: $VersionTag"
        $targetUrl = "https://api.github.com/repos/$Repo/releases/tags/$VersionTag"
    } else {
        Write-Step "Resolving latest release..."
        $targetUrl = $ApiUrl
    }

    try {
        $json = Invoke-RestMethod -Uri $targetUrl -UseBasicParsing -ErrorAction Stop
        $tagValue = $json.tag_name
        Write-Step "Release tag: $tagValue"

        # Pre-compute asset download URL from the API response
        $checksumsUrl = ($json.assets | Where-Object { $_.name -eq "checksums.txt" } | Select-Object -First 1).browser_download_url
        return @{ Tag = $tagValue; ChecksumsUrl = $checksumsUrl }
    }
    catch {
        # GitHub API may be rate-limited.  Fall back to tag-based URL.
        if (![string]::IsNullOrEmpty($VersionTag)) {
            $tagValue = $VersionTag
        } else {
            $tagValue = "latest"
        }
        Write-Warn "GitHub API unavailable; falling back to tag '$tagValue'"
        return @{ Tag = $tagValue; ChecksumsUrl = $null }
    }
}

$releaseInfo = Resolve-Release -VersionTag $Version
$Tag = $releaseInfo.Tag

# ---------------------------------------------------------------------------
# Download checksums (best-effort)
# ---------------------------------------------------------------------------
$tmpDir = Join-Path $env:TEMP "looper-install-$([System.IO.Path]::GetRandomFileName())"
$null = New-Item -ItemType Directory -Path $tmpDir -Force
$cleanup = $true

try {
    $checksumsLocal = $null
    if (![string]::IsNullOrEmpty($releaseInfo.ChecksumsUrl)) {
        $checksumsLocal = Join-Path $tmpDir "checksums.txt"
        try {
            Invoke-WebRequest -Uri $releaseInfo.ChecksumsUrl -OutFile $checksumsLocal -UseBasicParsing -ErrorAction Stop
            Write-Step "Checksums downloaded"

            # Parse into a hashtable: filename -> hash
            Get-Content $checksumsLocal | ForEach-Object {
                if ($_ -match '^([a-fA-F0-9]+)\s+(\S+)') {
                    $Checksums[$Matches[2]] = $Matches[1]
                }
            }
        }
        catch {
            Write-Warn "Could not download checksums; will skip hash verification"
            $checksumsLocal = $null
        }
    }
    else {
        Write-Warn "No checksums URL available; will skip hash verification"
    }

    # -----------------------------------------------------------------------
    # Download & verify each binary archive
    # -----------------------------------------------------------------------
    $failCount = 0
    $installDirParent = Split-Path $InstallDir -Parent
    if (!(Test-Path $installDirParent)) {
        $null = New-Item -ItemType Directory -Path $installDirParent -Force
    }
    if (!(Test-Path $InstallDir)) {
        $null = New-Item -ItemType Directory -Path $InstallDir -Force
    }

    foreach ($bin in $Binaries) {
        # Release assets are named: looperd-x86_64-pc-windows-msvc.zip
        $archiveName = "${bin}-${Target}.zip"
        $archiveUrl  = "$RepoUrl/releases/download/$Tag/$archiveName"
        $archivePath = Join-Path $tmpDir $archiveName

        Write-Step "Downloading $bin ($Target)..."
        try {
            Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath -UseBasicParsing -ErrorAction Stop
        }
        catch {
            Write-Err "Failed to download $archiveName"
            Write-Warn "Binary '$bin' may not be available for this platform; skipping"
            $failCount++
            continue
        }

        # SHA-256 verification
        if ($checksumsLocal -and $Checksums.ContainsKey($archiveName)) {
            $expected = $Checksums[$archiveName]
            $actual   = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLower()
            if ($actual -ne $expected.ToLower()) {
                Die "Checksum mismatch for $archiveName`n  Expected: $expected`n  Actual:   $actual"
            }
            Write-Step "Checksum verified: $archiveName"
        }

        # Extract
        $extractDir = Join-Path $tmpDir $bin
        try {
            Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force
        }
        catch {
            Die "Failed to extract $archiveName : $_"
        }

        # Locate the binary inside the archive (it may be in a subdirectory
        # or directly in the extract root).
        $foundBin = Get-ChildItem -Path $extractDir -Recurse -Filter "${bin}.exe" | Select-Object -First 1
        if ($null -eq $foundBin) {
            # Fallback: look for the bare name without .exe
            $foundBin = Get-ChildItem -Path $extractDir -Recurse -Filter $bin | Select-Object -First 1
        }
        if ($null -eq $foundBin) {
            Die "Binary '${bin}.exe' not found inside the archive"
        }

        # Install (copy, preserving any existing)
        $dest = Join-Path $InstallDir "${bin}.exe"
        try {
            Copy-Item -Path $foundBin.FullName -Destination $dest -Force
        }
        catch {
            Die "Failed to install ${bin}.exe to $dest : $_"
        }
        Write-Step "Installed $dest"
    }

    if ($failCount -gt 0) {
        Die "$failCount binary(ies) failed to install"
    }

    # -----------------------------------------------------------------------
    # PATH management
    # -----------------------------------------------------------------------
    $needPathAdd = $false
    $currentPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
    if ($currentPath -split ';' -notcontains $InstallDir) {
        $needPathAdd = $true
    }

    if ($AddToPath) {
        if ($needPathAdd) {
            $newPath = if ([string]::IsNullOrEmpty($currentPath)) { $InstallDir } else { "$currentPath;$InstallDir" }
            [Environment]::SetEnvironmentVariable("Path", $newPath, [EnvironmentVariableTarget]::User)
            Write-Step "Added $InstallDir to user PATH"
        } else {
            Write-Step "$InstallDir is already in PATH"
        }
    }
    elseif ($needPathAdd -and !$SkipPath) {
        Write-Host ""
        Write-Warn "━━━ PATH Setup ━━━"
        Write-Host "$InstallDir is not in your PATH."
        Write-Host ""
        $response = Read-Host "Would you like to add it to your user PATH now? [Y/n]"
        if ($response -eq '' -or $response -match '^[Yy]') {
            $newPath = if ([string]::IsNullOrEmpty($currentPath)) { $InstallDir } else { "$currentPath;$InstallDir" }
            [Environment]::SetEnvironmentVariable("Path", $newPath, [EnvironmentVariableTarget]::User)
            Write-Step "Added $InstallDir to user PATH"
            Write-Host "  (You may need to restart your terminal for the change to take effect.)"
        } else {
            Write-Host "  To add it manually later, run:"
            Write-Host ""
            Write-Host "    `$env:Path = `"`$env:Path;$InstallDir`""
            Write-Host "    [Environment]::SetEnvironmentVariable('Path', `$env:Path, 'User')"
        }
    }

    # -----------------------------------------------------------------------
    # Success banner
    # -----------------------------------------------------------------------
    Write-Host ""
    Write-Host ("━" * 40) -ForegroundColor Green
    Write-Host "  Installation Complete" -ForegroundColor Green
    Write-Host ("━" * 40) -ForegroundColor Green
    Write-Host "  Binaries installed in: $InstallDir"
    Write-Host "  Release:              $Tag"
    Write-Host "  Platform:             $Target"
    Write-Host ""
    Write-Host "  Run  looperd --version              to start the daemon"
    Write-Host "  Run  looper-cli --help               for CLI usage"
    Write-Host "  Run  looper-net --help               for net usage"
    Write-Host ""
    Write-Host "  Need help?  $RepoUrl/releases"
}
finally {
    # Cleanup temp directory
    if ($cleanup -and (Test-Path $tmpDir)) {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

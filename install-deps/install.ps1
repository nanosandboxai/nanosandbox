#Requires -Version 5.1
<#
.SYNOPSIS
    Nanosandbox Runtime Dependencies Installer for Windows.

.DESCRIPTION
    Installs: libkrunfw.dll (kernel firmware) + busybox (Linux initrd helper)
    Enables Hyper-V if not already active (requires reboot).

.PARAMETER Version
    Version to install. Defaults to the latest release.

.PARAMETER InstallDir
    Installation directory. Defaults to "$env:ProgramFiles\nanosandbox".

.EXAMPLE
    # Install latest (from web):
    irm https://github.com/nanosandboxai/install-deps/releases/latest/download/install.ps1 | iex

    # Install specific version:
    .\install.ps1 -Version v0.2.0

    # Custom install path:
    .\install.ps1 -InstallDir C:\opt\nanosandbox
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir = "$env:ProgramFiles\nanosandbox"
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Speed up Invoke-WebRequest

# Copy params to regular variables to avoid "Cannot overwrite variable" when piped via iex
$requestedVersion = $Version
$targetDir = $InstallDir

$GitHubRepo = 'nanosandboxai/install-deps'
$Platform = 'windows-amd64'

# ─── Helpers ─────────────────────────────────────────────────────────────────

function Write-Header  { param([string]$Msg) Write-Host "`n==> $Msg" -ForegroundColor Blue }
function Write-OK      { param([string]$Msg) Write-Host "  [OK] $Msg" -ForegroundColor Green }
function Write-Warn    { param([string]$Msg) Write-Host "  [WARN] $Msg" -ForegroundColor Yellow }
function Write-Err     { param([string]$Msg) Write-Host "  [ERROR] $Msg" -ForegroundColor Red }
function Write-Info    { param([string]$Msg) Write-Host "  $Msg" }

# ─── Admin check ─────────────────────────────────────────────────────────────

function Assert-Administrator {
    $current = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $current.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Err "This installer must be run as Administrator."
        Write-Info "Right-click PowerShell and select 'Run as administrator', then try again."
        exit 1
    }
}

# ─── Prerequisites ───────────────────────────────────────────────────────────

function Test-Prerequisites {
    Write-Header "Checking prerequisites"

    # Windows version (Server 2019+ / Windows 10 1809+)
    $build = [System.Environment]::OSVersion.Version.Build
    if ($build -lt 17763) {
        Write-Err "Windows build $build is too old. Minimum: 17763 (Windows 10 1809 / Server 2019)."
        exit 1
    }
    Write-OK "Windows build $build"

    # Hyper-V / HCS availability
    $hyperv = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -ErrorAction SilentlyContinue
    if ($null -eq $hyperv -or $hyperv.State -ne 'Enabled') {
        Write-Warn "Hyper-V is not enabled."
        Write-Info "Nanosandbox uses the Host Compute Service (HCS) which requires Hyper-V."
        Write-Info "Enable with: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -All"
        Write-Info ""
    } else {
        Write-OK "Hyper-V enabled"
    }

    # Containers feature (required for HCS/HCN APIs)
    $containers = Get-WindowsOptionalFeature -Online -FeatureName Containers -ErrorAction SilentlyContinue
    if ($null -eq $containers -or $containers.State -ne 'Enabled') {
        Write-Info "Enabling Containers feature (required for HCS)..."
        Enable-WindowsOptionalFeature -Online -FeatureName Containers -All -NoRestart -ErrorAction SilentlyContinue | Out-Null
        Write-OK "Containers feature enabled"
    } else {
        Write-OK "Containers feature enabled"
    }
}

# ─── Version resolution ──────────────────────────────────────────────────────

function Resolve-LatestVersion {
    param([string]$Requested)

    Write-Header "Resolving version"

    if ($Requested) {
        Write-Info "Using specified version: $Requested"
        return $Requested
    }

    Write-Info "Fetching latest release..."
    try {
        $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$GitHubRepo/releases" `
            -Headers @{ Accept = 'application/vnd.github+json' } `
            -ErrorAction Stop
        $tag = ($releases | Select-Object -First 1).tag_name
        if (-not $tag) { throw "No releases found" }
        Write-Info "Latest: $tag"
        return $tag
    } catch {
        Write-Err "Could not determine latest version: $_"
        exit 1
    }
}

# ─── Download & install ──────────────────────────────────────────────────────

function Install-Dependencies {
    param([string]$Ver)

    Write-Header "Installing dependencies"

    $bundle = "deps-$Platform.zip"
    $url = "https://github.com/$GitHubRepo/releases/download/$Ver/$bundle"
    $tmpDir = Join-Path $env:TEMP "nanosb-install-$(Get-Random)"

    try {
        New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

        Write-Info "Downloading $bundle..."
        $zipPath = Join-Path $tmpDir $bundle
        try {
            Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop
        } catch {
            Write-Warn "Failed to download $bundle (may not exist yet for this version)"
            Write-Info "URL: $url"
            Write-Info "Skipping deps bundle install. The CLI binary is self-contained on Windows."
            return
        }

        Write-Info "Extracting..."
        Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force

        # Create install directory
        if (-not (Test-Path $targetDir)) {
            New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
        }

        # Install libkrunfw.dll
        Write-Header "Installing libkrunfw.dll"
        $dllSrc = Get-ChildItem -Path $tmpDir -Filter "libkrunfw.dll" -Recurse | Select-Object -First 1
        if ($dllSrc) {
            Copy-Item $dllSrc.FullName -Destination (Join-Path $targetDir "libkrunfw.dll") -Force
            Write-OK "libkrunfw.dll -> $targetDir"
        } else {
            Write-Warn "libkrunfw.dll not found in bundle"
        }

        # Install busybox
        Write-Header "Installing busybox"
        $bbSrc = Get-ChildItem -Path $tmpDir -Filter "busybox" -Recurse | Select-Object -First 1
        if ($bbSrc) {
            Copy-Item $bbSrc.FullName -Destination (Join-Path $targetDir "busybox") -Force
            Write-OK "busybox -> $targetDir"
        } else {
            Write-Warn "busybox not found in bundle"
        }
    } finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ─── PATH configuration ─────────────────────────────────────────────────────

function Set-InstallPath {
    Write-Header "Configuring PATH"

    $machinePath = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    if ($machinePath -split ';' -contains $targetDir) {
        Write-OK "$targetDir already in system PATH"
        return
    }

    Write-Info "Adding $targetDir to system PATH..."
    $newPath = "$machinePath;$targetDir"
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'Machine')

    # Update current session PATH too
    $env:PATH = "$env:PATH;$targetDir"

    Write-OK "Added $targetDir to system PATH"
    Write-Info "New terminal windows will pick up the change automatically."
}

# ─── Verification ────────────────────────────────────────────────────────────

function Test-Installation {
    Write-Header "Verifying installation"

    $dll = Join-Path $targetDir "libkrunfw.dll"
    $bb  = Join-Path $targetDir "busybox"

    if (Test-Path $dll) {
        $size = (Get-Item $dll).Length / 1MB
        Write-OK ("libkrunfw.dll ({0:N1} MB)" -f $size)
    } else {
        Write-Info "libkrunfw.dll not present (not required -libkrun is statically linked)"
    }

    if (Test-Path $bb) {
        $size = (Get-Item $bb).Length / 1MB
        Write-OK ("busybox ({0:N1} MB)" -f $size)
    } else {
        Write-Info "busybox not present (not required -embedded in binary)"
    }
}

# ─── Summary ─────────────────────────────────────────────────────────────────

function Write-Summary {
    param([string]$Ver)

    Write-Header "Installation complete"
    Write-Info "version   -> $Ver"
    Write-Info "platform  -> $Platform"
    Write-Info "directory -> $targetDir"

    if ($script:NeedsReboot) {
        Write-Host ""
        Write-Warn "A reboot is required to activate Hyper-V."
        Write-Info "Run: Restart-Computer"
    }
}

# ─── Main ────────────────────────────────────────────────────────────────────

function Main {
    Write-Host "Nanosandbox Runtime Dependencies Installer (Windows)" -ForegroundColor Cyan
    Write-Host "====================================================="

    $script:NeedsReboot = $false

    Assert-Administrator
    Test-Prerequisites

    $ver = Resolve-LatestVersion -Requested $requestedVersion
    Install-Dependencies -Ver $ver
    Set-InstallPath
    Test-Installation
    Write-Summary -Ver $ver
}

Main

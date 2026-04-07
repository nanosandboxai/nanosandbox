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
    .\install.ps1 -Version v0.3.0

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
        Write-Info ""
        $answer = Read-Host "  Enable Hyper-V now? This requires a reboot. [y/N]"
        if ($answer -match '^[Yy]') {
            Write-Info "Enabling Hyper-V..."
            Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -All -NoRestart | Out-Null
            Write-OK "Hyper-V enabled (reboot required to activate)"
            $script:NeedsReboot = $true
        } else {
            Write-Warn "Skipping Hyper-V. Nanosandbox will not work until Hyper-V is enabled."
        }
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
    Write-Header "Resolving version"

    if ($Version) {
        Write-Info "Using specified version: $Version"
        return $Version
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
            Write-Err "Failed to download $bundle"
            Write-Info "URL: $url"
            Write-Info "Verify $Ver has a Windows build at:"
            Write-Info "  https://github.com/$GitHubRepo/releases/tag/$Ver"
            exit 1
        }

        Write-Info "Extracting..."
        Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force

        # Create install directory
        if (-not (Test-Path $InstallDir)) {
            New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        }

        # Install libkrunfw.dll
        Write-Header "Installing libkrunfw.dll"
        $dllSrc = Get-ChildItem -Path $tmpDir -Filter "libkrunfw.dll" -Recurse | Select-Object -First 1
        if ($dllSrc) {
            Copy-Item $dllSrc.FullName -Destination (Join-Path $InstallDir "libkrunfw.dll") -Force
            Write-OK "libkrunfw.dll -> $InstallDir"
        } else {
            Write-Warn "libkrunfw.dll not found in bundle"
        }

        # Install busybox
        Write-Header "Installing busybox"
        $bbSrc = Get-ChildItem -Path $tmpDir -Filter "busybox" -Recurse | Select-Object -First 1
        if ($bbSrc) {
            Copy-Item $bbSrc.FullName -Destination (Join-Path $InstallDir "busybox") -Force
            Write-OK "busybox -> $InstallDir"
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
    if ($machinePath -split ';' -contains $InstallDir) {
        Write-OK "$InstallDir already in system PATH"
        return
    }

    Write-Info "Adding $InstallDir to system PATH..."
    $newPath = "$machinePath;$InstallDir"
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'Machine')

    # Update current session PATH too
    $env:PATH = "$env:PATH;$InstallDir"

    Write-OK "Added $InstallDir to system PATH"
    Write-Info "New terminal windows will pick up the change automatically."
}

# ─── Verification ────────────────────────────────────────────────────────────

function Test-Installation {
    Write-Header "Verifying installation"

    $dll = Join-Path $InstallDir "libkrunfw.dll"
    $bb  = Join-Path $InstallDir "busybox"

    if (Test-Path $dll) {
        $size = (Get-Item $dll).Length / 1MB
        Write-OK "libkrunfw.dll ({0:N1} MB)" -f $size
    } else {
        Write-Warn "libkrunfw.dll not found at $dll"
    }

    if (Test-Path $bb) {
        $size = (Get-Item $bb).Length / 1MB
        Write-OK "busybox ({0:N1} MB)" -f $size
    } else {
        Write-Warn "busybox not found at $bb"
    }
}

# ─── Summary ─────────────────────────────────────────────────────────────────

function Write-Summary {
    param([string]$Ver)

    Write-Header "Installation complete"
    Write-Info "libkrunfw.dll -> $InstallDir\libkrunfw.dll"
    Write-Info "busybox       -> $InstallDir\busybox"
    Write-Info "version       -> $Ver"
    Write-Info "platform      -> $Platform"

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

    $ver = Resolve-LatestVersion
    Install-Dependencies -Ver $ver
    Set-InstallPath
    Test-Installation
    Write-Summary -Ver $ver
}

Main

#Requires -Version 5.1
<#
.SYNOPSIS
    Nanosandbox Runtime Dependencies Installer for Windows.

.DESCRIPTION
    Checks prerequisites (Hyper-V, Containers, WSL kernel) and installs
    libkrunfw.dll (kernel firmware) next to the nanosb CLI binary.

.EXAMPLE
    # Install latest (from web):
    irm https://github.com/nanosandboxai/install-deps/releases/latest/download/install.ps1 | iex

    # Install specific version:
    .\install.ps1 -Version v0.2.0
#>

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Speed up Invoke-WebRequest

# Wrap entire installer in a function so param() works both when run directly and via iex.
# Script-level param() creates optimized read-only variables that break under Invoke-Expression.
function Install-NanosandboxDeps {
    param(
        [string]$Version,
        [string]$InstallDir = "$env:USERPROFILE\.nanosandbox"
    )

    $GitHubRepo = 'nanosandboxai/install-deps'
    $Platform = 'windows-amd64'
    $targetDir = $InstallDir

    # --- Helpers ---
    function Write-Header  { param([string]$Msg) Write-Host "`n==> $Msg" -ForegroundColor Blue }
    function Write-OK      { param([string]$Msg) Write-Host "  [OK] $Msg" -ForegroundColor Green }
    function Write-Warn    { param([string]$Msg) Write-Host "  [WARN] $Msg" -ForegroundColor Yellow }
    function Write-Err     { param([string]$Msg) Write-Host "  [ERROR] $Msg" -ForegroundColor Red }
    function Write-Info    { param([string]$Msg) Write-Host "  $Msg" }

    Write-Host "Nanosandbox Runtime Dependencies Installer (Windows)" -ForegroundColor Cyan
    Write-Host "====================================================="

    # --- Prerequisites ---
    Write-Header "Checking prerequisites"

    $build = [System.Environment]::OSVersion.Version.Build
    if ($build -lt 17763) {
        Write-Err "Windows build $build is too old. Minimum: 17763 (Windows 10 1809 / Server 2019)."
        return
    }
    Write-OK "Windows build $build"

    # Check Hyper-V: try vmcompute service first (works on both Server and Desktop)
    $vmcompute = Get-Service vmcompute -ErrorAction SilentlyContinue
    if ($vmcompute -and $vmcompute.Status -eq 'Running') {
        Write-OK "Hyper-V / HCS enabled (vmcompute running)"
    } else {
        $hyperv = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -ErrorAction SilentlyContinue
        if ($null -eq $hyperv -or $hyperv.State -ne 'Enabled') {
            Write-Warn "Hyper-V is not enabled."
            Write-Info "Nanosandbox uses the Host Compute Service (HCS) which requires Hyper-V."
            Write-Info "Enable with: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All"
            Write-Info ""
        } else {
            Write-OK "Hyper-V enabled"
        }
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

    # WSL kernel (HCS uses the WSL2 kernel to boot Linux VMs)
    $wslKernel = "C:\Program Files\WSL\tools\kernel"
    if (Test-Path $wslKernel) {
        Write-OK "WSL kernel found"
    } else {
        Write-Warn "WSL kernel not found at: $wslKernel"
        Write-Info "Nanosandbox uses the WSL2 kernel to boot Linux VMs via HCS."
        Write-Info "Install WSL with: wsl --install --no-distribution"
        Write-Info ""
    }

    # --- Version resolution ---
    Write-Header "Resolving version"

    if ($Version) {
        Write-Info "Using specified version: $Version"
        $ver = $Version
    } else {
        Write-Info "Fetching latest release..."
        try {
            $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$GitHubRepo/releases" `
                -Headers @{ Accept = 'application/vnd.github+json' } `
                -ErrorAction Stop
            # Sort by published_at desc — the default /releases ordering is by
            # tag commit date, which can rank a re-tagged release below older
            # ones if its tag points to an older commit.
            $ver = ($releases | Sort-Object -Property published_at -Descending | Select-Object -First 1).tag_name
            if (-not $ver) { throw "No releases found" }
            Write-Info "Latest: $ver"
        } catch {
            Write-Err "Could not determine latest version: $_"
            return
        }
    }

    # --- Download & install dependencies ---
    Write-Header "Installing dependencies (libkrunfw.dll + busybox + vsock_proxy)"

    $libsDir = Join-Path $targetDir "libs"
    if (-not (Test-Path $libsDir)) {
        New-Item -ItemType Directory -Path $libsDir -Force | Out-Null
    }

    $bundle = "deps-$Platform.zip"
    $url = "https://github.com/$GitHubRepo/releases/download/$ver/$bundle"
    $tmpDir = Join-Path $env:TEMP "nanosb-install-$(Get-Random)"

    try {
        New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null
        Write-Info "Downloading $bundle..."
        $zipPath = Join-Path $tmpDir $bundle

        Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop

        Write-Info "Extracting..."
        Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force

        # libkrunfw.dll — kernel firmware
        $dllSrc = Get-ChildItem -Path $tmpDir -Filter "libkrunfw.dll" -Recurse | Select-Object -First 1
        if ($dllSrc) {
            Copy-Item $dllSrc.FullName -Destination (Join-Path $libsDir "libkrunfw.dll") -Force
            $size = $dllSrc.Length / 1MB
            Write-OK ("libkrunfw.dll ({0:N1} MB) -> $libsDir" -f $size)
            # Clean up legacy root-level copy if present
            $legacyDll = Join-Path $targetDir "libkrunfw.dll"
            if (Test-Path $legacyDll) {
                Remove-Item $legacyDll -Force -ErrorAction SilentlyContinue
                Write-Info "Removed legacy $legacyDll (moved to libs/)"
            }
        } else {
            Write-Warn "libkrunfw.dll not found in bundle"
        }

        # busybox — static Linux ELF, required for VM init script
        $bbSrc = Get-ChildItem -Path $tmpDir -Filter "busybox" -Recurse | Select-Object -First 1
        if ($bbSrc) {
            Copy-Item $bbSrc.FullName -Destination (Join-Path $libsDir "busybox") -Force
            $size = $bbSrc.Length / 1MB
            Write-OK ("busybox ({0:N1} MB) -> $libsDir" -f $size)
            # Clean up legacy root-level copy if present
            $legacyBb = Join-Path $targetDir "busybox"
            if (Test-Path $legacyBb) {
                Remove-Item $legacyBb -Force -ErrorAction SilentlyContinue
                Write-Info "Removed legacy $legacyBb (moved to libs/)"
            }
        } else {
            Write-Warn "busybox not found in bundle (VM init may fail)"
            Write-Info "You can manually place a static Linux busybox binary at: $libsDir\busybox"
        }

        # vsock_proxy — static Linux ELF, AF_VSOCK to TCP proxy for HvSocket guest communication
        $vpSrc = Get-ChildItem -Path $tmpDir -Filter "vsock_proxy" -Recurse | Select-Object -First 1
        if ($vpSrc) {
            Copy-Item $vpSrc.FullName -Destination (Join-Path $libsDir "vsock_proxy") -Force
            $size = $vpSrc.Length / 1KB
            Write-OK ("vsock_proxy ({0:N0} KB) -> $libsDir" -f $size)
            # Clean up legacy root-level copy if present
            $legacyVp = Join-Path $targetDir "vsock_proxy"
            if (Test-Path $legacyVp) {
                Remove-Item $legacyVp -Force -ErrorAction SilentlyContinue
                Write-Info "Removed legacy $legacyVp (moved to libs/)"
            }
        } else {
            Write-Warn "vsock_proxy not found in bundle (HvSocket communication will not work)"
        }
    } catch {
        Write-Warn "Failed to download deps bundle: $_"
        Write-Info "You may need to place files manually in: $libsDir"
    } finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    # --- Summary ---
    Write-Header "Installation complete"
    Write-Info "version   -> $ver"
    Write-Info "directory -> $libsDir"
}

# Invoke the function - @args passes through any command-line parameters
Install-NanosandboxDeps @args

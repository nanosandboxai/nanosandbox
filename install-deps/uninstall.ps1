#Requires -Version 5.1
<#
.SYNOPSIS
    Nanosandbox Runtime Dependencies Uninstaller for Windows.

.DESCRIPTION
    Removes: libkrunfw.dll + busybox
    Optionally removes the install directory and PATH entry.
    Does NOT remove the nanosb CLI binary or disable Hyper-V.

.PARAMETER InstallDir
    Installation directory to clean. Defaults to "$env:USERPROFILE\.nanosandbox".

.EXAMPLE
    # Uninstall (from web):
    irm https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.ps1 | iex

    # Uninstall from custom path:
    .\uninstall.ps1 -InstallDir C:\opt\nanosandbox
#>
[CmdletBinding()]
param(
    [string]$InstallDir = "$env:USERPROFILE\.nanosandbox"
)

$ErrorActionPreference = 'Stop'

# ─── Helpers ─────────────────────────────────────────────────────────────────

function Write-Header  { param([string]$Msg) Write-Host "`n==> $Msg" -ForegroundColor Blue }
function Write-OK      { param([string]$Msg) Write-Host "  [OK] $Msg" -ForegroundColor Green }
function Write-Info    { param([string]$Msg) Write-Host "  $Msg" }

# ─── Admin check ─────────────────────────────────────────────────────────────

function Assert-Administrator {
    $current = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $current.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Host "  [ERROR] This uninstaller must be run as Administrator." -ForegroundColor Red
        Write-Info "Right-click PowerShell and select 'Run as administrator', then try again."
        return
    }
}

# ─── Remove files ────────────────────────────────────────────────────────────

function Remove-Dependencies {
    Write-Header "Removing dependencies"

    $removed = 0

    # Current layout: libs/ subdirectory
    $libsDir = Join-Path $InstallDir "libs"
    if (Test-Path $libsDir) {
        Remove-Item $libsDir -Recurse -Force
        Write-OK "Removed $libsDir"
        $removed++
    }

    # Legacy layout: root-level files
    foreach ($file in @('libkrunfw.dll', 'busybox')) {
        $path = Join-Path $InstallDir $file
        if (Test-Path $path) {
            Remove-Item $path -Force
            Write-OK "Removed legacy $path"
            $removed++
        }
    }

    # Remove install directory if empty
    if ((Test-Path $InstallDir) -and @(Get-ChildItem $InstallDir).Count -eq 0) {
        Remove-Item $InstallDir -Force
        Write-OK "Removed empty directory $InstallDir"
    }

    return $removed
}

# ─── Clean PATH ──────────────────────────────────────────────────────────────

function Remove-FromPath {
    Write-Header "Cleaning PATH"

    $machinePath = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    $entries = $machinePath -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne '' }

    if ($entries.Count -lt ($machinePath -split ';').Count) {
        $newPath = $entries -join ';'
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'Machine')
        Write-OK "Removed $InstallDir from system PATH"
    } else {
        Write-Info "$InstallDir was not in system PATH"
    }
}

# ─── Clean caches ────────────────────────────────────────────────────────────

function Remove-Caches {
    Write-Header "Cleaning caches"

    # All nanosandbox data (cache, logs, vhdx, etc.) lives under ~/.nanosandbox/
    $nanosandboxData = Join-Path $env:USERPROFILE '.nanosandbox'
    if (Test-Path $nanosandboxData) {
        $answer = Read-Host "  Remove all nanosandbox data at $nanosandboxData? [y/N]"
        if ($answer -match '^[Yy]') {
            Remove-Item $nanosandboxData -Recurse -Force -ErrorAction SilentlyContinue
            Write-OK "Removed $nanosandboxData"
        } else {
            Write-Info "Kept $nanosandboxData"
        }
    }

    # Clean up legacy locations if they exist
    foreach ($legacy in @('C:\tmp\nanosb-vhdx-cache', "$env:LOCALAPPDATA\nanosandbox")) {
        if (Test-Path $legacy) {
            Remove-Item $legacy -Recurse -Force -ErrorAction SilentlyContinue
            Write-OK "Removed legacy directory $legacy"
        }
    }
}

# ─── Summary ─────────────────────────────────────────────────────────────────

function Write-Summary {
    Write-Header "Uninstall complete"
    Write-Info "Runtime dependencies have been removed."
    Write-Info "The nanosb CLI binary was NOT removed."
    Write-Info "Hyper-V was NOT disabled (other software may depend on it)."
}

# ─── Main ────────────────────────────────────────────────────────────────────

function Main {
    Write-Host "Nanosandbox Runtime Dependencies Uninstaller (Windows)" -ForegroundColor Cyan
    Write-Host "======================================================="

    Assert-Administrator
    Remove-Dependencies
    Remove-FromPath
    Remove-Caches
    Write-Summary
}

Main

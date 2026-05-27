#Requires -Version 5.1
<#
.SYNOPSIS
    Bootstrap a Windows development environment for building nanosb from source.

.DESCRIPTION
    Sets up everything needed to compile cli/target/debug/nanosb.exe on a fresh
    Windows machine. This script handles BUILD-time prerequisites only — it does
    NOT enable Hyper-V/WHPX/WSL or install runtime libraries. Pass
    -InstallRuntimeDeps to also fetch libkrunfw.dll and helper binaries via the
    install-deps installer (still requires manual Hyper-V/WSL setup to actually
    *run* nanosb).

    Steps performed (each is idempotent):
      1. Verify Windows version + git available
      2. winget-install: Visual Studio 2022 Build Tools (VC++ workload),
         Rustup, NASM, Visual C++ Redistributable, optional GitHub CLI
      3. Add NASM install dir to User PATH (winget does not do this)
      4. Optionally git pull cli + sandbox + runtime in lockstep
      5. Drop empty stub files into runtime/deps/libkrun/src/libkrunfw-win/
         so cargo commands targeting the runtime workspace don't trip its
         build.rs panic
      6. Optionally run install-deps (libkrunfw.dll + busybox + vsock_proxy +
         plan9_mount into %USERPROFILE%\.nanosandbox\libs\)
      7. cargo build (debug) from cli/

.PARAMETER RepoRoot
    Directory containing cli/, sandbox/, runtime/ as siblings. Default: parent
    of this script's repo.

.PARAMETER SkipPull
    Skip 'git pull' on cli/sandbox/runtime. Useful if you have WIP changes.

.PARAMETER SkipBuild
    Stop after toolchain setup; do not run 'cargo build'.

.PARAMETER InstallRuntimeDeps
    Also run the install-deps installer (libkrunfw.dll, busybox, vsock_proxy,
    plan9_mount). Required to actually *run* nanosb; not needed to compile it.

.PARAMETER InstallDepsVersion
    Version tag passed to the install-deps installer. Default: latest.

.EXAMPLE
    # First-time setup, build only:
    .\scripts\bootstrap-dev-windows.ps1

.EXAMPLE
    # Full setup including runtime libs:
    .\scripts\bootstrap-dev-windows.ps1 -InstallRuntimeDeps

.EXAMPLE
    # Re-run after editing local code, skip pulls:
    .\scripts\bootstrap-dev-windows.ps1 -SkipPull
#>

[CmdletBinding()]
param(
    [string]$RepoRoot,
    [switch]$SkipPull,
    [switch]$SkipBuild,
    [switch]$InstallRuntimeDeps,
    [string]$InstallDepsVersion
)

$ErrorActionPreference = 'Stop'
$ProgressPreference     = 'SilentlyContinue'

# --- Helpers (match install.ps1 style) ---
function Write-Header { param($msg) Write-Host "`n==> $msg" -ForegroundColor Blue }
function Write-Info   { param($msg) Write-Host "[INFO] $msg" -ForegroundColor Cyan }
function Write-Ok     { param($msg) Write-Host "[OK]   $msg" -ForegroundColor Green }
function Write-Warn   { param($msg) Write-Host "[WARN] $msg" -ForegroundColor Yellow }
function Write-Err    { param($msg) Write-Host "[ERR]  $msg" -ForegroundColor Red }

function Test-WingetPackageInstalled {
    param([string]$Id)
    $out = winget list --id $Id --exact 2>$null | Out-String
    return ($out -match [regex]::Escape($Id))
}

function Install-WingetPackage {
    param([string]$Id, [string]$Display, [string]$Override)
    if (Test-WingetPackageInstalled -Id $Id) {
        Write-Ok "$Display already installed"
        return
    }
    Write-Info "Installing $Display ($Id)..."
    $args = @('install', '--id', $Id, '--exact', '--silent', '--accept-source-agreements', '--accept-package-agreements')
    if ($Override) { $args += @('--override', $Override) }
    & winget @args
    if ($LASTEXITCODE -ne 0) {
        throw "winget install $Id failed with exit code $LASTEXITCODE"
    }
    Write-Ok "$Display installed"
}

# --- Resolve paths ---
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$cliRoot   = Split-Path -Parent $scriptDir
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent $cliRoot }
$cliRoot     = Join-Path $RepoRoot 'cli'
$sandboxRoot = Join-Path $RepoRoot 'sandbox'
$runtimeRoot = Join-Path $RepoRoot 'runtime'

Write-Host ""
Write-Host "  Nanosandbox dev environment bootstrap (Windows)" -ForegroundColor White
Write-Host "  ===============================================" -ForegroundColor DarkGray
Write-Host ""
Write-Info "RepoRoot    = $RepoRoot"
Write-Info "cli         = $cliRoot"
Write-Info "sandbox     = $sandboxRoot"
Write-Info "runtime     = $runtimeRoot"

# --- Step 1: Verify host ---
Write-Header "Verifying host"
$build = [System.Environment]::OSVersion.Version.Build
if ($build -lt 17763) {
    throw "Windows build $build is too old. Minimum: 17763 (Windows 10 1809 / Server 2019)."
}
Write-Ok "Windows build $build"

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "git is required but not on PATH. Install from https://git-scm.com/ and re-run."
}
Write-Ok "git $(git --version)"

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "winget is required but not on PATH. Install 'App Installer' from the Microsoft Store and re-run."
}

foreach ($p in @($cliRoot, $sandboxRoot, $runtimeRoot)) {
    if (-not (Test-Path $p)) {
        Write-Warn "Sibling repo missing: $p"
        Write-Warn "Clone all three repos under $RepoRoot before running this script:"
        Write-Warn "  git clone git@github.com:nanosandboxai/cli.git     $cliRoot"
        Write-Warn "  git clone git@github.com:nanosandboxai/sandbox.git $sandboxRoot"
        Write-Warn "  git clone git@github.com:nanosandboxai/runtime.git $runtimeRoot"
        throw "Required repo not found: $p"
    }
}
Write-Ok "All three repos present"

# --- Step 2: Install build toolchain via winget ---
Write-Header "Installing build toolchain (winget)"

# VS 2022 Build Tools with VC++ workload (provides cl.exe + link.exe + Win SDK).
# Rustc auto-detects MSVC via vswhere; cl/link don't need to be on PATH.
Install-WingetPackage `
    -Id 'Microsoft.VisualStudio.2022.BuildTools' `
    -Display 'Visual Studio 2022 Build Tools (VC++ workload)' `
    -Override '--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'

# Rustup (host triple x86_64-pc-windows-msvc by default on Windows x64)
Install-WingetPackage -Id 'Rustlang.Rustup' -Display 'Rustup'

# Re-shim PATH so rustup/cargo are visible in this session
$rustBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $rustBin) -and ($env:Path -notlike "*$rustBin*")) {
    $env:Path = "$rustBin;$env:Path"
}

if (Get-Command rustup -ErrorAction SilentlyContinue) {
    Write-Info "Configuring stable-x86_64-pc-windows-msvc toolchain..."
    & rustup default stable-x86_64-pc-windows-msvc | Out-Null
    & rustup component add rust-src llvm-tools-preview 2>&1 | Out-Null
    Write-Ok "Rust: $(rustc -V)"
} else {
    Write-Warn "rustup not found on PATH after install — open a new PowerShell and re-run."
}

# NASM — required by aws-lc-sys (transitive crypto dep of russh)
Install-WingetPackage -Id 'NASM.NASM' -Display 'NASM (Netwide Assembler)'

# Visual C++ Redistributable — required to *run* nanosb.exe outside cargo
Install-WingetPackage -Id 'Microsoft.VCRedist.2015+.x64' -Display 'Visual C++ 2015-2022 Redistributable (x64)'

# GitHub CLI — only needed if you fetch the kernel bundle CI artifact (Approach B)
if (-not (Test-WingetPackageInstalled -Id 'GitHub.cli')) {
    Write-Info "GitHub CLI not installed (only needed for the Approach B kernel-artifact path; skipping)"
}

# --- Step 3: Ensure NASM is on User PATH ---
Write-Header "Configuring NASM PATH"
$nasmCandidates = @(
    "$env:LOCALAPPDATA\bin\NASM",                    # winget user-scope default
    "$env:ProgramFiles\NASM",                        # vendor MSI default
    "${env:ProgramFiles(x86)}\NASM"
)
$nasmDir = $nasmCandidates | Where-Object { Test-Path (Join-Path $_ 'nasm.exe') } | Select-Object -First 1
if (-not $nasmDir) {
    Write-Warn "nasm.exe not found in standard install locations. Build will fail until NASM is installed and on PATH."
} else {
    Write-Info "NASM found at: $nasmDir"
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    if ($userPath -notlike "*$nasmDir*") {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$nasmDir", 'User')
        Write-Ok "Added $nasmDir to User PATH (effective in new shells)"
    } else {
        Write-Ok "$nasmDir already on User PATH"
    }
    # Make NASM visible in *this* shell so the build later in this script works
    if ($env:Path -notlike "*$nasmDir*") {
        $env:Path = "$nasmDir;$env:Path"
    }
    if (Get-Command nasm -ErrorAction SilentlyContinue) {
        Write-Ok "NASM: $((nasm -v 2>&1 | Select-Object -First 1).Trim())"
    }
}

# --- Step 4: Sync sibling repos ---
if ($SkipPull) {
    Write-Header "Skipping repo sync (-SkipPull)"
} else {
    Write-Header "Syncing cli + sandbox + runtime to origin/main"
    foreach ($repo in @($cliRoot, $sandboxRoot, $runtimeRoot)) {
        $name = Split-Path -Leaf $repo
        Write-Info "git pull --ff-only ($name)"
        Push-Location $repo
        try {
            $dirty = git status --porcelain
            if ($dirty) {
                Write-Warn "$name has uncommitted changes:"
                $dirty -split "`n" | ForEach-Object { Write-Host "    $_" }
                Write-Warn "Skipping pull for $name. Stash or commit, then re-run with -SkipPull or fresh."
                continue
            }
            git pull --ff-only 2>&1 | ForEach-Object { Write-Host "    $_" }
            if ($LASTEXITCODE -ne 0) {
                Write-Warn "Pull failed for $name (exit $LASTEXITCODE). Continuing — current commit preserved."
            } else {
                $head = git log -1 --format='%h %s'
                Write-Ok "$name @ $head"
            }
        } finally {
            Pop-Location
        }
    }
}

# --- Step 5: libkrunfw-win stub files ---
Write-Header "Staging libkrunfw-win stubs (defensive)"
$winDir = Join-Path $runtimeRoot 'deps\libkrun\src\libkrunfw-win'
if (-not (Test-Path $winDir)) {
    Write-Warn "libkrunfw-win directory not found: $winDir"
    Write-Warn "(runtime layout may have changed; skipping stubs)"
} else {
    $vmlinux    = Join-Path $winDir 'vmlinux.bin'
    $guestAddr  = Join-Path $winDir 'guest_addr.txt'
    $entryAddr  = Join-Path $winDir 'entry_addr.txt'
    if (-not (Test-Path $vmlinux))   { New-Item -ItemType File -Force -Path $vmlinux   | Out-Null; Write-Ok "Stubbed vmlinux.bin (zero bytes)" }    else { Write-Ok "vmlinux.bin already present" }
    if (-not (Test-Path $guestAddr)) { '0' | Set-Content -NoNewline -Encoding ascii -Path $guestAddr;  Write-Ok "Stubbed guest_addr.txt" } else { Write-Ok "guest_addr.txt already present" }
    if (-not (Test-Path $entryAddr)) { '0' | Set-Content -NoNewline -Encoding ascii -Path $entryAddr;  Write-Ok "Stubbed entry_addr.txt" } else { Write-Ok "entry_addr.txt already present" }
    Write-Info "(These three files are gitignored — won't dirty the working tree)"
}

# --- Step 6: Optionally fetch runtime libraries ---
if ($InstallRuntimeDeps) {
    Write-Header "Installing runtime libraries via install-deps"
    Write-Warn "install-deps places libkrunfw.dll + busybox + vsock_proxy + plan9_mount into %USERPROFILE%\.nanosandbox\libs\"
    Write-Warn "These are needed at *run* time, not at compile time."
    $depsRepo = 'nanosandboxai/install-deps'
    $tag = if ($InstallDepsVersion) { $InstallDepsVersion } else { 'latest' }
    $url = if ($tag -eq 'latest') {
        "https://github.com/$depsRepo/releases/latest/download/install.ps1"
    } else {
        "https://github.com/$depsRepo/releases/download/$tag/install.ps1"
    }
    Write-Info "Fetching $url ..."
    try {
        $script = Invoke-RestMethod -Uri $url -ErrorAction Stop
        # The script ends with `Install-NanosandboxDeps @args`, which would receive
        # this script's $args. Strip it and call with explicit params.
        $script = $script -replace 'Install-NanosandboxDeps\s+@args\s*$', ''
        Invoke-Expression $script
        if ($InstallDepsVersion) {
            Install-NanosandboxDeps -Version $InstallDepsVersion
        } else {
            Install-NanosandboxDeps
        }
    } catch {
        Write-Warn "install-deps failed: $_"
        Write-Warn "You can run it manually:  irm $url | iex"
    }
} else {
    Write-Info "Skipping runtime deps install (pass -InstallRuntimeDeps to enable)"
}

# --- Step 7: Build cli ---
if ($SkipBuild) {
    Write-Header "Skipping cargo build (-SkipBuild)"
} else {
    Write-Header "Building cli (cargo build)"
    Push-Location $cliRoot
    try {
        & cargo build
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
        $bin = Join-Path $cliRoot 'target\debug\nanosb.exe'
        if (Test-Path $bin) {
            Write-Ok "Built: $bin"
            $version = & $bin --version
            Write-Ok "Smoke test: $version"
        } else {
            Write-Warn "cargo build returned 0 but binary not found at $bin"
        }
    } finally {
        Pop-Location
    }
}

Write-Host ""
Write-Host "  Done." -ForegroundColor White
Write-Host ""
Write-Host "  Next steps:" -ForegroundColor White
Write-Host "    cd $cliRoot" -ForegroundColor DarkGray
Write-Host "    cargo build --release          # optimized binary" -ForegroundColor DarkGray
Write-Host "    .\target\debug\nanosb.exe --help" -ForegroundColor DarkGray
if (-not $InstallRuntimeDeps) {
    Write-Host ""
    Write-Host "  To actually *run* nanosb (not just compile):" -ForegroundColor White
    Write-Host "    1. Re-run this script with -InstallRuntimeDeps to fetch libkrunfw.dll" -ForegroundColor DarkGray
    Write-Host "    2. As Administrator, enable Hyper-V + WHPX + WSL kernel:" -ForegroundColor DarkGray
    Write-Host "         Enable-WindowsOptionalFeature -Online -All -FeatureName Microsoft-Hyper-V" -ForegroundColor DarkGray
    Write-Host "         Enable-WindowsOptionalFeature -Online -All -FeatureName HypervisorPlatform" -ForegroundColor DarkGray
    Write-Host "         Enable-WindowsOptionalFeature -Online -All -FeatureName VirtualMachinePlatform" -ForegroundColor DarkGray
    Write-Host "         wsl --install --no-distribution" -ForegroundColor DarkGray
    Write-Host "         Restart-Computer" -ForegroundColor DarkGray
    Write-Host "    3. nanosb doctor" -ForegroundColor DarkGray
}
Write-Host ""

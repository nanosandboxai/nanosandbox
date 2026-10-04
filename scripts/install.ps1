#Requires -Version 5.1
<#
.SYNOPSIS
    Nanosandbox CLI Installer for Windows.

.DESCRIPTION
    Downloads and installs the nanosb CLI binary and runtime dependencies on Windows.
    1. Checks prerequisites (Hyper-V, WHPX, WSL2, VirtualMachinePlatform) and enables missing features
    2. Downloads the nanosb.exe binary from GitHub Releases
    3. Installs runtime dependencies via install-deps
    4. Adds the install directory to the user PATH

.EXAMPLE
    # Install this release (tag is stamped by CI — use raw.githubusercontent.com,
    # not the release asset URL, because GitHub serves assets as octet-stream
    # which PowerShell's irm cannot pipe directly to iex):
    irm https://raw.githubusercontent.com/nanosandboxai/nanosandbox/main/scripts/install.ps1 | iex

    # Or download and run locally for a specific version:
    .\install.ps1 -Version v0.2.0-rc17
    .\install.ps1 -Version v0.2.0

    # Or download and run locally:
    .\install.ps1 -Version v0.2.0-rc17
    .\install.ps1 -Version v0.2.0
#>

$ErrorActionPreference = "Stop"

# Wrap entire installer in a function so param() works both when run directly and via iex.
# Script-level param() creates optimized read-only variables that break under Invoke-Expression.
function Install-NanosandboxCLI {
    param(
        [string]$Version = "",
        [string]$InstallDir = "$env:USERPROFILE\.nanosandbox",
        # Skip the Windows Defender exclusion prompt entirely. nanosb.exe is an
        # unsigned Rust binary which Defender's ML heuristics frequently flag
        # as a generic threat (Wacatac etc.). Adding an exclusion for the
        # install dir prevents the .exe from being silently quarantined on
        # download. Pass -SkipDefenderExclusion to opt out (e.g. when the
        # install dir is already covered by an existing exclusion).
        [switch]$SkipDefenderExclusion,
        # Skip the prompt and add the exclusion automatically. Useful for
        # unattended/CI installs.
        [switch]$AddDefenderExclusion,
        # Skip WSL2 prerequisite check entirely (for advanced users who know
        # they don't need WSL2 or will install it separately).
        [switch]$SkipWsl2Check
    )

    # --- Helpers ---
    function Write-Info    { param($msg) Write-Host "[INFO] $msg" -ForegroundColor Cyan }
    function Write-Ok      { param($msg) Write-Host "[OK]   $msg" -ForegroundColor Green }
    function Write-Warn    { param($msg) Write-Host "[WARN] $msg" -ForegroundColor Yellow }
    function Write-Err     { param($msg) Write-Host "[ERROR] $msg" -ForegroundColor Red }

    # --- Prerequisites ---
    Write-Host ""
    Write-Host "  Nanosandbox CLI Installer for Windows" -ForegroundColor White
    Write-Host "  ======================================" -ForegroundColor DarkGray
    Write-Host ""

    $build = [System.Environment]::OSVersion.Version.Build
    if ($build -lt 17763) {
        Write-Err "Windows 10 version 1809 (build 17763) or later is required. Current build: $build"
        return
    }

    # --- Windows prerequisites ---
    # Enable all required features in one pass, then do a single reboot if needed.
    # Features required: Hyper-V, Windows Hypervisor Platform (WHPX),
    # Windows Subsystem for Linux, Virtual Machine Platform (WSL2).
    # VC++ Redistributable is installed inline (no reboot needed).
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)

    # Check if the user is in the Hyper-V Administrators group (well-known SID
    # S-1-5-32-578). HCS APIs require either Admin or Hyper-V Administrators
    # membership to boot sandboxes.
    $isHyperVAdmin = $false
    try {
        $principal = New-Object Security.Principal.WindowsPrincipal(
            [Security.Principal.WindowsIdentity]::GetCurrent())
        $hypervSid = New-Object Security.Principal.SecurityIdentifier('S-1-5-32-578')
        $isHyperVAdmin = $principal.IsInRole($hypervSid)
    } catch {
        $isHyperVAdmin = $false
    }

    # Upfront guidance if neither elevated nor a Hyper-V Administrator.
    # This is the most common cause of confusing "access denied" errors at
    # sandbox-creation time (runtime issue #133).
    if (-not $isAdmin -and -not $isHyperVAdmin) {
        Write-Host ""
        Write-Warn "You are not running this installer as Administrator."
        Write-Warn "You are also not a member of the 'Hyper-V Administrators' group."
        Write-Host ""
        Write-Warn "Why this matters:"
        Write-Warn "  - Windows features (Hyper-V, WHPX, WSL, VirtualMachinePlatform)"
        Write-Warn "    cannot be enabled automatically."
        Write-Warn "  - Windows Defender exclusions for nanosb.exe cannot be added."
        Write-Warn "  - Sandbox creation will fail with access-denied errors from the"
        Write-Warn "    Host Compute Service (vmcompute) even after install completes."
        Write-Host ""
        Write-Warn "Recommended:"
        Write-Warn "  1) Close this window."
        Write-Warn "  2) Right-click PowerShell and choose 'Run as administrator'."
        Write-Warn "  3) Re-run the installer command."
        Write-Host ""
        Write-Warn "If you cannot run as Administrator, ask an admin to add you to the"
        Write-Warn "'Hyper-V Administrators' group:"
        Write-Warn "  Add-LocalGroupMember -Group 'Hyper-V Administrators' -Member <your-user>"
        Write-Warn "Then log out and back in for the group to take effect."
        Write-Host ""
        $answer = Read-Host "  Continue without elevation (the installer will skip feature setup)? [y/N]"
        if ($answer -notmatch '^[Yy]') {
            Write-Info "Aborted. Re-run the installer from an elevated terminal."
            return
        }
        Write-Host ""
    } elseif (-not $isAdmin -and $isHyperVAdmin) {
        Write-Info "Running without elevation, but the current user is in 'Hyper-V Administrators'."
        Write-Info "Sandbox creation should still work. Some installer steps (feature enables,"
        Write-Info "Defender exclusions) require Administrator and will be skipped."
    }

    $rebootNeeded = $false

    # -- Hyper-V --
    $vmcompute = Get-Service vmcompute -ErrorAction SilentlyContinue
    if ($vmcompute -and $vmcompute.Status -eq 'Running') {
        Write-Ok "Hyper-V enabled"
    } else {
        $hyperv = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -ErrorAction SilentlyContinue
        if (-not $hyperv -or $hyperv.State -ne "Enabled") {
            if (-not $isAdmin) {
                Write-Warn "Hyper-V is not enabled (requires Administrator to fix)."
            } else {
                Write-Info "Enabling Hyper-V..."
                try {
                    $r = Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All -NoRestart -ErrorAction Stop
                    Write-Ok "Hyper-V enabled"
                    if ($r.RestartNeeded) { $rebootNeeded = $true }
                } catch {
                    Write-Warn "Failed to enable Hyper-V: $_"
                }
            }
        } else {
            Write-Ok "Hyper-V enabled"
        }
    }

    # -- Windows Hypervisor Platform (WHPX) --
    $whpx = Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -ErrorAction SilentlyContinue
    if (-not $whpx -or $whpx.State -ne "Enabled") {
        if (-not $isAdmin) {
            Write-Warn "Windows Hypervisor Platform is not enabled (requires Administrator to fix)."
        } else {
            Write-Info "Enabling Windows Hypervisor Platform..."
            try {
                $r = Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All -NoRestart -ErrorAction Stop
                Write-Ok "Windows Hypervisor Platform enabled"
                if ($r.RestartNeeded) { $rebootNeeded = $true }
            } catch {
                Write-Warn "Failed to enable Windows Hypervisor Platform: $_"
            }
        }
    } else {
        Write-Ok "Windows Hypervisor Platform enabled"
    }

    # -- Visual C++ Redistributable (no reboot needed) --
    $vcKey = "HKLM:\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64"
    $vcInstalled = (Get-ItemProperty $vcKey -ErrorAction SilentlyContinue).Installed -eq 1
    if (-not $vcInstalled) {
        Write-Info "Installing Visual C++ 2015-2022 Redistributable..."
        try {
            $vcInstaller = Join-Path $env:TEMP "vc_redist.x64.exe"
            Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vc_redist.x64.exe" -OutFile $vcInstaller -UseBasicParsing
            Start-Process -FilePath $vcInstaller -ArgumentList "/install", "/quiet", "/norestart" -Wait
            Write-Ok "Visual C++ Redistributable installed"
        } catch {
            Write-Warn "Failed to install Visual C++ Redistributable: $_"
            Write-Warn "Install manually from: https://aka.ms/vs/17/release/vc_redist.x64.exe"
        }
    } else {
        Write-Ok "Visual C++ Redistributable installed"
    }

    # -- WSL2 (two Windows features: WSL + Virtual Machine Platform) --
    if (-not $SkipWsl2Check) {
        # Check WSL subsystem feature
        $wslFeature = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Windows-Subsystem-Linux -ErrorAction SilentlyContinue
        if (-not $wslFeature -or $wslFeature.State -ne "Enabled") {
            if (-not $isAdmin) {
                Write-Warn "Windows Subsystem for Linux is not enabled (requires Administrator to fix)."
            } else {
                Write-Info "Enabling Windows Subsystem for Linux..."
                try {
                    $r = Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Windows-Subsystem-Linux -All -NoRestart -ErrorAction Stop
                    Write-Ok "Windows Subsystem for Linux enabled"
                    if ($r.RestartNeeded) { $rebootNeeded = $true }
                } catch {
                    Write-Warn "Failed to enable Windows Subsystem for Linux: $_"
                }
            }
        } else {
            Write-Ok "Windows Subsystem for Linux enabled"
        }

        # Check Virtual Machine Platform feature (required for WSL2)
        $vmpFeature = Get-WindowsOptionalFeature -Online -FeatureName VirtualMachinePlatform -ErrorAction SilentlyContinue
        if (-not $vmpFeature -or $vmpFeature.State -ne "Enabled") {
            if (-not $isAdmin) {
                Write-Warn "Virtual Machine Platform is not enabled (requires Administrator to fix)."
            } else {
                Write-Info "Enabling Virtual Machine Platform..."
                try {
                    $r = Enable-WindowsOptionalFeature -Online -FeatureName VirtualMachinePlatform -All -NoRestart -ErrorAction Stop
                    Write-Ok "Virtual Machine Platform enabled"
                    if ($r.RestartNeeded) { $rebootNeeded = $true }
                } catch {
                    Write-Warn "Failed to enable Virtual Machine Platform: $_"
                }
            }
        } else {
            Write-Ok "Virtual Machine Platform enabled"
        }
    }

    # -- Single reboot if any feature needed it --
    # Register a RunOnce key so the installer resumes automatically after reboot.
    if ($rebootNeeded) {
        Write-Host ""
        Write-Warn "A single restart is required to activate all Windows virtualization features enabled above."
        Write-Info "The installer will resume automatically after login."
        Write-Host ""

        # Build the resume command: re-run this installer with the same version arg
        # from a PowerShell window that opens automatically after login.
        $resumeUrl = 'https://raw.githubusercontent.com/nanosandboxai/nanosandbox/main/scripts/install.ps1'
        $resumeInner = if ($Version) {
            "& ([scriptblock]::Create((irm '$resumeUrl'))) -Version '$Version'"
        } else {
            "irm '$resumeUrl' | iex"
        }
        $resumeCmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command `"$resumeInner`""
        try {
            $runOnceKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce"
            Set-ItemProperty -Path $runOnceKey -Name "NanosbInstall" -Value $resumeCmd -ErrorAction Stop
            Write-Ok "Installer will resume automatically after restart."
        } catch {
            Write-Warn "Could not register auto-resume: $_"
            Write-Warn "After restarting, re-run this installer manually to complete nanosb installation."
        }

        $restart = Read-Host "  Restart now? [Y/n]"
        if ($restart -notmatch '^[Nn]') {
            Restart-Computer -Force
        } else {
            Write-Info "Please restart your computer — the installer will resume automatically."
        }
        return
    }

    if (-not $isAdmin) {
        Write-Warn "Some prerequisites could not be checked (not running as Administrator)."
        Write-Warn "Re-run as Administrator for a fully automated setup."
        Write-Host ""
    }

    # --- Resolve version ---
    $releaseRepo = "nanosandboxai/nanosandbox"
    $resolvedVersion = $Version

    if (-not $resolvedVersion -or $resolvedVersion -eq "latest") {
        # No version specified: resolve latest release (including pre-releases)
        Write-Info "Resolving latest version..."
        try {
            $releases = Invoke-RestMethod "https://api.github.com/repos/$releaseRepo/releases?per_page=1"
            $resolvedVersion = $releases[0].tag_name
            if (-not $resolvedVersion) { throw "No releases found" }
        } catch {
            Write-Err "Failed to resolve latest version: $_"
            return
        }
    } else {
        # Specific version requested: verify the tag exists
        Write-Info "Verifying tag $resolvedVersion..."
        try {
            $null = Invoke-RestMethod "https://api.github.com/repos/$releaseRepo/releases/tags/$resolvedVersion"
        } catch {
            Write-Err "Release $resolvedVersion not found. Check available tags at: https://github.com/$releaseRepo/releases"
            return
        }
    }

    Write-Info "Installing nanosb $resolvedVersion"

    # --- Create install directory ---
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }
    Write-Info "Install directory: $InstallDir"

    # --- Windows Defender exclusion (must run BEFORE download) ---
    # Without this, Defender's ML heuristics frequently quarantine the freshly
    # downloaded nanosb.exe as a generic threat, leaving an empty install dir
    # and a confusing "command not recognized" error for the user.
    if (-not $SkipDefenderExclusion) {
        $defender = Get-MpComputerStatus -ErrorAction SilentlyContinue
        $defenderActive = $defender -and $defender.RealTimeProtectionEnabled
        if ($defenderActive) {
            $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

            $alreadyExcluded = $false
            try {
                $existing = (Get-MpPreference).ExclusionPath
                if ($existing -and ($existing -contains $InstallDir)) {
                    $alreadyExcluded = $true
                }
            } catch { }

            if ($alreadyExcluded) {
                Write-Info "$InstallDir already excluded from Windows Defender"
            } elseif (-not $isAdmin) {
                Write-Warn "Windows Defender real-time protection is active."
                Write-Warn "nanosb.exe is unsigned and may be flagged as a generic threat by Defender's ML heuristics."
                Write-Warn "To add an exclusion automatically, re-run this installer in an elevated (Administrator) PowerShell."
                Write-Warn "Or manually run, as Administrator:"
                Write-Warn "  Add-MpPreference -ExclusionPath '$InstallDir'"
                Write-Warn "  Add-MpPreference -ExclusionProcess 'nanosb.exe'"
                Write-Host ""
                $answer = Read-Host "  Continue without an exclusion (download may be quarantined)? [y/N]"
                if ($answer -notmatch '^[Yy]') {
                    Write-Info "Aborted by user. Re-run as Administrator to add the exclusion automatically."
                    return
                }
            } else {
                $consent = $AddDefenderExclusion
                if (-not $consent) {
                    Write-Warn "Windows Defender real-time protection is active."
                    Write-Warn "nanosb.exe is unsigned and may be flagged as a generic threat by Defender's ML heuristics."
                    Write-Warn "Adding a path exclusion for $InstallDir will prevent silent quarantine of the downloaded binary."
                    Write-Host ""
                    $answer = Read-Host "  Add Windows Defender exclusion for $InstallDir ? [Y/n]"
                    if ($answer -notmatch '^[Nn]') { $consent = $true }
                }
                if ($consent) {
                    try {
                        Add-MpPreference -ExclusionPath $InstallDir -ErrorAction Stop
                        Add-MpPreference -ExclusionProcess "nanosb.exe" -ErrorAction Stop
                        Write-Ok "Added Windows Defender exclusion: $InstallDir"
                        Write-Ok "Added Windows Defender process exclusion: nanosb.exe"
                    } catch {
                        Write-Warn "Failed to add Defender exclusion: $_"
                        Write-Warn "Proceeding anyway -- download may be quarantined."
                    }
                } else {
                    Write-Info "Skipped Defender exclusion -- download may be quarantined."
                }
            }
        }
    }

    # --- Download nanosb.exe ---
    $binaryName = "nanosb.exe"
    $downloadUrl = "https://github.com/$releaseRepo/releases/download/$resolvedVersion/$binaryName"
    $destPath = Join-Path $InstallDir "nanosb.exe"

    Write-Info "Downloading $binaryName..."
    try {
        Invoke-WebRequest -Uri $downloadUrl -OutFile $destPath -UseBasicParsing
        Write-Ok "Downloaded nanosb.exe"
    } catch {
        Write-Err "Failed to download nanosb.exe from $downloadUrl`n$_"
        return
    }

    # --- Install runtime dependencies ---
    Write-Info "Installing runtime dependencies..."
    # CLI and dependency installer scripts ship in the same release, so reuse
    # $resolvedVersion instead of re-querying the releases API.
    $depsTag = $resolvedVersion
    $depsUrl = "https://github.com/$releaseRepo/releases/download/$depsTag/install-deps.ps1"
    try {
        Write-Info "Fetching install-deps ($depsTag)..."
        $depsScript = Invoke-RestMethod $depsUrl
        # The script ends with `Install-NanosandboxDeps @args`, which would run
        # with empty $args here. Strip that auto-invocation so we can call the
        # function ourselves with the version pinned.
        $depsScript = $depsScript -replace 'Install-NanosandboxDeps\s+@args\s*$', ''
        Invoke-Expression $depsScript
        Install-NanosandboxDeps -Version $resolvedVersion -InstallDir $InstallDir
        Write-Ok "Runtime dependencies installed"
    } catch {
        Write-Warn "Failed to install runtime dependencies automatically: $_"
        Write-Warn "You may need to install them manually from: https://github.com/$releaseRepo/releases"
    }

    # --- Install default config files ---
    $configDest = Join-Path $InstallDir "agent_defaults.yaml"
    if (Test-Path $configDest) {
        Write-Info "agent_defaults.yaml already exists (keeping user config)"
    } else {
        $configUrl = "https://github.com/$releaseRepo/releases/download/$resolvedVersion/agent_defaults.yaml"
        try {
            Invoke-WebRequest -Uri $configUrl -OutFile $configDest -UseBasicParsing
            Write-Ok "Installed agent_defaults.yaml"
        } catch {
            Write-Warn "Could not download agent_defaults.yaml -- using built-in defaults"
        }
    }

    # --- Add to PATH ---
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
        # Also update the current session so the user doesn't need to open a new terminal
        $env:Path = "$env:Path;$InstallDir"
        Write-Ok "Added $InstallDir to user PATH (available immediately)"
    } else {
        Write-Info "$InstallDir already in PATH"
    }

    # --- Verify ---
    Write-Host ""
    Write-Ok "nanosb $resolvedVersion installed to $destPath"
    Write-Host ""
    Write-Host "  Get started:" -ForegroundColor White
    Write-Host "    nanosb doctor    # Check prerequisites" -ForegroundColor DarkGray
    Write-Host "    nanosb run       # Start a sandbox" -ForegroundColor DarkGray
    Write-Host ""
}

# Invoke the function - @args passes through any command-line parameters
Install-NanosandboxCLI @args

# AISmush Installer / Uninstaller for Windows
# Install:    irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex
# Uninstall:  aismush --uninstall
#   or:       & { irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex } --uninstall
$ErrorActionPreference = "Stop"

$Repo = "Skunk-Tech/aismush"
$Artifact = "aismush-windows-x86_64"
$InstallDir = "$env:LOCALAPPDATA\AISmush"
$Binary = "$InstallDir\aismush.exe"
$DataDir = "$env:USERPROFILE\.hybrid-proxy"

# ── Uninstall ──────────────────────────────────────────────────────────
if ($args -contains "--uninstall") {
    Write-Host ""
    Write-Host "  AISmush Uninstaller" -ForegroundColor Cyan
    Write-Host "  -------------------"
    Write-Host ""

    # Kill running proxy
    Get-Process -Name "aismush" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

    $removed = $false
    if (Test-Path $InstallDir) {
        Remove-Item -Recurse -Force $InstallDir
        Write-Host "  Removed: $InstallDir" -ForegroundColor Green
        $removed = $true
    }

    if (-not $removed) {
        Write-Host "  AISmush not found in $InstallDir"
    }

    # Remove from PATH
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -like "*$InstallDir*") {
        $NewPath = ($UserPath -split ";" | Where-Object { $_ -ne $InstallDir }) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "  Removed from PATH" -ForegroundColor Green
    }

    if (Test-Path $DataDir) {
        $confirm = Read-Host "  Delete data ($DataDir)? Includes database and memories. [y/N]"
        if ($confirm -eq "y" -or $confirm -eq "Y") {
            Remove-Item -Recurse -Force $DataDir
            Write-Host "  Removed: $DataDir" -ForegroundColor Green
        } else {
            Write-Host "  Kept: $DataDir"
        }
    }

    Write-Host ""
    Write-Host "  AISmush uninstalled." -ForegroundColor Green
    Write-Host ""
    exit
}

# ── Install ────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "  AISmush Installer" -ForegroundColor Cyan
Write-Host "  -----------------"
Write-Host ""

# Create install directory
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Get latest release download URL
$DownloadUrl = "https://github.com/$Repo/releases/latest/download/$Artifact.zip"
Write-Host "  Downloading: $DownloadUrl"

# Download and extract
$TmpDir = Join-Path $env:TEMP "aismush-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

try {
    $ZipFile = Join-Path $TmpDir "aismush.zip"
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ZipFile -UseBasicParsing

    Expand-Archive -Path $ZipFile -DestinationPath $TmpDir -Force

    # Stop any running aismush before overwriting
    Get-Process -Name "aismush" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    # Install binary
    Copy-Item (Join-Path $TmpDir "aismush.exe") -Destination $Binary -Force
    Write-Host "  Installed: $Binary" -ForegroundColor Green

    # Install start script alongside binary
    $StartScript = Join-Path $InstallDir "aismush-start.ps1"
    Copy-Item (Join-Path $TmpDir "start.ps1") -Destination $StartScript -Force -ErrorAction SilentlyContinue

    # Create .cmd wrapper so "aismush-start" works from cmd and PowerShell
    $CmdWrapper = Join-Path $InstallDir "aismush-start.cmd"
    '@echo off' + "`r`n" + 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0aismush-start.ps1" %*' | Set-Content $CmdWrapper -Encoding ASCII
} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}

# Add to user PATH if not already there
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$UserPath", "User")
    $env:Path = "$InstallDir;$env:Path"
    Write-Host "  Added to PATH: $InstallDir" -ForegroundColor Green
} else {
    Write-Host "  Already on PATH" -ForegroundColor Green
}

# Verify
Write-Host ""
try {
    $Ver = & $Binary --version 2>&1
    Write-Host "  Installed: $Ver" -ForegroundColor Green
} catch {
    Write-Host "  Binary installed but could not verify version" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "  Done! Open a NEW terminal and run:" -ForegroundColor Cyan
Write-Host ""
Write-Host "    aismush-start" -ForegroundColor White
Write-Host "      Sets up DeepSeek routing (saves ~90% on API costs)" -ForegroundColor Gray
Write-Host "      First run will ask for your DeepSeek API key (one time only)" -ForegroundColor Gray
Write-Host ""
Write-Host "    aismush-start --direct" -ForegroundColor White
Write-Host "      Uses Claude directly (no DeepSeek key needed)" -ForegroundColor Gray
Write-Host ""

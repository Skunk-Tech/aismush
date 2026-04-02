# AISmush Installer for Windows
# Usage: irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex
$ErrorActionPreference = "Stop"

$Repo = "Skunk-Tech/aismush"
$Artifact = "aismush-windows-x86_64"
$InstallDir = "$env:LOCALAPPDATA\AISmush"
$Binary = "$InstallDir\aismush.exe"

Write-Host ""
Write-Host "  AISmush Installer" -ForegroundColor Cyan
Write-Host "  -----------------"
Write-Host ""

# Create install directory
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Get latest release download URL
$DownloadUrl = "https://github.com/$Repo/releases/latest/download/$Artifact.tar.gz"
Write-Host "  Downloading: $DownloadUrl"

# Download and extract
$TmpDir = Join-Path $env:TEMP "aismush-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

try {
    $TarGz = Join-Path $TmpDir "aismush.tar.gz"
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TarGz -UseBasicParsing

    # Extract (tar is available on Windows 10+)
    tar xzf $TarGz -C $TmpDir

    # Stop any running aismush before overwriting
    Get-Process -Name "aismush" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    # Install binary
    Copy-Item (Join-Path $TmpDir "aismush.exe") -Destination $Binary -Force
    Write-Host "  Installed: $Binary" -ForegroundColor Green

    # Install start script alongside binary
    $StartScript = Join-Path $InstallDir "aismush-start.ps1"
    Copy-Item (Join-Path $TmpDir "start.ps1") -Destination $StartScript -Force -ErrorAction SilentlyContinue
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
Write-Host "    aismush" -ForegroundColor White
Write-Host ""
Write-Host "  Or use the start script:" -ForegroundColor Cyan
Write-Host ""
Write-Host "    aismush-start" -ForegroundColor White
Write-Host ""

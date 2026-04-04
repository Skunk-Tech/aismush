# AISmush - Hybrid Claude + DeepSeek Proxy (Windows)
$ErrorActionPreference = "Stop"

# Find binary: installed location first, then local directory
$BINARY = if (Get-Command "aismush.exe" -ErrorAction SilentlyContinue) {
    (Get-Command "aismush.exe").Source
} elseif (Test-Path "$env:LOCALAPPDATA\AISmush\aismush.exe") {
    "$env:LOCALAPPDATA\AISmush\aismush.exe"
} else {
    ".\aismush.exe"
}
$LOGDIR = "$env:USERPROFILE\.hybrid-proxy"
$LOGFILE = "$LOGDIR\proxy.log"
$PORT = if ($env:PROXY_PORT) { $env:PROXY_PORT } else { "1849" }

New-Item -ItemType Directory -Force -Path $LOGDIR | Out-Null

# Load config from multiple locations
$configPaths = @(".\config.json", ".\.deepseek-proxy.json", "$LOGDIR\config.json")
foreach ($cfg in $configPaths) {
    if ((Test-Path $cfg) -and -not $env:DEEPSEEK_API_KEY) {
        try {
            $config = Get-Content $cfg | ConvertFrom-Json
            if ($config.apiKey) { $env:DEEPSEEK_API_KEY = $config.apiKey }
        } catch {}
    }
}

# Parse --direct flag
$DirectMode = $false
$AISMUSH_FLAGS = ""
$ClaudeArgs = @()
foreach ($a in $args) {
    if ($a -eq "--direct") {
        $DirectMode = $true
        $AISMUSH_FLAGS = "--direct"
    } else {
        $ClaudeArgs += $a
    }
}

# Check if any provider is configured (skip in direct mode)
$HasOpenRouter = $false
$HasLocal = $false
$ConfigFile = "$LOGDIR\config.json"
if (Test-Path $ConfigFile) {
    try {
        $cfgData = Get-Content $ConfigFile | ConvertFrom-Json
        if ($cfgData.openrouterKey) { $HasOpenRouter = $true }
        if ($cfgData.local -and $cfgData.local.Count -gt 0) { $HasLocal = $true }
    } catch {}
}

$HasAnyProvider = ($env:DEEPSEEK_API_KEY -or $HasOpenRouter -or $HasLocal)

# First-time setup: offer interactive setup or quick DeepSeek key (skip in direct mode)
if (-not $DirectMode -and -not $HasAnyProvider) {
    Write-Host ""
    Write-Host "  AISmush - First Time Setup" -ForegroundColor Cyan
    Write-Host "  ──────────────────────────"
    Write-Host ""
    Write-Host "  No providers configured. Options:" -ForegroundColor White
    Write-Host ""
    Write-Host "    aismush --setup" -ForegroundColor White
    Write-Host "      Full interactive setup (DeepSeek, OpenRouter, local models)" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  Or paste a DeepSeek API key for quick start:" -ForegroundColor White
    Write-Host "  Get one at: https://platform.deepseek.com/api_keys" -ForegroundColor Gray
    Write-Host ""
    $key = Read-Host "  Paste DeepSeek key (or Enter for full setup)"
    if (-not $key) {
        # Run interactive setup
        & $BINARY --setup
        # Re-load config after setup
        if (Test-Path $ConfigFile) {
            try {
                $cfgData = Get-Content $ConfigFile | ConvertFrom-Json
                if ($cfgData.apiKey) { $env:DEEPSEEK_API_KEY = $cfgData.apiKey }
            } catch {}
        }
    } else {
        $env:DEEPSEEK_API_KEY = $key
        @{apiKey = $key} | ConvertTo-Json | Set-Content $ConfigFile
        Write-Host "  Key saved." -ForegroundColor Green
        Write-Host ""
    }
}

# Kill stale proxy
Get-NetTCPConnection -LocalPort $PORT -ErrorAction SilentlyContinue | ForEach-Object {
    Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "  AISmush - Hybrid Claude + DeepSeek Proxy" -ForegroundColor Cyan
Write-Host "  Port:      $PORT"
Write-Host "  Dashboard: http://localhost:$PORT/dashboard"
Write-Host "  Log:       $LOGFILE"
Write-Host ""

# Start proxy
$proxyArgs = if ($AISMUSH_FLAGS) { $AISMUSH_FLAGS } else { $null }
if ($proxyArgs) {
    $proxy = Start-Process -FilePath $BINARY -ArgumentList $proxyArgs -RedirectStandardOutput $LOGFILE -RedirectStandardError "$LOGDIR\proxy-err.log" -PassThru -WindowStyle Hidden
} else {
    $proxy = Start-Process -FilePath $BINARY -RedirectStandardOutput $LOGFILE -RedirectStandardError "$LOGDIR\proxy-err.log" -PassThru -WindowStyle Hidden
}
Start-Sleep -Milliseconds 500

if ($proxy.HasExited) {
    Write-Host "  Failed to start! Check $LOGFILE" -ForegroundColor Red
    exit 1
}

Write-Host "  Proxy started (PID $($proxy.Id))" -ForegroundColor Green

# Launch Claude Code
$env:ANTHROPIC_BASE_URL = "http://localhost:$PORT"
try {
    claude @ClaudeArgs
} finally {
    # Show stats
    try {
        $stats = Invoke-RestMethod "http://localhost:$PORT/stats" -ErrorAction SilentlyContinue
        if ($stats.total_requests -gt 0) {
            Write-Host ""
            Write-Host "  Session: $($stats.total_requests) requests (Claude: $($stats.claude_turns), DeepSeek: $($stats.deepseek_turns))"
            Write-Host "  Saved: `$$([math]::Round($stats.savings, 4)) ($([math]::Round($stats.savings_percent, 1))%)" -ForegroundColor Green
        }
    } catch {}
    Stop-Process -Id $proxy.Id -Force -ErrorAction SilentlyContinue
    Write-Host "  Proxy stopped."
}

# AISmush - Hybrid Claude + DeepSeek Proxy (Windows)
$ErrorActionPreference = "Stop"

$BINARY = ".\aismush.exe"
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

# First-time setup: ask for key if missing
if (-not $env:DEEPSEEK_API_KEY) {
    Write-Host ""
    Write-Host "  AISmush - First Time Setup" -ForegroundColor Cyan
    Write-Host "  ──────────────────────────"
    Write-Host ""
    Write-Host "  You need a DeepSeek API key (free tier available)."
    Write-Host "  Get one at: https://platform.deepseek.com/api_keys"
    Write-Host ""
    $key = Read-Host "  Paste your DeepSeek API key"
    if (-not $key) {
        Write-Host "  No key provided. Exiting." -ForegroundColor Red
        exit 1
    }
    $env:DEEPSEEK_API_KEY = $key
    # Save so they never have to do this again
    @{apiKey = $key} | ConvertTo-Json | Set-Content "$LOGDIR\config.json"
    Write-Host "  Key saved. You won't be asked again." -ForegroundColor Green
    Write-Host ""
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
$proxy = Start-Process -FilePath $BINARY -RedirectStandardOutput $LOGFILE -RedirectStandardError "$LOGDIR\proxy-err.log" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 500

if ($proxy.HasExited) {
    Write-Host "  Failed to start! Check $LOGFILE" -ForegroundColor Red
    exit 1
}

Write-Host "  Proxy started (PID $($proxy.Id))" -ForegroundColor Green

# Launch Claude Code
$env:ANTHROPIC_BASE_URL = "http://localhost:$PORT"
try {
    claude $args
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

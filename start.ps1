# AISmush - Hybrid Claude + DeepSeek Proxy (Windows)
$ErrorActionPreference = "Stop"

$BINARY = ".\aismush.exe"
$LOGDIR = "$env:USERPROFILE\.hybrid-proxy"
$LOGFILE = "$LOGDIR\proxy.log"
$PORT = if ($env:PROXY_PORT) { $env:PROXY_PORT } else { "1849" }

New-Item -ItemType Directory -Force -Path $LOGDIR | Out-Null

# Load config
if (Test-Path "config.json") {
    $config = Get-Content "config.json" | ConvertFrom-Json
    if ($config.apiKey -and -not $env:DEEPSEEK_API_KEY) {
        $env:DEEPSEEK_API_KEY = $config.apiKey
    }
}

if (-not $env:DEEPSEEK_API_KEY) {
    Write-Host "  No DeepSeek API key. Set DEEPSEEK_API_KEY or create config.json" -ForegroundColor Red
    exit 1
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

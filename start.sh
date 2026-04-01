#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"

BINARY="./aismush"
LOGDIR="$HOME/.hybrid-proxy"
LOGFILE="$LOGDIR/proxy.log"
mkdir -p "$LOGDIR"

# Build from source if binary doesn't exist
if [ ! -f "$BINARY" ] && [ -f "Cargo.toml" ]; then
    echo "Building AISmush (first time only)..."
    cargo build --release
    cp target/release/aismush .
fi

# Load config
if [ -f config.json ] && [ -z "$DEEPSEEK_API_KEY" ]; then
    export DEEPSEEK_API_KEY=$(python3 -c "import json;print(json.load(open('config.json')).get('apiKey',''),end='')" 2>/dev/null || true)
fi
if [ -f .deepseek-proxy.json ] && [ -z "$DEEPSEEK_API_KEY" ]; then
    export DEEPSEEK_API_KEY=$(python3 -c "import json;print(json.load(open('.deepseek-proxy.json')).get('apiKey',''),end='')" 2>/dev/null || true)
fi

PORT="${PROXY_PORT:-1849}"
lsof -ti:$PORT 2>/dev/null | xargs kill -9 2>/dev/null || true

echo ""
echo "  AISmush - Hybrid Claude + DeepSeek Proxy"
echo "  Port:      $PORT"
echo "  Dashboard: http://localhost:$PORT/dashboard"
echo "  Log:       $LOGFILE"
echo ""

if [ "$1" = "--proxy-only" ]; then
    echo "  ANTHROPIC_BASE_URL=http://localhost:$PORT claude"
    exec "$BINARY"
fi

"$BINARY" > "$LOGFILE" 2>&1 &
PROXY_PID=$!
sleep 0.3

if ! kill -0 $PROXY_PID 2>/dev/null; then
    echo "  Failed! Check $LOGFILE"
    tail -5 "$LOGFILE" 2>/dev/null
    exit 1
fi

cleanup() {
    echo ""
    curl -s "http://localhost:$PORT/stats" 2>/dev/null | python3 -c "
import sys,json
try:
    s=json.load(sys.stdin)
    t=s.get('total_requests',0)
    if t>0:
        print(f'  {t} requests (Claude:{s.get(\"claude_turns\",0)} DeepSeek:{s.get(\"deepseek_turns\",0)})')
        print(f'  Cost:\${s.get(\"actual_cost\",0):.4f} Saved:\${s.get(\"savings\",0):.4f} ({s.get(\"savings_percent\",0):.1f}%)')
except: pass
" 2>/dev/null || true
    kill $PROXY_PID 2>/dev/null; wait $PROXY_PID 2>/dev/null
    echo "  Stopped. Log: $LOGFILE"
}
trap cleanup EXIT INT TERM

ANTHROPIC_BASE_URL="http://localhost:$PORT" claude "$@" || true

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

# Parse flags
AISMUSH_FLAGS=""
AGENT_ARGS=()
FORCE_AGENT=""
for arg in "$@"; do
    if [ "$arg" = "--direct" ]; then
        AISMUSH_FLAGS="--direct"
    elif [ "$arg" = "--goose" ]; then
        FORCE_AGENT="goose"
    elif [ "$arg" = "--claude" ]; then
        FORCE_AGENT="claude"
    elif [ "$arg" = "--proxy-only" ]; then
        FORCE_AGENT="proxy-only"
    else
        AGENT_ARGS+=("$arg")
    fi
done

if [ -n "$AISMUSH_FLAGS" ]; then
    echo ""
    echo "  AISmush - Direct Mode (Claude only)"
    echo "  Compression, memory, agents, and tracking still active"
else
    echo ""
    echo "  AISmush - Smart Routing Mode"
fi
echo "  Port:      $PORT"
echo "  Dashboard: http://localhost:$PORT/dashboard"
echo "  Log:       $LOGFILE"
echo ""

# Check for update notification from previous run
UPDATE_FILE="$LOGDIR/update-available"
if [ -f "$UPDATE_FILE" ]; then
    NEW_VER=$(cat "$UPDATE_FILE" 2>/dev/null)
    echo "  ┌─────────────────────────────────────────────────┐"
    echo "  │  UPDATE AVAILABLE: v$NEW_VER                     "
    echo "  │  Run: aismush --upgrade                          "
    echo "  └─────────────────────────────────────────────────┘"
    echo ""
fi

# stdout → log, stderr passes through to terminal (for update notifications)
"$BINARY" $AISMUSH_FLAGS > "$LOGFILE" &
PROXY_PID=$!
sleep 0.5

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

# ── Detect and launch agent ─────────────────────────────────────────
HAS_CLAUDE=$(command -v claude >/dev/null 2>&1 && echo "yes" || echo "")
HAS_GOOSE=$(command -v goose >/dev/null 2>&1 && echo "yes" || echo "")

AGENT="$FORCE_AGENT"

if [ -z "$AGENT" ]; then
    if [ -n "$HAS_CLAUDE" ] && [ -n "$HAS_GOOSE" ]; then
        echo "  Found both Claude Code and Goose."
        echo ""
        echo "    1) Claude Code"
        echo "    2) Goose"
        echo ""
        read -p "  Launch which? [1/2]: " CHOICE
        case "$CHOICE" in
            2|goose|g) AGENT="goose" ;;
            *) AGENT="claude" ;;
        esac
    elif [ -n "$HAS_GOOSE" ]; then
        AGENT="goose"
    elif [ -n "$HAS_CLAUDE" ]; then
        AGENT="claude"
    else
        echo "  No AI agent found. Install Claude Code or Goose, then re-run."
        echo "  Or use: ./start.sh --proxy-only"
        kill $PROXY_PID 2>/dev/null
        exit 1
    fi
fi

case "$AGENT" in
    claude)
        echo "  Launching Claude Code..."
        echo ""
        ANTHROPIC_BASE_URL="http://localhost:$PORT" claude "${AGENT_ARGS[@]}" || true
        ;;
    goose)
        # Auto-configure Goose on first use (non-destructive)
        GOOSE_CFG="$HOME/.config/goose/config.yaml"
        if [ -f "$GOOSE_CFG" ]; then
            if ! grep -q "ANTHROPIC_HOST.*localhost.*$PORT" "$GOOSE_CFG" 2>/dev/null; then
                echo "  Configuring Goose to use AISmush..."
                cp "$GOOSE_CFG" "$GOOSE_CFG.pre-aismush"
                if grep -q "^ANTHROPIC_HOST:" "$GOOSE_CFG" 2>/dev/null; then
                    sed -i "s|^ANTHROPIC_HOST:.*|ANTHROPIC_HOST: http://localhost:$PORT|" "$GOOSE_CFG"
                else
                    echo "ANTHROPIC_HOST: http://localhost:$PORT" >> "$GOOSE_CFG"
                fi
                echo "  Done. (backup: $GOOSE_CFG.pre-aismush)"
            fi
        else
            mkdir -p "$(dirname "$GOOSE_CFG")"
            echo "ANTHROPIC_HOST: http://localhost:$PORT" > "$GOOSE_CFG"
        fi
        echo "  Launching Goose..."
        echo ""
        ANTHROPIC_HOST="http://localhost:$PORT" goose "${AGENT_ARGS[@]}" || true
        ;;
    proxy-only)
        echo "  Proxy running on http://localhost:$PORT"
        echo "  Dashboard: http://localhost:$PORT/dashboard"
        echo ""
        echo "  Connect any agent:"
        echo "    Claude Code:  aismush-start --claude"
        echo "    Goose:        aismush-start --goose"
        echo ""
        echo "  Press Ctrl+C to stop."
        wait $PROXY_PID
        ;;
esac

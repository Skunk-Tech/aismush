#!/usr/bin/env bash
set -e

# AISmush Installer / Uninstaller
# Install:    curl -fsSL .../install.sh | bash
# Uninstall:  curl -fsSL .../install.sh | bash -s -- --uninstall
#   or:       aismush --uninstall

VERSION="${AISMUSH_VERSION:-latest}"
INSTALL_DIR="${AISMUSH_INSTALL_DIR:-$HOME/.local/bin}"
REPO="Skunk-Tech/aismush"
DATA_DIR="$HOME/.hybrid-proxy"

# ── Uninstall ──────────────────────────────────────────────────────────
if [ "$1" = "--uninstall" ]; then
    echo ""
    echo "  AISmush Uninstaller"
    echo "  ───────────────────"
    echo ""

    # Kill running proxy
    if command -v lsof &>/dev/null; then
        lsof -ti:1849 2>/dev/null | xargs kill 2>/dev/null || true
    fi

    removed=0
    for f in "$INSTALL_DIR/aismush" "$INSTALL_DIR/aismush-start"; do
        if [ -f "$f" ]; then
            rm -f "$f"
            echo "  Removed: $f"
            removed=1
        fi
    done

    if [ $removed -eq 0 ]; then
        echo "  AISmush binaries not found in $INSTALL_DIR"
    fi

    if [ -d "$DATA_DIR" ]; then
        echo ""
        read -p "  Delete data ($DATA_DIR)? Includes database and memories. [y/N]: " confirm
        if [ "$confirm" = "y" ] || [ "$confirm" = "Y" ]; then
            rm -rf "$DATA_DIR"
            echo "  Removed: $DATA_DIR"
        else
            echo "  Kept: $DATA_DIR"
        fi
    fi

    echo ""
    echo "  AISmush uninstalled."
    echo ""
    exit 0
fi

# ── Install ────────────────────────────────────────────────────────────
echo ""
echo "  AISmush Installer"
echo "  ─────────────────"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)      echo "  Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH_TAG="x86_64" ;;
    aarch64|arm64) ARCH_TAG="arm64" ;;
    *)             echo "  Unsupported architecture: $ARCH"; exit 1 ;;
esac

ARTIFACT="aismush-${PLATFORM}-${ARCH_TAG}"
echo "  Platform: ${PLATFORM}-${ARCH_TAG}"

# Get download URL
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ARTIFACT}.tar.gz"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT}.tar.gz"
fi

echo "  Downloading: $DOWNLOAD_URL"

# Create install dir
mkdir -p "$INSTALL_DIR"

# Download and extract
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

curl -fsSL "$DOWNLOAD_URL" -o "$TMPDIR/aismush.tar.gz"
tar xzf "$TMPDIR/aismush.tar.gz" -C "$TMPDIR"

# Install binary (remove first to handle "Text file busy" when upgrading)
rm -f "$INSTALL_DIR/aismush" 2>/dev/null
cp "$TMPDIR/aismush" "$INSTALL_DIR/aismush"
chmod +x "$INSTALL_DIR/aismush"

echo "  Installed: $INSTALL_DIR/aismush"

# Check if install dir is on PATH
if ! echo "$PATH" | tr ':' '\n' | grep -q "^${INSTALL_DIR}$"; then
    echo ""
    echo "  Add to your PATH by adding this to ~/.bashrc or ~/.zshrc:"
    echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
    echo ""
fi

# Create wrapper script that handles everything
cat > "$INSTALL_DIR/aismush-start" << 'WRAPPER'
#!/usr/bin/env bash
set -e

LOGDIR="$HOME/.hybrid-proxy"
LOGFILE="$LOGDIR/proxy.log"
CONFIG="$LOGDIR/config.json"
PORT="${PROXY_PORT:-1849}"
mkdir -p "$LOGDIR"

# Load config from multiple locations
for cfg in "$PWD/config.json" "$PWD/.deepseek-proxy.json" "$CONFIG"; do
    if [ -f "$cfg" ] && [ -z "$DEEPSEEK_API_KEY" ]; then
        export DEEPSEEK_API_KEY=$(python3 -c "import json;print(json.load(open('$cfg')).get('apiKey',''),end='')" 2>/dev/null || true)
    fi
done

# If no key found anywhere, ask the user (skip in direct mode)
if [ -z "$DEEPSEEK_API_KEY" ] && ! echo "$@" | grep -q "\-\-direct"; then
    echo ""
    echo "  AISmush - First Time Setup"
    echo "  ──────────────────────────"
    echo ""
    echo "  You need a DeepSeek API key (free tier available)."
    echo "  Get one at: https://platform.deepseek.com/api_keys"
    echo ""
    read -p "  Paste your DeepSeek API key: " DEEPSEEK_API_KEY
    echo ""

    if [ -z "$DEEPSEEK_API_KEY" ]; then
        echo "  No key provided. Exiting."
        exit 1
    fi

    # Validate the key
    echo "  Testing key..."
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -X POST https://api.deepseek.com/v1/chat/completions \
        -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
        -H "Content-Type: application/json" \
        -d '{"model":"deepseek-chat","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}' \
        2>/dev/null)

    if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "429" ]; then
        echo "  Key is valid!"
    else
        echo "  Key validation failed (HTTP $HTTP_CODE)."
        echo "  Check your key at https://platform.deepseek.com/api_keys"
        read -p "  Save anyway? (y/n): " SAVE_ANYWAY
        if [ "$SAVE_ANYWAY" != "y" ]; then
            exit 1
        fi
    fi

    # Save it so they never have to do this again
    echo "{\"apiKey\":\"$DEEPSEEK_API_KEY\"}" > "$CONFIG"
    export DEEPSEEK_API_KEY
    echo "  Key saved to $CONFIG"
    echo ""
fi

# Handle --direct flag
AISMUSH_FLAGS=""
CLAUDE_ARGS=()
for arg in "$@"; do
    if [ "$arg" = "--direct" ]; then
        AISMUSH_FLAGS="--direct"
    else
        CLAUDE_ARGS+=("$arg")
    fi
done

# Kill stale proxy
lsof -ti:$PORT 2>/dev/null | xargs kill -9 2>/dev/null || true

# Start proxy silently
aismush $AISMUSH_FLAGS > "$LOGFILE" 2>&1 &
PROXY_PID=$!
sleep 0.3

if ! kill -0 $PROXY_PID 2>/dev/null; then
    echo "  AISmush failed to start. Check $LOGFILE"
    tail -3 "$LOGFILE" 2>/dev/null
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
        print(f'  AISmush: {t} requests (Claude:{s.get(\"claude_turns\",0)} DS:{s.get(\"deepseek_turns\",0)}) Saved:\${s.get(\"savings\",0):.4f} ({s.get(\"savings_percent\",0):.1f}%)')
except: pass
" 2>/dev/null || true
    kill $PROXY_PID 2>/dev/null; wait $PROXY_PID 2>/dev/null
}
trap cleanup EXIT INT TERM

ANTHROPIC_BASE_URL="http://localhost:$PORT" claude "${CLAUDE_ARGS[@]}" || true
WRAPPER

chmod +x "$INSTALL_DIR/aismush-start"

echo ""
echo "  Done! Run from any directory:"
echo ""
echo "    aismush-start"
echo ""
echo "  First run will ask for your DeepSeek API key (one time only)."
echo ""

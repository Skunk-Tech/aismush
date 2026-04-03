#!/usr/bin/env bash
# AISmush Release Script
# Usage: ./release.sh 0.4.3 "Description of changes"
set -e

# Load tokens from .env if present
if [ -f "$(dirname "$0")/.env" ]; then
    set -a
    source "$(dirname "$0")/.env"
    set +a
fi

VERSION="$1"
DESC="$2"
GITHUB_TOKEN="${GITHUB_TOKEN:-}"
CF_TOKEN="${CLOUDFLARE_API_TOKEN:-}"

if [ -z "$GITHUB_TOKEN" ]; then
    echo "Set GITHUB_TOKEN env var first"
    exit 1
fi

if [ -z "$VERSION" ] || [ -z "$DESC" ]; then
    echo "Usage: ./release.sh <version> <description>"
    echo "Example: ./release.sh 0.4.3 'Fix UTF-8 panic, split savings dashboard'"
    exit 1
fi

echo ""
echo "═══════════════════════════════════════════"
echo "  AISmush Release v${VERSION}"
echo "═══════════════════════════════════════════"
echo ""

# 1. Version bump
echo "[1/8] Version bump..."
sed -i "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml
echo "  Cargo.toml → ${VERSION}"

# 2. Build
echo "[2/8] Building..."
source ~/.cargo/env 2>/dev/null
cargo build --release 2>&1 | tail -1
cp target/release/aismush ~/.local/bin/

# 3. Tests
echo "[3/8] Testing..."
cargo test 2>&1 | grep "test result"

# 4. Git commit + push
echo "[4/8] Committing and pushing..."
git add -A
git commit -m "v${VERSION}: ${DESC}

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
git push "https://${GITHUB_TOKEN}@github.com/Skunk-Tech/aismush.git" main

# 5. Tag + release
echo "[5/8] Tagging v${VERSION}..."
git tag "v${VERSION}"
git push "https://${GITHUB_TOKEN}@github.com/Skunk-Tech/aismush.git" "v${VERSION}"

# 6. Deploy website
echo "[6/8] Deploying website..."
CLOUDFLARE_API_TOKEN="$CF_TOKEN" npx wrangler pages deploy /home/developer/aismush-web/site --project-name aismush-site 2>&1 | tail -1

# 7. Deploy worker (if changed)
echo "[7/8] Deploying worker..."
CLOUDFLARE_API_TOKEN="$CF_TOKEN" npx wrangler deploy 2>&1 | tail -1 || echo "  (no worker changes)"

# 8. Verify
echo "[8/8] Verifying..."
echo "  Binary: $(aismush --version)"
echo "  GitHub: https://github.com/Skunk-Tech/aismush/releases/tag/v${VERSION}"
echo "  Website: https://aismush.us.com"
echo "  Actions: https://github.com/Skunk-Tech/aismush/actions"
echo ""
echo "  Release v${VERSION} complete!"
echo ""

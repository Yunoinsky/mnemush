#!/bin/bash
# Install the mneme binary and the agent adapters.
# Usage: ./scripts/install.sh [--dev]
set -e

# Always operate from the project root, regardless of where the script was called from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

DEV=0
if [[ "$1" == "--dev" ]]; then DEV=1; fi

echo "→ Building mneme binary (Rust)..."
cargo build --release --manifest-path crates/mneme/Cargo.toml

BIN_PATH="crates/mneme/target/release/mneme-mcp"
if [[ ! -f "$BIN_PATH" ]]; then
    echo "ERROR: build did not produce $BIN_PATH"
    exit 1
fi

echo "→ Installing mneme-mcp to ~/.cargo/bin..."
mkdir -p ~/.cargo/bin
cp "$BIN_PATH" ~/.cargo/bin/mneme-mcp
cp crates/mneme/target/release/mneme ~/.cargo/bin/mneme 2>/dev/null || true
chmod +x ~/.cargo/bin/mneme-mcp ~/.cargo/bin/mneme

echo "→ Initializing ~/.mneme/..."
./crates/mneme/target/release/mneme init

echo "→ Building TS packages..."
npm run build --workspaces --if-present

echo ""
echo "✓ Done."
echo ""
echo "Next steps:"
echo "  - For Pi:   pi install npm:mneme-pi"
echo "  - For OpenCode:"
echo "      mkdir -p ~/.config/opencode/plugin"
echo "      ln -sf \"$PROJECT_ROOT/packages/mneme-opencode/dist/index.js\" \\"
echo "             ~/.config/opencode/plugin/mneme.js"
echo "  - Verify:   mneme --version"
echo "              mneme stats"

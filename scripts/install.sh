#!/bin/bash
# Install the mnemush binary and the agent adapters.
# Usage: ./scripts/install.sh [--dev]
set -e

# Always operate from the project root, regardless of where the script was called from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

DEV=0
if [[ "$1" == "--dev" ]]; then DEV=1; fi

echo "→ Building mnemush binary (Rust)..."
cargo build --release --manifest-path crates/mnemush/Cargo.toml

BIN_PATH="crates/mnemush/target/release/mnemush-mcp"
if [[ ! -f "$BIN_PATH" ]]; then
    echo "ERROR: build did not produce $BIN_PATH"
    exit 1
fi

echo "→ Installing mnemush-mcp to ~/.cargo/bin..."
mkdir -p ~/.cargo/bin
cp "$BIN_PATH" ~/.cargo/bin/mnemush-mcp
cp crates/mnemush/target/release/mnemush ~/.cargo/bin/mnemush 2>/dev/null || true
chmod +x ~/.cargo/bin/mnemush-mcp ~/.cargo/bin/mnemush

# macOS: ad-hoc re-sign after `cp`. The cp strips the original signature
# from the built binary, and macOS will SIGKILL an unsigned binary
# when it's launched (`mnemush-mcp` silently exits with broken pipe on
# every attempt). Ignore failure for non-macOS or missing codesign.
if [[ "$(uname -s)" == "Darwin" ]] && command -v codesign >/dev/null 2>&1; then
    codesign --force --sign - ~/.cargo/bin/mnemush-mcp 2>/dev/null || true
    codesign --force --sign - ~/.cargo/bin/mnemush 2>/dev/null || true
fi

echo "→ Initializing ~/.mnemush/..."
./crates/mnemush/target/release/mnemush init

echo "→ Building TS packages..."
npm run build --workspaces --if-present

echo ""
echo "✓ Done."
echo ""
echo "Next steps:"
echo "  - For Pi:   pi install npm:mnemush-pi"
echo "  - For OpenCode:"
echo "      mkdir -p ~/.config/opencode/plugin"
echo "      ln -sf \"$PROJECT_ROOT/packages/mnemush-opencode/dist/index.js\" \\"
echo "             ~/.config/opencode/plugin/mnemush.js"
echo "  - Verify:   mnemush --version"
echo "              mnemush stats"

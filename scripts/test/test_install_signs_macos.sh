#!/bin/bash
# TDD test: install.sh must re-sign binaries on macOS after `cp`.
#
# Problem: macOS kills binaries that were copied (not signed) with
# `cp`. AGENTS.md says: "After any `cp` of `~/.cargo/bin/mnemush*`,
# re-sign with `codesign --force --sign -`".
# Without this fix, `mnemush-mcp` launched right after install.sh on
# macOS gets SIGKILL'd (exit 137 / broken pipe) and the smoke test
# hangs. Users have to re-sign by hand.

set -e

INSTALL_SH="$(dirname "$0")/../install.sh"
[ -f "$INSTALL_SH" ] || { echo "FAIL: install.sh not found at $INSTALL_SH"; exit 1; }

FAIL=0

# 1. Must contain the codesign command
if ! grep -q "codesign" "$INSTALL_SH"; then
    echo "FAIL: install.sh does not call 'codesign' anywhere"
    FAIL=1
fi

# 2. The codesign call must use --force --sign - (ad-hoc re-sign)
if ! grep -q "codesign.*--force.*--sign" "$INSTALL_SH"; then
    echo "FAIL: codesign invocation missing '--force --sign -'"
    FAIL=1
fi

# 3. The codesign block must be guarded by a Darwin/macOS check so
#    Linux/CI runs aren't broken (codesign doesn't exist there).
if ! grep -qE "Darwin|uname.*[Mm]ac" "$INSTALL_SH"; then
    echo "FAIL: codesign block is not guarded by macOS check (Linux CI would break)"
    FAIL=1
fi

if [ $FAIL -eq 0 ]; then
    echo "PASS: install.sh has macOS-aware codesign"
    exit 0
fi
exit 1

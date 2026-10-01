#!/usr/bin/env bash
#
# Builds the macOS bundle (Droidlog.app and a .dmg) on a Mac.
#
#   bash scripts/build-macos.sh              # host architecture
#   bash scripts/build-macos.sh universal    # one .dmg for Intel + Apple silicon
#
# Prerequisites:
#   xcode-select --install                   # command line tools
#   curl https://sh.rustup.rs -sSf | sh      # rustup + stable toolchain
#   npm i -g pnpm
# For the universal build both targets must be installed:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
#
# The bundle carries the macOS `adb` (src-tauri/platform-tools-mac/adb), renamed
# into `platform-tools/adb` by tauri.macos.conf.json, so no SDK is needed at run
# time. Built locally it is not quarantined, so Gatekeeper runs it as-is; if the
# .app is ever distributed as a download, users get the usual "unidentified
# developer" prompt (right-click → Open) unless it is signed and notarised.
#
# Google ships this adb for both architectures; on Apple silicon an Intel-only
# binary still runs through Rosetta 2.

set -euo pipefail
cd "$(dirname "$0")/.."

command -v pnpm >/dev/null 2>&1 || { echo "pnpm is required: npm i -g pnpm"; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "cargo is required: https://rustup.rs"; exit 1; }
xcode-select -p >/dev/null 2>&1 || { echo "Xcode command line tools are required: xcode-select --install"; exit 1; }

if [ -f src-tauri/platform-tools-mac/adb ] && [ ! -x src-tauri/platform-tools-mac/adb ]; then
  echo "note: chmod +x src-tauri/platform-tools-mac/adb"
  chmod +x src-tauri/platform-tools-mac/adb
fi

[ -d node_modules ] || pnpm install

if [ "${1:-host}" = "universal" ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  pnpm tauri build --target universal-apple-darwin
  OUT="src-tauri/target/universal-apple-darwin/release/bundle"
else
  pnpm tauri build
  OUT="src-tauri/target/release/bundle"
fi

echo
echo "=== artifacts ==="
ls -1sh "$OUT"/macos/*.app 2>/dev/null || true
ls -1sh "$OUT"/dmg/*.dmg 2>/dev/null || true
echo
echo "Portable: copy the .app out of the bundle; it already contains platform-tools/."

#!/usr/bin/env bash
#
# Builds the Linux bundles (.deb and .AppImage) on a Linux machine.
#
#   bash scripts/build-linux.sh
#
# System packages (Debian/Ubuntu; Tauri's own prerequisites, nothing extra):
#   sudo apt update
#   sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
#     libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
# Fedora: sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file \
#     libappindicator-gtk3-devel librsvg2-devel && sudo dnf group install "C Development Tools and Libraries"
#
# The resulting bundles carry the Linux `adb` (src-tauri/platform-tools-linux/adb),
# renamed into `platform-tools/adb` by tauri.linux.conf.json, so the app finds its
# own adb without an SDK — the same contract as on Windows.
#
# Note: AppImage is built with FUSE; on a machine without `fusermount` the build
# still succeeds (it falls back to extraction), but running the AppImage needs
# FUSE 2 (`sudo apt install libfuse2`).

set -euo pipefail
cd "$(dirname "$0")/.."

command -v pnpm >/dev/null 2>&1 || { echo "pnpm is required: npm i -g pnpm"; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "cargo is required: https://rustup.rs (stable toolchain)"; exit 1; }

# A missing executable bit on the bundled adb is repaired at runtime by
# `locate::discover` (best-effort chmod), so a checkout that lost the bit still works.
if [ -f src-tauri/platform-tools-linux/adb ] && [ ! -x src-tauri/platform-tools-linux/adb ]; then
  echo "note: chmod +x src-tauri/platform-tools-linux/adb"
  chmod +x src-tauri/platform-tools-linux/adb
fi

[ -d node_modules ] || pnpm install
pnpm tauri build

echo
echo "=== artifacts ==="
ls -1sh src-tauri/target/release/bundle/deb/*.deb 2>/dev/null || true
ls -1sh src-tauri/target/release/bundle/appimage/*.AppImage 2>/dev/null || true
echo
echo "Portable: copy src-tauri/target/release/droidlog plus"
echo "          src-tauri/platform-tools-linux/ (as platform-tools/) into one folder."

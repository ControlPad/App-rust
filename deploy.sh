#!/usr/bin/env bash
#
# Build all Slidr artifacts, package the Windows MSI, publish them to the
# nginx web root, and write an authoritative build-info.json the landing
# page reads for its "Last build" stamp.
#
# Usage:  ./deploy.sh [--skip-windows]
#
set -euo pipefail
cd "$(dirname "$0")"

WEBROOT="/var/www/slidr"
WIN_TARGET="x86_64-pc-windows-gnu"
SKIP_WINDOWS=0
[[ "${1:-}" == "--skip-windows" ]] && SKIP_WINDOWS=1

# Make cargo available in non-interactive shells.
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

log() { printf '\033[1;35m▶ %s\033[0m\n' "$*"; }

BUILD_TIME_UTC="$(date -u +'%Y-%m-%d %H:%M UTC')"
BUILD_EPOCH="$(date -u +%s)"

# ── Build ────────────────────────────────────────────────────────────────
log "Building Linux release"
cargo build --release

if [[ "$SKIP_WINDOWS" -eq 0 ]]; then
  log "Cross-compiling Windows release ($WIN_TARGET)"
  cargo build --release --target "$WIN_TARGET"

  log "Packaging MSI (wixl)"
  cp "target/$WIN_TARGET/release/slidr.exe" installer/
  ( cd installer && wixl -o Slidr-Setup.msi slidr.wxs 2>&1 | grep -iv 'checksum\|GLib' || true )
fi

# ── Publish ──────────────────────────────────────────────────────────────
log "Publishing to $WEBROOT"
mkdir -p "$WEBROOT"
install -m644 target/release/slidr            "$WEBROOT/slidr-linux-x86_64"
if [[ "$SKIP_WINDOWS" -eq 0 ]]; then
  install -m644 installer/Slidr-Setup.msi      "$WEBROOT/Slidr-Setup.msi"
  install -m644 "target/$WIN_TARGET/release/slidr.exe" "$WEBROOT/slidr.exe"
fi

# ── Build-info ───────────────────────────────────────────────────────────
size_of() { [[ -f "$1" ]] && stat -c%s "$1" || echo 0; }
cat > "$WEBROOT/build-info.json" <<EOF
{
  "build_time": "$BUILD_TIME_UTC",
  "build_epoch": $BUILD_EPOCH,
  "artifacts": {
    "msi":   { "name": "Slidr-Setup.msi",     "bytes": $(size_of "$WEBROOT/Slidr-Setup.msi") },
    "exe":   { "name": "slidr.exe",           "bytes": $(size_of "$WEBROOT/slidr.exe") },
    "linux": { "name": "slidr-linux-x86_64",  "bytes": $(size_of "$WEBROOT/slidr-linux-x86_64") }
  }
}
EOF

log "Done — build $BUILD_TIME_UTC"
ls -lh "$WEBROOT"/{Slidr-Setup.msi,slidr.exe,slidr-linux-x86_64} 2>/dev/null || true
echo "Reload nginx not required (static files). Page: http://100.118.79.15/"

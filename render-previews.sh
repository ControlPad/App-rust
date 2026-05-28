#!/usr/bin/env bash
# Render each app view to a PNG by running the demo binary under xvfb-run.
set -euo pipefail
cd "$(dirname "$0")"

OUT="${1:-/tmp/slidr-preview}"
mkdir -p "$OUT"
BIN="./target/debug/slidr"
[[ -x "$BIN" ]] || { echo "binary not built; run cargo build first"; exit 1; }

snap() {
  local name="$1"; shift
  echo "▶ $name ($*)"
  cat >/tmp/slidr-snap.sh <<EOF
#!/usr/bin/env bash
set -e
SLINT_BACKEND=winit-femtovg LIBGL_ALWAYS_SOFTWARE=1 \
  SLIDR_CONFIG_DIR=/tmp/slidr-demo-config \
  "$BIN" --demo "\$@" >/tmp/slidr-stderr.log 2>&1 &
APP=\$!
sleep 2.5
import -window root "$OUT/$name.png" || true
kill \$APP 2>/dev/null || true
wait 2>/dev/null || true
EOF
  chmod +x /tmp/slidr-snap.sh
  xvfb-run -a --server-args="-screen 0 1300x820x24" /tmp/slidr-snap.sh "$@" || true
  if [[ -s "$OUT/$name.png" ]]; then
    ls -l "$OUT/$name.png"
  else
    echo "  ! no snapshot"
    tail -5 /tmp/slidr-stderr.log || true
  fi
}

snap home-connected     --page=0
snap home-disconnected  --page=0 --disconnected
snap home-edit          --page=0 --edit
snap home-collapsed     --page=0 --collapsed
snap home-collapsed-dark --page=0 --collapsed --dark
snap home-dark          --page=0 --dark
snap sliders            --page=1
snap buttons            --page=2
snap settings           --page=3
snap settings-curve     --page=3 --curve-custom
snap settings-dark      --page=3 --dark
snap assign-popup       --page=0 --popup=assign
snap preset-popup       --page=0 --popup=preset
snap wizard-step0       --page=2 --popup=wizard0
snap wizard-step1       --page=2 --popup=wizard1
snap wizard-step2       --page=2 --popup=wizard2

echo "Done → $OUT"
ls -1 "$OUT"

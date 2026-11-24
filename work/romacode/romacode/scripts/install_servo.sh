#!/usr/bin/env bash
# Install the Servo browser engine used by RomaCode's `browser` tool.
#
# RomaCode ships the WebDriver client/controller, not the engine itself. This
# script downloads the official Linux nightly from servo.org, verifies its
# published SHA-256, and installs it where the brain already looks:
#   ~/.local/share/romacode/servo/bin/{servo,servoshell}
#   ~/.local/share/romacode/servo/resources/...
#
# Usage:
#   bash scripts/install_servo.sh              # install / refresh
#   SERVO_PLATFORM=mac bash scripts/install_servo.sh
#   SERVE_DL_ONLY=1 bash scripts/install_servo.sh   # download to /tmp only
set -euo pipefail

BASE="https://download.servo.org/nightly"
PLATFORM="${SERVO_PLATFORM:-linux}"
case "$PLATFORM" in
  linux)       FILE="servo-x86_64-linux-gnu.tar.gz" ;;
  linux-aarch64) FILE="servo-aarch64-linux-gnu.tar.gz" ;;
  mac)         FILE="servo-x86_64-apple-darwin.dmg" ;;
  mac-arm64)   FILE="servo-aarch64-apple-darwin.dmg" ;;
  windows)     FILE="servo-x86_64-windows-msvc.zip" ;;
  *) echo "unknown SERVO_PLATFORM: $PLATFORM" >&2; exit 2 ;;
esac

URL="$BASE/$PLATFORM/$FILE"
WORK="${TMPDIR:-/tmp}/romacode-servo-download"
mkdir -p "$WORK"

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

say "Downloading $URL"
curl -fL --retry 3 -m 600 "$URL" -o "$WORK/$FILE"

say "Verifying checksum"
curl -fsSL -m 60 "$URL.sha256" -o "$WORK/$FILE.sha256"
( cd "$WORK" && sha256sum -c "$FILE.sha256" )

if [ "${SERVE_DL_ONLY:-0}" = "1" ]; then
  echo "Downloaded to $WORK/$FILE"
  exit 0
fi

DEST="${XDG_DATA_HOME:-$HOME/.local/share}/romacode/servo"
mkdir -p "$DEST/bin" "$DEST/resources"

if [[ "$FILE" == *.tar.gz ]]; then
  say "Extracting"
  rm -rf "$WORK/extracted"
  mkdir -p "$WORK/extracted"
  tar xzf "$WORK/$FILE" -C "$WORK/extracted"
  BIN="$(find "$WORK/extracted" -type f -name servoshell -perm -u+x | head -1)"
  [ -n "$BIN" ] || { echo "servoshell not found in archive" >&2; exit 1; }
  install -m 0755 "$BIN" "$DEST/bin/servoshell"
  ln -sf "$DEST/bin/servoshell" "$DEST/bin/servo"
  ROOT="$(dirname "$BIN")"
  if [ -d "$ROOT/resources" ]; then
    cp -a "$ROOT/resources/." "$DEST/resources/"
  fi
else
  echo "Only the Linux .tar.gz layout is automated here; install $FILE manually." >&2
  exit 1
fi

say "Installed"
ls -lh "$DEST/bin"
echo
echo "RomaCode resolves the engine in this order:"
echo "  browser.binary config -> ROMACODE_SERVO env -> $DEST/bin -> PATH"
echo "Smoke test:"
echo "  ROMACODE_SERVO=$DEST/bin/servoshell cargo test -p romacode-server --test browser_live -- --ignored --nocapture"

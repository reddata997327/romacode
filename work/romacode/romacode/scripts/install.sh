#!/usr/bin/env bash
# RomaCode installer — Kali Linux / Debian / Ubuntu
#
#   bash scripts/install.sh              # everything: deps + rust + build + install
#   bash scripts/install.sh --no-apt     # skip apt (deps already installed)
#   bash scripts/install.sh --no-adb     # skip the Android platform-tools (adb)
#   bash scripts/install.sh --build      # force a cargo rebuild even if bin/ has prebuilts
#
# If the archive ships prebuilt Linux x86_64 binaries in bin/, they are
# installed directly (no compile). Otherwise it builds from source.
#
# Installs up to three binaries:
#   romacode-server   the brain   (agent loop, langchain engine, tools, security gate)
#   romacode          the screen  (TUI client that connects to the server)
#   rustango          optional    (vendored Django-style web framework CLI)
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DO_APT=1
DO_ADB=1
FORCE_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --no-apt) DO_APT=0 ;;
    --no-adb) DO_ADB=0 ;;
    --build) FORCE_BUILD=1 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

say()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. System packages (Kali is Debian-based, so apt)
# ---------------------------------------------------------------------------
APT_PKGS=(build-essential pkg-config git curl ca-certificates file
          libx11-dev libxcursor-dev libxrandr-dev libxi-dev
          libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev)
ADB_PKGS=(adb)

if [ "$DO_APT" -eq 1 ]; then
  if command -v apt-get >/dev/null 2>&1; then
    say "Installing system packages (apt)"
    SUDO=""
    if [ "$(id -u)" -ne 0 ]; then
      command -v sudo >/dev/null 2>&1 || { warn "sudo not found; run this script as root or install sudo"; exit 1; }
      SUDO="sudo"
    fi
    $SUDO apt-get update -y
    $SUDO apt-get install -y "${APT_PKGS[@]}"
    if [ "$DO_ADB" -eq 1 ]; then
      $SUDO apt-get install -y "${ADB_PKGS[@]}" || warn "adb not installed — Android panel needs platform-tools (install later: apt install adb)"
    fi
  else
    warn "apt-get not found — install build-essential, pkg-config, git, curl and X11 dev libs manually"
  fi
fi

# ---------------------------------------------------------------------------
# 2. Rust toolchain (rustup; Kali's apt rustc is often older than 1.80)
# ---------------------------------------------------------------------------
NEED_RUSTUP=1
if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
  MAJOR=$(rustc --version | awk '{print $2}' | cut -d. -f1)
  MINOR=$(rustc --version | awk '{print $2}' | cut -d. -f2)
  if [ "$MAJOR" -gt 1 ] || [ "$MINOR" -ge 80 ]; then
    NEED_RUSTUP=0
    say "Using existing $(rustc --version)"
  else
    warn "rustc $(rustc --version | awk '{print $2}') is too old (need >= 1.80) — installing rustup"
  fi
fi

if [ "$NEED_RUSTUP" -eq 1 ]; then
  say "Installing Rust via rustup (stable, minimal)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain stable --profile minimal
  # make cargo visible right now
  export PATH="$HOME/.cargo/bin:$PATH"
fi
export PATH="$HOME/.cargo/bin:$PATH"

command -v cargo >/dev/null 2>&1 || { echo "cargo still not on PATH — open a new shell and re-run" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 3. Prebuilt binaries (bin/) or build from source
# ---------------------------------------------------------------------------
PREBUILT="$REPO_DIR/bin"
if [ "$FORCE_BUILD" -eq 0 ] && [ -x "$PREBUILT/romacode-server" ] && [ -x "$PREBUILT/romacode" ]; then
  say "Using the prebuilt binaries shipped in bin/ (no compile needed)"
  if command -v file >/dev/null 2>&1 && ! file "$PREBUILT/romacode" | grep -q 'x86-64\|x86_64'; then
    warn "prebuilt binaries are not x86_64 — falling back to a source build"
  else
    BIN_SRC="$PREBUILT"
    SKIP_BUILD=1
  fi
fi

if [ "${SKIP_BUILD:-0}" -ne 1 ]; then
  if [ "$DO_APT" -eq 0 ] && [ "$NEED_RUSTUP" -eq 1 ]; then
    warn "a usable Rust toolchain is required; install rustup or drop --no-apt"
  fi
  say "Building RomaCode (release) in $REPO_DIR"
  cd "$REPO_DIR"
  cargo build --release --locked
  BIN_SRC="$REPO_DIR/target/release"
fi

# ---------------------------------------------------------------------------
# 4. Install the binaries
# ---------------------------------------------------------------------------
DEST="/usr/local/bin"
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if command -v sudo >/dev/null 2>&1 && [ -w /usr/local/bin ] || sudo -n true 2>/dev/null; then
    SUDO="sudo"
  else
    DEST="$HOME/.local/bin"
    mkdir -p "$DEST"
  fi
fi

say "Installing romacode-server and romacode to $DEST"
$SUDO install -m 0755 "$BIN_SRC/romacode-server" "$DEST/romacode-server"
$SUDO install -m 0755 "$BIN_SRC/romacode"       "$DEST/romacode"
# Optional: the vendored rustango CLI (Django-style `runserver`, `migrate`,
# `startproject`, `startapp`, `createsuperuser`, …)
if [ -f "$BIN_SRC/rustango" ]; then
  $SUDO install -m 0755 "$BIN_SRC/rustango" "$DEST/rustango" \
    && echo "  + rustango CLI (web framework engine) installed"
fi

case ":$PATH:" in
  *":$DEST:"*) ;;
  *) warn "$DEST is not on your PATH — add:  export PATH=\"$DEST:\$PATH\"" ;;
esac

# ---------------------------------------------------------------------------
# 5. Done
# ---------------------------------------------------------------------------
say "Installed. Next steps:"
cat <<EOF

  # RomaCode auto-starts its brain: just run the screen —
  romacode                           # spawns romacode-server in the background
                                     # on first launch (detached daemon, log at
                                     # ~/.local/share/romacode/server.log)

  # First run only: copy the example config and add your provider key
  cp ${REPO_DIR}/config.example.json ~/.config/romacode/config.json
  #    edit: provider, model, api key   (see docs/SETTINGS.md)

  # Drive the brain yourself instead (legacy two-terminal mode):
  romacode-server serve              # binds 127.0.0.1:7712
  romacode --no-autostart            # in another terminal

  # Stop a background brain started by auto-start:
  pkill -f romacode-server           # or: kill \$(cat ~/.local/share/romacode/server.pid)

  # Android panel (optional): needs an emulator/device with adb authorized
  adb devices                        # verify a device shows up
EOF

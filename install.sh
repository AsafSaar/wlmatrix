#!/usr/bin/env bash
# Build wlmatrix and install it + the idle daemon + the user service.
# Safe to re-run. Requires: cargo, a Wayland/GNOME session, gdbus (glib).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"
CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wlmatrix"

echo "==> Building release binary"
cargo build --release --manifest-path "$REPO/Cargo.toml"

echo "==> Installing binary, daemon, and service"
mkdir -p "$BIN_DIR" "$UNIT_DIR" "$CONF_DIR"
ln -sf "$REPO/target/release/wlmatrix" "$BIN_DIR/wlmatrix"
install -m 0755 "$REPO/dist/wl-screensaver" "$BIN_DIR/wl-screensaver"
install -m 0644 "$REPO/dist/wl-screensaver.service" "$UNIT_DIR/wl-screensaver.service"

# Install a default config only if the user doesn't already have one.
if [ ! -f "$CONF_DIR/config.toml" ]; then
  install -m 0644 "$REPO/dist/config.toml" "$CONF_DIR/config.toml"
  echo "    wrote default config: $CONF_DIR/config.toml"
fi

echo "==> Enabling user service"
systemctl --user daemon-reload
systemctl --user enable --now wl-screensaver.service

echo
echo "Done. The screensaver starts after the idle timeout (default 5 min)."
echo "Quick test (5s idle):   SAVER_IDLE_MS=5000 $BIN_DIR/wl-screensaver"
echo "Configure (time/speed/color/etc):   edit $CONF_DIR/config.toml"
echo "Preview the look:       wlmatrix --color amber   (Ctrl-C / move mouse to exit)"

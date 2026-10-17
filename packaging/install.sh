#!/usr/bin/env bash
# Build LinuxPlay and install it, including start-menu entries and an icon.
#
#   ./packaging/install.sh            # install for the current user (~/.local)
#   sudo PREFIX=/usr/local ./packaging/install.sh   # install system-wide
#
# No arguments are needed for the common case.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

prefix="${PREFIX:-$HOME/.local}"
pkg="packaging"
appid="io.github.techlm77.LinuxPlay"

echo "Building (release)…"
cargo build --release

echo "Installing to $prefix …"
install -Dm755 target/release/linuxplay       "$prefix/bin/linuxplay"
install -Dm755 target/release/linuxplay-host  "$prefix/bin/linuxplay-host"
install -Dm644 "$pkg/$appid.desktop"          "$prefix/share/applications/$appid.desktop"
install -Dm644 "$pkg/$appid.Host.desktop"     "$prefix/share/applications/$appid.Host.desktop"
install -Dm644 "$pkg/$appid.svg"              "$prefix/share/icons/hicolor/scalable/apps/$appid.svg"

# Point Exec at the absolute binary path: a graphical session's PATH often
# does not include ~/.local/bin, so a bare "Exec=linuxplay" would not launch.
sed -i "s|^Exec=linuxplay|Exec=$prefix/bin/linuxplay|" \
    "$prefix/share/applications/$appid.desktop" \
    "$prefix/share/applications/$appid.Host.desktop"

# Refresh the desktop and icon caches so the menu entries show up at once.
update-desktop-database "$prefix/share/applications" 2>/dev/null || true
gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" 2>/dev/null || true

echo
echo "Done. 'LinuxPlay' and 'LinuxPlay Host' are now in your application menu."
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "Note: add $prefix/bin to your PATH to run 'linuxplay' from a terminal." ;;
esac

#!/usr/bin/env bash
# Build LinuxPlay and install it, including start-menu entries and an icon.
#
#   ./packaging/install.sh            # install for the current user (~/.local)
#   sudo PREFIX=/usr/local ./packaging/install.sh   # install system-wide
#   ./packaging/install.sh uninstall  # remove a previous install
#
# No arguments are needed for the common case.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

prefix="${PREFIX:-$HOME/.local}"
pkg="packaging"
appid="io.github.techlm77.LinuxPlay"

if [[ "${1:-}" == "uninstall" ]]; then
    echo "Removing LinuxPlay from $prefix …"
    rm -f "$prefix/bin/linuxplay" "$prefix/bin/linuxplay-host" \
          "$prefix/share/applications/$appid.desktop" \
          "$prefix/share/applications/$appid.Host.desktop" \
          "$prefix/share/icons/hicolor/scalable/apps/$appid.svg"
    update-desktop-database "$prefix/share/applications" 2>/dev/null || true
    gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" 2>/dev/null || true
    echo "Done."
    exit 0
fi

# The host builds with just GStreamer; the client (GUI) also needs GTK4 and
# libadwaita. On a machine meant only to be shared (a host), GTK4 may be
# absent, so build the client optionally and install whatever succeeded.
echo "Building the host…"
cargo build --release -p linuxplay-host

client_built=false
echo "Building the client…"
if cargo build --release -p linuxplay-client; then
    client_built=true
else
    echo "Note: the client (GUI) didn't build — installing the host only."
    echo "      To include the client, install GTK4 + libadwaita dev packages"
    echo "      (Ubuntu: libgtk-4-dev libadwaita-1-dev; Arch: gtk4 libadwaita) and re-run."
fi

echo "Installing to $prefix …"
install -Dm755 target/release/linuxplay-host  "$prefix/bin/linuxplay-host"
install -Dm644 "$pkg/$appid.Host.desktop"     "$prefix/share/applications/$appid.Host.desktop"
install -Dm644 "$pkg/$appid.svg"              "$prefix/share/icons/hicolor/scalable/apps/$appid.svg"
sed -i "s|^Exec=linuxplay|Exec=$prefix/bin/linuxplay|" "$prefix/share/applications/$appid.Host.desktop"

if $client_built; then
    install -Dm755 target/release/linuxplay   "$prefix/bin/linuxplay"
    install -Dm644 "$pkg/$appid.desktop"      "$prefix/share/applications/$appid.desktop"
    # A graphical session's PATH often omits ~/.local/bin, so Exec is absolute.
    sed -i "s|^Exec=linuxplay|Exec=$prefix/bin/linuxplay|" "$prefix/share/applications/$appid.desktop"
fi

# Refresh the desktop and icon caches so the menu entries show up at once.
update-desktop-database "$prefix/share/applications" 2>/dev/null || true
gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" 2>/dev/null || true

echo
if $client_built; then
    echo "Done. 'LinuxPlay' and 'LinuxPlay Host' are now in your application menu."
else
    echo "Done. 'LinuxPlay Host' is now in your application menu."
fi
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "Note: add $prefix/bin to your PATH to run 'linuxplay' from a terminal." ;;
esac

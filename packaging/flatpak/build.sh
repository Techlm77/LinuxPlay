#!/usr/bin/env bash
# Build and install LinuxPlay as a Flatpak, for the current user, no root.
#
#   ./packaging/flatpak/build.sh
#
# It uses flatpak-builder from the org.flatpak.Builder flatpak, so nothing
# needs to be installed with a package manager.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
manifest="$here/io.github.techlm77.LinuxPlay.yml"

# Make sure Flathub is available for user installs.
flatpak remote-add --user --if-not-exists flathub \
    https://dl.flathub.org/repo/flathub.flatpakrepo

# GNOME 51 is based on Freedesktop 26.08; the Rust SDK extension must match.
echo "Ensuring the runtime, SDK and build tools are installed…"
flatpak install -y --user flathub \
    org.gnome.Platform//51 \
    org.gnome.Sdk//51 \
    org.freedesktop.Sdk.Extension.rust-stable//26.08 \
    org.flatpak.Builder

echo "Building…"
flatpak run org.flatpak.Builder --user --force-clean --install \
    --state-dir="$here/.flatpak-builder" \
    "$here/build" \
    "$manifest"

echo
echo "Installed. Launch 'LinuxPlay' (and 'LinuxPlay Host') from your menu,"
echo "or: flatpak run io.github.techlm77.LinuxPlay"

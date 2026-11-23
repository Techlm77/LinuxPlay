#!/usr/bin/env bash
# Build LinuxPlay as a Flatpak for the current user, no root needed.
#
#   packaging/flatpak/build.sh            build and install for this user
#   packaging/flatpak/build.sh --bundle   also write LinuxPlay.flatpak, a
#                                         single file to install elsewhere
#
# It uses flatpak-builder from the org.flatpak.Builder flatpak, so nothing
# has to be installed with a package manager.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
app=io.github.techlm77.LinuxPlay
runtime=26.08

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo

echo "Making sure the runtime, SDK and build tools are installed…"
flatpak install -y --user --noninteractive flathub \
    "org.freedesktop.Platform//$runtime" \
    "org.freedesktop.Sdk//$runtime" \
    "org.freedesktop.Sdk.Extension.rust-stable//$runtime" \
    org.flatpak.Builder

echo "Building…"
flatpak run org.flatpak.Builder --user --force-clean --install \
    --state-dir="$here/.flatpak-builder" \
    --repo="$here/repo" \
    "$here/build" "$here/$app.yml"

if [[ "${1:-}" == "--bundle" ]]; then
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
        "$here/repo" "$here/LinuxPlay.flatpak" "$app"
    echo "Wrote $here/LinuxPlay.flatpak"
fi

echo
echo "Installed. Try: flatpak run $app --help"

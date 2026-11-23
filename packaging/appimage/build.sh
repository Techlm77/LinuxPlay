#!/bin/sh
# Build LinuxPlay as an AppImage: dist/LinuxPlay-<version>-<arch>.AppImage.
# Runs on Ubuntu 24.04 as root or with sudo: a CI runner, or locally with
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:24.04 packaging/appimage/build.sh
# It bundles LinuxPlay, SDL 3 (built in) and GStreamer with the plugins LinuxPlay
# uses. Graphics drivers, VA-API, CUDA, Wayland, X11, fonts and PipeWire come from
# the system, as the AppImage excludelist says. Needs glibc 2.39 or newer at run
# time (Ubuntu 24.04, Debian 13, Fedora 40, Mint 22 and newer; Arch-based);
# older systems use the Flatpak.
set -eu
cd "$(dirname "$0")/../.."
SRC=$PWD
ARCH=$(uname -m)
LIB=/usr/lib/$ARCH-linux-gnu
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
WORK=${WORK:-/tmp/linuxplay-appimage}
APPDIR=$WORK/AppDir
OUT=$SRC/dist
SUDO=; [ "$(id -u)" = 0 ] || SUDO=sudo

# Pinned tools: appimagetool 1.9.1 and the type2 runtime 20251108 (needs no libfuse2).
case $ARCH in
    x86_64) TOOL_SHA=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
            RUNTIME_SHA=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
    aarch64) TOOL_SHA=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
             RUNTIME_SHA=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
    *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
# Libraries every system provides itself (the AppImage excludelist), plus the
# ones that must match the system's drivers: VA-API, Vulkan, CUDA, Mesa.
EXCLUDE=' ld-linux.so.2 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1 libanl.so.1 libBrokenLocale.so.1
 libcidn.so.1 libc.so.6 libdl.so.2 libm.so.6 libmvec.so.1 libnss_compat.so.2 libnss_dns.so.2
 libnss_files.so.2 libnss_hesiod.so.2 libpthread.so.0 libresolv.so.2 librt.so.1 libthread_db.so.1
 libutil.so.1 libstdc++.so.6 libGL.so.1 libEGL.so.1 libGLdispatch.so.0 libGLX.so.0 libOpenGL.so.0
 libGLESv2.so.2 libdrm.so.2 libglapi.so.0 libgbm.so.1 libxcb.so.1 libX11.so.6 libX11-xcb.so.1
 libwayland-client.so.0 libwayland-egl.so.1 libwayland-cursor.so.0 libasound.so.2
 libfontconfig.so.1 libfreetype.so.6 libharfbuzz.so.0 libcom_err.so.2 libexpat.so.1 libgcc_s.so.1
 libgpg-error.so.0 libICE.so.6 libSM.so.6 libusb-1.0.so.0 libuuid.so.1 libz.so.1 libjack.so.0
 libpipewire-0.3.so.0 libxcb-dri3.so.0 libxcb-dri2.so.0 libfribidi.so.0 libgmp.so.10
 libva.so.2 libva-drm.so.2 libva-x11.so.2 libva-wayland.so.2 libvulkan.so.1 libcuda.so.1
 libudev.so.1 libsystemd.so.0 libdbus-1.so.3 libpulse.so.0 '
EXCLUDE=" $(echo $EXCLUDE) "
# GStreamer plugins LinuxPlay uses: capture, conversion, parsing, codecs
# (hardware and software), OpenGL hand-over, audio.
PLUGINS='coreelements app videoconvertscale videorate videotestsrc audioconvert audioresample
 autodetect pulseaudio opus ximagesrc pipewire videoparsersbad va nvcodec vulkan libav x264
 dav1d svtav1 opengl typefindfunctions volume'

echo "== Packages"
export DEBIAN_FRONTEND=noninteractive
$SUDO apt-get update -qq
$SUDO apt-get install -y -qq --no-install-recommends \
    build-essential cmake pkg-config curl ca-certificates file patchelf git \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-pipewire gstreamer1.0-x \
    gstreamer1.0-gl gstreamer1.0-pulseaudio \
    libx11-dev libxext-dev libxrandr-dev libxcursor-dev libxfixes-dev libxi-dev libxss-dev \
    libxtst-dev libxkbcommon-dev libwayland-dev wayland-protocols libdecor-0-dev libegl-dev \
    libgles-dev libdrm-dev libgbm-dev libudev-dev libdbus-1-dev >/dev/null

echo "== Rust"
if ! command -v cargo >/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
    . "$HOME/.cargo/env"
fi

echo "== LinuxPlay $VERSION"
export CARGO_TARGET_DIR=$WORK/target
cargo build --release --locked -p linuxplay -p linuxplay-host -F linuxplay/bundled-sdl
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib/gstreamer-1.0" "$APPDIR/usr/libexec" "$APPDIR/usr/share/linuxplay" "$OUT"
install -m755 "$CARGO_TARGET_DIR/release/linuxplay" "$CARGO_TARGET_DIR/release/linuxplay-host" "$APPDIR/usr/bin/"
echo "$VERSION" > "$APPDIR/usr/share/linuxplay/version"

echo "== GStreamer plugins"
for p in $PLUGINS; do
    if [ -e "$LIB/gstreamer-1.0/libgst$p.so" ]; then
        cp -L "$LIB/gstreamer-1.0/libgst$p.so" "$APPDIR/usr/lib/gstreamer-1.0/"
    else
        echo "   (no $p plugin on this system; left out)"
    fi
done
cp -L "$LIB/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" "$APPDIR/usr/libexec/"

echo "== Libraries"
excluded() { case $EXCLUDE in *" $1 "*) return 0 ;; esac; return 1; }
find "$APPDIR/usr" -type f | while read -r f; do
    file -b "$f" | grep -q '^ELF' || continue
    ldd "$f" | awk '/=> \//{print $1, $3}'
done | sort -u | while read -r soname path; do
    excluded "$soname" || [ -e "$APPDIR/usr/lib/$soname" ] || cp -L "$path" "$APPDIR/usr/lib/$soname"
done
find "$APPDIR/usr" -type f | while read -r f; do  # bundled libraries are found through $ORIGIN
    file -b "$f" | grep -q '^ELF' || continue
    patchelf --set-rpath "\$ORIGIN/$(realpath --relative-to="$(dirname "$f")" "$APPDIR/usr/lib")" "$f"
done
strip --strip-unneeded "$APPDIR/usr/bin/"* 2>/dev/null || true

echo "== AppImage"
install -m755 packaging/appimage/AppRun "$APPDIR/AppRun"
install -Dm644 packaging/io.github.techlm77.LinuxPlay.svg "$APPDIR/io.github.techlm77.LinuxPlay.svg"
sed 's/^NoDisplay=true$/NoDisplay=false/' packaging/io.github.techlm77.LinuxPlay.desktop > "$APPDIR/io.github.techlm77.LinuxPlay.desktop"
fetch() {  # url sha256 file
    curl -fsSL --retry 3 -o "$3" "$1"
    echo "$2  $3" | sha256sum -c - >/dev/null
}
fetch "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$ARCH.AppImage" "$TOOL_SHA" "$WORK/appimagetool"
fetch "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$ARCH" "$RUNTIME_SHA" "$WORK/runtime"
chmod +x "$WORK/appimagetool"
name="LinuxPlay-$VERSION-$ARCH.AppImage"
(cd "$OUT" && rm -f "$name" &&
    ARCH=$ARCH APPIMAGE_EXTRACT_AND_RUN=1 "$WORK/appimagetool" --no-appstream --runtime-file "$WORK/runtime" \
        "$APPDIR" "$name" >/dev/null)
[ "$(id -u)" != 0 ] || chown --reference="$SRC" "$OUT" "$OUT/$name"
ls -l "$OUT/$name"

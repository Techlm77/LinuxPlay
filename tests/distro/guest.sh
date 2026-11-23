#!/usr/bin/env bash
# LinuxPlay distro test, guest side. Copied into a lab VM and run over SSH as
# the test user. Prints KEY=VALUE lines that run.py collects; anything else is
# log. Apps are started inside the logged-in graphical session.
#
#   install FILE       install LinuxPlay the way a user would: a Flatpak bundle
#                      (*.flatpak) or an AppImage (*.AppImage), next to this script
#   facts              what the build finds here: encoders, decoders, uinput
#   stream CAPTURE     host + client on this machine for 14 s (CAPTURE: test or x11)
#   stop               end anything `stream` left running
set -u
APP=io.github.techlm77.LinuxPlay
DIR="$HOME/linuxplay-test"
PASSWORD="distro lab pairing password"
mkdir -p "$DIR"

kv() { printf '%s=%s\n' "$1" "$(printf '%s' "$2" | tr '\n' ' ' | sed 's/[[:space:]]*$//')"; }
have() { command -v "$1" >/dev/null 2>&1; }

# Borrow the graphical session's environment, so windows open on its screen.
import_session() {
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    local env="" p
    have systemctl && env=$(systemctl --user show-environment 2>/dev/null)
    if ! printf '%s\n' "$env" | grep -qE '^(WAYLAND_DISPLAY|DISPLAY)='; then
        env=""
        for p in $(pgrep -u "$(id -u)"); do
            if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -qE '^(WAYLAND_DISPLAY|DISPLAY)='; then
                env=$(tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null); break
            fi
        done
    fi
    [ -n "$env" ] || return 1
    while IFS= read -r e; do
        case "$e" in
            WAYLAND_DISPLAY=*|DISPLAY=*|DBUS_SESSION_BUS_ADDRESS=*|XDG_CURRENT_DESKTOP=*|XDG_SESSION_TYPE=*|XAUTHORITY=*|XDG_DATA_DIRS=*)
                export "${e?}" ;;
        esac
    done <<<"$env"
    export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
}

pkg_install() {
    if have apt-get; then sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y "$@"
    elif have dnf; then sudo -n dnf install -y "$@"
    elif have zypper; then sudo -n zypper -n install "$@"
    elif have pacman; then sudo -n pacman -S --noconfirm --needed "$@"
    else return 1; fi
}

plain() { sed 's/\x1b\[[0-9;]*m//g'; }

# How LinuxPlay was installed decides how it is run.
KIND=$(cat "$DIR/kind" 2>/dev/null || echo flatpak)
AI="$HOME/Applications/LinuxPlay.AppImage"
client() {
    if [ "$KIND" = appimage ]; then "$AI" "$@"; else flatpak run "$APP" "$@"; fi
}
host() {
    if [ "$KIND" = appimage ]; then "$AI" host "$@"; else flatpak run --command=linuxplay-host "$APP" "$@"; fi
}
host_config() {
    if [ "$KIND" = appimage ]; then echo "$HOME/.config/linuxplay/host"; else echo "$HOME/.var/app/$APP/config/linuxplay/host"; fi
}
stop_all() {
    if [ "$KIND" = appimage ]; then pkill -x linuxplay-host; pkill -x linuxplay; else flatpak kill "$APP"; fi
} 2>/dev/null

case "${1:-}" in
install)
    if [ "${2##*.}" = AppImage ]; then
        echo appimage > "$DIR/kind"
        mkdir -p "$HOME/Applications"
        install -m755 "$DIR/$2" "$AI"
        start=$(date +%s)
        # The AppImage mounts itself with FUSE 3's fusermount.
        if ! "$AI" --version > "$DIR/install.log" 2>&1; then
            if grep -qi fuse "$DIR/install.log"; then
                kv EXTRA_STEP "install FUSE 3 (the package that provides fusermount3)"
                pkg_install fuse3 >> "$DIR/install.log" 2>&1 || pkg_install fuse >> "$DIR/install.log" 2>&1
            fi
        fi
        kv INSTALL_SECONDS "$(( $(date +%s) - start ))"
        if "$AI" --version >> "$DIR/install.log" 2>&1; then
            kv INSTALL_OK yes; kv RUNTIME "AppImage $("$AI" --version 2>/dev/null)"
        else
            kv INSTALL_OK no; kv ERROR "$(tail -3 "$DIR/install.log")"; exit 1
        fi
        exit 0
    fi
    echo flatpak > "$DIR/kind"
    bundle="$DIR/$2"
    if ! have flatpak; then
        kv EXTRA_STEP "install Flatpak first"
        pkg_install flatpak > "$DIR/install.log" 2>&1 || { kv INSTALL_OK no; kv ERROR "could not install flatpak"; exit 1; }
    fi
    kv FLATPAK "$(flatpak --version)"
    start=$(date +%s)
    flatpak install --user -y --noninteractive "$bundle" >> "$DIR/install.log" 2>&1; rc=$?
    if [ $rc != 0 ]; then
        kv EXTRA_STEP "add Flathub first: flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
        flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo >> "$DIR/install.log" 2>&1
        flatpak install --user -y --noninteractive "$bundle" >> "$DIR/install.log" 2>&1; rc=$?
    fi
    # A bundle install sometimes finishes without the graphics drivers it said
    # it was installing (seen on Mint and Bazzite). The fix the README gives:
    # install them from Flathub.
    missing=""
    refs=$(flatpak list --all --columns=ref)
    printf '%s\n' "$refs" | grep -q '^org.freedesktop.Platform.GL.default/.*/26.08$' || missing="$missing org.freedesktop.Platform.GL.default//26.08"
    printf '%s\n' "$refs" | grep -q '^org.freedesktop.Platform.codecs-extra/' || missing="$missing org.freedesktop.Platform.codecs-extra//26.08-extra"
    if [ $rc = 0 ] && [ -n "$missing" ]; then
        kv EXTRA_STEP "parts missing after the install: flatpak install flathub$missing"
        flatpak install --user -y --noninteractive flathub $missing >> "$DIR/install.log" 2>&1; rc=$?
    fi
    kv INSTALL_SECONDS "$(( $(date +%s) - start ))"
    if [ $rc = 0 ] && flatpak info --user "$APP" >/dev/null 2>&1; then
        kv INSTALL_OK yes
        kv RUNTIME "$(flatpak info --user --show-runtime "$APP" 2>/dev/null)"
    else
        kv INSTALL_OK no; kv ERROR "$(tail -3 "$DIR/install.log")"; exit 1
    fi
    ;;
facts)
    import_session || kv WARNING "no graphical session found"
    kv HELP_CLIENT "$(client connect --help 2>&1 | grep -c -- '--fullscreen\|--game-mouse\|--display')"
    kv HELP_HOST "$(host --help 2>&1 | grep -c 'account\|invite\|sessions')"
    kv ENCODERS "$(host encoders 2>/dev/null | awk '{print $1}' | tr '\n' ' ')"
    if [ "$KIND" = appimage ]; then
        (cd "$DIR" && rm -rf squashfs-root && "$AI" --appimage-extract usr/lib/gstreamer-1.0/libgstximagesrc.so >/dev/null 2>&1)
        kv X11_CAPTURE "$([ -e "$DIR/squashfs-root/usr/lib/gstreamer-1.0/libgstximagesrc.so" ] && echo yes || echo no)"
        rm -rf "$DIR/squashfs-root"
    else
        kv X11_CAPTURE "$(flatpak run --command=gst-inspect-1.0 "$APP" --exists ximagesrc 2>/dev/null && echo yes || echo no)"
    fi
    # Virtual gamepads need write access to /dev/uinput, outside the sandbox too.
    if [ -w /dev/uinput ]; then kv UINPUT writable; elif [ -e /dev/uinput ]; then kv UINPUT "not writable ($(stat -c '%U:%G %a' /dev/uinput))"; else kv UINPUT missing; fi
    ;;
stream)
    capture="${2:-test}"
    import_session || { kv ERROR "no graphical session"; exit 1; }
    kv SESSION_TYPE "${XDG_SESSION_TYPE:-unknown}"
    conf=$(host_config)
    mkdir -p "$conf"; ( umask 077; printf '%s\n' "$PASSWORD" > "$conf/password" )
    stop_all
    extra=(); [ "$capture" = test ] && extra=(--test-size 1280x720 --test-pattern bars)
    if [ "$KIND" = appimage ]; then
        run_host=("$AI" host); run_client=("$AI")
    else
        run_host=(flatpak run --command=linuxplay-host "$APP"); run_client=(flatpak run --env=LINUXPLAY_PIN="$PASSWORD" "$APP")
    fi
    RUST_LOG=info nohup "${run_host[@]}" --listen loopback --capture "$capture" \
        --unattended --no-audio "${extra[@]}" > "$DIR/host-$capture.log" 2>&1 &
    sleep 4
    LINUXPLAY_PIN="$PASSWORD" RUST_LOG=info,linuxplay_client::viewer::stream=debug nohup "${run_client[@]}" \
        127.0.0.1 --display 0 --stats > "$DIR/client-$capture.log" 2>&1 &
    sleep 14
    kv SCREENSHOT_NOW yes
    sleep 3
    stop_all
    sleep 1
    hostlog=$(plain < "$DIR/host-$capture.log"); clientlog=$(plain < "$DIR/client-$capture.log")
    kv VIDEO_DRIVER "$(printf '%s\n' "$clientlog" | sed -n 's/.*video driver: //p' | head -1)"
    kv HOST_STREAM "$(printf '%s\n' "$hostlog" | sed -n 's/.*server[a-z:]*: streaming display \(.*\)/\1/p' | tail -1)"
    kv STATS "$(printf '%s\n' "$clientlog" | sed -n 's/.*viewer::stream: //p' | tail -1 | sed 's/ | peaks.*//')"
    kv STATS_LINES "$(printf '%s\n' "$clientlog" | grep -c 'viewer::stream: ')"
    kv CLIENT_PROBLEMS "$(printf '%s\n' "$clientlog" | grep -E ' WARN | ERROR |panicked' | cut -c29-220 | head -4 | tr '\n' ';')"
    kv HOST_PROBLEMS "$(printf '%s\n' "$hostlog" | grep -E ' WARN | ERROR |panicked' | grep -v 'unattended access is on\|close_notify' | cut -c29-220 | head -4 | tr '\n' ';')"
    ;;
stop)
    stop_all; kv STOPPED yes
    ;;
*)
    echo "usage: $0 install BUNDLE | facts | stream test|x11 | stop"; exit 2
    ;;
esac

#!/usr/bin/env bash
set -euo pipefail

PY="${PYTHON:-python3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

banner() {
  printf '\n\033[1;34m╔══════════════════════════════════════════════╗\033[0m\n'
  printf '\033[1;34m║\033[0m         LinuxPlay Bootstrap Environment      \033[1;34m║\033[0m\n'
  printf '\033[1;34m╚══════════════════════════════════════════════╝\033[0m\n'
}

section() {
  printf '\n\033[1;36m── %s ─────────────────────────────────────\033[0m\n' "$1"
}

msg()  { printf "\033[1;32m[OK]\033[0m %s\n" "$*"; }
warn() { printf "\033[1;33m[!!]\033[0m %s\n" "$*"; }
err()  { printf "\033[1;31m[ERR]\033[0m %s\n" "$*"; }

need_cmd() { command -v "$1" >/dev/null 2>&1; }
bail() { err "$1"; exit 1; }

usage() {
  cat <<EOF
Usage:
  ./run.sh check              # Check/install tools and Python deps
  ./run.sh start [args...]    # Run start.py (GUI launcher)
  ./run.sh host  [args...]    # Run host.py
  ./run.sh client [args...]   # Run client.py

Behavior:
  - Uses a local .venv in this repo (no global pip installs)
  - Installs only missing system deps (where supported)
  - Installs only missing Python deps into .venv

Python deps (inside .venv):
  PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil evdev cryptography
EOF
}

detect_pm() {
  if   need_cmd apt-get; then echo apt
  elif need_cmd dnf;      then echo dnf
  elif need_cmd zypper;   then echo zypper
  elif need_cmd pacman;   then echo pacman
  elif need_cmd apk;      then echo apk
  elif need_cmd emerge;   then echo emerge
  elif need_cmd nix-env || need_cmd nix; then echo nix
  else echo unknown; fi
}

pkg_for() {
  local pm="$1" cmd="$2"
  case "$pm" in
    apt)
      case "$cmd" in
        ffmpeg|ffplay)          echo "ffmpeg" ;;
        xdotool)                echo "xdotool" ;;
        xclip)                  echo "xclip" ;;
        pactl)                  echo "pulseaudio-utils" ;;
        setcap)                 echo "libcap2-bin" ;;
        wg)                     echo "wireguard-tools" ;;
        qrencode)               echo "qrencode" ;;
        glxinfo)                echo "mesa-utils" ;;
        pkg-config)             echo "pkg-config" ;;
        python3)                echo "python3" ;;
        python3-venv)           echo "python3-venv" ;;
        python3-pip)            echo "python3-pip" ;;
      esac
      ;;
    dnf)
      case "$cmd" in
        ffmpeg|ffplay)          echo "ffmpeg" ;;
        xdotool)                echo "xdotool" ;;
        xclip)                  echo "xclip" ;;
        pactl)                  echo "pulseaudio-utils" ;;
        setcap)                 echo "libcap" ;;
        wg)                     echo "wireguard-tools" ;;
        qrencode)               echo "qrencode" ;;
        glxinfo)                echo "mesa-demos" ;;
        pkg-config)             echo "pkgconf" ;;
        python3)                echo "python3" ;;
        python3-pip)            echo "python3-pip" ;;
        python3-venv)           echo "python3-virtualenv" ;;
      esac
      ;;
    zypper)
      case "$cmd" in
        ffmpeg|ffplay)          echo "ffmpeg" ;;
        xdotool)                echo "xdotool" ;;
        xclip)                  echo "xclip" ;;
        pactl)                  echo "pulseaudio-utils" ;;
        setcap)                 echo "libcap-progs" ;;
        wg)                     echo "wireguard-tools" ;;
        qrencode)               echo "qrencode" ;;
        glxinfo)                echo "Mesa-demo-x" ;;
        pkg-config)             echo "pkg-config" ;;
        python3)                echo "python3" ;;
        python3-venv)           echo "python3-venv" ;;
        python3-pip)            echo "python3-pip" ;;
      esac
      ;;
    pacman)
      case "$cmd" in
        ffmpeg|ffplay)          echo "ffmpeg" ;;
        xdotool)                echo "xdotool" ;;
        xclip)                  echo "xclip" ;;
        pactl)                  echo "pulseaudio" ;;
        setcap)                 echo "libcap" ;;
        wg)                     echo "wireguard-tools" ;;
        qrencode)               echo "qrencode" ;;
        glxinfo)                echo "mesa-demos" ;;
        pkg-config)             echo "pkgconf" ;;
        python3)                echo "python" ;;
        python3-pip)            echo "python-pip" ;;
      esac
      ;;
    apk)
      case "$cmd" in
        ffmpeg|ffplay)          echo "ffmpeg" ;;
        xdotool)                echo "xdotool" ;;
        xclip)                  echo "xclip" ;;
        pactl)                  echo "" ;;
        setcap)                 echo "libcap-utils" ;;
        wg)                     echo "wireguard-tools" ;;
        qrencode)               echo "qrencode" ;;
        pkg-config)             echo "pkgconf" ;;
        python3)                echo "python3" ;;
        python3-pip)            echo "py3-pip" ;;
        python3-venv)           echo "py3-virtualenv" ;;
      esac
      ;;
  esac
}

auto_install_missing_system() {
  local missing_cmds=("$@")
  local pm; pm="$(detect_pm)"

  [ "${#missing_cmds[@]}" -eq 0 ] && return 0

  if [[ "$pm" == "unknown" || "$pm" == "emerge" || "$pm" == "nix" ]]; then
    warn "Auto-install not supported on this system."
    warn "Please install manually: ${missing_cmds[*]}"
    return 1
  fi

  local pkgs=()
  local seen=""

  for cmd in "${missing_cmds[@]}"; do
    local pkg; pkg="$(pkg_for "$pm" "$cmd" || true)"
    if [ -n "$pkg" ] && [[ " $seen " != *" $pkg "* ]]; then
      pkgs+=("$pkg")
      seen+=" $pkg"
    elif [ -z "$pkg" ] && [[ " $seen " != *" $cmd "* ]]; then
      pkgs+=("$cmd")
      seen+=" $cmd"
    fi
  done

  [ "${#pkgs[@]}" -eq 0 ] && return 0

  section "Installing missing system packages ($pm)"
  printf 'Packages: %s\n' "${pkgs[*]}"

  case "$pm" in
    apt)
      sudo apt-get update -y
      sudo apt-get install -y --no-install-recommends "${pkgs[@]}"
      ;;
    dnf)
      sudo dnf install -y "${pkgs[@]}"
      ;;
    zypper)
      sudo zypper --non-interactive refresh
      sudo zypper --non-interactive install --no-recommends "${pkgs[@]}"
      ;;
    pacman)
      sudo pacman -Sy --noconfirm --needed "${pkgs[@]}"
      ;;
    apk)
      sudo apk add --no-cache "${pkgs[@]}"
      ;;
  esac
}

check_python_version() {
  if ! need_cmd "$PY"; then
    printf "ERROR: python3 not found\n"
    return 1
  fi
  local want="3.9.0"
  local got
  got="$("$PY" -c 'import sys; print(".".join(map(str, sys.version_info[:3])))' 2>/dev/null || echo "0")"
  if printf '%s\n' "$want" "$got" | sort -V -C 2>/dev/null; then
    printf "OK:    Python3 %s >= %s\n" "$got" "$want"
    return 0
  else
    printf "ERROR: Python3 %s < %s\n" "$got" "$want"
    return 1
  fi
}

check_apt_dev_libs() {
  local missing_apt=()
  local apt_libs=(
    pkg-config
    python3-dev
    libavdevice-dev
    libavfilter-dev
    libavformat-dev
    libavcodec-dev
    libswscale-dev
    libswresample-dev
    libavutil-dev
    libgl1
  )

  for p in "${apt_libs[@]}"; do
    dpkg -s "$p" >/dev/null 2>&1 || missing_apt+=("$p")
  done

  if [ "${#missing_apt[@]}" -gt 0 ]; then
    section "Installing FFmpeg/OpenGL dev libraries (apt)"
    printf 'Packages: %s\n' "${missing_apt[*]}"
    sudo apt-get update -y
    sudo apt-get install -y --no-install-recommends "${missing_apt[@]}"
  fi
}

check_system_deps() {
  section "System dependency check"
  local missing=()

  check_python_version || missing+=("python3")

  for cmd in ffmpeg ffplay xdotool xclip pactl setcap wg qrencode pkg-config; do
    if need_cmd "$cmd"; then
      printf "OK:    %-10s (%s)\n" "$cmd" "$cmd"
    else
      printf "MISSING: %-8s\n" "$cmd"
      missing+=("$cmd")
    fi
  done

  if need_cmd glxinfo; then
    printf "OK:    OpenGL probe (glxinfo)\n"
  else
    printf "WARN:  glxinfo not found (OpenGL probe skipped)\n"
  fi

  if [ "${#missing[@]}" -gt 0 ]; then
    auto_install_missing_system "${missing[@]}" || return 1

    section "Re-check after install"
    for cmd in "${missing[@]}"; do
      if ! need_cmd "$cmd"; then
        err "Still missing required tool: $cmd"
        return 1
      else
        printf "OK:    %-10s (%s)\n" "$cmd" "$cmd"
      fi
    done
  fi

  if [ "$(detect_pm)" = "apt" ]; then
    check_apt_dev_libs
  fi

  msg "All core system tools detected."
  return 0
}

ensure_venv() {
  if [ ! -d ".venv" ]; then
    section "Python virtual environment"
    msg "Creating local virtualenv at: $SCRIPT_DIR/.venv"
    "$PY" -m venv .venv || bail "Failed to create virtualenv (install python3-venv)."
  fi

  source .venv/bin/activate || bail "Failed to activate .venv"

  if [ -z "${VIRTUAL_ENV:-}" ]; then
    bail "VIRTUAL_ENV not set after activation (refusing to touch global pip)."
  fi

  printf '\n\033[1;35m[env]\033[0m Using isolated Python environment: \033[1m%s\033[0m\n' "$VIRTUAL_ENV"
}

ensure_pydeps() {
  section "Python dependencies (.venv)"
  echo "Required:"
  echo "  PyQt5 PyOpenGL PyOpenGL_accelerate av numpy pynput pyperclip psutil evdev cryptography"
  echo

  local missing
  set +e
  missing="$(
    python - <<'EOF'
import importlib, sys

MODS = {
    "PyQt5":                "PyQt5",
    "OpenGL":               "PyOpenGL",
    "OpenGL.GL":            "PyOpenGL",
    "PyOpenGL_accelerate":  "PyOpenGL_accelerate",
    "av":                   "av",
    "numpy":                "numpy",
    "pynput":               "pynput",
    "pyperclip":            "pyperclip",
    "psutil":               "psutil",
    "evdev":                "evdev",
    "cryptography":         "cryptography",
}

missing = {}
for mod, pkg in MODS.items():
    try:
        importlib.import_module(mod)
        print(f"OK:    {pkg}")
    except Exception:
        missing[pkg] = True
        print(f"MISSING: {pkg}")

if missing:
    sys.stdout.write("MISSING_PKGS " + " ".join(sorted(missing.keys())))
    sys.exit(1)
sys.exit(0)
EOF
  )"
  local status=$?
  set -e

  local to_install=""
  if [ $status -ne 0 ]; then
    to_install="$(printf "%s\n" "$missing" | awk '/^MISSING_PKGS /{ $1=""; sub(/^ /,""); print }')"
  fi

  if [ -n "$to_install" ]; then
    echo
    msg "Installing missing Python packages into .venv:"
    echo "  $to_install"
    python -m pip install -U pip wheel setuptools
    python -m pip install $to_install || {
      err "pip install failed for: $to_install"
      warn "If 'av' failed to build, ensure FFmpeg dev libraries are installed (see README)."
      exit 1
    }
  else
    msg "All required Python packages are available in .venv."
  fi
}

bootstrap() {
  banner
  check_system_deps || bail "System dependency check failed."
  ensure_venv
  ensure_pydeps
}

run_mode() {
  local mode="${1:-}"; shift || true

  case "$mode" in
    check)
      bootstrap
      echo
      msg "Environment ready."
      echo "You can now run:"
      echo "  ./run.sh start"
      echo "  ./run.sh host --gui ..."
      echo "  ./run.sh client --host_ip 1.2.3.4 ..."
      ;;
    start)
      bootstrap
      exec python3 start.py "$@"
      ;;
    host)
      bootstrap
      exec python3 host.py "$@"
      ;;
    client)
      bootstrap
      exec python3 client.py "$@"
      ;;
    ""|-h|--help|help)
      usage
      ;;
    *)
      err "Unknown mode: $mode"
      usage
      exit 1
      ;;
  esac
}

run_mode "$@"

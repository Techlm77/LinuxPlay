#!/usr/bin/env python3
import os
import subprocess
import argparse
import sys
import logging
import time
import threading
import psutil
import socket
import atexit
import signal
import struct
import platform as py_platform

from shutil import which

from PyQt5.QtWidgets import (
    QApplication, QWidget, QVBoxLayout, QTextEdit, QPushButton, QLabel, QHBoxLayout
)
from PyQt5.QtGui import QFont, QPalette, QColor, QKeySequence
from PyQt5.QtCore import Qt, QTimer, QObject, pyqtSignal

UDP_VIDEO_PORT = 5000
UDP_CONTROL_PORT = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
FILE_UPLOAD_PORT = 7003
UDP_HEARTBEAT_PORT = 7004
UDP_GAMEPAD_PORT = 7005
UDP_AUDIO_PORT = 6001

DEFAULT_FPS = "30"
LEGACY_BITRATE = "8M"
DEFAULT_RES = "1920x1080"

IS_LINUX   = py_platform.system() == "Linux"

HEARTBEAT_INTERVAL = 1.0
HEARTBEAT_TIMEOUT  = 10.0
RECONNECT_COOLDOWN = 2.0

def _marker_value() -> str:
    marker = os.environ.get("LINUXPLAY_MARKER", "LinuxPlayHost")
    sid = os.environ.get("LINUXPLAY_SID", "")
    return f"{marker}:{sid}" if sid else marker

def _ffmpeg_base_cmd() -> list:
    return ["ffmpeg", "-hide_banner", "-loglevel", "error"]

def _marker_opt() -> list:
    return ["-metadata", f"comment={_marker_value()}"]

try:
    from pynput.mouse import Controller as MouseCtl, Button
    from pynput.keyboard import Controller as KeyCtl, Key
    HAVE_PYNPUT = True
    _mouse = MouseCtl()
    _keys = KeyCtl()
except Exception:
    HAVE_PYNPUT = False

try:
    import pyperclip
    HAVE_PYPERCLIP = True
except Exception:
    HAVE_PYPERCLIP = False

try:
    from evdev import UInput, ecodes, AbsInfo
    HAVE_UINPUT = True
except Exception:
    HAVE_UINPUT = False

class HostState:
    def __init__(self):
        self.video_threads = []
        self.audio_thread = None
        self.current_bitrate = LEGACY_BITRATE
        self.last_clipboard_content = ""
        self.ignore_clipboard_update = False
        self.should_terminate = False
        self.video_thread_lock = threading.Lock()
        self.clipboard_lock = threading.Lock()
        self.handshake_sock = None
        self.control_sock = None
        self.clipboard_listener_sock = None
        self.file_upload_sock = None
        self.heartbeat_sock = None
        self.last_pong_ts = 0.0
        self.last_disconnect_ts = 0.0
        self.client_ip = None
        self.monitors = []
        self.shutdown_lock = threading.Lock()
        self.shutdown_reason = None
        self.net_mode = "lan"
        self.starting_streams = False
        self.gamepad_thread = None

host_state = HostState()
HOST_ARGS = None

def _map_nvenc_tune(tune: str) -> str:
    t = (tune or "").strip().lower()
    if not t:
        return ""

    valid_nvenc_tunes = {
        "ultra-low-latency": "ull",
        "low-latency": "ll",
        "high-quality": "hq",
        "high-performance": "hp",
        "performance": "hp",
        "lossless": "lossless",
        "lossless-highperf": "losslesshp",
        "blu-ray": "bd",
        "auto": "",
        "none": "",
        "default": "",
    }

    if t in valid_nvenc_tunes:
        return valid_nvenc_tunes[t]

    logging.warning("Unrecognized NVENC tune '%s' — passing through as-is.", t)
    return t

def _vaapi_fmt_for_pix_fmt(pix_fmt: str, codec: str) -> str:
    pf = (pix_fmt or "").strip().lower()
    valid_vaapi_fmts = {

        "nv12", "yuv420p", "yuyv422", "uyvy422", "yuv422p",
        "yuv444p", "rgb0", "bgr0", "rgba", "bgra",

        "p010", "p010le", "yuv420p10", "yuv420p10le",
        "yuv422p10", "yuv422p10le", "yuv444p10", "yuv444p10le",

        "yuv444p12le", "yuv444p16le",
    }

    if pf in valid_vaapi_fmts:
        logging.info("Using requested VAAPI pix_fmt '%s' for codec %s.", pf, codec)
        return pf

    logging.warning("Unrecognized pix_fmt '%s' — falling back to 'nv12'.", pf)
    return "nv12"

def trigger_shutdown(reason: str):
    with host_state.shutdown_lock:
        if host_state.should_terminate:
            return
        host_state.should_terminate = True
        host_state.shutdown_reason = reason
        logging.critical("FATAL/STOP: %s -- stopping all streams and listeners.", reason)

        for s in (
            host_state.handshake_sock,
            host_state.control_sock,
            host_state.clipboard_listener_sock,
            host_state.file_upload_sock,
        ):
            try:
                if s:
                    try:
                        s.shutdown(socket.SHUT_RDWR)
                    except Exception:
                        pass
                    s.close()
            except Exception:
                pass

        set_status(f"Stopping… ({reason})")

def stop_all():
    host_state.should_terminate = True

    with host_state.video_thread_lock:
        for thread in host_state.video_threads:
            thread.stop()
            thread.join(timeout=2)
        host_state.video_threads.clear()

    if host_state.audio_thread:
        host_state.audio_thread.stop()
        host_state.audio_thread.join(timeout=2)
        host_state.audio_thread = None
    if host_state.gamepad_thread:
        try:
            host_state.gamepad_thread.stop()
            host_state.gamepad_thread.join(timeout=2)
        except Exception:
            pass
        host_state.gamepad_thread = None
    for s in (
        host_state.handshake_sock,
        host_state.control_sock,
        host_state.clipboard_listener_sock,
        host_state.file_upload_sock,
    ):
        try:
            if s:
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except Exception:
                    pass
                s.close()
        except Exception:
            pass

    host_state.starting_streams = False

def stop_streams_only():
    with host_state.video_thread_lock:
        if host_state.video_threads:
            logging.info("Stopping active video streams...")
            for t in host_state.video_threads:
                try:
                    t.stop()
                    t.join(timeout=2)
                except Exception as e:
                    logging.debug(f"Error stopping video thread: {e}")
            host_state.video_threads.clear()

        if host_state.audio_thread:
            try:
                host_state.audio_thread.stop()
                host_state.audio_thread.join(timeout=2)
            except Exception as e:
                logging.debug(f"Error stopping audio thread: {e}")
            host_state.audio_thread = None

        host_state.starting_streams = False
        host_state.last_disconnect_ts = time.time()
        logging.info("All streams stopped and cooldown set.")

def cleanup():
    stop_all()
atexit.register(cleanup)

def has_nvidia():
    return which("nvidia-smi") is not None

def is_intel_cpu():
    try:
        if IS_LINUX:
            with open("/proc/cpuinfo","r") as f:
                return "GenuineIntel" in f.read()
        p = (py_platform.processor() or "").lower()
        return "intel" in p or "intel" in py_platform.platform().lower()
    except Exception:
        return False

def has_vaapi():
    return IS_LINUX and os.path.exists("/dev/dri/renderD128")

def ffmpeg_has_encoder(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-encoders"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        return name.lower() in out
    except Exception:
        return False

def ffmpeg_has_demuxer(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-demuxers"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        for line in out.splitlines():
            line = line.strip().lower()
            if line.startswith("d ") or line.startswith(" d "):
                parts = line.split()
                if len(parts) >= 2 and parts[1] == name.lower():
                    return True
        return False
    except Exception:
        return False

def ffmpeg_has_device(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-devices"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        for line in out.splitlines():
            line = line.strip().lower()
            if line.startswith("d ") or line.startswith(" d "):
                parts = line.split()
                if len(parts) >= 2 and parts[1] == name.lower():
                    return True
        return False
    except Exception:
        return False

class StreamThread(threading.Thread):
    def __init__(self, cmd, name):
        super().__init__(daemon=True)
        self.cmd = cmd
        self.name = name
        self.process = None
        self._running = True

    def run(self):
        logging.info("Starting %s: %s", self.name, " ".join(self.cmd))
        try:
            self.process = subprocess.Popen(
                self.cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                universal_newlines=True
            )

            try:
                import psutil, os
                ps = psutil.Process(self.process.pid)
                ps.nice(-10)
                cpu_count = os.cpu_count()
                if cpu_count and cpu_count > 4:
                    ps.cpu_affinity(list(range(0, min(cpu_count, 8))))
                logging.debug(f"Affinity + priority applied to {self.name}")
            except Exception as e:
                logging.debug(f"Affinity set failed: {e}")

        except Exception as e:
            trigger_shutdown(f"{self.name} failed to start: {e}")
            return

        while self._running and not host_state.should_terminate:
            ret = self.process.poll()
            if ret is not None:
                try:
                    _, err = self.process.communicate(timeout=0.5)
                except Exception:
                    err = ""
                if ret != 0 and not host_state.should_terminate and self._running:
                    logging.error("%s exited (%s). stderr:\n%s", self.name, ret, err or "(no output)")
                    trigger_shutdown(f"{self.name} crashed/quit with code {ret}")
                break
            time.sleep(0.2)

    def stop(self):
        self._running = False
        try:
            if self.process and self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=1.5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
        except Exception:
            pass

def _detect_monitors_linux():
    try:
        out = subprocess.check_output(["xrandr", "--listmonitors"], universal_newlines=True)
    except Exception as e:
        logging.warning("xrandr failed (%s); using default single monitor.", e)
        return []
    mons = []
    for line in out.strip().splitlines()[1:]:
        parts = line.split()
        for part in parts:
            if 'x' in part and '+' in part:
                try:
                    res, ox, oy = part.split('+')
                    w,h = res.split('x')
                    w = int(w.split('/')[0]); h = int(h.split('/')[0])
                    mons.append((w,h,int(ox),int(oy))); break
                except Exception:
                    continue
    return mons

def detect_monitors():
    return _detect_monitors_linux()

def _input_ll_flags():
    return [
        "-fflags","nobuffer","-avioflags","direct",
        "-use_wallclock_as_timestamps","1",
        "-thread_queue_size","64",
        "-probesize","32",
        "-analyzeduration","0",
    ]

def _output_sync_flags():
    return ["-fps_mode","passthrough"]

def _mpegts_ll_mux_flags():
    return ["-flush_packets","1","-max_interleave_delta","0","-muxdelay","0","-muxpreload","0","-mpegts_flags","resend_headers"]

def _best_ts_pkt_size(mtu_guess: int, ipv6: bool) -> int:
    if mtu_guess <= 0:
        mtu_guess = 1500
    overhead = 48 if ipv6 else 28
    max_payload = max(512, mtu_guess - overhead)
    return max(188, (max_payload // 188) * 188)

def _parse_bitrate_bits(bstr: str) -> int:
    if not bstr: return 0
    s = str(bstr).strip().lower()
    try:
        if s.endswith("k"):
            return int(float(s[:-1]) * 1000)
        if s.endswith("m"):
            return int(float(s[:-1]) * 1_000_000)
        if s.endswith("g"):
            return int(float(s[:-1]) * 1_000_000_000)
        return int(float(s))
    except Exception:
        return 0

def _format_bits(bits: int) -> str:
    if bits >= 1_000_000:
        return f"{max(1, int(bits/1_000_000))}M"
    if bits >= 1000:
        return f"{max(1, int(bits/1000))}k"
    return str(max(1, bits))

def _target_bpp(codec: str, fps: int) -> float:
    c = (codec or "h.264").lower()
    if c in ("h.265","hevc"):
        base = 0.045
    else:
        base = 0.07
    if fps >= 90:
        base += 0.02
    return base

def _safe_nvenc_preset(preset):
    allowed = {"default","fast","medium","slow","hp","hq","bd","ll","llhq","llhp",
               "lossless","p1","p2","p3","p4","p5","p6","p7"}
    return preset if preset in allowed else "p4"

def _pick_encoder_args(codec: str, hwenc: str, preset: str, gop: str, qp: str,
                       tune: str, bitrate: str, pix_fmt: str):
    codec = (codec or "h.264").lower()
    hwenc = (hwenc or "auto").lower()
    preset_l = (preset or "").strip().lower()
    extra_filters = []
    enc = []

    def ensure(name: str) -> bool:
        ok = ffmpeg_has_encoder(name)
        if not ok:
            logging.warning("Requested encoder '%s' not found; falling back to CPU.", name)
        return ok

    if hwenc == "auto":
        if codec == "h.264":
            if has_nvidia() and ffmpeg_has_encoder("h264_nvenc"):
                hwenc = "nvenc"
            elif is_intel_cpu() and ffmpeg_has_encoder("h264_qsv"):
                hwenc = "qsv"
            elif has_vaapi() and ffmpeg_has_encoder("h264_vaapi"):
                hwenc = "vaapi"
            else:
                hwenc = "cpu"
        elif codec == "h.265":
            if has_nvidia() and ffmpeg_has_encoder("hevc_nvenc"):
                hwenc = "nvenc"
            elif is_intel_cpu() and ffmpeg_has_encoder("hevc_qsv"):
                hwenc = "qsv"
            elif has_vaapi() and ffmpeg_has_encoder("hevc_vaapi"):
                hwenc = "vaapi"
            else:
                hwenc = "cpu"
        else:
            hwenc = "cpu"

    adaptive = getattr(HOST_ARGS, "adaptive", False)
    if not bitrate or str(bitrate).lower() in ("0", "auto"):
        if "vaapi" in hwenc:
            dynamic_flags = ["-rc_mode", "CQP", "-qp", qp or "21"]
        elif "nvenc" in hwenc:
            dynamic_flags = ["-rc", "constqp", "-qp", qp or "23"]
        elif "qsv" in hwenc:
            dynamic_flags = ["-rc_mode", "ICQ", "-icq_quality", qp or "23"]
        else:
            dynamic_flags = ["-crf", qp or "23"]
    elif adaptive:
        if "nvenc" in hwenc:
            dynamic_flags = ["-rc", "vbr", "-maxrate", bitrate, "-cq", qp or "23"]
        elif "vaapi" in hwenc:
            dynamic_flags = ["-rc_mode", "CQP", "-qp", qp or "21"]
        elif "qsv" in hwenc:
            dynamic_flags = ["-rc_mode", "ICQ", "-icq_quality", qp or "23"]
        else:
            dynamic_flags = ["-crf", qp or "23"]
    else:
        dynamic_flags = ["-b:v", bitrate]

    try:
        gop_val = int(gop)
        use_gop = gop_val > 0
    except Exception:
        gop_val = 0
        use_gop = False

    if codec == "h.264":
        if hwenc == "nvenc" and ensure("h264_nvenc"):
            enc = [
                "-c:v", "h264_nvenc",
                "-preset", _safe_nvenc_preset(preset_l or "llhq"),
                *(["-g", str(gop_val)] if use_gop else []),
                "-bf", "0", "-rc-lookahead", "0", "-refs", "1",
                "-flags2", "+fast", *dynamic_flags,
                "-pix_fmt", pix_fmt, "-bsf:v", "h264_mp4toannexb"
            ]

            if tune:
                _t = tune.strip().lower()
                if _t in ("zerolatency", "ull", "ultra-low-latency", "ultra_low_latency"):
                    enc += ["-tune", "ull"]
                elif _t in ("low-latency", "ll", "low_latency"):
                    enc += ["-tune", "ll"]
                elif _t in ("hq", "film", "quality", "high_quality"):
                    enc += ["-tune", "hq"]
                elif _t in ("lossless",):
                    enc += ["-tune", "lossless"]
            else:
                enc += ["-tune", "ll"]

        elif hwenc == "qsv" and ensure("h264_qsv"):
            enc = ["-c:v", "h264_qsv", *dynamic_flags,
                   "-pix_fmt", pix_fmt, "-bsf:v", "h264_mp4toannexb"]

        elif hwenc == "vaapi" and has_vaapi() and ensure("h264_vaapi"):
            va_fmt = _vaapi_fmt_for_pix_fmt(pix_fmt, codec)
            extra_filters += ["-vf", f"format={va_fmt},hwupload",
                              "-vaapi_device", "/dev/dri/renderD128"]
            enc = ["-c:v", "h264_vaapi", "-bf", "0", *dynamic_flags,
                   "-bsf:v", "h264_mp4toannexb"]

        else:
            enc = ["-c:v", "libx264", "-preset", preset_l or "ultrafast",
                   "-tune", tune or "zerolatency",
                   *(["-g", str(gop_val)] if use_gop else []),
                   *dynamic_flags, "-pix_fmt", pix_fmt,
                   "-bsf:v", "h264_mp4toannexb"]

    elif codec == "h.265":
        if hwenc == "nvenc" and ensure("hevc_nvenc"):
            enc = [
                "-c:v", "hevc_nvenc",
                "-preset", _safe_nvenc_preset(preset_l or "p5"),
                *(["-g", str(gop_val)] if use_gop else []),
                "-bf", "0", "-rc-lookahead", "0", "-refs", "1",
                "-flags2", "+fast", *dynamic_flags,
                "-pix_fmt", pix_fmt, "-bsf:v", "hevc_mp4toannexb"
            ]

            if tune:
                _t = tune.strip().lower()
                if _t in ("zerolatency", "ull", "ultra-low-latency", "ultra_low_latency"):
                    enc += ["-tune", "ull"]
                elif _t in ("low-latency", "ll", "low_latency"):
                    enc += ["-tune", "ll"]
                elif _t in ("hq", "film", "quality", "high_quality"):
                    enc += ["-tune", "hq"]
                elif _t in ("lossless",):
                    enc += ["-tune", "lossless"]
            else:
                enc += ["-tune", "ll"]

        elif hwenc == "qsv" and ensure("hevc_qsv"):
            enc = ["-c:v", "hevc_qsv", *dynamic_flags,
                   "-pix_fmt", pix_fmt, "-bsf:v", "hevc_mp4toannexb"]

        elif hwenc == "vaapi" and has_vaapi() and ensure("hevc_vaapi"):
            va_fmt = _vaapi_fmt_for_pix_fmt(pix_fmt, codec)
            extra_filters += ["-vf", f"format={va_fmt},hwupload",
                              "-vaapi_device", "/dev/dri/renderD128"]
            enc = ["-c:v", "hevc_vaapi", "-bf", "0", *dynamic_flags,
                   "-bsf:v", "hevc_mp4toannexb"]

        else:
            enc = ["-c:v", "libx265", "-preset", preset_l or "ultrafast",
                   "-tune", tune or "zerolatency",
                   *(["-g", str(gop_val)] if use_gop else []),
                   *dynamic_flags, "-pix_fmt", pix_fmt,
                   "-bsf:v", "hevc_mp4toannexb"]

    return extra_filters, enc

def _pick_kms_device():
    for cand in ("card0","card1","card2"):
        p = f"/dev/dri/{cand}"
        if os.path.exists(p):
            return p
    return "/dev/dri/card0"

def build_video_cmd(args, bitrate, monitor_info, video_port):
    try:
        fps_i = int(str(args.framerate))
    except Exception:
        fps_i = 60

    w, h, ox, oy = monitor_info
    preset = args.preset.strip().lower() if args.preset else ""
    gop, qp, tune, pix_fmt = args.gop, args.qp, args.tune, args.pix_fmt

    codec_name = (args.encoder if args.encoder and args.encoder.lower() != "none" else "h.264")
    min_bits = int(w) * int(h) * max(1, fps_i) * _target_bpp(codec_name, fps_i)
    cur_bits = _parse_bitrate_bits(bitrate)

    if cur_bits < min_bits:
        safe_bits = int(min_bits)
        safe_str = _format_bits(safe_bits)
        logging.warning(
            "Bitrate too low for %dx%d@%dfps (%s < %s). Bumping to %s.",
            w, h, fps_i, str(bitrate), _format_bits(cur_bits), safe_str
        )
        bitrate = safe_str
        host_state.current_bitrate = safe_str

    ip = getattr(host_state, "client_ip", None)
    if not ip or not isinstance(ip, str) or ip.strip().lower() in ("none", "", "0.0.0.0"):
        logging.error(f"build_video_cmd: invalid client IP ({ip!r}) — refusing to build ffmpeg command.")
        return None

    base_in = [*(_ffmpeg_base_cmd()), *(_input_ll_flags())]
    disp = args.display
    if "." not in disp:
        disp = f"{disp}.0"

    capture_pref = (os.environ.get("LINUXPLAY_CAPTURE", "auto") or "auto").lower()
    kms_available = ffmpeg_has_device("kmsgrab")
    vaapi_available = has_vaapi()

    def _vaapi_possible_for_codec():
        enc = (args.encoder or "h.264").lower()
        return (
            (enc == "h.264" and ffmpeg_has_encoder("h264_vaapi")) or
            (enc == "h.265" and ffmpeg_has_encoder("hevc_vaapi"))
        )

    use_kms = False
    if capture_pref == "kmsgrab":
        use_kms = True
    elif capture_pref == "auto" and kms_available:
        if (
            (args.hwenc in ("auto", "vaapi") and vaapi_available and _vaapi_possible_for_codec())
            or (args.hwenc == "cpu")
        ):
            use_kms = True

    if use_kms:
        kms_dev = os.environ.get("LINUXPLAY_KMS_DEVICE", _pick_kms_device())
        logging.info("Linux capture: kmsgrab (%s) selected (pref=%s).", kms_dev, capture_pref)

        input_side = [
            *base_in,
            "-f", "kmsgrab",
            "-framerate", str(fps_i),
            "-device", kms_dev,
            "-i", "-",
        ]

        _k_extra_filters, encode = _pick_encoder_args(
            codec=args.encoder, hwenc=args.hwenc, preset=preset,
            gop=gop, qp=qp, tune=tune, bitrate=bitrate, pix_fmt=pix_fmt
        )

        if any(x in encode for x in ("h264_vaapi", "hevc_vaapi")):
            _vaapi_fmt = {
                "nv12": "nv12",
                "yuv420p": "nv12",
                "p010": "p010",
                "yuv420p10": "p010",
            }.get((pix_fmt or "nv12").lower(), "nv12")

            extra_filters = [
                "-vf", f"hwmap=derive_device=vaapi,scale_vaapi=w={w}:h={h}:format={_vaapi_fmt}",
                "-vaapi_device", "/dev/dri/renderD128"
            ]

        elif args.hwenc == "cpu":
            extra_filters = ["-vf", f"hwdownload,format={pix_fmt or 'yuv420p'}"]

        else:
            logging.warning(
                "kmsgrab requested but encoder backend '%s' not supported; falling back to x11grab.",
                args.hwenc
            )
            use_kms = False

    if not use_kms:
        logging.info("Linux capture: x11grab selected (pref=%s, kms=%s).", capture_pref, kms_available)
        input_arg = f"{disp}+{ox},{oy}"
        input_side = [
            *base_in,
            "-f", "x11grab",
            "-draw_mouse", "0",
            "-framerate", str(fps_i),
            "-video_size", f"{w}x{h}",
            "-i", input_arg,
        ]

        extra_filters, encode = _pick_encoder_args(
            codec=args.encoder, hwenc=args.hwenc, preset=preset,
            gop=gop, qp=qp, tune=tune, bitrate=bitrate, pix_fmt=pix_fmt
        )

    output_side = _output_sync_flags()
    out = [
        *(_mpegts_ll_mux_flags()),
        "-flags", "+low_delay",
        "-f", "mpegts",
        *_marker_opt(),
        (
            f"udp://{ip}:{video_port}"
            f"?pkt_size=1316"
            f"&buffer_size=65536"
            f"&fifo_size=32768"
            f"&overrun_nonfatal=1"
            f"&max_delay=0"
        ),
    ]

    full_cmd = input_side + output_side + (extra_filters or []) + encode + out
    return full_cmd

def build_audio_cmd():
    opus_app = os.environ.get("LP_OPUS_APP", "voip")
    opus_fd  = os.environ.get("LP_OPUS_FD", "10")

    net_mode = getattr(host_state, "net_mode", "lan")
    aud_buf = "4194304" if net_mode == "wifi" else "512"
    aud_delay = "150000" if net_mode == "wifi" else "0"

    mon = os.environ.get("PULSE_MONITOR", "")
    if not mon and which("pactl"):
        try:
            out = subprocess.check_output(
                ["pactl", "list", "short", "sources"],
                text=True,
                stderr=subprocess.DEVNULL
            )
            best = None
            for line in out.splitlines():
                parts = line.split("\t")
                if len(parts) >= 5:
                    name, state = parts[1], parts[4].upper()
                    if ".monitor" in name:
                        if state == "RUNNING":
                            best = name
                            break
                        elif state == "IDLE" and not best:
                            best = name
            if best:
                mon = best
        except Exception as e:
            logging.warning("PulseAudio monitor detection failed: %s", e)

    if not mon:
        mon = "default.monitor"
    elif not mon.endswith(".monitor"):
        mon += ".monitor"

    logging.info("Using PulseAudio source: %s", mon)

    input_side = [
        *(_ffmpeg_base_cmd()),
        *(_input_ll_flags()),
        "-f", "pulse",
        "-i", mon,
    ]

    output_side = _output_sync_flags()

    encode = [
        "-c:a", "libopus",
        "-b:a", "128k",
        "-application", opus_app,
        "-frame_duration", opus_fd,
    ]

    out = [
        *(_mpegts_ll_mux_flags()),
        *_marker_opt(),
        "-f", "mpegts",
        f"udp://{host_state.client_ip}:{UDP_AUDIO_PORT}"
        f"?pkt_size=1316&buffer_size={aud_buf}&overrun_nonfatal=1&max_delay={aud_delay}"
    ]

    return input_side + output_side + encode + out

def _inject_mouse_move(x,y):
    if HAVE_PYNPUT:
        try: _mouse.position = (int(x), int(y))
        except Exception as e: logging.debug("pynput move failed: %s", e)
    elif IS_LINUX:
        subprocess.Popen(["xdotool","mousemove",str(x),str(y)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def _inject_mouse_down(btn):
    if HAVE_PYNPUT:
        b = {"1": Button.left, "2": Button.middle, "3": Button.right}.get(btn, Button.left)
        try: _mouse.press(b)
        except Exception as e: logging.debug("pynput mousedown failed: %s", e)
    elif IS_LINUX:
        subprocess.Popen(["xdotool","mousedown",btn], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def _inject_mouse_up(btn):
    if HAVE_PYNPUT:
        b = {"1": Button.left, "2": Button.middle, "3": Button.right}.get(btn, Button.left)
        try: _mouse.release(b)
        except Exception as e: logging.debug("pynput mouseup failed: %s", e)
    elif IS_LINUX:
        subprocess.Popen(["xdotool","mouseup",btn], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def _inject_scroll(btn):
    if HAVE_PYNPUT:
        try:
            if btn == "4": _mouse.scroll(0, +1)
            elif btn == "5": _mouse.scroll(0, -1)
            elif btn == "6": _mouse.scroll(-1, 0)
            elif btn == "7": _mouse.scroll(+1, 0)
        except Exception as e:
            logging.debug("pynput scroll failed: %s", e)
    elif IS_LINUX:
        subprocess.Popen(["xdotool","click",btn], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

_key_map = {
    "Escape": Key.esc if HAVE_PYNPUT else None, "Tab": Key.tab if HAVE_PYNPUT else None, "BackSpace": Key.backspace if HAVE_PYNPUT else None,
    "Return": Key.enter if HAVE_PYNPUT else None, "Insert": Key.insert if HAVE_PYNPUT else None, "Delete": Key.delete if HAVE_PYNPUT else None,
    "Home": Key.home if HAVE_PYNPUT else None, "End": Key.end if HAVE_PYNPUT else None,
    "Left": Key.left if HAVE_PYNPUT else None, "Up": Key.up if HAVE_PYNPUT else None, "Right": Key.right if HAVE_PYNPUT else None, "Down": Key.down if HAVE_PYNPUT else None,
    "Page_Up": Key.page_up if HAVE_PYNPUT else None, "Page_Down": Key.page_down if HAVE_PYNPUT else None,
    "Shift_L": Key.shift if HAVE_PYNPUT else None, "Control_L": Key.ctrl if HAVE_PYNPUT else None,
    "Alt_L": Key.alt if HAVE_PYNPUT else None, "Alt_R": (Key.alt_gr if HAVE_PYNPUT and hasattr(Key,"alt_gr") else (Key.alt if HAVE_PYNPUT else None)),
    "Super_L": (Key.cmd if HAVE_PYNPUT else None), "Caps_Lock": (Key.caps_lock if HAVE_PYNPUT else None),
    "F1": Key.f1 if HAVE_PYNPUT else None, "F2": Key.f2 if HAVE_PYNPUT else None, "F3": Key.f3 if HAVE_PYNPUT else None,
    "F4": Key.f4 if HAVE_PYNPUT else None, "F5": Key.f5 if HAVE_PYNPUT else None, "F6": Key.f6 if HAVE_PYNPUT else None,
    "F7": Key.f7 if HAVE_PYNPUT else None, "F8": Key.f8 if HAVE_PYNPUT else None, "F9": Key.f9 if HAVE_PYNPUT else None,
    "F10": Key.f10 if HAVE_PYNPUT else None, "F11": Key.f11 if HAVE_PYNPUT else None, "F12": Key.f12 if HAVE_PYNPUT else None,
    "space": Key.space if HAVE_PYNPUT else None,
}

CHAR_TO_X11 = {
    '-':'minus', '=':'equal', '[':'bracketleft', ']':'bracketright', '\\':'backslash',
    ';':'semicolon', "'":'apostrophe', ',':'comma', '.':'period', '/':'slash', '`':'grave',
    '!':'exclam', '"':'quotedbl', '#':'numbersign', '$':'dollar', '%':'percent',
    '&':'ampersand', '*':'asterisk', '(':'parenleft', ')':'parenright', '_':'underscore',
    '+':'plus', '{':'braceleft', '}':'braceright', '|':'bar', ':':'colon',
    '<':'less', '>':'greater', '?':'question', '£':'sterling', '¬':'notsign', '¦':'brokenbar',
}

NAME_TO_CHAR = {
    'minus':'-', 'equal':'=', 'bracketleft':'[', 'bracketright':']', 'backslash':'\\',
    'semicolon':';', 'apostrophe':"'", 'comma':',', 'period':'.', 'slash':'/', 'grave':'`',
    'exclam':'!', 'quotedbl':'"', 'numbersign':'#', 'dollar':'$', 'percent':'%',
    'ampersand':'&', 'asterisk':'*', 'parenleft':'(', 'parenright':')', 'underscore':'_',
    'plus':'+', 'braceleft':'{', 'braceright':'}', 'bar':'|', 'colon':':',
    'less':'<', 'greater':'>', 'question':'?', 'sterling':'£', 'notsign':'¬', 'brokenbar':'¦',
}

def _inject_key(action, name):
    if HAVE_PYNPUT:
        k = _key_map.get(name)
        try:
            if k:
                (_keys.press if action == "down" else _keys.release)(k)
                return

            if isinstance(name, str) and len(name) == 1:
                (_keys.press if action == "down" else _keys.release)(name)
                return

            ch = NAME_TO_CHAR.get(name)
            if ch:
                (_keys.press if action == "down" else _keys.release)(ch)
                return
        except Exception as e:
            logging.debug("pynput key %s failed for %r: %s", action, name, e)
        return

    if IS_LINUX:
        try:
            keyname = name
            if isinstance(name, str) and len(name) == 1:
                keyname = CHAR_TO_X11.get(name, name)
            cmd = ["xdotool", "keydown" if action == "down" else "keyup", keyname]
            subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception as e:
            logging.debug("xdotool key %s failed for %r: %s", action, name, e)

def tcp_handshake_server(sock, encoder_str, args):
    logging.info("TCP handshake server on %d", TCP_HANDSHAKE_PORT)
    set_status("Waiting for client handshake…")
    while not host_state.should_terminate:
        try:
            conn, addr = sock.accept()
            logging.info("Handshake from %s", addr)
            host_state.client_ip = addr[0]
            data = conn.recv(1024).decode("utf-8", errors="replace").strip()
            if data == "HELLO":
                monitors_str = ";".join(f"{w}x{h}+{ox}+{oy}" for (w,h,ox,oy) in host_state.monitors) if host_state.monitors else DEFAULT_RES
                resp = f"OK:{encoder_str}:{monitors_str}"
                conn.sendall(resp.encode("utf-8"))
            else:
                conn.sendall(b"FAIL")
            conn.close()
            set_status(f"Client: {host_state.client_ip}")
        except OSError:
            break
        except Exception as e:
            trigger_shutdown(f"Handshake server error: {e}")
            break

def start_streams_for_current_client(args):
    ip = getattr(host_state, "client_ip", None)
    if not ip:
        logging.warning("start_streams_for_current_client: no valid client IP — waiting for handshake.")
        return

    with host_state.video_thread_lock:
        if host_state.starting_streams:
            logging.debug("start_streams_for_current_client: already starting; skipping duplicate call.")
            return
        if host_state.video_threads:
            logging.debug("start_streams_for_current_client: video threads already active; skipping.")
            return

        if getattr(host_state, "last_disconnect_ts", 0) > 0:
            elapsed = time.time() - host_state.last_disconnect_ts
            if elapsed < 2.0:
                logging.debug(f"start_streams_for_current_client: cooldown {elapsed:.2f}s — skipping restart.")
                return

        host_state.starting_streams = True
        try:
            host_state.video_threads = []
            for i, mon in enumerate(host_state.monitors):
                cmd = build_video_cmd(args, host_state.current_bitrate, mon, UDP_VIDEO_PORT + i)
                if not cmd:
                    logging.warning(f"Video {i} skipped — invalid or incomplete ffmpeg command.")
                    continue
                t = StreamThread(cmd, f"Video {i}")
                t.start()
                host_state.video_threads.append(t)

            if args.audio == "enable" and not host_state.audio_thread:
                ac = build_audio_cmd()
                if ac:
                    host_state.audio_thread = StreamThread(ac, "Audio")
                    host_state.audio_thread.start()
        except Exception as e:
            logging.error(f"start_streams_for_current_client: exception while starting — {e}")
        finally:
            host_state.starting_streams = False

def control_listener(sock):
    logging.info("Control listener UDP %d", UDP_CONTROL_PORT)
    while not host_state.should_terminate:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="ignore").strip()
            if not msg:
                continue

            tokens = msg.split()
            cmd = tokens[0].upper() if tokens else ""

            if cmd == "NET" and len(tokens) >= 2:
                mode = tokens[1].strip().lower()
                if mode in ("wifi", "lan"):
                    old = getattr(host_state, "net_mode", "lan")
                    if mode != old:
                        logging.info("Network mode switch requested: %s -> %s", old, mode)
                        host_state.net_mode = mode
                        try:
                            stop_streams_only()
                            if HOST_ARGS:
                                start_streams_for_current_client(HOST_ARGS)
                        except Exception as e:
                            logging.error("Restart after NET failed: %s", e)
                continue

            elif cmd == "GOODBYE":
                logging.info("Client at %s disconnected cleanly.", addr[0])
                try:
                    stop_streams_only()
                    host_state.client_ip = None
                    host_state.starting_streams = False
                    set_status("Client disconnected — waiting for connection…")
                    logging.debug("All streams stopped after GOODBYE.")
                    time.sleep(RECONNECT_COOLDOWN)
                except Exception as e:
                    logging.error(f"Error handling GOODBYE cleanup: {e}")
                continue

            elif cmd == "MOUSE_PKT" and len(tokens) == 5:
                try:
                    pkt_type = int(tokens[1])
                    bmask = int(tokens[2])
                    x = int(tokens[3])
                    y = int(tokens[4])
                except ValueError:
                    continue

                _inject_mouse_move(x, y)

                if pkt_type == 1:
                    if bmask & 1: _inject_mouse_down("1")
                    if bmask & 2: _inject_mouse_down("2")
                    if bmask & 4: _inject_mouse_down("3")
                elif pkt_type == 3:
                    if bmask & 1: _inject_mouse_up("1")
                    if bmask & 2: _inject_mouse_up("2")
                    if bmask & 4: _inject_mouse_up("3")

            elif cmd == "MOUSE_SCROLL" and len(tokens) == 2:
                _inject_scroll(tokens[1])

            elif cmd == "KEY_PRESS" and len(tokens) == 2:
                _inject_key("down", tokens[1])
            elif cmd == "KEY_RELEASE" and len(tokens) == 2:
                _inject_key("up", tokens[1])

        except OSError:
            break
        except Exception as e:
            trigger_shutdown(f"Control listener error: {e}")
            break

def clipboard_monitor_host():
    if not HAVE_PYPERCLIP:
        logging.info("pyperclip not available; host clipboard sync disabled.")
        return
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    while not host_state.should_terminate:
        current = ""
        try:
            current = (pyperclip.paste() or "").strip()
        except Exception:
            pass
        with host_state.clipboard_lock:
            if (not host_state.ignore_clipboard_update and current and current != host_state.last_clipboard_content and host_state.client_ip):
                host_state.last_clipboard_content = current
                msg = f"CLIPBOARD_UPDATE HOST {current}".encode("utf-8")
                try:
                    sock.sendto(msg, (host_state.client_ip, UDP_CLIPBOARD_PORT))
                except Exception as e:
                    trigger_shutdown(f"Clipboard send error: {e}")
                    break
        time.sleep(1)
    sock.close()

def clipboard_listener_host(sock):
    if not HAVE_PYPERCLIP:
        return
    while not host_state.should_terminate:
        try:
            data, addr = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="ignore")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "CLIENT":
                new_content = tokens[2]
                with host_state.clipboard_lock:
                    host_state.ignore_clipboard_update = True
                    try:
                        if (pyperclip.paste() or "") != new_content:
                            pyperclip.copy(new_content)
                    except Exception as e:
                        trigger_shutdown(f"Clipboard apply error: {e}")
                        break
                    finally:
                        host_state.ignore_clipboard_update = False
        except OSError:
            break
        except Exception as e:
            trigger_shutdown(f"Clipboard listener error: {e}")
            break

def recvall(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk: return None
        data += chunk
    return data

def file_upload_listener():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    host_state.file_upload_sock = s
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("", FILE_UPLOAD_PORT))
        s.listen(5)
        logging.info("File upload listener TCP %d", FILE_UPLOAD_PORT)
    except Exception as e:
        trigger_shutdown(f"File upload listener bind/listen failed: {e}")
        try: s.close()
        finally:
            host_state.file_upload_sock = None
        return

    while not host_state.should_terminate:
        try:
            conn, addr = s.accept()
            header = recvall(conn, 4)
            if not header:
                conn.close()
                continue
            filename_length = int.from_bytes(header, "big")
            filename = recvall(conn, filename_length).decode("utf-8")
            file_size = int.from_bytes(recvall(conn, 8), "big")
            dest_dir = os.path.join(os.path.expanduser("~"), "LinuxPlayDrop")
            os.makedirs(dest_dir, exist_ok=True)
            dest_path = os.path.join(dest_dir, filename)
            with open(dest_path, "wb") as f:
                remaining = file_size
                while remaining > 0:
                    chunk = conn.recv(min(4096, remaining))
                    if not chunk: break
                    f.write(chunk); remaining -= len(chunk)
            conn.close()
            logging.info("Received file %s (%d bytes)", dest_path, file_size)
        except OSError:
            break
        except Exception as e:
            trigger_shutdown(f"File upload error: {e}")
            break
    try:
        s.close()
    finally:
        host_state.file_upload_sock = None

def heartbeat_manager(args):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("", UDP_HEARTBEAT_PORT))
        host_state.heartbeat_sock = s
        logging.info("Heartbeat manager running on UDP %d", UDP_HEARTBEAT_PORT)
    except Exception as e:
        trigger_shutdown(f"Heartbeat socket error: {e}")
        return

    last_ping = 0.0
    host_state.last_pong_ts = time.time()

    while not host_state.should_terminate:
        now = time.time()

        if host_state.client_ip:
            if now - last_ping >= HEARTBEAT_INTERVAL:
                try:
                    s.sendto(b"PING", (host_state.client_ip, UDP_HEARTBEAT_PORT))
                    last_ping = now
                except Exception as e:
                    logging.warning("Heartbeat send error: %s", e)

            s.settimeout(0.5)
            try:
                data, addr = s.recvfrom(1024)
                msg = data.decode("utf-8", errors="ignore").strip()
                if msg.startswith("PONG") and addr[0] == host_state.client_ip:
                    host_state.last_pong_ts = now
            except socket.timeout:
                pass
            except Exception as e:
                logging.debug("Heartbeat recv error: %s", e)

            if now - host_state.last_pong_ts > HEARTBEAT_TIMEOUT:
                if host_state.client_ip:
                    logging.warning(
                        "Heartbeat timeout from %s — no PONG or GOODBYE, stopping streams.",
                        host_state.client_ip
                    )
                    try:
                        stop_streams_only()
                    except Exception as e:
                        logging.error("Error stopping streams after timeout: %s", e)

                    host_state.client_ip = None
                    host_state.starting_streams = False
                    set_status("Client disconnected — waiting for connection…")

                    time.sleep(RECONNECT_COOLDOWN)

                host_state.last_pong_ts = now
        else:
            time.sleep(0.5)

def resource_monitor():
    p = psutil.Process(os.getpid())

    def read_gpu_usage():
        try:
            import pynvml
            pynvml.nvmlInit()
            handle = pynvml.nvmlDeviceGetHandleByIndex(0)
            util = pynvml.nvmlDeviceGetUtilizationRates(handle)
            return f"GPU: {util.gpu}% VRAM: {util.memory}% (NVENC)"
        except Exception:
            pass

        try:
            for card in os.listdir("/sys/class/drm"):
                busy_path = f"/sys/class/drm/{card}/device/gpu_busy_percent"
                if os.path.exists(busy_path):
                    with open(busy_path, "r") as f:
                        val = f.read().strip()
                        return f"GPU: {val}% (VAAPI)"
        except Exception:
            pass

        try:
            cmd = ["timeout", "0.5", "intel_gpu_top", "-J"]
            out = subprocess.check_output(cmd, stderr=subprocess.DEVNULL).decode()
            if '"Busy"' in out:
                import json
                j = json.loads(out)
                busy = j["engines"]["Render/3D/0"]["busy"]
                return f"GPU: {busy}% (iGPU)"
        except Exception:
            pass

        return ""

    while not host_state.should_terminate:
        cpu = p.cpu_percent(interval=1)
        mem = p.memory_info().rss / (1024*1024)
        gpu_info = read_gpu_usage()
        logging.info(f"[MONITOR] CPU: {cpu:.1f}% | RAM: {mem:.1f} MB" + (f" | {gpu_info}" if gpu_info else ""))
        time.sleep(5)

def stats_broadcast():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    while not host_state.should_terminate:
        if host_state.client_ip:
            try:
                cpu = psutil.cpu_percent(interval=None)
                mem = psutil.virtual_memory().used / (1024 * 1024)

                gpu = 0.0
                try:
                    import pynvml
                    pynvml.nvmlInit()
                    h = pynvml.nvmlDeviceGetHandleByIndex(0)
                    gpu = float(pynvml.nvmlDeviceGetUtilizationRates(h).gpu)
                except Exception:
                    try:
                        for card in os.listdir("/sys/class/drm"):
                            busy_path = f"/sys/class/drm/{card}/device/gpu_busy_percent"
                            if os.path.exists(busy_path):
                                with open(busy_path) as f:
                                    gpu = float(f.read().strip())
                                break
                    except Exception:
                        gpu = 0.0

                fps = getattr(host_state, "current_fps", 0)
                msg = f"STATS {cpu:.1f} {gpu:.1f} {mem:.1f} {fps:.1f}"
                sock.sendto(msg.encode("utf-8"), (host_state.client_ip, UDP_HEARTBEAT_PORT))
            except Exception:
                pass
        time.sleep(1)

class GamepadServer(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self._running = True
        self.sock = None
        self.ui = None
        self._dpad = {"left": False, "right": False, "up": False, "down": False}
        self._hatx = 0
        self._haty = 0

    def _open_socket(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVLOWAT, 1)
        s.bind(("", UDP_GAMEPAD_PORT))
        s.setblocking(False)
        return s

    def _open_uinput(self):
        if not (IS_LINUX and HAVE_UINPUT):
            return None
        caps = {
            ecodes.EV_KEY: [
                ecodes.BTN_SOUTH, ecodes.BTN_EAST, ecodes.BTN_NORTH, ecodes.BTN_WEST,
                ecodes.BTN_TL, ecodes.BTN_TR, ecodes.BTN_TL2, ecodes.BTN_TR2,
                ecodes.BTN_SELECT, ecodes.BTN_START,
                ecodes.BTN_THUMBL, ecodes.BTN_THUMBR,
                getattr(ecodes, "BTN_MODE", 0x13c),
            ],
            ecodes.EV_ABS: [
                (ecodes.ABS_X,   AbsInfo(0, -32768, 32767, 16, 0, 0)),
                (ecodes.ABS_Y,   AbsInfo(0, -32768, 32767, 16, 0, 0)),
                (ecodes.ABS_RX,  AbsInfo(0, -32768, 32767, 16, 0, 0)),
                (ecodes.ABS_RY,  AbsInfo(0, -32768, 32767, 16, 0, 0)),
                (ecodes.ABS_Z,   AbsInfo(0, 0, 255, 0, 0, 0)),
                (ecodes.ABS_RZ,  AbsInfo(0, 0, 255, 0, 0, 0)),
                (ecodes.ABS_HAT0X, AbsInfo(0, -1, 1, 0, 0, 0)),
                (ecodes.ABS_HAT0Y, AbsInfo(0, -1, 1, 0, 0, 0)),
            ],
        }
        ui = UInput(
            caps,
            name="LinuxPlay Virtual Gamepad",
            bustype=0x03,
            vendor=0x045e,
            product=0x028e,
            version=0x0110,
        )
        ui.write(ecodes.EV_ABS, ecodes.ABS_Z, 0)
        ui.write(ecodes.EV_ABS, ecodes.ABS_RZ, 0)
        ui.syn()
        return ui

    def run(self):
        try:
            import psutil
            psutil.Process(os.getpid()).nice(-10)
        except Exception:
            pass

        try:
            self.sock = self._open_socket()
            self.ui = self._open_uinput()
            if not self.ui:
                logging.info("Gamepad server active (pass-through), but uinput unavailable.")
            else:
                logging.info("Gamepad server active on UDP %d with virtual device.", UDP_GAMEPAD_PORT)
        except Exception as e:
            logging.error("Gamepad server init failed: %s", e)
            return

        buf = bytearray(64)
        pending = []
        unpack_event = struct.Struct("!Bhh").unpack_from

        while self._running and not host_state.should_terminate:
            try:
                n, _ = self.sock.recvfrom_into(buf)
            except BlockingIOError:
                time.sleep(0.0005)
                continue
            except OSError:
                break
            if n < 5:
                continue

            try:
                for i in range(0, n - 4, 5):
                    try:
                        t, c, v = unpack_event(buf, i)
                    except Exception:
                        continue
                    if not self.ui:
                        continue

                    if t == ecodes.EV_KEY and c in (
                        ecodes.KEY_LEFT, ecodes.KEY_RIGHT, ecodes.KEY_UP, ecodes.KEY_DOWN
                    ):
                        if c == ecodes.KEY_LEFT:
                            self._dpad["left"] = (v != 0)
                        elif c == ecodes.KEY_RIGHT:
                            self._dpad["right"] = (v != 0)
                        elif c == ecodes.KEY_UP:
                            self._dpad["up"] = (v != 0)
                        elif c == ecodes.KEY_DOWN:
                            self._dpad["down"] = (v != 0)

                        new_hatx = (
                            -1 if self._dpad["left"] and not self._dpad["right"]
                            else (1 if self._dpad["right"] and not self._dpad["left"] else 0)
                        )
                        new_haty = (
                            -1 if self._dpad["up"] and not self._dpad["down"]
                            else (1 if self._dpad["down"] and not self._dpad["up"] else 0)
                        )

                        if new_hatx != self._hatx:
                            self._hatx = new_hatx
                            pending.append((ecodes.EV_ABS, ecodes.ABS_HAT0X, self._hatx))
                        if new_haty != self._haty:
                            self._haty = new_haty
                            pending.append((ecodes.EV_ABS, ecodes.ABS_HAT0Y, self._haty))
                    else:
                        pending.append((t, c, v))

                if pending:
                    for et, ec, ev in pending:
                        self.ui.write(et, ec, ev)
                    self.ui.syn()
                    pending.clear()

            except Exception as e:
                logging.debug("Gamepad parse/write error: %s", e)

        try:
            if self.ui:
                self.ui.close()
        except Exception:
            pass
        try:
            if self.sock:
                self.sock.close()
        except Exception:
            pass

    def stop(self):
        self._running = False
        try:
            if self.sock:
                self.sock.close()
        except Exception:
            pass

def session_manager(args):
    while not host_state.should_terminate:
        if time.time() - host_state.last_disconnect_ts < RECONNECT_COOLDOWN:
            time.sleep(0.5)
            continue

        if host_state.client_ip and not host_state.video_threads:
            set_status(f"Client: {host_state.client_ip}")
            start_streams_for_current_client(args)
        time.sleep(0.5)

def _signal_handler(signum, frame):
    logging.info("Signal %s received, shutting down…", signum)
    trigger_shutdown(f"Signal {signum}")
    stop_all()
    try:
        sys.exit(0)
    except SystemExit:
        pass

def core_main(args, use_signals=True) -> int:
    if use_signals:
        try:
            for _sig in (signal.SIGINT, signal.SIGTERM):
                signal.signal(_sig, _signal_handler)
        except Exception:
            pass

    logging.debug("FFmpeg marker in use: %s", _marker_value())

    host_state.current_bitrate = args.bitrate
    host_state.monitors = detect_monitors() or [(1920,1080,0,0)]

    global HOST_ARGS
    HOST_ARGS = args

    try:
        host_state.handshake_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        host_state.handshake_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        host_state.handshake_sock.bind(("", TCP_HANDSHAKE_PORT))
        host_state.handshake_sock.listen(5)
    except Exception as e:
        trigger_shutdown(f"Handshake socket error: {e}")
        stop_all(); return 1

    try:
        host_state.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        host_state.control_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        host_state.control_sock.bind(("", UDP_CONTROL_PORT))
    except Exception as e:
        trigger_shutdown(f"Control socket error: {e}")
        stop_all(); return 1

    try:
        host_state.clipboard_listener_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        host_state.clipboard_listener_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        host_state.clipboard_listener_sock.bind(("", UDP_CLIPBOARD_PORT))
    except Exception as e:
        trigger_shutdown(f"Clipboard socket error: {e}")
        stop_all(); return 1

    threading.Thread(target=tcp_handshake_server, args=(host_state.handshake_sock, args.encoder, args), daemon=True).start()
    threading.Thread(target=clipboard_monitor_host, daemon=True).start()
    threading.Thread(target=clipboard_listener_host, args=(host_state.clipboard_listener_sock,), daemon=True).start()
    threading.Thread(target=file_upload_listener, daemon=True).start()

    logging.info("Waiting for client handshake…")

    threading.Thread(target=heartbeat_manager, args=(args,), daemon=True).start()
    threading.Thread(target=session_manager, args=(args,), daemon=True).start()
    threading.Thread(target=control_listener, args=(host_state.control_sock,), daemon=True).start()
    threading.Thread(target=resource_monitor, daemon=True).start()
    threading.Thread(target=stats_broadcast, daemon=True).start()
    if IS_LINUX:
        try:
            host_state.gamepad_thread = GamepadServer()
            host_state.gamepad_thread.start()
        except Exception as e:
            logging.error("Failed to start gamepad server: %s", e)
    logging.info("Host running. Close window or Ctrl+C to quit.")
    try:
        while not host_state.should_terminate:
            time.sleep(0.2)
    except KeyboardInterrupt:
        trigger_shutdown("KeyboardInterrupt")
    finally:
        reason = host_state.shutdown_reason
        stop_all()
        if reason:
            logging.critical("Stopped due to error: %s", reason)
            return 1
        else:
            logging.info("Shutdown complete.")
            return 0

class LogEmitter(QObject):
    log = pyqtSignal(str)
    status = pyqtSignal(str)

log_emitter = LogEmitter()

def set_status(text: str):
    try:
        log_emitter.status.emit(text)
    except Exception:
        pass

class QtLogHandler(logging.Handler):
    def __init__(self):
        super().__init__()

    def emit(self, record):
        try:
            msg = self.format(record)
        except Exception:
            msg = record.getMessage()
        try:
            log_emitter.log.emit(msg)
        except Exception:
            pass

def _apply_dark_palette(app: QApplication):
    app.setStyle("Fusion")
    palette = app.palette()
    palette.setColor(QPalette.Window, QColor(53,53,53))
    palette.setColor(QPalette.WindowText, Qt.white)
    palette.setColor(QPalette.Base, QColor(35,35,35))
    palette.setColor(QPalette.AlternateBase, QColor(53,53,53))
    palette.setColor(QPalette.ToolTipBase, Qt.white)
    palette.setColor(QPalette.ToolTipText, Qt.white)
    palette.setColor(QPalette.Text, Qt.white)
    palette.setColor(QPalette.Button, QColor(53,53,53))
    palette.setColor(QPalette.ButtonText, Qt.white)
    palette.setColor(QPalette.BrightText, Qt.red)
    palette.setColor(QPalette.Link, QColor(42,130,218))
    palette.setColor(QPalette.Highlight, QColor(42,130,218))
    palette.setColor(QPalette.HighlightedText, Qt.black)
    app.setPalette(palette)

class HostWindow(QWidget):
    def __init__(self, args):
        super().__init__()
        self.args = args
        self.core_thread = None
        self.setWindowTitle("LinuxPlay Host")
        self.resize(840, 520)

        layout = QVBoxLayout(self)

        self.statusLabel = QLabel("Idle")
        self.statusLabel.setAlignment(Qt.AlignLeft | Qt.AlignVCenter)

        self.logView = QTextEdit()
        self.logView.setReadOnly(True)
        font = QFont("monospace"); font.setStyleHint(QFont.TypeWriter)
        self.logView.setFont(font)

        buttons = QHBoxLayout()
        self.stopBtn = QPushButton("Stop")
        self.stopBtn.clicked.connect(self._on_stop)

        self.stopBtn.setAutoDefault(False)
        self.stopBtn.setDefault(False)
        self.stopBtn.setFocusPolicy(Qt.NoFocus)
        self.stopBtn.setShortcut(QKeySequence())
        self.stopBtn.setEnabled(False)
        QTimer.singleShot(1200, lambda: self.stopBtn.setEnabled(True))
        self.logView.setFocus()

        buttons.addStretch(1)
        buttons.addWidget(self.stopBtn)

        layout.addWidget(self.statusLabel)
        layout.addWidget(self.logView)
        layout.addLayout(buttons)

        self._log_handler = QtLogHandler()
        fmt = logging.Formatter("%(asctime)s [%(levelname)s] %(message)s", "%H:%M:%S")
        self._log_handler.setFormatter(fmt)
        logging.getLogger().addHandler(self._log_handler)
        logging.getLogger().setLevel(logging.getLogger().level)

        log_emitter.log.connect(self.append_log)
        log_emitter.status.connect(self.set_status_text)

        self._start_core()

        self._poll_timer = QTimer(self)
        self._poll_timer.timeout.connect(self._poll_core_done)
        self._poll_timer.start(300)

    def _start_core(self):
        self.set_status_text("Starting…")
        self.append_log("Launching host core…")
        self.core_rc = None

        def _run():
            rc = core_main(self.args, use_signals=False)
            self.core_rc = rc
            QTimer.singleShot(0, lambda: QApplication.instance().exit(rc))

        self.core_thread = threading.Thread(target=_run, name="HostCore", daemon=True)
        self.core_thread.start()

    def _poll_core_done(self):
        if host_state.should_terminate:
            self.stopBtn.setEnabled(False)

    def _on_stop(self):
        if not self.stopBtn.isEnabled():
            return
        self.stopBtn.setEnabled(False)
        self.append_log("Stop requested by user.")
        trigger_shutdown("User pressed Stop")

    def append_log(self, text: str):
        self.logView.append(text)
        self.logView.moveCursor(self.logView.textCursor().End)

    def set_status_text(self, text: str):
        self.statusLabel.setText(text)

    def closeEvent(self, event):
        if not host_state.should_terminate:
            trigger_shutdown("Window closed")
        event.accept()

def parse_args():
    p = argparse.ArgumentParser(description="LinuxPlay Host (Linux only)")
    p.add_argument("--gui", action="store_true", help="Show host GUI window.")
    p.add_argument("--encoder", choices=["none","h.264","h.265"], default="none")
    p.add_argument("--hwenc", choices=["auto","cpu","nvenc","qsv","vaapi"], default="auto",
                   help="Manual encoder backend selection (auto=heuristic).")
    p.add_argument("--framerate", default=DEFAULT_FPS)
    p.add_argument("--bitrate", default=LEGACY_BITRATE)
    p.add_argument("--audio", choices=["enable","disable"], default="disable")
    p.add_argument("--adaptive", action="store_true")
    p.add_argument("--display", default=":0")
    p.add_argument("--preset", default="")
    p.add_argument("--gop", default="30")
    p.add_argument("--qp", default="")
    p.add_argument("--tune", default="")
    p.add_argument("--pix_fmt", default="yuv420p")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    return args

def main():
    args = parse_args()

    logging.basicConfig(level=(logging.DEBUG if args.debug else logging.INFO),
                        format="%(asctime)s [%(levelname)s] %(message)s",
                        datefmt="%H:%M:%S")

    if not IS_LINUX:
        logging.critical("Hosting is Linux-only. Run this on a Linux machine.")
        return 2

    if args.gui:
        app = QApplication(sys.argv)
        _apply_dark_palette(app)
        w = HostWindow(args)
        w.show()
        rc = app.exec_()
        sys.exit(rc)
    else:
        rc = core_main(args, use_signals=True)
        sys.exit(rc)

if __name__ == "__main__":
    main()

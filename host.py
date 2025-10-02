#!/usr/bin/env python3
import os
import subprocess
import argparse
import sys
import logging
import time
import threading
import socket
import atexit
import signal
import platform as py_platform
from shutil import which

from PyQt5.QtWidgets import (
    QApplication, QWidget, QVBoxLayout, QTextEdit, QPushButton, QLabel, QHBoxLayout
)
from PyQt5.QtGui import QFont, QPalette, QColor, QKeySequence
from PyQt5.QtCore import Qt, QTimer, QObject, pyqtSignal

try:
    HERE = os.path.dirname(os.path.abspath(__file__))
    ffbin = os.path.join(HERE, "ffmpeg", "bin")
    if os.name == "nt" and os.path.exists(os.path.join(ffbin, "ffmpeg.exe")):
        os.environ["PATH"] = ffbin + os.pathsep + os.environ.get("PATH", "")
except Exception:
    pass

UDP_VIDEO_PORT = 5000
UDP_AUDIO_PORT = 6001
UDP_CONTROL_PORT = 7000
UDP_CLIPBOARD_PORT = 7002
TCP_HANDSHAKE_PORT = 7001
FILE_UPLOAD_PORT = 7003
DEFAULT_FPS = "30"
DEFAULT_BITRATE = "8M"
DEFAULT_RES = "1920x1080"

IS_WINDOWS = py_platform.system() == "Windows"
IS_LINUX   = py_platform.system() == "Linux"

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

class HostState:
    def __init__(self):
        self.video_threads = []
        self.audio_thread = None
        self.current_bitrate = DEFAULT_BITRATE
        self.last_clipboard_content = ""
        self.ignore_clipboard_update = False
        self.should_terminate = False
        self.video_thread_lock = threading.Lock()
        self.clipboard_lock = threading.Lock()
        self.handshake_sock = None
        self.control_sock = None
        self.clipboard_listener_sock = None
        self.file_upload_sock = None
        self.client_ip = None
        self.monitors = []
        self.shutdown_lock = threading.Lock()
        self.shutdown_reason = None

host_state = HostState()

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
                if s: s.close()
            except Exception:
                pass
        set_status(f"Stopping… ({reason})")

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

def is_amd_gpu_windows():
    if not IS_WINDOWS:
        return False
    return os.environ.get("LINUXPLAY_FORCE_AMF","0") == "1"

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
    for s in (
        host_state.handshake_sock,
        host_state.control_sock,
        host_state.clipboard_listener_sock,
        host_state.file_upload_sock,
    ):
        try:
            if s: s.close()
        except Exception:
            pass

def cleanup():
    stop_all()
atexit.register(cleanup)

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

def _detect_monitors_windows():
    try:
        import ctypes
        from ctypes import wintypes
        user32 = ctypes.windll.user32
        MONITORENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.POINTER(wintypes.RECT), ctypes.c_double)
        result = []
        def cb(hMonitor, hdc, lprc, lparam):
            r = lprc.contents
            w = r.right - r.left; h = r.bottom - r.top
            result.append((int(w), int(h), int(r.left), int(r.top)))
            return 1
        user32.EnumDisplayMonitors(0, 0, MONITORENUMPROC(cb), 0)
        if not result:
            SM_XVIRTUALSCREEN = 76; SM_YVIRTUALSCREEN = 77; SM_CXVIRTUALSCREEN = 78; SM_CYVIRTUALSCREEN = 79
            x = user32.GetSystemMetrics(SM_XVIRTUALSCREEN)
            y = user32.GetSystemMetrics(SM_YVIRTUALSCREEN)
            w = user32.GetSystemMetrics(SM_CXVIRTUALSCREEN)
            h = user32.GetSystemMetrics(SM_CYVIRTUALSCREEN)
            result = [(int(w), int(h), int(x), int(y))]
        return result
    except Exception as e:
        logging.error("EnumDisplayMonitors failed: %s", e)
        return []

def detect_monitors():
    if IS_WINDOWS:
        return _detect_monitors_windows()
    return _detect_monitors_linux()

def _input_ll_flags():
    return [
        "-fflags","nobuffer",
        "-use_wallclock_as_timestamps","1",
        "-thread_queue_size","64",
        "-probesize","32",
        "-analyzeduration","0",
    ]

def _output_sync_flags():
    return ["-vsync","0", "-fps_mode","passthrough"]

def _mpegts_ll_mux_flags():
    return ["-flush_packets","1", "-max_interleave_delta","0", "-muxdelay","0", "-muxpreload","0"]

def _safe_nvenc_preset(preset):
    allowed = {"default","fast","medium","slow","hp","hq","bd","ll","llhq","llhp",
               "lossless","p1","p2","p3","p4","p5","p6","p7"}
    return preset if preset in allowed else "p4"

def _amf_quality_from_preset(preset: str) -> str:
    return {
        "ultrafast":"speed","superfast":"speed","veryfast":"speed",
        "fast":"balanced","medium":"balanced",
        "slow":"quality","veryslow":"quality",
        "p1":"quality","p2":"quality","p3":"balanced","p4":"balanced","p5":"balanced","p6":"speed","p7":"speed",
        "ll":"speed","llhq":"balanced","llhp":"speed","hp":"balanced","hq":"quality","bd":"quality",
    }.get(preset or "balanced","balanced")

def _pick_encoder_args(codec: str, hwenc: str, preset: str, gop: str, qp: str, tune: str, bitrate: str, pix_fmt: str):
    codec = (codec or "h.264").lower()
    hwenc = (hwenc or "auto").lower()
    preset_l = (preset or "").strip().lower()
    extra_filters = []
    enc = []

    def ensure(name: str) -> bool:
        ok = ffmpeg_has_encoder(name)
        if not ok:
            logging.warning("Requested encoder '%s' not found in this FFmpeg build; falling back to CPU.", name)
        return ok

    if hwenc == "auto":
        if codec == "h.264":
            if has_nvidia() and ffmpeg_has_encoder("h264_nvenc"):
                hwenc = "nvenc"
            elif is_intel_cpu() and ffmpeg_has_encoder("h264_qsv"):
                hwenc = "qsv"
            elif IS_WINDOWS and is_amd_gpu_windows() and ffmpeg_has_encoder("h264_amf"):
                hwenc = "amf"
            elif has_vaapi() and ffmpeg_has_encoder("h264_vaapi"):
                hwenc = "vaapi"
            else:
                hwenc = "cpu"
        elif codec == "h.265":
            if has_nvidia() and ffmpeg_has_encoder("hevc_nvenc"):
                hwenc = "nvenc"
            elif is_intel_cpu() and ffmpeg_has_encoder("hevc_qsv"):
                hwenc = "qsv"
            elif IS_WINDOWS and is_amd_gpu_windows() and ffmpeg_has_encoder("hevc_amf"):
                hwenc = "amf"
            elif has_vaapi() and ffmpeg_has_encoder("hevc_vaapi"):
                hwenc = "vaapi"
            else:
                hwenc = "cpu"
        elif codec == "av1":
            if has_nvidia() and ffmpeg_has_encoder("av1_nvenc"):
                hwenc = "nvenc"
            elif is_intel_cpu() and ffmpeg_has_encoder("av1_qsv"):
                hwenc = "qsv"
            elif has_vaapi() and ffmpeg_has_encoder("av1_vaapi"):
                hwenc = "vaapi"
            elif IS_WINDOWS and is_amd_gpu_windows() and ffmpeg_has_encoder("av1_amf"):
                hwenc = "amf"
            else:
                hwenc = "cpu"

    if codec == "h.264":
        if hwenc == "nvenc" and ensure("h264_nvenc"):
            enc = ["-c:v","h264_nvenc","-preset", _safe_nvenc_preset(preset_l or "llhq"),
                   "-g", gop, "-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if not tune: enc += ["-tune","ll"]
            if qp: enc += ["-qp", qp]
        elif hwenc == "qsv" and ensure("h264_qsv"):
            enc = ["-c:v","h264_qsv","-g", gop, "-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if qp: enc += ["-global_quality", qp]
        elif hwenc == "amf" and IS_WINDOWS and ensure("h264_amf"):
            enc = ["-c:v","h264_amf","-quality", _amf_quality_from_preset(preset_l),
                   "-g", gop,"-b:v",bitrate,"-pix_fmt",pix_fmt]
        elif hwenc == "vaapi" and has_vaapi() and ensure("h264_vaapi"):
            extra_filters += ["-vf","format=nv12,hwupload","-vaapi_device","/dev/dri/renderD128"]
            enc = ["-c:v","h264_vaapi","-g", gop,"-bf","0","-b:v",bitrate]
            enc += ["-qp", qp or "20"]
        else:
            enc = ["-c:v","libx264","-preset",(preset_l or "ultrafast"),
                   "-g", gop,"-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt,
                   "-tune", (tune or "zerolatency")]
            if qp: enc += ["-qp", qp]

    elif codec == "h.265":
        if hwenc == "nvenc" and ensure("hevc_nvenc"):
            enc = ["-c:v","hevc_nvenc","-preset", _safe_nvenc_preset(preset_l or "p5"),
                   "-g", gop, "-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if not tune: enc += ["-tune","ll"]
            if qp: enc += ["-qp", qp]
        elif hwenc == "qsv" and ensure("hevc_qsv"):
            enc = ["-c:v","hevc_qsv","-g", gop,"-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if qp: enc += ["-global_quality", qp]
        elif hwenc == "amf" and IS_WINDOWS and ensure("hevc_amf"):
            enc = ["-c:v","hevc_amf","-quality", _amf_quality_from_preset(preset_l),
                   "-g", gop,"-b:v",bitrate,"-pix_fmt",pix_fmt]
        elif hwenc == "vaapi" and has_vaapi() and ensure("hevc_vaapi"):
            extra_filters += ["-vf","format=nv12,hwupload","-vaapi_device","/dev/dri/renderD128"]
            enc = ["-c:v","hevc_vaapi","-g", gop,"-bf","0","-b:v",bitrate]
            enc += ["-qp", qp or "20"]
        else:
            enc = ["-c:v","libx265","-preset",(preset_l or "ultrafast"),
                   "-g", gop,"-bf","0","-b:v",bitrate,
                   "-tune", (tune or "zerolatency")]
            if qp: enc += ["-qp", qp]

    elif codec == "av1":
        if hwenc == "nvenc" and ensure("av1_nvenc"):
            enc = ["-c:v","av1_nvenc","-preset", _safe_nvenc_preset(preset_l or "p5"),
                   "-g", gop,"-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if qp: enc += ["-qp", qp]
        elif hwenc == "qsv" and ensure("av1_qsv"):
            enc = ["-c:v","av1_qsv","-g", gop,"-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt]
            if qp: enc += ["-global_quality", qp]
        elif hwenc == "amf" and IS_WINDOWS and ensure("av1_amf"):
            enc = ["-c:v","av1_amf","-g", gop,"-b:v",bitrate,"-pix_fmt",pix_fmt]
            if qp: enc += ["-qp", qp]
        elif hwenc == "vaapi" and has_vaapi() and ensure("av1_vaapi"):
            extra_filters += ["-vf","format=nv12,hwupload","-vaapi_device","/dev/dri/renderD128"]
            enc = ["-c:v","av1_vaapi","-g", gop,"-bf","0","-b:v",bitrate]
            enc += ["-qp", qp or "20"]
        else:
            enc = ["-c:v","libaom-av1","-cpu-used","4","-g", gop,"-b:v",bitrate]
            if qp: enc += ["-qp", qp]

    else:
        enc = ["-c:v","libx264","-preset",(preset_l or "ultrafast"),
               "-g", gop,"-bf","0","-b:v",bitrate,"-pix_fmt",pix_fmt,
               "-tune", (tune or "zerolatency")]
        if qp: enc += ["-qp", qp]

    return extra_filters, enc

def _pick_kms_device():
    for cand in ("card0","card1","card2"):
        p = f"/dev/dri/{cand}"
        if os.path.exists(p):
            return p
    return "/dev/dri/card0"

def build_video_cmd(args, bitrate, monitor_info, video_port):
    w, h, ox, oy = monitor_info
    preset = args.preset.strip().lower() if args.preset else ""
    gop, qp, tune, pix_fmt = args.gop, args.qp, args.tune, args.pix_fmt

    if IS_WINDOWS:
        if args.capture != "auto":
            windows_demux = args.capture
        else:
            windows_demux = "ddagrab" if (ffmpeg_has_demuxer("ddagrab") or ffmpeg_has_device("ddagrab")) else "gdigrab"

        input_side = [
            *(_ffmpeg_base_cmd()),
            *(_input_ll_flags()),
            "-f", windows_demux,
            "-framerate", args.framerate,
            "-offset_x", str(ox), "-offset_y", str(oy),
            "-video_size", f"{w}x{h}",
            "-draw_mouse","0",
            "-i","desktop",
        ]

        extra_filters, encode = _pick_encoder_args(
            codec=args.encoder, hwenc=args.hwenc, preset=preset,
            gop=gop, qp=qp, tune=tune, bitrate=bitrate, pix_fmt=pix_fmt
        )

    else:
        disp = args.display
        if "." not in disp:
            disp = f"{disp}.0"

        capture_pref = (os.environ.get("LINUXPLAY_CAPTURE","auto") or "auto").lower()
        kms_available = ffmpeg_has_device("kmsgrab")
        vaapi_available = has_vaapi()

        def _vaapi_possible_for_codec():
            enc = (args.encoder or "h.264").lower()
            return (
                (enc == "h.264" and ffmpeg_has_encoder("h264_vaapi")) or
                (enc == "h.265" and ffmpeg_has_encoder("hevc_vaapi")) or
                (enc == "av1"   and ffmpeg_has_encoder("av1_vaapi"))
            )

        use_kms = False
        if capture_pref == "kmsgrab":
            use_kms = True
        elif capture_pref == "auto" and kms_available:
            if (args.hwenc in ("auto","vaapi") and vaapi_available and _vaapi_possible_for_codec()) or (args.hwenc == "cpu"):
                use_kms = True

        if use_kms:
            kms_dev = os.environ.get("LINUXPLAY_KMS_DEVICE", _pick_kms_device())
            logging.info("Linux capture: kmsgrab (%s) selected (pref=%s).", kms_dev, capture_pref)

            input_side = [
                *(_ffmpeg_base_cmd()),
                *(_input_ll_flags()),
                "-f","kmsgrab",
                "-framerate", args.framerate,
                "-device", kms_dev,
                "-i","-",
            ]

            _k_extra_filters, encode = _pick_encoder_args(
                codec=args.encoder, hwenc=args.hwenc, preset=preset,
                gop=gop, qp=qp, tune=tune, bitrate=bitrate, pix_fmt=pix_fmt
            )

            if any(x in encode for x in ("h264_vaapi","hevc_vaapi","av1_vaapi")):
                extra_filters = ["-vf", f"hwmap=derive_device=vaapi,scale_vaapi=w={w}:h={h}:format=nv12",
                                 "-vaapi_device","/dev/dri/renderD128"]
            elif args.hwenc == "cpu":
                extra_filters = ["-vf", "hwdownload,format=bgr0"]
            else:
                logging.warning("kmsgrab requested but encoder backend '%s' not supported with kmsgrab; using x11grab.", args.hwenc)
                use_kms = False

        if not use_kms:
            logging.info("Linux capture: x11grab selected (pref=%s, kms=%s).", capture_pref, kms_available)
            input_arg = f"{disp}+{ox},{oy}"
            input_side = [
                *(_ffmpeg_base_cmd()),
                *(_input_ll_flags()),
                "-f","x11grab","-draw_mouse","0",
                "-framerate", args.framerate, "-video_size", f"{w}x{h}",
                "-i", input_arg,
            ]
            extra_filters, encode = _pick_encoder_args(
                codec=args.encoder, hwenc=args.hwenc, preset=preset,
                gop=gop, qp=qp, tune=tune, bitrate=bitrate, pix_fmt=pix_fmt
            )

    output_side = _output_sync_flags()

    out = [
        *(_mpegts_ll_mux_flags()),
        "-f","mpegts",
        *_marker_opt(),
        f"udp://{host_state.client_ip}:{video_port}?pkt_size=1316&buffer_size=2048&overrun_nonfatal=1&max_delay=0"
    ]
    return input_side + output_side + (extra_filters or []) + encode + out

def _list_dshow_audio():
    try:
        out = subprocess.check_output(
            ["ffmpeg","-hide_banner","-f","dshow","-list_devices","true","-i","dummy"],
            stderr=subprocess.STDOUT, universal_newlines=True
        )
    except Exception:
        return []
    devs = []
    for line in out.splitlines():
        line = line.strip()
        if line.startswith('"') and line.endswith('"'):
            devs.append(line.strip('"'))
    return devs

def build_audio_cmd():
    if IS_WINDOWS:
        want = os.environ.get("LINUXPLAY_DSHOW_AUDIO", "").strip()
        devs = _list_dshow_audio()
        pick = None
        if want and any(want.lower() == d.lower() for d in devs):
            pick = want
        else:
            for cand in ["virtual-audio-capturer","Stereo Mix","CABLE Output","Line (VB-Audio)"]:
                for d in devs:
                    if cand.lower() in d.lower():
                        pick = d; break
                if pick: break
        if not pick and devs:
            pick = devs[0]
        if not pick:
            logging.error("No dshow audio device found; audio disabled.")
            return None
        input_side = [
            *(_ffmpeg_base_cmd()),
            *(_input_ll_flags()),
            "-f","dshow","-i", f"audio={pick}",
        ]
    else:
        mon = os.environ.get("PULSE_MONITOR","")
        if not mon and which("pactl"):
            try:
                out = subprocess.check_output(["pactl","list","short","sources"], universal_newlines=True)
                for line in out.splitlines():
                    parts = line.split()
                    if len(parts) >= 2 and ".monitor" in parts[1]:
                        mon = parts[1]; break
            except Exception:
                pass
        if not mon:
            mon = "default.monitor"
        input_side = [
            *(_ffmpeg_base_cmd()),
            *(_input_ll_flags()),
            "-f","pulse","-i", mon,
        ]

    output_side = _output_sync_flags()
    encode = [
        "-c:a","libopus","-b:a","128k",
        "-application","voip","-frame_duration","10"
    ]
    out = [
        *(_mpegts_ll_mux_flags()),
        *_marker_opt(),
        "-f","mpegts", f"udp://{host_state.client_ip}:{UDP_AUDIO_PORT}?pkt_size=1316&buffer_size=512&overrun_nonfatal=1&max_delay=0"
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
    """
    action: "down" or "up"
    name:   may be a special name (Escape, F1, Left...), a symbolic name (minus, braceleft),
            or a single literal character ('-', '_', '{', '£', etc.)
    """
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

def control_listener(sock):
    logging.info("Control listener UDP %d", UDP_CONTROL_PORT)
    while not host_state.should_terminate:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="ignore").strip()
            if not msg: continue
            tokens = msg.split()
            cmd = tokens[0] if tokens else ""
            if cmd == "MOUSE_MOVE" and len(tokens) == 3:
                _inject_mouse_move(tokens[1], tokens[2])
            elif cmd == "MOUSE_PRESS" and len(tokens) == 4:
                _inject_mouse_move(tokens[2], tokens[3]); _inject_mouse_down(tokens[1])
            elif cmd == "MOUSE_RELEASE" and len(tokens) == 2:
                _inject_mouse_up(tokens[1])
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

def adaptive_bitrate_manager(args):
    while not host_state.should_terminate:
        time.sleep(30)
        if host_state.should_terminate: break
        with host_state.video_thread_lock:
            new_bitrate = DEFAULT_BITRATE if host_state.current_bitrate != DEFAULT_BITRATE else \
                          f"{int(int(''.join(filter(str.isdigit, DEFAULT_BITRATE))) * 0.6)}M"
            if new_bitrate != host_state.current_bitrate:
                logging.info("ABR switch: %s -> %s", host_state.current_bitrate, new_bitrate)
                new_threads = []
                for i, mon in enumerate(host_state.monitors):
                    cmd = build_video_cmd(args, new_bitrate, mon, UDP_VIDEO_PORT + i)
                    t = StreamThread(cmd, f"Video {i} (ABR)")
                    t.start(); new_threads.append(t)
                for t in host_state.video_threads:
                    t.stop(); t.join()
                host_state.video_threads = new_threads
                host_state.current_bitrate = new_bitrate

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
    while host_state.client_ip is None and not host_state.should_terminate:
        time.sleep(0.1)

    if host_state.should_terminate:
        reason = host_state.shutdown_reason or "Unknown error"
        logging.critical("Exiting before start: %s", reason)
        stop_all()
        return 1

    logging.info("Client: %s", host_state.client_ip)

    with host_state.video_thread_lock:
        host_state.video_threads = []
        for i, mon in enumerate(host_state.monitors):
            cmd = build_video_cmd(args, host_state.current_bitrate, mon, UDP_VIDEO_PORT + i)
            logging.info("Starting Video %d: %s", i, " ".join(cmd))
            t = StreamThread(cmd, f"Video {i}")
            t.start(); host_state.video_threads.append(t)

    if args.audio == "enable":
        ac = build_audio_cmd()
        if ac:
            logging.info("Starting Audio: %s", " ".join(ac))
            host_state.audio_thread = StreamThread(ac, "Audio")
            host_state.audio_thread.start()

    if args.adaptive:
        threading.Thread(target=adaptive_bitrate_manager, args=(args,), daemon=True).start()

    threading.Thread(target=control_listener, args=(host_state.control_sock,), daemon=True).start()

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
    p = argparse.ArgumentParser(description="LinuxPlay Host (Linux/Windows)")
    p.add_argument("--gui", action="store_true", help="Show host GUI window.")
    p.add_argument("--encoder", choices=["none","h.264","h.265","av1"], default="none")
    p.add_argument("--hwenc", choices=["auto","cpu","nvenc","qsv","amf","vaapi"], default="auto",
                   help="Manual encoder backend selection (auto=heuristic).")
    p.add_argument("--capture", choices=["auto","ddagrab","gdigrab"], default="auto",
                   help="Windows capture: auto chooses ddagrab if available, else gdigrab.")
    p.add_argument("--framerate", default=DEFAULT_FPS)
    p.add_argument("--bitrate", default=DEFAULT_BITRATE)
    p.add_argument("--audio", choices=["enable","disable"], default="disable")
    p.add_argument("--adaptive", action="store_true")
    p.add_argument("--display", default=":0")  # ignored on Windows
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

#!/usr/bin/env python3
import sys
import os
import json
import argparse
import logging
import subprocess
import platform
import threading
import uuid
import shutil

from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout,
    QFormLayout, QComboBox, QCheckBox, QPushButton, QGroupBox,
    QLineEdit, QTextEdit, QLabel, QMessageBox, QListWidget, QScrollArea
)
from PyQt5.QtGui import QPalette, QColor
from PyQt5.QtCore import Qt, QTimer

HERE = os.path.dirname(os.path.abspath(__file__))
try:
    FFBIN = os.path.join(HERE, "ffmpeg", "bin")
    if os.name == "nt" and os.path.exists(os.path.join(FFBIN, "ffmpeg.exe")):
        os.environ["PATH"] = FFBIN + os.pathsep + os.environ.get("PATH", "")
except Exception:
    pass

IS_WINDOWS = platform.system() == "Windows"
IS_LINUX = platform.system() == "Linux"
WG_INFO_PATH = "/tmp/linuxplay_wg_info.json"
CFG_PATH = os.path.join(os.path.expanduser("~"), ".linuxplay_start_cfg.json")
LINUXPLAY_MARKER = "LinuxPlayHost"

def _client_cert_present(base_dir):
    try:
        import os
        cert_p = os.path.join(base_dir, "client_cert.pem")
        key_p  = os.path.join(base_dir, "client_key.pem")
        return os.path.exists(cert_p) and os.path.exists(key_p)
    except Exception:
        return False


def ffmpeg_ok():
    try:
        subprocess.check_output(["ffmpeg", "-hide_banner", "-version"], stderr=subprocess.STDOUT, universal_newlines=True)
        return True
    except Exception:
        return False

_FFENC_CACHE = {}
_FFDEV_CACHE = {}

def ffmpeg_has_encoder(name):
    name = name.lower()
    if name in _FFENC_CACHE:
        return _FFENC_CACHE[name]
    try:
        out = subprocess.check_output(
            ["ffmpeg","-hide_banner","-encoders"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        _FFENC_CACHE[name] = name in out
        return _FFENC_CACHE[name]
    except Exception:
        _FFENC_CACHE[name] = False
        return False

def ffmpeg_has_device(name):
    name = name.lower()
    if name in _FFDEV_CACHE:
        return _FFDEV_CACHE[name]
    try:
        out = subprocess.check_output(
            ["ffmpeg","-hide_banner","-devices"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        found=False
        for line in out.splitlines():
            s=line.strip()
            if s.startswith("d ") or s.startswith(" d "):
                parts=s.split()
                if len(parts)>=2 and parts[1]==name:
                    found=True; break
        _FFDEV_CACHE[name]=found
        return found
    except Exception:
        _FFDEV_CACHE[name]=False
        return False

def check_encoder_support(codec):
    key = codec.lower().replace(".", "")
    if key == "h264":
        names = ["h264_nvenc", "h264_qsv", "h264_amf", "libx264", "h264_vaapi"]
    elif key == "h265":
        names = ["hevc_nvenc", "hevc_qsv", "hevc_amf", "libx265", "hevc_vaapi"]
    else:
        return False
    return any(ffmpeg_has_encoder(n) for n in names)

def check_decoder_support(codec):
    try:
        output = subprocess.check_output(["ffmpeg", "-hide_banner", "-decoders"], stderr=subprocess.STDOUT, universal_newlines=True).lower()
    except Exception:
        return False
    key = codec.lower().replace(".", "")
    if key == "h264":
        return " h264 " in output or "\nh264\n" in output
    if key == "h265":
        return " hevc " in output or "\nhevc\n" in output
    return False

def load_cfg():
    try:
        with open(CFG_PATH, "r") as f:
            return json.load(f)
    except Exception:
        return {}

def save_cfg(data):
    try:
        with open(CFG_PATH, "w") as f:
            json.dump(data, f, indent=2)
    except Exception:
        pass

ENCODER_NAME_MAP = {
    ("h.264", "nvenc"): "h264_nvenc",
    ("h.264", "qsv"): "h264_qsv",
    ("h.264", "amf"): "h264_amf",
    ("h.264", "vaapi"): "h264_vaapi",
    ("h.264", "cpu"): "libx264",
    ("h.265", "nvenc"): "hevc_nvenc",
    ("h.265", "qsv"): "hevc_qsv",
    ("h.265", "amf"): "hevc_amf",
    ("h.265", "vaapi"): "hevc_vaapi",
    ("h.265", "cpu"): "libx265"
}

BACKEND_READABLE = {
    "auto": "Auto (detect in host.py)",
    "cpu": "CPU (libx264/libx265/libaom)",
    "nvenc": "NVIDIA NVENC",
    "qsv": "Intel Quick Sync (QSV)",
    "amf": "AMD AMF",
    "vaapi": "Linux VAAPI"
}

def backends_for_codec(codec):
    base = ["auto", "cpu", "nvenc", "qsv", "amf", "vaapi"]
    if IS_WINDOWS:
        if "vaapi" in base:
            base.remove("vaapi")
    else:
        if "amf" in base:
            base.remove("amf")
    pruned = []
    for b in base:
        if b in ("auto", "cpu"):
            pruned.append(b)
            continue
        enc_name = ENCODER_NAME_MAP.get((codec, b))
        if enc_name and ffmpeg_has_encoder(enc_name):
            pruned.append(b)
    items = [f"{b} - {BACKEND_READABLE[b]}" for b in pruned]
    return pruned, items

def _proc_is_running(p):
    try:
        return (p is not None) and (p.poll() is None)
    except Exception:
        return False

def _ffmpeg_running_for_us(marker=LINUXPLAY_MARKER):
    if IS_WINDOWS:
        try:
            out = subprocess.check_output(["powershell", "-NoProfile", "-Command", "Get-CimInstance Win32_Process -Filter \"Name='ffmpeg.exe'\" | Select-Object -Expand CommandLine"], stderr=subprocess.DEVNULL, universal_newlines=True)
            for line in out.splitlines():
                if marker in line:
                    return True
        except Exception:
            pass
        return False
    else:
        try:
            out = subprocess.check_output(["ps", "-eo", "pid,args"], universal_newlines=True)
            for line in out.splitlines():
                if "ffmpeg" in line and marker in line:
                    return True
        except Exception:
            pass
        return False

def _warn_ffmpeg(parent):
    QMessageBox.critical(parent, "FFmpeg not found", "FFmpeg was not found on PATH.\n\nWindows: keep ffmpeg\\bin next to the app or install FFmpeg.\nLinux: install ffmpeg via your package manager.")

class HostTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        main_layout = QVBoxLayout()
        wg_group = QGroupBox("Security Status")
        wg_layout = QVBoxLayout()
        self.wgStatus = QLabel("WireGuard: checking…")
        self.wgStatus.setWordWrap(True)
        wg_layout.addWidget(self.wgStatus)
        wg_group.setLayout(wg_layout)
        main_layout.addWidget(wg_group)
        form_group = QGroupBox("Host Configuration")
        form_layout = QFormLayout()
        self.profileCombo = QComboBox()
        self.profileCombo.addItems(["Default", "Lowest Latency", "Balanced", "High Quality"])
        self.profileCombo.currentIndexChanged.connect(self.profileChanged)
        form_layout.addRow("Profile:", self.profileCombo)
        self.encoderCombo = QComboBox()
        self.encoderCombo.addItem("none")
        if check_encoder_support("h.264"):
            self.encoderCombo.addItem("h.264")
        if check_encoder_support("h.265"):
            self.encoderCombo.addItem("h.265")
        self.encoderCombo.currentIndexChanged.connect(self._refresh_backend_choices)
        self.hwencCombo = QComboBox()
        self.framerateCombo = QComboBox()
        self.framerateCombo.addItems([
            "15", 
            "24", 
            "30", 
            "45", 
            "60", 
            "75", 
            "90", 
            "100", 
            "120", 
            "144", 
            "165", 
            "200", 
            "240", 
            "300", 
            "360"
        ])
        self.bitrateCombo = QComboBox()
        self.bitrateCombo.addItems([
            "0", 
            "50k", 
            "100k", 
            "200k", 
            "300k", 
            "400k", 
            "500k", 
            "750k", 
            "1M",
            "2M", 
            "3M", 
            "4M", 
            "5M", 
            "6M", 
            "8M", 
            "10M", 
            "12M", 
            "15M", 
            "20M", 
            "25M", 
            "30M", 
            "35M", 
            "40M", 
            "45M", 
            "50M", 
            "60M", 
            "70M", 
            "80M", 
            "90M", 
            "100M"
        ])
        self.audioCombo = QComboBox()
        self.audioCombo.addItems(["enable", "disable"])
        self.audioModeCombo = QComboBox()
        self.audioModeCombo.addItems(["Voice (low-latency)", "Music (quality)"])
        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")
        self.displayCombo = QComboBox()
        self.displayCombo.addItems([
            ":0", 
            ":1", 
            ":2", 
            ":3", 
            ":4", 
            ":5", 
            ":6"
        ])
        self.presetCombo = QComboBox()
        self.presetCombo.addItems([
            "Default", 
            "zerolatency", 
            "ultra-low-latency", 
            "realtime",
            "ultrafast", 
            "superfast", 
            "veryfast", 
            "faster", 
            "fast", 
            "medium", 
            "slow", 
            "slower", 
            "veryslow",
            "llhp", 
            "llhq", 
            "hp", 
            "hq",
            "p1", 
            "p2", 
            "p3", 
            "p4", 
            "p5", 
            "p6", 
            "p7",
            "lossless", 
            "speed", 
            "balanced", 
            "quality"
        ])

        self.gopCombo = QComboBox()
        self.gopCombo.addItems([
            "Auto", 
            "1", 
            "2", 
            "3", 
            "4", 
            "5", 
            "8", 
            "10",
            "15", 
            "20", 
            "30"])

        self.qpCombo = QComboBox()
        self.qpCombo.addItems([
            "None",
            "0", 
            "5", 
            "10", 
            "15", 
            "18", 
            "20", 
            "22", 
            "25", 
            "28", 
            "30", 
            "32", 
            "35", 
            "38", 
            "40", 
            "45", 
            "50"
        ])
        self.tuneCombo = QComboBox()
        self.tuneCombo.addItems([
            "None", 
            "auto", 
            "default",
            "low-latency", 
            "ultra-low-latency", 
            "zerolatency",
            "high-quality", 
            "high-performance", 
            "performance",
            "lossless", 
            "lossless-highperf", 
            "blu-ray"
        ])
        self.pixFmtCombo = QComboBox()
        self.pixFmtCombo.addItems([
            "yuv420p", 
            "nv12", 
            "yuv422p", 
            "yuyv422", 
            "uyvy422", 
            "yuv444p",
            "rgb0", 
            "bgr0", 
            "rgba",
            "bgra",
            "p010le", 
            "yuv420p10le", 
            "yuv422p10le", 
            "yuv444p10le",
            "p016le", 
            "yuv444p12le", 
            "yuv444p16le"
        ])
        self.debugCheck = QCheckBox("Enable Debug")
        self.captureHint = QLabel("")
        if ffmpeg_has_device("kmsgrab"):
            self.captureHint.setText("Capture: kmsgrab available (requires CAP_SYS_ADMIN; cursor not shown). Fallback: x11grab.")
        else:
            self.captureHint.setText("Capture: x11grab (kmsgrab not detected).")
        form_layout.addRow("Encoder (codec):", self.encoderCombo)
        form_layout.addRow("Encoder Backend:", self.hwencCombo)
        form_layout.addRow("Framerate:", self.framerateCombo)
        form_layout.addRow("Max Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Audio Mode:", self.audioModeCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)
        self.linuxCaptureCombo = QComboBox()
        self.linuxCaptureCombo.addItem("auto", userData="auto")
        self.linuxCaptureCombo.addItem("kmsgrab", userData="kmsgrab")
        self.linuxCaptureCombo.addItem("x11grab", userData="x11grab")
        form_layout.addRow("Linux Capture:", self.linuxCaptureCombo)
        form_layout.addRow("X Display:", self.displayCombo)
        form_layout.addRow("Preset:", self.presetCombo)
        form_layout.addRow("GOP:", self.gopCombo)
        form_layout.addRow("QP:", self.qpCombo)
        form_layout.addRow("Tune:", self.tuneCombo)
        form_layout.addRow("Pixel Format:", self.pixFmtCombo)
        form_layout.addRow("Debug:", self.debugCheck)
        form_layout.addRow("Capture:", self.captureHint)
        form_group.setLayout(form_layout)
        main_layout.addWidget(form_group)
        button_layout = QHBoxLayout()
        self.startButton = QPushButton("Start Host")
        self.startButton.clicked.connect(self.start_host)
        button_layout.addWidget(self.startButton)
        button_layout.addStretch(1)
        main_layout.addLayout(button_layout)
        self.statusLabel = QLabel("Ready")
        self.statusLabel.setStyleSheet("color: #bbb")
        main_layout.addWidget(self.statusLabel)
        main_layout.addStretch()
        self.setLayout(main_layout)
        self.host_process = None
        self._exit_watcher_thread = None
        self.pollTimerWG = QTimer(self)
        self.pollTimerWG.timeout.connect(self.refresh_wg_status)
        self.pollTimerWG.start(1500)
        self.procTimer = QTimer(self)
        self.procTimer.timeout.connect(self._poll_process_state)
        self.procTimer.start(1000)
        self.profileChanged(0)
        self._refresh_backend_choices()
        self._load_saved()
        self._update_buttons()

    def refresh_wg_status(self):
        if not IS_LINUX:
            self.wgStatus.setText("WireGuard: not supported on this OS")
            self.wgStatus.setStyleSheet("color: #bbb")
            return
        active = False
        installed = shutil.which("wg") is not None
        if installed:
            try:
                out = subprocess.check_output(["wg", "show"], stderr=subprocess.DEVNULL, universal_newlines=True)
                active = "interface:" in out
            except subprocess.CalledProcessError:
                pass
        else:
            try:
                out = subprocess.check_output(["ip", "-d", "link", "show", "type", "wireguard"], stderr=subprocess.DEVNULL, universal_newlines=True)
                active = bool(out.strip())
                installed = True
            except Exception:
                pass
        if active:
            self.wgStatus.setText("WireGuard detected and active")
            self.wgStatus.setStyleSheet("color: #7CFC00")
        elif installed:
            self.wgStatus.setText("WireGuard installed, no active tunnel")
            self.wgStatus.setStyleSheet("color: #f44")
        else:
            self.wgStatus.setText("WireGuard not installed")
            self.wgStatus.setStyleSheet("color: #f44")

    def profileChanged(self, idx):
        profile = self.profileCombo.currentText()
        if profile == "Lowest Latency":
            self.encoderCombo.setCurrentText("h.264" if self.encoderCombo.findText("h.264") != -1 else "none")
            self.framerateCombo.setCurrentText("60")
            self.bitrateCombo.setCurrentText("2M")
            self.audioCombo.setCurrentText("disable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("llhp" if self.presetCombo.findText("llhp") != -1 else "zerolatency")
            self.gopCombo.setCurrentText("1")
            self.qpCombo.setCurrentText("23")
            self.tuneCombo.setCurrentText("ultra-low-latency")
            self.pixFmtCombo.setCurrentText("yuv420p")
            self._refresh_backend_choices(preselect="auto")
        elif profile == "Balanced":
            self.encoderCombo.setCurrentText("h.264" if self.encoderCombo.findText("h.264") != -1 else "none")
            self.framerateCombo.setCurrentText("45")
            self.bitrateCombo.setCurrentText("4M")
            self.audioCombo.setCurrentText("enable")
            self.adaptiveCheck.setChecked(True)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("fast")
            self.gopCombo.setCurrentText("15")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("film")
            self.pixFmtCombo.setCurrentText("yuv420p")
            self._refresh_backend_choices(preselect="auto")
        elif profile == "High Quality":
            self.encoderCombo.setCurrentText("h.265" if self.encoderCombo.findText("h.265") != -1 else "h.264")
            self.framerateCombo.setCurrentText("30")
            self.bitrateCombo.setCurrentText("16M")
            self.audioCombo.setCurrentText("enable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("slow")
            self.gopCombo.setCurrentText("30")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("None")
            self.pixFmtCombo.setCurrentText("yuv444p")
            self._refresh_backend_choices(preselect="auto")
        else:
            self.encoderCombo.setCurrentText("h.265" if self.encoderCombo.findText("h.265") != -1 else ("h.264" if self.encoderCombo.findText("h.264") != -1 else "none"))
            self.framerateCombo.setCurrentText("30")
            self.bitrateCombo.setCurrentText("8M")
            self.audioCombo.setCurrentText("enable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("Default")
            self.gopCombo.setCurrentText("30")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("None")
            self.pixFmtCombo.setCurrentText("yuv420p")
            self._refresh_backend_choices(preselect="auto")

    def _refresh_backend_choices(self, preselect=None):
        codec = self.encoderCombo.currentText()
        self.hwencCombo.clear()
        if codec == "none":
            for key, label in [("auto", BACKEND_READABLE["auto"]), ("cpu", BACKEND_READABLE["cpu"])]:
                self.hwencCombo.addItem(f"{key} - {label}", key)
            idx = self.hwencCombo.findData("cpu")
            if idx != -1:
                self.hwencCombo.setCurrentIndex(idx)
            return
        keys, pretty = backends_for_codec(codec)
        if "auto" not in keys:
            keys.insert(0, "auto"); pretty.insert(0, f"auto - {BACKEND_READABLE['auto']}")
        if "cpu" not in keys:
            ins = 1 if "auto" in keys else 0
            keys.insert(ins, "cpu"); pretty.insert(ins, f"cpu - {BACKEND_READABLE['cpu']}")
        for k, label in zip(keys, pretty):
            self.hwencCombo.addItem(label, k)
        want = preselect or "auto"
        idx = self.hwencCombo.findData(want)
        if idx == -1:
            idx = 0
        self.hwencCombo.setCurrentIndex(idx)

    def _poll_process_state(self):
        if self.host_process is not None and self.host_process.poll() is not None:
            self.host_process = None
        self._update_buttons()

    def _update_buttons(self):
        running_host = _proc_is_running(self.host_process)
        running_ffmpeg = _ffmpeg_running_for_us()
        can_start = not (running_host or running_ffmpeg)
        self.startButton.setEnabled(can_start)
        if running_host:
            self.startButton.setToolTip("Disabled: Host is running.")
            self.statusLabel.setText("Host running…")
        elif running_ffmpeg:
            self.startButton.setToolTip("Disabled: LinuxPlay FFmpeg still running.")
            self.statusLabel.setText("LinuxPlay ffmpeg still running…")
        else:
            self.startButton.setToolTip("Start the host.")
            self.statusLabel.setText("Ready")

    def start_host(self):
        if not IS_LINUX:
            QMessageBox.critical(
                self,
                "Unsupported OS",
                "Hosting is only supported on Linux. Use the Client tab instead.",
            )
            return

        if not ffmpeg_ok():
            _warn_ffmpeg(self)
            self._update_buttons()
            return

        encoder = self.encoderCombo.currentText()
        if encoder == "none":
            QMessageBox.warning(
                self,
                "Select an encoder",
                "Encoder is set to 'none'. Pick h.264 or h.265 before starting the host.",
            )
            self._update_buttons()
            return

        framerate = self.framerateCombo.currentText()
        bitrate = self.bitrateCombo.currentText()
        audio = self.audioCombo.currentText()
        adaptive = self.adaptiveCheck.isChecked()
        display = self.displayCombo.currentText()

        preset = "" if self.presetCombo.currentText() in ("Default", "None") else self.presetCombo.currentText()
        gop = self.gopCombo.currentText()
        qp_val = self.qpCombo.currentText()
        qp = "" if qp_val in ("None", "", None) else qp_val
        tune_val = self.tuneCombo.currentText()
        tune = "" if tune_val in ("None", "", None) else tune_val
        pix_fmt = self.pixFmtCombo.currentText()
        debug = self.debugCheck.isChecked()
        hwenc = self.hwencCombo.currentData() or "auto"
    
        cmd = [
            sys.executable,
            os.path.join(HERE, "host.py"),
            "--gui",
            "--encoder", encoder,
            "--framerate", framerate,
            "--bitrate", bitrate,
            "--audio", audio,
            "--pix_fmt", pix_fmt,
            "--hwenc", hwenc,
        ]

        if adaptive:
            cmd.append("--adaptive")
        if preset:
            cmd.extend(["--preset", preset])
        if qp:
            cmd.extend(["--qp", qp])
        if tune:
            cmd.extend(["--tune", tune])
        if debug:
            cmd.append("--debug")
        cmd.extend(["--display", display])

        try:
            _gop_i = int(gop)
        except Exception:
            _gop_i = 0
        if _gop_i > 0:
            cmd.extend(["--gop", str(_gop_i)])
        elif preset.lower() in ("llhp", "zerolatency", "ultra-low-latency", "ull"):
            cmd.extend(["--gop", "1"])

        self._save_current()

        env = os.environ.copy()
        env["LINUXPLAY_MARKER"] = LINUXPLAY_MARKER
        env["LINUXPLAY_SID"] = env.get("LINUXPLAY_SID") or str(uuid.uuid4())

        _am = self.audioModeCombo.currentText().lower()
        if "music" in _am:
            env["LP_OPUS_APP"] = "audio"
            env["LP_OPUS_FD"] = "20"
        else:
            env["LP_OPUS_APP"] = "voip"
            env["LP_OPUS_FD"] = "10"

        cap_mode = getattr(self, "linuxCaptureCombo", None)
        cap_val = cap_mode.currentData() if cap_mode else "auto"
        env["LINUXPLAY_CAPTURE"] = cap_val or "auto"

        kms_dev = getattr(self, "kmsDeviceEdit", None)
        if kms_dev and hasattr(kms_dev, "text"):
            val = kms_dev.text().strip()
            if val:
                env["LINUXPLAY_KMS_DEVICE"] = val

        try:
            self.host_process = subprocess.Popen(
                cmd,
                preexec_fn=os.setsid,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=env,
            )
        except Exception as e:
            logging.error("Failed to start host: %s", e)
            QMessageBox.critical(self, "Start Host Failed", str(e))
            self.host_process = None
            self._update_buttons()
            return

        def _watch():
            try:
                _ = self.host_process.wait()
            except Exception:
                pass

            def done():
                self.host_process = None
                self._update_buttons()

            QTimer.singleShot(0, done)

        self._exit_watcher_thread = threading.Thread(
            target=_watch, name="HostExitWatcher", daemon=True
        )
        self._exit_watcher_thread.start()
        self._update_buttons()

    def _save_current(self):
        data = load_cfg()
        data["host"] = {
            "profile": self.profileCombo.currentText(),
            "encoder": self.encoderCombo.currentText(),
            "hwenc": self.hwencCombo.currentData() or "auto",
            "framerate": self.framerateCombo.currentText(),
            "bitrate": self.bitrateCombo.currentText(),
            "audio": self.audioCombo.currentText(),
            "audio_mode": self.audioModeCombo.currentText(),
            "adaptive": self.adaptiveCheck.isChecked(),
            "display": self.displayCombo.currentText(),
            "preset": self.presetCombo.currentText(),
            "gop": self.gopCombo.currentText(),
            "qp": self.qpCombo.currentText(),
            "tune": self.tuneCombo.currentText(),
            "pix_fmt": self.pixFmtCombo.currentText(),
            "capture": (self.linuxCaptureCombo.currentData() if hasattr(self, "linuxCaptureCombo") else "auto")
        }
        save_cfg(data)

    def _load_saved(self):
        cfg = load_cfg().get("host", {})
        if not cfg:
            return
        def set_combo(combo, val):
            if not val:
                return
            idx = combo.findText(val)
            if idx != -1:
                combo.setCurrentIndex(idx)
        set_combo(self.profileCombo, cfg.get("profile"))
        set_combo(self.encoderCombo, cfg.get("encoder"))
        self._refresh_backend_choices()
        saved_hwenc = cfg.get("hwenc", "auto")
        idx = self.hwencCombo.findData(saved_hwenc)
        if idx != -1:
            self.hwencCombo.setCurrentIndex(idx)
        set_combo(self.framerateCombo, cfg.get("framerate"))
        set_combo(self.bitrateCombo, cfg.get("bitrate"))
        set_combo(self.audioCombo, cfg.get("audio"))
        set_combo(self.audioModeCombo, cfg.get("audio_mode", "Voice (low-latency)"))
        self.adaptiveCheck.setChecked(bool(cfg.get("adaptive", False)))
        set_combo(self.displayCombo, cfg.get("display"))
        set_combo(self.presetCombo, cfg.get("preset"))
        set_combo(self.gopCombo, cfg.get("gop"))
        set_combo(self.qpCombo, cfg.get("qp"))
        set_combo(self.tuneCombo, cfg.get("tune"))
        set_combo(self.pixFmtCombo, cfg.get("pix_fmt"))
        if IS_LINUX and hasattr(self, "linuxCaptureCombo"):
            cap_val = cfg.get("capture", "auto")
            for i in range(self.linuxCaptureCombo.count()):
                if self.linuxCaptureCombo.itemData(i) == cap_val:
                    self.linuxCaptureCombo.setCurrentIndex(i)
                    break

class ClientTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        main_layout = QVBoxLayout()

        form_group = QGroupBox("Client Configuration")
        form_layout = QFormLayout()

        self.decoderCombo = QComboBox()
        self.decoderCombo.addItem("none")
        if check_decoder_support("h.264"):
            self.decoderCombo.addItem("h.264")
        if check_decoder_support("h.265"):
            self.decoderCombo.addItem("h.265")

        self.hwaccelCombo = QComboBox()
        self.hwaccelCombo.addItems(["auto", "cpu", "cuda", "qsv", "d3d11va", "dxva2", "vaapi"])
        if IS_WINDOWS:
            idx = self.hwaccelCombo.findText("vaapi")
            if idx != -1:
                self.hwaccelCombo.removeItem(idx)
        else:
            for item in ["d3d11va", "dxva2"]:
                idx = self.hwaccelCombo.findText(item)
                if idx != -1:
                    self.hwaccelCombo.removeItem(idx)

        self.hostIPEdit = QComboBox()
        self.hostIPEdit.setEditable(True)
        self.hostIPEdit.setToolTip("Host IP (LAN) or WireGuard tunnel IP (e.g., 10.13.13.1)")

        if IS_LINUX and os.path.exists(WG_INFO_PATH):
            try:
                with open(WG_INFO_PATH, "r") as f:
                    info = json.load(f)
                t_ip = info.get("host_tunnel_ip", "")
                if t_ip:
                    self.hostIPEdit.addItem(t_ip)
            except Exception:
                pass

        last = load_cfg().get("client", {})
        for ip in last.get("recent_ips", []):
            self.hostIPEdit.addItem(ip)

        self.audioCombo = QComboBox()
        self.audioCombo.addItems(["enable", "disable"])
        self.audioModeCombo = QComboBox()
        self.audioModeCombo.addItems(["Voice (low-latency)", "Music (quality)"])

        self.monitorField = QLineEdit("0")
        self.netCombo = QComboBox()
        self.netCombo.addItems(["auto", "lan", "wifi"])
        self.ultraCheck = QCheckBox("Ultra (LAN only)")
        self.debugCheck = QCheckBox("Enable Debug")

        self.gamepadCombo = QComboBox()
        self.gamepadCombo.addItems(["enable", "disable"])
        self.gamepadDevEdit = QLineEdit()
        self.gamepadDevEdit.setPlaceholderText("/dev/input/eventX (optional)")

        self._load_saved_client_extras()

        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("HW accel:", self.hwaccelCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Audio Mode:", self.audioModeCombo)
        form_layout.addRow("Monitor (index or 'all'):", self.monitorField)
        form_layout.addRow("Network Mode:", self.netCombo)
        form_layout.addRow("Ultra Mode:", self.ultraCheck)
        form_layout.addRow("Debug:", self.debugCheck)
        form_layout.addRow("Gamepad:", self.gamepadCombo)
        form_layout.addRow("Gamepad Device:", self.gamepadDevEdit)

        self.pinEdit = QLineEdit()
        self.pinEdit.setPlaceholderText("Enter 6-digit host PIN")
        form_layout.addRow("Host PIN:", self.pinEdit)

        _here = HERE
        self._cert_auth = _client_cert_present(_here)
        self._apply_cert_ui_state(self._cert_auth)

        self._cert_refresh_timer = QTimer(self)
        self._cert_refresh_timer.timeout.connect(self._refresh_cert_detection)
        self._cert_refresh_timer.start(2000)

        form_group.setLayout(form_layout)
        button_layout = QHBoxLayout()
        self.startButton = QPushButton("Start Client")
        self.startButton.clicked.connect(self.start_client)
        button_layout.addWidget(self.startButton)

        main_layout.addWidget(form_group)
        main_layout.addLayout(button_layout)
        main_layout.addStretch()
        self.setLayout(main_layout)

    def _apply_cert_ui_state(self, has_cert: bool):
        if has_cert:
            self.pinEdit.clear()
            self.pinEdit.setEnabled(False)
            self.pinEdit.setPlaceholderText("Client certificate detected — PIN not required")
            self.pinEdit.setToolTip("Using certificate authentication (client_cert.pem + client_key.pem).")
        else:
            self.pinEdit.setEnabled(True)
            self.pinEdit.setPlaceholderText("Enter 6-digit host PIN")
            self.pinEdit.setToolTip("Enter PIN shown on host display.")

    def _refresh_cert_detection(self):
        try:
            now_has = _client_cert_present(HERE)
        except Exception:
            now_has = False
        if now_has != getattr(self, "_cert_auth", False):
            self._cert_auth = now_has
            self._apply_cert_ui_state(now_has)
            state = "detected" if now_has else "removed"
            logging.info(f"[AUTO] Client certificate {state}, UI updated.")

    def _load_saved_client_extras(self):
        cfg = load_cfg().get("client", {})

        def set_combo(combo, val):
            if not val:
                return
            idx = combo.findText(val)
            if idx != -1:
                combo.setCurrentIndex(idx)

        set_combo(self.decoderCombo, cfg.get("decoder"))
        set_combo(self.hwaccelCombo, cfg.get("hwaccel"))
        set_combo(self.audioCombo, cfg.get("audio"))
        self.monitorField.setText(cfg.get("monitor", "0"))
        set_combo(self.netCombo, cfg.get("net", "auto"))
        self.ultraCheck.setChecked(bool(cfg.get("ultra", False)))
        self.debugCheck.setChecked(bool(cfg.get("debug", False)))
        set_combo(self.gamepadCombo, cfg.get("gamepad", "enable"))
        self.gamepadDevEdit.setText(cfg.get("gamepad_dev", ""))

    def start_client(self):
        if not ffmpeg_ok():
            _warn_ffmpeg(self)
            return

        decoder = self.decoderCombo.currentText()
        host_ip = self.hostIPEdit.currentText().strip()
        audio = self.audioCombo.currentText()
        monitor = self.monitorField.text().strip() or "0"
        debug = self.debugCheck.isChecked()
        hwaccel = self.hwaccelCombo.currentText()
        net = self.netCombo.currentText()
        ultra = self.ultraCheck.isChecked()
        gamepad = self.gamepadCombo.currentText()
        gamepad_dev = self.gamepadDevEdit.text().strip() or None
        pin = self.pinEdit.text().strip()
        if getattr(self, "_cert_auth", False):
            pin = ""

        if not host_ip:
            self.hostIPEdit.setEditText("Enter host IP or WG tunnel IP")
            return

        cfg = load_cfg()
        client_cfg = cfg.get("client", {})
        rec = client_cfg.get("recent_ips", [])
        if host_ip and host_ip not in rec:
            rec = [host_ip] + rec[:4]

        client_cfg.update({
            "recent_ips": rec,
            "decoder": decoder,
            "hwaccel": hwaccel,
            "audio": audio,
            "monitor": monitor,
            "debug": bool(debug),
            "net": net,
            "ultra": bool(ultra),
            "gamepad": gamepad,
            "gamepad_dev": gamepad_dev,
            "pin": pin
        })
        cfg["client"] = client_cfg
        save_cfg(cfg)

        cmd = [
            sys.executable, os.path.join(HERE, "client.py"),
            "--decoder", decoder,
            "--host_ip", host_ip,
            "--audio", audio,
            "--monitor", monitor,
            "--hwaccel", hwaccel,
            "--net", net,
            "--gamepad", gamepad
        ]
        if gamepad_dev:
            cmd.extend(["--gamepad_dev", gamepad_dev])
        if ultra:
            cmd.append("--ultra")
        if debug:
            cmd.append("--debug")
        if pin:
            cmd.extend(["--pin", pin])

        try:
            subprocess.Popen(cmd)
        except Exception as e:
            logging.error(f"Failed to start client: {e}")
            QMessageBox.critical(self, "Start Client Failed", str(e))

class HelpTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout()

        help_text = (
            "<h1>LinuxPlay Help</h1>"
            "<p><b>LinuxPlay</b> provides ultra-low-latency desktop streaming using FFmpeg over UDP, "
            "with TCP used for session handshakes and UDP channels for input, clipboard, and optional audio.</p>"

            "<h2>Security</h2>"
            "<p>For internet (WAN) streaming, it is strongly recommended to tunnel all traffic through "
            "<b>WireGuard</b> on the host system. Clients should connect using the tunnel’s internal IP. "
            "On trusted local networks (LAN), this step can be safely skipped.</p>"

            "<h2>Capture Backends</h2>"
            "<ul>"
            "<li><b>kmsgrab</b> (KMS/DRM): Provides the lowest capture latency but requires elevated privileges. "
            "Grant permission with:<br><code>sudo setcap cap_sys_admin+ep $(which ffmpeg)</code>."
            "Note that the hardware cursor is not drawn by kmsgrab.</li>"
            "<li><b>x11grab</b>: Compatible with most X11 sessions; easier to set up but slightly higher latency.</li>"
            "</ul>"

            "<h2>Platform Support</h2>"
            "<p>The <b>Host</b> is supported on Linux only. Clients are available for Linux and Windows. "
            "macOS clients may function via compatibility layers but are not officially supported.</p>"

            "<h2>Performance Tips</h2>"
            "<ul>"
            "<li>Enable <b>Ultra Mode</b> for LAN use only; it disables internal buffering for minimum delay.</li>"
            "<li>Recommended baseline for smooth playback: "
            "<code>H.264</code> codec, preset <code>llhq</code> or <code>ultrafast</code>, GOP <code>10</code>, "
            "audio disabled (optional), and moderate bitrates (e.g. 8–12&nbsp;Mbps for 1080p).</li>"
            "<li>Select your encoder backend explicitly — NVENC, QSV, AMF, VAAPI, or CPU — "
            "to ensure consistent performance across sessions.</li>"
            "</ul>"

            "<h2>General Notes</h2>"
            "<ul>"
            "<li>Multi-monitor streaming is supported. Choose a specific monitor index or <b>all</b> to capture every display.</li>"
            "<li>The host window includes a Stop button; closing it also terminates the active session safely.</li>"
            "<li>Clipboard sync and drag-and-drop are available in compatible clients.</li>"
            "</ul>"
        )

        help_view = QTextEdit()
        help_view.setReadOnly(True)
        help_view.setHtml(help_text)
        layout.addWidget(help_view)
        self.setLayout(layout)

class SponsorsTab(QWidget):
    def __init__(self):
        super().__init__()

        sponsors = [
            {"name": "gw3583", "label": "$50.00", "type": "one-time"},
            {"name": "Ulthes", "label": "$25.00", "type": "one-time"},
            {"name": "None yet", "label": "$0", "type": "monthly"},
        ]

        monthly_sponsors = [s for s in sponsors if s["type"] == "monthly"]
        onetime_sponsors = [s for s in sponsors if s["type"] == "one-time"]

        total_onetime = sum(float(s["label"].replace("$", "")) for s in onetime_sponsors if "$" in s["label"])
        total_monthly = sum(float(s["label"].replace("$", "")) for s in monthly_sponsors if "$" in s["label"])

        layout = QVBoxLayout(spacing=8)

        title = QLabel("<h2>LinuxPlay Sponsors Wall</h2>")
        title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(title)

        tagline = QLabel("Recognising everyone who helps push a Linux-first streaming stack forward.")
        tagline.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(tagline)

        summary = QLabel(
            f"Monthly sponsors: {len(monthly_sponsors)} (${total_monthly:.2f}) | "
            f"One-time sponsors: {len(onetime_sponsors)} (${total_onetime:.2f})"
        )
        summary.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(summary)

        columns = QHBoxLayout(spacing=20)

        def make_list(title_text, sponsors_list):
            col = QVBoxLayout()
            label = QLabel(f"<b>{title_text}</b>")
            label.setAlignment(Qt.AlignmentFlag.AlignCenter)
            col.addWidget(label)

            list_widget = QListWidget()
            list_widget.setSelectionMode(QListWidget.SelectionMode.NoSelection)
            list_widget.setAlternatingRowColors(True)

            if sponsors_list:
                max_name_len = max(len(s["name"]) for s in sponsors_list)
                for s in sponsors_list:
                    line = f"{s['name'].ljust(max_name_len)}  |  {s['label']}"
                    list_widget.addItem(line)
            else:
                list_widget.addItem("- None yet -")

            col.addWidget(list_widget)
            return col

        columns.addLayout(make_list("Monthly Sponsors", monthly_sponsors))
        columns.addLayout(make_list("One-time Sponsors", onetime_sponsors))
        layout.addLayout(columns)

        thanks = QLabel(
            "<i>If your name is listed here, you're part of LinuxPlay.<br>"
            "Thank you for backing this project.</i>"
        )
        thanks.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(thanks)
        layout.addStretch()

        self.setLayout(layout)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("LinuxPlay")

        self.tabs = QTabWidget()
        self.clientTab = ClientTab()
        self.helpTab = HelpTab()
        self.sponsorsTab = SponsorsTab()

        if IS_LINUX:
            self.hostTab = HostTab()
            self.tabs.addTab(self.hostTab, "Host")

        self.tabs.addTab(self.clientTab, "Client")
        self.tabs.addTab(self.helpTab, "Help")
        self.tabs.addTab(self.sponsorsTab, "Sponsors")

        main_layout = QVBoxLayout()
        main_layout.addWidget(self.tabs)
        self.setLayout(main_layout)

    def closeEvent(self, event):
        event.accept()

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--debug", action="store_true")
    args, _ = parser.parse_known_args()
    logging.basicConfig(level=(logging.DEBUG if args.debug else logging.INFO), format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%H:%M:%S")
    app = QApplication(sys.argv)
    app.setStyle("Fusion")
    palette = app.palette()
    palette.setColor(QPalette.Window, QColor(53, 53, 53))
    palette.setColor(QPalette.WindowText, Qt.white)
    palette.setColor(QPalette.Base, QColor(35, 35, 35))
    palette.setColor(QPalette.AlternateBase, QColor(53, 53, 53))
    palette.setColor(QPalette.ToolTipBase, Qt.white)
    palette.setColor(QPalette.ToolTipText, Qt.white)
    palette.setColor(QPalette.Text, Qt.white)
    palette.setColor(QPalette.Button, QColor(53, 53, 53))
    palette.setColor(QPalette.ButtonText, Qt.white)
    palette.setColor(QPalette.Highlight, QColor(38, 128, 218))
    palette.setColor(QPalette.HighlightedText, Qt.black)
    app.setPalette(palette)
    w = StartWindow()
    w.resize(860, 620)
    w.show()
    sys.exit(app.exec_())

if __name__ == "__main__":
    main()

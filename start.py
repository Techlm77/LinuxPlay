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

from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout, QFormLayout,
    QComboBox, QCheckBox, QPushButton, QGroupBox, QLineEdit, QTextEdit, QLabel, QMessageBox
)
from PyQt5.QtGui import QPalette, QColor, QPixmap
from PyQt5.QtCore import Qt, QTimer

try:
    HERE = os.path.dirname(os.path.abspath(__file__))
    FFBIN = os.path.join(HERE, "ffmpeg", "bin")
    if os.name == "nt" and os.path.exists(os.path.join(FFBIN, "ffmpeg.exe")):
        os.environ["PATH"] = FFBIN + os.pathsep + os.environ.get("PATH", "")
except Exception:
    pass

IS_WINDOWS = platform.system() == "Windows"
IS_LINUX   = platform.system() == "Linux"

WG_INFO_PATH = "/tmp/linuxplay_wg_info.json"  # {"host_tunnel_ip": "10.13.13.1"}
WG_QR_PATH   = "/tmp/linuxplay_wg_peer.png"   # QR image for mobile client

CFG_PATH = os.path.join(os.path.expanduser("~"), ".linuxplay_start_cfg.json")

LINUXPLAY_MARKER = "LinuxPlayHost"

def ffmpeg_ok():
    try:
        subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-version"],
            stderr=subprocess.STDOUT, universal_newlines=True
        )
        return True
    except Exception:
        return False

def ffmpeg_has_encoder(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-encoders"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        return name.lower() in out
    except Exception:
        return False

def ffmpeg_supports_demuxer(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-h", f"demuxer={name}"],
            stderr=subprocess.STDOUT, universal_newlines=True
        )
        return (name in out.lower())
    except Exception:
        return False

def ffmpeg_has_device(name: str) -> bool:
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-devices"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
        for line in out.splitlines():
            s = line.strip().lower()
            if s.startswith("d ") or s.startswith(" d "):
                parts = s.split()
                if len(parts) >= 2 and parts[1] == name.lower():
                    return True
        return False
    except Exception:
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
        output = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-decoders"],
            stderr=subprocess.STDOUT, universal_newlines=True
        ).lower()
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
    ("h.264", "qsv"):   "h264_qsv",
    ("h.264", "amf"):   "h264_amf",
    ("h.264", "vaapi"): "h264_vaapi",
    ("h.264", "cpu"):   "libx264",

    ("h.265", "nvenc"): "hevc_nvenc",
    ("h.265", "qsv"):   "hevc_qsv",
    ("h.265", "amf"):   "hevc_amf",
    ("h.265", "vaapi"): "hevc_vaapi",
    ("h.265", "cpu"):   "libx265",
}

BACKEND_READABLE = {
    "auto": "Auto (detect in host.py)",
    "cpu":  "CPU (libx264/libx265/libaom)",
    "nvenc":"NVIDIA NVENC",
    "qsv":  "Intel Quick Sync (QSV)",
    "amf":  "AMD AMF",
    "vaapi":"Linux VAAPI",
}

def backends_for_codec(codec: str):
    base = ["auto", "cpu", "nvenc", "qsv", "amf", "vaapi"]
    if IS_WINDOWS:
        if "vaapi" in base: base.remove("vaapi")
    else:
        if "amf" in base: base.remove("amf")
    pruned = []
    for b in base:
        if b in ("auto", "cpu"):
            pruned.append(b); continue
        enc_name = ENCODER_NAME_MAP.get((codec, b))
        if enc_name and ffmpeg_has_encoder(enc_name):
            pruned.append(b)
    items = [f"{b} - {BACKEND_READABLE[b]}" for b in pruned]
    return pruned, items

def _proc_is_running(p) -> bool:
    try:
        return (p is not None) and (p.poll() is None)
    except Exception:
        return False

def _ffmpeg_running_for_us(marker: str = LINUXPLAY_MARKER) -> bool:
    if IS_WINDOWS:
        try:
            out = subprocess.check_output(
                ["powershell", "-NoProfile", "-Command",
                 "Get-CimInstance Win32_Process -Filter \"Name='ffmpeg.exe'\" | "
                 "Select-Object -Expand CommandLine"],
                stderr=subprocess.DEVNULL, universal_newlines=True
            )
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

class HostTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)

        main_layout = QVBoxLayout()

        wg_group = QGroupBox("Secure (WireGuard)")
        wg_layout = QVBoxLayout()
        self.useWG = QCheckBox("Launch with WireGuard (recommended for WAN)")
        self.useWG.setChecked(False)
        self.wgStatus = QLabel("WireGuard: idle")
        self.wgStatus.setWordWrap(True)
        self.wgQR = QLabel()
        self.wgQR.setAlignment(Qt.AlignCenter)
        self.wgQR.setVisible(False)
        self.btnWGSetup = QPushButton("Set up WireGuard (show QR)")
        self.btnWGTeardown = QPushButton("Tear down WireGuard")
        self.btnWGSetup.clicked.connect(self.setup_wireguard)
        self.btnWGTeardown.clicked.connect(self.teardown_wireguard)
        wg_layout.addWidget(self.useWG)
        wg_layout.addWidget(self.wgStatus)
        wg_layout.addWidget(self.btnWGSetup)
        wg_layout.addWidget(self.btnWGTeardown)
        wg_layout.addWidget(self.wgQR)
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
            "15","24","30","45","60","75","90","100","120","144","165","200","240","300","360"
        ])

        self.bitrateCombo = QComboBox()
        self.bitrateCombo.addItems([
            "0","50k","100k","200k","300k","400k","500k","750k",
            "1M","1.5M","2M","3M","4M","5M","6M","8M","10M","12M",
            "15M","20M","25M","30M","35M","40M","45M","50M",
            "60M","70M","80M","90M","100M","125M","150M","200M","250M","300M"
        ])

        self.audioCombo = QComboBox()
        self.audioCombo.addItems([
            "enable",
            "disable"
        ])

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")

        self.displayCombo = QComboBox()
        self.displayCombo.addItems([":0",":1",":2",":3",":4"])

        self.presetCombo = QComboBox()
        self.presetCombo.addItems([
            "Default",
            "ultrafast","superfast","veryfast","faster","fast","medium","slow","slower","veryslow",
            "llhp","llhq","hp","hq","p1","p2","p3","p4","p5","p6","p7",
            "lossless","speed","balanced","quality"
        ])

        self.gopCombo = QComboBox()
        self.gopCombo.addItems([
            "1","2","3","5","8","10","12","15","20","30","45","60","90","120","180","240","360"
        ])

        self.qpCombo = QComboBox()
        self.qpCombo.addItems([
            "None","0","3","5","8","10","12","15","18","20","22","23","25","28","30","32","35",
            "38","40","42","45","48","50","52","55","58","60"
        ])

        self.tuneCombo = QComboBox()
        self.tuneCombo.addItems([
            "None",
            "zerolatency",
            "film",
            "animation",
            "grain",
            "psnr",
            "ssim",
            "fastdecode",
            "stillimage",
            "stupidlyfast"
        ])

        self.pixFmtCombo = QComboBox()
        self.pixFmtCombo.addItems([
            "yuv420p",
            "nv12",
            "yuv422p",
            "yuv444p",
            "p010le",
            "p016le",
            "rgb0",
            "bgr0",
            "rgba"
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
        form_layout.addRow("Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)

        self.linuxCaptureCombo = QComboBox()
        self.linuxCaptureCombo.addItem("auto", userData="auto")
        self.linuxCaptureCombo.addItem("kmsgrab", userData="kmsgrab")
        self.linuxCaptureCombo.addItem("x11grab", userData="x11grab")
        form_layout.addRow("Linux Capture:", self.linuxCaptureCombo)
        form_layout.addRow("X Display (fallback):", self.displayCombo)

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

    def setup_wireguard(self):
        if not IS_LINUX:
            return
        self.wgStatus.setText("WireGuard: configuring…")
        self.wgQR.setVisible(False)
        try:
            subprocess.Popen(["/bin/bash", "-c", "sudo /usr/local/bin/linuxplay-wg-setup-host.sh"])
        except Exception as e:
            self.wgStatus.setText(f"WireGuard: setup failed: {e}")

    def teardown_wireguard(self):
        if not IS_LINUX:
            return
        self.wgStatus.setText("WireGuard: tearing down…")
        self.wgQR.setVisible(False)
        try:
            subprocess.Popen(["/bin/bash", "-c", "sudo /usr/local/bin/linuxplay-wg-teardown.sh"])
        except Exception as e:
            self.wgStatus.setText(f"WireGuard: teardown failed: {e}")

    def refresh_wg_status(self):
        if not IS_LINUX:
            return
        qr_exists = os.path.exists(WG_QR_PATH)
        info_exists = os.path.exists(WG_INFO_PATH)
        if qr_exists or info_exists:
            self.wgStatus.setText("WireGuard: ready")
            if qr_exists:
                pix = QPixmap(WG_QR_PATH)
                if not pix.isNull():
                    self.wgQR.setPixmap(pix.scaledToWidth(260, Qt.SmoothTransformation))
                    self.wgQR.setVisible(True)
        else:
            self.wgStatus.setText("WireGuard: idle")
            self.wgQR.setVisible(False)

    def profileChanged(self, idx):
        profile = self.profileCombo.currentText()

        if profile == "Lowest Latency":
            self.encoderCombo.setCurrentText(
                "h.264" if self.encoderCombo.findText("h.264") != -1 else "none"
            )
            self.framerateCombo.setCurrentText("60")
            self.bitrateCombo.setCurrentText("2M")
            self.audioCombo.setCurrentText("disable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText(
                "llhq" if self.presetCombo.findText("llhq") != -1 else "ultrafast"
            )
            self.gopCombo.setCurrentText("8")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("zerolatency")
            self.pixFmtCombo.setCurrentText("yuv420p")
            self._refresh_backend_choices(preselect="auto")

        elif profile == "Balanced":
            self.encoderCombo.setCurrentText(
                "h.264" if self.encoderCombo.findText("h.264") != -1 else "none"
            )
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
            self.encoderCombo.setCurrentText(
                "h.265" if self.encoderCombo.findText("h.265") != -1 else "h.264"
            )
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
            self.encoderCombo.setCurrentText(
                "h.265"
                if self.encoderCombo.findText("h.265") != -1
                else ("h.264" if self.encoderCombo.findText("h.264") != -1 else "none")
            )
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

    def _refresh_backend_choices(self, preselect: str = None):
        codec = self.encoderCombo.currentText()
        self.hwencCombo.clear()
        if codec == "none":
            items = [("auto", BACKEND_READABLE["auto"]), ("cpu", BACKEND_READABLE["cpu"])]
            for key, label in items:
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
                "Hosting is only supported on Linux. Use the Client tab instead."
            )
            return

        if not ffmpeg_ok():
            self._warn_ffmpeg()
            self._update_buttons()
            return

        encoder  = self.encoderCombo.currentText()

        if encoder == "none":
            QMessageBox.warning(
                self,
                "Select an encoder",
                "Encoder is set to 'none'. Pick h.264 or h.265 before starting the host."
            )
            self._update_buttons()
            return

        framerate = self.framerateCombo.currentText()
        bitrate   = self.bitrateCombo.currentText()
        audio     = self.audioCombo.currentText()
        adaptive  = self.adaptiveCheck.isChecked()
        display   = self.displayCombo.currentText()
        preset    = "" if self.presetCombo.currentText() == "Default" else self.presetCombo.currentText()
        gop       = self.gopCombo.currentText()
        qp        = "" if self.qpCombo.currentText() == "None" else self.qpCombo.currentText()
        tune      = "" if self.tuneCombo.currentText() == "None" else self.tuneCombo.currentText()
        pix_fmt   = self.pixFmtCombo.currentText()
        debug     = self.debugCheck.isChecked()
        hwenc     = self.hwencCombo.currentData() or "auto"

        cmd = [
            sys.executable, os.path.join(HERE, "host.py"),
            "--gui",
            "--encoder", encoder,
            "--framerate", framerate,
            "--bitrate", bitrate,
            "--audio", audio,
            "--gop", gop,
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

        self._save_current()

        env = os.environ.copy()
        env["LINUXPLAY_MARKER"] = LINUXPLAY_MARKER
        env["LINUXPLAY_SID"] = env.get("LINUXPLAY_SID") or str(uuid.uuid4())

        cap_mode = getattr(self, "linuxCaptureCombo", None)
        cap_val = cap_mode.currentData() if cap_mode else "auto"
        env["LINUXPLAY_CAPTURE"] = cap_val or "auto"

        try:
            self.host_process = subprocess.Popen(
                cmd,
                preexec_fn=os.setsid,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=env
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
            target=_watch,
            name="HostExitWatcher",
            daemon=True
        )
        self._exit_watcher_thread.start()

        self._update_buttons()

    def _warn_ffmpeg(self):
        QMessageBox.critical(self, "FFmpeg not found",
            "FFmpeg was not found on PATH.\n\n"
            "Windows: keep ffmpeg\\bin next to the app or install FFmpeg.\n"
            "Linux: install ffmpeg via your package manager.")

    def _save_current(self):
        data = load_cfg()
        data["host"] = {
            "profile": self.profileCombo.currentText(),
            "encoder": self.encoderCombo.currentText(),
            "hwenc":   self.hwencCombo.currentData() or "auto",
            "framerate": self.framerateCombo.currentText(),
            "bitrate": self.bitrateCombo.currentText(),
            "audio": self.audioCombo.currentText(),
            "adaptive": self.adaptiveCheck.isChecked(),
            "display": self.displayCombo.currentText(),
            "preset": self.presetCombo.currentText(),
            "gop": self.gopCombo.currentText(),
            "qp": self.qpCombo.currentText(),
            "tune": self.tuneCombo.currentText(),
            "pix_fmt": self.pixFmtCombo.currentText(),
            "wg": bool(getattr(self, "useWG", None) and self.useWG.isChecked()),
            "capture": (getattr(self, "linuxCaptureCombo", None).currentData()
                        if IS_LINUX and hasattr(self, "linuxCaptureCombo") else "auto")
        }
        save_cfg(data)

    def _load_saved(self):
        cfg = load_cfg().get("host", {})
        if not cfg: return
        def set_combo(combo: QComboBox, val: str):
            if not val: return
            idx = combo.findText(val)
            if idx != -1: combo.setCurrentIndex(idx)
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
        self.adaptiveCheck.setChecked(bool(cfg.get("adaptive", False)))
        set_combo(self.displayCombo, cfg.get("display"))
        set_combo(self.presetCombo, cfg.get("preset"))
        set_combo(self.gopCombo, cfg.get("gop"))
        set_combo(self.qpCombo, cfg.get("qp"))
        set_combo(self.tuneCombo, cfg.get("tune"))
        set_combo(self.pixFmtCombo, cfg.get("pix_fmt"))
        if IS_LINUX and hasattr(self, "useWG"):
            self.useWG.setChecked(bool(cfg.get("wg", False)))
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
        if check_decoder_support("h.264"): self.decoderCombo.addItem("h.264")
        if check_decoder_support("h.265"): self.decoderCombo.addItem("h.265")

        self.hwaccelCombo = QComboBox()
        self.hwaccelCombo.addItems(["auto","cpu","cuda","qsv","d3d11va","dxva2","vaapi"])
        if IS_WINDOWS:
            idx = self.hwaccelCombo.findText("vaapi")
            if idx != -1: self.hwaccelCombo.removeItem(idx)
        else:
            for item in ["d3d11va","dxva2"]:
                idx = self.hwaccelCombo.findText(item)
                if idx != -1: self.hwaccelCombo.removeItem(idx)

        self.hostIPEdit = QComboBox()
        self.hostIPEdit.setEditable(True)
        self.hostIPEdit.setToolTip("Host IP (LAN) or WireGuard tunnel IP (e.g., 10.13.13.1)")
        if IS_LINUX and os.path.exists(WG_INFO_PATH):
            try:
                with open(WG_INFO_PATH, "r") as f:
                    info = json.load(f)
                t_ip = info.get("host_tunnel_ip","")
                if t_ip:
                    self.hostIPEdit.addItem(t_ip)
            except Exception:
                pass

        last = load_cfg().get("client", {})
        for ip in last.get("recent_ips", []):
            self.hostIPEdit.addItem(ip)

        self.audioCombo = QComboBox(); self.audioCombo.addItems(["enable","disable"])
        self.monitorField = QLineEdit("0")
        self.netCombo = QComboBox(); self.netCombo.addItems(["auto","lan","wifi"])
        self.ultraCheck = QCheckBox("Ultra (LAN only)")
        self.debugCheck = QCheckBox("Enable Debug")

        self._load_saved_client_extras()

        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("HW accel:", self.hwaccelCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Monitor (index or 'all'):", self.monitorField)
        form_layout.addRow("Network Mode:", self.netCombo)
        form_layout.addRow("Ultra Mode:", self.ultraCheck)
        form_layout.addRow("Debug:", self.debugCheck)
        form_group.setLayout(form_layout)

        button_layout = QHBoxLayout()
        self.startButton = QPushButton("Start Client")
        self.startButton.clicked.connect(self.start_client)
        button_layout.addWidget(self.startButton)

        main_layout.addWidget(form_group)
        main_layout.addLayout(button_layout)
        main_layout.addStretch()
        self.setLayout(main_layout)

    def _load_saved_client_extras(self):
        cfg = load_cfg().get("client", {})
        def set_combo_text(c: QComboBox, val: str):
            if not val: return
            idx = c.findText(val)
            if idx != -1: c.setCurrentIndex(idx)
        set_combo_text(self.audioCombo, cfg.get("audio", "disable"))
        set_combo_text(self.hwaccelCombo, cfg.get("hwaccel", "auto"))
        set_combo_text(self.decoderCombo, cfg.get("decoder", "none"))
        set_combo_text(self.netCombo, cfg.get("net", "auto"))
        self.ultraCheck.setChecked(bool(cfg.get("ultra", False)))
        self.monitorField.setText(cfg.get("monitor", "0"))

    def start_client(self):
        if not ffmpeg_ok():
            self._warn_ffmpeg()
            return

        decoder = self.decoderCombo.currentText()
        host_ip = self.hostIPEdit.currentText().strip()
        audio   = self.audioCombo.currentText()
        monitor = self.monitorField.text().strip() or "0"
        debug   = self.debugCheck.isChecked()
        hwaccel = self.hwaccelCombo.currentText()
        net     = self.netCombo.currentText()
        ultra   = self.ultraCheck.isChecked()

        if not host_ip:
            self.hostIPEdit.setEditText("Enter host IP or WG tunnel IP")
            return

        cfg = load_cfg()
        client_cfg = cfg.get("client", {})
        rec = client_cfg.get("recent_ips", [])
        if host_ip and host_ip not in rec:
            rec = [host_ip] + rec
            rec = rec[:5]
        client_cfg.update({
            "recent_ips": rec,
            "decoder": decoder,
            "hwaccel": hwaccel,
            "audio": audio,
            "monitor": monitor,
            "debug": bool(debug),
            "net": net,
            "ultra": bool(ultra),
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
            "--net", net
        ]
        
        if ultra:
            cmd.append("--ultra")
        if debug:
            cmd.append("--debug")
        try:
            subprocess.Popen(cmd)
        except Exception as e:
            logging.error("Failed to start client: %s", e)
            QMessageBox.critical(self, "Start Client Failed", str(e))

    def _warn_ffmpeg(self):
        QMessageBox.critical(self, "FFmpeg not found",
            "FFmpeg was not found on PATH.\n\n"
            "Windows: keep ffmpeg\\bin next to the app or install FFmpeg.\n"
            "Linux: install ffmpeg via your package manager.")

class HelpTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout()
        help_text = """
        <h1>LinuxPlay Help</h1>
        <p>LinuxPlay streams your desktop with ultra-low latency using FFmpeg over UDP, plus a TCP handshake and UDP control/clipboard.</p>
        <h2>Security</h2>
        <p>For WAN use, enable <b>WireGuard</b> on Linux hosts and connect clients to the tunnel IP. On LAN you can skip it.</p>
        <h2>Notes</h2>
        <ul>
          <li>Linux capture can use <b>kmsgrab</b> (KMS/DRM) or <b>x11grab</b>. kmsgrab generally requires <code>CAP_SYS_ADMIN</code> on the ffmpeg binary (e.g. <code>sudo setcap cap_sys_admin+ep $(which ffmpeg)</code>) and does not draw the cursor.</li>
          <li>Hosting is Linux-only. Windows and macOS can run the client and connect to a Linux host.</li>
          <li>Multi-monitor is supported; choose a monitor index or "all" on the client.</li>
          <li><b>Ultra mode</b>: tick "Ultra (LAN only)" in Client; the client auto-disables it on Wi-Fi/WAN.</li>
          <li>For lowest latency try: h.264, preset llhq (or ultrafast), GOP 10, audio disabled, and keep bitrates modest.</li>
          <li>Use the <b>Encoder Backend</b> to select NVENC/QSV/AMF/VAAPI/CPU explicitly.</li>
          <li>The Host opens in its own window; use that window's Stop button (or close it) to end the session.</li>
        </ul>
        """
        help_view = QTextEdit()
        help_view.setReadOnly(True)
        help_view.setHtml(help_text)
        layout.addWidget(help_view)
        self.setLayout(layout)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("LinuxPlay")
        self.tabs = QTabWidget()
        self.clientTab = ClientTab()
        self.helpTab = HelpTab()
        if IS_LINUX:
            self.hostTab = HostTab()
            self.tabs.addTab(self.hostTab, "Host")
        self.tabs.addTab(self.clientTab, "Client")
        self.tabs.addTab(self.helpTab, "Help")
        main_layout = QVBoxLayout()
        main_layout.addWidget(self.tabs)
        self.setLayout(main_layout)
        
    def closeEvent(self, event):
        event.accept()

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--debug", action="store_true")
    args, _ = parser.parse_known_args()

    logging.basicConfig(level=(logging.DEBUG if args.debug else logging.INFO),
                        format="%(asctime)s [%(levelname)s] %(message)s",
                        datefmt="%H:%M:%S")

    app = QApplication(sys.argv)
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

    window = StartWindow()
    window.resize(780, 760)
    window.show()
    sys.exit(app.exec_())

if __name__ == "__main__":
    main()

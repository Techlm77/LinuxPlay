#!/usr/bin/env python3
import sys
import subprocess
import os
import signal
from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout, QFormLayout,
    QComboBox, QCheckBox, QPushButton, QGroupBox, QLineEdit, QTextEdit, QLabel
)
from PyQt5.QtGui import QPalette, QColor
from PyQt5.QtCore import Qt
from shutil import which

def has_nvidia():
    from shutil import which as shwhich
    return shwhich("nvidia-smi") is not None

def has_vaapi():
    return os.path.exists("/dev/dri/renderD128")

def check_encoder_support(codec):
    try:
        output = subprocess.check_output(["ffmpeg", "-encoders"], stderr=subprocess.DEVNULL).decode()
    except Exception:
        return False
    if codec == "h265":
        return any(x in output for x in ["hevc_nvenc", "hevc_vaapi", "hevc_qsv"])
    elif codec == "av1":
        return any(x in output for x in ["av1_nvenc", "av1_vaapi", "av1_qsv"])
    elif codec == "h.264":
        return any(x in output for x in ["h264_nvenc", "h264_vaapi", "h264_qsv"])
    return False

def check_decoder_support(codec):
    try:
        output = subprocess.check_output(["ffmpeg", "-decoders"], stderr=subprocess.DEVNULL).decode()
    except Exception:
        return False
    if codec == "h265":
        return any(x in output for x in ["hevc_nvdec", "hevc_vaapi", "hevc_qsv", "hevc_cuvid"])
    elif codec == "av1":
        return any(x in output for x in ["av1_nvdec", "av1_vaapi", "av1_qsv"])
    elif codec == "h.264":
        return any(x in output for x in ["h264_nvdec", "h264_vaapi", "h264_qsv", "h264_cuvid"])
    return False

class HostTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)

        main_layout = QVBoxLayout()

        self.profileCombo = QComboBox()
        self.profileCombo.setEditable(False)
        self.profileCombo.addItems(["Default", "Lowest Latency", "Balanced", "High Quality"])
        self.profileCombo.currentIndexChanged.connect(self.profileChanged)

        form_group = QGroupBox("Host Configuration")
        form_layout = QFormLayout()

        form_layout.addRow("Profile:", self.profileCombo)

        self.encoderCombo = QComboBox()
        self.encoderCombo.setEditable(False)
        self.encoderCombo.addItem("none")
        if check_encoder_support("h.264"):
            self.encoderCombo.addItem("h.264")
        if check_encoder_support("h265"):
            self.encoderCombo.addItem("h.265")
        if check_encoder_support("av1"):
            self.encoderCombo.addItem("av1")

        self.framerateCombo = QComboBox()
        self.framerateCombo.setEditable(False)
        self.framerateCombo.addItems(["24", "30", "45", "60", "75", "90", "120", "144", "240"])

        self.bitrateCombo = QComboBox()
        self.bitrateCombo.setEditable(False)
        self.bitrateCombo.addItems(["250k", "500k", "1M", "2M", "4M", "8M", "16M", "20M", "32M"])

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(False)
        self.audioCombo.addItems(["enable", "disable", "loopback"])

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")

        self.displayCombo = QComboBox()
        self.displayCombo.setEditable(False)
        self.displayCombo.addItems([":0", ":1", ":2"])

        self.presetCombo = QComboBox()
        self.presetCombo.setEditable(False)
        self.presetCombo.addItems(["Default", "ultrafast", "superfast", "veryfast", "fast", "medium", "slow", "veryslow", "llhq"])

        self.gopCombo = QComboBox()
        self.gopCombo.setEditable(False)
        self.gopCombo.addItems(["5", "10", "15", "20", "30", "45", "60", "90", "120"])

        self.qpCombo = QComboBox()
        self.qpCombo.setEditable(False)
        self.qpCombo.addItems(["None", "10", "20", "30", "40", "50"])

        self.tuneCombo = QComboBox()
        self.tuneCombo.setEditable(False)
        self.tuneCombo.addItems(["None", "zerolatency", "film", "animation", "grain", "psnr", "ssim", "fastdecode"])

        self.pixFmtCombo = QComboBox()
        self.pixFmtCombo.setEditable(False)
        self.pixFmtCombo.addItems(["yuv420p", "yuv422p", "yuv444p", "nv12"])

        self.debugCheck = QCheckBox("Enable Debug")

        self.secKeyEdit = QLineEdit()
        self.secKeyEdit.setPlaceholderText("Enter base64-encoded 32-byte key")

        form_layout.addRow("Encoder:", self.encoderCombo)
        form_layout.addRow("Framerate:", self.framerateCombo)
        form_layout.addRow("Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)
        form_layout.addRow("X Display:", self.displayCombo)
        form_layout.addRow("Preset:", self.presetCombo)
        form_layout.addRow("GOP:", self.gopCombo)
        form_layout.addRow("QP:", self.qpCombo)
        form_layout.addRow("Tune:", self.tuneCombo)
        form_layout.addRow("Pixel Format:", self.pixFmtCombo)
        form_layout.addRow("Debug:", self.debugCheck)
        form_layout.addRow("Security Key:", self.secKeyEdit)
        form_group.setLayout(form_layout)

        button_layout = QHBoxLayout()
        self.startButton = QPushButton("Start Host")
        self.stopButton = QPushButton("Stop Host")
        button_layout.addWidget(self.startButton)
        button_layout.addWidget(self.stopButton)

        main_layout.addWidget(form_group)
        main_layout.addLayout(button_layout)
        main_layout.addStretch()
        self.setLayout(main_layout)

        self.startButton.clicked.connect(self.start_host)
        self.stopButton.clicked.connect(self.stop_host)
        self.host_process = None

    def profileChanged(self, index):
        profile = self.profileCombo.currentText()
        if profile == "Lowest Latency":
            self.encoderCombo.setCurrentText("h.264")
            self.framerateCombo.setCurrentText("60")
            self.bitrateCombo.setCurrentText("500k")
            self.audioCombo.setCurrentText("disable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("ultrafast")
            self.gopCombo.setCurrentText("10")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("zerolatency")
            self.pixFmtCombo.setCurrentText("yuv420p")
        elif profile == "Balanced":
            self.encoderCombo.setCurrentText("h.264")
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
        else:
            self.encoderCombo.setCurrentText("none")
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

    def start_host(self):
        encoder = self.encoderCombo.currentText()
        framerate = self.framerateCombo.currentText()
        bitrate = self.bitrateCombo.currentText()
        audio = self.audioCombo.currentText()
        adaptive = self.adaptiveCheck.isChecked()
        display = self.displayCombo.currentText()
        preset = "" if self.presetCombo.currentText() == "Default" else self.presetCombo.currentText()
        gop = self.gopCombo.currentText()
        qp = "" if self.qpCombo.currentText() == "None" else self.qpCombo.currentText()
        tune = "" if self.tuneCombo.currentText() == "None" else self.tuneCombo.currentText()
        pix_fmt = self.pixFmtCombo.currentText()
        debug = self.debugCheck.isChecked()
        sec_key = self.secKeyEdit.text().strip()
        if not sec_key:
            self.secKeyEdit.setText("Please enter a valid security key")
            return
        cmd = [
            sys.executable, "host.py",
            "--encoder", encoder,
            "--framerate", framerate,
            "--bitrate", bitrate,
            "--audio", audio,
            "--display", display,
            "--gop", gop,
            "--pix_fmt", pix_fmt,
            "--security-key", sec_key
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
        self.stop_host()
        self.host_process = subprocess.Popen(cmd, preexec_fn=os.setsid)

    def stop_host(self):
        if self.host_process:
            try:
                os.killpg(self.host_process.pid, signal.SIGTERM)
                self.host_process.wait(3)
            except Exception:
                if self.host_process:
                    self.host_process.kill()
            self.host_process = None

class ClientTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        main_layout = QVBoxLayout()
        form_group = QGroupBox("Client Configuration")
        form_layout = QFormLayout()
        self.decoderCombo = QComboBox()
        self.decoderCombo.setEditable(False)
        self.decoderCombo.addItem("none")
        if check_decoder_support("h.264"):
            self.decoderCombo.addItem("h.264")
        if check_decoder_support("h265"):
            self.decoderCombo.addItem("h.265")
        if check_decoder_support("av1"):
            self.decoderCombo.addItem("av1")
        self.hostIPEdit = QComboBox()
        self.hostIPEdit.setEditable(True)
        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(False)
        self.audioCombo.addItems(["enable", "disable"])
        self.monitorField = QLineEdit()
        self.monitorField.setText("0")
        self.debugCheck = QCheckBox("Enable Debug")
        self.secKeyEdit = QLineEdit()
        self.secKeyEdit.setPlaceholderText("Enter base64-encoded 32-byte key")
        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Monitor (index or 'all'):", self.monitorField)
        form_layout.addRow("Debug:", self.debugCheck)
        form_layout.addRow("Security Key:", self.secKeyEdit)
        form_group.setLayout(form_layout)
        button_layout = QHBoxLayout()
        self.startButton = QPushButton("Start Client")
        button_layout.addWidget(self.startButton)
        main_layout.addWidget(form_group)
        main_layout.addLayout(button_layout)
        main_layout.addStretch()
        self.setLayout(main_layout)
        self.startButton.clicked.connect(self.start_client)

    def start_client(self):
        decoder = self.decoderCombo.currentText()
        host_ip = self.hostIPEdit.currentText()
        audio = self.audioCombo.currentText()
        monitor = self.monitorField.text()
        debug = self.debugCheck.isChecked()
        sec_key = self.secKeyEdit.text().strip()
        if not sec_key:
            self.secKeyEdit.setText("Please enter a valid security key")
            return
        cmd = [
            sys.executable, "client.py",
            "--decoder", decoder,
            "--host_ip", host_ip,
            "--audio", audio,
            "--monitor", monitor,
            "--security-key", sec_key
        ]
        if debug:
            cmd.append("--debug")
        subprocess.Popen(cmd)

class HelpTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout()
        help_text = """
        <h1>Remote Desktop Viewer Help</h1>
        <p>This application streams your desktop from a host to a client.</p>
        <h2>Host Configuration</h2>
        <ul>
            <li><b>Profile:</b> 
              <ul>
                <li><b>Default:</b> Base defaults.</li>
                <li><b>Lowest Latency:</b> Minimal delay settings.</li>
                <li><b>Balanced:</b> A compromise between latency and quality.</li>
                <li><b>High Quality:</b> Optimized for quality (with higher latency).</li>
              </ul>
            </li>
            <li><b>Encoder:</b> Options: none, h.264, h.265, av1.</li>
            <li><b>Framerate:</b> Frame rate to capture.</li>
            <li><b>Bitrate:</b> Target bitrate.</li>
            <li><b>Audio:</b> Enable/disable or loopback.</li>
            <li><b>Adaptive:</b> Adaptive bitrate.</li>
            <li><b>X Display:</b> X session (usually :0).</li>
            <li><b>Preset:</b> Encoder preset.</li>
            <li><b>GOP:</b> Keyframe interval.</li>
            <li><b>QP:</b> Quantization Parameter.</li>
            <li><b>Tune:</b> Tuning option.</li>
            <li><b>Pixel Format:</b> e.g. yuv420p.</li>
            <li><b>Security Key:</b> A shared base64‑encoded 32‑byte key for encryption. Generate one with Fernet.</li>
            <li><b>Debug:</b> Extra logging.</li>
        </ul>
        <h2>Client Configuration</h2>
        <ul>
            <li><b>Decoder:</b> Must match host encoder.</li>
            <li><b>Host IP:</b> The host’s IP address.</li>
            <li><b>Audio:</b> Enable or disable audio.</li>
            <li><b>Monitor:</b> Monitor index (or "all" for multiple windows).</li>
            <li><b>Security Key:</b> Must match the host’s security key.</li>
            <li><b>Debug:</b> Enable extra logging.</li>
        </ul>
        <h2>Usage</h2>
        <p>Start the Host with your chosen settings and shared security key, then launch the Client to view the stream. The TCP handshake and file transfers are secured via TLS, and control/clipboard UDP messages are encrypted.</p>
        """
        from PyQt5.QtWidgets import QTextEdit
        help_view = QTextEdit()
        help_view.setReadOnly(True)
        help_view.setHtml(help_text)
        layout.addWidget(help_view)
        self.setLayout(layout)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay)")
        self.tabs = QTabWidget()
        self.hostTab = HostTab()
        self.clientTab = ClientTab()
        self.helpTab = HelpTab()
        self.tabs.addTab(self.hostTab, "Host")
        self.tabs.addTab(self.clientTab, "Client")
        self.tabs.addTab(self.helpTab, "Help")
        main_layout = QVBoxLayout()
        main_layout.addWidget(self.tabs)
        self.setLayout(main_layout)

    def closeEvent(self, event):
        if self.hostTab.host_process:
            self.hostTab.stop_host()
        event.accept()

def main():
    application = QApplication(sys.argv)
    application.setStyle("Fusion")
    from PyQt5.QtGui import QPalette, QColor
    palette = application.palette()
    palette.setColor(QPalette.Window, QColor(53, 53, 53))
    palette.setColor(QPalette.WindowText, Qt.white)
    palette.setColor(QPalette.Base, QColor(35, 35, 35))
    palette.setColor(QPalette.AlternateBase, QColor(53, 53, 53))
    palette.setColor(QPalette.ToolTipBase, Qt.white)
    palette.setColor(QPalette.ToolTipText, Qt.white)
    palette.setColor(QPalette.Text, Qt.white)
    palette.setColor(QPalette.Button, QColor(53, 53, 53))
    palette.setColor(QPalette.ButtonText, Qt.white)
    palette.setColor(QPalette.BrightText, Qt.red)
    palette.setColor(QPalette.Link, QColor(42, 130, 218))
    palette.setColor(QPalette.Highlight, QColor(42, 130, 218))
    palette.setColor(QPalette.HighlightedText, Qt.black)
    application.setPalette(palette)
    window = StartWindow()
    window.resize(600, 500)
    window.show()
    sys.exit(application.exec_())

if __name__ == "__main__":
    main()

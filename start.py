#!/usr/bin/env python3
import sys
import subprocess
import os
import signal
from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout,
    QFormLayout, QComboBox, QCheckBox, QPushButton, QGroupBox, QLineEdit, QTextEdit
)
from PyQt5.QtGui import QPalette, QColor
from PyQt5.QtCore import Qt
from shutil import which

def has_nvidia():
    return which("nvidia-smi") is not None

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
        return has_nvidia() or has_vaapi()
    return False

def check_decoder_support(codec):
    try:
        output = subprocess.check_output(["ffmpeg", "-decoders"], stderr=subprocess.DEVNULL).decode()
    except Exception:
        return False
    if codec == "h265":
        return any(x in output for x in ["hevc_nvdec", "hevc_vaapi", "hevc_qsv", "hevc_cuvid"])
    elif codec == "av1":
        return "av1" in output
    elif codec == "h.264":
        return has_nvidia() or has_vaapi()
    return False

class HostTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)

        main_layout = QVBoxLayout()

        self.profileCombo = QComboBox()
        self.profileCombo.setEditable(False)
        self.profileCombo.addItems(["Default", "Lowest Latency"])
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
        self.framerateCombo.addItems(["30", "45", "60", "75", "90", "120"])

        self.bitrateCombo = QComboBox()
        self.bitrateCombo.setEditable(False)
        self.bitrateCombo.addItems(["1M", "2M", "4M", "8M", "16M", "20M"])

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(False)
        self.audioCombo.addItems(["enable", "disable"])

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")

        self.passwordField = QLineEdit()
        self.passwordField.setEchoMode(QLineEdit.Password)

        self.displayCombo = QComboBox()
        self.displayCombo.setEditable(False)
        self.displayCombo.addItems([":0", ":1"])

        self.presetCombo = QComboBox()
        self.presetCombo.setEditable(False)
        self.presetCombo.addItems(["Default", "ultrafast", "veryfast", "fast", "medium", "slow", "veryslow", "llhq"])

        self.gopCombo = QComboBox()
        self.gopCombo.setEditable(False)
        self.gopCombo.addItems(["10", "15", "20", "30", "45", "60", "90", "120"])

        self.qpCombo = QComboBox()
        self.qpCombo.setEditable(False)
        self.qpCombo.addItems(["None", "10", "20", "30", "40", "50"])

        self.tuneCombo = QComboBox()
        self.tuneCombo.setEditable(False)
        self.tuneCombo.addItems(["None", "zerolatency", "film", "animation", "grain", "psnr", "ssim"])

        self.pixFmtCombo = QComboBox()
        self.pixFmtCombo.setEditable(False)
        self.pixFmtCombo.addItems(["yuv420p", "yuv422p", "yuv444p"])

        self.debugCheck = QCheckBox("Enable Debug")

        form_layout.addRow("Encoder:", self.encoderCombo)
        form_layout.addRow("Framerate:", self.framerateCombo)
        form_layout.addRow("Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)
        form_layout.addRow("Password:", self.passwordField)
        form_layout.addRow("X Display:", self.displayCombo)
        form_layout.addRow("Preset:", self.presetCombo)
        form_layout.addRow("GOP:", self.gopCombo)
        form_layout.addRow("QP:", self.qpCombo)
        form_layout.addRow("Tune:", self.tuneCombo)
        form_layout.addRow("Pixel Format:", self.pixFmtCombo)
        form_layout.addRow("Debug:", self.debugCheck)
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
        if self.profileCombo.currentText() == "Lowest Latency":
            self.encoderCombo.setCurrentText("h.264")
            self.framerateCombo.setCurrentText("60")
            self.bitrateCombo.setCurrentText("1M")
            self.audioCombo.setCurrentText("disable")
            self.adaptiveCheck.setChecked(False)
            self.displayCombo.setCurrentText(":0")
            self.presetCombo.setCurrentText("ultrafast")
            self.gopCombo.setCurrentText("10")
            self.qpCombo.setCurrentText("None")
            self.tuneCombo.setCurrentText("zerolatency")
            self.pixFmtCombo.setCurrentText("yuv420p")

    def start_host(self):
        encoder = self.encoderCombo.currentText()
        framerate = self.framerateCombo.currentText()
        bitrate = self.bitrateCombo.currentText()
        audio = self.audioCombo.currentText()
        adaptive = self.adaptiveCheck.isChecked()
        password = self.passwordField.text()
        display = self.displayCombo.currentText()
        preset = "" if self.presetCombo.currentText() == "Default" else self.presetCombo.currentText()
        gop = self.gopCombo.currentText()
        qp = "" if self.qpCombo.currentText() == "None" else self.qpCombo.currentText()
        tune = "" if self.tuneCombo.currentText() == "None" else self.tuneCombo.currentText()
        pix_fmt = self.pixFmtCombo.currentText()
        debug = self.debugCheck.isChecked()

        cmd = [
            sys.executable, "host.py",
            "--encoder", encoder,
            "--framerate", framerate,
            "--bitrate", bitrate,
            "--audio", audio,
            "--display", display,
            "--gop", gop,
            "--pix_fmt", pix_fmt
        ]
        if adaptive:
            cmd.append("--adaptive")
        if password:
            cmd.extend(["--password", password])
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
                pgid = os.getpgid(self.host_process.pid)
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

        self.passwordField = QLineEdit()
        self.passwordField.setEchoMode(QLineEdit.Password)

        self.monitorField = QLineEdit()
        self.monitorField.setText("0")

        self.debugCheck = QCheckBox("Enable Debug")

        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Password:", self.passwordField)
        form_layout.addRow("Monitor (index or 'all'):", self.monitorField)
        form_layout.addRow("Debug:", self.debugCheck)
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
        password = self.passwordField.text()
        monitor = self.monitorField.text()
        debug = self.debugCheck.isChecked()

        cmd = [
            sys.executable, "client.py",
            "--decoder", decoder,
            "--host_ip", host_ip,
            "--audio", audio,
            "--monitor", monitor
        ]
        if password:
            cmd.extend(["--password", password])
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
                <li><b>Default:</b> Use the values you manually select in each field.</li>
                <li><b>Lowest Latency:</b> Automatically fills in values for minimal latency.</li>
              </ul>
            </li>
            <li><b>Encoder:</b> The video encoder to use. Options: none, h.264, h.265, av1. (Under the Lowest Latency profile, h.264 is selected.)</li>
            <li><b>Framerate:</b> The number of frames per second captured. Options range from 30 to 120 fps.</li>
            <li><b>Bitrate:</b> The target video bitrate. Lower bitrates (e.g. 1M) help reduce delay.</li>
            <li><b>Audio:</b> Enable or disable audio streaming.</li>
            <li><b>Adaptive:</b> Whether the bitrate automatically adjusts based on network conditions.</li>
            <li><b>Password:</b> An optional password for secure connections.</li>
            <li><b>X Display:</b> The X session to capture (typically :0 for the primary session).</li>
            <li><b>Preset:</b> The encoder preset affecting speed and quality. “ultrafast” minimizes encoding delay.</li>
            <li><b>GOP:</b> Group Of Pictures size (keyframe interval). A lower value (e.g. 10) increases keyframe frequency.</li>
            <li><b>QP:</b> The Quantization Parameter. “None” means no fixed value is set.</li>
            <li><b>Tune:</b> A tuning option (for example, zerolatency bypasses buffering for real-time encoding).</li>
            <li><b>Pixel Format:</b> The output pixel format (e.g. yuv420p).</li>
            <li><b>Debug:</b> Enables detailed logging for troubleshooting.</li>
        </ul>
        <h2>Client Configuration</h2>
        <ul>
            <li><b>Decoder:</b> The video decoder to use. Must match the host's encoder.</li>
            <li><b>Host IP:</b> The IP address of the host machine.</li>
            <li><b>Audio:</b> Enable or disable audio playback.</li>
            <li><b>Password:</b> Enter the password if required by the host.</li>
            <li><b>Monitor:</b> Specify the monitor index (e.g. 0, 1) or "all" to open multiple windows.</li>
            <li><b>Debug:</b> Enables extra logging for troubleshooting.</li>
        </ul>
        <h2>Usage</h2>
        <p>Start the Host using the Host tab with your chosen settings. Then, launch the Client from the Client tab to view the stream. This Help tab describes the purpose of each configuration option.</p>
        """
        help_view = QTextEdit()
        help_view.setReadOnly(True)
        help_view.setHtml(help_text)
        layout.addWidget(help_view)
        self.setLayout(layout)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")
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
    window.resize(600, 400)
    window.show()
    sys.exit(application.exec_())

if __name__ == "__main__":
    main()

#!/usr/bin/env python3
import sys
import subprocess
import os
import signal
from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout,
    QFormLayout, QComboBox, QCheckBox, QPushButton, QGroupBox, QLineEdit
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
        form_group = QGroupBox("Host Configuration")
        form_layout = QFormLayout()

        self.encoderCombo = QComboBox()
        self.encoderCombo.setEditable(True)
        self.encoderCombo.addItem("none")
        if check_encoder_support("h.264"):
            self.encoderCombo.addItem("h.264")
        if check_encoder_support("h265"):
            self.encoderCombo.addItem("h.265")
        if check_encoder_support("av1"):
            self.encoderCombo.addItem("av1")

        self.resolutionCombo = QComboBox()
        self.resolutionCombo.setEditable(True)
        self.resolutionCombo.addItem("1920x1080")
        self.resolutionCombo.addItem("1600x900")
        self.resolutionCombo.addItem("1280x720")

        self.framerateCombo = QComboBox()
        self.framerateCombo.setEditable(True)
        self.framerateCombo.addItem("30")
        self.framerateCombo.addItem("60")

        self.bitrateCombo = QComboBox()
        self.bitrateCombo.setEditable(True)
        self.bitrateCombo.addItem("8M")
        self.bitrateCombo.addItem("5M")
        self.bitrateCombo.addItem("2M")

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(True)
        self.audioCombo.addItem("enable")
        self.audioCombo.addItem("disable")

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")

        self.passwordField = QLineEdit()
        self.passwordField.setEchoMode(QLineEdit.Password)

        self.displayCombo = QComboBox()
        self.displayCombo.setEditable(True)
        self.displayCombo.addItem(":0")
        self.displayCombo.addItem(":1")

        self.debugCheck = QCheckBox("Enable Debug")

        form_layout.addRow("Encoder:", self.encoderCombo)
        form_layout.addRow("Resolution:", self.resolutionCombo)
        form_layout.addRow("Framerate:", self.framerateCombo)
        form_layout.addRow("Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)
        form_layout.addRow("Password:", self.passwordField)
        form_layout.addRow("X Display:", self.displayCombo)
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

    def start_host(self):
        encoder = self.encoderCombo.currentText()
        resolution = self.resolutionCombo.currentText()
        framerate = self.framerateCombo.currentText()
        bitrate = self.bitrateCombo.currentText()
        audio = self.audioCombo.currentText()
        adaptive = self.adaptiveCheck.isChecked()
        password = self.passwordField.text()
        display = self.displayCombo.currentText()
        debug = self.debugCheck.isChecked()

        cmd = [
            sys.executable, "host.py",
            "--encoder", encoder,
            "--resolution", resolution,
            "--framerate", framerate,
            "--bitrate", bitrate,
            "--audio", audio,
            "--display", display
        ]
        if adaptive:
            cmd.append("--adaptive")
        if password:
            cmd.extend(["--password", password])
        if debug:
            cmd.append("--debug")

        self.stop_host()
        self.host_process = subprocess.Popen(cmd, preexec_fn=os.setsid)

    def stop_host(self):
        if self.host_process:
            try:
                pgid = os.getpgid(self.host_process.pid)
                os.killpg(pgid, signal.SIGTERM)
                self.host_process.wait(3)
            except:
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
        self.decoderCombo.setEditable(True)
        self.decoderCombo.addItem("none")
        if check_decoder_support("h.264"):
            self.decoderCombo.addItem("h.264")
        if check_decoder_support("h265"):
            self.decoderCombo.addItem("h.265")
        if check_decoder_support("av1"):
            self.decoderCombo.addItem("av1")

        self.hostIPEdit = QComboBox()
        self.hostIPEdit.setEditable(True)

        self.resolutionCombo = QComboBox()
        self.resolutionCombo.setEditable(True)
        self.resolutionCombo.addItem("1920x1080")
        self.resolutionCombo.addItem("1600x900")
        self.resolutionCombo.addItem("1280x720")

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(True)
        self.audioCombo.addItem("enable")
        self.audioCombo.addItem("disable")

        self.passwordField = QLineEdit()
        self.passwordField.setEchoMode(QLineEdit.Password)

        self.debugCheck = QCheckBox("Enable Debug")

        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Remote Resolution:", self.resolutionCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Password:", self.passwordField)
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
        resolution = self.resolutionCombo.currentText()
        audio = self.audioCombo.currentText()
        password = self.passwordField.text()
        debug = self.debugCheck.isChecked()

        cmd = [
            sys.executable, "client.py",
            "--decoder", decoder,
            "--host_ip", host_ip,
            "--remote_resolution", resolution,
            "--audio", audio
        ]
        if password:
            cmd.extend(["--password", password])
        if debug:
            cmd.append("--debug")

        subprocess.Popen(cmd)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")

        self.tabs = QTabWidget()
        self.hostTab = HostTab()
        self.clientTab = ClientTab()
        self.tabs.addTab(self.hostTab, "Host")
        self.tabs.addTab(self.clientTab, "Client")

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

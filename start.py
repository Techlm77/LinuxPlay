#!/usr/bin/env python3
import sys
import subprocess
import os
import json
import signal
from PyQt5.QtWidgets import (
    QApplication, QWidget, QTabWidget, QVBoxLayout, QHBoxLayout,
    QFormLayout, QComboBox, QCheckBox, QPushButton, QGroupBox, QLineEdit
)
from PyQt5.QtGui import QIcon, QPixmap, QPalette, QColor
from PyQt5.QtCore import Qt
import subprocess
from shutil import which

def load_history():
    try:
        with open("history.json", "r") as f:
            return json.load(f)
    except:
        return {}

def save_history(data):
    with open("history.json", "w") as f:
        json.dump(data, f, indent=2)

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
    def __init__(self, history, save_callback, parent=None):
        super().__init__(parent)
        self.history = history
        self.save_callback = save_callback
        self.host_process = None

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
        if "encoder" in self.history and self.history["encoder"]:
            self.encoderCombo.setCurrentText(self.history["encoder"][0])

        self.resolutionCombo = QComboBox()
        self.resolutionCombo.setEditable(True)
        if "resolution" in self.history:
            for item in self.history["resolution"]:
                self.resolutionCombo.addItem(item)
        self.resolutionCombo.addItem("1920x1080")
        self.resolutionCombo.addItem("1600x900")
        self.resolutionCombo.addItem("1280x720")

        self.framerateCombo = QComboBox()
        self.framerateCombo.setEditable(True)
        if "framerate" in self.history:
            for item in self.history["framerate"]:
                self.framerateCombo.addItem(item)
        self.framerateCombo.addItem("30")
        self.framerateCombo.addItem("60")

        self.bitrateCombo = QComboBox()
        self.bitrateCombo.setEditable(True)
        if "bitrate" in self.history:
            for item in self.history["bitrate"]:
                self.bitrateCombo.addItem(item)
        self.bitrateCombo.addItem("8M")
        self.bitrateCombo.addItem("5M")
        self.bitrateCombo.addItem("2M")

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(True)
        if "audio" in self.history:
            for item in self.history["audio"]:
                self.audioCombo.addItem(item)
        self.audioCombo.addItem("enable")
        self.audioCombo.addItem("disable")

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")

        self.passwordCombo = QComboBox()
        self.passwordCombo.setEditable(True)
        if "host_password" in self.history:
            for item in self.history["host_password"]:
                self.passwordCombo.addItem(item)
        self.passwordCombo.lineEdit().setEchoMode(QLineEdit.Password)

        self.displayCombo = QComboBox()
        self.displayCombo.setEditable(True)
        if "display" in self.history:
            for item in self.history["display"]:
                self.displayCombo.addItem(item)
        self.displayCombo.addItem(":0")
        self.displayCombo.addItem(":1")

        form_layout.addRow("Encoder:", self.encoderCombo)
        form_layout.addRow("Resolution:", self.resolutionCombo)
        form_layout.addRow("Framerate:", self.framerateCombo)
        form_layout.addRow("Bitrate:", self.bitrateCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Adaptive:", self.adaptiveCheck)
        form_layout.addRow("Password:", self.passwordCombo)
        form_layout.addRow("X Display:", self.displayCombo)
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

    def start_host(self):
        encoder = self.encoderCombo.currentText()
        resolution = self.resolutionCombo.currentText()
        framerate = self.framerateCombo.currentText()
        bitrate = self.bitrateCombo.currentText()
        audio = self.audioCombo.currentText()
        adaptive = self.adaptiveCheck.isChecked()
        password = self.passwordCombo.currentText()
        display = self.displayCombo.currentText()

        self.update_history("encoder", encoder)
        self.update_history("resolution", resolution)
        self.update_history("framerate", framerate)
        self.update_history("bitrate", bitrate)
        self.update_history("audio", audio)
        self.update_history("host_password", password)
        self.update_history("display", display)
        self.save_callback()

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

    def update_history(self, key, value):
        if key not in self.history:
            self.history[key] = []
        if value and value not in self.history[key]:
            self.history[key].insert(0, value)
            if len(self.history[key]) > 10:
                self.history[key].pop()

class ClientTab(QWidget):
    def __init__(self, history, save_callback, parent=None):
        super().__init__(parent)
        self.history = history
        self.save_callback = save_callback

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
        if "decoder" in self.history and self.history["decoder"]:
            self.decoderCombo.setCurrentText(self.history["decoder"][0])

        self.hostIPEdit = QComboBox()
        self.hostIPEdit.setEditable(True)
        if "host_ip" in self.history:
            for item in self.history["host_ip"]:
                self.hostIPEdit.addItem(item)

        self.resolutionCombo = QComboBox()
        self.resolutionCombo.setEditable(True)
        if "remote_resolution" in self.history:
            for item in self.history["remote_resolution"]:
                self.resolutionCombo.addItem(item)
        self.resolutionCombo.addItem("1920x1080")
        self.resolutionCombo.addItem("1600x900")
        self.resolutionCombo.addItem("1280x720")

        self.audioCombo = QComboBox()
        self.audioCombo.setEditable(True)
        if "client_audio" in self.history:
            for item in self.history["client_audio"]:
                self.audioCombo.addItem(item)
        self.audioCombo.addItem("enable")
        self.audioCombo.addItem("disable")

        self.passwordCombo = QComboBox()
        self.passwordCombo.setEditable(True)
        if "client_password" in self.history:
            for item in self.history["client_password"]:
                self.passwordCombo.addItem(item)
        self.passwordCombo.lineEdit().setEchoMode(QLineEdit.Password)

        form_layout.addRow("Decoder:", self.decoderCombo)
        form_layout.addRow("Host IP:", self.hostIPEdit)
        form_layout.addRow("Remote Resolution:", self.resolutionCombo)
        form_layout.addRow("Audio:", self.audioCombo)
        form_layout.addRow("Password:", self.passwordCombo)
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
        password = self.passwordCombo.currentText()

        self.update_history("decoder", decoder)
        self.update_history("host_ip", host_ip)
        self.update_history("remote_resolution", resolution)
        self.update_history("client_audio", audio)
        self.update_history("client_password", password)
        self.save_callback()

        cmd = [
            sys.executable, "client.py",
            "--decoder", decoder,
            "--host_ip", host_ip,
            "--remote_resolution", resolution,
            "--audio", audio
        ]
        if password:
            cmd.extend(["--password", password])

        subprocess.Popen(cmd)

    def update_history(self, key, value):
        if key not in self.history:
            self.history[key] = []
        if value and value not in self.history[key]:
            self.history[key].insert(0, value)
            if len(self.history[key]) > 10:
                self.history[key].pop()

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")

        self.history_data = load_history()

        self.tabs = QTabWidget()
        self.hostTab = HostTab(self.history_data, self.save_history)
        self.clientTab = ClientTab(self.history_data, self.save_history)
        self.tabs.addTab(self.hostTab, "Host")
        self.tabs.addTab(self.clientTab, "Client")

        main_layout = QVBoxLayout()
        main_layout.addWidget(self.tabs)
        self.setLayout(main_layout)

    def closeEvent(self, event):
        if self.hostTab.host_process:
            self.hostTab.stop_host()
        event.accept()

    def save_history(self):
        save_history(self.history_data)

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

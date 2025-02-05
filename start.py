#!/usr/bin/env python3
import sys
import subprocess
import socket
import threading
import argparse
import os
from PyQt5.QtWidgets import (QApplication, QWidget, QTabWidget, QVBoxLayout, QFormLayout, 
                             QLineEdit, QComboBox, QCheckBox, QPushButton, QSizePolicy)
from PyQt5.QtGui import QIcon, QPixmap
from PyQt5.QtCore import Qt

class HostTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QFormLayout()
        self.encoderCombo = QComboBox()
        self.encoderCombo.addItems(["nvenc", "vaapi", "none"])
        layout.addRow("Encoder:", self.encoderCombo)

        self.resolutionEdit = QLineEdit("1920x1080")
        layout.addRow("Resolution:", self.resolutionEdit)

        self.framerateEdit = QLineEdit("30")
        layout.addRow("Framerate:", self.framerateEdit)

        self.bitrateEdit = QLineEdit("8M")
        layout.addRow("Bitrate:", self.bitrateEdit)

        self.audioCombo = QComboBox()
        self.audioCombo.addItems(["enable", "disable"])
        layout.addRow("Audio:", self.audioCombo)

        self.adaptiveCheck = QCheckBox("Enable Adaptive Bitrate")
        layout.addRow("Adaptive:", self.adaptiveCheck)

        self.passwordEdit = QLineEdit()
        self.passwordEdit.setEchoMode(QLineEdit.Password)
        layout.addRow("Password:", self.passwordEdit)

        self.startButton = QPushButton("Start Host")
        layout.addRow(self.startButton)

        self.setLayout(layout)

class ClientTab(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QFormLayout()
        self.decoderCombo = QComboBox()
        self.decoderCombo.addItems(["nvdec", "vaapi", "none"])
        layout.addRow("Decoder:", self.decoderCombo)

        self.hostIPEdit = QLineEdit()
        layout.addRow("Host IP:", self.hostIPEdit)

        self.resolutionEdit = QLineEdit("1920x1080")
        layout.addRow("Remote Resolution:", self.resolutionEdit)

        self.audioCombo = QComboBox()
        self.audioCombo.addItems(["enable", "disable"])
        layout.addRow("Audio:", self.audioCombo)

        self.passwordEdit = QLineEdit()
        self.passwordEdit.setEchoMode(QLineEdit.Password)
        layout.addRow("Password:", self.passwordEdit)

        self.startButton = QPushButton("Start Client")
        layout.addRow(self.startButton)

        self.setLayout(layout)

class StartWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")

        self.tabs = QTabWidget()
        self.hostTab = HostTab()
        self.clientTab = ClientTab()
        self.tabs.addTab(self.hostTab, "Host")
        self.tabs.addTab(self.clientTab, "Client")

        layout = QVBoxLayout()
        layout.addWidget(self.tabs)
        self.setLayout(layout)

        self.hostTab.startButton.clicked.connect(self.start_host)
        self.clientTab.startButton.clicked.connect(self.start_client)

        try:
            import urllib.request
            req = urllib.request.Request("https://techlm77.co.uk/png/logo.png", headers={"User-Agent": "Mozilla/5.0"})
            data = urllib.request.urlopen(req).read()
            pixmap = QPixmap()
            pixmap.loadFromData(data)
            self.setWindowIcon(QIcon(pixmap))
        except Exception as e:
            print(f"Failed to load window icon: {e}")

    def start_host(self):
        encoder = self.hostTab.encoderCombo.currentText()
        resolution = self.hostTab.resolutionEdit.text()
        framerate = self.hostTab.framerateEdit.text()
        bitrate = self.hostTab.bitrateEdit.text()
        audio = self.hostTab.audioCombo.currentText()
        adaptive = self.hostTab.adaptiveCheck.isChecked()
        password = self.hostTab.passwordEdit.text()

        cmd = ["python3", "host.py",
               "--encoder", encoder,
               "--resolution", resolution,
               "--framerate", framerate,
               "--bitrate", bitrate,
               "--audio", audio]
        if adaptive:
            cmd.append("--adaptive")
        if password:
            cmd.extend(["--password", password])
        print("Starting host with command:", " ".join(cmd))
        subprocess.Popen(cmd)

    def start_client(self):
        decoder = self.clientTab.decoderCombo.currentText()
        host_ip = self.clientTab.hostIPEdit.text()
        resolution = self.clientTab.resolutionEdit.text()
        audio = self.clientTab.audioCombo.currentText()
        password = self.clientTab.passwordEdit.text()

        cmd = ["python3", "client.py",
               "--decoder", decoder,
               "--host_ip", host_ip,
               "--remote_resolution", resolution,
               "--audio", audio]
        if password:
            cmd.extend(["--password", password])
        print("Starting client with command:", " ".join(cmd))
        subprocess.Popen(cmd)

if __name__ == "__main__":
    app = QApplication(sys.argv)
    window = StartWindow()
    window.resize(400, 300)
    window.show()
    sys.exit(app.exec_())

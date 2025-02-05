#!/usr/bin/env python3
import sys
import av
import argparse
import logging
import subprocess
import socket
import time
import threading
import urllib.request
from PyQt5.QtWidgets import QApplication, QLabel, QMainWindow, QSizePolicy
from PyQt5.QtGui import QImage, QPixmap, QIcon
from PyQt5.QtCore import QThread, pyqtSignal, Qt
import atexit

DEFAULT_UDP_PORT   = 5000
DEFAULT_RESOLUTION = "1920x1080"
MULTICAST_IP       = "239.0.0.1"
CONTROL_PORT       = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
MOUSE_MOVE_THROTTLE = 0.02

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S"
)

def tcp_handshake_client(host_ip, password):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.connect((host_ip, TCP_HANDSHAKE_PORT))
    except Exception as e:
        logging.error(f"TCP handshake connection failed: {e}")
        return False
    handshake_msg = f"PASSWORD:{password}" if password else "PASSWORD:"
    sock.sendall(handshake_msg.encode("utf-8"))
    resp = sock.recv(1024).decode("utf-8", errors="replace").strip()
    sock.close()
    if resp == "OK":
        logging.info("TCP handshake successful.")
        return True
    else:
        logging.error("TCP handshake failed: Incorrect password.")
        return False

class DecoderThread(QThread):
    frame_ready = pyqtSignal(QImage)
    def __init__(self, input_url, decoder_opts, parent=None):
        super().__init__(parent)
        self.input_url = input_url
        self.decoder_opts = decoder_opts
        self._running = True
    def run(self):
        try:
            container = av.open(self.input_url, options=self.decoder_opts)
            video_stream = container.streams.video[0]
        except Exception as e:
            logging.error(f"Error opening video stream: {e}")
            return
        while self._running:
            try:
                for frame in container.decode(video=0):
                    img = frame.to_image()
                    data = img.tobytes("raw", "RGB")
                    qimg = QImage(data, img.width, img.height, QImage.Format_RGB888)
                    self.frame_ready.emit(qimg)
                    if not self._running:
                        break
            except Exception as e:
                logging.error(f"Decoding error: {e}")
                self.msleep(10)
    def stop(self):
        self._running = False

class VideoWidget(QLabel):
    def __init__(self, control_callback, rwidth, rheight, parent=None):
        super().__init__(parent)
        self.setScaledContents(True)
        self.setMouseTracking(True)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Expanding)
        self.setContentsMargins(0, 0, 0, 0)
        self.control_callback = control_callback
        self.remote_width  = rwidth
        self.remote_height = rheight
        self.last_mouse_move = 0
        self.clipboard = QApplication.clipboard()
        self.clipboard.dataChanged.connect(self.on_clipboard_change)
        self.last_clipboard = self.clipboard.text()
        self.ignore_clipboard = False

    def on_clipboard_change(self):
        new_text = self.clipboard.text()
        if not self.ignore_clipboard and new_text and new_text != self.last_clipboard:
            self.last_clipboard = new_text
            msg = f"CLIPBOARD_UPDATE CLIENT {new_text}"
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
            sock.sendto(msg.encode("utf-8"), (MULTICAST_IP, UDP_CLIPBOARD_PORT))
            logging.info("Client clipboard updated and broadcast.")

    def mouseMoveEvent(self, e):
        now = time.time()
        if now - self.last_mouse_move < MOUSE_MOVE_THROTTLE:
            return
        self.last_mouse_move = now
        if self.width() and self.height():
            rx = int(e.x() / self.width() * self.remote_width)
            ry = int(e.y() / self.height() * self.remote_height)
            self.control_callback(f"MOUSE_MOVE {rx} {ry}")
        e.accept()

    def mousePressEvent(self, e):
        button_map = {Qt.LeftButton:"1", Qt.MiddleButton:"2", Qt.RightButton:"3"}
        b = button_map.get(e.button(), "")
        if b and self.width() and self.height():
            rx = int(e.x() / self.width() * self.remote_width)
            ry = int(e.y() / self.height() * self.remote_height)
            self.control_callback(f"MOUSE_PRESS {b} {rx} {ry}")
        e.accept()

    def mouseReleaseEvent(self, e):
        button_map = {Qt.LeftButton:"1", Qt.MiddleButton:"2", Qt.RightButton:"3"}
        b = button_map.get(e.button(), "")
        if b:
            self.control_callback(f"MOUSE_RELEASE {b}")
        e.accept()

    def wheelEvent(self, e):
        delta = e.angleDelta()
        if delta.y() != 0:
            b = "4" if delta.y() > 0 else "5"
            self.control_callback(f"MOUSE_SCROLL {b}")
            e.accept()
        elif delta.x() != 0:
            b = "6" if delta.x() < 0 else "7"
            self.control_callback(f"MOUSE_SCROLL {b}")
            e.accept()

    def keyPressEvent(self, e):
        key_name = self._get_key_name(e)
        if key_name:
            self.control_callback(f"KEY_PRESS {key_name}")
        e.accept()

    def keyReleaseEvent(self, e):
        key_name = self._get_key_name(e)
        if key_name:
            self.control_callback(f"KEY_RELEASE {key_name}")
        e.accept()

    def _get_key_name(self, event):
        from PyQt5.QtCore import Qt
        key = event.key()
        text = event.text()
        key_map = {
            Qt.Key_Escape: "Escape",
            Qt.Key_Tab: "Tab",
            Qt.Key_Backtab: "Tab",
            Qt.Key_Backspace: "BackSpace",
            Qt.Key_Return: "Return",
            Qt.Key_Enter: "Return",
            Qt.Key_Insert: "Insert",
            Qt.Key_Delete: "Delete",
            Qt.Key_Pause: "Pause",
            Qt.Key_Print: "Print",
            Qt.Key_SysReq: "Sys_Req",
            Qt.Key_Clear: "Clear",
            Qt.Key_Home: "Home",
            Qt.Key_End: "End",
            Qt.Key_Left: "Left",
            Qt.Key_Up: "Up",
            Qt.Key_Right: "Right",
            Qt.Key_Down: "Down",
            Qt.Key_PageUp: "Page_Up",
            Qt.Key_PageDown: "Page_Down",
            Qt.Key_Shift: "Shift_L",
            Qt.Key_Control: "Control_L",
            Qt.Key_Meta: "Super_L",
            Qt.Key_Alt: "Alt_L",
            Qt.Key_AltGr: "Alt_R",
            Qt.Key_CapsLock: "Caps_Lock",
            Qt.Key_NumLock: "Num_Lock",
            Qt.Key_ScrollLock: "Scroll_Lock",
            Qt.Key_F1: "F1",
            Qt.Key_F2: "F2",
            Qt.Key_F3: "F3",
            Qt.Key_F4: "F4",
            Qt.Key_F5: "F5",
            Qt.Key_F6: "F6",
            Qt.Key_F7: "F7",
            Qt.Key_F8: "F8",
            Qt.Key_F9: "F9",
            Qt.Key_F10: "F10",
            Qt.Key_F11: "F11",
            Qt.Key_F12: "F12",
            Qt.Key_Space: "space",
            Qt.Key_QuoteLeft: "grave",
            Qt.Key_Minus: "minus",
            Qt.Key_Equal: "equal",
            Qt.Key_BracketLeft: "bracketleft",
            Qt.Key_BracketRight: "bracketright",
            Qt.Key_Backslash: "backslash",
            Qt.Key_Semicolon: "semicolon",
            Qt.Key_Apostrophe: "apostrophe",
            Qt.Key_Comma: "comma",
            Qt.Key_Period: "period",
            Qt.Key_Slash: "slash",
            Qt.Key_Exclam: "exclam",
            Qt.Key_QuoteDbl: "quotedbl",
            Qt.Key_NumberSign: "numbersign",
            Qt.Key_Dollar: "dollar",
            Qt.Key_Percent: "percent",
            Qt.Key_Ampersand: "ampersand",
            Qt.Key_Asterisk: "asterisk",
            Qt.Key_ParenLeft: "parenleft",
            Qt.Key_ParenRight: "parenright",
            Qt.Key_Underscore: "underscore",
            Qt.Key_Plus: "plus",
            Qt.Key_BraceLeft: "braceleft",
            Qt.Key_BraceRight: "braceright",
            Qt.Key_Bar: "bar",
            Qt.Key_Colon: "colon",
            Qt.Key_Less: "less",
            Qt.Key_Greater: "greater",
            Qt.Key_Question: "question",
        }
        if key in key_map:
            return key_map[key]
        if (Qt.Key_A <= key <= Qt.Key_Z) or (Qt.Key_0 <= key <= Qt.Key_9):
            return chr(key).lower()
        if text:
            if text == "£":
                return "sterling"
            if text == "¬":
                return "notsign"
            return text
        return None

class MainWindow(QMainWindow):
    def __init__(self, decoder_opts, rwidth, rheight, host_ip, password, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")
        self.remote_width  = rwidth
        self.remote_height = rheight
        self.control_addr  = (host_ip, CONTROL_PORT)
        self.control_sock  = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.control_sock.setblocking(False)
        self.password = password
        self.video_widget = VideoWidget(self.send_control, rwidth, rheight)
        self.setCentralWidget(self.video_widget)
        self.video_widget.setFocus()
        mcast_url = f"udp://@{MULTICAST_IP}:{DEFAULT_UDP_PORT}?fifo_size=5000000&overrun_nonfatal=1"
        self.decoder_thread = DecoderThread(mcast_url, decoder_opts)
        self.decoder_thread.frame_ready.connect(self.update_image, Qt.DirectConnection)
        self.decoder_thread.start()

    def update_image(self, qimg):
        self.video_widget.setPixmap(QPixmap.fromImage(qimg))

    def send_control(self, msg):
        if self.password:
            msg = f"PASSWORD:{self.password}:" + msg
        try:
            self.control_sock.sendto(msg.encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.error(f"Error sending control message: {e}")

def clipboard_listener_client():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("", UDP_CLIPBOARD_PORT))
    while True:
        try:
            data, addr = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="replace")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "HOST":
                new_content = tokens[2]
                clipboard = QApplication.clipboard()
                clipboard.blockSignals(True)
                if new_content != clipboard.text():
                    clipboard.setText(new_content)
                    logging.info("Client clipboard updated from host.")
                clipboard.blockSignals(False)
        except Exception as e:
            logging.error(f"Client clipboard listener error: {e}")

def control_listener_client():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("", CONTROL_PORT))
    logging.info(f"Client control listener active on UDP port {CONTROL_PORT}")
    while True:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="replace").strip()
            if msg:
                logging.info(f"Client received control message: {msg}")
        except Exception as e:
            logging.error(f"Client control listener error: {e}")

def cleanup():
    try:
        pass
    except Exception:
        pass

atexit.register(cleanup)

def main():
    parser = argparse.ArgumentParser(description="Remote Desktop Client (Production Ready)")
    parser.add_argument("--decoder", choices=["nvdec", "vaapi", "none"], default="none",
                        help="Choose hardware decoder: nvdec, vaapi, or none.")
    parser.add_argument("--host_ip", required=True,
                        help="Host IP for control events and TCP handshake.")
    parser.add_argument("--remote_resolution", default=DEFAULT_RESOLUTION,
                        help="Remote screen resolution, e.g., 1920x1080.")
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable",
                        help="Enable or disable audio playback.")
    parser.add_argument("--password", default="",
                        help="Optional password for control events and TCP handshake.")
    args = parser.parse_args()

    try:
        w, h = map(int, args.remote_resolution.lower().split("x"))
    except Exception:
        logging.error("Invalid remote_resolution format. Use e.g. 1600x900.")
        sys.exit(1)

    decoder_opts = {}
    if args.decoder == "nvdec":
        decoder_opts["hwaccel"] = "nvdec"
    elif args.decoder == "vaapi":
        decoder_opts["hwaccel"] = "vaapi"
        decoder_opts["vaapi_device"] = "/dev/dri/renderD128"

    if not tcp_handshake_client(args.host_ip, args.password):
        sys.exit("TCP handshake failed. Exiting.")

    app = QApplication(sys.argv)

    clipboard_thread = threading.Thread(target=clipboard_listener_client, daemon=True)
    clipboard_thread.start()

    control_thread = threading.Thread(target=control_listener_client, daemon=True)
    control_thread.start()

    audio_proc = None
    if args.audio == "enable":
        audio_cmd = [
            "ffplay",
            "-hide_banner",
            "-loglevel", "error",
            "-fflags", "nobuffer",
            "-flags", "low_delay",
            "-autoexit",
            "-nodisp",
            f"udp://@{MULTICAST_IP}:6001?fifo_size=5000000&overrun_nonfatal=1"
        ]
        logging.info("Starting audio playback...")
        audio_proc = subprocess.Popen(audio_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    try:
        req = urllib.request.Request("https://techlm77.co.uk/png/logo.png", headers={"User-Agent": "Mozilla/5.0"})
        data = urllib.request.urlopen(req).read()
        pixmap = QPixmap()
        pixmap.loadFromData(data)
        app.setWindowIcon(QIcon(pixmap))
    except Exception as e:
        logging.error(f"Failed to load window icon: {e}")

    window = MainWindow(decoder_opts, w, h, args.host_ip, args.password)
    window.show()
    ret = app.exec_()

    if audio_proc:
        audio_proc.terminate()
    sys.exit(ret)

if __name__ == "__main__":
    main()

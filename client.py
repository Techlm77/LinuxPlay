#!/usr/bin/env python3
import sys
import av
import argparse
import logging
import subprocess
import socket
import time
import threading
import os
from PyQt5.QtWidgets import QApplication, QLabel, QMainWindow, QSizePolicy, QMessageBox
from PyQt5.QtGui import QImage, QPixmap
from PyQt5.QtCore import QThread, pyqtSignal, Qt
import atexit
from shutil import which

DEFAULT_UDP_PORT = 5000
DEFAULT_RESOLUTION = "1920x1080"
MULTICAST_IP = "239.0.0.1"
CONTROL_PORT = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
FILE_UPLOAD_PORT = 7003
MOUSE_MOVE_THROTTLE = 0.02

def has_nvidia():
    return which("nvidia-smi") is not None

def has_vaapi():
    return os.path.exists("/dev/dri/renderD128")

def tcp_handshake_client(host_ip, password):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        logging.info("Attempting TCP handshake with %s:%s", host_ip, TCP_HANDSHAKE_PORT)
        sock.connect((host_ip, TCP_HANDSHAKE_PORT))
    except Exception as e:
        logging.error("TCP handshake connection failed: %s", e)
        sock.close()
        return (False, None)

    handshake_msg = f"PASSWORD:{password}" if password else "PASSWORD:"
    sock.sendall(handshake_msg.encode("utf-8"))

    try:
        resp = sock.recv(1024).decode("utf-8", errors="replace").strip()
    except Exception as e:
        logging.error("Failed to receive handshake response: %s", e)
        sock.close()
        return (False, None)

    sock.close()

    if resp.startswith("OK:"):
        logging.info("TCP handshake successful.")
        parts = resp.split(":", 2)
        if len(parts) >= 3:
            host_encoder = parts[1].strip()
            monitor_info = parts[2].strip()
        else:
            host_encoder = parts[1].strip()
            monitor_info = DEFAULT_RESOLUTION
        return (True, (host_encoder, monitor_info))
    else:
        logging.error("TCP handshake failed: Incorrect password or unknown error.")
        return (False, None)

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
        except Exception as e:
            logging.error("Error opening video stream: %s", e)
            return

        while self._running:
            try:
                for frame in container.decode(video=0):
                    if not self._running:
                        break
                    img = frame.to_image()
                    data = img.tobytes("raw", "RGB")
                    qimg = QImage(data, img.width, img.height, QImage.Format_RGB888)
                    self.frame_ready.emit(qimg)
            except Exception as e:
                logging.error("Decoding error: %s", e)
                QThread.msleep(10)
                continue

    def stop(self):
        self._running = False

class VideoWidget(QLabel):
    def __init__(self, control_callback, rwidth, rheight, offset_x, offset_y, parent=None):
        super().__init__(parent)
        self.setScaledContents(True)
        self.setMouseTracking(True)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Expanding)
        self.setContentsMargins(0, 0, 0, 0)

        self.control_callback = control_callback
        self.remote_width = rwidth
        self.remote_height = rheight
        self.offset_x = offset_x
        self.offset_y = offset_y
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
            rx = self.offset_x + int(e.x() / self.width() * self.remote_width)
            ry = self.offset_y + int(e.y() / self.height() * self.remote_height)
            self.control_callback(f"MOUSE_MOVE {rx} {ry}")
        e.accept()

    def mousePressEvent(self, e):
        button_map = {Qt.LeftButton: "1", Qt.MiddleButton: "2", Qt.RightButton: "3"}
        b = button_map.get(e.button(), "")
        if b and self.width() and self.height():
            rx = self.offset_x + int(e.x() / self.width() * self.remote_width)
            ry = self.offset_y + int(e.y() / self.height() * self.remote_height)
            self.control_callback(f"MOUSE_PRESS {b} {rx} {ry}")
        e.accept()

    def mouseReleaseEvent(self, e):
        button_map = {Qt.LeftButton: "1", Qt.MiddleButton: "2", Qt.RightButton: "3"}
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
            Qt.Key_Escape: "Escape", Qt.Key_Tab: "Tab", Qt.Key_Backtab: "Tab", Qt.Key_Backspace: "BackSpace",
            Qt.Key_Return: "Return", Qt.Key_Enter: "Return", Qt.Key_Insert: "Insert", Qt.Key_Delete: "Delete",
            Qt.Key_Pause: "Pause", Qt.Key_Print: "Print", Qt.Key_SysReq: "Sys_Req", Qt.Key_Clear: "Clear",
            Qt.Key_Home: "Home", Qt.Key_End: "End", Qt.Key_Left: "Left", Qt.Key_Up: "Up", Qt.Key_Right: "Right",
            Qt.Key_Down: "Down", Qt.Key_PageUp: "Page_Up", Qt.Key_PageDown: "Page_Down", Qt.Key_Shift: "Shift_L",
            Qt.Key_Control: "Control_L", Qt.Key_Meta: "Super_L", Qt.Key_Alt: "Alt_L", Qt.Key_AltGr: "Alt_R",
            Qt.Key_CapsLock: "Caps_Lock", Qt.Key_NumLock: "Num_Lock", Qt.Key_ScrollLock: "Scroll_Lock",
            Qt.Key_F1: "F1", Qt.Key_F2: "F2", Qt.Key_F3: "F3", Qt.Key_F4: "F4", Qt.Key_F5: "F5", Qt.Key_F6: "F6",
            Qt.Key_F7: "F7", Qt.Key_F8: "F8", Qt.Key_F9: "F9", Qt.Key_F10: "F10", Qt.Key_F11: "F11", Qt.Key_F12: "F12",
            Qt.Key_Space: "space", Qt.Key_QuoteLeft: "grave", Qt.Key_Minus: "minus", Qt.Key_Equal: "equal",
            Qt.Key_BracketLeft: "bracketleft", Qt.Key_BracketRight: "bracketright", Qt.Key_Backslash: "backslash",
            Qt.Key_Semicolon: "semicolon", Qt.Key_Apostrophe: "apostrophe", Qt.Key_Comma: "comma",
            Qt.Key_Period: "period", Qt.Key_Slash: "slash", Qt.Key_Exclam: "exclam", Qt.Key_QuoteDbl: "quotedbl",
            Qt.Key_NumberSign: "numbersign", Qt.Key_Dollar: "dollar", Qt.Key_Percent: "percent",
            Qt.Key_Ampersand: "ampersand", Qt.Key_Asterisk: "asterisk", Qt.Key_ParenLeft: "parenleft",
            Qt.Key_ParenRight: "parenright", Qt.Key_Underscore: "underscore", Qt.Key_Plus: "plus",
            Qt.Key_BraceLeft: "braceleft", Qt.Key_BraceRight: "braceright", Qt.Key_Bar: "bar",
            Qt.Key_Colon: "colon", Qt.Key_Less: "less", Qt.Key_Greater: "greater", Qt.Key_Question: "question"
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
    def __init__(self, decoder_opts, rwidth, rheight, host_ip, password, udp_port, offset_x, offset_y, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay by Techlm77)")
        self.remote_width = rwidth
        self.remote_height = rheight
        self.offset_x = offset_x
        self.offset_y = offset_y
        self.control_addr = (host_ip, CONTROL_PORT)
        self.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.control_sock.setblocking(False)
        self.password = password

        self.video_widget = VideoWidget(self.send_control, rwidth, rheight, offset_x, offset_y)
        self.setCentralWidget(self.video_widget)
        self.video_widget.setFocus()
        
        self.setAcceptDrops(True)

        logging.debug("Client selected decoder options: %s", decoder_opts)
        logging.debug("Client connecting to host at %s, resolution %sx%s", host_ip, rwidth, rheight)

        video_url = f"udp://0.0.0.0:{udp_port}?fifo_size=5000000&overrun_nonfatal=1"
        self.decoder_thread = DecoderThread(video_url, decoder_opts)
        self.decoder_thread.frame_ready.connect(self.update_image)
        self.decoder_thread.start()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event):
        urls = event.mimeData().urls()
        if urls:
            file_path = urls[0].toLocalFile()
            if os.path.isfile(file_path):
                threading.Thread(target=self.upload_file, args=(file_path,), daemon=True).start()
                event.acceptProposedAction()
            else:
                event.ignore()

    def upload_file(self, file_path):
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect((self.control_addr[0], FILE_UPLOAD_PORT))
            filename = os.path.basename(file_path)
            filename_bytes = filename.encode('utf-8')
            filename_length = len(filename_bytes)
            header = filename_length.to_bytes(4, byteorder='big') + filename_bytes
            file_size = os.path.getsize(file_path)
            header += file_size.to_bytes(8, byteorder='big')
            sock.sendall(header)
            with open(file_path, 'rb') as f:
                while True:
                    data = f.read(4096)
                    if not data:
                        break
                    sock.sendall(data)
            sock.close()
            logging.info("File %s uploaded successfully.", filename)
        except Exception as e:
            logging.error("Error uploading file: %s", e)

    def update_image(self, qimg):
        self.video_widget.setPixmap(QPixmap.fromImage(qimg))

    def send_control(self, msg):
        if self.password:
            msg = f"PASSWORD:{self.password}:{msg}"
        try:
            self.control_sock.sendto(msg.encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.error("Error sending control message: %s", e)

def clipboard_listener_client():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.bind(("", UDP_CLIPBOARD_PORT))
    except Exception as e:
        logging.error("Clipboard listener bind failed: %s", e)
        return
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
            logging.error("Client clipboard listener error: %s", e)

def control_listener_client():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.bind(("", CONTROL_PORT))
    except Exception as e:
        logging.error("Client control listener bind failed: %s", e)
        return
    logging.info("Client control listener active on UDP port %s", CONTROL_PORT)
    while True:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="replace").strip()
            if msg:
                logging.info("Client received control message: %s", msg)
        except Exception as e:
            logging.error("Client control listener error: %s", e)

def cleanup():
    pass

atexit.register(cleanup)

def main():
    parser = argparse.ArgumentParser(description="Remote Desktop Client (Production Ready)")
    parser.add_argument("--decoder", choices=["none", "h.264", "h.265", "av1"], default="none")
    parser.add_argument("--host_ip", required=True)
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable")
    parser.add_argument("--password", default="")
    parser.add_argument("--monitor", default="0", help="Monitor index to view or 'all' for all monitors")
    parser.add_argument("--debug", action="store_true", help="Enable debug mode with more logging.")
    args = parser.parse_args()

    if args.debug:
        logging.basicConfig(
            level=logging.DEBUG,
            format="%(asctime)s [%(levelname)s] %(message)s",
            datefmt="%H:%M:%S"
        )
    else:
        logging.basicConfig(
            level=logging.INFO,
            format="%(asctime)s [%(levelname)s] %(message)s",
            datefmt="%H:%M:%S"
        )

    handshake_ok, host_info = tcp_handshake_client(args.host_ip, args.password)
    if not handshake_ok:
        sys.exit("TCP handshake failed. Exiting.")
    host_encoder, monitor_info_str = host_info
    try:
        monitors = []
        if ";" in monitor_info_str:
            parts = monitor_info_str.split(";")
            for part in parts:
                try:
                    if '+' in part:
                        res_part, ox, oy = part.split('+')
                        w_str, h_str = res_part.split('x')
                        w = int(w_str)
                        h = int(h_str)
                        ox = int(ox)
                        oy = int(oy)
                    else:
                        w, h = map(int, part.split('x'))
                        ox = 0
                        oy = 0
                    monitors.append((w, h, ox, oy))
                except Exception:
                    pass
        else:
            if '+' in monitor_info_str:
                res_part, ox, oy = monitor_info_str.split('+')
                w_str, h_str = res_part.split('x')
                w = int(w_str)
                h = int(h_str)
                ox = int(ox)
                oy = int(oy)
                monitors.append((w, h, ox, oy))
            else:
                w, h = map(int, monitor_info_str.lower().split("x"))
                monitors.append((w, h, 0, 0))
    except Exception:
        logging.error("Error parsing monitor info from host; using default.")
        w, h = map(int, DEFAULT_RESOLUTION.lower().split("x"))
        monitors = [(w, h, 0, 0)]

    if args.decoder == "none":
        logging.warning("You selected 'none' decoder, but host is using '%s'. Attempting raw decode fallback...", host_encoder)
    else:
        if args.decoder.replace(".", "") != host_encoder.replace(".", ""):
            logging.error("Encoder/decoder mismatch: Host uses '%s', client selected '%s'.", host_encoder, args.decoder)
            temp_app = QApplication.instance() or QApplication(sys.argv)
            QMessageBox.critical(None, "Decoder Mismatch",
                f"ERROR: The host is currently using '{host_encoder}' encoder, but your decoder is '{args.decoder}'.\n"
                f"Please switch to '{host_encoder}' instead.")
            sys.exit(1)

    decoder_opts = {}
    if args.decoder == "h.264":
        if has_nvidia():
            decoder_opts["hwaccel"] = "h264_nvdec"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "h264_vaapi"
    elif args.decoder == "h.265":
        if has_nvidia():
            decoder_opts["hwaccel"] = "hevc_nvdec"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "hevc_vaapi"
    elif args.decoder == "av1":
        if has_nvidia():
            decoder_opts["hwaccel"] = "av1_nvdec"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "av1_vaapi"

    app = QApplication(sys.argv)

    if args.monitor.lower() == "all":
        windows = []
        base_port = DEFAULT_UDP_PORT
        for i, mon in enumerate(monitors):
            w, h, ox, oy = mon
            window = MainWindow(decoder_opts, w, h, args.host_ip, args.password, base_port + i, ox, oy)
            window.setWindowTitle(f"Remote Desktop Viewer - Monitor {i}")
            window.show()
            windows.append(window)
        ret = app.exec_()
    else:
        try:
            mon_index = int(args.monitor)
        except Exception:
            mon_index = 0
        if mon_index < 0 or mon_index >= len(monitors):
            logging.error("Invalid monitor index %d, defaulting to 0", mon_index)
            mon_index = 0
        w, h, ox, oy = monitors[mon_index]
        window = MainWindow(decoder_opts, w, h, args.host_ip, args.password, DEFAULT_UDP_PORT + mon_index, ox, oy)
        window.setWindowTitle(f"Remote Desktop Viewer - Monitor {mon_index}")
        window.show()
        ret = app.exec_()

    sys.exit(ret)

if __name__ == "__main__":
    main()

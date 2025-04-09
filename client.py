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
import numpy as np
import ssl
from PyQt5.QtWidgets import QApplication, QMainWindow, QMessageBox, QOpenGLWidget
from PyQt5.QtCore import QThread, QTimer, pyqtSignal, Qt
from PyQt5.QtGui import QSurfaceFormat
import atexit
from shutil import which
from OpenGL.GL import *
from OpenGL.GLUT import *
from cryptography.fernet import Fernet

security_key = None
cipher = None
ssl_context = None

MOUSE_MOVE_THROTTLE = 0.005  
DEFAULT_UDP_PORT = 5000
DEFAULT_RESOLUTION = "1920x1080"
MULTICAST_IP = "239.0.0.1"
CONTROL_PORT = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
FILE_UPLOAD_PORT = 7003

audio_proc = None

def has_nvidia():
    return which("nvidia-smi") is not None

def has_vaapi():
    return os.path.exists("/dev/dri/renderD128")

def is_intel_cpu():
    try:
        with open("/proc/cpuinfo", "r") as f:
            return "GenuineIntel" in f.read()
    except Exception:
        return False

def secure_sendto(sock, message, addr):
    encrypted = cipher.encrypt(message.encode("utf-8"))
    sock.sendto(encrypted, addr)

def secure_recvfrom(sock, bufsize):
    data, addr = sock.recvfrom(bufsize)
    try:
        decrypted = cipher.decrypt(data).decode("utf-8")
    except Exception as e:
        decrypted = ""
    return decrypted, addr

def tcp_handshake_client(host_ip):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        logging.info("Connecting to host %s:%s for handshake", host_ip, TCP_HANDSHAKE_PORT)
        sock.connect((host_ip, TCP_HANDSHAKE_PORT))
        secure_sock = ssl_context.wrap_socket(sock, server_hostname=host_ip)
    except Exception as e:
        logging.error("Handshake connection failed: %s", e)
        sock.close()
        return (False, None)
    handshake_msg = "HELLO"
    secure_sock.sendall(handshake_msg.encode("utf-8"))
    try:
        resp = secure_sock.recv(1024).decode("utf-8", errors="replace").strip()
    except Exception as e:
        logging.error("Failed to receive handshake response: %s", e)
        secure_sock.close()
        return (False, None)
    secure_sock.close()
    if resp.startswith("OK:"):
        logging.info("Handshake successful.")
        parts = resp.split(":", 2)
        if len(parts) >= 3:
            host_encoder = parts[1].strip()
            monitor_info = parts[2].strip()
        else:
            host_encoder = parts[1].strip()
            monitor_info = DEFAULT_RESOLUTION
        return (True, (host_encoder, monitor_info))
    else:
        logging.error("Handshake failed. Response: %s", resp)
        return (False, None)

class DecoderThread(QThread):
    frame_ready = pyqtSignal(object)
    def __init__(self, input_url, decoder_opts, window, parent=None):
        super().__init__(parent)
        self.input_url = input_url
        self.decoder_opts = decoder_opts
        self.window = window
        self.decoder_opts.setdefault("probesize", "32")
        self.decoder_opts.setdefault("analyzeduration", "0")
        self._running = True

    def run(self):
        while self._running:
            container = None
            try:
                container = av.open(self.input_url, options=self.decoder_opts)
                for frame in container.decode(video=0):
                    if not self._running:
                        break
                    arr = frame.to_ndarray(format="rgb24")
                    with self.window.latest_lock:
                        self.window.latest_frame = (arr, frame.width, frame.height)
            except av.error.InvalidDataError as e:
                logging.error("InvalidDataError in decoding: %s", e)
            except Exception as e:
                logging.error("Decoding error: %s", e)
            finally:
                if container is not None:
                    try:
                        container.close()
                    except Exception as e:
                        logging.error("Error closing container: %s", e)
            time.sleep(0.005)

    def stop(self):
        self._running = False

class VideoWidgetGL(QOpenGLWidget):
    def __init__(self, control_callback, rwidth, rheight, offset_x, offset_y, parent=None):
        super().__init__(parent)
        self.setMouseTracking(True)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAutoFillBackground(False)
        self.setAttribute(Qt.WA_OpaquePaintEvent, True)
        self.setAttribute(Qt.WA_NoSystemBackground, True)
        
        self.control_callback = control_callback
        self.texture_width = rwidth
        self.texture_height = rheight
        self.offset_x = offset_x
        self.offset_y = offset_y
        self.last_mouse_move = 0
        self.frame_data = None
        
        self.clipboard = QApplication.clipboard()
        self.clipboard.dataChanged.connect(self.on_clipboard_change)
        self.last_clipboard = self.clipboard.text()
        self.ignore_clipboard = False
        
        self.texture_id = None
        self.pbo_ids = []
        self.current_pbo = 0

    def on_clipboard_change(self):
        new_text = self.clipboard.text()
        if not self.ignore_clipboard and new_text and new_text != self.last_clipboard:
            self.last_clipboard = new_text
            msg = f"CLIPBOARD_UPDATE CLIENT {new_text}"
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
            secure_sendto(sock, msg, (MULTICAST_IP, UDP_CLIPBOARD_PORT))
            logging.info("Client clipboard updated and broadcast.")

    def initializeGL(self):
        glClearColor(0.0, 0.0, 0.0, 1.0)
        self.texture_id = glGenTextures(1)
        self._initialize_texture(self.texture_width, self.texture_height)
    
    def _initialize_texture(self, width, height):
        glBindTexture(GL_TEXTURE_2D, self.texture_id)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR)
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, width, height, 0, GL_RGB, GL_UNSIGNED_BYTE, None)
        glBindTexture(GL_TEXTURE_2D, 0)
        if self.pbo_ids is not None and len(self.pbo_ids) > 0:
            glDeleteBuffers(len(self.pbo_ids), self.pbo_ids)
        self.pbo_ids = list(glGenBuffers(2))
        buffer_size = width * height * 3
        for pbo in self.pbo_ids:
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, pbo)
            glBufferData(GL_PIXEL_UNPACK_BUFFER, buffer_size, None, GL_STREAM_DRAW)
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)
        self.texture_width = width
        self.texture_height = height

    def resizeTexture(self, new_width, new_height):
        logging.info("Resizing texture from %dx%d to %dx%d", self.texture_width, self.texture_height, new_width, new_height)
        self._initialize_texture(new_width, new_height)

    def paintGL(self):
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT)
        if self.frame_data:
            arr, frame_width, frame_height = self.frame_data
            if frame_width != self.texture_width or frame_height != self.texture_height:
                self.resizeTexture(frame_width, frame_height)
            data = np.ascontiguousarray(arr, dtype=np.uint8)
            buffer_size = data.nbytes
            current_pbo = self.pbo_ids[self.current_pbo]
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, current_pbo)
            ptr = glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, buffer_size,
                                    GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT)
            if ptr:
                from ctypes import memmove, c_void_p
                memmove(c_void_p(ptr), data.ctypes.data, buffer_size)
                glUnmapBuffer(GL_PIXEL_UNPACK_BUFFER)
            glBindTexture(GL_TEXTURE_2D, self.texture_id)
            glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, frame_width, frame_height, GL_RGB, GL_UNSIGNED_BYTE, None)
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)
            glEnable(GL_TEXTURE_2D)
            glBegin(GL_QUADS)
            glTexCoord2f(0.0, 1.0); glVertex2f(-1.0, -1.0)
            glTexCoord2f(1.0, 1.0); glVertex2f(1.0, -1.0)
            glTexCoord2f(1.0, 0.0); glVertex2f(1.0, 1.0)
            glTexCoord2f(0.0, 0.0); glVertex2f(-1.0, 1.0)
            glEnd()
            glDisable(GL_TEXTURE_2D)
            glBindTexture(GL_TEXTURE_2D, 0)
            self.current_pbo = (self.current_pbo + 1) % 2

    def updateFrame(self, frame_tuple):
        self.frame_data = frame_tuple
        self.update()

    def mouseMoveEvent(self, e):
        now = time.time()
        if now - self.last_mouse_move < MOUSE_MOVE_THROTTLE:
            return
        self.last_mouse_move = now
        if self.width() and self.height():
            rx = self.offset_x + int(e.x() / self.width() * self.texture_width)
            ry = self.offset_y + int(e.y() / self.height() * self.texture_height)
            self.control_callback(f"MOUSE_MOVE {rx} {ry}")
        e.accept()

    def mousePressEvent(self, e):
        button_map = {Qt.LeftButton: "1", Qt.MiddleButton: "2", Qt.RightButton: "3"}
        b = button_map.get(e.button(), "")
        if b and self.width() and self.height():
            rx = self.offset_x + int(e.x() / self.width() * self.texture_width)
            ry = self.offset_y + int(e.y() / self.height() * self.texture_height)
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
        if e.isAutoRepeat():
            return
        key_name = self._get_key_name(e)
        if key_name:
            self.control_callback(f"KEY_PRESS {key_name}")
        e.accept()

    def keyReleaseEvent(self, e):
        if e.isAutoRepeat():
            return
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
    def __init__(self, decoder_opts, rwidth, rheight, host_ip, udp_port, offset_x, offset_y, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Remote Desktop Viewer (LinuxPlay)")
        self.texture_width = rwidth
        self.texture_height = rheight
        self.offset_x = offset_x
        self.offset_y = offset_y

        self.latest_frame = None
        self.latest_lock = threading.Lock()

        self.control_addr = (host_ip, CONTROL_PORT)
        self.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.control_sock.setblocking(False)
        self.video_widget = VideoWidgetGL(self.send_control, rwidth, rheight, offset_x, offset_y)
        self.setCentralWidget(self.video_widget)
        self.video_widget.setFocus()
        self.setAcceptDrops(True)
        logging.debug("Using decoder options: %s", decoder_opts)
        logging.debug("Connecting to host %s, resolution %sx%s", host_ip, rwidth, rheight)
        video_url = f"udp://0.0.0.0:{udp_port}?fifo_size=1024&max_delay=0&overrun_nonfatal=1"
        self.decoder_thread = DecoderThread(video_url, decoder_opts, self)
        self.decoder_thread.start()
        self.timer = QTimer(self)
        self.timer.timeout.connect(self.poll_frame)
        self.timer.start(16)

    def poll_frame(self):
        with self.latest_lock:
            frame = self.latest_frame
        if frame:
            self.video_widget.updateFrame(frame)

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event):
        urls = event.mimeData().urls()
        if urls:
            for url in urls:
                file_path = url.toLocalFile()
                if os.path.isdir(file_path):
                    for root, dirs, files in os.walk(file_path):
                        for f in files:
                            full_path = os.path.join(root, f)
                            threading.Thread(target=self.upload_file, args=(full_path,), daemon=True).start()
                elif os.path.isfile(file_path):
                    threading.Thread(target=self.upload_file, args=(file_path,), daemon=True).start()
            event.acceptProposedAction()
        else:
            event.ignore()

    def upload_file(self, file_path):
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect((self.control_addr[0], FILE_UPLOAD_PORT))
            secure_sock = ssl_context.wrap_socket(sock)
            filename = os.path.basename(file_path)
            filename_bytes = filename.encode('utf-8')
            filename_length = len(filename_bytes)
            header = filename_length.to_bytes(4, byteorder='big') + filename_bytes
            file_size = os.path.getsize(file_path)
            header += file_size.to_bytes(8, byteorder='big')
            secure_sock.sendall(header)
            with open(file_path, 'rb') as f:
                while True:
                    data = f.read(4096)
                    if not data:
                        break
                    secure_sock.sendall(data)
            secure_sock.close()
            logging.info("File %s uploaded successfully.", filename)
        except Exception as e:
            logging.error("Error uploading file: %s", e)

    def update_image(self, frame_tuple):
        self.video_widget.updateFrame(frame_tuple)

    def send_control(self, msg):
        try:
            secure_sendto(self.control_sock, msg, self.control_addr)
        except Exception as e:
            logging.error("Error sending control message: %s", e)

    def closeEvent(self, event):
        self.timer.stop()
        self.decoder_thread.stop()
        self.decoder_thread.wait(2000)
        global audio_proc
        if audio_proc is not None:
            try:
                audio_proc.terminate()
            except Exception as e:
                logging.error("Error terminating audio process: %s", e)
        event.accept()

def clipboard_listener_client():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.bind(("", UDP_CLIPBOARD_PORT))
        mreq = socket.inet_aton(MULTICAST_IP) + socket.inet_aton("0.0.0.0")
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
    except Exception as e:
        logging.error("Clipboard listener bind failed: %s", e)
        return
    while True:
        try:
            msg, addr = secure_recvfrom(sock, 65535)
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
            msg, addr = secure_recvfrom(sock, 2048)
            if msg:
                logging.info("Client received control message: %s", msg)
        except Exception as e:
            logging.error("Client control listener error: %s", e)

def cleanup():
    pass

atexit.register(cleanup)

def main():
    parser = argparse.ArgumentParser(description="Remote Desktop Client (Optimized for Ultra-Low Latency) with Security")
    parser.add_argument("--decoder", choices=["none", "h.264", "h.265", "av1"], default="none")
    parser.add_argument("--host_ip", required=True)
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable")
    parser.add_argument("--monitor", default="0", help="Monitor index to view or 'all' for all monitors")
    parser.add_argument("--debug", action="store_true", help="Enable debug mode with more logging.")
    parser.add_argument("--security-key", required=True, help="Base64-encoded 32-byte key for encryption (Fernet)")
    args = parser.parse_args()

    global security_key, cipher, ssl_context
    security_key = args.security_key.encode("utf-8")
    cipher = Fernet(security_key)
    ssl_context = ssl.create_default_context()
    ssl_context.check_hostname = False
    ssl_context.verify_mode = ssl.CERT_NONE

    fmt = QSurfaceFormat()
    fmt.setSwapInterval(0)
    QSurfaceFormat.setDefaultFormat(fmt)

    app = QApplication(sys.argv)

    if args.debug:
        logging.basicConfig(level=logging.DEBUG, format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%H:%M:%S")
    else:
        logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%H:%M:%S")

    handshake_ok, host_info = tcp_handshake_client(args.host_ip)
    if not handshake_ok:
        sys.exit("Handshake failed. Exiting.")
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
        logging.error("Error parsing monitor info; using default.")
        w, h = map(int, DEFAULT_RESOLUTION.lower().split("x"))
        monitors = [(w, h, 0, 0)]

    if args.decoder == "none":
        logging.warning("You selected 'none' decoder, but host is using '%s'. Attempting raw decode fallback...", host_encoder)
    else:
        if args.decoder.replace(".", "") != host_encoder.replace(".", ""):
            logging.error("Encoder/decoder mismatch: Host uses '%s', client selected '%s'.", host_encoder, args.decoder)
            QMessageBox.critical(None, "Decoder Mismatch",
                f"ERROR: The host is currently using '{host_encoder}' encoder, but your decoder is '{args.decoder}'.\n"
                f"Please switch to '{host_encoder}' instead.")
            sys.exit(1)

    decoder_opts = {}
    if args.decoder == "h.264":
        if has_nvidia():
            decoder_opts["hwaccel"] = "h264_nvdec"
        elif is_intel_cpu():
            decoder_opts["hwaccel"] = "h264_qsv"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "h264_vaapi"
    elif args.decoder == "h.265":
        if has_nvidia():
            decoder_opts["hwaccel"] = "hevc_nvdec"
        elif is_intel_cpu():
            decoder_opts["hwaccel"] = "hevc_qsv"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "hevc_vaapi"
    elif args.decoder == "av1":
        if has_nvidia():
            decoder_opts["hwaccel"] = "av1_nvdec"
        elif is_intel_cpu():
            decoder_opts["hwaccel"] = "av1_qsv"
        elif has_vaapi():
            decoder_opts["hwaccel"] = "av1_vaapi"

    threading.Thread(target=clipboard_listener_client, daemon=True).start()
    threading.Thread(target=control_listener_client, daemon=True).start()

    global audio_proc
    if args.audio == "enable":
        audio_cmd = [
            "ffplay",
            "-hide_banner",
            "-loglevel", "error",
            "-fflags", "nobuffer",
            "-flags", "low_delay",
            "-autoexit",
            "-nodisp",
            "-i", f"udp://@{MULTICAST_IP}:6001?fifo_size=512&max_delay=0&pkt_size=1316&overrun_nonfatal=1"
        ]
        logging.info("Starting audio playback with ffplay...")
        audio_proc = subprocess.Popen(audio_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    if args.monitor.lower() == "all":
        windows = []
        base_port = DEFAULT_UDP_PORT
        for i, mon in enumerate(monitors):
            w, h, ox, oy = mon
            window = MainWindow(decoder_opts, w, h, args.host_ip, base_port + i, ox, oy)
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
        window = MainWindow(decoder_opts, w, h, args.host_ip, DEFAULT_UDP_PORT + mon_index, ox, oy)
        window.setWindowTitle(f"Remote Desktop Viewer - Monitor {mon_index}")
        window.show()
        ret = app.exec_()

    if audio_proc:
        try:
            audio_proc.terminate()
        except Exception as e:
            logging.error("Error terminating audio process: %s", e)
    sys.exit(ret)

if __name__ == "__main__":
    main()

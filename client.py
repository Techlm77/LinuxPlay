#!/usr/bin/env python3
import sys
import os
import argparse
import logging
import socket
import time
import threading
import subprocess
import platform as py_platform
from queue import Queue

try:
    HERE = os.path.dirname(os.path.abspath(__file__))
    ffbin = os.path.join(HERE, "ffmpeg", "bin")
    if os.name == "nt" and os.path.exists(os.path.join(ffbin, "ffmpeg.exe")):
        os.environ["PATH"] = ffbin + os.pathsep + os.environ.get("PATH", "")
except Exception:
    pass

import av
import numpy as np

from PyQt5.QtWidgets import QApplication, QMainWindow, QMessageBox, QOpenGLWidget
from PyQt5.QtCore import QThread, pyqtSignal, Qt, QTimer
from PyQt5.QtGui import QSurfaceFormat

from OpenGL.GL import *

MOUSE_MOVE_THROTTLE = 0.003
DEFAULT_UDP_PORT = 5000
DEFAULT_RESOLUTION = "1920x1080"
CONTROL_PORT = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
UDP_AUDIO_PORT = 6001
UDP_HEARTBEAT_PORT = 7004

IS_WINDOWS = py_platform.system() == "Windows"
IS_LINUX   = py_platform.system() == "Linux"

CLIPBOARD_INBOX = Queue()

audio_proc = None

def ffmpeg_hwaccels():
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-hwaccels"],
            stderr=subprocess.STDOUT, universal_newlines=True
        )
        accels = set()
        for line in out.splitlines():
            name = line.strip()
            if name and not name.lower().startswith("hardware acceleration methods"):
                accels.add(name)
        return accels
    except Exception:
        return set()

def choose_auto_hwaccel():
    accels = ffmpeg_hwaccels()
    if IS_WINDOWS:
        for cand in ("d3d11va", "cuda", "dxva2", "qsv"):
            if cand in accels:
                return cand
        return "cpu"
    else:
        for cand in ("vaapi", "qsv", "cuda"):
            if cand in accels:
                return cand
        return "cpu"


def detect_network_mode(host_ip: str) -> str:
    try:
        if IS_LINUX:
            import subprocess, re, os
            out = subprocess.check_output(["ip", "route", "get", host_ip], universal_newlines=True, stderr=subprocess.STDOUT)
            m = re.search(r"\bdev\s+(\S+)", out)
            iface = m.group(1) if m else ""
            if iface and os.path.exists(f"/sys/class/net/{iface}/wireless"):
                return "wifi"
            if iface.startswith("wl"):
                return "wifi"
            return "lan"
        elif IS_WINDOWS:
            import subprocess
            ps = ["powershell", "-NoProfile", "-Command",
                  "(Get-NetRoute -DestinationPrefix %s/32 | Sort-Object -Property RouteMetric | Select-Object -First 1).InterfaceAlias" % host_ip]
            try:
                alias = subprocess.check_output(ps, universal_newlines=True, stderr=subprocess.DEVNULL).strip()
            except Exception:
                alias = ""
            if alias:
                ps2 = ["powershell", "-NoProfile", "-Command",
                       "($a = Get-NetAdapter -Name '%s' -ErrorAction SilentlyContinue) | Select-Object -Expand NdisPhysicalMedium" % alias.replace("'", "''")]
                try:
                    medium = subprocess.check_output(ps2, universal_newlines=True, stderr=subprocess.DEVNULL).strip().lower()
                    if "wireless" in medium or "802.11" in medium:
                        return "wifi"
                except Exception:
                    pass
            ps3 = ["powershell", "-NoProfile", "-Command", "(Get-NetAdapter -Physical | ? {$_.Status -eq 'Up'} | ? {$_.NdisPhysicalMedium -like '*Wireless*'}).Name | Select-Object -First 1"]
            try:
                any_wifi = subprocess.check_output(ps3, universal_newlines=True, stderr=subprocess.DEVNULL).strip()
                if any_wifi: return "wifi"
            except Exception:
                pass
            return "lan"
    except Exception:
        return "lan"


def tcp_handshake_client(host_ip):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        logging.info("Handshake to %s:%s", host_ip, TCP_HANDSHAKE_PORT)
        sock.connect((host_ip, TCP_HANDSHAKE_PORT))
        sock.sendall(b"HELLO")
        resp = sock.recv(1024).decode("utf-8", errors="replace").strip()
        sock.close()
    except Exception as e:
        logging.error("Handshake failed: %s", e)
        return (False, None)
    if resp.startswith("OK:"):
        parts = resp.split(":", 2)
        host_encoder = parts[1].strip()
        monitor_info = parts[2].strip() if len(parts) > 2 else DEFAULT_RESOLUTION
        return (True, (host_encoder, monitor_info))
    logging.error("Handshake response invalid: %s", resp)
    return (False, None)

class DecoderThread(QThread):
    frame_ready = pyqtSignal(object)

    def __init__(self, input_url, decoder_opts):
        super().__init__()
        self.input_url = input_url
        self.decoder_opts = dict(decoder_opts) if decoder_opts else {}
        self.decoder_opts.setdefault("probesize", "32")
        self.decoder_opts.setdefault("analyzeduration", "0")
        self.decoder_opts.setdefault("fflags", "nobuffer")
        self.decoder_opts.setdefault("flags", "low_delay")
        self.decoder_opts.setdefault("reorder_queue_size", "0")
        self.decoder_opts.setdefault("rtbufsize", "256k")
        self._running = True
        self._sw_fallback_done = False

    def _open_container(self):
        logging.debug("Opening stream with opts: %s", self.decoder_opts)
        return av.open(self.input_url, options=self.decoder_opts)

    def run(self):
        while self._running:
            container = None
            try:
                container = self._open_container()

                try:
                    vstream = next(s for s in container.streams if s.type == "video")
                    cc = vstream.codec_context
                    cc.thread_count = 1
                except Exception:
                    pass

                for frame in container.decode(video=0):
                    if not self._running:
                        break
                    arr = frame.to_ndarray(format="rgb24")
                    self.frame_ready.emit((arr, frame.width, frame.height))
            except Exception as e:
                logging.error("Decoding error: %s", e)
                if not self._sw_fallback_done and "hwaccel" in self.decoder_opts:
                    logging.warning("Falling back to software decoding…")
                    self.decoder_opts.pop("hwaccel", None)
                    self.decoder_opts.pop("hwaccel_device", None)
                    self._sw_fallback_done = True
                time.sleep(0.03)
            finally:
                if container:
                    try: container.close()
                    except Exception: pass
            time.sleep(0.002)

    def stop(self):
        self._running = False

class VideoWidgetGL(QOpenGLWidget):
    def __init__(self, control_callback, rwidth, rheight, offset_x, offset_y, host_ip, parent=None):
        super().__init__(parent)
        self.setMouseTracking(True)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAutoFillBackground(False)
        self.setAttribute(Qt.WA_OpaquePaintEvent, True)
        self.setAttribute(Qt.WA_NoSystemBackground, True)

        self.host_ip = host_ip
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
            msg = f"CLIPBOARD_UPDATE CLIENT {new_text}".encode("utf-8")
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            try:
                sock.sendto(msg, (self.host_ip, UDP_CLIPBOARD_PORT))
            finally:
                sock.close()

    def initializeGL(self):
        glDisable(GL_DEPTH_TEST)
        glDisable(GL_DITHER)
        glClearColor(0.0, 0.0, 0.0, 1.0)
        self.texture_id = glGenTextures(1)
        self._initialize_texture(self.texture_width, self.texture_height)

    def _initialize_texture(self, w, h):
        glBindTexture(GL_TEXTURE_2D, self.texture_id)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR)
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1)
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, w, h, 0, GL_RGB, GL_UNSIGNED_BYTE, None)
        glBindTexture(GL_TEXTURE_2D, 0)

        if self.pbo_ids:
            glDeleteBuffers(len(self.pbo_ids), self.pbo_ids)
        self.pbo_ids = list(glGenBuffers(2))
        buf_size = w * h * 3
        for pbo in self.pbo_ids:
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, pbo)
            glBufferData(GL_PIXEL_UNPACK_BUFFER, buf_size, None, GL_STREAM_DRAW)
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)

        self.texture_width, self.texture_height = w, h

    def resizeTexture(self, w, h):
        logging.info("Resize texture %dx%d -> %dx%d", self.texture_width, self.texture_height, w, h)
        self._initialize_texture(w, h)

    def paintGL(self):
        glClear(GL_COLOR_BUFFER_BIT)
        if self.frame_data:
            arr, fw, fh = self.frame_data
            if (fw != self.texture_width) or (fh != self.texture_height):
                self.resizeTexture(fw, fh)
            data = np.ascontiguousarray(arr, dtype=np.uint8)
            size = data.nbytes
            current_pbo = self.pbo_ids[self.current_pbo]
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, current_pbo)
            ptr = glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, size, GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT)
            if ptr:
                from ctypes import memmove, c_void_p
                memmove(c_void_p(ptr), data.ctypes.data, size)
                glUnmapBuffer(GL_PIXEL_UNPACK_BUFFER)
            glBindTexture(GL_TEXTURE_2D, self.texture_id)
            glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, fw, fh, GL_RGB, GL_UNSIGNED_BYTE, None)
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)

            glEnable(GL_TEXTURE_2D)
            glBegin(GL_QUADS)
            glTexCoord2f(0.0, 1.0); glVertex2f(-1.0, -1.0)
            glTexCoord2f(1.0, 1.0); glVertex2f( 1.0, -1.0)
            glTexCoord2f(1.0, 0.0); glVertex2f( 1.0,  1.0)
            glTexCoord2f(0.0, 0.0); glVertex2f(-1.0,  1.0)
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
        bmap = {Qt.LeftButton: "1", Qt.MiddleButton: "2", Qt.RightButton: "3"}
        b = bmap.get(e.button(), "")
        if b and self.width() and self.height():
            rx = self.offset_x + int(e.x() / self.width() * self.texture_width)
            ry = self.offset_y + int(e.y() / self.height() * self.texture_height)
            self.control_callback(f"MOUSE_PRESS {b} {rx} {ry}")
        e.accept()

    def mouseReleaseEvent(self, e):
        bmap = {Qt.LeftButton: "1", Qt.MiddleButton: "2", Qt.RightButton: "3"}
        b = bmap.get(e.button(), "")
        if b:
            self.control_callback(f"MOUSE_RELEASE {b}")
        e.accept()

    def wheelEvent(self, e):
        d = e.angleDelta()
        if d.y() != 0:
            b = "4" if d.y() > 0 else "5"
            self.control_callback(f"MOUSE_SCROLL {b}")
        elif d.x() != 0:
            b = "6" if d.x() < 0 else "7"
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

        text = event.text()
        if text and len(text) == 1 and ord(text) >= 0x20:
            if text == " ":
                return "space"
            return text

        key = event.key()
        key_map = {
            Qt.Key_Escape:"Escape", Qt.Key_Tab:"Tab", Qt.Key_Backtab:"Tab", Qt.Key_Backspace:"BackSpace",
            Qt.Key_Return:"Return", Qt.Key_Enter:"Return", Qt.Key_Insert:"Insert", Qt.Key_Delete:"Delete",
            Qt.Key_Pause:"Pause", Qt.Key_Print:"Print", Qt.Key_SysReq:"Sys_Req", Qt.Key_Clear:"Clear",
            Qt.Key_Home:"Home", Qt.Key_End:"End", Qt.Key_Left:"Left", Qt.Key_Up:"Up", Qt.Key_Right:"Right", Qt.Key_Down:"Down",
            Qt.Key_PageUp:"Page_Up", Qt.Key_PageDown:"Page_Down", Qt.Key_Shift:"Shift_L", Qt.Key_Control:"Control_L",
            Qt.Key_Meta:"Super_L", Qt.Key_Alt:"Alt_L", Qt.Key_AltGr:"Alt_R", Qt.Key_CapsLock:"Caps_Lock",
            Qt.Key_NumLock:"Num_Lock", Qt.Key_ScrollLock:"Scroll_Lock",
            Qt.Key_F1:"F1", Qt.Key_F2:"F2", Qt.Key_F3:"F3", Qt.Key_F4:"F4", Qt.Key_F5:"F5", Qt.Key_F6:"F6",
            Qt.Key_F7:"F7", Qt.Key_F8:"F8", Qt.Key_F9:"F9", Qt.Key_F10:"F10", Qt.Key_F11:"F11", Qt.Key_F12:"F12",
            Qt.Key_Space:"space",
        }
        if key in key_map:
            return key_map[key]

        if (Qt.Key_A <= key <= Qt.Key_Z) or (Qt.Key_0 <= key <= Qt.Key_9):
            try:
                return chr(key).lower()
            except Exception:
                pass

        if text:
            return text
        return None

class MainWindow(QMainWindow):
    def __init__(self, decoder_opts, rwidth, rheight, host_ip, udp_port, offset_x, offset_y, net_mode='lan', parent=None):
        super().__init__(parent)
        self.setWindowTitle("LinuxPlay")
        self.texture_width = rwidth
        self.texture_height = rheight
        self.offset_x = offset_x
        self.offset_y = offset_y
        self.host_ip = host_ip

        self.control_addr = (host_ip, CONTROL_PORT)
        self.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.control_sock.setblocking(False)
        
        try:
            self.control_sock.sendto(f"NET {net_mode}".encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.debug(f"NET announce failed: {e}")

        self.video_widget = VideoWidgetGL(self.send_control, rwidth, rheight, offset_x, offset_y, host_ip)
        self.setCentralWidget(self.video_widget)
        self.video_widget.setFocus()
        self.setAcceptDrops(True)

        video_url = (f"udp://0.0.0.0:{udp_port}?fifo_size=1000000&buffer_size=8388608&max_delay=200000&overrun_nonfatal=1" if net_mode == "wifi" else f"udp://0.0.0.0:{udp_port}?fifo_size=50000&buffer_size=262144&max_delay=0&overrun_nonfatal=1")
        logging.debug("Decoder options: %s", decoder_opts)

        self.decoder_thread = DecoderThread(video_url, decoder_opts)
        self.decoder_thread.frame_ready.connect(self.video_widget.updateFrame, Qt.QueuedConnection)
        self.decoder_thread.start()

        self.clip_timer = QTimer(self)
        self.clip_timer.timeout.connect(self._drain_clipboard_inbox)
        self.clip_timer.start(10)

    def _drain_clipboard_inbox(self):
        changed = False
        while not CLIPBOARD_INBOX.empty():
            text = CLIPBOARD_INBOX.get_nowait()
            cb = QApplication.clipboard()
            self.video_widget.ignore_clipboard = True
            if text != cb.text():
                cb.setText(text)
                changed = True
            self.video_widget.ignore_clipboard = False
        if changed:
            self.video_widget.last_clipboard = QApplication.clipboard().text()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event):
        urls = event.mimeData().urls()
        if urls:
            for url in urls:
                path = url.toLocalFile()
                if os.path.isdir(path):
                    for root, _, files in os.walk(path):
                        for f in files:
                            self._spawn_upload(os.path.join(root, f))
                elif os.path.isfile(path):
                    self._spawn_upload(path)
            event.acceptProposedAction()
        else:
            event.ignore()

    def _spawn_upload(self, file_path):
        threading.Thread(target=self.upload_file, args=(file_path,), daemon=True).start()

    def upload_file(self, file_path):
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect((self.control_addr[0], 7003))
            filename = os.path.basename(file_path).encode("utf-8")
            header = len(filename).to_bytes(4, "big") + filename
            size = os.path.getsize(file_path)
            header += size.to_bytes(8, "big")
            sock.sendall(header)
            with open(file_path, "rb") as f:
                while True:
                    data = f.read(4096)
                    if not data:
                        break
                    sock.sendall(data)
            sock.close()
            logging.info("Uploaded: %s", file_path)
        except Exception as e:
            logging.error("Upload error: %s", e)

    def send_control(self, msg):
        try:
            self.control_sock.sendto(msg.encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.error("Control send error: %s", e)

    def closeEvent(self, event):
        self.clip_timer.stop()
        self.decoder_thread.stop()
        self.decoder_thread.wait(2000)
        global audio_proc
        if audio_proc is not None:
            try:
                audio_proc.terminate()
            except Exception as e:
                logging.error("ffplay term error: %s", e)
        event.accept()

def clipboard_listener_client(_host_ip):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.bind(("", UDP_CLIPBOARD_PORT))
    except Exception as e:
        logging.error("Clipboard listener bind failed: %s", e)
        return
    while True:
        try:
            data, _ = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="ignore")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "HOST":
                new_content = tokens[2]
                CLIPBOARD_INBOX.put(new_content)
        except Exception as e:
            logging.error("Clipboard client error: %s", e)
            
def heartbeat_responder_client(host_ip):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("", UDP_HEARTBEAT_PORT))
        logging.info("Heartbeat responder listening on UDP %d", UDP_HEARTBEAT_PORT)
    except Exception as e:
        logging.error("Heartbeat responder bind failed: %s", e)
        return
    while True:
        try:
            data, addr = sock.recvfrom(1024)
            if data.startswith(b"PING"):
                try:
                    sock.sendto(b"PONG", (host_ip, UDP_HEARTBEAT_PORT))
                except Exception as e:
                    logging.debug("Heartbeat send error: %s", e)
        except Exception as e:
            logging.debug("Heartbeat recv error: %s", e)

def main():
    p = argparse.ArgumentParser(description="LinuxPlay Client (Linux/Windows)")
    p.add_argument("--decoder", choices=["none","h.264","h.265","av1"], default="none")
    p.add_argument("--host_ip", required=True)
    p.add_argument("--audio", choices=["enable","disable"], default="disable")
    p.add_argument("--monitor", default="0", help="Index or 'all'")
    p.add_argument("--hwaccel", choices=["auto","cpu","cuda","qsv","d3d11va","dxva2","vaapi"], default="auto",
                   help="Force a specific hardware decoder (Auto picks best for your OS/GPU).")
    p.add_argument("--debug", action="store_true")
    p.add_argument("--net", choices=["auto","lan","wifi"], default="auto")
    args = p.parse_args()

    fmt = QSurfaceFormat()
    fmt.setSwapInterval(0)
    QSurfaceFormat.setDefaultFormat(fmt)

    app = QApplication(sys.argv)

    logging.basicConfig(level=(logging.DEBUG if args.debug else logging.INFO),
                        format="%(asctime)s [%(levelname)s] %(message)s",
                        datefmt="%H:%M:%S")

    ok, host_info = tcp_handshake_client(args.host_ip)
    threading.Thread(target=heartbeat_responder_client, args=(args.host_ip,), daemon=True).start()
    if not ok:
        QMessageBox.critical(None, "Handshake Failed", "Could not negotiate with host.")
        sys.exit(1)
    host_encoder, monitor_info_str = host_info

    net_mode = args.net
    if net_mode == "auto":
        try:
            net_mode = detect_network_mode(args.host_ip)
        except Exception:
            net_mode = "lan"
    logging.info(f"Network mode: {net_mode}")

    try:
        monitors = []
        if ";" in monitor_info_str:
            for part in monitor_info_str.split(";"):
                if not part:
                    continue
                if '+' in part:
                    res, ox, oy = part.split('+'); w,h = map(int, res.split('x'))
                    monitors.append((w,h,int(ox),int(oy)))
                else:
                    w,h = map(int, part.split('x')); monitors.append((w,h,0,0))
        else:
            if '+' in monitor_info_str:
                res, ox, oy = monitor_info_str.split('+'); w,h = map(int, res.split('x'))
                monitors.append((w,h,int(ox),int(oy)))
            else:
                w,h = map(int, monitor_info_str.lower().split("x")); monitors.append((w,h,0,0))
    except Exception:
        logging.error("Monitor parse error, defaulting.")
        w,h = map(int, DEFAULT_RESOLUTION.lower().split("x"))
        monitors = [(w,h,0,0)]

    chosen = args.hwaccel
    if chosen == "auto":
        chosen = choose_auto_hwaccel()
    logging.info("HW accel selected: %s", chosen)

    decoder_opts = {}
    if chosen != "cpu":
        decoder_opts["hwaccel"] = chosen
        if chosen == "vaapi":
            decoder_opts["hwaccel_device"] = "/dev/dri/renderD128"

    threading.Thread(target=clipboard_listener_client, args=(args.host_ip,), daemon=True).start()

    global audio_proc
    if args.audio == "enable":
        audio_cmd = [
            "ffplay","-hide_banner","-loglevel","error",
            *([] if net_mode == "wifi" else ["-fflags","nobuffer"]), "-flags","low_delay",
            "-autoexit","-nodisp",
            "-i", (f"udp://@0.0.0.0:{UDP_AUDIO_PORT}?fifo_size=800000&buffer_size=4194304&max_delay=150000&pkt_size=1316&overrun_nonfatal=1" if net_mode == "wifi" else f"udp://@0.0.0.0:{UDP_AUDIO_PORT}?fifo_size=40000&max_delay=0&pkt_size=1316&overrun_nonfatal=1")
        ]
        logging.info("Audio with ffplay…")
        try:
            audio_proc = subprocess.Popen(audio_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except FileNotFoundError:
            logging.error("ffplay not found. Add ffmpeg/bin to PATH or install ffmpeg.")

    if args.monitor.lower() == "all":
        windows = []
        for i, mon in enumerate(monitors):
            w,h,ox,oy = mon
            win = MainWindow(decoder_opts, w, h, args.host_ip, DEFAULT_UDP_PORT + i, ox, oy)
            win.setWindowTitle(f"LinuxPlay - Monitor {i}")
            win.show()
            windows.append(win)
        ret = app.exec_()
    else:
        try:
            mon_index = int(args.monitor)
        except Exception:
            mon_index = 0
        if mon_index < 0 or mon_index >= len(monitors):
            mon_index = 0
        w,h,ox,oy = monitors[mon_index]
        win = MainWindow(decoder_opts, w, h, args.host_ip, DEFAULT_UDP_PORT + mon_index, ox, oy)
        win.setWindowTitle(f"LinuxPlay - Monitor {mon_index}")
        win.show()
        ret = app.exec_()

    if audio_proc:
        try:
            audio_proc.terminate()
        except Exception as e:
            logging.error("ffplay term error: %s", e)
    sys.exit(ret)

if __name__ == "__main__":
    main()

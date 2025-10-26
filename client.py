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
import psutil
import statistics
import shutil
import json
import av
import numpy as np
import struct

from PyQt5.QtWidgets import QApplication, QMainWindow, QMessageBox, QOpenGLWidget
from PyQt5.QtCore import QThread, pyqtSignal, Qt, QTimer, QPoint
from PyQt5.QtGui import QSurfaceFormat, QPainter, QFont, QColor
from OpenGL.GL import *
from queue import Queue

try:
    HERE = os.path.dirname(os.path.abspath(__file__))
    ffbin = os.path.join(HERE, "ffmpeg", "bin")
    if os.name == "nt" and os.path.exists(os.path.join(ffbin, "ffmpeg.exe")):
        os.environ["PATH"] = ffbin + os.pathsep + os.environ.get("PATH", "")
except Exception:
    pass

DEFAULT_UDP_PORT = 5000
CONTROL_PORT = 7000
TCP_HANDSHAKE_PORT = 7001
UDP_CLIPBOARD_PORT = 7002
UDP_FILE_PORT = 7003
UDP_HEARTBEAT_PORT = 7004
UDP_GAMEPAD_PORT = 7005
UDP_AUDIO_PORT = 6001

DEFAULT_RESOLUTION = "1920x1080"

IS_WINDOWS = py_platform.system() == "Windows"
IS_LINUX   = py_platform.system() == "Linux"

CLIPBOARD_INBOX = Queue()
audio_proc = None
CLIENT_STATE = {
    "connected": False,
    "last_heartbeat": 0.0,
    "net_mode": "lan",
    "reconnecting": False
}

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

def _best_ts_pkt_size(mtu_guess: int, ipv6: bool) -> int:
    if mtu_guess <= 0:
        mtu_guess = 1500
    overhead = 48 if ipv6 else 28
    max_payload = max(512, mtu_guess - overhead)
    return max(188, (max_payload // 188) * 188)

def detect_network_mode(host_ip: str) -> str:
    try:
        if IS_LINUX:
            import subprocess, re, os
            out = subprocess.check_output(["ip", "route", "get", host_ip],
                                          universal_newlines=True,
                                          stderr=subprocess.STDOUT)
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
                  f"(Get-NetRoute -DestinationPrefix {host_ip}/32 | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias"]
            alias = subprocess.check_output(ps, universal_newlines=True,
                                            stderr=subprocess.DEVNULL).strip()
            if alias:
                ps2 = ["powershell", "-NoProfile", "-Command",
                       f"($a = Get-NetAdapter -Name '{alias.replace('\"','')}') | Select-Object -Expand NdisPhysicalMedium"]
                medium = subprocess.check_output(ps2, universal_newlines=True,
                                                 stderr=subprocess.DEVNULL).strip().lower()
                if "wireless" in medium or "802.11" in medium:
                    return "wifi"
            return "lan"
    except Exception:
        return "lan"

def tcp_handshake_client(host_ip):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(5)
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
        CLIENT_STATE["connected"] = True
        CLIENT_STATE["last_heartbeat"] = time.time()
        return (True, (host_encoder, monitor_info))
    logging.error("Invalid handshake response: %s", resp)
    return (False, None)

def heartbeat_responder(host_ip):
    def loop():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                sock.bind(("", UDP_HEARTBEAT_PORT))
            except OSError as e:
                logging.error(f"Heartbeat bind failed: {e}")
                return
            sock.settimeout(2)
            logging.info("Heartbeat responder active on UDP %s", UDP_HEARTBEAT_PORT)
            while CLIENT_STATE["connected"]:
                try:
                    data, addr = sock.recvfrom(256)
                    if data == b"PING":
                        sock.sendto(b"PONG", addr)
                        CLIENT_STATE["last_heartbeat"] = time.time()
                except socket.timeout:
                    if time.time() - CLIENT_STATE["last_heartbeat"] > 10:
                        CLIENT_STATE["connected"] = False
                        CLIENT_STATE["reconnecting"] = True
                except Exception:
                    time.sleep(0.2)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t

def clipboard_listener(app_clipboard):
    def loop():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                sock.bind(("", UDP_CLIPBOARD_PORT))
            except OSError as e:
                logging.error(f"Clipboard listener bind failed: {e}")
                return
            logging.info("Listening for clipboard updates on UDP %s", UDP_CLIPBOARD_PORT)
            while CLIENT_STATE["connected"]:
                try:
                    data, _ = sock.recvfrom(65535)
                    msg = data.decode("utf-8", errors="replace").strip()
                    if msg.startswith("CLIPBOARD_UPDATE HOST"):
                        text = msg.split("HOST", 1)[1].strip()
                        if text:
                            app_clipboard.blockSignals(True)
                            app_clipboard.setText(text)
                            app_clipboard.blockSignals(False)
                except Exception:
                    time.sleep(0.2)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t

def audio_listener(host_ip):
    def loop():
        global audio_proc
        cmd = [
            "ffplay",
            "-hide_banner", "-loglevel", "error",
            "-nodisp", "-autoexit",
            "-fflags", "nobuffer",
            "-flags", "low_delay",
            "-f", "mpegts",
            f"udp://{host_ip}:{UDP_AUDIO_PORT}?overrun_nonfatal=1&buffer_size=32768"
        ]
        logging.info("Audio listener: %s", " ".join(cmd))
        try:
            audio_proc = subprocess.Popen(
                cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL
            )
            audio_proc.wait()
        except Exception as e:
            logging.error("Audio listener failed: %s", e)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t

class DecoderThread(QThread):
    frame_ready = pyqtSignal(object)

    def __init__(self, input_url, decoder_opts, ultra=False):
        super().__init__()
        self.input_url = input_url
        self.decoder_opts = dict(decoder_opts or {})
        self.decoder_opts.setdefault("probesize", "32")
        self.decoder_opts.setdefault("analyzeduration", "0")
        self.decoder_opts.setdefault("scan_all_pmts", "1")
        self.decoder_opts.setdefault("fflags", "nobuffer")
        self.decoder_opts.setdefault("flags", "low_delay")
        self.decoder_opts.setdefault("reorder_queue_size", "0")
        self.decoder_opts.setdefault("rtbufsize", "2M")
        self.decoder_opts.setdefault("fpsprobesize", "1")

        self._running = True
        self._sw_fallback_done = False
        self.ultra = ultra
        self._emit_interval = 0.0
        self._last_emit = 0.0
        self._frame_count = 0
        self._avg_decode_time = 0.0
        self._restart_delay = 0.5
        self._last_error = ""
        self._has_first_frame = False

    def _open_container(self):
        logging.debug("Opening stream with opts: %s", self.decoder_opts)
        return av.open(self.input_url, format="mpegts", options=self.decoder_opts)

    def run(self):
        while self._running:
            container = None
            try:
                container = self._open_container()
                vstream = next((s for s in container.streams if s.type == "video"), None)
                if not vstream:
                    logging.warning("No video stream detected, retrying...")
                    time.sleep(0.5)
                    continue

                cc = vstream.codec_context
                cc.thread_count = 1 if self.ultra else 2
                try:
                    cc.low_delay = True
                    cc.skip_frame = "NONREF"
                except Exception:
                    pass

                for frame in container.decode(video=0):
                    if not self._running:
                        break
                    if not frame or frame.is_corrupt:
                        continue

                    t0 = time.perf_counter()
                    arr = frame.to_ndarray(format="rgb24")
                    if not arr.flags["C_CONTIGUOUS"]:
                        arr = np.ascontiguousarray(arr, dtype=np.uint8)

                    self._has_first_frame = True
                    self.frame_ready.emit((arr, frame.width, frame.height))

                    t1 = time.perf_counter()
                    self._frame_count += 1
                    decode_time = (t1 - t0) * 1000
                    self._avg_decode_time = (
                        0.9 * self._avg_decode_time + 0.1 * decode_time
                        if self._frame_count > 1 else decode_time
                    )

                    if self._emit_interval > 0:
                        elapsed = time.time() - self._last_emit
                        if elapsed < self._emit_interval:
                            continue
                    self._last_emit = time.time()

                if self._running:
                    if not self._has_first_frame:
                        logging.info("Still waiting for video data...")
                    else:
                        logging.warning("Stream ended — reconnecting in %.1fs...", self._restart_delay)
                    time.sleep(self._restart_delay)

            except Exception as e:
                err = str(e)
                if err != self._last_error:
                    logging.error(f"Decode error: {err}")
                    self._last_error = err

                if not self._sw_fallback_done and "hwaccel" in self.decoder_opts:
                    logging.warning("HW decode failed — switching to CPU.")
                    self.decoder_opts.pop("hwaccel", None)
                    self.decoder_opts.pop("hwaccel_device", None)
                    self._sw_fallback_done = True
                    continue

                if self._running:
                    time.sleep(self._restart_delay)

            finally:
                try:
                    if container:
                        container.close()
                except Exception:
                    pass

    def stop(self):
        self._running = False
        time.sleep(0.05)

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
        self.frame_data = None
        self._pending_resize = None

        self.clipboard = QApplication.clipboard()
        self.clipboard.dataChanged.connect(self.on_clipboard_change)
        self.last_clipboard = self.clipboard.text()
        self.ignore_clipboard = False

        self.texture_id = None
        self.pbo_ids = []
        self.current_pbo = 0

        self._proc = psutil.Process(os.getpid())
        self._frame_times = []
        self._fps = 0.0
        self._cpu = 0.0
        self._ram = 0.0
        self._last_stats_update = 0.0
        self._last_mouse_ts = 0.0
        self._mouse_throttle = 0.0025
        self._last_frame_recv = time.time()

    def on_clipboard_change(self):
        new_text = self.clipboard.text()
        if self.ignore_clipboard or not new_text or new_text == self.last_clipboard:
            return
        self.last_clipboard = new_text
        msg = f"CLIPBOARD_UPDATE CLIENT {new_text}".encode("utf-8")
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.sendto(msg, (self.host_ip, UDP_CLIPBOARD_PORT))
        except Exception:
            pass

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
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE)
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
        glFlush()

    def resizeTexture(self, w, h):
        if (w, h) != (self.texture_width, self.texture_height):
            logging.info(f"Resize texture {self.texture_width}x{self.texture_height} → {w}x{h}")
            self._pending_resize = (w, h)

    def paintGL(self):
        glClear(GL_COLOR_BUFFER_BIT)
        now = time.time()

        has_signal = (now - self._last_frame_recv) < 2.5

        if not self.frame_data:
            self._draw_overlay_text("Waiting for signal…" if not has_signal else "No video")
            return

        arr, fw, fh = self.frame_data

        if self._pending_resize:
            w, h = self._pending_resize
            self._initialize_texture(w, h)
            self._pending_resize = None

        data = np.ascontiguousarray(arr, dtype=np.uint8)
        size = data.nbytes

        current_pbo = self.pbo_ids[self.current_pbo]
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, current_pbo)
        try:
            ptr = glMapBufferRange(GL_PIXEL_UNPACK_BUFFER, 0, size,
                                   GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT | GL_MAP_UNSYNCHRONIZED_BIT)
            if ptr:
                from ctypes import memmove, c_void_p
                memmove(c_void_p(ptr), data.ctypes.data, size)
                glUnmapBuffer(GL_PIXEL_UNPACK_BUFFER)
            else:
                glBufferSubData(GL_PIXEL_UNPACK_BUFFER, 0, data)
        except Exception:
            pass

        glBindTexture(GL_TEXTURE_2D, self.texture_id)
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1)
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, fw, fh, GL_RGB, GL_UNSIGNED_BYTE, None)
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)

        aspect_tex = fw / float(fh)
        aspect_win = self.width() / float(self.height())
        if aspect_win > aspect_tex:
            sx = aspect_tex / aspect_win
            sy = 1.0
        else:
            sx = 1.0
            sy = aspect_win / aspect_tex

        glEnable(GL_TEXTURE_2D)
        glBegin(GL_QUADS)
        glTexCoord2f(0.0, 1.0); glVertex2f(-sx, -sy)
        glTexCoord2f(1.0, 1.0); glVertex2f(sx, -sy)
        glTexCoord2f(1.0, 0.0); glVertex2f(sx, sy)
        glTexCoord2f(0.0, 0.0); glVertex2f(-sx, sy)
        glEnd()
        glDisable(GL_TEXTURE_2D)
        glBindTexture(GL_TEXTURE_2D, 0)

        self.current_pbo = (self.current_pbo + 1) % len(self.pbo_ids)

        self._frame_times.append(now)
        if len(self._frame_times) > 90:
            self._frame_times.pop(0)
        if len(self._frame_times) >= 2:
            diffs = [t2 - t1 for t1, t2 in zip(self._frame_times, self._frame_times[1:])]
            mean_diff = statistics.mean(diffs)
            self._fps = 1.0 / mean_diff if mean_diff > 0 else 0.0

        if now - self._last_stats_update >= 1.0:
            self._last_stats_update = now
            try:
                self._cpu = self._proc.cpu_percent(interval=None)
                self._ram = self._proc.memory_info().rss / (1024 * 1024)
            except Exception:
                pass

        overlay = f"FPS: {self._fps:.0f} | CPU: {self._cpu:.0f}% | RAM: {self._ram:.0f} MB"
        if not CLIENT_STATE["connected"]:
            overlay += " | RECONNECTING…"
        elif CLIENT_STATE["reconnecting"]:
            overlay += " | Weak Signal"

        self._draw_overlay_text(overlay)

    def _draw_overlay_text(self, text):
        p = QPainter(self)
        p.setRenderHint(QPainter.TextAntialiasing)
        p.setFont(QFont("Menlo", 10, QFont.Medium))
        rect = p.fontMetrics().boundingRect(text)
        rect.adjust(-10, -6, 10, 6)
        rect.moveTopRight(QPoint(self.width() - 12, 10))
        p.setBrush(QColor(0, 0, 0, 180))
        p.setPen(Qt.NoPen)
        p.drawRoundedRect(rect, 8, 8)
        p.setPen(QColor(240, 240, 240))
        p.drawText(rect.adjusted(10, 0, 0, 0), Qt.AlignVCenter, text)
        p.end()

    def updateFrame(self, frame_tuple):
        self.frame_data = frame_tuple
        _, fw, fh = frame_tuple
        if (fw, fh) != (self.texture_width, self.texture_height):
            self.resizeTexture(fw, fh)
        self._last_frame_recv = time.time()
        if self.isVisible():
            self.update()

    def send_mouse_packet(self, pkt_type, bmask, x, y):
        msg = f"MOUSE_PKT {pkt_type} {bmask} {x} {y}"
        try:
            self.control_callback(msg)
        except Exception:
            pass

    def _scaled_mouse_coords(self, e):
        ww, wh = self.width(), self.height()
        fw, fh = self.texture_width, self.texture_height
        aspect_tex = fw / float(fh)
        aspect_win = ww / float(wh)

        if aspect_win > aspect_tex:
            view_h = wh
            view_w = aspect_tex / aspect_win * ww
            offset_x = (ww - view_w) / 2.0
            offset_y = 0
        else:
            view_w = ww
            view_h = aspect_win / aspect_tex * wh
            offset_x = 0
            offset_y = (wh - view_h) / 2.0

        nx = (e.x() - offset_x) / view_w
        ny = (e.y() - offset_y) / view_h

        nx = min(max(nx, 0.0), 1.0)
        ny = min(max(ny, 0.0), 1.0)

        rx = self.offset_x + int(nx * fw)
        ry = self.offset_y + int(ny * fh)
        return rx, ry

    def mousePressEvent(self, e):
        bmap = {Qt.LeftButton: 1, Qt.MiddleButton: 2, Qt.RightButton: 4}
        bmask = bmap.get(e.button(), 0)
        if bmask:
            rx, ry = self._scaled_mouse_coords(e)
            self.send_mouse_packet(1, bmask, rx, ry)
        e.accept()

    def mouseMoveEvent(self, e):
        now = time.time()
        if now - self._last_mouse_ts < self._mouse_throttle:
            return
        self._last_mouse_ts = now

        rx, ry = self._scaled_mouse_coords(e)
        buttons = 0
        if e.buttons() & Qt.LeftButton: buttons |= 1
        if e.buttons() & Qt.MiddleButton: buttons |= 2
        if e.buttons() & Qt.RightButton: buttons |= 4
        self.send_mouse_packet(2, buttons, rx, ry)
        e.accept()

    def mouseReleaseEvent(self, e):
        bmap = {Qt.LeftButton: 1, Qt.MiddleButton: 2, Qt.RightButton: 4}
        bmask = bmap.get(e.button(), 0)
        if bmask:
            rx, ry = self._scaled_mouse_coords(e)
            self.send_mouse_packet(3, bmask, rx, ry)
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
        text = event.text()
        if text and len(text) == 1 and ord(text) >= 0x20:
            return "space" if text == " " else text
        key = event.key()
        key_map = {
            Qt.Key_Escape: "Escape", Qt.Key_Tab: "Tab", Qt.Key_Backtab: "Tab",
            Qt.Key_Backspace: "BackSpace", Qt.Key_Return: "Return", Qt.Key_Enter: "Return",
            Qt.Key_Insert: "Insert", Qt.Key_Delete: "Delete", Qt.Key_Pause: "Pause",
            Qt.Key_Print: "Print", Qt.Key_Home: "Home", Qt.Key_End: "End",
            Qt.Key_Left: "Left", Qt.Key_Up: "Up", Qt.Key_Right: "Right", Qt.Key_Down: "Down",
            Qt.Key_PageUp: "Page_Up", Qt.Key_PageDown: "Page_Down",
            Qt.Key_Shift: "Shift_L", Qt.Key_Control: "Control_L",
            Qt.Key_Meta: "Super_L", Qt.Key_Alt: "Alt_L", Qt.Key_AltGr: "Alt_R",
            Qt.Key_CapsLock: "Caps_Lock", Qt.Key_NumLock: "Num_Lock",
            Qt.Key_ScrollLock: "Scroll_Lock",
        }
        if key in key_map:
            return key_map[key]
        if (Qt.Key_A <= key <= Qt.Key_Z) or (Qt.Key_0 <= key <= Qt.Key_9):
            try:
                return chr(key).lower()
            except Exception:
                pass
        return text or None

class GamepadThread(threading.Thread):
    def __init__(self, host_ip, port, path_hint=None):
        super().__init__(daemon=True)
        self.host_ip = host_ip
        self.port = port
        self.path_hint = path_hint
        self._running = True
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

    def _find_device(self):
        try:
            from evdev import InputDevice, list_devices
        except Exception:
            return None
        if self.path_hint:
            try:
                return InputDevice(self.path_hint)
            except Exception:
                return None
        candidates = []
        for p in list_devices():
            try:
                d = InputDevice(p)
                name = (d.name or "").lower()
                if any(k in name for k in ("controller", "gamepad", "xbox", "dualshock", "dual sense", "8bitdo", "ps")):
                    candidates.append(d)
                    continue
                caps = d.capabilities(verbose=True)
                if any(n for (typ, codes) in caps for (code, n) in (codes or []) if n.startswith(("BTN_", "ABS_"))):
                    candidates.append(d)
            except Exception:
                pass
        if not candidates:
            return None

        def score(dev):
            s = 0
            try:
                n = (dev.name or "").lower()
                if "controller" in n or "gamepad" in n: s += 5
                if "xbox" in n or "dual" in n or "8bitdo" in n or "ps" in n: s += 3
                caps = dev.capabilities(verbose=True)
                if any((codes or []) for (_t, codes) in caps): s += 1
            except Exception:
                pass
            return s

        candidates.sort(key=score, reverse=True)
        return candidates[0]

    def run(self):
        if not IS_LINUX:
            return
        try:
            from evdev import ecodes, InputDevice
        except Exception:
            return

        dev = self._find_device()
        if not dev:
            return

        try:
            dev.grab()
        except Exception:
            pass

        pack_event = struct.Struct("!Bhh").pack
        sendto = self.sock.sendto
        addr = (self.host_ip, self.port)

        try:
            for event in dev.read_loop():
                if not self._running:
                    break
                t = int(event.type)
                c = int(event.code)
                v = int(event.value)
                if t in (ecodes.EV_KEY, ecodes.EV_ABS, ecodes.EV_SYN):
                    try:
                        sendto(pack_event(t, c, v), addr)
                    except Exception:
                        pass
        except Exception:
            pass

        try:
            dev.ungrab()
        except Exception:
            pass

    def stop(self):
        self._running = False
        try:
            self.sock.close()
        except Exception:
            pass

class MainWindow(QMainWindow):
    def __init__(self, decoder_opts, rwidth, rheight, host_ip, udp_port,
                 offset_x, offset_y, net_mode='lan', parent=None, ultra=False, gamepad="disable", gamepad_dev=None):
        super().__init__(parent)
        self.setWindowTitle("LinuxPlay")
        self.texture_width, self.texture_height = rwidth, rheight
        self.offset_x, self.offset_y = offset_x, offset_y
        self.host_ip, self.ultra = host_ip, ultra
        self._running, self._restarts = True, 0
        self.gamepad_mode = gamepad
        self.gamepad_dev = gamepad_dev
        self.control_addr = (host_ip, CONTROL_PORT)
        self.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.control_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.control_sock.setblocking(False)

        try:
            self.control_sock.sendto(f"NET {net_mode}".encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.debug(f"NET announce failed: {e}")

        self.video_widget = VideoWidgetGL(self.send_control, rwidth, rheight,
                                          offset_x, offset_y, host_ip)
        self.setCentralWidget(self.video_widget)
        self.video_widget.setFocus()
        self.setAcceptDrops(True)

        mtu_guess = int(os.environ.get("LINUXPLAY_MTU", "1500"))
        pkt = _best_ts_pkt_size(mtu_guess, False)
        self.video_url = (
            f"udp://@0.0.0.0:{udp_port}"
            f"?pkt_size={pkt}"
            f"&reuse=1&buffer_size=65536&fifo_size=32768"
            f"&overrun_nonfatal=1&max_delay=0"
        )

        self.decoder_opts = dict(decoder_opts)
        logging.debug("Decoder options: %s", self.decoder_opts)

        self._start_decoder_thread()
        self._start_background_threads()

        self.clip_timer = QTimer(self)
        self.clip_timer.timeout.connect(self._drain_clipboard_inbox)
        self.clip_timer.start(10)

        self.status_timer = QTimer(self)
        self.status_timer.timeout.connect(self._poll_connection_state)
        self.status_timer.start(1000)

    def _start_background_threads(self):
        try:
            self._heartbeat_thread = heartbeat_responder(self.host_ip)
        except Exception as e:
            logging.error(f"Heartbeat responder failed: {e}")
            self._heartbeat_thread = None

        try:
            self._audio_thread = audio_listener(self.host_ip)
        except Exception as e:
            logging.error(f"Audio listener failed: {e}")
            self._audio_thread = None

        try:
            self._clip_thread = clipboard_listener(QApplication.clipboard())
        except Exception as e:
            logging.error(f"Clipboard listener failed: {e}")
            self._clip_thread = None
        self._gp_thread = None
        if self.gamepad_mode == "enable" and IS_LINUX:
            try:
                self._gp_thread = GamepadThread(self.host_ip, UDP_GAMEPAD_PORT, self.gamepad_dev)
                self._gp_thread.start()
                logging.info("Controller forwarding started from %s -> %s", self.gamepad_dev or "/dev/input/event*", f"{self.host_ip}:{UDP_GAMEPAD_PORT}")
            except Exception as e:
                logging.error("Gamepad thread failed: %s", e)
                self._gp_thread = None
    def _start_decoder_thread(self):
        self.decoder_thread = DecoderThread(self.video_url, self.decoder_opts, ultra=self.ultra)
        self.decoder_thread.frame_ready.connect(self.video_widget.updateFrame, Qt.DirectConnection)
        self.decoder_thread.finished.connect(self._on_decoder_exit)
        self.decoder_thread.start()
        logging.info("Decoder thread started")

    def _on_decoder_exit(self):
        if not self._running:
            return
        self._restarts += 1
        delay = min(1.0 + (self._restarts * 0.3), 5.0)
        logging.warning(f"Decoder thread exited — attempting restart in {delay:.1f}s")
        QTimer.singleShot(int(delay * 1000), self._restart_decoder_safe)

    def _restart_decoder_safe(self):
        if self._running:
            try:
                self._start_decoder_thread()
            except Exception as e:
                logging.error(f"Decoder restart failed: {e}")

    def _poll_connection_state(self):
        now = time.time()
        age = now - CLIENT_STATE.get("last_heartbeat", 0)
        if age > 6 and CLIENT_STATE["connected"]:
            CLIENT_STATE["connected"], CLIENT_STATE["reconnecting"] = False, True
            logging.warning("Lost heartbeat from host")
        elif age <= 6 and CLIENT_STATE["reconnecting"]:
            CLIENT_STATE["connected"], CLIENT_STATE["reconnecting"] = True, False
            logging.info("Heartbeat restored")

    def _drain_clipboard_inbox(self):
        changed = False
        while not CLIPBOARD_INBOX.empty():
            text = CLIPBOARD_INBOX.get_nowait()
            cb = QApplication.clipboard()
            current = cb.text()
            if text and text != current:
                self.video_widget.ignore_clipboard = True
                cb.setText(text)
                self.video_widget.ignore_clipboard = False
                changed = True
        if changed:
            self.video_widget.last_clipboard = QApplication.clipboard().text()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()
        else:
            event.ignore()

    def dropEvent(self, event):
        urls = event.mimeData().urls()
        if not urls:
            event.ignore()
            return

        files_to_upload = []
        for url in urls:
            path = url.toLocalFile()
            if os.path.isdir(path):
                for root, _, files in os.walk(path):
                    for f in files:
                        files_to_upload.append(os.path.join(root, f))
            elif os.path.isfile(path):
                files_to_upload.append(path)

        for fpath in files_to_upload:
            threading.Thread(target=self.upload_file, args=(fpath,), daemon=True).start()
        event.acceptProposedAction()

    def upload_file(self, file_path):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                sock.connect((self.control_addr[0], UDP_FILE_PORT))
                filename = os.path.basename(file_path).encode("utf-8")
                header = len(filename).to_bytes(4, "big") + filename
                size = os.path.getsize(file_path)
                header += size.to_bytes(8, "big")
                sock.sendall(header)
                with open(file_path, "rb") as f:
                    while True:
                        chunk = f.read(4096)
                        if not chunk:
                            break
                        sock.sendall(chunk)
            logging.info(f"Uploaded: {file_path}")
        except Exception as e:
            logging.error(f"Upload error for {file_path}: {e}")

    def send_control(self, msg):
        try:
            self.control_sock.sendto(msg.encode("utf-8"), self.control_addr)
        except Exception as e:
            logging.error(f"Control send error: {e}")

    def closeEvent(self, event):
        self._running = False
        CLIENT_STATE["connected"] = False
        logging.info("Closing client window…")

        try:
            self.control_sock.sendto(b"GOODBYE", self.control_addr)
            logging.info("Sent GOODBYE to host")
        except Exception as e:
            logging.debug(f"GOODBYE send failed: {e}")

        for timer_name in ("clip_timer", "status_timer"):
            timer = getattr(self, timer_name, None)
            if timer:
                try:
                    timer.stop()
                except Exception:
                    pass

        if hasattr(self, "decoder_thread"):
            try:
                self.decoder_thread.stop()
                self.decoder_thread.wait(2000)
            except Exception as e:
                logging.debug(f"Decoder cleanup error: {e}")
        if getattr(self, "_gp_thread", None):
            try:
                self._gp_thread.stop()
            except Exception:
                pass
        global audio_proc
        if audio_proc:
            try:
                audio_proc.terminate()
                audio_proc.wait(timeout=2)
            except Exception as e:
                logging.error(f"ffplay term error: {e}")
            audio_proc = None

        try:
            self.control_sock.close()
        except Exception:
            pass

        event.accept()

def main():
    p = argparse.ArgumentParser(description="LinuxPlay Client (Linux/Windows)")
    p.add_argument("--decoder", choices=["none","h.264","h.265"], default="none")
    p.add_argument("--host_ip", required=True)
    p.add_argument("--audio", choices=["enable","disable"], default="disable")
    p.add_argument("--monitor", default="0", help="Index or 'all'")
    p.add_argument("--hwaccel", choices=["auto","cpu","cuda","qsv","d3d11va","dxva2","vaapi"], default="auto")
    p.add_argument("--debug", action="store_true")
    p.add_argument("--net", choices=["auto","lan","wifi"], default="auto")
    p.add_argument("--ultra", action="store_true", help="Enable ultra-low-latency (LAN only). Auto-disabled on Wi-Fi/WAN.")
    p.add_argument("--gamepad", choices=["enable","disable"], default="enable")
    p.add_argument("--gamepad_dev", default=None)
    args = p.parse_args()

    os.environ["vblank_mode"] = "0"
    os.environ["__GL_SYNC_TO_VBLANK"] = "0"
    os.environ["__GL_SYNC_DISPLAY_DEVICE"] = "none"
    os.environ["QT_LOGGING_RULES"] = "qt.qpa.*=false"
    if IS_WINDOWS:
        os.environ["QT_OPENGL"] = "angle"
        os.environ["QT_ANGLE_PLATFORM"] = "d3d11"
    else:
        os.environ.setdefault("QT_OPENGL", "desktop")
        os.environ.setdefault("QT_XCB_GL_INTEGRATION", "xcb_egl")

    fmt = QSurfaceFormat()
    fmt.setSwapInterval(0)
    fmt.setSwapBehavior(QSurfaceFormat.SingleBuffer)
    QSurfaceFormat.setDefaultFormat(fmt)

    try:
        ps = psutil.Process(os.getpid())
        ps.nice(-5)
    except Exception:
        pass

    app = QApplication(sys.argv)
    logging.basicConfig(level=(logging.DEBUG if args.debug else logging.INFO),
                        format="%(asctime)s [%(levelname)s] %(message)s",
                        datefmt="%H:%M:%S")

    ok, host_info = tcp_handshake_client(args.host_ip)
    if not ok or not host_info:
        QMessageBox.critical(None, "Handshake Failed", "Could not negotiate with host.")
        sys.exit(1)
    host_encoder, monitor_info_str = host_info
    CLIENT_STATE["connected"] = True
    CLIENT_STATE["last_heartbeat"] = time.time()

    net_mode = args.net
    if net_mode == "auto":
        try:
            net_mode = detect_network_mode(args.host_ip)
        except Exception:
            net_mode = "lan"
    logging.info(f"Network mode: {net_mode}")

    ultra_active = args.ultra and (net_mode == "lan")
    if args.ultra and not ultra_active:
        logging.info("Ultra requested but disabled on %s; using safe buffering.", net_mode)
    elif ultra_active:
        logging.info("⚡ Ultra mode enabled (LAN): minimal buffering, no B-frame reordering.")

    try:
        monitors = []
        parts = [p for p in monitor_info_str.split(";") if p]
        for part in parts:
            if "+" in part:
                res, ox, oy = part.split("+"); w, h = map(int, res.split("x"))
                monitors.append((w, h, int(ox), int(oy)))
            else:
                w, h = map(int, part.split("x")); monitors.append((w, h, 0, 0))
        if not monitors:
            raise ValueError
    except Exception:
        logging.error("Monitor parse error, defaulting to %s", DEFAULT_RESOLUTION)
        w, h = map(int, DEFAULT_RESOLUTION.split("x"))
        monitors = [(w, h, 0, 0)]

    chosen = args.hwaccel
    if chosen == "auto":
        chosen = choose_auto_hwaccel()
    logging.info("HW accel selected: %s", chosen)

    decoder_opts = {}
    if chosen != "cpu":
        decoder_opts["hwaccel"] = chosen
        if chosen == "vaapi":
            decoder_opts["hwaccel_device"] = "/dev/dri/renderD128"

    if ultra_active:
        decoder_opts.update({
            "fflags": "nobuffer",
            "flags": "low_delay",
            "flags2": "+fast",
            "probesize": "32",
            "analyzeduration": "0",
            "rtbufsize": "512k",
            "threads": "1",
            "skip_frame": "noref",
        })

    if args.monitor.lower() == "all":
        windows = []
        for i, (w, h, ox, oy) in enumerate(monitors):
            win = MainWindow(decoder_opts, w, h, args.host_ip, DEFAULT_UDP_PORT + i, ox, oy, net_mode, ultra=ultra_active, gamepad=args.gamepad, gamepad_dev=args.gamepad_dev)
            win.setWindowTitle(f"LinuxPlay - Monitor {i}")
            win.show()
            windows.append(win)
        ret = app.exec_()
    else:
        try:
            idx = int(args.monitor)
        except Exception:
            idx = 0
        if idx < 0 or idx >= len(monitors):
            idx = 0
        w, h, ox, oy = monitors[idx]
        win = MainWindow(decoder_opts, w, h, args.host_ip, DEFAULT_UDP_PORT + idx, ox, oy, net_mode, ultra=ultra_active, gamepad=args.gamepad, gamepad_dev=args.gamepad_dev)
        win.setWindowTitle(f"LinuxPlay - Monitor {idx}")
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

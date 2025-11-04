#!/usr/bin/env python3
import argparse
import base64
import contextlib
import ctypes
import hashlib
import importlib.util
import json
import logging
import mmap
import os
import platform as py_platform
import re
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path
from queue import Queue

import av
import numpy as np
import psutil
from OpenGL.GL import *
from PyQt5.QtCore import Qt, QThread, QTimer, pyqtSignal
from PyQt5.QtGui import QSurfaceFormat
from PyQt5.QtWidgets import QApplication, QInputDialog, QLineEdit, QMainWindow, QMessageBox, QOpenGLWidget


try:
    from evdev import InputDevice, ecodes, list_devices

    HAVE_EVDEV = True
except ImportError:
    HAVE_EVDEV = False

try:
    import pynvml

    HAVE_PYNVML = True
except ImportError:
    HAVE_PYNVML = False


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
IS_LINUX = py_platform.system() == "Linux"

CLIPBOARD_INBOX = Queue()


class AudioProcessManager:
    """Manages the audio process to avoid global state."""

    proc = None


audio_proc_manager = AudioProcessManager()
CLIENT_STATE = {"connected": False, "last_heartbeat": 0.0, "net_mode": "lan", "reconnecting": False}

try:
    HERE = Path(__file__).resolve().parent
    ffbin = HERE / "ffmpeg" / "bin"

    if os.name == "nt":
        ffmpeg_exe = ffbin / "ffmpeg.exe"
        if ffmpeg_exe.exists():
            os.environ["PATH"] = str(ffbin) + os.pathsep + os.environ.get("PATH", "")
    else:
        ffmpeg_bin = ffbin / "ffmpeg"
        if ffmpeg_bin.exists():
            os.environ["PATH"] = str(ffbin) + os.pathsep + os.environ.get("PATH", "")
except Exception as e:
    logging.debug(f"FFmpeg path init failed: {e}")


def _probe_hardware_capabilities():
    try:
        vk_spec = importlib.util.find_spec("vulkan")
        vk_available = vk_spec is not None
    except Exception:
        vk_available = False

    gbm_exists = any(Path(p).exists() for p in ("/dev/dri/renderD128", "/dev/dri/renderD129"))
    kms_exists = any(Path(p).exists() for p in ("/dev/dri/card0", "/dev/dri/card1"))
    logging.info(f"Hardware paths: GBM={gbm_exists}, KMS={kms_exists}, Vulkan={vk_available}")


_probe_hardware_capabilities()


def ffmpeg_hwaccels():
    try:
        out = subprocess.check_output(
            ["ffmpeg", "-hide_banner", "-hwaccels"], stderr=subprocess.STDOUT, universal_newlines=True
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
            out = subprocess.check_output(
                ["ip", "route", "get", host_ip], universal_newlines=True, stderr=subprocess.STDOUT
            )
            m = re.search(r"\bdev\s+(\S+)", out)
            iface = m.group(1) if m else ""
            if iface and Path(f"/sys/class/net/{iface}/wireless").exists():
                return "wifi"
            if iface.startswith("wl"):
                return "wifi"
            return "lan"
        if IS_WINDOWS:
            ps = [
                "powershell",
                "-NoProfile",
                "-Command",
                f"(Get-NetRoute -DestinationPrefix {host_ip}/32 | Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias",
            ]
            alias = subprocess.check_output(ps, universal_newlines=True, stderr=subprocess.DEVNULL).strip()
            if alias:
                # Remove quotes from alias name
                alias_clean = alias.replace('"', "")
                ps2 = [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    f"($a = Get-NetAdapter -Name '{alias_clean}') | Select-Object -Expand NdisPhysicalMedium",
                ]
                medium = (
                    subprocess.check_output(ps2, universal_newlines=True, stderr=subprocess.DEVNULL).strip().lower()
                )
                if "wireless" in medium or "802.11" in medium:
                    return "wifi"
            return "lan"
    except Exception:
        return "lan"
    return "lan"


def _read_pem_cert_fingerprint(pem_path: str) -> str:
    try:
        data = Path(pem_path).read_text(encoding="utf-8")
        m = re.search(r"-----BEGIN CERTIFICATE-----\s+([A-Za-z0-9+/=\s]+?)\s+-----END CERTIFICATE-----", data, re.S)
        if not m:
            return ""
        der = base64.b64decode("".join(m.group(1).split()))
        return hashlib.sha256(der).hexdigest().upper()
    except Exception:
        return ""


def tcp_handshake_client(host_ip, pin=None):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(5)
    try:
        logging.info("Handshake to %s:%s", host_ip, TCP_HANDSHAKE_PORT)
        sock.connect((host_ip, TCP_HANDSHAKE_PORT))
        try:
            here = Path(__file__).resolve().parent
            cert_path = here / "client_cert.pem"
            key_path = here / "client_key.pem"
        except Exception:
            cert_path = Path("client_cert.pem")
            key_path = Path("client_key.pem")
        if cert_path.exists() and key_path.exists():
            fp_hex = _read_pem_cert_fingerprint(cert_path)
            if fp_hex:
                try:
                    sock.sendall(f"HELLO CERTFP:{fp_hex}".encode())
                    resp = sock.recv(1024).decode("utf-8", errors="replace").strip()
                    logging.debug("Cert handshake response: %s", resp)
                    if resp.startswith("OK:"):
                        parts = resp.split(":", 2)
                        host_encoder = parts[1].strip()
                        monitor_info = parts[2].strip() if len(parts) > 2 else DEFAULT_RESOLUTION
                        sock.close()
                        CLIENT_STATE["connected"] = True
                        CLIENT_STATE["last_heartbeat"] = time.time()
                        logging.info("Authenticated via client certificate (FP %s…) — PIN skipped", fp_hex[:12])
                        return (True, (host_encoder, monitor_info))
                    if resp.startswith("FAIL:UNTRUSTEDCERT"):
                        logging.warning("Client cert not yet trusted by host — falling back to PIN.")
                    else:
                        logging.warning("Unexpected response to CERTFP auth (%s) — falling back to PIN", resp)
                except Exception as _e:
                    logging.debug("CERTFP path failed: %s — falling back to PIN", _e)

        code = (pin or "").strip()
        if not code or len(code) != 6 or not code.isdigit():
            dlg = QInputDialog()
            dlg.setWindowTitle("Enter Host PIN")
            dlg.setLabelText("6-digit PIN (rotates every 30s):")
            dlg.setInputMode(QInputDialog.TextInput)
            dlg.resize(360, 150)

            le = dlg.findChild(QLineEdit)
            if le is None:
                dlg.setTextValue("")
                le = dlg.findChild(QLineEdit)
            if le is not None:
                le.setEchoMode(QLineEdit.Password)
                le.setMaxLength(6)
                le.setPlaceholderText("••••••")

            ok = dlg.exec_()
            code = dlg.textValue().strip()
            if not ok or not code or not code.isdigit() or len(code) != 6:
                sock.close()
                logging.error("PIN entry cancelled or invalid.")
                QMessageBox.critical(None, "Invalid PIN", "PIN entry was cancelled or invalid.")
                return (False, None)

        sock.sendall(f"HELLO {code}".encode())
        resp = sock.recv(1024).decode("utf-8", errors="replace").strip()
        logging.debug("Handshake response: %s", resp)

        if resp.startswith("OK:"):
            parts = resp.split(":", 2)
            host_encoder = parts[1].strip()
            monitor_info = parts[2].strip() if len(parts) > 2 else DEFAULT_RESOLUTION
            sock.close()
            CLIENT_STATE["connected"] = True
            CLIENT_STATE["last_heartbeat"] = time.time()
            return (True, (host_encoder, monitor_info))

        if resp.startswith("BUSY"):
            logging.error("Host is already in a session with another client.")
            QMessageBox.critical(None, "Host Busy", "The host is already connected to another client.")
            sock.close()
            return (False, None)

        if resp.startswith("FAIL:BADPIN"):
            logging.error("Incorrect or expired PIN.")
            QMessageBox.critical(None, "Authentication Failed", "The PIN is incorrect or expired. Please try again.")
            sock.close()
            return (False, None)

        logging.error("Unexpected handshake response: %s", resp)
        QMessageBox.critical(None, "Handshake Error", f"Unexpected response from host:\n{resp}")
        sock.close()
        return (False, None)

    except Exception as e:
        logging.error("Handshake failed: %s", e)
        QMessageBox.critical(None, "Connection Error", f"Handshake failed:\n{e}")
        with contextlib.suppress(Exception):
            sock.close()
        return (False, None)


def heartbeat_responder(host_ip):
    def loop():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            # No bind() - use ephemeral port for outbound connection
            sock.settimeout(2)
            logging.info("Heartbeat responder active (outbound-only, no firewall port needed)")
            host_addr = (host_ip, UDP_HEARTBEAT_PORT)

            # Send initial PONG to establish connection with host
            try:
                sock.sendto(b"PONG", host_addr)
                logging.debug("Sent initial PONG to establish heartbeat connection")
            except Exception as e:
                logging.warning(f"Initial PONG send failed: {e}")

            while CLIENT_STATE["connected"]:
                try:
                    # Receive PING from host (will arrive on our ephemeral port)
                    data, addr = sock.recvfrom(256)
                    if data == b"PING" and addr[0] == host_ip:
                        # Reply to host
                        sock.sendto(b"PONG", host_addr)
                        CLIENT_STATE["last_heartbeat"] = time.time()
                except TimeoutError:  # noqa: PERF203
                    if time.time() - CLIENT_STATE["last_heartbeat"] > 10:
                        CLIENT_STATE["connected"] = False
                        CLIENT_STATE["reconnecting"] = True
                except Exception:
                    time.sleep(0.2)
                    continue

    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def clipboard_listener(app_clipboard, host_ip):
    def loop():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            # No bind() - use ephemeral port for receiving on same socket used for sending
            sock.settimeout(2)
            logging.info("Clipboard listener active (outbound-only, no firewall port needed)")
            host_addr = (host_ip, UDP_CLIPBOARD_PORT)

            # Send a keepalive to establish connection path through NAT/firewall
            try:
                sock.sendto(b"CLIPBOARD_KEEPALIVE", host_addr)
                logging.debug("Sent clipboard keepalive to establish connection")
            except Exception as e:
                logging.warning(f"Clipboard keepalive send failed: {e}")

            while CLIENT_STATE["connected"]:
                try:
                    data, addr = sock.recvfrom(65535)
                    # Only accept from authenticated host
                    if addr[0] != host_ip:
                        continue
                    msg = data.decode("utf-8", errors="replace").strip()
                    if msg.startswith("CLIPBOARD_UPDATE HOST"):
                        text = msg.split("HOST", 1)[1].strip()
                        if text:
                            app_clipboard.blockSignals(True)
                            app_clipboard.setText(text)
                            app_clipboard.blockSignals(False)
                except TimeoutError:
                    pass
                except Exception:
                    time.sleep(0.2)
                    continue

    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def audio_listener(host_ip):
    def loop():
        cmd = [
            "ffplay",
            "-hide_banner",
            "-loglevel",
            "info",
            "-nodisp",
            "-autoexit",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-af",
            "aresample=matrix_encoding=none",
            "-f",
            "mpegts",
            f"udp://{host_ip}:{UDP_AUDIO_PORT}?overrun_nonfatal=1&buffer_size=32768",
        ]
        logging.info("Audio listener: %s", " ".join(cmd))
        try:
            audio_proc_manager.proc = subprocess.Popen(
                cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, universal_newlines=True
            )
            for line in audio_proc_manager.proc.stdout:
                if "Stream #" in line and "Audio" in line:
                    logging.info(line.strip())
            audio_proc_manager.proc.wait()
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
        self._hw_name = None

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

                for attr, value in (
                    ("low_delay", True),
                    ("skip_frame", "NONREF"),
                    ("has_b_frames", False),
                    ("strict_std_compliance", "experimental"),
                    ("framerate", None),
                    ("delay", 0),
                ):
                    with contextlib.suppress(Exception):
                        setattr(cc, attr, value)

                with contextlib.suppress(Exception):
                    cc.flags2 = "+fast"

                hw_device = getattr(cc, "hw_device_ctx", None)
                if hw_device is None and "hwaccel" in self.decoder_opts:
                    hw_type = self.decoder_opts["hwaccel"]
                    dev = self.decoder_opts.get("hwaccel_device", None)

                    hw_type_map = {
                        "vaapi": "vaapi",
                        "nvdec": "cuda",
                        "cuda": "cuda",
                        "qsv": "qsv",
                        "d3d11va": "d3d11va",
                        "dxva2": "dxva2",
                    }
                    hw_type_norm = hw_type_map.get(hw_type, hw_type)

                    try:
                        if hasattr(av, "HwDeviceContext"):
                            if not dev:
                                if hw_type_norm == "vaapi":
                                    dev = "/dev/dri/renderD128"
                                elif hw_type_norm in ("cuda", "nvdec"):
                                    dev = "cuda"
                                else:
                                    dev = None
                            hw_ctx = av.HwDeviceContext.create(hw_type_norm, device=dev)
                            cc.hw_device_ctx = hw_ctx
                            self._hw_name = hw_type_norm
                            logging.info(f"DecoderThread: Using hardware decode via {hw_type_norm} ({dev or 'auto'})")
                        else:
                            logging.warning(
                                f"PyAV build lacks HwDeviceContext; using software decode for {hw_type_norm}."
                            )
                            self._hw_name = "CPU"
                            self.decoder_opts.pop("hwaccel", None)
                            self.decoder_opts.pop("hwaccel_device", None)
                    except Exception as e:
                        logging.warning(f"Hardware decode init failed for {hw_type_norm}: {e}")
                        self._hw_name = "CPU"
                        self.decoder_opts.pop("hwaccel", None)
                        self.decoder_opts.pop("hwaccel_device", None)

                t_decode = []

                for frame in container.decode(video=0):
                    if not self._running:
                        break
                    if not frame or frame.is_corrupt:
                        continue

                    t0 = time.perf_counter()
                    dmabuf_fd = None

                    try:
                        if frame.hw_frames_ctx:
                            pass
                        if hasattr(frame, "planes") and frame.planes:
                            p = frame.planes[0]
                            if hasattr(p, "fd"):
                                dmabuf_fd = p.fd
                            elif hasattr(p, "buffer_ptr") and isinstance(p.buffer_ptr, int):
                                dmabuf_fd = p.buffer_ptr
                    except Exception:
                        dmabuf_fd = None

                    if dmabuf_fd is not None:
                        self._has_first_frame = True
                        self.frame_ready.emit(("dmabuf", dmabuf_fd, frame.width, frame.height))
                    else:
                        arr = frame.to_ndarray(format="rgb24")
                        if not arr.flags["C_CONTIGUOUS"]:
                            arr = np.ascontiguousarray(arr, dtype=np.uint8)
                        self._has_first_frame = True
                        self.frame_ready.emit((arr, frame.width, frame.height))

                    t1 = time.perf_counter()
                    self._frame_count += 1
                    decode_time = (t1 - t0) * 1000
                    self._avg_decode_time = (
                        0.9 * self._avg_decode_time + 0.1 * decode_time if self._frame_count > 1 else decode_time
                    )

                    if len(t_decode) < 120:
                        t_decode.append(decode_time)
                    else:
                        avg = statistics.mean(t_decode)
                        logging.debug(f"Avg decode time: {avg:.2f} ms ({self._hw_name or 'CPU'})")
                        t_decode.clear()

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


class RenderBackend:
    def render_frame(self, frame_tuple):
        pass

    def is_valid(self):
        return False

    def name(self):
        return "unknown"


class RenderKMSDRM(RenderBackend):
    def __init__(self):
        self.valid = False
        self.fd = None
        self.gbm = None
        self.bo = None
        self.map = None
        self.stride = 0
        self.width = 0
        self.height = 0
        self.device_path = None

        for node in ("/dev/dri/renderD128", "/dev/dri/renderD129", "/dev/dri/card0", "/dev/dri/card1"):
            node_path = Path(node)
            if node_path.exists() and os.access(node, os.W_OK):
                try:
                    self.fd = os.open(node, os.O_RDWR | os.O_CLOEXEC)
                    self.device_path = node
                    self.valid = True
                    break
                except Exception:
                    continue

        if not self.valid:
            logging.debug("KMSDRM: no accessible DRM device found.")
            return

        try:
            self.libgbm = ctypes.CDLL("libgbm.so.1")

            self.libgbm.gbm_create_device.argtypes = [ctypes.c_int]
            self.libgbm.gbm_create_device.restype = ctypes.c_void_p

            self.libgbm.gbm_bo_create.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint32,
                ctypes.c_uint32,
                ctypes.c_uint32,
                ctypes.c_uint32,
            ]
            self.libgbm.gbm_bo_create.restype = ctypes.c_void_p

            self.libgbm.gbm_bo_get_stride.argtypes = [ctypes.c_void_p]
            self.libgbm.gbm_bo_get_stride.restype = ctypes.c_uint32

            self.libgbm.gbm_bo_destroy.argtypes = [ctypes.c_void_p]
            self.libgbm.gbm_device_destroy.argtypes = [ctypes.c_void_p]

            self.gbm = self.libgbm.gbm_create_device(self.fd)
            if not self.gbm:
                raise RuntimeError("gbm_create_device() failed")

            self.valid = True
            logging.info(f"KMSDRM initialized (safe render-node) via {self.device_path}")
        except Exception as e:
            logging.debug(f"KMSDRM init failed: {e}")
            self.valid = False

    def is_valid(self):
        return self.valid

    def name(self):
        return "KMSDRM"

    def _alloc_bo(self, w, h):
        if not self.valid or not self.gbm:
            return
        try:
            if self.bo:
                self.libgbm.gbm_bo_destroy(self.bo)
                self.bo = None

            DRM_FORMAT_ARGB8888 = 0x34325241
            GBM_BO_USE_RENDERING = 1 << 1

            self.bo = self.libgbm.gbm_bo_create(self.gbm, w, h, DRM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING)
            if not self.bo:
                raise RuntimeError("gbm_bo_create() failed")

            self.stride = self.libgbm.gbm_bo_get_stride(self.bo)
            size = self.stride * h

            if self.map:
                self.map.close()
            self.map = mmap.mmap(self.fd, size, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE, offset=0)
            self.width, self.height = w, h
            logging.debug(f"KMSDRM GBM buffer {w}x{h} stride={self.stride}")
        except Exception as e:
            logging.debug(f"KMSDRM alloc failed: {e}")
            self.valid = False

    def _import_dmabuf(self, fd, w, h):
        try:
            size = w * h * 4
            with mmap.mmap(fd, size, mmap.MAP_SHARED, mmap.PROT_READ) as buf:
                data = buf.read(size)
            logging.debug(f"KMSDRM: imported dmabuf FD={fd} ({w}x{h})")
            return np.frombuffer(data, dtype=np.uint8).reshape(h, w, 4)
        except Exception as e:
            logging.debug(f"KMSDRM: dmabuf import failed: {e}")
            return None

    def render_frame(self, frame_tuple):
        if not self.valid:
            return
        t0 = time.perf_counter()
        try:
            is_dmabuf = (
                isinstance(frame_tuple, tuple)
                and len(frame_tuple) == 4
                and isinstance(frame_tuple[0], str)
                and frame_tuple[0] == "dmabuf"
            )

            if is_dmabuf:
                _, fd, w, h = frame_tuple
                w, h = int(w), int(h)
                arr = self._import_dmabuf(fd, w, h)
                if not isinstance(arr, np.ndarray) or arr.size == 0:
                    return
            else:
                arr, w, h = frame_tuple
                w, h = int(w), int(h)
                if not isinstance(arr, np.ndarray) or arr.size == 0:
                    return

            cur_w = int(getattr(self, "width", 0) or 0)
            cur_h = int(getattr(self, "height", 0) or 0)
            if (w != cur_w) or (h != cur_h):
                self._alloc_bo(w, h)

            data = np.ascontiguousarray(arr, dtype=np.uint8)
            if self.map and hasattr(self.map, "write"):
                self.map.seek(0)
                self.map.write(data.tobytes())

            dt = (time.perf_counter() - t0) * 1000.0
            logging.debug(
                f"KMSDRM upload {w}x{h} ({data.nbytes / 1024 / 1024:.2f} MB) in {dt:.2f} ms to {self.device_path}"
            )
        except Exception as e:
            logging.debug(f"KMSDRM render error: {e}")


class RenderVulkan(RenderBackend):
    def __init__(self):
        try:
            self.valid = True
        except Exception:
            self.valid = False

    def is_valid(self):
        return self.valid

    def name(self):
        return "Vulkan"

    def render_frame(self, frame_tuple):
        try:
            if (
                isinstance(frame_tuple, tuple)
                and len(frame_tuple) == 4
                and isinstance(frame_tuple[0], str)
                and frame_tuple[0] == "dmabuf"
            ):
                return
            arr, w, h = frame_tuple
            if not isinstance(arr, np.ndarray) or arr.size == 0:
                return
            t0 = time.perf_counter()
            _ = np.mean(arr)
            dt = (time.perf_counter() - t0) * 1000.0
            logging.debug(f"Vulkan simulated render {int(w)}x{int(h)} in {dt:.2f} ms")
        except Exception as e:
            logging.debug(f"Vulkan render error: {e}")


class RenderOpenGL(RenderBackend):
    def __init__(self):
        self.valid = True

    def is_valid(self):
        return self.valid

    def name(self):
        return "OpenGL"

    def render_frame(self, frame_tuple):
        try:
            if (
                isinstance(frame_tuple, tuple)
                and len(frame_tuple) == 4
                and isinstance(frame_tuple[0], str)
                and frame_tuple[0] == "dmabuf"
            ):
                return
            arr, w, h = frame_tuple
            if not isinstance(arr, np.ndarray) or arr.size == 0:
                return
            t0 = time.perf_counter()
            _ = np.mean(arr)
            dt = (time.perf_counter() - t0) * 1000.0
            logging.debug(f"OpenGL simulated render {int(w)}x{int(h)} in {dt:.2f} ms")
        except Exception as e:
            logging.debug(f"OpenGL render error: {e}")


def pick_best_renderer():
    renderers = (RenderKMSDRM, RenderVulkan, RenderOpenGL)
    selected = None
    for renderer_cls in renderers:
        r = renderer_cls()
        logging.debug(f"Trying renderer: {r.name()} (valid={r.is_valid()})")
        if r.is_valid():
            selected = r
            logging.info(f"Renderer selected: {r.name()}")
            if hasattr(r, "device_path") and r.device_path:
                logging.info(f"Using device path: {r.device_path}")
            break

    if not selected:
        logging.warning("No GPU renderer found, using dummy software renderer.")
        selected = RenderBackend()
    return selected


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
        self._last_frame_recv = time.time()
        self._last_mouse_ts = 0.0
        self._mouse_throttle = 0.0025

        if not logging.getLogger().hasHandlers():
            logging.basicConfig(
                level=logging.DEBUG, format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%H:%M:%S"
            )

        logging.info("────────────────────────────────────────────")
        logging.info("Renderer Initialization Summary")
        logging.info(f"Session type: {os.environ.get('XDG_SESSION_TYPE', 'unknown')}")
        logging.info(f"Desktop: {os.environ.get('XDG_CURRENT_DESKTOP', 'unknown')}")
        logging.info(f"Display server: {os.environ.get('WAYLAND_DISPLAY') or os.environ.get('DISPLAY', 'n/a')}")
        for node in ("/dev/dri/renderD128", "/dev/dri/renderD129", "/dev/dri/card0", "/dev/dri/card1"):
            exists = "✅" if Path(node).exists() else "❌"
            access = "🟢" if os.access(node, os.W_OK) else "🔴"
            logging.info(f"  {node:<20} exists={exists} access={access}")
        logging.info("Renderer priority order: KMSDRM → Vulkan → OpenGL")

        self.renderer = pick_best_renderer()
        logging.info(f"Using render backend: {self.renderer.name()}")
        if hasattr(self.renderer, "device_path") and self.renderer.device_path:
            logging.info(f"Bound to device: {self.renderer.device_path}")
        logging.info("────────────────────────────────────────────")

    def on_clipboard_change(self):
        new_text = self.clipboard.text()
        if self.ignore_clipboard or not new_text or new_text == self.last_clipboard:
            return
        self.last_clipboard = new_text
        msg = f"CLIPBOARD_UPDATE CLIENT {new_text}".encode()
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

        buf_size = w * h * 3
        self.pbo_ids = list(glGenBuffers(3))
        for pbo in self.pbo_ids:
            glBindBuffer(GL_PIXEL_UNPACK_BUFFER, pbo)
            glBufferData(GL_PIXEL_UNPACK_BUFFER, buf_size, None, GL_STREAM_DRAW)
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)

        self.texture_width, self.texture_height = w, h
        self.current_pbo = 0
        glFlush()

    def resizeTexture(self, w, h):
        if (w, h) != (self.texture_width, self.texture_height):
            logging.info(f"Resize texture {self.texture_width}x{self.texture_height} → {w}x{h}")
            self._pending_resize = (w, h)

    def paintGL(self):
        if not self.frame_data:
            glClear(GL_COLOR_BUFFER_BIT)
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
        glBufferData(GL_PIXEL_UNPACK_BUFFER, size, None, GL_STREAM_DRAW)
        ptr = glMapBuffer(GL_PIXEL_UNPACK_BUFFER, GL_WRITE_ONLY)
        if ptr:
            ctypes.memmove(ptr, data.ctypes.data, size)
            glUnmapBuffer(GL_PIXEL_UNPACK_BUFFER)

        glBindTexture(GL_TEXTURE_2D, self.texture_id)
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1)
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, fw, fh, GL_RGB, GL_UNSIGNED_BYTE, None)
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0)

        aspect_tex = fw / float(fh)
        aspect_win = self.width() / float(self.height())
        if aspect_win > aspect_tex:
            sx, sy = (aspect_tex / aspect_win), 1.0
        else:
            sx, sy = 1.0, (aspect_win / aspect_tex)

        glClear(GL_COLOR_BUFFER_BIT)
        glEnable(GL_TEXTURE_2D)
        glBegin(GL_QUADS)
        glTexCoord2f(0.0, 1.0)
        glVertex2f(-sx, -sy)
        glTexCoord2f(1.0, 1.0)
        glVertex2f(sx, -sy)
        glTexCoord2f(1.0, 0.0)
        glVertex2f(sx, sy)
        glTexCoord2f(0.0, 0.0)
        glVertex2f(-sx, sy)
        glEnd()
        glDisable(GL_TEXTURE_2D)
        glBindTexture(GL_TEXTURE_2D, 0)
        glFlush()

        self.current_pbo = (self.current_pbo + 1) % len(self.pbo_ids)

    def updateFrame(self, frame_tuple):
        self.frame_data = frame_tuple
        _, fw, fh = frame_tuple
        if (fw, fh) != (self.texture_width, self.texture_height):
            self.resizeTexture(fw, fh)
        self._last_frame_recv = time.time()

        now = time.time()
        if not hasattr(self, "_frame_times"):
            self._frame_times = []
        self._frame_times.append(now)
        if len(self._frame_times) > 90:
            self._frame_times.pop(0)
        if len(self._frame_times) >= 2:
            diffs = [t2 - t1 for t1, t2 in zip(self._frame_times, self._frame_times[1:], strict=False)]
            mean_diff = statistics.mean(diffs)
            self._fps = 1.0 / mean_diff if mean_diff > 0 else 0.0

        try:
            self.renderer.render_frame(frame_tuple)
        except Exception as e:
            logging.debug(f"Renderer {self.renderer.name()} failed: {e}")

        if self.isVisible():
            t = time.time()
            if not hasattr(self, "_last_draw") or (t - getattr(self, "_last_draw", 0)) > (1 / 240):
                self._last_draw = t
                self.update()

    def _flush_pending_mouse(self):
        if not hasattr(self, "_pending_mouse") or self._pending_mouse is None:
            return
        now = time.time()
        if now - self._last_mouse_ts < self._mouse_throttle:
            return
        rx, ry, buttons = self._pending_mouse
        self.send_mouse_packet(2, buttons, rx, ry)
        self._last_mouse_ts = now
        self._pending_mouse = None

    def send_mouse_packet(self, pkt_type, bmask, x, y):
        msg = f"MOUSE_PKT {pkt_type} {bmask} {x} {y}"
        with contextlib.suppress(Exception):
            self.control_callback(msg)

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
        rx, ry = self._scaled_mouse_coords(e)
        buttons = 0
        if e.buttons() & Qt.LeftButton:
            buttons |= 1
        if e.buttons() & Qt.MiddleButton:
            buttons |= 2
        if e.buttons() & Qt.RightButton:
            buttons |= 4

        if not hasattr(self, "_pending_mouse"):
            self._pending_mouse = None
        self._pending_mouse = (rx, ry, buttons)

        self._flush_pending_mouse()
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
        if not HAVE_EVDEV:
            return None
        try:
            pass
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
                if "controller" in n or "gamepad" in n:
                    s += 5
                if "xbox" in n or "dual" in n or "8bitdo" in n or "ps" in n:
                    s += 3
                caps = dev.capabilities(verbose=True)
                if any((codes or []) for (_t, codes) in caps):
                    s += 1
            except Exception:
                pass
            return s

        candidates.sort(key=score, reverse=True)
        return candidates[0]

    def run(self):
        if not IS_LINUX:
            return
        if not HAVE_EVDEV:
            return
        try:
            pass
        except Exception:
            return

        dev = self._find_device()
        if not dev:
            return

        with contextlib.suppress(Exception):
            dev.grab()

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
                    with contextlib.suppress(Exception):
                        sendto(pack_event(t, c, v), addr)
        except Exception:
            pass

        with contextlib.suppress(Exception):
            dev.ungrab()

    def stop(self):
        self._running = False
        with contextlib.suppress(Exception):
            self.sock.close()


class MainWindow(QMainWindow):
    def __init__(
        self,
        decoder_opts,
        rwidth,
        rheight,
        host_ip,
        udp_port,
        offset_x,
        offset_y,
        net_mode="lan",
        parent=None,
        ultra=False,
        gamepad="disable",
        gamepad_dev=None,
    ):
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
            self.control_sock.sendto(f"NET {net_mode}".encode(), self.control_addr)
        except Exception as e:
            logging.debug(f"NET announce failed: {e}")

        self.video_widget = VideoWidgetGL(self.send_control, rwidth, rheight, offset_x, offset_y, host_ip)
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

        self._proc = psutil.Process(os.getpid())

        self._start_decoder_thread()
        self._start_background_threads()
        self._start_timers()

    def _start_timers(self):
        self.clip_timer = QTimer(self)
        self.clip_timer.timeout.connect(self._drain_clipboard_inbox)
        self.clip_timer.start(10)

        self.status_timer = QTimer(self)
        self.status_timer.timeout.connect(self._poll_connection_state)
        self.status_timer.start(1000)

        self.stats_timer = QTimer(self)
        self.stats_timer.timeout.connect(self._update_stats)
        self.stats_timer.start(1000)

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
            self._clip_thread = clipboard_listener(QApplication.clipboard(), self.host_ip)
        except Exception as e:
            logging.error(f"Clipboard listener failed: {e}")
            self._clip_thread = None

        self._gp_thread = None
        if self.gamepad_mode == "enable" and IS_LINUX:
            try:
                self._gp_thread = GamepadThread(self.host_ip, UDP_GAMEPAD_PORT, self.gamepad_dev)
                self._gp_thread.start()
                logging.info(
                    "Controller forwarding started from %s -> %s",
                    self.gamepad_dev or "/dev/input/event*",
                    f"{self.host_ip}:{UDP_GAMEPAD_PORT}",
                )
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

    def _read_gpu_usage(self):
        if not HAVE_PYNVML:
            return None
        try:
            pynvml.nvmlInit()
            handle = pynvml.nvmlDeviceGetHandleByIndex(0)
            util = pynvml.nvmlDeviceGetUtilizationRates(handle)
            return f"{util.gpu}% (NVENC)"
        except Exception:
            pass

        try:
            for card in Path("/sys/class/drm").iterdir():
                busy_path = card / "device" / "gpu_busy_percent"
                if busy_path.exists():
                    val = busy_path.read_text().strip()
                    return f"{val}% (VAAPI)"
        except Exception:
            pass

        try:
            cmd = ["timeout", "0.5", "intel_gpu_top", "-J"]
            out = subprocess.check_output(cmd, stderr=subprocess.DEVNULL).decode()
            if '"Busy"' in out:
                j = json.loads(out)
                busy = j["engines"]["Render/3D/0"]["busy"]
                return f"{busy}% (iGPU)"
        except Exception:
            pass

        return "N/A"

    def _update_stats(self):
        try:
            cpu = self._proc.cpu_percent(interval=None)
            mem = self._proc.memory_info().rss / (1024 * 1024)
            gpu = self._read_gpu_usage()
            fps = getattr(self.video_widget, "_fps", 0.0)
            renderer_name = getattr(self.video_widget.renderer, "name", lambda: "Unknown")()
            device_info = getattr(self.video_widget.renderer, "device_path", None)
            backend = f"{renderer_name} ({Path(device_info).name})" if device_info else renderer_name

            base_title = "LinuxPlay"
            status = ""
            if not CLIENT_STATE["connected"]:
                status = " | RECONNECTING…"
            elif CLIENT_STATE["reconnecting"]:
                status = " | Weak Signal"

            new_title = (
                f"{base_title} — {backend} | FPS: {fps:.0f} | CPU: {cpu:.0f}% | RAM: {mem:.0f} MB | GPU: {gpu}{status}"
            )
            self.setWindowTitle(new_title)
        except Exception as e:
            logging.debug(f"Stats update failed: {e}")

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
            if Path(path).is_dir():
                for root, _, files in os.walk(path):
                    files_to_upload.extend(str(Path(root) / f) for f in files)
            elif Path(path).is_file():
                files_to_upload.append(path)

        for fpath in files_to_upload:
            threading.Thread(target=self.upload_file, args=(fpath,), daemon=True).start()
        event.acceptProposedAction()

    def upload_file(self, file_path):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                sock.connect((self.control_addr[0], UDP_FILE_PORT))
                filename = Path(file_path).name.encode("utf-8")
                header = len(filename).to_bytes(4, "big") + filename
                size = Path(file_path).stat().st_size
                header += size.to_bytes(8, "big")
                sock.sendall(header)
                with Path(file_path).open("rb") as f:
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

        for timer_name in ("clip_timer", "status_timer", "stats_timer"):
            timer = getattr(self, timer_name, None)
            if timer:
                with contextlib.suppress(Exception):
                    timer.stop()

        if hasattr(self, "decoder_thread"):
            try:
                self.decoder_thread.stop()
                self.decoder_thread.wait(2000)
            except Exception as e:
                logging.debug(f"Decoder cleanup error: {e}")

        if getattr(self, "_gp_thread", None):
            with contextlib.suppress(Exception):
                self._gp_thread.stop()

        if audio_proc_manager.proc:
            try:
                audio_proc_manager.proc.terminate()
                audio_proc_manager.proc.wait(timeout=2)
            except Exception as e:
                logging.error(f"ffplay term error: {e}")
            audio_proc_manager.proc = None

        with contextlib.suppress(Exception):
            self.control_sock.close()

        event.accept()


def main():
    p = argparse.ArgumentParser(description="LinuxPlay Client (Linux/Windows)")
    p.add_argument("--decoder", choices=["none", "h.264", "h.265"], default="none")
    p.add_argument("--host_ip", required=True)
    p.add_argument("--pin", default=None, help="6-digit host PIN (optional; will prompt if required)")
    p.add_argument("--audio", choices=["enable", "disable"], default="disable")
    p.add_argument("--monitor", default="0", help="Index or 'all'")
    p.add_argument("--hwaccel", choices=["auto", "cpu", "cuda", "qsv", "d3d11va", "dxva2", "vaapi"], default="auto")
    p.add_argument("--debug", action="store_true")
    p.add_argument("--net", choices=["auto", "lan", "wifi"], default="auto")
    p.add_argument(
        "--ultra", action="store_true", help="Enable ultra-low-latency (LAN only). Auto-disabled on Wi-Fi/WAN."
    )
    p.add_argument("--gamepad", choices=["enable", "disable"], default="enable")
    p.add_argument("--gamepad_dev", default=None)
    args = p.parse_args()

    for var in list(os.environ):
        if var.startswith(("MESA_", "LIBGL_", "__GL_", "QT_LOGGING", "vblank_mode")):
            del os.environ[var]

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

    sys.stdout.reconfigure(line_buffering=True)
    sys.stderr.reconfigure(line_buffering=True)
    try:
        ps = psutil.Process(os.getpid())
        ps.nice(-5)
    except Exception:
        pass

    LOG_LEVEL = logging.DEBUG if args.debug else logging.INFO
    LOG_FORMAT = "%(asctime)s [%(levelname)s] %(message)s"
    LOG_DATEFMT = "%H:%M:%S"

    root_logger = logging.getLogger()
    root_logger.setLevel(LOG_LEVEL)
    for h in list(root_logger.handlers):
        root_logger.removeHandler(h)

    console = logging.StreamHandler(sys.stdout)
    console.setLevel(LOG_LEVEL)
    console.setFormatter(logging.Formatter(LOG_FORMAT, datefmt=LOG_DATEFMT))
    root_logger.addHandler(console)

    try:
        file_handler = logging.FileHandler("linuxplay_client.log", mode="w", encoding="utf-8")
        file_handler.setLevel(LOG_LEVEL)
        file_handler.setFormatter(logging.Formatter(LOG_FORMAT, datefmt=LOG_DATEFMT))
        root_logger.addHandler(file_handler)
    except Exception:
        pass

    logging.info("────────────────────────────────────────────")
    logging.info("LinuxPlay Client starting up")
    logging.info(f"Python: {sys.version.split()[0]}, Platform: {sys.platform}")
    logging.info("────────────────────────────────────────────")

    app = QApplication(sys.argv)

    ok, host_info = tcp_handshake_client(args.host_ip, args.pin)
    if not ok or not host_info:
        QMessageBox.critical(None, "Handshake Failed", "Could not negotiate with host.")
        sys.exit(1)
    _host_encoder, monitor_info_str = host_info
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
        logging.info("Ultra mode enabled (LAN): minimal buffering, no B-frame reordering.")

    try:
        monitors = []
        parts = [p for p in monitor_info_str.split(";") if p]
        for part in parts:
            if "+" in part:
                res, ox, oy = part.split("+")
                w, h = map(int, res.split("x"))
                monitors.append((w, h, int(ox), int(oy)))
            else:
                w, h = map(int, part.split("x"))
                monitors.append((w, h, 0, 0))
        if not monitors:
            raise ValueError
    except Exception:
        logging.error("Monitor parse error, defaulting to %s", DEFAULT_RESOLUTION)
        w, h = map(int, DEFAULT_RESOLUTION.split("x"))
        monitors = [(w, h, 0, 0)]

    chosen = args.hwaccel
    if chosen == "auto":
        chosen = choose_auto_hwaccel()
    logging.info(f"HW accel selected: {chosen}")

    decoder_opts = {}
    if chosen != "cpu":
        decoder_opts["hwaccel"] = chosen
        if chosen == "vaapi":
            decoder_opts["hwaccel_device"] = "/dev/dri/renderD128"

    if ultra_active:
        decoder_opts.update(
            {
                "fflags": "nobuffer",
                "flags": "low_delay",
                "flags2": "+fast",
                "probesize": "32",
                "analyzeduration": "0",
                "rtbufsize": "512k",
                "threads": "1",
                "skip_frame": "noref",
            }
        )

    windows = []
    if args.monitor.lower() == "all":
        for i, (w, h, ox, oy) in enumerate(monitors):
            win = MainWindow(
                decoder_opts,
                w,
                h,
                args.host_ip,
                DEFAULT_UDP_PORT + i,
                ox,
                oy,
                net_mode,
                ultra=ultra_active,
                gamepad=args.gamepad,
                gamepad_dev=args.gamepad_dev,
            )
            win.setWindowTitle(f"LinuxPlay — Monitor {i}")
            win.show()
            windows.append(win)
    else:
        try:
            idx = int(args.monitor)
        except Exception:
            idx = 0
        if idx < 0 or idx >= len(monitors):
            idx = 0
        w, h, ox, oy = monitors[idx]
        win = MainWindow(
            decoder_opts,
            w,
            h,
            args.host_ip,
            DEFAULT_UDP_PORT + idx,
            ox,
            oy,
            net_mode,
            ultra=ultra_active,
            gamepad=args.gamepad,
            gamepad_dev=args.gamepad_dev,
        )
        win.setWindowTitle(f"LinuxPlay — Monitor {idx}")
        win.show()
        windows.append(win)

    ret = app.exec_()

    try:
        if audio_proc_manager.proc:
            audio_proc_manager.proc.terminate()
    except Exception as e:
        logging.error("ffplay term error: %s", e)

    sys.exit(ret)


if __name__ == "__main__":
    main()

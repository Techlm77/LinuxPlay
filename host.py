#!/usr/bin/env python3
import os
import subprocess
import argparse
import sys
import logging
import time
import threading
import socket
import atexit
from shutil import which

UDP_VIDEO_PORT = 5000
UDP_AUDIO_PORT = 6001
UDP_CONTROL_PORT = 7000
UDP_CLIPBOARD_PORT = 7002
TCP_HANDSHAKE_PORT = 7001
FILE_UPLOAD_PORT = 7003
MULTICAST_IP = "239.0.0.1"
DEFAULT_FPS = "30"
DEFAULT_BITRATE = "8M"
DEFAULT_RES = "1920x1080"

class HostState:
    def __init__(self):
        self.video_threads = []
        self.audio_thread = None
        self.current_bitrate = DEFAULT_BITRATE
        self.host_password = None
        self.last_clipboard_content = ""
        self.ignore_clipboard_update = False
        self.should_terminate = False
        self.video_thread_lock = threading.Lock()
        self.clipboard_lock = threading.Lock()
        self.handshake_sock = None
        self.control_sock = None
        self.clipboard_listener_sock = None
        self.client_ip = None
        self.monitors = []

host_state = HostState()

def has_nvidia():
    return which("nvidia-smi") is not None

def has_vaapi():
    return os.path.exists("/dev/dri/renderD128")

def stop_all():
    host_state.should_terminate = True
    with host_state.video_thread_lock:
        for thread in host_state.video_threads:
            thread.stop()
            thread.join(timeout=2)
    if host_state.audio_thread:
        host_state.audio_thread.stop()
        host_state.audio_thread.join(timeout=2)
    if host_state.handshake_sock:
        try:
            host_state.handshake_sock.close()
        except:
            pass
    if host_state.control_sock:
        try:
            host_state.control_sock.close()
        except:
            pass
    if host_state.clipboard_listener_sock:
        try:
            host_state.clipboard_listener_sock.close()
        except:
            pass

def cleanup():
    stop_all()

atexit.register(cleanup)

class StreamThread(threading.Thread):
    def __init__(self, cmd, name):
        super().__init__(daemon=True)
        self.cmd = cmd
        self.name = name
        self.process = None
        self._running = True

    def run(self):
        logging.info("Starting %s stream: %s", self.name, " ".join(self.cmd))
        self.process = subprocess.Popen(
            self.cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            universal_newlines=True
        )
        while self._running:
            if host_state.should_terminate:
                break
            ret = self.process.poll()
            if ret is not None:
                out, err = self.process.communicate()
                logging.error("%s process ended unexpectedly. Return code: %s. Error output:\n%s",
                              self.name, ret, err)
                break
            time.sleep(0.5)

    def stop(self):
        self._running = False
        if self.process:
            self.process.terminate()

def shutil_which(cmd):
    import shutil
    return shutil.which(cmd)

def get_display(default=":0"):
    return os.environ.get("DISPLAY", default)

def detect_pulse_monitor():
    monitor = os.environ.get("PULSE_MONITOR")
    if monitor:
        return monitor
    if not shutil_which("pactl"):
        logging.warning("pactl not found, using default.monitor")
        return "default.monitor"
    try:
        output = subprocess.check_output(["pactl", "list", "short", "sources"], universal_newlines=True)
        for line in output.splitlines():
            parts = line.split()
            if len(parts) >= 2 and ".monitor" in parts[1]:
                return parts[1]
    except Exception as e:
        logging.error("Error detecting PulseAudio monitor: %s", e)
    return "default.monitor"

def detect_monitors():
    try:
        output = subprocess.check_output(["xrandr", "--listmonitors"], universal_newlines=True)
    except Exception as e:
        logging.error("Failed to detect monitors: %s", e)
        return []
    lines = output.strip().splitlines()
    monitors = []
    for line in lines[1:]:
        parts = line.split()
        for part in parts:
            if 'x' in part and '+' in part:
                try:
                    res_part, ox, oy = part.split('+')
                    w_str, h_str = res_part.split('x')
                    w = int(w_str.split('/')[0])
                    h = int(h_str.split('/')[0])
                    ox = int(ox)
                    oy = int(oy)
                    monitors.append((w, h, ox, oy))
                    break
                except Exception as e:
                    logging.error("Error parsing monitor info: %s", e)
                    continue
    return monitors

def build_video_cmd(args, bitrate, monitor_info, video_port):
    w, h, ox, oy = monitor_info
    video_size = f"{w}x{h}"
    disp = args.display
    if "." not in disp:
        disp = f"{disp}.0"
    input_arg = f"{disp}+{ox},{oy}"
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel", "error",
        "-threads", "0",
        "-f", "x11grab",
        "-framerate", args.framerate,
        "-video_size", video_size,
        "-i", input_arg
    ]
    preset = args.preset if args.preset else (
        "llhq" if (args.encoder in ["h.264", "h.265", "av1"] and has_nvidia()) else "ultrafast")
    gop = args.gop
    qp = args.qp
    tune = args.tune
    pix_fmt = args.pix_fmt

    if args.encoder == "h.264":
        if has_nvidia():
            encode = [
                "-c:v", "h264_nvenc",
                "-preset", preset,
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", pix_fmt
            ]
            if qp:
                encode.extend(["-qp", qp])
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "h264_vaapi",
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate
            ]
            if qp:
                encode.extend(["-qp", qp])
            else:
                encode.extend(["-qp", "20"])
        else:
            encode = [
                "-c:v", "libx264",
                "-preset", preset,
            ]
            if tune:
                encode.extend(["-tune", tune])
            else:
                encode.extend(["-tune", "zerolatency"])
            encode.extend([
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", pix_fmt
            ])
            if qp:
                encode.extend(["-qp", qp])
    elif args.encoder == "h.265":
        if has_nvidia():
            encode = [
                "-c:v", "hevc_nvenc",
                "-preset", preset,
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", pix_fmt
            ]
            if qp:
                encode.extend(["-qp", qp])
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "hevc_vaapi",
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate
            ]
            if qp:
                encode.extend(["-qp", qp])
            else:
                encode.extend(["-qp", "20"])
        else:
            encode = [
                "-c:v", "libx265",
                "-preset", preset,
            ]
            if tune:
                encode.extend(["-tune", tune])
            else:
                encode.extend(["-tune", "zerolatency"])
            encode.extend([
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate
            ])
            if qp:
                encode.extend(["-qp", qp])
    elif args.encoder == "av1":
        if has_nvidia():
            encode = [
                "-c:v", "av1_nvenc",
                "-preset", preset,
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", pix_fmt
            ]
            if qp:
                encode.extend(["-qp", qp])
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "av1_vaapi",
                "-g", gop,
                "-bf", "0",
                "-b:v", bitrate
            ]
            if qp:
                encode.extend(["-qp", qp])
            else:
                encode.extend(["-qp", "20"])
        else:
            encode = [
                "-c:v", "libaom-av1",
                "-strict", "experimental",
                "-cpu-used", "4",
                "-g", gop,
                "-b:v", bitrate
            ]
            if qp:
                encode.extend(["-qp", qp])
    else:
        encode = [
            "-c:v", "libx264",
            "-preset", preset,
        ]
        if tune:
            encode.extend(["-tune", tune])
        else:
            encode.extend(["-tune", "zerolatency"])
        encode.extend([
            "-g", gop,
            "-bf", "0",
            "-b:v", bitrate,
            "-pix_fmt", pix_fmt
        ])
        if qp:
            encode.extend(["-qp", qp])
    dest_ip = host_state.client_ip
    out = [
        "-f", "mpegts",
        f"udp://{dest_ip}:{video_port}?pkt_size=1316&buffer_size=65536"
    ]
    return cmd + encode + out

def build_audio_cmd():
    monitor_source = detect_pulse_monitor()
    return [
        "ffmpeg",
        "-hide_banner",
        "-loglevel", "error",
        "-fflags", "nobuffer",
        "-flags", "low_delay",
        "-f", "pulse",
        "-i", monitor_source,
        "-c:a", "libopus",
        "-b:a", "128k",
        "-f", "mpegts",
        f"udp://{MULTICAST_IP}:{UDP_AUDIO_PORT}?pkt_size=1316&buffer_size=65536"
    ]

def adaptive_bitrate_manager(args):
    while not host_state.should_terminate:
        time.sleep(30)
        if host_state.should_terminate:
            break
        with host_state.video_thread_lock:
            if host_state.current_bitrate == DEFAULT_BITRATE:
                try:
                    base = int("".join(filter(str.isdigit, DEFAULT_BITRATE)))
                    new_bitrate = f"{int(base*0.6)}M"
                except:
                    new_bitrate = DEFAULT_BITRATE
            else:
                new_bitrate = DEFAULT_BITRATE

            if new_bitrate != host_state.current_bitrate:
                logging.info("Adaptive ABR: Switching bitrate from %s to %s",
                             host_state.current_bitrate, new_bitrate)
                new_threads = []
                for i, mon in enumerate(host_state.monitors):
                    video_port = UDP_VIDEO_PORT + i
                    new_cmd = build_video_cmd(args, new_bitrate, mon, video_port)
                    new_thread = StreamThread(new_cmd, f"Video Monitor {i} (Adaptive)")
                    new_thread.start()
                    new_threads.append(new_thread)
                for thread in host_state.video_threads:
                    thread.stop()
                    thread.join()
                host_state.video_threads = new_threads
                host_state.current_bitrate = new_bitrate

def tcp_handshake_server(sock, encoder_str, args):
    logging.info("TCP Handshake server listening on port %s", TCP_HANDSHAKE_PORT)
    while not host_state.should_terminate:
        try:
            conn, addr = sock.accept()
            logging.info("TCP handshake connection from %s", addr)
            host_state.client_ip = addr[0]
            data = conn.recv(1024).decode("utf-8", errors="replace").strip()
            logging.info("Received handshake: '%s'", data)
            expected = f"PASSWORD:{host_state.host_password}" if host_state.host_password else "PASSWORD:"
            if data == expected:
                if host_state.monitors:
                    monitors_str = ";".join(f"{w}x{h}+{ox}+{oy}" for (w, h, ox, oy) in host_state.monitors)
                else:
                    monitors_str = DEFAULT_RES
                resp = f"OK:{encoder_str}:{monitors_str}"
                conn.sendall(resp.encode("utf-8"))
                logging.info("Handshake from %s successful. Sent %s", addr, resp)
            else:
                conn.sendall("FAIL".encode("utf-8"))
                logging.error("Handshake from %s failed. Expected '%s', got '%s'",
                              addr, expected, data)
            conn.close()
        except OSError:
            break
        except Exception as e:
            logging.error("TCP handshake server error: %s", e)
            break

def control_listener(sock):
    logging.info("Control listener active on UDP port %s", UDP_CONTROL_PORT)
    while not host_state.should_terminate:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="replace").strip()

            if host_state.host_password:
                prefix = f"PASSWORD:{host_state.host_password}:"
                if not msg.startswith(prefix):
                    logging.warning("Rejected control message from %s due to password mismatch.", addr)
                    continue
                msg = msg[len(prefix):].strip()

            tokens = msg.split()
            if not tokens:
                continue
            cmd = tokens[0]
            if cmd == "MOUSE_MOVE" and len(tokens) == 3:
                subprocess.Popen(["xdotool", "mousemove", tokens[1], tokens[2]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif cmd == "MOUSE_PRESS" and len(tokens) == 4:
                subprocess.Popen(["xdotool", "mousemove", tokens[2], tokens[3]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                subprocess.Popen(["xdotool", "mousedown", tokens[1]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif cmd == "MOUSE_RELEASE" and len(tokens) == 2:
                subprocess.Popen(["xdotool", "mouseup", tokens[1]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif cmd == "MOUSE_SCROLL" and len(tokens) == 2:
                subprocess.Popen(["xdotool", "click", tokens[1]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif cmd == "KEY_PRESS" and len(tokens) == 2:
                subprocess.Popen(["xdotool", "keydown", tokens[1]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            elif cmd == "KEY_RELEASE" and len(tokens) == 2:
                subprocess.Popen(["xdotool", "keyup", tokens[1]],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            else:
                logging.warning("Ignored unsupported control message: %s", msg)
        except OSError:
            break
        except Exception as e:
            logging.error("Control listener error: %s", e)
            break

def clipboard_monitor_host():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    while not host_state.should_terminate:
        try:
            proc = subprocess.run(
                ["xclip", "-o", "-selection", "clipboard"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                universal_newlines=True
            )
            current = proc.stdout.strip()
        except:
            current = ""
        with host_state.clipboard_lock:
            if (not host_state.ignore_clipboard_update and
               current and current != host_state.last_clipboard_content):
                host_state.last_clipboard_content = current
                msg = f"CLIPBOARD_UPDATE HOST {current}"
                sock.sendto(msg.encode("utf-8"), (MULTICAST_IP, UDP_CLIPBOARD_PORT))
                logging.info("Host clipboard updated and broadcast.")
        time.sleep(1)
    sock.close()

def clipboard_listener_host(sock):
    while not host_state.should_terminate:
        try:
            data, addr = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="replace")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "CLIENT":
                new_content = tokens[2]
                with host_state.clipboard_lock:
                    host_state.ignore_clipboard_update = True
                    proc = subprocess.run(
                        ["xclip", "-o", "-selection", "clipboard"],
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        universal_newlines=True
                    )
                    current = proc.stdout.strip()
                    if new_content != current:
                        p = subprocess.Popen(["xclip", "-selection", "clipboard", "-in"], stdin=subprocess.PIPE)
                        p.communicate(new_content.encode("utf-8"))
                        logging.info("Host clipboard updated from client.")
                    host_state.ignore_clipboard_update = False
        except OSError:
            break
        except Exception as e:
            logging.error("Host clipboard listener error: %s", e)
            break

def recvall(sock, n):
    data = b""
    while len(data) < n:
        packet = sock.recv(n - len(data))
        if not packet:
            return None
        data += packet
    return data

def file_upload_listener():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("", FILE_UPLOAD_PORT))
    s.listen(5)
    logging.info("File upload listener active on TCP port %s", FILE_UPLOAD_PORT)
    while not host_state.should_terminate:
        try:
            conn, addr = s.accept()
            logging.info("File upload connection from %s", addr)
            header = recvall(conn, 4)
            if not header:
                conn.close()
                continue
            filename_length = int.from_bytes(header, byteorder='big')
            filename_bytes = recvall(conn, filename_length)
            filename = filename_bytes.decode('utf-8')
            filesize_bytes = recvall(conn, 8)
            file_size = int.from_bytes(filesize_bytes, byteorder='big')
            dest_dir = os.path.expanduser("~/LinuxPlayDrop")
            if not os.path.exists(dest_dir):
                os.makedirs(dest_dir)
            dest_path = os.path.join(dest_dir, filename)
            with open(dest_path, 'wb') as f:
                remaining = file_size
                while remaining > 0:
                    chunk_size = 4096 if remaining >= 4096 else remaining
                    chunk = conn.recv(chunk_size)
                    if not chunk:
                        break
                    f.write(chunk)
                    remaining -= len(chunk)
            logging.info("Received file %s (%d bytes)", dest_path, file_size)
            conn.close()
        except Exception as e:
            logging.error("File upload error: %s", e)
    s.close()

def main():
    parser = argparse.ArgumentParser(description="Remote Desktop Host (Production Ready)")
    parser.add_argument("--encoder", choices=["none", "h.264", "h.265", "av1"], default="none")
    parser.add_argument("--framerate", default=DEFAULT_FPS)
    parser.add_argument("--bitrate", default=DEFAULT_BITRATE)
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable")
    parser.add_argument("--adaptive", action="store_true")
    parser.add_argument("--password", default="")
    parser.add_argument("--display", default=":0")
    parser.add_argument("--preset", default="", help="Encoder preset (if empty, built-in default is used)")
    parser.add_argument("--gop", default="30", help="Group of Pictures size (keyframe interval)")
    parser.add_argument("--qp", default="", help="Quantization Parameter (leave empty for none)")
    parser.add_argument("--tune", default="", help="Tune option (e.g., zerolatency)")
    parser.add_argument("--pix_fmt", default="yuv420p", help="Pixel format (default: yuv420p)")
    parser.add_argument("--debug", action="store_true", help="Enable debug logging")
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

    host_state.host_password = args.password if args.password else None
    host_state.current_bitrate = args.bitrate

    host_state.monitors = detect_monitors()
    if not host_state.monitors:
        try:
            w, h = map(int, DEFAULT_RES.lower().split("x"))
        except:
            w, h = map(int, "1920x1080".split("x"))
        host_state.monitors = [(w, h, 0, 0)]

    encoder_str = args.encoder

    host_state.handshake_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    host_state.handshake_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        host_state.handshake_sock.bind(("", TCP_HANDSHAKE_PORT))
        host_state.handshake_sock.listen(5)
    except Exception as e:
        logging.error("Failed to bind TCP handshake port %s: %s", TCP_HANDSHAKE_PORT, e)
        stop_all()
        sys.exit(1)

    host_state.control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    host_state.control_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        host_state.control_sock.bind(("", UDP_CONTROL_PORT))
    except Exception as e:
        logging.error("Failed to bind control port %s: %s", UDP_CONTROL_PORT, e)
        stop_all()
        sys.exit(1)

    host_state.clipboard_listener_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    host_state.clipboard_listener_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        host_state.clipboard_listener_sock.bind(("", UDP_CLIPBOARD_PORT))
    except Exception as e:
        logging.error("Failed to bind clipboard port %s: %s", UDP_CLIPBOARD_PORT, e)
        stop_all()
        sys.exit(1)

    handshake_thread = threading.Thread(target=tcp_handshake_server, args=(host_state.handshake_sock, encoder_str, args), daemon=True)
    handshake_thread.start()

    clipboard_monitor_thread = threading.Thread(target=clipboard_monitor_host, daemon=True)
    clipboard_monitor_thread.start()

    clipboard_listener_thread = threading.Thread(target=clipboard_listener_host, args=(host_state.clipboard_listener_sock,), daemon=True)
    clipboard_listener_thread.start()

    file_thread = threading.Thread(target=file_upload_listener, daemon=True)
    file_thread.start()

    logging.info("Waiting for client to connect for unicast video streaming...")
    while host_state.client_ip is None and not host_state.should_terminate:
        time.sleep(0.1)
    logging.info("Client connected from %s, starting video streams.", host_state.client_ip)

    with host_state.video_thread_lock:
        host_state.video_threads = []
        for i, mon in enumerate(host_state.monitors):
            video_port = UDP_VIDEO_PORT + i
            video_cmd = build_video_cmd(args, host_state.current_bitrate, mon, video_port)
            logging.debug("Video command for monitor %d: %s", i, " ".join(video_cmd))
            stream_thread = StreamThread(video_cmd, f"Video Monitor {i}")
            stream_thread.start()
            host_state.video_threads.append(stream_thread)

    if args.audio == "enable":
        audio_cmd = build_audio_cmd()
        logging.debug("Audio command: %s", " ".join(audio_cmd))
        host_state.audio_thread = StreamThread(audio_cmd, "Audio")
        host_state.audio_thread.start()

    if args.adaptive:
        abr_thread = threading.Thread(target=adaptive_bitrate_manager, args=(args,), daemon=True)
        abr_thread.start()

    ctrl_thread = threading.Thread(target=control_listener, args=(host_state.control_sock,), daemon=True)
    ctrl_thread.start()

    logging.info("Host running. Press Ctrl+C to exit.")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        logging.info("Shutting down host...")
        stop_all()
        sys.exit(0)

if __name__ == "__main__":
    main()

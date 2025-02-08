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
MULTICAST_IP = "239.0.0.1"
DEFAULT_RES = "1920x1080"
DEFAULT_FPS = "30"
DEFAULT_BITRATE = "8M"

video_thread_lock = threading.Lock()
current_video_thread = None
current_audio_thread = None
current_bitrate = DEFAULT_BITRATE
host_password = None
last_clipboard_content = ""
clipboard_lock = threading.Lock()
ignore_clipboard_update = False
handshake_sock = None
control_sock = None
clipboard_listener_sock = None
should_terminate = False

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S"
)

def has_nvidia():
    return which("nvidia-smi") is not None

def has_vaapi():
    return os.path.exists("/dev/dri/renderD128")

def stop_all():
    global should_terminate
    should_terminate = True
    with video_thread_lock:
        if current_video_thread:
            current_video_thread.stop()
            current_video_thread.join(timeout=2)
    if current_audio_thread:
        current_audio_thread.stop()
        current_audio_thread.join(timeout=2)
    if handshake_sock:
        try:
            handshake_sock.close()
        except:
            pass
    if control_sock:
        try:
            control_sock.close()
        except:
            pass
    if clipboard_listener_sock:
        try:
            clipboard_listener_sock.close()
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
        logging.info("Starting {} stream: {}".format(self.name, " ".join(self.cmd)))
        self.process = subprocess.Popen(
            self.cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            universal_newlines=True
        )
        while self._running:
            if should_terminate:
                break
            ret = self.process.poll()
            if ret is not None:
                out, err = self.process.communicate()
                logging.error("{} process ended unexpectedly. Return code: {}. Error output:\n{}".format(self.name, ret, err))
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
    disp = os.environ.get("DISPLAY", default)
    return disp

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
        logging.error("Error detecting PulseAudio monitor: {}".format(e))
    return "default.monitor"

def build_video_cmd(args, bitrate):
    disp = args.display
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel", "error",
        "-threads", "0",
        "-f", "x11grab",
        "-framerate", args.framerate,
        "-video_size", args.resolution,
        "-i", disp
    ]
    if args.encoder == "h.264":
        if has_nvidia():
            encode = [
                "-c:v", "h264_nvenc",
                "-preset", "llhq",
                "-g", "30",
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", "yuv420p"
            ]
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "h264_vaapi",
                "-g", "30",
                "-bf", "0",
                "-qp", "20",
                "-b:v", bitrate
            ]
        else:
            encode = [
                "-c:v", "libx264",
                "-preset", "ultrafast",
                "-tune", "zerolatency",
                "-g", "30",
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", "yuv420p"
            ]
    elif args.encoder == "h.265":
        if has_nvidia():
            encode = [
                "-c:v", "hevc_nvenc",
                "-preset", "llhq",
                "-g", "30",
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", "yuv420p"
            ]
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "hevc_vaapi",
                "-g", "30",
                "-bf", "0",
                "-qp", "20",
                "-b:v", bitrate
            ]
        else:
            encode = [
                "-c:v", "libx265",
                "-preset", "ultrafast",
                "-tune", "zerolatency",
                "-g", "30",
                "-bf", "0",
                "-b:v", bitrate
            ]
    elif args.encoder == "av1":
        if has_nvidia():
            encode = [
                "-c:v", "av1_nvenc",
                "-preset", "llhq",
                "-g", "30",
                "-bf", "0",
                "-b:v", bitrate,
                "-pix_fmt", "yuv420p"
            ]
        elif has_vaapi():
            encode = [
                "-vf", "format=nv12,hwupload",
                "-vaapi_device", "/dev/dri/renderD128",
                "-c:v", "av1_vaapi",
                "-g", "30",
                "-bf", "0",
                "-qp", "20",
                "-b:v", bitrate
            ]
        else:
            encode = [
                "-c:v", "libaom-av1",
                "-strict", "experimental",
                "-cpu-used", "4",
                "-g", "30",
                "-b:v", bitrate
            ]
    else:
        encode = [
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-tune", "zerolatency",
            "-g", "30",
            "-bf", "0",
            "-b:v", bitrate,
            "-pix_fmt", "yuv420p"
        ]
    out = [
        "-f", "mpegts",
        "udp://{}:{}?pkt_size=1316&buffer_size=65536".format(MULTICAST_IP, UDP_VIDEO_PORT)
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
        "udp://{}:{}?pkt_size=1316&buffer_size=65536".format(MULTICAST_IP, UDP_AUDIO_PORT)
    ]

def adaptive_bitrate_manager(args):
    global current_bitrate, current_video_thread
    while not should_terminate:
        time.sleep(30)
        if should_terminate:
            break
        with video_thread_lock:
            if current_bitrate == DEFAULT_BITRATE:
                try:
                    base = int("".join(filter(str.isdigit, DEFAULT_BITRATE)))
                    new_bitrate = "{}M".format(int(base * 0.6))
                except:
                    new_bitrate = DEFAULT_BITRATE
            else:
                new_bitrate = DEFAULT_BITRATE

            if new_bitrate != current_bitrate:
                logging.info("Adaptive ABR: Switching bitrate from {} to {}".format(current_bitrate, new_bitrate))
                new_cmd = build_video_cmd(args, new_bitrate)
                new_thread = StreamThread(new_cmd, "Video (Adaptive)")
                new_thread.start()
                time.sleep(3)
                if current_video_thread:
                    current_video_thread.stop()
                    current_video_thread.join()
                current_video_thread = new_thread
                current_bitrate = new_bitrate

def tcp_handshake_server(sock):
    logging.info("TCP Handshake server listening on port {}".format(TCP_HANDSHAKE_PORT))
    while not should_terminate:
        try:
            conn, addr = sock.accept()
            logging.info("TCP handshake connection from {}".format(addr))
            data = conn.recv(1024).decode("utf-8", errors="replace").strip()
            logging.info("Received handshake: '{}'".format(data))
            expected = "PASSWORD:{}".format(host_password) if host_password else "PASSWORD:"
            if data == expected:
                conn.sendall("OK".encode("utf-8"))
                logging.info("Handshake from {} successful.".format(addr))
            else:
                conn.sendall("FAIL".encode("utf-8"))
                logging.error("Handshake from {} failed. Expected '{}', got '{}'".format(addr, expected, data))
            conn.close()
        except OSError:
            break
        except Exception as e:
            logging.error("TCP handshake server error: {}".format(e))
            break

def control_listener(sock):
    logging.info("Control listener active on UDP port {}".format(UDP_CONTROL_PORT))
    while not should_terminate:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="replace").strip()

            if host_password:
                prefix = "PASSWORD:{}:".format(host_password)
                if not msg.startswith(prefix):
                    logging.warning("Rejected control message from {} due to password mismatch.".format(addr))
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
                logging.warning("Ignored unsupported control message: {}".format(msg))
        except OSError:
            break
        except Exception as e:
            logging.error("Control listener error: {}".format(e))
            break

def clipboard_monitor_host():
    global last_clipboard_content
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    while not should_terminate:
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
        with clipboard_lock:
            if not ignore_clipboard_update and current and current != last_clipboard_content:
                last_clipboard_content = current
                msg = "CLIPBOARD_UPDATE HOST {}".format(current)
                sock.sendto(msg.encode("utf-8"), (MULTICAST_IP, UDP_CLIPBOARD_PORT))
                logging.info("Host clipboard updated and broadcast.")
        time.sleep(1)
    sock.close()

def clipboard_listener_host(sock):
    global ignore_clipboard_update
    while not should_terminate:
        try:
            data, addr = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="replace")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "CLIENT":
                new_content = tokens[2]
                with clipboard_lock:
                    ignore_clipboard_update = True
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
                    ignore_clipboard_update = False
        except OSError:
            break
        except Exception as e:
            logging.error("Host clipboard listener error: {}".format(e))
            break

def main():
    global current_video_thread, current_audio_thread
    global current_bitrate, host_password
    global handshake_sock, control_sock, clipboard_listener_sock

    parser = argparse.ArgumentParser(description="Remote Desktop Host (Production Ready)")
    parser.add_argument("--encoder", choices=["none", "h.264", "h.265", "av1"], default="none")
    parser.add_argument("--resolution", default=DEFAULT_RES)
    parser.add_argument("--framerate", default=DEFAULT_FPS)
    parser.add_argument("--bitrate", default=DEFAULT_BITRATE)
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable")
    parser.add_argument("--adaptive", action="store_true")
    parser.add_argument("--password", default="")
    parser.add_argument("--display", default=":0")
    args = parser.parse_args()

    host_password = args.password if args.password else None
    current_bitrate = args.bitrate

    handshake_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    handshake_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        handshake_sock.bind(("", TCP_HANDSHAKE_PORT))
        handshake_sock.listen(5)
    except Exception as e:
        logging.error("Failed to bind TCP handshake port {}: {}".format(TCP_HANDSHAKE_PORT, e))
        stop_all()
        sys.exit(1)

    control_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    control_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        control_sock.bind(("", UDP_CONTROL_PORT))
    except Exception as e:
        logging.error("Failed to bind control port {}: {}".format(UDP_CONTROL_PORT, e))
        stop_all()
        sys.exit(1)

    clipboard_listener_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    clipboard_listener_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        clipboard_listener_sock.bind(("", UDP_CLIPBOARD_PORT))
    except Exception as e:
        logging.error("Failed to bind clipboard port {}: {}".format(UDP_CLIPBOARD_PORT, e))
        stop_all()
        sys.exit(1)

    handshake_thread = threading.Thread(target=tcp_handshake_server, args=(handshake_sock,), daemon=True)
    handshake_thread.start()

    clipboard_monitor_thread = threading.Thread(target=clipboard_monitor_host, daemon=True)
    clipboard_monitor_thread.start()

    clipboard_listener_thread = threading.Thread(target=clipboard_listener_host, args=(clipboard_listener_sock,), daemon=True)
    clipboard_listener_thread.start()

    video_cmd = build_video_cmd(args, current_bitrate)
    with video_thread_lock:
        current_video_thread = StreamThread(video_cmd, "Video")
        current_video_thread.start()

    if args.audio == "enable":
        audio_cmd = build_audio_cmd()
        current_audio_thread = StreamThread(audio_cmd, "Audio")
        current_audio_thread.start()

    if args.adaptive:
        abr_thread = threading.Thread(target=adaptive_bitrate_manager, args=(args,), daemon=True)
        abr_thread.start()

    ctrl_thread = threading.Thread(target=control_listener, args=(control_sock,), daemon=True)
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

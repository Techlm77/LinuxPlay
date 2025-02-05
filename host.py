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
current_bitrate = DEFAULT_BITRATE
host_password = None
last_clipboard_content = ""
clipboard_lock = threading.Lock()
ignore_clipboard_update = False

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S"
)

def get_display():
    return os.environ.get("DISPLAY", ":0")

def detect_pulse_monitor():
    monitor = os.environ.get("PULSE_MONITOR")
    if monitor:
        return monitor
    try:
        output = subprocess.check_output(["pactl", "list", "short", "sources"], universal_newlines=True)
        for line in output.splitlines():
            parts = line.split()
            if len(parts) >= 2 and ".monitor" in parts[1]:
                return parts[1]
    except Exception as e:
        logging.error(f"Error detecting PulseAudio monitor: {e}")
    return "default.monitor"

def build_video_cmd(args, bitrate):
    display = get_display()
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel", "error",
        "-threads", "0",
        "-f", "x11grab",
        "-framerate", args.framerate,
        "-video_size", args.resolution,
        "-i", display
    ]
    if args.encoder == "nvenc":
        encode = [
            "-c:v", "h264_nvenc",
            "-preset", "llhq",
            "-g", "30",
            "-bf", "0",
            "-b:v", bitrate,
            "-pix_fmt", "yuv420p"
        ]
    elif args.encoder == "vaapi":
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
    out = [
        "-f", "mpegts",
        f"udp://{MULTICAST_IP}:{UDP_VIDEO_PORT}?pkt_size=1316&buffer_size=65536"
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

class StreamThread(threading.Thread):
    def __init__(self, cmd, name):
        super().__init__(daemon=True)
        self.cmd = cmd
        self.name = name
        self.process = None
        self._running = True

    def run(self):
        logging.info(f"Starting {self.name} stream: {' '.join(self.cmd)}")
        self.process = subprocess.Popen(self.cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, universal_newlines=True)
        while self._running:
            ret = self.process.poll()
            if ret is not None:
                out, err = self.process.communicate()
                logging.error(f"{self.name} process ended unexpectedly. Return code: {ret}. Error output:\n{err}")
                break
            time.sleep(0.5)

    def stop(self):
        self._running = False
        if self.process:
            self.process.terminate()

def adaptive_bitrate_manager(args):
    global current_bitrate, current_video_thread
    while True:
        time.sleep(30)
        with video_thread_lock:
            if current_bitrate == DEFAULT_BITRATE:
                try:
                    base = int(''.join(filter(str.isdigit, DEFAULT_BITRATE)))
                    new_bitrate = f"{int(base * 0.6)}M"
                except Exception:
                    new_bitrate = DEFAULT_BITRATE
            else:
                new_bitrate = DEFAULT_BITRATE
            if new_bitrate != current_bitrate:
                logging.info(f"Adaptive ABR: Switching bitrate from {current_bitrate} to {new_bitrate}")
                new_cmd = build_video_cmd(args, new_bitrate)
                new_thread = StreamThread(new_cmd, "Video (Adaptive)")
                new_thread.start()
                time.sleep(3)
                if current_video_thread:
                    current_video_thread.stop()
                    current_video_thread.join()
                current_video_thread = new_thread
                current_bitrate = new_bitrate

def tcp_handshake_server():
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("", TCP_HANDSHAKE_PORT))
    sock.listen(5)
    logging.info(f"TCP Handshake server listening on port {TCP_HANDSHAKE_PORT}")
    while True:
        try:
            conn, addr = sock.accept()
            logging.info(f"TCP handshake connection from {addr}")
            data = conn.recv(1024).decode("utf-8", errors="replace").strip()
            logging.info(f"Received handshake: '{data}'")
            expected = f"PASSWORD:{host_password}" if host_password else "PASSWORD:"
            if data == expected:
                conn.sendall("OK".encode("utf-8"))
                logging.info(f"Handshake from {addr} successful.")
            else:
                conn.sendall("FAIL".encode("utf-8"))
                logging.error(f"Handshake from {addr} failed. Expected '{expected}', got '{data}'")
            conn.close()
        except Exception as e:
            logging.error(f"TCP handshake server error: {e}")

def control_listener():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("", UDP_CONTROL_PORT))
    logging.info(f"Control listener active on UDP port {UDP_CONTROL_PORT}")
    while True:
        try:
            data, addr = sock.recvfrom(2048)
            msg = data.decode("utf-8", errors="replace").strip()
            if host_password:
                prefix = f"PASSWORD:{host_password}:"
                if not msg.startswith(prefix):
                    logging.warning(f"Rejected control message from {addr} due to password mismatch.")
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
                logging.warning(f"Ignored unsupported control message: {msg}")
        except Exception as e:
            logging.error(f"Control listener error: {e}")

def clipboard_monitor_host():
    global last_clipboard_content
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    while True:
        try:
            proc = subprocess.run(["xclip", "-o", "-selection", "clipboard"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
            current = proc.stdout.strip()
        except Exception as e:
            logging.error(f"Error reading clipboard: {e}")
            current = ""
        with clipboard_lock:
            if not ignore_clipboard_update and current and current != last_clipboard_content:
                last_clipboard_content = current
                msg = f"CLIPBOARD_UPDATE HOST {current}"
                sock.sendto(msg.encode("utf-8"), (MULTICAST_IP, UDP_CLIPBOARD_PORT))
                logging.info("Host clipboard updated and broadcast.")
        time.sleep(1)

def clipboard_listener_host():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("", UDP_CLIPBOARD_PORT))
    while True:
        try:
            data, addr = sock.recvfrom(65535)
            msg = data.decode("utf-8", errors="replace")
            tokens = msg.split(maxsplit=2)
            if len(tokens) >= 3 and tokens[0] == "CLIPBOARD_UPDATE" and tokens[1] == "CLIENT":
                new_content = tokens[2]
                with clipboard_lock:
                    global ignore_clipboard_update
                    ignore_clipboard_update = True
                    proc = subprocess.run(["xclip", "-o", "-selection", "clipboard"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
                    current = proc.stdout.strip()
                    if new_content != current:
                        p = subprocess.Popen(["xclip", "-selection", "clipboard", "-in"], stdin=subprocess.PIPE)
                        p.communicate(new_content.encode("utf-8"))
                        logging.info("Host clipboard updated from client.")
                    ignore_clipboard_update = False
        except Exception as e:
            logging.error(f"Host clipboard listener error: {e}")

def cleanup():
    try:
        if current_video_thread:
            current_video_thread.stop()
            current_video_thread.join(timeout=2)
    except Exception:
        pass

atexit.register(cleanup)

def main():
    global current_video_thread, current_bitrate, host_password
    parser = argparse.ArgumentParser(description="Remote Desktop Host (Production Ready)")
    parser.add_argument("--encoder", choices=["nvenc", "vaapi", "none"], default="none",
                        help="Video encoder: nvenc, vaapi, or none (CPU x264).")
    parser.add_argument("--resolution", default=DEFAULT_RES,
                        help="Capture resolution, e.g., 1920x1080.")
    parser.add_argument("--framerate", default=DEFAULT_FPS,
                        help="Capture framerate, e.g., 30.")
    parser.add_argument("--bitrate", default=DEFAULT_BITRATE,
                        help="Initial video bitrate, e.g., 8M.")
    parser.add_argument("--audio", choices=["enable", "disable"], default="disable",
                        help="Enable or disable audio streaming.")
    parser.add_argument("--adaptive", action="store_true",
                        help="Enable adaptive bitrate switching.")
    parser.add_argument("--password", default="",
                        help="Optional password for control messages and handshake.")
    args = parser.parse_args()

    host_password = args.password if args.password else None
    current_bitrate = args.bitrate

    handshake_thread = threading.Thread(target=tcp_handshake_server, daemon=True)
    handshake_thread.start()

    clipboard_monitor_thread = threading.Thread(target=clipboard_monitor_host, daemon=True)
    clipboard_monitor_thread.start()
    clipboard_listener_thread = threading.Thread(target=clipboard_listener_host, daemon=True)
    clipboard_listener_thread.start()

    logging.info("Waiting for a successful TCP handshake...")
    handshake_success = False
    while not handshake_success:
        temp_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            temp_sock.connect(("localhost", TCP_HANDSHAKE_PORT))
            handshake_msg = f"PASSWORD:{host_password}" if host_password else "PASSWORD:"
            temp_sock.sendall(handshake_msg.encode("utf-8"))
            resp = temp_sock.recv(1024).decode("utf-8", errors="replace").strip()
            temp_sock.close()
            if resp == "OK":
                handshake_success = True
                logging.info("A successful TCP handshake was received.")
            else:
                logging.info("Received FAIL from handshake; waiting for a correct handshake...")
                time.sleep(3)
        except Exception:
            time.sleep(3)

    video_cmd = build_video_cmd(args, current_bitrate)
    current_video_thread = StreamThread(video_cmd, "Video")
    current_video_thread.start()

    audio_thread = None
    if args.audio == "enable":
        audio_cmd = build_audio_cmd()
        audio_thread = StreamThread(audio_cmd, "Audio")
        audio_thread.start()

    if args.adaptive:
        abr_thread = threading.Thread(target=adaptive_bitrate_manager, args=(args,), daemon=True)
        abr_thread.start()

    ctrl_thread = threading.Thread(target=control_listener, daemon=True)
    ctrl_thread.start()

    logging.info("Host running. Press Ctrl+C to exit.")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        logging.info("Shutting down host...")
        if current_video_thread:
            current_video_thread.stop()
            current_video_thread.join()
        if audio_thread:
            audio_thread.stop()
            audio_thread.join()
        sys.exit(0)

if __name__ == "__main__":
    main()

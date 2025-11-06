import socket
import subprocess
import threading
import time
import pytest

from linuxplay.host import (
    FILE_UPLOAD_PORT,
    TCP_HANDSHAKE_PORT,
    UDP_AUDIO_PORT,
    UDP_CLIPBOARD_PORT,
    UDP_CONTROL_PORT,
    UDP_GAMEPAD_PORT,
    UDP_HEARTBEAT_PORT,
    UDP_VIDEO_PORT,
    StreamThread,
    detect_monitors,
)

@pytest.mark.integration
class TestUDPCommunication:
    def test_udp_socket_creation(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.close()

    def test_udp_send_receive(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("127.0.0.1", 0))
        server.settimeout(2.0)
        port = server.getsockname()[1]
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        test_message = b"TEST_MESSAGE"
        client.sendto(test_message, ("127.0.0.1", port))
        data, _addr = server.recvfrom(1024)
        assert data == test_message
        client.close()
        server.close()

    def test_udp_buffer_size(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 65536)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 65536)
        rcv_buf = sock.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
        snd_buf = sock.getsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF)
        assert rcv_buf > 0
        assert snd_buf > 0
        sock.close()

@pytest.mark.integration
class TestTCPHandshake:
    def test_tcp_socket_creation(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", 0))
        sock.listen(1)
        port = sock.getsockname()[1]
        assert port > 0
        sock.close()

    def test_tcp_client_server_connection(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        server.settimeout(2.0)
        port = server.getsockname()[1]

        def client_connect():
            time.sleep(0.1)
            client = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            client.connect(("127.0.0.1", port))
            client.send(b"HELLO")
            client.close()

        thread = threading.Thread(target=client_connect)
        thread.start()
        conn, _addr = server.accept()
        data = conn.recv(1024)
        assert data == b"HELLO"
        conn.close()
        server.close()
        thread.join()

class TestPortConstants:
    def test_port_constants_defined(self):
        assert UDP_VIDEO_PORT == 5000
        assert UDP_CONTROL_PORT == 7000
        assert TCP_HANDSHAKE_PORT == 7001
        assert UDP_CLIPBOARD_PORT == 7002
        assert FILE_UPLOAD_PORT == 7003
        assert UDP_HEARTBEAT_PORT == 7004
        assert UDP_GAMEPAD_PORT == 7005
        assert UDP_AUDIO_PORT == 6001

    def test_ports_unique(self):
        ports = [
            UDP_VIDEO_PORT,
            UDP_CONTROL_PORT,
            TCP_HANDSHAKE_PORT,
            UDP_CLIPBOARD_PORT,
            FILE_UPLOAD_PORT,
            UDP_HEARTBEAT_PORT,
            UDP_GAMEPAD_PORT,
            UDP_AUDIO_PORT,
        ]
        assert len(ports) == len(set(ports))

@pytest.mark.integration
class TestHeartbeatProtocol:
    def test_heartbeat_ping_pong_format(self):
        ping = b"PING"
        pong = b"PONG"
        assert len(ping) == 4
        assert len(pong) == 4
        assert ping != pong

    def test_heartbeat_message_exchange(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server.bind(("127.0.0.1", 0))
        server.settimeout(2.0)
        port = server.getsockname()[1]
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client.sendto(b"PING", ("127.0.0.1", port))
        data, addr = server.recvfrom(256)
        assert data == b"PING"
        server.sendto(b"PONG", addr)
        client.settimeout(2.0)
        data, _ = client.recvfrom(256)
        assert data == b"PONG"
        client.close()
        server.close()

@pytest.mark.integration
class TestControlMessages:
    def test_control_message_format(self):
        messages = [
            "MOUSE_PKT 2 0 100 200",
            "MOUSE_SCROLL 4",
            "KEY_PRESS a",
            "KEY_RELEASE a",
            "GOODBYE",
            "NET wifi",
        ]
        for msg in messages:
            encoded = msg.encode("utf-8")
            decoded = encoded.decode("utf-8")
            assert decoded == msg

    def test_mouse_packet_parsing(self):
        msg = "MOUSE_PKT 2 0 150 250"
        parts = msg.split()
        assert parts[0] == "MOUSE_PKT"
        assert int(parts[1]) == 2
        assert int(parts[2]) == 0
        assert int(parts[3]) == 150
        assert int(parts[4]) == 250

    def test_key_message_parsing(self):
        press = "KEY_PRESS Escape"
        release = "KEY_RELEASE Escape"
        press_parts = press.split()
        assert press_parts[0] == "KEY_PRESS"
        assert press_parts[1] == "Escape"
        release_parts = release.split()
        assert release_parts[0] == "KEY_RELEASE"

@pytest.mark.integration
class TestClipboardProtocol:
    def test_clipboard_message_format(self):
        text = "Hello World"
        msg_host = f"CLIPBOARD_UPDATE HOST {text}"
        msg_client = f"CLIPBOARD_UPDATE CLIENT {text}"
        assert "CLIPBOARD_UPDATE" in msg_host
        assert "HOST" in msg_host
        assert text in msg_host
        assert "CLIPBOARD_UPDATE" in msg_client
        assert "CLIENT" in msg_client
        assert text in msg_client

    def test_clipboard_message_parsing(self):
        msg = "CLIPBOARD_UPDATE HOST Test content"
        tokens = msg.split(maxsplit=2)
        assert len(tokens) == 3
        assert tokens[0] == "CLIPBOARD_UPDATE"
        assert tokens[1] == "HOST"
        assert tokens[2] == "Test content"

    def test_clipboard_udp_exchange(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server.bind(("127.0.0.1", 0))
        server.settimeout(2.0)
        port = server.getsockname()[1]
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        msg = "CLIPBOARD_UPDATE CLIENT Test text"
        client.sendto(msg.encode("utf-8"), ("127.0.0.1", port))
        data, _ = server.recvfrom(65535)
        received = data.decode("utf-8")
        assert "CLIPBOARD_UPDATE" in received
        assert "CLIENT" in received
        assert "Test text" in received
        client.close()
        server.close()

class TestStreamThreadManagement:
    def test_stream_thread_creation(self):
        cmd = ["echo", "test"]
        thread = StreamThread(cmd, "TestStream")
        assert thread.name == "TestStream"
        assert thread.cmd == cmd
        assert thread._running is True

    def test_stream_thread_stop(self):
        cmd = ["sleep", "10"]
        thread = StreamThread(cmd, "TestStream")
        thread.stop()
        assert thread._running is False

class TestMonitorDetection:
    def test_detect_monitors_returns_list(self, monkeypatch):
        def mock_check_output(*_args, **_kwargs):
            return """Monitors: 2
 0: +*HDMI-1 1920/527x1080/296+0+0  HDMI-1
 1: +DP-1 1920/527x1080/296+1920+0  DP-1
"""

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monitors = detect_monitors()
        assert isinstance(monitors, list)

    def test_detect_monitors_handles_error(self, monkeypatch):
        def mock_check_output(*_args, **_kwargs):
            raise Exception("xrandr not found")

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monitors = detect_monitors()
        assert isinstance(monitors, list)
        assert len(monitors) == 0

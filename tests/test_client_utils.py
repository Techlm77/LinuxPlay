"""Unit tests for client.py utility functions."""

import pytest


class TestNetworkModeDetection:
    """Tests for network mode detection."""

    def test_detect_network_mode_wifi_linux(self, mock_linux_platform, monkeypatch):
        """Test WiFi detection on Linux."""
        import subprocess
        from pathlib import Path

        from linuxplay.client import detect_network_mode

        def mock_check_output(cmd, **kwargs):
            if cmd[0] == "ip":
                return "dev wlp3s0 src 192.168.1.100"
            return ""

        def mock_path_exists(self):
            # /sys/class/net/wlp3s0/wireless exists for WiFi
            return "wireless" in str(self)

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monkeypatch.setattr(Path, "exists", mock_path_exists)

        mode = detect_network_mode("192.168.1.1")
        assert mode == "wifi"

    def test_detect_network_mode_lan_linux(self, mock_linux_platform, monkeypatch):
        """Test LAN detection on Linux."""
        import subprocess
        from pathlib import Path

        from linuxplay.client import detect_network_mode

        def mock_check_output(cmd, **kwargs):
            if cmd[0] == "ip":
                return "dev eth0 src 192.168.1.100"
            return ""

        def mock_path_exists(self):
            return False  # No wireless directory

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monkeypatch.setattr(Path, "exists", mock_path_exists)

        mode = detect_network_mode("192.168.1.1")
        assert mode == "lan"

    def test_detect_network_mode_fallback(self, monkeypatch):
        """Test fallback to LAN on detection failure."""
        import subprocess

        from linuxplay.client import detect_network_mode

        def mock_check_output(*args, **kwargs):
            raise Exception("Command failed")

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)

        mode = detect_network_mode("192.168.1.1")
        assert mode == "lan"  # Should default to LAN on error


class TestHardwareAccelSelection:
    """Tests for hardware acceleration selection."""

    def test_choose_auto_hwaccel_windows(self, mock_windows_platform, monkeypatch):
        """Test auto hardware accel selection on Windows."""
        from linuxplay.client import choose_auto_hwaccel, ffmpeg_hwaccels

        def mock_hwaccels():
            return {"d3d11va", "cuda", "dxva2"}

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)

        result = choose_auto_hwaccel()
        assert result in ["d3d11va", "cuda", "dxva2", "qsv"]

    def test_choose_auto_hwaccel_linux(self, mock_linux_platform, monkeypatch):
        """Test auto hardware accel selection on Linux."""
        from linuxplay.client import choose_auto_hwaccel, ffmpeg_hwaccels

        def mock_hwaccels():
            return {"vaapi", "cuda"}

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)

        result = choose_auto_hwaccel()
        assert result in ["vaapi", "cuda", "qsv"]

    def test_choose_auto_hwaccel_cpu_fallback(self, monkeypatch):
        """Test CPU fallback when no hardware accel available."""
        from linuxplay.client import choose_auto_hwaccel, ffmpeg_hwaccels

        def mock_hwaccels():
            return set()  # No hardware acceleration

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)

        result = choose_auto_hwaccel()
        assert result == "cpu"


class TestMPEGTSPacketSize:
    """Tests for MPEG-TS packet size calculation in client."""

    def test_best_ts_pkt_size_ipv4(self):
        """Test packet size for IPv4."""
        from linuxplay.client import _best_ts_pkt_size

        result = _best_ts_pkt_size(1500, False)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_ipv6(self):
        """Test packet size for IPv6."""
        from linuxplay.client import _best_ts_pkt_size

        result = _best_ts_pkt_size(1500, True)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_minimum(self):
        """Test minimum packet size handling."""
        from linuxplay.client import _best_ts_pkt_size

        result = _best_ts_pkt_size(400, False)
        assert result >= 188
        assert result % 188 == 0


class TestClientStateManagement:
    """Tests for client state management."""

    def test_client_state_initial(self):
        """Test initial client state."""
        from linuxplay.client import CLIENT_STATE

        assert CLIENT_STATE["connected"] is False
        assert CLIENT_STATE["last_heartbeat"] >= 0
        assert CLIENT_STATE["net_mode"] in ["lan", "wifi"]
        assert CLIENT_STATE["reconnecting"] is False

    def test_client_state_update(self):
        """Test updating client state."""
        from linuxplay.client import CLIENT_STATE

        # Update state
        CLIENT_STATE["connected"] = True
        CLIENT_STATE["net_mode"] = "wifi"

        assert CLIENT_STATE["connected"] is True
        assert CLIENT_STATE["net_mode"] == "wifi"

        # Reset for other tests
        CLIENT_STATE["connected"] = False
        CLIENT_STATE["net_mode"] = "lan"


class TestRendererSelection:
    """Tests for renderer backend selection."""

    def test_pick_best_renderer_returns_valid(self, monkeypatch):
        """Test that pick_best_renderer returns a valid renderer."""
        from linuxplay.client import pick_best_renderer

        renderer = pick_best_renderer()
        assert renderer is not None
        assert hasattr(renderer, "render_frame")
        assert hasattr(renderer, "is_valid")
        assert hasattr(renderer, "name")

    def test_renderer_has_name(self, monkeypatch):
        """Test that renderer has a name."""
        from linuxplay.client import pick_best_renderer

        renderer = pick_best_renderer()
        name = renderer.name()
        assert isinstance(name, str)
        assert len(name) > 0


class TestKeyMapping:
    """Tests for key name mapping."""

class TestHardwareCapabilities:
    """Tests for hardware capability probing."""

    def test_probe_hardware_capabilities_no_error(self, monkeypatch):
        """Test hardware probing doesn't raise errors."""
        from pathlib import Path

        from linuxplay.client import _probe_hardware_capabilities

        def mock_path_exists(self):
            return False

        monkeypatch.setattr(Path, "exists", mock_path_exists)

        # Should not raise an exception
        _probe_hardware_capabilities()

    def test_ffmpeg_hwaccels_returns_set(self, monkeypatch):
        """Test ffmpeg_hwaccels returns a set."""
        import subprocess

        from linuxplay.client import ffmpeg_hwaccels

        def mock_check_output(*args, **kwargs):
            return "Hardware acceleration methods:\ncuda\nvaapi\n"

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)

        result = ffmpeg_hwaccels()
        assert isinstance(result, set)
        assert "cuda" in result
        assert "vaapi" in result

    def test_ffmpeg_hwaccels_handles_error(self, monkeypatch):
        """Test ffmpeg_hwaccels handles errors gracefully."""
        import subprocess

        from linuxplay.client import ffmpeg_hwaccels

        def mock_check_output(*args, **kwargs):
            raise Exception("Command failed")

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)

        result = ffmpeg_hwaccels()
        assert isinstance(result, set)
        assert len(result) == 0  # Empty set on error

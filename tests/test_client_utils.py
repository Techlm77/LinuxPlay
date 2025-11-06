import subprocess
from pathlib import Path
from linuxplay.client import (
    CLIENT_STATE,
    _best_ts_pkt_size,
    _probe_hardware_capabilities,
    choose_auto_hwaccel,
    detect_network_mode,
    ffmpeg_hwaccels,
    pick_best_renderer,
)

class TestNetworkModeDetection:
    def test_detect_network_mode_wifi_linux(self, monkeypatch):
        def mock_check_output(cmd, **_kwargs):
            if cmd[0] == "ip":
                return "dev wlp3s0 src 192.168.1.100"
            return ""

        def mock_path_exists(_self):
            return "wireless" in str(_self)

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monkeypatch.setattr(Path, "exists", mock_path_exists)
        mode = detect_network_mode("192.168.1.1")
        assert mode == "wifi"

    def test_detect_network_mode_lan_linux(self, monkeypatch):
        def mock_check_output(cmd, **_kwargs):
            if cmd[0] == "ip":
                return "dev eth0 src 192.168.1.100"
            return ""

        def mock_path_exists(_self):
            return False

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        monkeypatch.setattr(Path, "exists", mock_path_exists)
        mode = detect_network_mode("192.168.1.1")
        assert mode == "lan"

    def test_detect_network_mode_fallback(self, monkeypatch):
        def mock_check_output(*_args, **_kwargs):
            raise Exception("Command failed")

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        mode = detect_network_mode("192.168.1.1")
        assert mode == "lan"

class TestHardwareAccelSelection:
    def test_choose_auto_hwaccel_windows(self, monkeypatch):
        def mock_hwaccels():
            return {"d3d11va", "cuda", "dxva2"}

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)
        result = choose_auto_hwaccel()
        assert result in ["d3d11va", "cuda", "dxva2", "qsv"]

    def test_choose_auto_hwaccel_linux(self, monkeypatch):
        def mock_hwaccels():
            return {"vaapi", "cuda"}

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)
        result = choose_auto_hwaccel()
        assert result in ["vaapi", "cuda", "qsv"]

    def test_choose_auto_hwaccel_cpu_fallback(self, monkeypatch):
        def mock_hwaccels():
            return set()

        monkeypatch.setattr("linuxplay.client.ffmpeg_hwaccels", mock_hwaccels)
        result = choose_auto_hwaccel()
        assert result == "cpu"

class TestMPEGTSPacketSize:
    def test_best_ts_pkt_size_ipv4(self):
        result = _best_ts_pkt_size(1500, False)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_ipv6(self):
        result = _best_ts_pkt_size(1500, True)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_minimum(self):
        result = _best_ts_pkt_size(400, False)
        assert result >= 188
        assert result % 188 == 0

class TestClientStateManagement:
    def test_client_state_initial(self):
        assert CLIENT_STATE["connected"] is False
        assert CLIENT_STATE["last_heartbeat"] >= 0
        assert CLIENT_STATE["net_mode"] in ["lan", "wifi"]
        assert CLIENT_STATE["reconnecting"] is False

    def test_client_state_update(self):
        CLIENT_STATE["connected"] = True
        CLIENT_STATE["net_mode"] = "wifi"
        assert CLIENT_STATE["connected"] is True
        assert CLIENT_STATE["net_mode"] == "wifi"
        CLIENT_STATE["connected"] = False
        CLIENT_STATE["net_mode"] = "lan"

class TestRendererSelection:
    def test_pick_best_renderer_returns_valid(self):
        renderer = pick_best_renderer()
        assert renderer is not None
        assert hasattr(renderer, "render_frame")
        assert hasattr(renderer, "is_valid")
        assert hasattr(renderer, "name")

    def test_renderer_has_name(self):
        renderer = pick_best_renderer()
        name = renderer.name()
        assert isinstance(name, str)
        assert len(name) > 0

class TestKeyMapping:
    pass

class TestHardwareCapabilities:
    def test_probe_hardware_capabilities_no_error(self, monkeypatch):
        def mock_path_exists(_self):
            return False

        monkeypatch.setattr(Path, "exists", mock_path_exists)
        _probe_hardware_capabilities()

    def test_ffmpeg_hwaccels_returns_set(self, monkeypatch):
        def mock_check_output(*_args, **_kwargs):
            return "Hardware acceleration methods:\ncuda\nvaapi\n"

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        result = ffmpeg_hwaccels()
        assert isinstance(result, set)
        assert "cuda" in result
        assert "vaapi" in result

    def test_ffmpeg_hwaccels_handles_error(self, monkeypatch):
        def mock_check_output(*_args, **_kwargs):
            raise Exception("Command failed")

        monkeypatch.setattr(subprocess, "check_output", mock_check_output)
        result = ffmpeg_hwaccels()
        assert isinstance(result, set)
        assert len(result) == 0

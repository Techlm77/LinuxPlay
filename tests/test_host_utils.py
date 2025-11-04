"""Unit tests for host.py utility functions."""

import pytest


class TestBitrateUtils:
    """Tests for bitrate parsing and formatting."""

    def test_parse_bitrate_with_k_suffix(self):
        """Test parsing bitrates with 'k' suffix."""
        from linuxplay.host import _parse_bitrate_bits

        assert _parse_bitrate_bits("100k") == 100_000
        assert _parse_bitrate_bits("500K") == 500_000
        assert _parse_bitrate_bits("1.5k") == 1_500

    def test_parse_bitrate_with_m_suffix(self):
        """Test parsing bitrates with 'M' suffix."""
        from linuxplay.host import _parse_bitrate_bits

        assert _parse_bitrate_bits("1M") == 1_000_000
        assert _parse_bitrate_bits("10m") == 10_000_000
        assert _parse_bitrate_bits("2.5M") == 2_500_000

    def test_parse_bitrate_with_g_suffix(self):
        """Test parsing bitrates with 'G' suffix."""
        from linuxplay.host import _parse_bitrate_bits

        assert _parse_bitrate_bits("1G") == 1_000_000_000
        assert _parse_bitrate_bits("0.5g") == 500_000_000

    def test_parse_bitrate_plain_number(self):
        """Test parsing plain number bitrates."""
        from linuxplay.host import _parse_bitrate_bits

        assert _parse_bitrate_bits("1000") == 1000
        assert _parse_bitrate_bits("500000") == 500_000

    def test_parse_bitrate_invalid(self):
        """Test parsing invalid bitrates returns 0."""
        from linuxplay.host import _parse_bitrate_bits

        assert _parse_bitrate_bits("") == 0
        assert _parse_bitrate_bits("invalid") == 0
        assert _parse_bitrate_bits(None) == 0

    def test_format_bits_megabits(self):
        """Test formatting bits as megabits."""
        from linuxplay.host import _format_bits

        assert _format_bits(1_000_000) == "1M"
        assert _format_bits(5_000_000) == "5M"
        assert _format_bits(10_500_000) == "10M"

    def test_format_bits_kilobits(self):
        """Test formatting bits as kilobits."""
        from linuxplay.host import _format_bits

        assert _format_bits(1000) == "1k"
        assert _format_bits(500_000) == "500k"

    def test_format_bits_minimum(self):
        """Test formatting ensures minimum of 1."""
        from linuxplay.host import _format_bits

        assert _format_bits(0) == "1"
        assert _format_bits(500) == "1"


class TestPINGeneration:
    """Tests for PIN generation and validation."""

    def test_gen_pin_default_length(self):
        """Test PIN generation with default length."""
        from linuxplay.host import _gen_pin

        pin = _gen_pin(6)
        assert len(pin) == 6
        assert pin.isdigit()

    def test_gen_pin_custom_length(self):
        """Test PIN generation with custom length."""
        from linuxplay.host import _gen_pin

        pin = _gen_pin(4)
        assert len(pin) == 4
        assert pin.isdigit()

    def test_gen_pin_uniqueness(self):
        """Test that generated PINs are different."""
        from linuxplay.host import _gen_pin

        pins = [_gen_pin(6) for _ in range(10)]
        # Most PINs should be unique (statistically)
        assert len(set(pins)) >= 8


class TestNVENCUtils:
    """Tests for NVENC utility functions."""

    def test_safe_nvenc_preset_valid(self):
        """Test valid NVENC preset mapping."""
        from linuxplay.host import _safe_nvenc_preset

        assert _safe_nvenc_preset("fast") == "fast"
        assert _safe_nvenc_preset("p4") == "p4"
        assert _safe_nvenc_preset("llhp") == "llhp"

    def test_safe_nvenc_preset_aliases(self):
        """Test NVENC preset aliases."""
        from linuxplay.host import _safe_nvenc_preset

        assert _safe_nvenc_preset("ultrafast") == "p1"
        assert _safe_nvenc_preset("veryfast") == "p3"
        assert _safe_nvenc_preset("medium") == "p5"
        assert _safe_nvenc_preset("slow") == "p6"

    def test_safe_nvenc_preset_latency_aliases(self):
        """Test NVENC low-latency preset aliases."""
        from linuxplay.host import _safe_nvenc_preset

        assert _safe_nvenc_preset("ll") == "ll"
        assert _safe_nvenc_preset("low-latency") == "ll"
        assert _safe_nvenc_preset("ull") == "llhp"
        assert _safe_nvenc_preset("zerolatency") == "llhp"

    def test_safe_nvenc_preset_invalid_fallback(self):
        """Test invalid NVENC preset falls back to p4."""
        from linuxplay.host import _safe_nvenc_preset

        assert _safe_nvenc_preset("invalid") == "p4"
        assert _safe_nvenc_preset("") == "p4"

    def test_map_nvenc_tune_valid(self):
        """Test valid NVENC tune mapping."""
        from linuxplay.host import _map_nvenc_tune

        assert _map_nvenc_tune("ull") == "ull"
        assert _map_nvenc_tune("ll") == "ll"
        assert _map_nvenc_tune("hq") == "hq"

    def test_map_nvenc_tune_aliases(self):
        """Test NVENC tune aliases."""
        from linuxplay.host import _map_nvenc_tune

        assert _map_nvenc_tune("ultra-low-latency") == "ull"
        assert _map_nvenc_tune("zerolatency") == "ull"
        assert _map_nvenc_tune("low-latency") == "ll"
        assert _map_nvenc_tune("high-quality") == "hq"


class TestQPNormalization:
    """Tests for QP value normalization."""

    def test_norm_qp_valid_range(self):
        """Test QP normalization for valid range."""
        from linuxplay.host import _norm_qp

        assert _norm_qp("23") == "23"
        assert _norm_qp("0") == "0"
        assert _norm_qp("51") == "51"

    def test_norm_qp_clamp_low(self):
        """Test QP clamping for values below 0."""
        from linuxplay.host import _norm_qp

        assert _norm_qp("-5") == "0"
        assert _norm_qp("-100") == "0"

    def test_norm_qp_clamp_high(self):
        """Test QP clamping for values above 51."""
        from linuxplay.host import _norm_qp

        assert _norm_qp("60") == "51"
        assert _norm_qp("100") == "51"

    def test_norm_qp_invalid(self):
        """Test QP normalization with invalid input."""
        from linuxplay.host import _norm_qp

        assert _norm_qp("invalid") == ""
        assert _norm_qp("") == ""
        assert _norm_qp(None) == ""


class TestMPEGTSPacketSize:
    """Tests for MPEG-TS packet size calculation."""

    def test_best_ts_pkt_size_ipv4_default_mtu(self):
        """Test packet size calculation with IPv4 and default MTU."""
        from linuxplay.host import _best_ts_pkt_size

        # Default MTU 1500, IPv4 overhead 28 bytes = 1472 payload
        # 1472 / 188 = 7.82... -> 7 * 188 = 1316
        result = _best_ts_pkt_size(1500, False)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_ipv6_default_mtu(self):
        """Test packet size calculation with IPv6 and default MTU."""
        from linuxplay.host import _best_ts_pkt_size

        # Default MTU 1500, IPv6 overhead 48 bytes = 1452 payload
        # 1452 / 188 = 7.72... -> 7 * 188 = 1316
        result = _best_ts_pkt_size(1500, True)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_jumbo_frame(self):
        """Test packet size calculation with jumbo frames."""
        from linuxplay.host import _best_ts_pkt_size

        # Jumbo frame MTU 9000, IPv4 overhead 28 = 8972 payload
        # 8972 / 188 = 47.7... -> 47 * 188 = 8836
        result = _best_ts_pkt_size(9000, False)
        assert result == 8836
        assert result % 188 == 0

    def test_best_ts_pkt_size_minimum(self):
        """Test packet size calculation with small MTU."""
        from linuxplay.host import _best_ts_pkt_size

        # Even with MTU 500, should return at least 188
        result = _best_ts_pkt_size(500, False)
        assert result >= 188
        assert result % 188 == 0


class TestVAAPIFormat:
    """Tests for VAAPI pixel format selection."""

    def test_vaapi_fmt_valid_formats(self):
        """Test valid VAAPI pixel formats."""
        from linuxplay.host import _vaapi_fmt_for_pix_fmt

        assert _vaapi_fmt_for_pix_fmt("nv12", "h264") == "nv12"
        assert _vaapi_fmt_for_pix_fmt("yuv420p", "h264") == "yuv420p"
        assert _vaapi_fmt_for_pix_fmt("p010", "hevc") == "p010"

    def test_vaapi_fmt_aliases(self):
        """Test VAAPI format aliases."""
        from linuxplay.host import _vaapi_fmt_for_pix_fmt

        assert _vaapi_fmt_for_pix_fmt("yuv420", "h264") == "yuv420p"
        assert _vaapi_fmt_for_pix_fmt("420p", "h264") == "yuv420p"

    def test_vaapi_fmt_invalid_fallback(self):
        """Test invalid formats fall back to nv12."""
        from linuxplay.host import _vaapi_fmt_for_pix_fmt

        assert _vaapi_fmt_for_pix_fmt("invalid", "h264") == "nv12"
        assert _vaapi_fmt_for_pix_fmt("", "h264") == "nv12"


class TestTargetBPP:
    """Tests for target bits-per-pixel calculation."""

    def test_target_bpp_h264_standard(self):
        """Test BPP for H.264 at standard framerates."""
        from linuxplay.host import _target_bpp

        bpp_30fps = _target_bpp("h.264", 30)
        bpp_60fps = _target_bpp("h.264", 60)

        assert 0.05 <= bpp_30fps <= 0.10
        assert bpp_60fps > bpp_30fps  # Higher FPS needs more BPP

    def test_target_bpp_h265_standard(self):
        """Test BPP for H.265 (more efficient)."""
        from linuxplay.host import _target_bpp

        bpp_h264 = _target_bpp("h.264", 60)
        bpp_h265 = _target_bpp("h.265", 60)

        assert bpp_h265 < bpp_h264  # H.265 should need less BPP

    def test_target_bpp_high_framerate(self):
        """Test BPP increases for high framerates."""
        from linuxplay.host import _target_bpp

        bpp_90fps = _target_bpp("h.264", 90)
        bpp_120fps = _target_bpp("h.264", 120)

        assert bpp_90fps >= 0.07  # Should add bonus for high FPS
        assert bpp_120fps >= 0.07


class TestMarkerValue:
    """Tests for FFmpeg marker generation."""

    def test_marker_value_default(self):
        """Test marker value with default settings."""
        from linuxplay.host import _marker_value

        marker = _marker_value()
        assert "LinuxPlayHost" in marker

    def test_marker_value_with_sid(self, monkeypatch):
        """Test marker value with session ID."""
        from linuxplay.host import _marker_value

        monkeypatch.setenv("LINUXPLAY_SID", "test-session-123")
        marker = _marker_value()
        assert "LinuxPlayHost:test-session-123" == marker

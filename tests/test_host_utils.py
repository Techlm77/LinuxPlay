from linuxplay.host import (
    _best_ts_pkt_size,
    _format_bits,
    _gen_pin,
    _map_nvenc_tune,
    _marker_value,
    _norm_qp,
    _parse_bitrate_bits,
    _safe_nvenc_preset,
    _target_bpp,
    _vaapi_fmt_for_pix_fmt,
)

class TestBitrateUtils:
    def test_parse_bitrate_with_k_suffix(self):
        assert _parse_bitrate_bits("100k") == 100_000
        assert _parse_bitrate_bits("500K") == 500_000
        assert _parse_bitrate_bits("1.5k") == 1_500

    def test_parse_bitrate_with_m_suffix(self):
        assert _parse_bitrate_bits("1M") == 1_000_000
        assert _parse_bitrate_bits("10m") == 10_000_000
        assert _parse_bitrate_bits("2.5M") == 2_500_000

    def test_parse_bitrate_with_g_suffix(self):
        assert _parse_bitrate_bits("1G") == 1_000_000_000
        assert _parse_bitrate_bits("0.5g") == 500_000_000

    def test_parse_bitrate_plain_number(self):
        assert _parse_bitrate_bits("1000") == 1000
        assert _parse_bitrate_bits("500000") == 500_000

    def test_parse_bitrate_invalid(self):
        assert _parse_bitrate_bits("") == 0
        assert _parse_bitrate_bits("invalid") == 0
        assert _parse_bitrate_bits(None) == 0

    def test_format_bits_megabits(self):
        assert _format_bits(1_000_000) == "1M"
        assert _format_bits(5_000_000) == "5M"
        assert _format_bits(10_500_000) == "10M"

    def test_format_bits_kilobits(self):
        assert _format_bits(1000) == "1k"
        assert _format_bits(500_000) == "500k"

    def test_format_bits_minimum(self):
        assert _format_bits(0) == "1"
        assert _format_bits(500) == "500"

class TestPINGeneration:
    def test_gen_pin_default_length(self):
        pin = _gen_pin(6)
        assert len(pin) == 6
        assert pin.isdigit()

    def test_gen_pin_custom_length(self):
        pin = _gen_pin(4)
        assert len(pin) == 4
        assert pin.isdigit()

    def test_gen_pin_uniqueness(self):
        pins = [_gen_pin(6) for _ in range(10)]
        assert len(set(pins)) >= 8

class TestNVENCUtils:
    def test_safe_nvenc_preset_valid(self):
        assert _safe_nvenc_preset("p4") == "p4"
        assert _safe_nvenc_preset("p1") == "p1"
        assert _safe_nvenc_preset("llhp") == "llhp"

    def test_safe_nvenc_preset_aliases(self):
        assert _safe_nvenc_preset("ultrafast") == "p1"
        assert _safe_nvenc_preset("veryfast") == "p3"
        assert _safe_nvenc_preset("medium") == "p5"
        assert _safe_nvenc_preset("slow") == "p6"

    def test_safe_nvenc_preset_latency_aliases(self):
        assert _safe_nvenc_preset("ll") == "ll"
        assert _safe_nvenc_preset("low-latency") == "ll"
        assert _safe_nvenc_preset("ull") == "llhp"
        assert _safe_nvenc_preset("zerolatency") == "llhp"

    def test_safe_nvenc_preset_invalid_fallback(self):
        assert _safe_nvenc_preset("invalid") == "p4"
        assert _safe_nvenc_preset("") == "p4"

    def test_map_nvenc_tune_valid(self):
        assert _map_nvenc_tune("ull") == "ull"
        assert _map_nvenc_tune("ll") == "ll"
        assert _map_nvenc_tune("hq") == "hq"

    def test_map_nvenc_tune_aliases(self):
        assert _map_nvenc_tune("ultra-low-latency") == "ull"
        assert _map_nvenc_tune("zerolatency") == "ull"
        assert _map_nvenc_tune("low-latency") == "ll"
        assert _map_nvenc_tune("high-quality") == "hq"

class TestQPNormalization:
    def test_norm_qp_valid_range(self):
        assert _norm_qp("23") == "23"
        assert _norm_qp("0") == "0"
        assert _norm_qp("51") == "51"

    def test_norm_qp_clamp_low(self):
        assert _norm_qp("-5") == "0"
        assert _norm_qp("-100") == "0"

    def test_norm_qp_clamp_high(self):
        assert _norm_qp("60") == "51"
        assert _norm_qp("100") == "51"

    def test_norm_qp_invalid(self):
        assert _norm_qp("invalid") == ""
        assert _norm_qp("") == ""
        assert _norm_qp(None) == ""

class TestMPEGTSPacketSize:
    def test_best_ts_pkt_size_ipv4_default_mtu(self):
        result = _best_ts_pkt_size(1500, False)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_ipv6_default_mtu(self):
        result = _best_ts_pkt_size(1500, True)
        assert result == 1316
        assert result % 188 == 0

    def test_best_ts_pkt_size_jumbo_frame(self):
        result = _best_ts_pkt_size(9000, False)
        assert result == 8836
        assert result % 188 == 0

    def test_best_ts_pkt_size_minimum(self):
        result = _best_ts_pkt_size(500, False)
        assert result >= 188
        assert result % 188 == 0

class TestVAAPIFormat:
    def test_vaapi_fmt_valid_formats(self):
        assert _vaapi_fmt_for_pix_fmt("nv12", "h264") == "nv12"
        assert _vaapi_fmt_for_pix_fmt("yuv420p", "h264") == "yuv420p"
        assert _vaapi_fmt_for_pix_fmt("p010", "hevc") == "p010"

    def test_vaapi_fmt_aliases(self):
        assert _vaapi_fmt_for_pix_fmt("yuv420", "h264") == "yuv420p"
        assert _vaapi_fmt_for_pix_fmt("420p", "h264") == "yuv420p"

    def test_vaapi_fmt_invalid_fallback(self):
        assert _vaapi_fmt_for_pix_fmt("invalid", "h264") == "nv12"
        assert _vaapi_fmt_for_pix_fmt("", "h264") == "nv12"

class TestTargetBPP:
    def test_target_bpp_h264_standard(self):
        bpp_30fps = _target_bpp("h.264", 30)
        bpp_60fps = _target_bpp("h.264", 60)
        assert bpp_30fps == 0.07
        assert bpp_60fps == 0.07

    def test_target_bpp_h265_standard(self):
        bpp_h264 = _target_bpp("h.264", 60)
        bpp_h265 = _target_bpp("h.265", 60)
        assert bpp_h265 < bpp_h264

    def test_target_bpp_high_framerate(self):
        bpp_90fps = _target_bpp("h.264", 90)
        bpp_120fps = _target_bpp("h.264", 120)
        assert bpp_90fps >= 0.07
        assert bpp_120fps >= 0.07

class TestMarkerValue:
    def test_marker_value_default(self):
        marker = _marker_value()
        assert "LinuxPlayHost" in marker

    def test_marker_value_with_sid(self, monkeypatch):
        monkeypatch.setenv("LINUXPLAY_SID", "test-session-123")
        marker = _marker_value()
        assert marker == "LinuxPlayHost:test-session-123"

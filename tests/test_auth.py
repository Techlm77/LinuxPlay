import json
from pathlib import Path
import pytest
from linuxplay.host import (
    CA_CERT,
    CA_KEY,
    PIN_LENGTH,
    PIN_ROTATE_SECS,
    TCP_HANDSHAKE_PORT,
    TRUSTED_DB,
    UDP_CLIPBOARD_PORT,
    UDP_HEARTBEAT_PORT,
    HostState,
    _ensure_ca,
    _gen_pin,
    _load_trust_db,
    _marker_opt,
    _marker_value,
    _save_trust_db,
    _trust_record_for,
    _verify_fingerprint_trusted,
)

class TestPINManagement:
    def test_pin_generation_length(self):
        pin = _gen_pin(6)
        assert len(pin) == 6
        assert pin.isdigit()

    def test_pin_generation_range(self):
        pin = _gen_pin(6)
        pin_int = int(pin)
        assert 0 <= pin_int <= 999999

    def test_pin_leading_zeros(self):
        pins = [_gen_pin(6) for _ in range(100)]
        assert all(len(p) == 6 for p in pins)

class TestCertificateAuthSetup:
    def test_ensure_ca_creates_files(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        try:
            result = _ensure_ca()
            if result:
                assert Path(CA_CERT).exists()
                assert Path(CA_KEY).exists()
        except ImportError:
            pytest.skip("cryptography module not available")

    def test_ensure_ca_skips_if_exists(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        try:
            Path(CA_CERT).write_text("existing cert")
            Path(CA_KEY).write_text("existing key")
            result = _ensure_ca()
            if result:
                assert Path(CA_CERT).read_text() == "existing cert"
                assert Path(CA_KEY).read_text() == "existing key"
        except ImportError:
            pytest.skip("cryptography module not available")

class TestTrustedClientsDatabase:
    def test_load_trust_db_empty(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        db = _load_trust_db()
        assert "trusted_clients" in db
        assert isinstance(db["trusted_clients"], list)

    def test_save_and_load_trust_db(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        test_db = {
            "trusted_clients": [
                {
                    "fingerprint": "ABCD1234",
                    "common_name": "test-client",
                    "issued_on": "2024-01-01T00:00:00Z",
                    "status": "trusted",
                }
            ]
        }
        success = _save_trust_db(test_db)
        assert success is True
        assert Path(TRUSTED_DB).exists()
        loaded = _load_trust_db()
        assert len(loaded["trusted_clients"]) == 1
        assert loaded["trusted_clients"][0]["fingerprint"] == "ABCD1234"

    def test_trust_record_for_existing(self):
        db = {
            "trusted_clients": [
                {"fingerprint": "ABC123", "status": "trusted"},
                {"fingerprint": "DEF456", "status": "trusted"},
            ]
        }
        record = _trust_record_for("ABC123", db)
        assert record is not None
        assert record["fingerprint"] == "ABC123"

    def test_trust_record_for_missing(self):
        db = {"trusted_clients": [{"fingerprint": "ABC123", "status": "trusted"}]}
        record = _trust_record_for("MISSING", db)
        assert record is None

    def test_verify_fingerprint_trusted(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        test_db = {"trusted_clients": [{"fingerprint": "TRUSTED123", "status": "trusted"}]}
        Path(TRUSTED_DB).write_text(json.dumps(test_db))
        result = _verify_fingerprint_trusted("TRUSTED123")
        assert result is True

    def test_verify_fingerprint_untrusted(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        test_db = {"trusted_clients": [{"fingerprint": "TRUSTED123", "status": "trusted"}]}
        Path(TRUSTED_DB).write_text(json.dumps(test_db))
        result = _verify_fingerprint_trusted("UNTRUSTED")
        assert result is False

class TestHostState:
    def test_host_state_initialization(self):
        state = HostState()
        assert state.video_threads == []
        assert state.session_active is False
        assert state.authed_client_ip is None
        assert state.pin_code is None
        assert state.should_terminate is False
        assert hasattr(state, "pin_lock")
        assert hasattr(state, "video_thread_lock")

    def test_host_state_pin_management(self):
        state = HostState()
        state.pin_code = "123456"
        state.pin_expiry = 100.0
        assert state.pin_code == "123456"
        assert state.pin_expiry == 100.0

class TestMarkerValue:
    def test_marker_value_basic(self):
        marker = _marker_value()
        assert "LinuxPlayHost" in marker

    def test_marker_value_with_session_id(self, monkeypatch):
        monkeypatch.setenv("LINUXPLAY_SID", "test-123")
        marker = _marker_value()
        assert marker == "LinuxPlayHost:test-123"

    def test_marker_opt_returns_list(self):
        opt = _marker_opt()
        assert isinstance(opt, list)
        assert "-metadata" in opt

class TestCertificateFingerprint:
    pass

class TestSecurityConstants:
    def test_pin_length_constant(self):
        assert PIN_LENGTH == 6
        assert isinstance(PIN_LENGTH, int)

    def test_pin_rotate_seconds(self):
        assert PIN_ROTATE_SECS == 30
        assert isinstance(PIN_ROTATE_SECS, int)

    def test_port_constants(self):
        assert TCP_HANDSHAKE_PORT == 7001
        assert UDP_CLIPBOARD_PORT == 7002
        assert UDP_HEARTBEAT_PORT == 7004

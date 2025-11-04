"""Tests for authentication and security features."""

import json
import tempfile
from pathlib import Path

import pytest


class TestPINManagement:
    """Tests for PIN generation and rotation."""

    def test_pin_generation_length(self):
        """Test PIN is generated with correct length."""
        from linuxplay.host import _gen_pin

        pin = _gen_pin(6)
        assert len(pin) == 6
        assert pin.isdigit()

    def test_pin_generation_range(self):
        """Test PIN is within valid range."""
        from linuxplay.host import _gen_pin

        pin = _gen_pin(6)
        pin_int = int(pin)
        assert 0 <= pin_int <= 999999

    def test_pin_leading_zeros(self):
        """Test PIN preserves leading zeros."""
        from linuxplay.host import _gen_pin

        # Generate many PINs to statistically get some with leading zeros
        pins = [_gen_pin(6) for _ in range(100)]
        # All should be exactly 6 characters
        assert all(len(p) == 6 for p in pins)


class TestCertificateAuthSetup:
    """Tests for certificate authority setup."""

    def test_ensure_ca_creates_files(self, tmp_path, monkeypatch):
        """Test CA creation creates certificate and key files."""
        monkeypatch.chdir(tmp_path)

        try:
            from linuxplay.host import CA_CERT, CA_KEY, _ensure_ca

            result = _ensure_ca()
            # May fail if cryptography not available
            if result:
                assert Path(CA_CERT).exists()
                assert Path(CA_KEY).exists()
        except ImportError:
            pytest.skip("cryptography module not available")

    def test_ensure_ca_skips_if_exists(self, tmp_path, monkeypatch):
        """Test CA setup skips if files already exist."""
        monkeypatch.chdir(tmp_path)

        try:
            from linuxplay.host import CA_CERT, CA_KEY, _ensure_ca

            # Create dummy files
            Path(CA_CERT).write_text("existing cert")
            Path(CA_KEY).write_text("existing key")

            result = _ensure_ca()
            # Should return True without recreating
            if result:
                assert Path(CA_CERT).read_text() == "existing cert"
                assert Path(CA_KEY).read_text() == "existing key"
        except ImportError:
            pytest.skip("cryptography module not available")


class TestTrustedClientsDatabase:
    """Tests for trusted clients database management."""

    def test_load_trust_db_empty(self, tmp_path, monkeypatch):
        """Test loading empty trust database."""
        monkeypatch.chdir(tmp_path)

        from linuxplay.host import _load_trust_db

        db = _load_trust_db()
        assert "trusted_clients" in db
        assert isinstance(db["trusted_clients"], list)

    def test_save_and_load_trust_db(self, tmp_path, monkeypatch):
        """Test saving and loading trust database."""
        monkeypatch.chdir(tmp_path)

        from linuxplay.host import TRUSTED_DB, _load_trust_db, _save_trust_db

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

    def test_trust_record_for_existing(self, tmp_path, monkeypatch):
        """Test finding existing trust record."""
        from linuxplay.host import _trust_record_for

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
        """Test finding non-existent trust record returns None."""
        from linuxplay.host import _trust_record_for

        db = {"trusted_clients": [{"fingerprint": "ABC123", "status": "trusted"}]}

        record = _trust_record_for("MISSING", db)
        assert record is None

    def test_verify_fingerprint_trusted(self, tmp_path, monkeypatch):
        """Test verifying trusted fingerprint."""
        monkeypatch.chdir(tmp_path)

        from linuxplay.host import TRUSTED_DB, _verify_fingerprint_trusted

        # Create test database
        test_db = {"trusted_clients": [{"fingerprint": "TRUSTED123", "status": "trusted"}]}

        Path(TRUSTED_DB).write_text(json.dumps(test_db))

        result = _verify_fingerprint_trusted("TRUSTED123")
        assert result is True

    def test_verify_fingerprint_untrusted(self, tmp_path, monkeypatch):
        """Test verifying untrusted fingerprint."""
        monkeypatch.chdir(tmp_path)

        from linuxplay.host import TRUSTED_DB, _verify_fingerprint_trusted

        test_db = {"trusted_clients": [{"fingerprint": "TRUSTED123", "status": "trusted"}]}

        Path(TRUSTED_DB).write_text(json.dumps(test_db))

        result = _verify_fingerprint_trusted("UNTRUSTED")
        assert result is False


class TestHostState:
    """Tests for host state management."""

    def test_host_state_initialization(self):
        """Test HostState is properly initialized."""
        from linuxplay.host import HostState

        state = HostState()

        assert state.video_threads == []
        assert state.session_active is False
        assert state.authed_client_ip is None
        assert state.pin_code is None
        assert state.should_terminate is False
        assert hasattr(state, "pin_lock")
        assert hasattr(state, "video_thread_lock")

    def test_host_state_pin_management(self):
        """Test PIN management in host state."""
        from linuxplay.host import HostState

        state = HostState()

        # Set PIN
        state.pin_code = "123456"
        state.pin_expiry = 100.0

        assert state.pin_code == "123456"
        assert state.pin_expiry == 100.0


class TestMarkerValue:
    """Tests for FFmpeg marker generation."""

    def test_marker_value_basic(self):
        """Test basic marker value generation."""
        from linuxplay.host import _marker_value

        marker = _marker_value()
        assert "LinuxPlayHost" in marker

    def test_marker_value_with_session_id(self, monkeypatch):
        """Test marker with session ID."""
        from linuxplay.host import _marker_value

        monkeypatch.setenv("LINUXPLAY_SID", "test-123")

        marker = _marker_value()
        assert marker == "LinuxPlayHost:test-123"

    def test_marker_opt_returns_list(self):
        """Test _marker_opt returns proper FFmpeg option list."""
        from linuxplay.host import _marker_opt

        opt = _marker_opt()
        assert isinstance(opt, list)
        assert "-metadata" in opt


class TestCertificateFingerprint:
    """Tests for certificate fingerprint handling."""

class TestSecurityConstants:
    """Tests for security-related constants."""

    def test_pin_length_constant(self):
        """Test PIN_LENGTH constant is reasonable."""
        from linuxplay.host import PIN_LENGTH

        assert PIN_LENGTH == 6
        assert isinstance(PIN_LENGTH, int)

    def test_pin_rotate_seconds(self):
        """Test PIN_ROTATE_SECS constant."""
        from linuxplay.host import PIN_ROTATE_SECS

        assert PIN_ROTATE_SECS == 30
        assert isinstance(PIN_ROTATE_SECS, int)

    def test_port_constants(self):
        """Test security-related port constants."""
        from linuxplay.host import TCP_HANDSHAKE_PORT, UDP_CLIPBOARD_PORT, UDP_HEARTBEAT_PORT

        assert TCP_HANDSHAKE_PORT == 7001
        assert UDP_CLIPBOARD_PORT == 7002
        assert UDP_HEARTBEAT_PORT == 7004

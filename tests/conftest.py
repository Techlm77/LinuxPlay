import platform
import subprocess
import pytest

@pytest.fixture
def mock_ffmpeg_available(monkeypatch):
    def mock_check_output(*_args, **_kwargs):
        return b"ffmpeg version 6.0"

    monkeypatch.setattr(subprocess, "check_output", mock_check_output)

@pytest.fixture
def mock_linux_platform(monkeypatch):
    monkeypatch.setattr(platform, "system", lambda: "Linux")

@pytest.fixture
def mock_windows_platform(monkeypatch):
    monkeypatch.setattr(platform, "system", lambda: "Windows")

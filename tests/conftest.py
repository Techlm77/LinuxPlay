"""Pytest configuration and shared fixtures."""

import pytest


@pytest.fixture
def mock_ffmpeg_available(monkeypatch):
    """Mock FFmpeg as available."""
    import subprocess

    def mock_check_output(*args, **kwargs):
        return b"ffmpeg version 6.0"

    monkeypatch.setattr(subprocess, "check_output", mock_check_output)


@pytest.fixture
def mock_linux_platform(monkeypatch):
    """Mock platform as Linux."""
    import platform

    monkeypatch.setattr(platform, "system", lambda: "Linux")


@pytest.fixture
def mock_windows_platform(monkeypatch):
    """Mock platform as Windows."""
    import platform

    monkeypatch.setattr(platform, "system", lambda: "Windows")

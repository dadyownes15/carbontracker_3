import pytest

PINNED = dict(global_=True, power=10.0, intensity=100.0, update_interval=0.05)
"""No hardware, no network: the world-average grid, a stated draw and a fixed intensity."""


@pytest.fixture(autouse=True)
def no_key(monkeypatch, tmp_path):
    """Never the user's key: no variable, and a config directory of our own."""
    monkeypatch.delenv("CARBONTRACKER_EM_KEY", raising=False)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "config"))

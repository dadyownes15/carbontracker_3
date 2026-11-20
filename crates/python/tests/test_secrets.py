"""A key given to the package is in no output and no file it writes."""

import pytest

from carbontracker import CarbonTracker

KEY = "SENTINEL-key-0123456789"


def files_hold_key(root):
    return [p for p in root.rglob("*") if p.is_file() and KEY in p.read_text(errors="replace")]


# AC/REC-24
def test_a_key_given_as_api_keys_is_in_no_output_or_record(tmp_path, capsys, monkeypatch):
    monkeypatch.setenv("CARBONTRACKER_EM_URL", "http://127.0.0.1:9/v3")
    tracker = CarbonTracker(
        epochs=1, global_=True, power=10.0, update_interval=0.05, verbose=2,
        log_dir=str(tmp_path / "logs"), api_keys={"electricitymaps": KEY},
    )
    tracker.epoch_start()
    tracker.epoch_end()
    tracker.stop()
    out = capsys.readouterr()
    assert KEY not in out.out and KEY not in out.err
    assert files_hold_key(tmp_path / "logs") == []


# AC/FE-84
@pytest.mark.parametrize("api_keys", [{"no_such_service": KEY}, {"electricitymaps": [KEY]}, [KEY]])
def test_a_refused_api_keys_does_not_repeat_the_key(api_keys):
    with pytest.raises(ValueError) as refused:
        CarbonTracker(epochs=1, global_=True, power=10.0, log_dir=None, api_keys=api_keys)
    assert KEY not in str(refused.value)

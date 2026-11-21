"""Every script in examples/ runs as written, with power and intensity fixed so
it measures nothing and asks no one."""

import subprocess
import sys
from pathlib import Path

import pytest

EXAMPLES = Path(__file__).resolve().parents[3] / "examples"

# Runs a script as __main__, with CarbonTracker's defaults fixed: the world
# average unless the script states a location, a stated draw and intensity.
HARNESS = """
import runpy, sys
from carbontracker import tracker as module
original = module.CarbonTracker.__init__
def pinned(self, *args, **keywords):
    if not any(keywords.get(k) for k in ("zone", "country", "coordinates", "global_")):
        keywords["global_"] = True
    keywords.setdefault("power", 10.0)
    keywords.setdefault("intensity", 100.0)
    original(self, *args, **keywords)
module.CarbonTracker.__init__ = pinned
script = sys.argv[1]
sys.argv = [script]
runpy.run_path(script, run_name="__main__")
"""


def run(script, cwd):
    return subprocess.run(
        [sys.executable, "-c", HARNESS, str(script)], cwd=cwd, capture_output=True, text=True, timeout=120
    )


@pytest.mark.parametrize(
    "name",
    [
        "train.py",
        "train_profiling.py",
        pytest.param(
            "train_budget.py",
            marks=pytest.mark.xfail(
                strict=True,
                reason="known: predict() after the last epoch_end() raises, since that epoch stops the tracker",
            ),
        ),
    ],
)
def test_a_training_example_runs_and_records_its_run(name, tmp_path):
    finished = run(EXAMPLES / "training" / name, tmp_path)
    assert finished.returncode == 0, finished.stderr
    assert list((tmp_path / "logs").rglob("*.ct")), finished.stderr


def test_every_training_example_is_run_here():
    tested = {"train.py", "train_profiling.py", "train_budget.py"}
    assert {p.name for p in (EXAMPLES / "training").glob("*.py")} == tested

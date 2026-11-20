"""Choosing the sources a span bills, always pinned (see conftest)."""

import json
import time

import pytest
from conftest import PINNED

from carbontracker import CarbonTracker

TWO_DEVICES = dict(global_=True, intensity=100.0, cpu_power=20.0, gpu_power=50.0, components="cpu,gpu", update_interval=0.05)
"""A stated CPU and a stated GPU at a fixed intensity: no hardware read for them, no network."""


def record(log_dir):
    """The run's record, line by line."""
    [path] = log_dir.glob("*/*.ct")
    return [json.loads(line) for line in path.read_text().splitlines()]


def stated(tracker, domain):
    """The tracker's stated source of `domain`."""
    [source] = [source for source in tracker.sources.values() if source.label == f"static ({domain})"]
    return source


def billed(log_dir):
    """What each span in the record bills, by name, in start order."""
    lines = [line for line in record(log_dir) if line["kind"] == "span_started"]
    return [(line["name"], line["billed"]) for line in lines]


# AC/PY-45
# AC/PY-46
# AC/PY-47
# AC/PY-54
def test_domain_names_bill_those_domains_and_no_sources_bills_every_source(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        tracker.span("one", sources="gpu").start().stop()
        tracker.span("two", None, sources=["cpu", "ram"]).start().stop()
        tracker.span("every").start().stop()

    assert billed(tmp_path) == [
        ("one", {"domains": ["gpu"]}),
        ("two", {"domains": ["cpu", "ram"]}),
        ("every", "all"),
    ]


# AC/PY-48
# AC/PY-54
def test_sources_from_tracker_sources_bill_exactly_those_sources(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **TWO_DEVICES) as tracker:
        cpu, gpu = stated(tracker, "cpu"), stated(tracker, "gpu")
        tracker.span("one", sources=gpu).start().stop()
        tracker.span("both", sources=[cpu, gpu]).start().stop()

    assert billed(tmp_path) == [
        ("one", {"sources": [gpu.id]}),
        ("both", {"sources": [cpu.id, gpu.id]}),
    ]


REFUSED = {
    "a name that is not a domain": lambda tracker, other: "tpu",
    "a source id as a string": lambda tracker, other: next(iter(tracker.sources)),
    "an empty list": lambda tracker, other: [],
    "an empty iterable": lambda tracker, other: iter(()),
    "names and sources mixed": lambda tracker, other: ["cpu", stated(tracker, "system")],
    "an intensity source": lambda tracker, other: next(iter(other.session.intensities.values())),
    # Equal to the tracker's own source in every field.
    "a source of another tracker": lambda tracker, other: stated(other, "system"),
    "another type": lambda tracker, other: 7,
    "a list holding another type": lambda tracker, other: ["cpu", None],
}


# AC/PY-52
@pytest.mark.parametrize("case", REFUSED, ids=list(REFUSED))
def test_invalid_sources_raise_value_error_from_span_whatever_ignore_errors_says(tmp_path, case):
    other = CarbonTracker(verbose=0, **PINNED)
    other.stop()
    with CarbonTracker(verbose=0, log_dir=tmp_path, ignore_errors=True, **PINNED) as tracker:
        with pytest.raises(ValueError) as refused:
            tracker.span("refused", sources=REFUSED[case](tracker, other))

    for name in ["cpu", "gpu", "ram", "ane", "system"]:
        assert name in str(refused.value)
    assert billed(tmp_path) == []


def settled(log_dir):
    """Each settled span in the record, by name."""
    return {line["name"]: line for line in record(log_dir) if line["kind"] == "span_settled"}


# AC/PY-51
def test_a_span_keeps_its_sources_on_each_with_call_and_start(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        span = tracker.span("again", sources="gpu")

        @span
        def decorated():
            pass

        with span:
            pass
        decorated()
        span.start().stop()

    assert billed(tmp_path) == [("again", {"domains": ["gpu"]})] * 3


# AC/PY-55
# AC/PY-59
def test_a_spans_report_covers_only_the_sources_it_selected_billed_at_their_intensity(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **TWO_DEVICES) as tracker:
        cpu, gpu = stated(tracker, "cpu"), stated(tracker, "gpu")
        with tracker.span("gpu only", sources="gpu"):
            time.sleep(0.1)
        with tracker.span("cpu only", sources=[cpu]):
            time.sleep(0.1)

    reports = settled(tmp_path)
    assert list(reports["gpu only"]["per_source"]) == [gpu.id]
    assert list(reports["cpu only"]["per_source"]) == [cpu.id]
    # The GPU is billed at the stated 100 g/kWh.
    [usage] = reports["gpu only"]["per_source"].values()
    assert usage["energy_kwh"] > 0
    assert usage["emissions_g"] == pytest.approx(usage["energy_kwh"] * 100.0)


# AC/PY-56
def test_a_childs_sources_are_its_own_whatever_its_parent_selects(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **TWO_DEVICES) as tracker:
        cpu, gpu = stated(tracker, "cpu"), stated(tracker, "gpu")
        with tracker.span("parent", sources="cpu"):
            with tracker.span("child", sources="gpu"):
                time.sleep(0.1)

    reports = settled(tmp_path)
    assert list(reports["child"]["per_source"]) == [gpu.id]
    assert list(reports["parent"]["per_source"]) == [cpu.id]


# AC/PY-57
def test_epochs_bill_every_source_while_a_selecting_span_runs(tmp_path):
    with CarbonTracker(epochs=2, verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        for _ in range(2):
            tracker.epoch_start()
            with tracker.span("decode", sources="gpu"):
                pass
            tracker.epoch_end()

    assert billed(tmp_path) == [("epoch", "all"), ("decode", {"domains": ["gpu"]})] * 2


NOTHING_SELECTED = "selected sources that nothing on this host resolved"


# AC/PY-53
# AC/PY-60
def test_a_domain_left_out_by_components_runs_cuts_nothing_and_warns(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, global_=True, intensity=100.0, gpu_power=50.0, components="gpu") as tracker:
        with tracker.span("on the cpu", sources="cpu"):
            time.sleep(0.05)

    report = settled(tmp_path)["on the cpu"]
    assert report["per_source"] == {}
    assert report["energy_kwh"] == 0
    assert any(NOTHING_SELECTED in warning for warning in report["warnings"])


# AC/PY-53
# AC/PY-60
def test_a_domain_the_host_lacks_runs_cuts_nothing_and_warns(tmp_path):
    # The stated whole-system draw is all this tracker measures: no GPU.
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        with tracker.span("on the gpu", sources="gpu"):
            time.sleep(0.05)

    report = settled(tmp_path)["on the gpu"]
    assert report["per_source"] == {}
    assert any(NOTHING_SELECTED in warning for warning in report["warnings"])


# AC/PY-54
def test_a_profiled_span_bills_what_its_sources_select(tmp_path):
    with CarbonTracker(mode="profiler", verbose=0, log_dir=tmp_path, gpu_power=10.0, components="gpu") as tracker:
        with tracker.span("decode", sources="gpu"):
            pass

    assert ("decode", {"domains": ["gpu"]}) in billed(tmp_path)

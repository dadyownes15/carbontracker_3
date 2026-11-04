"""Profiler mode through the public interface. A stated GPU draw stands in for
the hardware: the draw is flat, so every span's dynamic energy is zero, and a
stated source is never waited for."""

import sys
import time
import types

import pytest

from carbontracker import CarbonTracker, ProfilerSpanReport, track

WATTS = 10.0
STATED = dict(gpu_power=WATTS, components="gpu")
"""No hardware: a stated GPU draw. Profiler mode never contacts the network."""


def profile(**arguments):
    return CarbonTracker(mode="profiler", verbose=0, **{**STATED, **arguments})


# Reports


def test_each_span_name_gets_one_report_over_its_calls():  # AC/PRF-33
    with profile() as tracker:
        for _ in range(3):
            with tracker.span("tokenize"):
                time.sleep(0.01)
            with tracker.span("forward"):
                time.sleep(0.02)

    spans = tracker.session.spans
    assert set(spans) == {"tokenize", "forward"}
    assert all(isinstance(report, ProfilerSpanReport) for report in spans.values())
    assert spans["tokenize"].calls == 3
    assert spans["forward"].duration_s >= 0.06


def test_a_flat_draw_is_all_baseline():  # AC/PRF-14, AC/PRF-15
    with profile() as tracker:
        with tracker.span("work"):
            time.sleep(0.05)

    report = tracker.session.spans["work"]
    assert tracker.session.baseline_w == pytest.approx(WATTS, rel=0.01)
    assert report.gross_energy_j == pytest.approx(WATTS * report.duration_s, rel=0.05)
    assert abs(report.dynamic_energy_j) < 0.05 * report.gross_energy_j
    assert report.gross_power_w == pytest.approx(WATTS, rel=0.05)
    assert report.dynamic_power_w == pytest.approx(report.dynamic_energy_j / report.duration_s)


def test_a_stated_source_is_never_waited_for():  # AC/PRF-9
    with profile() as tracker:
        with tracker.span("work"):
            pass
    assert tracker.session.spans["work"].waited_s < 0.01


def test_without_baseline_subtraction_dynamic_is_gross():  # AC/PRF-25
    with profile(subtract_baseline=False) as tracker:
        with tracker.span("work"):
            time.sleep(0.02)

    report = tracker.session.spans["work"]
    assert report.dynamic_energy_j == report.gross_energy_j
    assert tracker.session.baseline_w is None


def test_without_baseline_subtraction_power_is_over_the_window():  # AC/PRF-25
    # The inner span's settling is a wait inside the outer span's window.
    with profile(subtract_baseline=False, settle_s=0.2) as tracker:
        with tracker.span("outer"):
            with tracker.span("inner"):
                time.sleep(0.02)

    outer = tracker.session.spans["outer"]
    assert outer.waited_s > 0.1
    assert outer.gross_power_w == pytest.approx(WATTS, rel=0.05)
    assert outer.dynamic_power_w == pytest.approx(WATTS, rel=0.05)


def test_a_stated_baseline_is_not_measured():  # AC/PRF-26
    began = time.perf_counter()
    with profile(baseline_w=4.0, baseline_s=5.0) as tracker:
        with tracker.span("work"):
            time.sleep(0.02)
    # A measured baseline would take baseline_s = 5 s on its own.
    assert time.perf_counter() - began < 4.0
    assert tracker.session.baseline_w == 4.0
    report = tracker.session.spans["work"]
    assert report.dynamic_power_w == pytest.approx(WATTS - 4.0, rel=0.05)


def test_settling_waits_before_each_span_and_is_billed_outside_it():  # AC/PRF-24, AC/PRF-12
    with profile(settle_s=0.05) as tracker:
        with tracker.span("outer"):
            with tracker.span("inner"):
                time.sleep(0.01)

    spans = tracker.session.spans
    assert spans["inner"].waited_s < 0.01
    assert spans["inner"].duration_s == pytest.approx(0.01, abs=0.01)
    # The inner span's settling is inside outer, and not in its work time.
    assert spans["outer"].waited_s >= 0.05
    assert spans["outer"].duration_s < 0.04


def test_spans_are_timed_after_the_streams_finish(monkeypatch):  # AC/PRF-22
    calls = []
    cuda = types.SimpleNamespace(is_available=lambda: True, synchronize=lambda: calls.append(time.perf_counter()))
    monkeypatch.setitem(sys.modules, "torch", types.SimpleNamespace(cuda=cuda))

    with profile() as tracker:
        before = len(calls)
        with tracker.span("kernel"):
            pass
        assert len(calls) == before + 1

    calls.clear()
    with profile(sync_streams=False) as tracker:
        with tracker.span("kernel"):
            pass
    assert calls == []


def test_the_session_reports_electricity_and_no_emissions():  # AC/PRF-1
    with profile() as tracker:
        with tracker.span("work"):
            time.sleep(0.01)
    session = tracker.session
    assert session.total.energy_kwh > 0
    assert session.total.emissions_g is None
    assert all(usage.emissions_g is None for usage in session.per_source.values())


def test_the_baseline_span_is_not_reported():  # AC/PRF-14
    with profile() as tracker:
        pass
    assert tracker.session.spans == {}


# Arguments


@pytest.mark.parametrize(
    "arguments",
    [
        dict(epochs=3),
        dict(epochs_before_pred=2),
        dict(monitor_epochs=2),
        dict(zone="DK-DK2"),
        dict(country="DK"),
        dict(coordinates=(55.0, 12.0)),
        dict(global_=True),
        dict(intensity=100.0),
        dict(api_keys={"electricitymaps": "k"}),
    ],
)
def test_grid_and_epoch_arguments_are_refused(arguments):  # AC/PRF-29
    with pytest.raises(ValueError, match="profiler"):
        CarbonTracker(mode="profiler", ignore_errors=True, **STATED, **arguments)


@pytest.mark.parametrize(
    "arguments",
    [
        dict(sync_streams=False),
        dict(wait=False),
        dict(settle_s=0.1),
        dict(subtract_baseline=False),
        dict(baseline_w=50.0),
        dict(baseline_s=2.0),
    ],
)
def test_measuring_settings_are_refused_in_tracker_mode(arguments):  # AC/PRF-29
    with pytest.raises(ValueError, match="profiler"):
        CarbonTracker(global_=True, ignore_errors=True, **STATED, **arguments)


@pytest.mark.parametrize(
    "arguments, message",
    [
        (dict(settle_s=-1.0), "settle_s"),
        (dict(settle_s=float("nan")), "settle_s"),
        (dict(baseline_s=0.0), "baseline_s"),
        (dict(baseline_w=-1.0), "baseline_w"),
        (dict(subtract_baseline=False, baseline_w=50.0), "subtract_baseline"),
        (dict(subtract_baseline=False, baseline_s=2.0), "subtract_baseline"),
    ],
)
def test_invalid_measuring_settings_are_refused(arguments, message):  # AC/PRF-28
    with pytest.raises(ValueError, match=message):
        CarbonTracker(mode="profiler", ignore_errors=True, **STATED, **arguments)


def test_an_unknown_mode_is_refused():  # AC/PRF-19
    with pytest.raises(ValueError, match="mode"):
        CarbonTracker(mode="benchmark", ignore_errors=True, **STATED)


def test_epochs_and_predictions_are_refused_while_profiling():  # AC/PRF-30
    with profile() as tracker:
        with pytest.raises(ValueError, match="profiler"):
            tracker.epoch_start()
        with pytest.raises(ValueError, match="profiler"):
            tracker.predict(3)


def test_track_profiles_each_call_as_one_span():  # AC/PRF-31
    @track(mode="profiler", verbose=0, **STATED)
    def matmul(a, b):
        return a * b

    assert matmul(6, 7) == 42
    assert set(matmul.session.spans) == {"matmul"}


# Console


def test_the_table_has_electricity_and_no_carbon(capsys):  # AC/PRF-34
    with CarbonTracker(mode="profiler", **STATED) as tracker:
        with tracker.span("forward"):
            time.sleep(0.01)

    out = capsys.readouterr().out
    assert "forward" in out and "Dynamic" in out and "baseline" in out
    assert "CO2" not in out and "kWh" not in out


def test_the_table_shows_each_call_under_its_parent(capsys):  # AC/PRF-36
    with CarbonTracker(mode="profiler", **STATED) as tracker:
        for _ in range(2):
            with tracker.span("pipeline"):
                with tracker.span("tokenize"):
                    time.sleep(0.01)
                with tracker.span("forward"):
                    time.sleep(0.01)
                with tracker.span("forward"):
                    time.sleep(0.01)

    import re

    rows = [line for line in capsys.readouterr().out.splitlines() if line.endswith(" W") and " J " in line]
    labels = [re.split(r"\s{2,}", row.strip())[0] for row in rows]
    assert labels == [
        "pipeline #1", "├─ tokenize", "├─ forward #1", "└─ forward #2",
        "pipeline #2", "├─ tokenize", "├─ forward #1", "└─ forward #2",
    ], rows
    assert tracker.session.spans["forward"].calls == 4


def test_siblings_with_one_name_are_numbered_and_unique_names_are_bare():  # AC/PRF-36
    from carbontracker.profiler import tree_labels

    assert tree_labels(["a", "b", "a"]) == ["a #1", "b", "a #2"]
    assert tree_labels(["tokenize", "forward"]) == ["tokenize", "forward"]


def test_verbose_zero_prints_nothing(capsys):  # AC/PRF-34
    with profile() as tracker:
        with tracker.span("forward"):
            pass
    assert capsys.readouterr().out == ""


# Overlapping spans


class _Native:
    """Records which calls waited for reads."""

    def __init__(self):
        self.calls = []
        self._next = 0

    def _id(self):
        self._next += 1
        return self._next

    def start_span_paced(self, name, parent):
        self.calls.append(("start paced", name))
        return self._id(), 0.0, 0.0, []

    def start_span(self, name, parent):
        self.calls.append(("start", name))
        return self._id()

    def stop_span_paced(self, span, work):
        self.calls.append(("stop paced", span))
        return 0.0, []

    def stop_span(self, span, work):
        self.calls.append(("stop", span))


def _profiler():
    from carbontracker.profiler import Profiler, Settings

    native = _Native()
    return Profiler(native, Settings(sync_streams=False)), native


def test_spans_in_order_and_nested_spans_wait_for_reads():  # AC/PRF-5, AC/PRF-6
    profiler, native = _profiler()
    outer = profiler.start("outer", None)
    inner = profiler.start("inner", outer)
    profiler.stop(inner, [])
    profiler.stop(outer, [])
    assert [call for call, _ in native.calls] == ["start paced", "start paced", "stop paced", "stop paced"]


def test_a_span_overlapping_a_sibling_neither_waits_nor_stalls_it():  # AC/PRF-35
    profiler, native = _profiler()
    first = profiler.start("chat", None)
    second = profiler.start("embed", None)
    profiler.stop(first, [])
    profiler.stop(second, [])
    assert [call for call, _ in native.calls] == ["start paced", "start", "stop", "stop paced"]


def test_overlapping_spans_on_a_flat_draw_share_the_idle_once():  # AC/PRF-15, AC/PRF-35
    import threading

    with profile() as tracker:
        overlap = threading.Barrier(2)

        def request(name, seconds):
            with tracker.span(name):
                overlap.wait()
                time.sleep(seconds)

        threads = [threading.Thread(target=request, args=(name, seconds)) for name, seconds in (("chat", 0.3), ("embed", 0.1))]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

    spans = tracker.session.spans
    for name in ("chat", "embed"):
        report = spans[name]
        assert abs(report.dynamic_energy_j) < 0.05 * report.gross_energy_j, (name, report)
    gross = spans["chat"].gross_energy_j + spans["embed"].gross_energy_j
    assert gross == pytest.approx(WATTS * spans["chat"].duration_s, rel=0.1)

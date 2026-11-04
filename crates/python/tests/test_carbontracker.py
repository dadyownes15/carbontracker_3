"""The package through its public interface, always pinned (see conftest)."""

import asyncio
import builtins
import dataclasses
import datetime
import gc
import importlib.metadata
import inspect
import json
import math
import os
import pickle
import subprocess
import sys
import threading
import time
import re
import tracemalloc
import types
import warnings

import pytest
from conftest import PINNED

import carbontracker
import carbontracker.tracker
from carbontracker import CarbonTracker, Session, TrackingError, track


def run_epochs(tracker, count):
    for _ in range(count):
        tracker.epoch_start()
        tracker.epoch_end()


def span_events(log_dir):
    """Every span-started event in the run's JSON lines: name -> (id, parent)."""
    started = [line for line in record(log_dir) if line["kind"] == "span_started"]
    return [(line["name"], line["span"], line["parent"]) for line in started]


def record(log_dir):
    """The run's record, line by line."""
    [path] = log_dir.glob("*/*.ct")
    return [json.loads(line) for line in path.read_text().splitlines()]


# carbontracker 2's interface


# AC/PY-9
def test_carbontracker_2s_example_loop_runs_unchanged(capsys):
    tracker = CarbonTracker(epochs=3, **PINNED)
    run_epochs(tracker, 3)
    session = tracker.stop()

    out = capsys.readouterr().out
    assert "CarbonTracker: power " in out and "CarbonTracker: intensity  world average" in out
    assert "CarbonTracker: ready. Tracking started." in out
    assert "CarbonTracker: grid world average, 100 gCO2eq/kWh avg (stated)" in out
    assert "Predicted consumption for 3 epoch(s):" in out
    assert "Actual consumption:" in out
    assert "km travelled by car" in out
    assert "Finished monitoring." in out
    assert isinstance(session, Session) and session.total.energy_kwh > 0


# AC/PY-2
def test_carbontracker_2s_arguments_keep_their_names_positions_and_defaults():
    params = list(inspect.signature(CarbonTracker).parameters.values())
    # From carbontracker 2.4.7's tracker.py; its `epochs` had no default.
    assert [(p.name, p.default) for p in params[1:14]] == [
        ("epochs_before_pred", 1), ("monitor_epochs", -1), ("update_interval", 1), ("interpretable", True),
        ("stop_and_confirm", False), ("ignore_errors", False), ("components", "all"), ("devices_by_pid", False),
        ("log_dir", None), ("log_file_prefix", ""), ("verbose", 1), ("decimal_precision", 12), ("api_keys", None),
    ]
    assert params[0].name == "epochs"
    assert all(p.kind == p.POSITIONAL_OR_KEYWORD for p in params[:14])
    assert all(p.kind in (p.KEYWORD_ONLY, p.VAR_KEYWORD) for p in params[14:])


# AC/PY-2
def test_all_fourteen_arguments_can_be_given_by_position(tmp_path):
    pinned = {name: value for name, value in PINNED.items() if name != "update_interval"}
    tracker = CarbonTracker(
        3, 1, 2, 0.05, False, False, False, "all", False, tmp_path, "rank0", 0, 3, {}, **pinned
    )
    run_epochs(tracker, 2)
    assert tracker.session is not None, "monitor_epochs, third, stopped it"
    [folder] = tmp_path.iterdir()
    assert folder.name.startswith("rank0_run-"), "log_dir and log_file_prefix, tenth and eleventh"


# AC/PY-15
def test_the_prediction_is_kept_in_the_session():
    tracker = CarbonTracker(epochs=10, epochs_before_pred=2, verbose=0, **PINNED)
    run_epochs(tracker, 2)
    prediction = tracker.stop().prediction

    assert prediction.measured == 2
    assert prediction.of_total == 10
    assert prediction.intensity_g_per_kwh == pytest.approx(100.0)
    assert prediction.total.energy_kwh > 0


# AC/PY-11
def test_monitor_epochs_stops_tracking_by_itself(tmp_path, capsys):
    tracker = CarbonTracker(epochs=5, monitor_epochs=2, epochs_before_pred=0, log_dir=tmp_path, **PINNED)
    run_epochs(tracker, 2)
    assert tracker.session is not None
    assert "Actual consumption:" in capsys.readouterr().out
    run_epochs(tracker, 1)  # ignored, as in 2.4.7
    assert tracker.stop() is tracker.session
    assert capsys.readouterr().out == ""
    epochs = [name for name, *_ in span_events(tmp_path) if name == "epoch"]
    assert len(epochs) == 2


# AC/PY-12
def test_an_early_stop_says_so_and_leaves_out_the_open_epoch(tmp_path, capsys):
    tracker = CarbonTracker(epochs=5, epochs_before_pred=0, log_dir=tmp_path, **PINNED)
    run_epochs(tracker, 1)
    tracker.epoch_start()
    tracker.stop()

    out = capsys.readouterr().out
    assert "Training was interrupted before all 5 epochs were monitored." in out
    assert "Actual consumption:" in out
    [outcome] = [line for line in record(tmp_path) if line["kind"] == "outcome"]
    assert (outcome["ended"], outcome["epochs_done"]) == ("interrupted", 1)


# AC/PY-33
def test_verbose_above_one_prints_as_one(capsys):
    CarbonTracker(verbose=2, **PINNED).stop()
    assert "Finished monitoring." in capsys.readouterr().out


# AC/PY-33
def test_verbose_zero_prints_nothing(capsys):
    CarbonTracker(verbose=0, **PINNED).stop()
    assert capsys.readouterr().out == ""


# AC/PY-33
def test_verbose_zero_still_prints_an_error(capsys):
    tracker = CarbonTracker(epochs=2, verbose=0, ignore_errors=True, **PINNED)
    tracker.epoch_end()  # before any epoch_start: a tracking failure
    assert "Continued training without monitoring" in capsys.readouterr().out


# AC/PY-10
def test_epochs_before_pred_zero_predicts_nothing(capsys):
    tracker = CarbonTracker(epochs=3, epochs_before_pred=0, **PINNED)
    run_epochs(tracker, 3)
    assert tracker.session.prediction is None
    assert "Predicted consumption" not in capsys.readouterr().out


# AC/PY-4
def test_stop_and_confirm_is_gone_and_says_why():
    with pytest.raises(TypeError, match="stop_and_confirm was removed in carbontracker 3: it waited on input()"):
        CarbonTracker(epochs=2, stop_and_confirm=True, **PINNED)
    # False, in its position, still works.
    pinned = {name: value for name, value in PINNED.items() if name != "update_interval"}
    CarbonTracker(2, 1, -1, PINNED["update_interval"], True, False, verbose=0, **pinned).stop()


# AC/PY-4
@pytest.mark.parametrize(
    "name, replacement",
    [
        ("sim_cpu", "cpu_power="), ("sim_cpu_tdp", "cpu_power="), ("sim_cpu_util", "cpu_power="),
        ("sim_gpu", "gpu_power="), ("sim_gpu_watts", "gpu_power="), ("sim_gpu_util", "gpu_power="),
    ],
)
def test_a_simulated_device_names_its_replacement(name, replacement):
    with pytest.raises(TypeError, match=f"{name} was removed in carbontracker 3: .*{replacement}"):
        CarbonTracker(epochs=1, ignore_errors=True, **{name: 1}, **PINNED)


def test_an_unknown_argument_is_a_type_error():
    with pytest.raises(TypeError, match="unexpected keyword argument 'colour'"):
        CarbonTracker(epochs=1, colour=True)


# AC/PY-7
def test_a_tracking_failure_raises_a_runtime_error_with_its_cause():
    tracker = CarbonTracker(epochs=2, **PINNED)
    with pytest.raises(TrackingError, match="tracking failed") as raised:
        tracker.epoch_end()  # before any epoch_start
    assert isinstance(raised.value, RuntimeError)
    assert raised.value.__cause__ is not None
    tracker.stop()


# AC/PY-8
def test_an_ignored_failure_stops_tracking_and_training_goes_on(tmp_path, capsys):
    tracker = CarbonTracker(epochs=3, ignore_errors=True, log_dir=tmp_path, **PINNED)
    run_epochs(tracker, 1)
    tracker.epoch_end()  # no epoch open: a tracking failure
    out = capsys.readouterr().out
    assert "Traceback" in out and "Continued training without monitoring" in out
    session = tracker.session
    assert session is not None, "the session up to the failure"

    run_epochs(tracker, 2)  # training goes on, untracked
    assert capsys.readouterr().out == ""
    assert tracker.stop() is session
    [outcome] = [line for line in record(tmp_path) if line["kind"] == "outcome"]
    assert (outcome["ended"], outcome["epochs_done"]) == ("failed", 1)
    assert [name for name, *_ in span_events(tmp_path)] == ["epoch"]


# Measuring any code


# AC/PY-25
def test_with_stops_tracking_at_the_end_of_the_block():
    with CarbonTracker(verbose=0, **PINNED) as tracker:
        pass
    assert tracker.session is not None

    with pytest.raises(KeyError):
        with CarbonTracker(verbose=0, **PINNED) as tracker:
            raise KeyError("boom")
    assert tracker.session is not None


# AC/PY-13
def test_without_epochs_stop_prints_the_usual_output_and_returns_the_session(capsys):
    tracker = CarbonTracker(**PINNED)
    session = tracker.stop()

    out = capsys.readouterr().out
    assert "Actual consumption:" in out and "Finished monitoring." in out
    assert "interrupted" not in out
    assert isinstance(session, Session)
    with pytest.raises(ValueError, match="epochs"):
        tracker.epoch_start()


# AC/PY-19
def test_spans_nest_as_the_code_does(tmp_path):
    with CarbonTracker(epochs=10, verbose=0, log_dir=tmp_path, **PINNED) as tracker:

        @tracker.span("eval")
        def evaluate():
            return "done"

        with tracker.span("train") as train:
            train.mark("samples", 64)
            assert evaluate() == "done"
        assert evaluate() == "done"

        tracker.epoch_start()
        manual = tracker.span("manual").start()
        manual.stop({"steps": 3})
        manual.stop()  # twice: nothing
        tracker.epoch_end()

    events = span_events(tmp_path)
    ids = {}
    for name, id, parent in events:
        ids.setdefault(name, []).append((id, parent))
    [(train, train_parent)] = ids["train"]
    assert train_parent is None
    assert [parent for _, parent in ids["eval"]] == [train, None]
    [(epoch, _)] = ids["epoch"]
    assert ids["manual"] == [(ids["manual"][0][0], epoch)]


# AC/PY-19
def test_an_explicit_parent_wins(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        outer = tracker.span("outer").start()
        with tracker.span("other"):
            tracker.span("child", parent=outer).start().stop()
        outer.stop()

    events = {name: (id, parent) for name, id, parent in span_events(tmp_path)}
    assert events["child"][1] == events["outer"][0]


# AC/PY-16
def test_each_with_and_start_opens_a_new_span(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        span = tracker.span("again")
        with span:
            pass
        with span:
            pass
        span.start().stop()

    ids = [id for name, id, _ in span_events(tmp_path) if name == "again"]
    assert len(ids) == 3 and len(set(ids)) == 3


# AC/PY-16
# AC/PY-17
# AC/PY-18
def test_span_calls_never_wait_for_a_readout():
    # One readout a minute: a call that waited for one would take that long.
    with CarbonTracker(verbose=0, **{**PINNED, "update_interval": 60}) as tracker:
        began = time.monotonic()
        for _ in range(200):
            with tracker.span("quick") as span:
                span.mark("samples", 1)
            tracker.span("by-hand").start().stop({"steps": 1})
        took = time.monotonic() - began
    assert took < 5, f"200 spans took {took:.1f} s"


# AC/PY-17
# AC/PY-18
def test_marks_and_work_reach_the_record_until_the_span_stops(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:
        span = tracker.span("work").start()
        span.mark("samples", 64)
        span.mark("steps")
        span.stop({"tokens": 3})
        span.mark("samples", 5)  # after the stop: nothing
        span.stop({"tokens": 9})  # a second stop: nothing

    lines = record(tmp_path)
    marked = [line["work"] for line in lines if line["kind"] == "marked"]
    assert marked == [[{"kind": "samples", "amount": 64.0}], [{"kind": "steps", "amount": 1.0}]]
    stopped = [line["work"] for line in lines if line["kind"] == "span_stopped"]
    assert stopped == [[{"kind": "tokens", "amount": 3.0}]]


# AC/PY-16
def test_recursion_gives_a_span_per_call(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:

        @tracker.span("fib")
        def fib(n):
            return n if n < 2 else fib(n - 1) + fib(n - 2)

        assert fib(5) == 5

    calls = [(id, parent) for name, id, parent in span_events(tmp_path) if name == "fib"]
    assert len(calls) == 15
    assert len({id for id, _ in calls}) == 15
    assert sum(parent is None for _, parent in calls) == 1


# AC/PY-19
def test_threads_and_tasks_nest_independently(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:

        def work(name):
            with tracker.span(name):
                with tracker.span(f"{name}-inner"):
                    pass

        threads = [threading.Thread(target=work, args=(f"t{i}",)) for i in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        @tracker.span("task")
        async def task(i):
            with tracker.span(f"task{i}-inner"):
                await asyncio.sleep(0.01)

        async def main():
            await asyncio.gather(*(task(i) for i in range(3)))

        asyncio.run(main())

    events = span_events(tmp_path)
    by_id = {id: name for name, id, _ in events}
    for name, id, parent in events:
        if name.startswith("t") and name.endswith("-inner") and not name.startswith("task"):
            assert by_id[parent] == name.removesuffix("-inner")
        if name.startswith("task") and name.endswith("-inner"):
            assert by_id[parent] == "task"
    assert [name for name, *_ in events].count("task") == 3


# AC/PY-20
def test_an_exception_in_a_span_still_stops_it(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:

        @tracker.span("fails")
        def fails():
            raise KeyError("boom")

        @tracker.span("fails-async")
        async def fails_async():
            raise KeyError("boom")

        with pytest.raises(KeyError):
            fails()
        with pytest.raises(KeyError):
            asyncio.run(fails_async())
        with pytest.raises(KeyError):
            with tracker.span("also"):
                raise KeyError("boom")
        with tracker.span("after") as after:
            pass

    lines = record(tmp_path)
    started = {line["span"]: line["name"] for line in lines if line["kind"] == "span_started"}
    stopped = [started[line["span"]] for line in lines if line["kind"] == "span_stopped"]
    assert stopped == ["fails", "fails-async", "also", "after"]
    assert [parent for name, _, parent in span_events(tmp_path) if name == "after"] == [None], "none left open"


# AC/PY-21
def test_a_decorated_function_keeps_its_name_and_docstring():
    with CarbonTracker(verbose=0, **PINNED) as tracker:

        @tracker.span("plain")
        def plain(x):
            """Doubles."""
            return 2 * x

        @tracker.span("waits")
        async def waits(x):
            """Waits, then doubles."""
            await asyncio.sleep(0)
            return 2 * x

        assert (plain.__name__, plain.__doc__) == ("plain", "Doubles.")
        assert (waits.__name__, waits.__doc__) == ("waits", "Waits, then doubles.")
        assert inspect.iscoroutinefunction(waits)
        assert plain(2) == 4 and asyncio.run(waits(2)) == 4


# AC/PY-23
def test_after_stop_a_decorated_function_runs_untracked():
    tracker = CarbonTracker(verbose=0, **PINNED)

    @tracker.span("work")
    def work():
        return 42

    @tracker.span("waits")
    async def waits():
        return 43

    tracker.stop()
    assert work() == 42
    assert asyncio.run(waits()) == 43
    with pytest.raises(RuntimeError, match="stopped"):
        tracker.span("late").start()


# AC/PY-22
def test_epoch_is_the_epochs_span_name():
    with CarbonTracker(verbose=0, **PINNED) as tracker:
        with pytest.raises(ValueError, match="epoch"):
            tracker.span("epoch")


# AC/PY-31
def test_track_makes_each_call_a_session(tmp_path):
    @track(verbose=0, log_dir=tmp_path, **PINNED)
    def train(x):
        return x * 2

    assert train(21) == 42
    first = train.session
    assert train(1) == 2
    assert isinstance(train.session, Session) and train.session is not first
    assert train.__name__ == "train"
    records = sorted(tmp_path.glob("*/*.ct"))
    assert len(records) == 2
    for path in records:
        lines = [json.loads(line) for line in path.read_text().splitlines()]
        assert [line["name"] for line in lines if line["kind"] == "span_started"] == ["train"]
        assert lines[-1]["kind"] == "session"

    @track
    def bare():
        return "ok"

    # Bare means defaults: probing this host, so only check it's wrapped.
    assert bare.__name__ == "bare" and bare.session is None


# AC/PY-31
def test_track_wraps_async_functions():
    @track(verbose=0, **PINNED)
    async def train():
        await asyncio.sleep(0.01)
        return "ok"

    assert asyncio.run(train()) == "ok"
    assert isinstance(train.session, Session)


# Results


# AC/PY-27
def test_the_session_is_just_data():
    with CarbonTracker(verbose=0, **PINNED) as tracker:
        with tracker.span("work"):
            time.sleep(0.1)
    session = tracker.session
    del tracker
    gc.collect()

    assert pickle.loads(pickle.dumps(session)) == session
    assert json.loads(json.dumps(dataclasses.asdict(session), default=str))["total"]["energy_kwh"] > 0
    with pytest.raises(dataclasses.FrozenInstanceError):
        session.duration_s = 0
    assert [source.kind for source in session.intensities.values()] == ["intensity"]
    assert session.started_at.utcoffset() == datetime.timedelta(0)
    assert abs(datetime.datetime.now(datetime.timezone.utc) - session.started_at) < datetime.timedelta(minutes=1)
    # 10 W stated, times the default PUE of 1.58, over the session's seconds;
    # 100 g per kWh.
    watts = session.total.energy_kwh * 3_600_000 / session.duration_s
    assert watts == pytest.approx(10 * 1.58, rel=1e-3)
    assert session.total.emissions_g == pytest.approx(session.total.energy_kwh * 100)
    assert 0.1 <= session.duration_s < 30


# AC/PY-26
def test_stop_twice_returns_the_same_session():
    tracker = CarbonTracker(verbose=0, **PINNED)
    assert tracker.stop() is tracker.stop()


# AC/PY-26
def test_stop_returns_once_everything_is_written(tmp_path):
    tracker = CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED)
    session = tracker.stop()
    lines = record(tmp_path)
    assert lines[-1]["kind"] == "session"
    assert lines[-1]["energy_kwh"] == pytest.approx(session.total.energy_kwh)
    [log] = tmp_path.glob("*/output.log")
    assert log.read_text().rstrip().endswith("Finished monitoring.")


# Configuring


INVALID = [
    (dict(global_=True, country="DK"), "at most one"),
    (dict(zone="DK-DK2", coordinates=(55.0, 12.0)), "at most one"),
    (dict(country="Denmark"), "two letters"),
    (dict(country="D1"), "two letters"),
    (dict(coordinates=(95.0, 0.0)), "within"),
    (dict(coordinates=(0.0, 181.0)), "within"),
    (dict(zone=" "), "zone"),
    (dict(zone=""), "zone"),
    (dict(global_=True, power=10.0, cpu_power=5.0), "whole machine"),
    (dict(global_=True, power=10.0, gpu_power=5.0), "whole machine"),
    (dict(global_=True, power=-1.0), "power"),
    (dict(global_=True, cpu_power=-1.0), "cpu_power"),
    (dict(global_=True, gpu_power=-1.0), "gpu_power"),
    (dict(global_=True, intensity=-1.0), "intensity"),
    (dict(global_=True, intensity=0.0), "intensity"),
    (dict(global_=True, pue=0.5), "pue"),
    (dict(global_=True, update_interval=0), "update_interval"),
    (dict(global_=True, update_interval=-1), "update_interval"),
    (dict(global_=True, components="tpu"), "components"),
    (dict(global_=True, epochs=-1), "epochs"),
    (dict(global_=True, decimal_precision=-1), "decimal_precision"),
    (dict(global_=True, api_keys="key"), "api_keys"),
    (dict(global_=True, api_keys={"electricitymaps": 5}), "api_keys"),
    (dict(global_=True, api_keys={"carbonintensityGB": "k"}), "api_keys: no service named carbonintensityGB"),
    (dict(global_=True, epochs=3, monitor_epochs=0), "monitor_epochs"),
    (dict(global_=True, monitor_epochs=0), "monitor_epochs"),
    (dict(global_=True, epochs=3, epochs_before_pred=2, monitor_epochs=1), "monitor_epochs"),
]


# AC/PY-3
@pytest.mark.parametrize("arguments, message", INVALID)
def test_invalid_arguments_raise_value_error_whatever_ignore_errors_says(arguments, message):
    with pytest.raises(ValueError, match=message):
        CarbonTracker(ignore_errors=True, verbose=0, **arguments)


# AC/PY-3
@pytest.mark.parametrize("arguments, message", INVALID)
def test_an_invalid_argument_starts_nothing(tmp_path, capsys, arguments, message):
    with pytest.raises(ValueError):
        CarbonTracker(ignore_errors=True, log_dir=tmp_path / "logs", **arguments)
    assert not (tmp_path / "logs").exists()
    assert capsys.readouterr() == ("", "")


def test_a_stated_cpu_draw_is_served_as_static():
    with CarbonTracker(verbose=0, global_=True, intensity=100.0, cpu_power=50.0, components="cpu") as tracker:
        pass
    # "cpu" takes RAM along, as in 2.4.7, which this host may measure.
    [cpu] = [source for source in tracker.session.sources.values() if source.label == "static (cpu)"]
    assert cpu.fidelity == "stated"


@pytest.mark.skipif(sys.platform == "win32", reason="file modes are Unix")
# AC/PY-6
def test_a_config_file_others_can_read_gets_a_warning(tmp_path, capsys):
    config = tmp_path / "config" / "carbontracker"
    config.mkdir(parents=True)
    (config / "config.toml").write_text('[keys]\nelectricity_maps = "k"\n')
    os.chmod(config / "config.toml", 0o644)

    session = CarbonTracker(verbose=0, **PINNED).stop()
    assert "can be read by others: chmod 600" in capsys.readouterr().err
    assert session is not None and session.total.energy_kwh > 0, "tracking went on"


# AC/PY-3
def test_a_config_file_naming_an_unknown_service_is_invalid(tmp_path):
    config = tmp_path / "config" / "carbontracker"
    config.mkdir(parents=True)
    (config / "config.toml").write_text('[keys]\nnope = "k"\n')
    os.chmod(config / "config.toml", 0o600)

    with pytest.raises(ValueError, match="no service named nope"):
        CarbonTracker(**PINNED)


# Output


# AC/PY-35
@pytest.mark.parametrize("prefix, start", [("rank0", "rank0_run-"), ("", "run-")])
def test_each_run_gets_a_folder_with_its_record_and_output(tmp_path, prefix, start):
    before = datetime.datetime.now().replace(microsecond=0)
    with CarbonTracker(verbose=1, log_dir=tmp_path, log_file_prefix=prefix, **PINNED):
        time.sleep(0.2)
    after = datetime.datetime.now()

    [folder] = tmp_path.iterdir()
    match = re.fullmatch(re.escape(start) + r"(\d{8}_\d{6})-([a-z0-9]{8})", folder.name)
    assert match, folder.name
    stamp, id = match.groups()
    # The start, in local time.
    assert before <= datetime.datetime.strptime(stamp, "%Y%m%d_%H%M%S") <= after, stamp
    assert sorted(path.name for path in folder.iterdir()) == ["output.log", f"run-{id}.ct"]
    assert "Finished monitoring." in (folder / "output.log").read_text()
    assert [line["kind"] for line in record(tmp_path)][-1] == "session"


# AC/PY-34
def test_without_a_log_dir_nothing_is_written(tmp_path, monkeypatch):
    work = tmp_path / "work"
    work.mkdir()
    monkeypatch.chdir(work)
    tracker = CarbonTracker(epochs=2, **PINNED)
    run_epochs(tracker, 2)
    assert list(work.iterdir()) == []


# AC/PY-36
def test_output_log_holds_the_whole_output_whatever_verbose_says(tmp_path, capsys):
    tracker = CarbonTracker(epochs=2, verbose=0, log_dir=tmp_path, **PINNED)
    run_epochs(tracker, 2)
    assert capsys.readouterr().out == ""

    [log] = tmp_path.glob("*/output.log")
    text = log.read_text()
    for line in ["CarbonTracker: power ", "ready. Tracking started.", "Predicted consumption for 2 epoch(s):",
                 "Actual consumption:", "Finished monitoring."]:
        assert line in text, line


# AC/PY-37
def test_a_log_dir_that_cannot_be_written_is_a_tracking_failure(tmp_path, capsys):
    taken = tmp_path / "a-file"
    taken.write_text("")
    with pytest.raises(TrackingError) as raised:
        CarbonTracker(log_dir=taken, **PINNED)
    assert isinstance(raised.value.__cause__, OSError)

    tracker = CarbonTracker(epochs=2, log_dir=taken, ignore_errors=True, **PINNED)
    assert "Continued training without monitoring" in capsys.readouterr().out
    run_epochs(tracker, 2)  # training goes on
    assert tracker.stop() is None


# AC/PY-32
def test_output_goes_through_tqdm_write_when_tqdm_is_imported(monkeypatch, capsys):
    written = []
    tqdm = types.ModuleType("tqdm")
    tqdm.write = written.append
    monkeypatch.setitem(sys.modules, "tqdm", tqdm)

    CarbonTracker(**PINNED).stop()
    assert capsys.readouterr().out == ""
    assert any("Finished monitoring." in line for line in written)


# AC/PY-32
def test_output_goes_through_print(monkeypatch):
    printed = []
    monkeypatch.setattr(builtins, "print", lambda *args, **_: printed.append(" ".join(map(str, args))))
    CarbonTracker(**PINNED).stop()
    assert any("Finished monitoring." in line for line in printed)


# Lifecycle and performance


@pytest.mark.skipif(not hasattr(os, "fork"), reason="needs fork")
@pytest.mark.filterwarnings("ignore:This process .* is multi-threaded:DeprecationWarning")
# AC/PY-30
def test_a_forked_child_runs_spans_untracked(tmp_path):
    with CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED) as tracker:

        @tracker.span("work")
        def work():
            return 1

        pid = os.fork()
        if pid == 0:  # the child: no tracker threads here
            try:
                with tracker.span("in-child") as span:
                    span.mark("steps")
                ok = work() == 1 and tracker.span("x").start().id is None
                tracker.stop()
                carbontracker.tracker._stop_all()  # what exit would run
            except BaseException:
                ok = False
            os._exit(0 if ok else 1)
        _, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0
        assert work() == 1  # the parent still tracks

    lines = record(tmp_path)
    assert [name for name, *_ in span_events(tmp_path)] == ["work"]
    assert [line["kind"] for line in lines].count("session") == 1
    assert not [line for line in lines if line["kind"] == "marked"]


# AC/PY-24
def test_many_spans_hold_no_memory():
    with CarbonTracker(verbose=0, **PINNED) as tracker:

        @tracker.span("step")
        def step():
            pass

        for _ in range(500):
            step()
        tracemalloc.start()
        before, _ = tracemalloc.get_traced_memory()
        for _ in range(5_000):
            step()
        after, _ = tracemalloc.get_traced_memory()
        tracemalloc.stop()

    assert after - before < 100_000, f"grew by {after - before} bytes"


def command(*arguments):
    """The installed ``carbontracker`` command, beside this Python."""
    script = os.path.join(os.path.dirname(sys.executable), "carbontracker")
    assert os.path.exists(script), "installing the package installs the command"
    return subprocess.run([script, *arguments], capture_output=True, text=True, check=False)


# AC/PY-42
def test_installing_the_package_installs_the_command():
    distribution = importlib.metadata.distribution("carbontracker")
    [entry] = [entry for entry in distribution.entry_points if entry.group == "console_scripts" and entry.name == "carbontracker"]
    assert entry.value == "carbontracker.cli:main"

    pinned = ["--global", "--power", "10", "--intensity", "100", "--no-log"]
    result = command(*pinned, sys.executable, "-c", "print('out'); raise SystemExit(3)")
    assert result.returncode == 3, result.stderr
    assert result.stdout == "out\n"
    assert " summary:" in result.stderr
    bad = command("--global", "--country", "DK", "true")
    assert bad.returncode == 2 and bad.stderr


# AC/PY-42, AC/FE-32
@pytest.mark.skipif(not hasattr(os, "killpg"), reason="process groups are Unix")
def test_ctrl_c_under_the_command_exits_130_without_a_traceback(tmp_path):
    import signal

    script = os.path.join(os.path.dirname(sys.executable), "carbontracker")
    started = tmp_path / "started"
    pinned = ["--global", "--power", "10", "--intensity", "100", "--no-log"]
    process = subprocess.Popen(
        [script, *pinned, "sh", "-c", f"touch {started}; exec sleep 30"],
        stderr=subprocess.PIPE, text=True, start_new_session=True,
    )
    deadline = time.monotonic() + 20
    while not started.exists():
        assert time.monotonic() < deadline, "the command never started"
        time.sleep(0.02)
    # Like a terminal's Ctrl-C: SIGINT to the whole group.
    os.killpg(process.pid, signal.SIGINT)
    _, stderr = process.communicate(timeout=20)
    assert process.returncode == 130, stderr
    assert " summary:" in stderr
    assert "Traceback" not in stderr and "KeyboardInterrupt" not in stderr, stderr


# AC/PY-29
def test_a_tracker_never_stopped_is_stopped_at_exit(tmp_path):
    script = "\n".join([
        "import sys",
        "from carbontracker import CarbonTracker",
        f"tracker = CarbonTracker(epochs=3, verbose=0, log_dir=sys.argv[1], **{PINNED!r})",
        "tracker.epoch_start()",
        "tracker.epoch_end()",
    ])
    result = subprocess.run([sys.executable, "-c", script, str(tmp_path)], capture_output=True, text=True, check=False)
    assert result.returncode == 0, result.stderr

    lines = record(tmp_path)
    assert lines[-1]["kind"] == "session"
    [outcome] = [line for line in lines if line["kind"] == "outcome"]
    assert (outcome["ended"], outcome["epochs_done"]) == ("interrupted", 1)
    [log] = tmp_path.glob("*/output.log")
    assert "Finished monitoring." in log.read_text()


# AC/PY-43
def test_it_never_exits_reads_input_or_restarts(tmp_path, monkeypatch):
    def refuse(*_):
        raise AssertionError("called")

    monkeypatch.setattr(builtins, "input", refuse)
    monkeypatch.setattr(sys, "exit", refuse)
    monkeypatch.setattr(os, "_exit", refuse)
    monkeypatch.setattr(os, "execv", refuse)

    tracker = CarbonTracker(epochs=3, monitor_epochs=2, log_dir=tmp_path / "a", **PINNED)
    run_epochs(tracker, 3)
    tracker.stop()
    failing = CarbonTracker(epochs=3, ignore_errors=True, log_dir=tmp_path / "b", **PINNED)
    failing.epoch_end()
    failing.stop()
    with pytest.raises(TrackingError):
        CarbonTracker(epochs=3, **PINNED).epoch_end()
    # The key is given once, to the constructor: nothing sets it later.
    assert not [name for name in dir(CarbonTracker) if "key" in name.lower()]


# AC/PY-1
def test_the_package_exports_what_users_name():
    names = {"CarbonTracker", "Session", "Span", "RunningSpan", "Source", "Usage", "Prediction", "ProfilerSpanReport", "Report",
             "TrackingError", "report", "track"}
    assert set(carbontracker.__all__) == names
    for name in names:
        assert getattr(carbontracker, name) is not None
    assert carbontracker.tracker.CarbonTracker is carbontracker.CarbonTracker
    assert issubclass(carbontracker.TrackingError, RuntimeError)


# AC/PY-14
# AC/PY-15
def test_a_prediction_can_be_asked_for_besides_the_one_made_by_itself():
    # A long interval, so the last epoch has not settled when predict() is asked.
    tracker = CarbonTracker(epochs=10, epochs_before_pred=1, verbose=0, **{**PINNED, "update_interval": 1.0})
    run_epochs(tracker, 2)
    asked = tracker.predict(20)
    assert (asked.measured, asked.of_total) == (2, 20)
    assert tracker.predict().of_total == 10, "the planned epochs by default"
    assert tracker.stop().prediction.measured == 1, "the one made after epochs_before_pred"
    with pytest.raises(RuntimeError):
        tracker.predict()


# AC/PY-14, AC/PY-28
def test_a_prediction_before_any_epoch_is_none():
    with CarbonTracker(epochs=3, verbose=0, **PINNED) as tracker:
        assert tracker.predict() is None
        tracker.epoch_start()
        assert tracker.predict() is None, "an open epoch is not measured"
        tracker.epoch_end()
        assert tracker.predict().measured == 1


# AC/PY-44
def test_epoch_end_without_epoch_start_names_it(capsys):
    with CarbonTracker(epochs=3, verbose=0, **PINNED) as tracker:
        with pytest.raises(TrackingError, match=r"epoch_start\(\)") as raised:
            tracker.epoch_end()
        assert "NoneType" not in str(raised.value)
    ignored = CarbonTracker(epochs=3, ignore_errors=True, verbose=0, **PINNED)
    ignored.epoch_end()
    assert "epoch_start()" in capsys.readouterr().out
    assert ignored.stop() is not None


# AC/PY-14
def test_a_prediction_needs_a_number_of_epochs():
    with CarbonTracker(verbose=0, **PINNED) as tracker:
        with pytest.raises(ValueError, match="epochs"):
            tracker.predict()


# AC/PY-35
def test_trackers_started_together_never_share_log_files(tmp_path):
    # As each rank of a distributed job does: same directory, same second.
    first = CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED)
    second = CarbonTracker(verbose=0, log_dir=tmp_path, **PINNED)
    first.stop()
    second.stop()

    records = sorted(tmp_path.glob("*/*.ct"))
    assert len(records) == 2 and len(list(tmp_path.glob("*/output.log"))) == 2
    for record in records:
        kinds = [json.loads(line)["kind"] for line in record.read_text().splitlines()]
        assert kinds.count("session") == 1, record.name


def test_the_record_says_what_wrote_it_and_how_the_run_ended(tmp_path):
    tracker = CarbonTracker(epochs=5, monitor_epochs=2, verbose=0, log_dir=tmp_path, **PINNED)
    run_epochs(tracker, 2)

    lines = record(tmp_path)
    run = lines[0]
    assert (run["kind"], run["format"], run["frontend"]) == ("run", 1, "python")
    # How this process was started.
    assert run["command"] == getattr(sys, "orig_argv", run["command"])
    assert run["epochs"] == {"planned": 5, "monitor": 2, "before_prediction": 1}
    assert run["config"]["location"] == {"stated": "global"}
    assert (lines[1]["kind"], lines[2]["kind"]) == ("probe", "catalog")
    [outcome] = [line for line in lines if line["kind"] == "outcome"]
    assert (outcome["ended"], outcome["epochs_done"]) == ("monitored", 2)
    assert lines[-1]["kind"] == "session"
    assert not [line for line in lines if line["kind"] == "prediction"], "only what was measured"


@pytest.mark.parametrize(
    "steps, ended",
    [
        (lambda tracker: run_epochs(tracker, 3), "completed"),
        (lambda tracker: (run_epochs(tracker, 1), tracker.stop()), "interrupted"),
    ],
)
def test_the_outcome_follows_how_tracking_stopped(tmp_path, steps, ended):
    tracker = CarbonTracker(epochs=3, verbose=0, log_dir=tmp_path, **PINNED)
    steps(tracker)
    [outcome] = [line for line in record(tmp_path) if line["kind"] == "outcome"]
    assert outcome["ended"] == ended


# Reading runs back


def two_runs(log_dir):
    """Two pinned runs of two epochs each, and their sessions."""
    sessions = []
    for _ in range(2):
        tracker = CarbonTracker(epochs=2, verbose=0, log_dir=log_dir, **PINNED)
        run_epochs(tracker, 2)
        sessions.append(json.loads(record_lines(log_dir)[-1]))
    return sessions


def record_lines(log_dir):
    """The newest record's lines."""
    newest = max(log_dir.glob("*/*.ct"), key=lambda path: path.stat().st_mtime_ns)
    return newest.read_text().splitlines()


# AC/PY-38
def test_report_sums_the_session_lines(tmp_path):
    from carbontracker import Report, report

    sessions = two_runs(tmp_path)
    found = report(tmp_path)
    assert isinstance(found, Report)
    assert [run.ended for run in found.runs] == ["completed", "completed"]
    assert found.total.runs == 2
    assert found.total.energy_kwh == pytest.approx(sum(s["energy_kwh"] for s in sessions))
    assert found.total.emissions_g == pytest.approx(sum(s["emissions_g"] for s in sessions))
    assert [group.path for group in found.spans] == ["epoch"]
    assert found.spans[0].count == 4
    assert found.incomplete == [] and found.unreadable == []
    assert "Total (2 runs)" in str(found)
    assert found.csv().startswith("file,started,")
    assert found.table(spans=True).startswith("Span ")
    pickle.dumps(found)


# AC/PY-38
def test_report_names_what_it_left_out(tmp_path):
    from carbontracker import report

    two_runs(tmp_path)
    (tmp_path / "broken.ct").write_text("not json\n")
    [first, *_] = sorted(tmp_path.glob("*/*.ct"))
    newer = tmp_path / "newer.ct"
    newer.write_text(first.read_text().replace('"format":1,', '"format":2,', 1))
    found = report(tmp_path)
    assert found.total.runs == 2
    assert sorted(file.file for file in found.unreadable) == [str(tmp_path / "broken.ct"), str(newer)]
    assert all(file.reason for file in found.unreadable)


def rewrite(path, change):
    """Each line of the record at ``path``, through ``change``; ``None`` drops it."""
    lines = [change(json.loads(line)) for line in path.read_text().splitlines()]
    path.write_text("".join(json.dumps(line) + "\n" for line in lines if line is not None))


def without_emissions(value):
    """``value`` with every ``emissions_g`` absent, as a run with no intensity records it."""
    if isinstance(value, dict):
        return {key: None if key.endswith("emissions_g") else without_emissions(item) for key, item in value.items()}
    if isinstance(value, list):
        return [without_emissions(item) for item in value]
    return value


# AC/PY-38
def test_report_leaves_out_a_run_without_its_session_line(tmp_path):
    from carbontracker import report

    sessions = two_runs(tmp_path)
    [first, second] = sorted(tmp_path.glob("*/*.ct"), key=lambda path: path.stat().st_mtime_ns)
    rewrite(second, lambda line: None if line["kind"] == "session" else line)
    found = report(tmp_path)
    assert [run.file for run in found.incomplete] == [str(second)]
    assert found.total.runs == 1
    assert found.total.energy_kwh == pytest.approx(sessions[0]["energy_kwh"])


# AC/PY-28
# AC/PY-38
def test_absent_emissions_are_none_never_zero(tmp_path, capsys):
    from carbontracker import parser, report

    sessions = two_runs(tmp_path)
    [first, second] = sorted(tmp_path.glob("*/*.ct"), key=lambda path: path.stat().st_mtime_ns)
    rewrite(second, without_emissions)
    found = report(tmp_path)
    assert [run.emissions_g is None for run in found.runs] == [False, True]
    assert found.total.energy_kwh == pytest.approx(sessions[0]["energy_kwh"] + sessions[1]["energy_kwh"])
    assert found.total.emissions_g == pytest.approx(sessions[0]["emissions_g"])
    assert found.total.without_emissions == 1

    rewrite(first, without_emissions)
    found = report(tmp_path)
    assert found.total.emissions_g is None and found.total.without_emissions == 2
    assert found.spans[0].emissions_g is None
    energy, co2eq, _ = parser.aggregate_consumption(tmp_path)
    assert energy == pytest.approx(found.total.energy_kwh) and math.isnan(co2eq)


def cli_report(*arguments):
    result = subprocess.run(
        [sys.executable, "-m", "carbontracker.cli", "report", *map(str, arguments)],
        capture_output=True, text=True, check=False,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout


# AC/PY-39
def test_report_prints_what_the_cli_prints(tmp_path):
    from carbontracker import report

    two_runs(tmp_path)
    found = report(tmp_path)
    assert cli_report(tmp_path).startswith(str(found) + "\n")
    assert cli_report("--spans", tmp_path) == found.table(spans=True) + "\n"
    saved = tmp_path / "saved.csv"
    cli_report("--csv", "-o", saved, tmp_path)
    assert saved.read_text() == found.csv()
    cli_report("--csv", "--spans", "-o", saved, tmp_path)
    assert saved.read_text() == found.csv(spans=True)
    assert found.csv() == found.csv(spans=False)


# AC/PY-40
def test_report_on_a_missing_path_raises(tmp_path):
    from carbontracker import report

    with pytest.raises(FileNotFoundError):
        report(tmp_path / "nowhere")


# AC/PY-41
def test_the_2_4_7_parser_keeps_its_names(tmp_path, capsys):
    from carbontracker import parser, report

    two_runs(tmp_path)
    total = report(tmp_path).total
    energy, co2eq, equivalents = parser.aggregate_consumption(tmp_path)
    assert (energy, co2eq) == pytest.approx((total.energy_kwh, total.emissions_g))
    assert "km travelled by car" in equivalents
    parser.print_aggregate(tmp_path)
    assert capsys.readouterr().out.startswith("The training of models in this work is estimated to use")

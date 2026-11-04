"""``CarbonTracker``: the main tracker object.

    from carbontracker.tracker import CarbonTracker

    tracker = CarbonTracker(epochs=max_epochs)
    for epoch in range(max_epochs):
        tracker.epoch_start()
        # Your model training.
        tracker.epoch_end()
    session = tracker.stop()

Everything 2.4.7 did still works the same way, and prints the same, but
for the closing "Actual consumption:", which now covers the whole run. New
in 3.0.0: ``stop()`` returns a ``Session`` of plain data, ``with
CarbonTracker() as tracker:`` stops at the end of the block, spans measure
any part of your code (``with tracker.span("eval"):`` or
``@tracker.span("eval")``), ``@track`` measures a whole function,
``predict()`` predicts the run at any time, and a few keyword-only
arguments say where the grid is and what draw to assume.
"""

from __future__ import annotations

import atexit
import dataclasses
import functools
import inspect
import os
import sys
import traceback
import weakref
from typing import Callable, Dict, Optional, Tuple

from . import _native
from . import profiler as _profiler
from .types import Prediction, Session, _prediction, _session
from .spans import RunningSpan, Span, innermost, untracked

PUE_2023 = 1.58
"""Default PUE: the Uptime Institute 2023 global average."""

EPOCH = "epoch"
"""The name of each epoch's span, which the prediction is made from."""

class TrackingError(RuntimeError):
    """Raised when tracking fails and ``ignore_errors`` is not set."""


REMOVED = {
    "stop_and_confirm": "it waited on input(), which hangs a job with no terminal; ask after epoch_end() yourself",
    "sim_cpu": "state the CPU's draw with cpu_power=<watts>",
    "sim_cpu_tdp": "state the CPU's draw with cpu_power=<watts>",
    "sim_cpu_util": "state the CPU's draw, utilisation included, with cpu_power=<watts>",
    "sim_gpu": "state the GPU's draw with gpu_power=<watts>",
    "sim_gpu_watts": "state the GPU's draw with gpu_power=<watts>",
    "sim_gpu_util": "state the GPU's draw, utilisation included, with gpu_power=<watts>",
}
"""Arguments of 2.4.7 that are gone, and what to use instead."""


class CarbonTracker:
    """The main interface for starting, stopping and reporting through carbontracker.

    Args:
        epochs: total epochs of your training loop. ``None`` (new in 3.0.0)
            for code without epochs: then nothing is predicted.
        epochs_before_pred: epochs to monitor before printing the predicted
            consumption; -1 for all epochs, 0 for no prediction.
        monitor_epochs: epochs to monitor; prints the actual consumption when
            reached and stops tracking. -1 for all epochs.
        update_interval: seconds between measurements.
        interpretable: also say what the CO2eq is equivalent to (km by car).
        stop_and_confirm: removed in carbontracker 3; ``True`` is a
            ``TypeError``. It waited on ``input()``, which hangs a job with no
            terminal.
        ignore_errors: if tracking fails, stop tracking and let training go
            on; otherwise raise ``TrackingError``.
        components: ``"all"``, ``"cpu"``, ``"gpu"`` or ``"cpu,gpu"``.
        devices_by_pid: measure only the GPUs this process and its children
            run on.
        log_dir: where each run gets a folder of its own, holding its
            record (``.ct``) and its output (``output.log``); ``None``
            writes none.
        log_file_prefix: put before each run folder's name.
        verbose: 0 prints only warnings and errors; 1 (or more) prints
            carbontracker's output. ``output.log`` always holds the output,
            and the record every event and readout.
        decimal_precision: decimals in the consumption blocks.
        api_keys: ``{"electricitymaps": key}``. Wins over
            ``CARBONTRACKER_EM_KEY``, which wins over the key
            ``carbontracker init`` saved. Any other name is a ``ValueError``.
        zone: new in 3.0.0. The grid's Electricity Maps zone, e.g. ``"DK-DK2"``.
        country: new in 3.0.0. The grid's country, e.g. ``"DK"``.
        coordinates: new in 3.0.0. Where the host is, ``(latitude, longitude)``.
        global_: new in 3.0.0. Use the world-average grid; contacts no one.
            At most one of ``zone``, ``country``, ``coordinates`` and
            ``global_``; with none, the location is estimated from this IP.
        power: new in 3.0.0. The whole machine's draw in watts, instead of
            measuring.
        cpu_power: new in 3.0.0. The CPU's draw in watts; the rest is measured.
        gpu_power: new in 3.0.0. The GPU's draw in watts; the rest is measured.
        intensity: new in 3.0.0. A fixed grid intensity in gCO2eq/kWh; no
            network.
        pue: new in 3.0.0. Power usage effectiveness; 2.4.7's 1.58 by default,
            1 in profiler mode.
        mode: new in 3.0.0. ``"tracker"``, or ``"profiler"`` for short spans
            that run in order: electricity only, each span isolated by waiting
            for reads and subtracting the idle draw.
        sync_streams, wait, settle_s, subtract_baseline, baseline_w,
            baseline_s: new in 3.0.0, profiler mode only; see
            ``carbontracker.profiler.Settings``.

    Raises:
        ValueError: an invalid or conflicting argument, whatever
            ``ignore_errors`` says.
        TypeError: a ``sim_*`` argument, or ``stop_and_confirm=True``, which
            carbontracker 3 removed.
        TrackingError: tracking couldn't start, unless ``ignore_errors``.

    Example:
        Spans measure any part of your code, nested in the current epoch:

            tracker.epoch_start()
            with tracker.span("forward") as span:
                ...
                span.mark("samples", 64)
            tracker.epoch_end()
    """

    def __init__(
        self,
        epochs: Optional[int] = None,
        epochs_before_pred: int = 1,
        monitor_epochs: int = -1,
        update_interval: float = 1,
        interpretable: bool = True,
        stop_and_confirm: bool = False,
        ignore_errors: bool = False,
        components: str = "all",
        devices_by_pid: bool = False,
        log_dir: Optional[str] = None,
        log_file_prefix: str = "",
        verbose: int = 1,
        decimal_precision: int = 12,
        api_keys: Optional[Dict[str, str]] = None,
        *,
        zone: Optional[str] = None,
        country: Optional[str] = None,
        coordinates: Optional[Tuple[float, float]] = None,
        global_: bool = False,
        power: Optional[float] = None,
        cpu_power: Optional[float] = None,
        gpu_power: Optional[float] = None,
        intensity: Optional[float] = None,
        pue: Optional[float] = None,
        mode: str = "tracker",
        sync_streams: Optional[bool] = None,
        wait: Optional[bool] = None,
        settle_s: Optional[float] = None,
        subtract_baseline: Optional[bool] = None,
        baseline_w: Optional[float] = None,
        baseline_s: Optional[float] = None,
        **removed,
    ):
        for name in removed:
            if name in REMOVED:
                raise TypeError(f"{name} was removed in carbontracker 3: {REMOVED[name]}")
            raise TypeError(f"CarbonTracker() got an unexpected keyword argument {name!r}")
        if stop_and_confirm:
            raise TypeError(f"stop_and_confirm was removed in carbontracker 3: {REMOVED['stop_and_confirm']}")
        measuring = dict(
            sync_streams=sync_streams, wait=wait, settle_s=settle_s,
            subtract_baseline=subtract_baseline, baseline_w=baseline_w, baseline_s=baseline_s,
        )
        if mode not in ("tracker", "profiler"):
            raise ValueError(f"mode must be 'tracker' or 'profiler', got {mode!r}")
        self._profiling: Optional[_profiler.Settings] = None
        if mode == "tracker":
            for name, value in measuring.items():
                if value is not None:
                    raise ValueError(f"{name} is a profiler setting: give it with mode='profiler'")
        else:
            given = dict(
                epochs=epochs is not None, epochs_before_pred=epochs_before_pred != 1,
                monitor_epochs=monitor_epochs != -1, zone=zone is not None, country=country is not None,
                coordinates=coordinates is not None, global_=global_, intensity=intensity is not None,
                api_keys=api_keys is not None,
            )
            _profiler.refused([name for name, is_given in given.items() if is_given])
            self._profiling = _profiler.settings(measuring)
            # Electricity only: the world-average grid is offline, and nothing bills it.
            global_ = True
        if pue is None:
            # The device's own draw when profiling; the data centre's otherwise.
            pue = 1.0 if mode == "profiler" else PUE_2023
        if epochs is not None and epochs < 0:
            raise ValueError("epochs cannot be negative")
        if decimal_precision < 0:
            raise ValueError("decimal_precision cannot be negative")
        if intensity is not None and not intensity > 0:
            raise ValueError("intensity must be above zero gCO2eq/kWh")
        if api_keys is not None and not (
            isinstance(api_keys, dict) and all(isinstance(v, str) for v in [*api_keys, *api_keys.values()])
        ):
            raise ValueError('api_keys must be a dict of strings, e.g. {"electricitymaps": key}')
        if monitor_epochs != -1:
            if monitor_epochs < epochs_before_pred:
                raise ValueError("monitor_epochs cannot be less than epochs_before_pred")
            if monitor_epochs == 0:
                raise ValueError("monitor_epochs cannot be zero")
        elif epochs is not None:
            monitor_epochs = epochs

        self.epochs = epochs
        self.epochs_before_pred = epochs_before_pred
        self.monitor_epochs = monitor_epochs
        self.update_interval = update_interval
        self.interpretable = interpretable
        self.ignore_errors = ignore_errors
        self.components = components
        self.devices_by_pid = devices_by_pid
        self.log_dir = log_dir
        self.log_file_prefix = log_file_prefix
        self.verbose = verbose
        self.decimal_precision = decimal_precision
        self.api_keys = api_keys
        self._where = dict(zone=zone, country=country, coordinates=coordinates, global_=global_)
        self._stated = dict(power=power, cpu_power=cpu_power, gpu_power=gpu_power, intensity=intensity)
        self._pue = pue
        self.deleted = False
        self.epoch_counter = 0
        self.session: Optional[Session] = None
        """The ``Session``, once tracking has stopped."""

        self._tracking = False
        self._forked = False
        self._epoch: Optional[int] = None
        """The open epoch's span id."""
        self._native = None
        self._profiler: Optional[_profiler.Profiler] = None
        try:
            self._native = self._start()
            if self._profiling is not None:
                self._profiler = _profiler.Profiler(self._native, self._profiling)
                self._profiler.calibrate()
            self._tracking = True
            _live.add(self)
        except ValueError:
            raise
        except Exception as e:
            self._handle_error(e)

    def __enter__(self) -> CarbonTracker:
        return self

    def __exit__(self, *_exception) -> None:
        # Stopped even on an exception, which is not swallowed.
        self.stop()

    def epoch_start(self):
        """Starts tracking energy consumption for current epoch. Call in the beginning of training loop."""
        if self._profiling is not None:
            raise ValueError("a profiler run has no epochs: use spans")
        if self.epochs is None:
            raise ValueError("epoch_start() needs the number of epochs: CarbonTracker(epochs=...)")
        if self.deleted:
            return
        try:
            self._epoch = self._native.start_span(EPOCH, None)
        except Exception as e:
            self._handle_error(e)

    def epoch_end(self):
        """Ends tracking energy consumption for current epoch. Call in the end of training loop."""
        if self.deleted:
            return
        try:
            if self._epoch is None:
                raise RuntimeError("epoch_end() was called with no epoch open: call epoch_start() first")
            self._native.stop_span(self._epoch, [])
            self._epoch = None
            self.epoch_counter += 1
            if self.epoch_counter == self.monitor_epochs:
                self._delete("completed" if self.monitor_epochs == self.epochs else "monitored")
        except Exception as e:
            self._handle_error(e)

    def span(self, name: str, parent: Optional[RunningSpan] = None) -> Span:
        """New in 3.0.0. Measure part of your code as a named span.

        The span starts on each ``with``, each call of a decorated function,
        or ``start()``. It nests under ``parent`` if given, else the innermost
        span running here, else the open epoch.

        Args:
            name: shown in the log files, e.g. ``"eval"``. ``"epoch"`` is
                taken by the epochs.
            parent: the span to nest under, instead of the default.

        Raises:
            ValueError: for the name ``"epoch"``.
            RuntimeError: on starting, once tracking has stopped. A decorated
                function then runs untracked instead.
        """
        if name == EPOCH:
            raise ValueError(f"{EPOCH!r} is the epochs' span name; choose another")
        return Span(self, name, parent)

    def predict(self, epochs: Optional[int] = None) -> Optional[Prediction]:
        """New in 3.0.0. What the whole run is expected to cost, from the epochs
        measured so far, scaled to ``epochs`` (by default, the planned ones);
        ``None`` until an epoch is measured. The prediction printed after
        ``epochs_before_pred`` epochs is the session's; this one is made now.

        Raises:
            ValueError: without ``epochs`` here or on the tracker.
            RuntimeError: once tracking has stopped.
        """
        if self._profiling is not None:
            raise ValueError("a profiler run predicts nothing")
        epochs = epochs if epochs is not None else self.epochs
        if epochs is None:
            raise ValueError("predict() needs a number of epochs: predict(epochs) or CarbonTracker(epochs=...)")
        raw = self._native.predict(epochs, self.epoch_counter)
        return _prediction(raw) if raw["measured"] > 0 else None

    def stop(self) -> Optional[Session]:
        """Ensure that tracker is stopped and deleted. E.g. use with early
        stopping, where not all monitor_epochs have been run. Returns the
        ``Session``; calling it again returns the same one."""
        if self.deleted:
            return self.session
        try:
            # An open epoch is closed when the session stops.
            self._epoch = None
            self._delete("interrupted" if self.epochs is not None else "completed")
        except Exception as e:
            self._handle_error(e)
        return self.session

    def _start(self):
        """Probe this host and start measuring; the run prints what is measured."""
        native = _native.Tracker(
            zone=self._where["zone"],
            country=self._where["country"],
            coordinates=tuple(map(float, self._where["coordinates"])) if self._where["coordinates"] is not None else None,
            global_=self._where["global_"],
            **self._stated,
            components=self.components,
            interval=float(self.update_interval),
            pue=float(self._pue),
            api_keys=self.api_keys,
            devices_by_pid=self.devices_by_pid,
            log_dir=os.fspath(self.log_dir) if self.log_dir is not None else None,
            log_file_prefix=self.log_file_prefix,
            epochs=self.epochs,
            epochs_before_pred=self.epochs_before_pred,
            monitor_epochs=max(self.monitor_epochs, 0),
            verbose=self.verbose,
            decimal_precision=self.decimal_precision,
            interpretable=self.interpretable,
            profiler=self._profiling is not None,
            write=_writer(),
        )
        warning = native.config_warning()
        if warning is not None:
            print(f"{_native.PREFIX}{warning}", file=sys.stderr, flush=True)
        return native

    def _start_span(self, name: str, parent: Optional[RunningSpan]) -> RunningSpan:
        if self._forked:
            return untracked(self, name)
        if not self._tracking:
            raise RuntimeError("tracking has stopped")
        parent = parent or innermost(self)
        parent_id = parent.id if parent is not None else self._epoch
        if self._profiler is not None:
            id = self._profiler.start(name, parent_id)
        else:
            id = self._native.start_span(name, parent_id)
        return RunningSpan(self._native, self, id, name)

    def _stop_span(self, id: int, work) -> None:
        if self._profiler is not None:
            self._profiler.stop(id, work)
        else:
            self._native.stop_span(id, work)

    def _delete(self, ended: str):
        self._finished(self._native.stop(ended, self._epochs_done()))

    def _epochs_done(self) -> Optional[int]:
        return self.epoch_counter if self.epochs is not None else None

    def _finished(self, raw: dict) -> None:
        """Tracking has stopped: keep the session."""
        self.session = _session(raw)
        if self._profiler is not None:
            self.session = self._profiled(raw, self.session)
        self.deleted = True
        self._tracking = False
        _live.discard(self)

    def _profiled(self, raw: dict, session: Session) -> Session:
        """The session with each span name's report and no emissions; prints
        the table."""
        baseline = self._profiler.baseline(raw["spans"])
        spans = self._profiler.reports(raw["spans"], baseline)
        electricity = lambda usage: dataclasses.replace(usage, emissions_g=None)  # noqa: E731
        session = dataclasses.replace(
            session,
            total=electricity(session.total),
            claimed=electricity(session.claimed),
            unclaimed=electricity(session.unclaimed),
            per_source={id: electricity(usage) for id, usage in session.per_source.items()},
            intensities={},
            spans=spans,
            baseline_w=baseline,
        )
        if self.verbose > 0:
            sources = [f"{source.label} {source.fidelity}" for source in session.sources.values() if source.kind == "power"]
            write = _writer()
            for line in _profiler.table(_native.PREFIX, sources, baseline, self._profiler.calls):
                write(line)
        return session

    def _handle_error(self, error):
        if self.ignore_errors:
            err_str = "".join(traceback.format_exception(type(error), error, error.__traceback__))
            print(f"{_native.PREFIX}{_native.ignored_error(err_str)}", flush=True)
            # Stop monitoring but continue training.
            self.deleted = True
            self._tracking = False
            _live.discard(self)
            if self._native is not None:
                try:
                    self._finished(self._native.stop("failed", self._epochs_done()))
                except Exception:
                    pass
        else:
            raise TrackingError(f"tracking failed: {error}") from error


def track(function=None, /, **keywords):
    """New in 3.0.0. Track each call of ``function`` as one whole session.

    For a function called once, like ``train`` or ``main``: each call probes
    again and prints its own output. For a function called many times, use
    ``@tracker.span(...)`` inside one tracker instead.

    Args:
        function: the function, when used bare as ``@track``.
        **keywords: passed on to ``CarbonTracker``, as in
            ``@track(country="DK")``; not the epoch ones.

    The last call's ``Session`` is on the decorated function's ``session``.

    Example:
        @track(country="DK")
        def train():
            ...
    """

    def decorate(function):
        if inspect.iscoroutinefunction(function):

            @functools.wraps(function)
            async def tracked(*args, **kwargs):
                with CarbonTracker(**keywords) as tracker:
                    with tracker.span(function.__name__):
                        result = await function(*args, **kwargs)
                tracked.session = tracker.session
                return result

        else:

            @functools.wraps(function)
            def tracked(*args, **kwargs):
                with CarbonTracker(**keywords) as tracker:
                    with tracker.span(function.__name__):
                        result = function(*args, **kwargs)
                tracked.session = tracker.session
                return result

        tracked.session = None
        return tracked

    return decorate(function) if function is not None else decorate


_live: "weakref.WeakSet[CarbonTracker]" = weakref.WeakSet()
"""Trackers still running."""


@atexit.register
def _stop_all() -> None:
    """A tracker never stopped still finishes at exit, so its files are complete."""
    for tracker in list(_live):
        tracker.stop()


def _forked() -> None:
    """A forked child has the trackers but not their threads: it tracks
    nothing, and sends nothing to a queue no thread reads."""
    for tracker in list(_live):
        tracker._forked = True
        tracker._tracking = False
        tracker.deleted = True
    _live.clear()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_forked)


def _writer() -> Callable[[str], None]:
    """Where the run's lines go: ``tqdm.write`` when tqdm is in use, so its
    bars stay whole, else ``print``. Through Python either way, so they reach
    a notebook cell."""
    tqdm = sys.modules.get("tqdm")
    if tqdm is not None and hasattr(tqdm, "write"):
        return tqdm.write
    if tqdm is not None and hasattr(tqdm, "tqdm"):
        return tqdm.tqdm.write

    def write(line: str) -> None:
        print(line, flush=True)

    return write

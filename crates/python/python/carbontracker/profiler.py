"""Profiler mode: short spans that run in order, isolated by waiting for a
read at each boundary and subtracting the idle draw. A span that overlaps a
sibling neither waits nor stalls it.

    with CarbonTracker(mode="profiler") as tracker:
        with tracker.span("tokenize"):
            ...
"""

from __future__ import annotations

import math
import sys
import threading
import time
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

from .types import ProfilerSpanReport

BASELINE = "ct:baseline"
"""The idle span that measures the baseline; never reported."""

MEASURING = ("sync_streams", "wait", "settle_s", "subtract_baseline", "baseline_w", "baseline_s")
"""Profiler mode's own settings."""

REFUSED = {
    "epochs": "a profiler run has no epochs",
    "epochs_before_pred": "a profiler run predicts nothing",
    "monitor_epochs": "a profiler run has no epochs",
    "zone": "profiler mode measures electricity only",
    "country": "profiler mode measures electricity only",
    "coordinates": "profiler mode measures electricity only",
    "global_": "profiler mode measures electricity only",
    "intensity": "profiler mode measures electricity only",
    "api_keys": "profiler mode contacts no service",
}
"""Arguments profiler mode cannot use, and why."""


@dataclass(frozen=True)
class Settings:
    sync_streams: bool = True
    wait: bool = True
    settle_s: float = 0.0
    subtract_baseline: bool = True
    baseline_w: Optional[float] = None
    baseline_s: float = 1.0


def settings(given: Dict[str, object]) -> Settings:
    """The settings given, the rest defaulted. Raises ``ValueError`` for an
    invalid value."""
    chosen = Settings(**{name: value for name, value in given.items() if value is not None})

    def seconds(name: str, value: float, zero: bool) -> None:
        if not (isinstance(value, (int, float)) and math.isfinite(value) and (value >= 0 if zero else value > 0)):
            raise ValueError(f"{name} must be a number of seconds{'' if zero else ' above zero'}, got {value!r}")

    seconds("settle_s", chosen.settle_s, zero=True)
    seconds("baseline_s", chosen.baseline_s, zero=False)
    if chosen.baseline_w is not None and not (math.isfinite(chosen.baseline_w) and chosen.baseline_w >= 0):
        raise ValueError(f"baseline_w must be watts at or above zero, got {chosen.baseline_w!r}")
    if not chosen.subtract_baseline:
        for name in ("baseline_w", "baseline_s"):
            if given.get(name) is not None:
                raise ValueError(f"{name} sets the baseline, which subtract_baseline=False does not subtract")
    return chosen


@dataclass
class _Call:
    """One settled call of a span, for the table."""

    record: "_Record"
    duration_s: float
    waited_s: float
    dynamic_j: float
    gross_j: float
    over_s: float
    """The seconds its power is over: work time, or the window when no
    baseline is subtracted."""
    warnings: List[str]


@dataclass
class _Record:
    name: str
    parent: Optional[_Record]
    inside: float = 0.0
    """Seconds waited inside the span's window, its own and its descendants'."""
    late: List[str] = field(default_factory=list)
    opened: float = 0.0
    closed: Optional[float] = None
    """When the span's start and stop cuts were placed, by `time.perf_counter`."""

    def within(self, other: _Record) -> bool:
        """Whether this span is `other` or one of its descendants."""
        record: Optional[_Record] = self
        while record is not None:
            if record is other:
                return True
            record = record.parent
        return False


class Profiler:
    """One profiler run's spans: starts and stops them on reads, and turns
    the settled reports into a report per span name."""

    def __init__(self, native, chosen: Settings):
        self._native = native
        self.settings = chosen
        self._records: Dict[int, _Record] = {}
        self._open: List[_Record] = []
        self._lock = threading.Lock()
        self.calls: List[_Call] = []
        """Every settled call, in the order it started; set by `reports`."""

    def _overlapped(self, record: Optional[_Record], parent: Optional[_Record]) -> bool:
        """Whether an open span other than this one is neither its ancestor
        nor its descendant: a sibling running at the same time."""
        with self._lock:
            for other in self._open:
                if other is record or (parent is not None and parent.within(other)):
                    continue
                if record is not None and (record.within(other) or other.within(record)):
                    continue
                return True
        return False

    def calibrate(self) -> None:
        """Measure the baseline: an idle span, started and stopped on reads."""
        if self.settings.subtract_baseline and self.settings.baseline_w is None:
            span = self.start(BASELINE, None)
            time.sleep(self.settings.baseline_s)
            self.stop(span, [])

    def start(self, name: str, parent: Optional[int]) -> int:
        began = time.perf_counter()
        if self.settings.settle_s:
            time.sleep(self.settings.settle_s)
        outer = self._records.get(parent) if parent is not None else None
        if self.settings.wait and not self._overlapped(None, outer):
            span, _, after, late = self._native.start_span_paced(name, parent)
        else:
            span, after, late = self._native.start_span(name, parent), 0.0, []
        now = time.perf_counter()
        if outer is not None:
            # Settling and the wait before the start are inside the parent.
            outer.inside += now - began - after
        record = _Record(name, outer, inside=after, late=list(late), opened=now - after)
        with self._lock:
            self._records[span] = record
            self._open.append(record)
        return span

    def stop(self, span: int, work: List[Tuple[str, float]]) -> None:
        if self.settings.sync_streams:
            _synchronize()
        record = self._records.get(span)
        if self.settings.wait and not (record is not None and self._overlapped(record, record.parent)):
            waited, late = self._native.stop_span_paced(span, work)
        else:
            self._native.stop_span(span, work)
            waited, late = 0.0, []
        if record is not None:
            now = time.perf_counter()
            with self._lock:
                # Its open descendants close with it.
                for other in [other for other in self._open if other.within(record)]:
                    other.closed = now
                    self._open.remove(other)
            record.inside += waited
            record.late += late
            if record.parent is not None:
                record.parent.inside += record.inside

    def baseline(self, settled: List[dict]) -> Optional[float]:
        """The idle draw to subtract, in watts; ``None`` when none is."""
        if not self.settings.subtract_baseline:
            return None
        if self.settings.baseline_w is not None:
            return float(self.settings.baseline_w)
        for raw in settled:
            if raw["name"] == BASELINE and raw["id"] in self._records and raw["duration_s"] > 0:
                return raw["energy_j"] / raw["duration_s"]
        return None

    def _idle_shares(self) -> Dict[int, float]:
        """Per span, the share of its window that is its own, from 0 to 1.

        The engine splits a stretch evenly among the innermost spans open in
        it, so a span's share at each moment is the part of those that are
        it or its descendants. Alone, the share is 1."""
        spans = [(span, record) for span, record in self._records.items() if record.closed is not None]
        moments = sorted({at for _, record in spans for at in (record.opened, record.closed)})
        own = {span: 0.0 for span, _ in spans}
        for start, end in zip(moments, moments[1:]):
            active = [(span, record) for span, record in spans if record.opened <= start < record.closed]
            innermost = [record for _, record in active if not any(o is not record and o.within(record) for _, o in active)]
            for span, record in active:
                part = sum(1 for inner in innermost if inner.within(record))
                own[span] += (end - start) * part / len(innermost)
        return {
            span: own[span] / (record.closed - record.opened) if record.closed > record.opened else 1.0
            for span, record in spans
        }

    def reports(self, settled: List[dict], baseline: Optional[float]) -> Dict[str, ProfilerSpanReport]:
        """Each span name's report, from every settled span of this run."""
        sums: Dict[str, dict] = {}
        shares = self._idle_shares()
        self.calls = []
        for raw in settled:
            record = self._records.get(raw["id"])
            if record is None or record.name == BASELINE:
                continue
            window = raw["duration_s"]
            work = max(window - record.inside, 0.0)
            measured = raw["energy_j"]
            share = shares.get(raw["id"], 1.0)
            if baseline is None:
                dynamic = gross = measured
            else:
                dynamic = measured - baseline * window * share
                gross = dynamic + baseline * work * share
            warnings = raw["warnings"] + [f"not isolated: no fresh sample from source {s} in time" for s in record.late]
            self.calls.append(_Call(record, work, record.inside, dynamic, gross, work if baseline is not None else window, warnings))
            total = sums.setdefault(
                record.name,
                dict(calls=0, duration_s=0.0, window_s=0.0, waited_s=0.0, dynamic=0.0, gross=0.0, samples=None, warnings=[]),
            )
            total["calls"] += 1
            total["duration_s"] += work
            total["window_s"] += window
            total["waited_s"] += record.inside
            total["dynamic"] += dynamic
            total["gross"] += gross
            total["samples"] = raw["samples"] if total["samples"] is None else min(total["samples"], raw["samples"])
            for warning in raw["warnings"] + [f"not isolated: no fresh sample from source {s} in time" for s in record.late]:
                if warning not in total["warnings"]:
                    total["warnings"].append(warning)

        def per_second(energy: float, seconds: float) -> float:
            return energy / seconds if seconds > 0 else 0.0

        def over(total: dict) -> float:
            # Without a baseline the energy is the whole window's, waits included.
            return total["duration_s"] if baseline is not None else total["window_s"]

        return {
            name: ProfilerSpanReport(
                name=name,
                calls=total["calls"],
                duration_s=total["duration_s"],
                waited_s=total["waited_s"],
                dynamic_energy_j=total["dynamic"],
                gross_energy_j=total["gross"],
                dynamic_power_w=per_second(total["dynamic"], over(total)),
                gross_power_w=per_second(total["gross"], over(total)),
                samples=total["samples"] or 0,
                warnings=total["warnings"],
            )
            for name, total in sums.items()
        }


def _synchronize() -> None:
    """Wait for the accelerator's queued work, when PyTorch is loaded and CUDA
    is available."""
    torch = sys.modules.get("torch")
    cuda = getattr(torch, "cuda", None)
    if cuda is not None and cuda.is_available():
        cuda.synchronize()


def tree_labels(names: List[str]) -> List[str]:
    """Sibling names, numbered in order where two or more share one."""
    seen: Dict[str, int] = {}
    labels = []
    for name in names:
        seen[name] = seen.get(name, 0) + 1
        labels.append(f"{name} #{seen[name]}" if names.count(name) > 1 else name)
    return labels


def table(prefix: str, sources: List[str], baseline: Optional[float], calls: List[_Call]) -> List[str]:
    """The lines printed when a profiler run stops: every call, under its
    parent."""
    lines = [f"{prefix}profiler   {', '.join(sources) or 'nothing measured'}"]
    lines.append(f"{prefix}baseline   {baseline:.1f} W" if baseline is not None else f"{prefix}baseline   not subtracted")

    children: Dict[Optional[int], List[_Call]] = {}
    for call in sorted(calls, key=lambda call: call.record.opened):
        children.setdefault(id(call.record.parent) if call.record.parent is not None else None, []).append(call)
    rows: List[Tuple[str, _Call]] = []

    def walk(parent: Optional[int], indent: str) -> None:
        siblings = children.get(parent, [])
        for index, (call, label) in enumerate(zip(siblings, tree_labels([call.record.name for call in siblings]))):
            last = index == len(siblings) - 1
            branch = "" if parent is None else ("└─ " if last else "├─ ")
            rows.append((indent + branch + label, call))
            walk(id(call.record), indent if parent is None else indent + ("   " if last else "│  "))

    walk(None, "")
    width = max([16] + [len(label) + 2 for label, _ in rows])
    header = f"  {'Span':<{width}}{'Duration':>11}{'Waited':>11}{'Dynamic':>10}{'Gross':>10}{'Power':>9}"
    lines += ["", header, "  " + "-" * (len(header) - 2)]
    for label, call in rows:
        power = call.dynamic_j / call.over_s if call.over_s > 0 else 0.0
        lines.append(
            f"  {label:<{width}}{call.duration_s * 1e3:>8.1f} ms{call.waited_s * 1e3:>8.1f} ms"
            f"{call.dynamic_j:>8.2f} J{call.gross_j:>8.2f} J{power:>7.1f} W"
        )
        for warning in call.warnings:
            lines.append(f"  {' ' * width}{warning}")
    return lines


def refused(given: List[str]) -> None:
    """Raise ``ValueError`` for the first argument given that profiler mode
    cannot use."""
    for name in given:
        if name in REFUSED:
            raise ValueError(f"{name} cannot be given in profiler mode: {REFUSED[name]}")

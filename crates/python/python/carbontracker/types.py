"""What ``CarbonTracker.stop()`` gives back: plain, frozen dataclasses.

Just the data: no methods beyond what ``dataclass`` gives, and no handle to
the tracker, so ``dataclasses.asdict``, ``pickle`` and
``json.dumps(asdict(session), default=str)`` all work, and a ``Session``
outlives the tracker that made it.

Units are in the field names: energy in kWh, emissions in grams of CO2eq,
durations in seconds. Times are timezone-aware ``datetime``s in UTC.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional


@dataclass(frozen=True)
class Usage:
    energy_kwh: float
    emissions_g: Optional[float]
    """`None` where energy was drawn with no grid intensity to bill it against."""


@dataclass(frozen=True)
class Source:
    """One measured series: a device's power, or the grid's intensity."""

    id: str
    """The engine's source id, e.g. ``"7.0"``."""
    kind: str
    """``"power"`` or ``"intensity"``."""
    label: str
    """For people: ``"Apple M2 (gpu)"``, ``"grid DK-DK2"``."""
    fidelity: str
    """``"measured"``, ``"stated"``, ``"modelled"`` or ``"assumed"``."""


@dataclass(frozen=True)
class Prediction:
    """What the whole run is expected to cost. Not a measurement."""

    total: Usage
    """The whole run, measured part included."""
    duration_s: float
    remaining_s: float
    intensity_g_per_kwh: float
    """Mean over the remaining time."""
    measured: int
    """Spans averaged over."""
    of_total: float
    """The multiplier, e.g. the number of epochs."""
    warnings: List[str]


@dataclass(frozen=True)
class ProfilerSpanReport:
    """Profiler mode: every call of one span name, summed. Energy in joules,
    power in watts, over the work time ``duration_s``."""

    name: str
    calls: int
    duration_s: float
    """Work time: the span's window less the waits inside it."""
    waited_s: float
    """Waits for reads inside the span, its own and its descendants'."""
    dynamic_energy_j: float
    """Measured energy less the baseline over the window. May be slightly
    negative for a span that drew nothing above idle."""
    gross_energy_j: float
    """Dynamic energy plus the baseline over the work time."""
    dynamic_power_w: float
    gross_power_w: float
    samples: int
    """The fewest samples any waited-for source had in one call."""
    warnings: List[str]


@dataclass(frozen=True)
class Session:
    """What a tracker measured, from start to ``stop()``: the engine's own
    totals. Each span's detail is in the log files."""

    started_at: datetime
    duration_s: float
    total: Usage
    claimed: Usage
    """The part billed to spans."""
    unclaimed: Usage
    """The part no span claimed: time with no span running."""
    per_source: Dict[str, Usage]
    sources: Dict[str, Source]
    intensities: Dict[str, Source]
    """The intensity sources the power sources were billed against, by id;
    empty when only power is tracked."""
    location: Optional[str]
    """What probing knew about where the host is."""
    warnings: List[str]
    prediction: Optional[Prediction]
    """The prediction printed after ``epochs_before_pred`` epochs, if any."""
    spans: Dict[str, ProfilerSpanReport] = field(default_factory=dict)
    """Profiler mode: each span name's report; empty in tracker mode."""
    baseline_w: Optional[float] = None
    """Profiler mode: the idle draw subtracted; ``None`` when none is."""


def _time(seconds: Optional[float]) -> Optional[datetime]:
    return None if seconds is None else datetime.fromtimestamp(seconds, tz=timezone.utc)


def _usages(raw: Dict[str, dict]) -> Dict[str, Usage]:
    return {id: Usage(**usage) for id, usage in raw.items()}


def _prediction(raw: dict) -> Prediction:
    return Prediction(**{**raw, "total": Usage(**raw["total"])})


def _session(raw: dict) -> Session:
    return Session(
        started_at=_time(raw["started_at"]),
        duration_s=raw["duration_s"],
        total=Usage(**raw["total"]),
        claimed=Usage(**raw["claimed"]),
        unclaimed=Usage(**raw["unclaimed"]),
        per_source=_usages(raw["per_source"]),
        sources={id: Source(**source) for id, source in raw["sources"].items()},
        intensities={id: Source(**source) for id, source in raw["intensities"].items()},
        location=raw["location"],
        warnings=raw["warnings"],
        prediction=_prediction(raw["prediction"]) if raw["prediction"] is not None else None,
    )

"""``report(*paths)``: the runs recorded in ``.ct`` files, summed.

    from carbontracker import report

    found = report("logs/")
    found.total.energy_kwh
    print(found)                  # the runs table
    print(found.table(spans=True))
    found.csv()                   # one row per run

Only what runs measured is summed: a run without its session line is in
``incomplete`` and left out of ``total``. Units are in the field names.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional, Union

from . import _native


@dataclass(frozen=True)
class Run:
    """One run's record."""

    file: str
    started: datetime
    frontend: str
    """``"cli"`` or ``"python"``."""
    command: List[str]
    ended: Optional[str]
    """``"completed"``, ``"exited"``, ``"signalled"``, …; ``None`` if not recorded."""
    code: Optional[int]
    signal: Optional[int]
    duration_s: Optional[float]
    """``None`` for a run that never ended normally, as are energy and emissions."""
    energy_kwh: Optional[float]
    emissions_g: Optional[float]
    complete: bool


@dataclass(frozen=True)
class SpanGroup:
    """The settled spans at one path, e.g. ``"epoch/eval"``, across runs."""

    path: str
    count: int
    energy_kwh: float
    mean_kwh: float
    min_kwh: float
    max_kwh: float
    emissions_g: Optional[float]
    without_emissions: int
    """Spans whose energy had no grid intensity."""


@dataclass(frozen=True)
class Total:
    """The complete runs, summed."""

    runs: int
    duration_s: float
    energy_kwh: float
    emissions_g: Optional[float]
    """Over the runs with emissions; ``None`` if none had any."""
    without_emissions: int


@dataclass(frozen=True)
class Unreadable:
    file: str
    reason: str


@dataclass(frozen=True)
class Report:
    runs: List[Run]
    """In the order they started."""
    spans: List[SpanGroup]
    total: Total
    unreadable: List[Unreadable]
    _text: Dict[str, object] = field(repr=False, compare=False)

    @property
    def incomplete(self) -> List[Run]:
        """The runs left out of ``total``: no session line."""
        return [run for run in self.runs if not run.complete]

    def table(self, spans: bool = False) -> str:
        """The runs table, or the spans table."""
        return self._text["spans" if spans else "runs"]

    def csv(self, spans: bool = False) -> str:
        """One row per run, or per span group, at full precision."""
        return self._text["spans_csv" if spans else "runs_csv"]

    def __str__(self) -> str:
        return self.table()


def report(*paths: Union[str, os.PathLike]) -> Report:
    """The runs recorded in ``.ct`` files under ``paths``, folders searched
    recursively. Raises ``FileNotFoundError`` for a path that is not there."""
    raw = _native.report([os.fspath(path) for path in paths])
    runs = [
        Run(**{**run, "started": datetime.fromtimestamp(run["started"], tz=timezone.utc)}) for run in raw["runs"]
    ]
    return Report(
        runs=runs,
        spans=[SpanGroup(**group) for group in raw["spans"]],
        total=Total(**raw["total"]),
        unreadable=[Unreadable(**file) for file in raw["unreadable"]],
        _text=raw["text"],
    )

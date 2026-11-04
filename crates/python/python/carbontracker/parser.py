"""carbontracker 2.4.7's ``parser``, over the runs' records: the total and
its sentence for a paper. See ``carbontracker.report`` for the rest."""

from __future__ import annotations

import math
from typing import Dict, Tuple

from . import _native
from .report import report


def aggregate_consumption(log_dir) -> Tuple[float, float, Dict[str, float]]:
    """The complete runs under ``log_dir``: (kWh, gCO2eq, {unit: amount}).
    The CO2eq is NaN when no run had a grid intensity."""
    total = report(log_dir).total
    if total.emissions_g is None:
        return total.energy_kwh, math.nan, {}
    return total.energy_kwh, total.emissions_g, dict(_native.equivalents(total.emissions_g))


def print_aggregate(log_dir) -> None:
    """Prints the sentence for a paper, then what the total leaves out."""
    found = report(log_dir)
    print(found._text["citation"])
    for note in found._text["notes"]:
        print(note)

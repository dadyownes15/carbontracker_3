"""carbontracker: track and predict the energy use and carbon footprint of
training deep learning models.

    from carbontracker.tracker import CarbonTracker

The ``carbontracker`` command tracks a whole command from outside;
``report(*paths)`` sums the runs recorded.
"""

from . import parser
from .report import Report, report
from .types import Prediction, ProfilerSpanReport, Session, Source, Usage
from .spans import RunningSpan, Span
from .tracker import CarbonTracker, TrackingError, track

__all__ = [
    "CarbonTracker", "Prediction", "ProfilerSpanReport", "Report", "RunningSpan", "Session", "Source", "Span", "TrackingError", "Usage",
    "report", "track",
]

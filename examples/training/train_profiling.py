"""See which part of each epoch costs what, with spans.

    python train_profiling.py

The run is recorded in logs/.
"""

import time

from carbontracker import CarbonTracker, report

EPOCHS = 3


def busy(seconds):
    """Stand-in for real work: keep one core busy."""
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        pass


with CarbonTracker(epochs=EPOCHS, log_dir="logs") as tracker:
    for epoch in range(EPOCHS):
        tracker.epoch_start() ## or tracker.span("epoch")
        with tracker.span("train") as span:
            busy(0.6)
            span.mark("samples", 512)
        with tracker.span("eval"):
            busy(0.2)
        tracker.epoch_end()

print(report("logs").table(spans=True))

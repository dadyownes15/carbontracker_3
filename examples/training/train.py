"""The simplest way to track a run: decorate the function that does the work.

    python train.py

The run is recorded in logs/.
"""

import time

from carbontracker import track


def busy(seconds):
    """Stand-in for real work: keep one core busy."""
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        pass


@track(log_dir="logs")
def train():
    for epoch in range(3):
        busy(0.8)


train()
print(train.session.total)

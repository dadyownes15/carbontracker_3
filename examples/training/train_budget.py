"""Stop a run before it gets too expensive, by guarding on predict().

    python train_budget.py

The run is recorded in logs/.
"""

import time

from carbontracker import CarbonTracker

EPOCHS = 10
BUDGET_G = 0.003  # grams of CO2eq


def busy(seconds):
    """Stand-in for real work: keep one core busy."""
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        pass


tracker = CarbonTracker(epochs=EPOCHS, log_dir="logs")
for epoch in range(EPOCHS):
    tracker.epoch_start()
    busy(0.6)
    tracker.epoch_end()

    predicted = tracker.predict()  # the whole run, from the epochs so far
    if predicted and predicted.total.emissions_g and predicted.total.emissions_g > BUDGET_G:
        print(f"Stopping after {epoch + 1} epochs: predicted {predicted.total.emissions_g:.4f} g")
        break

tracker.stop()

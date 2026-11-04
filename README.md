# carbontracker

Carbontracker records the energy use and carbon emissions of machine learning,
and of any other program you run. The goal is to make it easy for researchers
to know what their experiments cost, without the tracking costing the
experiment anything.

Version 3 is a rewrite. The engine is now written in Rust instead of Python,
to make use of Rust's strengths in multithreading and memory management. It
runs on its own thread next to your code, and marking work never waits on it.
The Python API of carbontracker 2 is kept, so existing code runs unchanged.

The classic `epoch_start` and `epoch_end` are now just one kind of span. A
span can be any part of your code, so carbontracker is no longer limited to
training loops: an inference server, a data pipeline or a shell script can be
tracked the same way.

## Install

```sh
pip install --no-index --find-links https://github.com/dadyownes15/carbontracker_3/releases/expanded_assets/v3.0.0.dev0 carbontracker
```

| Requires | |
|---|---|
| Python | 3.9 or later |
| Linux | Intel CPUs (RAPL) and NVIDIA GPUs |
| macOS | Apple silicon |

On Linux, the CPU's energy files are readable by root only on most systems.
To measure the CPU as a normal user, run this once after each boot:

```sh
sudo chmod o+r /sys/class/powercap/intel-rapl:*/energy_uj
```

## Track a training loop

```python
from carbontracker.tracker import CarbonTracker

tracker = CarbonTracker(epochs=max_epochs, log_dir="logs")

# Training loop.
for epoch in range(max_epochs):
    tracker.epoch_start()

    # Your model training.

    tracker.epoch_end()

# Optional: Add a stop in case of early termination before all monitor_epochs has
# been monitored to ensure that actual consumption is reported.
session = tracker.stop()
print(session.total)
```

#### Example output

Before the first epoch, it says what it measures and how:

```
CarbonTracker: gathering sources…
CarbonTracker: power      Apple M2 Pro (system, cpu, gpu, ane, ram)  measured
CarbonTracker: intensity  DK                                         modelled by country-average
CarbonTracker: forecast   DK, next 24 h                              modelled by projection
CarbonTracker: ready. Tracking started.
```

After the first epoch, it predicts the whole run:

```
CarbonTracker: 
Predicted consumption for 10 epoch(s):
	Time:	0:00:30
	Energy:	0.000208511156 kWh
	CO2eq:	0.029879736158 g
	This is equivalent to:
	0.000279772810 km travelled by car
CarbonTracker: note: predicted from 1 epoch, so it can't see how much epochs vary; epochs_before_pred=3 gives a steadier estimate
CarbonTracker: note: no forecast is published for this grid, so the current intensity is held forward
```

When training ends, it prints what was measured:

```
CarbonTracker: 
Actual consumption:
	Time:	0:00:30
	Energy:	0.000276530224 kWh
	CO2eq:	0.039626897207 g
	This is equivalent to:
	0.000371038363 km travelled by car
CarbonTracker: grid DK, 143 gCO2eq/kWh avg (modelled)
CarbonTracker: logs in logs/run-20261104_023021-6spnoumh
CarbonTracker: WARNING - part of this total is an estimate, not measured on this machine: modelled grid DK
CarbonTracker: Finished monitoring.
```

`stop()` returns the same numbers, so `print(session.total)` gives:

```
Usage(energy_kwh=0.00027653022375851463, emissions_g=0.03962689720728912)
```

## See which part costs what

```python
from carbontracker import CarbonTracker, report

with CarbonTracker(epochs=3, log_dir="logs") as tracker:
    for epoch in range(3):
        tracker.epoch_start()
        with tracker.span("train") as span:
            train_one_epoch()
            span.mark("samples", 512)
        with tracker.span("eval"):
            evaluate()
        tracker.epoch_end()

print(report("logs").table(spans=True))
```

```
Span         Count   Total    Mean     Min     Max       CO2eq
epoch/train      3  60.8 J  20.3 J  12.5 J  25.6 J   0.00242 g
epoch/eval       3  21.5 J  7.16 J  6.19 J  7.96 J  0.000855 g
epoch            3  82.3 J  27.4 J  18.7 J  33.5 J   0.00328 g
```

## Track one function

```python
from carbontracker import track

@track(country="DK")
def fine_tune():
    ...

fine_tune()
print(fine_tune.session.total)   # Usage(energy_kwh=..., emissions_g=...)
```

## Stop before a run gets too expensive

```python
tracker = CarbonTracker(epochs=100)
for epoch in range(100):
    tracker.epoch_start()
    ...
    tracker.epoch_end()
    predicted = tracker.predict()   # whole run, from the epochs so far; None before the first
    if predicted and predicted.total.emissions_g and predicted.total.emissions_g > budget_g:
        break
tracker.stop()
```

## Track a server

```python
from fastapi import FastAPI
from carbontracker import CarbonTracker

tracker = CarbonTracker(log_dir="logs")
app = FastAPI()

@app.post("/chat")
@tracker.span("chat")
async def chat(prompt: str):
    return await llm.generate(prompt)

@app.post("/embed")
@tracker.span("embed")
async def embed(text: str):
    return await encoder.embed(text)
```

Each request is a span of its own. Requests that run at the same time share
the energy used while they overlap.

```sh
carbontracker report --spans logs
```

```
Span   Count   Total     Mean       Min      Max       CO2eq
embed    200  22.6 J  0.113 J  0.0104 J  0.290 J  0.000901 g
chat      40   172 J   4.31 J    3.84 J   4.81 J   0.00686 g
```

## From the command line

Installing the package also installs a `carbontracker` command. Put it in
front of any command; no code changes needed. Carbontracker writes to stderr,
so the command's own output and exit code pass through untouched.

```sh
carbontracker --country DK python train.py
```

```
CarbonTracker: gathering sources…
CarbonTracker: power      Apple M2 Pro (system, cpu, gpu, ane, ram)  measured
CarbonTracker: intensity  DK                                         modelled by country-average
CarbonTracker: forecast   DK, next 24 h                              modelled by projection
CarbonTracker: ready. Starting python train.py
...
CarbonTracker: python train.py summary:
	Time:       0:00:03
	Energy:     0.0000240 kWh
	Intensity:  143 gCO2eq/kWh avg, DK (modelled)
	CO2eq:      0.00345 g
	This is equivalent to:
	0.0000323 km travelled by car
	Logs:       ./logs/run-20261104_020815-ymdwd6xg
CarbonTracker: WARNING - part of this total is an estimate, not measured on this machine: modelled grid DK
```

| Command | Does |
|---|---|
| `carbontracker <command>` | Track a whole command |
| `carbontracker check` | Show what would be measured on this machine |
| `carbontracker init` | Save an Electricity Maps key for live grid data |
| `carbontracker report logs` | Sum the recorded runs; `--spans` groups them by span, `--csv` saves a spreadsheet |

### Set up a machine

`init` asks for your [Electricity Maps](https://www.electricitymaps.com) key,
saves it to `~/.config/carbontracker/config.toml` (readable only by you), and
then runs `check`. Without a key, carbontracker uses the country's average
intensity.

```sh
carbontracker init
```

```
Electricity Maps API key:
```

`check` shows what each value would be on this machine and how it is
obtained, without starting a run:

```sh
carbontracker check
```

```
  power      live       cpu, gpu, ane, ram, system
  location   live       DK (55.77, 12.50), from this IP
  intensity  modelled   country average for DK
  forecast   modelled   current intensity held flat, 24 h
  key        none for electricity_maps; run `carbontracker init`
  providers  ioreport, macmon, ip-geolocation, country-average, projection
```

| Word | Means |
|---|---|
| `live` | Measured on this machine, or fetched from a live service |
| `stated` | A value you gave |
| `modelled` | Taken from built-in data, such as a country's average |
| `fallback` | A rough default, used when nothing better is available |

A key that is set but not accepted is warned about below the rows. `check -v`
lists every provider and whether it was used.

## Report for a paper

```sh
carbontracker report logs
```

```
Started (UTC)     Command          Duration         Energy      CO2eq  Ended
2026-11-04 01:08  python train.py   0:00:03  0.0000240 kWh  0.00345 g  exited 0

Total (1 run)                       0:00:03  0.0000240 kWh  0.00345 g

The training of models in this work is estimated to use 0.0000240 kWh of electricity contributing to 0.00000345 kg of CO2eq. This is equivalent to 0.0000323 km travelled by car. Measured by carbontracker (https://github.com/saintslab/carbontracker).
```

Only measured runs are summed, never predictions. From Python:
`report("logs")`, or carbontracker 2's `parser.print_aggregate("logs")`.

Each run leaves a folder in `logs/` with its record (`run-<id>.ct`) and what
it printed (`output.log`). For a spreadsheet, group the spans and save CSV:

```sh
carbontracker report --spans --csv logs
```

This saves `logs/spans.csv` next to the runs, replacing an older one:

```
path,count,energy_kwh,mean_kwh,min_kwh,max_kwh,emissions_g,without_emissions
epoch/train,3,0.00001689463138712686,0.000005631543795708954,0.0000034760707161845356,0.0000071061038781346676,0.002421007773520462,0
epoch/eval,3,0.000005969336964200766,0.000001989778988066922,0.000001719212480422269,0.0000022106292557643888,0.0008554084940914949,0
epoch,3,0.00002286499510523428,0.000007621665035078094,0.000005195618597756864,0.000009317112254063062,0.0032765634018780175,0
```

Without `--spans`, `--csv` saves one row per run to `logs/runs.csv`. `-o FILE`
saves to another path.

## Options

| | Python | CLI |
|---|---|---|
| Grid location (default: from IP) | `zone=`, `country=`, `coordinates=`, `global_=True` | `--zone`, `--country`, `--coordinates`, `--global` |
| Fixed draw instead of measuring | `power=`, `cpu_power=`, `gpu_power=` | `--power`, `--cpu-power`, `--gpu-power` |
| Fixed intensity, no network | `intensity=` | `--intensity` |
| Live grid data | `api_keys={"electricitymaps": KEY}` | `carbontracker init` |

## Profiler mode

For short stages that run one after another, such as the steps of a
pipeline. Each span waits for a fresh power reading at its start and end, so
its neighbours' energy never mixes into it. The idle draw is measured once at
the start and subtracted. The arguments below are the defaults.

```python
from carbontracker import CarbonTracker

with CarbonTracker(
    mode="profiler",
    wait=True,
    sync_streams=True,
    settle_s=0.0,
    subtract_baseline=True,
    baseline_w=None,
    baseline_s=1.0,
) as tracker:
    for item in batch:
        with tracker.span("pipeline"):
            with tracker.span("tokenize"):
                tokens = tokenizer(item)
            with tracker.span("forward"):
                out = model(tokens)
```

With two items in the batch:

```
CarbonTracker: profiler   Apple M2 Pro (ane) measured, Apple M2 Pro (gpu) measured, Apple M2 Pro (ram) measured, Apple M2 Pro (cpu) measured
CarbonTracker: baseline   0.3 W

  Span               Duration     Waited   Dynamic     Gross    Power
  -------------------------------------------------------------------
  pipeline #1        350.5 ms    66.5 ms    1.76 J    1.86 J    5.0 W
  ├─ tokenize         50.2 ms     8.5 ms    0.10 J    0.11 J    1.9 W
  └─ forward         300.1 ms    12.8 ms    1.63 J    1.73 J    5.4 W
  pipeline #2        350.1 ms    15.0 ms    2.03 J    2.14 J    5.8 W
  ├─ tokenize         50.0 ms     1.7 ms    0.29 J    0.30 J    5.8 W
  └─ forward         300.0 ms     1.9 ms    1.73 J    1.83 J    5.8 W
```

Each call is one row, under its parent. Siblings with the same name are
numbered. Dynamic energy is above idle; gross adds the idle draw back.
`tracker.session.spans` holds the same numbers summed per name.

### Baseline and waiting

- **Baseline.** Before the first span, the profiler measures the idle draw
  for `baseline_s` (1 s) and subtracts it from every span. What is left is
  the energy the work itself added.
- **Waiting.** A power counter only updates every so often. Without waiting,
  two short spans inside one update get the same average and blur together.
  With waiting, each span starts and ends on a fresh reading. That costs up to
  one update interval per span, shown as `Waited`.

From our runs:

| Setup | Result |
|---|---|
| Apple M2 Pro, a 0.4 s idle span right after a 0.4 s busy one | Without waiting 2.66 W, as the busy span's energy leaked in. With waiting 0.43 W, the true idle draw |
| Apple M2 Pro, 0.15 s busy stages | Waiting for one reading: 24 % low and 30 % high in two runs. Waiting for two, as the profiler does: within 1 % of the stage repeated alone |
| NVIDIA, a simulated pipeline of four stages in 0.9 s | Without waiting 54 % off per stage on average. With waiting 13 %, in 3.9 s instead of 0.9 s |

### Which mode

| | Tracker mode | Tracker mode, stage repeated | Profiler mode |
|---|---|---|---|
| Good for | Epochs, whole runs, servers | One short stage that can run alone | Short stages that must run in order |
| Extra time | None | The repeats | Up to one counter read per span |
| Short spans | Blur into each other | Sharp, after dividing by the repeats | Sharp |
| Reports | Energy and emissions | Energy and emissions | Joules and watts only |
| Spans | Any, may overlap | One stage at a time | One after another; concurrent spans don't wait |

A short span is one shorter than the slowest counter's update interval: about
10 ms on Intel RAPL, 100 ms on Apple silicon, 1 s for NVIDIA's energy
counter.

### Options

| Option | Default | Does |
|---|---|---|
| `wait` | `True` | Wait for a fresh read at each span's start and end. `False` never waits, and short spans blur as in tracker mode. Concurrent spans, such as two requests at once, never wait |
| `sync_streams` | `True` | Wait for queued CUDA work to finish before a span ends, when PyTorch uses CUDA |
| `settle_s` | `0.0` | Idle this long before each span, so every call starts from idle clocks. Slower, steadier |
| `subtract_baseline` | `True` | Subtract the idle draw. `False` reports everything measured in the span's window |
| `baseline_w` | `None` | A known idle draw in watts. `None` measures it at the start |
| `baseline_s` | `1.0` | How long the idle draw is measured at the start. Longer is steadier |

Profiler mode measures the components only (CPU, GPU, RAM, neural engine),
never a whole-system total, and contacts no network service. Other programs'
load on the same devices counts towards a span, so profile on a quiet
machine. `epoch_start()` and `predict()` are not available in it.

## License

MIT. See [LICENSE](LICENSE).

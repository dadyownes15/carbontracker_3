# The default registry — `carbontracker-providers`

Probes this host and decides what to measure. One of several possible provider
sets; a Slurm or container tracker brings its own.

## Coverage slots

**A source answers one question, and only one source answers each question.**
Two sources answering the same one would bill the same energy twice. <!-- AC/PROV-1 -->

For power the question is which device, which part of it, and whose use is
being measured: the whole machine, one process, one job. For grid intensity it
is which grid.

So a machine with one processor and two graphics cards has three power
questions, one per device. A candidate that reads graphics cards answers two of
them; one that reads processors answers the third. Neither can answer a
question another candidate has already answered, so no device is read twice.

**A candidate is not probed when every domain it reads is already answered.**
Devices are named only once a candidate is probed, and two candidates can name
one chip differently, so before probing the domain is the only safe test. A
second card that no earlier candidate reads is therefore left unmeasured, and
the candidate order puts the candidates that read every card first. <!-- AC/PROV-48 -->

**Measuring one device at two scopes is two questions, not one.** A
whole-device reading and a job-scoped reading of the same card would both be
kept, and both billed. Keeping one source per question does not prevent that.
What prevents it is that a tracker measuring job-scoped use brings a provider
set of its own rather than joining this one. <!-- AC/PROV-2 -->

## Candidates and chains

**What the caller asks for decides which candidates there are; the chain only
chooses between the ones left.** A stated figure never outranks a real
measurement: unless it was asked for, it is not a candidate at all. <!-- AC/PROV-3 -->

**Stating a figure ends the search for it.** Giving the machine's draw means
power is not probed. Giving the processor's or the graphics card's draw settles
that part and leaves the rest to be probed. Giving the grid's intensity settles
both the intensity and the forecast. Giving the location means nothing is sent
to the geolocation service. Naming which parts to measure drops the rest before
any candidate runs. <!-- AC/PROV-4 -->

**Whatever is left is tried best first.**

| Chain | Order, once what was stated is out |
|---|---|
| Power | RAPL (Linux) → NVML energy → NVML power → IOReport (macOS) → macmon (macOS) → powermetrics (macOS) → generic CPU | <!-- AC/PROV-5 -->
| Location | IP geolocation, else none, meaning the global average | <!-- AC/PROV-6 -->
| Intensity | Electricity Maps → country average → global average (475 gCO2e/kWh) | <!-- AC/PROV-7 -->
| Forecast | Electricity Maps forecast → the current intensity held flat | <!-- AC/PROV-8 -->

**Every chain ends in a candidate that cannot fail**, so probing always
produces an answer, at worst a modelled average rather than a reading. A run
therefore always measures something. <!-- AC/PROV-9 -->

**An intensity or forecast candidate reads the location in its own terms, or
declines.** Each describes the grid it actually measures, which is not always
the grid that was asked for. <!-- AC/PROV-10 -->

## Probing

`ProviderRegistry::new(ProbeConfig)` is checked; `probe()` answers a
`Resolution`. `probe_with(callback)` reports each `ProbeProgress` step as it
happens. **The registry never prints**: progress goes to the callback. <!-- AC/PROV-11 -->

`ProbeConfig` is `measure` (which domains to cover), `grid` (location and API
keys) and `pinned` (stated power and intensity, which skip probing entirely).

A `Resolution` says what was chosen and what was not, with a `Verdict` per
`CandidateOutcome`, for a frontend to show. `resolve()` turns it into the
`Sources` a tracker starts over and the `Forecaster` for the grid it bills. <!-- AC/PROV-12 -->

**A registry builds providers because probing must read.** A probe confirms
that a candidate can answer by reading it once. The provider it builds keeps
whatever the probe set up or learned: a connection, a sampler thread, a driver
handle, or the answer it just fetched, which answers its first read. <!-- AC/PROV-13 -->

**A registry hands over a provider only for the sources it kept.** A provider
the chain kept nothing of is dropped. One the chain kept some sources of
answers for those alone: macmon, when NVML has taken the GPU, answers for its
CPU, ANE, RAM and system, and not the GPU. <!-- AC/PROV-14 -->

Pinned power and intensity make a run fully offline and deterministic: nothing
is probed and nothing is contacted. Every test above the provider level uses
this. <!-- AC/PROV-15 -->

## The forecast

**The forecast is not a source.** It is never billed, so the engine never sees
it. The registry resolves it next to the sources, for the grid the run is
billed against: the Electricity Maps forecast, otherwise the resolved
intensity held flat. <!-- AC/PROV-16 -->

| `Forecaster` | |
|---|---|
| `location()` | The grid it forecasts, which is always the billed one | <!-- AC/PROV-17 -->
| `fidelity()` | `Measured` for a published forecast, `Modelled` for a projection, `Stated` when pinned, `Assumed` for the global average held flat | <!-- AC/PROV-18 -->
| `horizon()` | How far ahead the curve reaches, which can be shorter than asked for | <!-- AC/PROV-19 -->
| `served_by()` | The candidate behind it, e.g. `projection` | <!-- AC/PROV-20 -->
| `latest()` | The latest curve, at once: `None` before a fetch has finished, `Some(None)` if no fetch has found a curve yet (a later failed fetch keeps the last curve); a stale or missing curve is fetched again in the background | <!-- AC/PROV-21 -->
| `ask(reply)` | Fetches the curve; the `ForecastReport` arrives on `reply`, or `None` | <!-- AC/PROV-22 -->

**Asking never blocks the asker.** The fetch runs on a thread of its own and is
answered within 10 s, with no curve if it took longer. **Asking again within a
minute answers the last curve**, so the forecaster and the intensity provider
do not spend one key's allowance between them; the probe's fetch counts as the
first. <!-- AC/PROV-23 --> <!-- AC/PROV-24 -->

## The static provider

**`StaticProvider` serves sources whose value never changes:** a stated draw
or intensity, or a modelled figure such as a country average. It is exported,
so any provider set can use it. Each source has one value, given when it is
built; its description says where the value came from. Its cooldown is
`Never`: its first read reports each value, stamped at that read, and the
series holds it from then on. Its read never fails. <!-- AC/PROV-25 -->

## The default providers

Each meets "The provider port" in [02](02-engine.md). A cooldown's evidence is
a measurement on named hardware, not a guess.

| Provider | Sources | `SampleKind` | Cooldown |
|---|---|---|---|
| RAPL | Each CPU package, and each DRAM zone | `Total` | 10 ms | <!-- AC/PROV-26 -->
| NVML energy | Each GPU with an energy counter | `Total` | 1 s | <!-- AC/PROV-27 -->
| NVML power | Each GPU without one | `Instant` | 100 ms | <!-- AC/PROV-28 -->
| IOReport | CPU, GPU, ANE and RAM | `Total` | 10 ms | <!-- AC/PROV-49 -->
| macmon | CPU, GPU, ANE, RAM and system | `Instant` | One sampler window, 100 ms: a read sooner can only answer `Unchanged` | <!-- AC/PROV-29 -->
| powermetrics | CPU, GPU and ANE | `Instant` | Zero; each read is a window | <!-- AC/PROV-30 -->
| Electricity Maps | The intensity of one zone | `Held` | 1 min | <!-- AC/PROV-31 -->
| `StaticProvider` | Stated power and intensity, generic CPU, country and global averages | `Instant` or `Held` | `Never` | <!-- AC/PROV-32 -->

**A counter going down is a wrap only when the counter's range is known.**
Otherwise the energy since the last read is unknown, and that read is
`Failed(Invalid)`. <!-- AC/PROV-33 -->

### RAPL

**It reads the powercap counters of each CPU package and DRAM zone,** leaving
out `psys`, which overlaps both. Reading `energy_uj` needs root on most
systems; without it, the probe declines, and a later refusal is `Denied`. <!-- AC/PROV-34 -->

**It relies on the watchdog to see every wrap.** `energy_uj` counts up to
`max_energy_range_uj` and starts again from zero, about every 20 minutes at
200 W with a 262 kJ range. The watchdog reads it at least once per
`readout_interval`, far more often, so each read sees at most one wrap and
counts it. A zone with no `max_energy_range_uj`, or a counter that restarts
when a CPU goes offline, is `Failed(Invalid)`. <!-- AC/PROV-35 -->

**Its cooldown is 10 ms.** The counter updates about every millisecond, and
irregularly. Read 1 ms apart, 4.6% of reads are stale; read 0.5 ms apart,
energy deviates by 6.1%; from 10 ms on, by 0.57% on average. <!-- AC/PROV-36 -->

- Intel SDM Vol. 3B, `MSR_PKG_ENERGY_STATUS`: updated about every 1 ms.
- Linux `amd_energy` documentation: updated every 1 ms, 15.3 µJ units.
- Dauner et al., *Evaluating the Influence of Measurement Frequency on Energy
  Readings Using Intel RAPL and NVIDIA NVML*, HotCarbon 2026: Xeon Gold 6326
  and Ryzen 9 5950X, gaps from 0.5 ms to 1 s.

### NVML

**One look at the driver splits the GPUs by what they report,** into two
candidates: `nvml-energy` reads a GPU with an energy counter (Volta and newer)
as a `Total`; `nvml-power` reads one without as an `Instant` from its power
reading. A GPU with no power reading at all is not served. When the run
measures given processes, only GPUs running one of them are served. <!-- AC/PROV-37 -->

**Its energy counter's cooldown is 1 s.** NVIDIA does not document how often
the counter updates; it is measured never to update faster than about every
100 ms. Read 10 ms apart, an RTX 4090 under-reports energy by 75.6%; 100 ms
apart, by 13.3%; 1 s apart, by 1.6%. An A100 matches its power readings within
0.2%. The counter does not wrap; one that restarts, as after a driver reload,
is `Failed(Invalid)`. <!-- AC/PROV-38 -->

**Its power reading's cooldown is 100 ms, and it is stamped at the middle of
its window.** Pascal and Volta update it about every 20 ms, Turing and newer
about every 100 ms. On Ampere and newer, except the GA100, it is the average
over the last second, so it is stamped half a second before the read; on
older GPUs it is instantaneous, and stamped at the read. <!-- AC/PROV-39 -->

- NVIDIA, `nvml.h`, `nvmlDeviceGetPowerUsage`: averaged over 1 s on Ampere
  (except GA100) and newer; instantaneous on GA100 and older.
- Yang et al., *Accurate and Convenient Energy Measurements for GPUs*, arXiv
  2312.02741: update periods and averaging windows across more than 70 GPUs.
- Dauner et al., HotCarbon 2026: energy counter staleness and bias on A100,
  RTX 4090, RTX 5060 Ti and RTX 6000.

### IOReport

**It reads Apple Silicon's IOReport energy model as running totals, with no
sudo and no thread of its own.** Each read takes one snapshot of the energy
model's channels it uses and reports, per domain, the energy since its first
read, stamped at the read. <!-- AC/PROV-50 -->

**Each domain sums its own channels.** CPU is every channel ending in `CPU
Energy`, one per die on an Ultra; GPU is `GPU Energy`; ANE the channels
starting `ANE`; RAM those starting `DRAM`. Channels counted inside these, such
as `GPU SRAM` or a core cluster's, are left out. A chip with none of them is
not served. <!-- AC/PROV-51 -->

**Its cooldown is 10 ms.** A snapshot of the channels it uses takes a few
milliseconds, and the totals move at every snapshot. <!-- AC/PROV-52 -->

**macmon comes after it, and then serves only the system rail:** the SMC's
`PSTR` reading, which IOReport has no counter for.

- Measured on one M2 Pro, macOS 26.2: over 1066 snapshots about 3 ms apart the
  CPU total never decreased and moved at every one, in steps of 1 mJ; a
  snapshot of the whole energy model took 1.5 ms. To be confirmed on more
  machines.

### macmon

**It reads Apple Silicon's IOReport energy model, on a sampler thread it
owns.** The sampler takes back-to-back 100 ms windows and queues them. A read
takes every window queued since the last one, or answers `Unchanged` when none
is. <!-- AC/PROV-40 -->

**CPU, GPU, ANE and RAM are energy over the window; system is not.** Each of
the first four is stamped at the window's middle. System is the SMC's `PSTR`
power reading, taken as the window ends, and is stamped there. <!-- AC/PROV-41 -->

**Its cooldown is zero: the sampler sets the pace.** The windows are 100 ms
because sampling is not free: a 10 ms sampler adds about 0.3–0.4 W of CPU
draw, a 100 ms one at most about 0.05 W. <!-- AC/PROV-42 -->

- Measured on one M2 Pro, macOS 26.2, macmon 0.8.2, no published source.
  Energy over 20 s agreed within 3% idle and 0.4% under load between 10 ms,
  100 ms and 1 s windows. To be confirmed on more machines.

### powermetrics

**It runs one `powermetrics` window per read,** through `sudo` without a
password prompt; without it, the probe declines, and a later refusal is
`Denied`. Each read takes about 1.5 s and reports a 1 s window, stamped at
its middle. It is the Intel-Mac fallback, where IOReport has no power channel. <!-- AC/PROV-43 -->

- Apple, `man powermetrics`: average power values over the sample interval.
- Measured on one M2 Pro, macOS 26.2: `-n1 -i 1000` reports a 1004 ms window
  and takes 1.5 s to run.

### Electricity Maps

**It reads the latest carbon intensity of one zone.** Each sample is stamped
with the time Electricity Maps gives for the measurement. An answer with the
same time as the last one is `Unchanged`. The probe's confirming fetch answers
its first read. <!-- AC/PROV-44 -->

**Its cooldown is 1 min.** Electricity Maps publishes far less often, and a
faster rate would spend the API's request allowance for nothing. A rejected
key is `Denied`; a refusal for asking too often is `RateLimited`. <!-- AC/PROV-45 -->

### The static figures

**Every fixed figure is a `StaticProvider`.** A power or intensity the user
stated is `Stated`. Generic CPU, the TDP of the host's CPU model at an assumed
half load, and the country average, from the country table, are `Modelled`.
The global average, 475 gCO2e/kWh, is `Assumed`. <!-- AC/PROV-46 -->

## Fidelity

Every source says how its figure was arrived at: `Measured` (read from the
hardware or a live API), `Modelled` (derived, such as a country average),
`Stated` (given by the caller), or `Assumed` (a fixed fallback, such as the
global average). A frontend shows fidelity so a user can tell a
reading from an estimate.

## Keys

`config_file::keys` merges, each winning over the one before: the saved config
file, then `CARBONTRACKER_EM_KEY` in the environment, then keys given by the
caller. It reports when the config file is readable by others, so a frontend
can warn; it does not warn itself. <!-- AC/PROV-47 -->

## Why

- **The chains end in something infallible** so that probing never fails a run.
  A tracker always measures something, even if only a modelled average.
- **A cooldown rests on measurement** because how fast a device can honestly
  be read differs by device and generation, and is rarely documented.
- **A fixed value is a provider, not an engine notion,** so that pinned runs,
  which every test above the providers uses, read through the same path as
  measured ones.
- **The registry never prints** so that the same registry serves the CLI, the
  Python package and a TUI without each having to suppress the others' output.

## Anchors

```anchor
carbontracker_providers::ProviderRegistry
carbontracker_providers::ProbeProgress
carbontracker_providers::Resolution
carbontracker_providers::Forecaster
carbontracker_providers::ForecastReport
carbontracker_providers::StaticProvider
carbontracker_providers::ProbeConfig
carbontracker_providers::ProbeConfigError
carbontracker_providers::GridConfig
carbontracker_providers::Pinned
carbontracker_providers::Measure
carbontracker_providers::ApiKey
carbontracker_providers::KeyedService
carbontracker_providers::Location
carbontracker_providers::Estimate
carbontracker_providers::Coordinates
carbontracker_providers::CountryCode
carbontracker_providers::Verdict
carbontracker_providers::CandidateOutcome
carbontracker_providers::config_file
carbontracker_providers::process_tree
```

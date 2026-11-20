# Open intensity — live grid intensity from public sources

Live carbon intensity for the grids whose operators or public institutes
publish it openly, so a run on those grids measures its intensity without an
Electricity Maps key. Each open source is an intensity candidate in the
default registry ([03](03-providers.md)), under the same rules as
Electricity Maps: it reads the location in its own terms or declines, and it
describes the grid it actually measures. <!-- AC/OPEN-1 unbuilt -->

The chains of [03](03-providers.md) (AC/PROV-7, AC/PROV-8) place them; this
document says what each one reads and when.

## The sources

| Candidate | Publisher | Grids | Published every | Forecast |
|---|---|---|---|---|
| `energinet` | Energinet, Energi Data Service (`CO2Emis`, `CO2EmisProg`) | `DK-DK1`, `DK-DK2` | 5 min | Yes, to the end of the next day | <!-- AC/OPEN-2 -->
| `neso` | NESO, Carbon Intensity API | `GB` (Great Britain, national), and its regions `GB-NESO-1` to `GB-NESO-17` | 30 min | Yes, 48 h | <!-- AC/OPEN-3 unbuilt -->
| `energy-charts` | Fraunhofer ISE, Energy-Charts (`co2eq`) | The countries it publishes `co2eq` for | 15 min | Yes | <!-- AC/OPEN-4 unbuilt -->

**A source serves only the grids its table row names.** A location outside
them is declined without a request. <!-- AC/OPEN-6 -->

**No open source takes a key, and none sends anything that identifies the
user**: no account, and no location finer than the grid it asks for, apart
from the outward postcode `neso` sends to find a region. <!-- AC/OPEN-7 -->

## Reading the location

| Location | `energinet` | `neso` | `energy-charts` |
|---|---|---|---|
| Stated zone | `DK-DK1`, `DK-DK2` | `GB`, or a region `GB-NESO-<id>` | A country code it publishes | <!-- AC/OPEN-8 -->
| Stated country | Declined: Denmark is two grids | `GB` | Served | <!-- AC/OPEN-9 -->
| Coordinates, stated or estimated | `DK-DK2` east of the Great Belt and on Bornholm, `DK-DK1` west of it | `GB`, national, unless in Northern Ireland | The country they lie in | <!-- AC/OPEN-10 -->
| Estimated, with a postcode | As for coordinates | The region NESO gives for the outward postcode | The country | <!-- AC/OPEN-39 unbuilt -->

**A zone finer than a source publishes falls back to the zone's country**, as
Electricity Maps does, and the description says the country, not the zone
asked for: `SE-SE3` read from `energy-charts` is described as `SE`. <!-- AC/OPEN-11 unbuilt -->

**A NESO region is stated by NESO's region id**: 1 to 14 are the
distribution regions, from `GB-NESO-1` North Scotland to `GB-NESO-14` South
East England, and 15 to 17 are England, Scotland and Wales. An id outside 1
to 17 is declined, saying which. <!-- AC/OPEN-37 unbuilt -->

**An estimated location carries the postcode the geolocation service gives**,
in `Estimate`, when it gives one. <!-- AC/OPEN-40 unbuilt -->

**`neso` finds an estimated location's region by asking NESO for its outward
postcode**, the part before the space, and keeps the region NESO answers
with for every later read. With no postcode, or no region in the answer, it
reads `GB`, national. Coordinates are never sent. <!-- AC/OPEN-41 unbuilt -->

**Northern Ireland is not on the GB grid.** `GB-NIR`, a postcode starting
`BT`, or coordinates there, is declined by `neso`. <!-- AC/OPEN-12 unbuilt -->

## Reading

**Each read takes the latest published figure, stamped at the start of the
interval it covers.** An answer with the same stamp as the last is
`Unchanged`. The probe's confirming fetch answers the first read, as for
Electricity Maps (AC/PROV-13). <!-- AC/OPEN-14 -->

**A figure is the publisher's latest for an interval that has begun, never
one for an interval still to come.** `energinet` reads `CO2Emis`;
`energy-charts` the latest interval with a `co2eq`; `neso` national the
latest half-hour with an `actual`. <!-- AC/OPEN-15 -->

**A NESO region is read from the current half-hour's `forecast`**, since NESO
publishes no other figure for a region, not even afterwards. <!-- AC/OPEN-38 unbuilt -->

**Every open source is `Measured` and `Held`.** <!-- AC/OPEN-17 -->

**The candidate's name in the resolution says the publisher's basis**, e.g.
`energinet (direct CO2)`, since public sources differ in what they count and
Electricity Maps is asked for lifecycle figures. <!-- AC/OPEN-18 unbuilt -->

| Candidate | Cooldown |
|---|---|
| `energinet` | 1 min | <!-- AC/OPEN-19 -->
| `neso` | 5 min | <!-- AC/OPEN-20 unbuilt -->
| `energy-charts` | 5 min | <!-- AC/OPEN-21 unbuilt -->

**A refusal for asking too often is `RateLimited`.** <!-- AC/OPEN-23 -->

## The chains

**Intensity:** Electricity Maps → `energinet` → `neso` → `energy-charts` →
country average → global average. <!-- AC/OPEN-24 unbuilt -->

**Electricity Maps stays first.** A user who gives a key has chosen it, and
its lifecycle figures match the country table below it. With no key it
declines, and the open sources answer. <!-- AC/OPEN-25 unbuilt -->

**A grid's own operator comes before an aggregator.** Denmark is read from
`energinet`, not `energy-charts`. <!-- AC/OPEN-26 unbuilt -->

**Forecast:** Electricity Maps forecast → the forecast of the open source that
won intensity, for the same grid → the current intensity held flat. A
forecast never comes from a publisher other than the one billed, so the curve
and the bill describe one grid on one basis. <!-- AC/OPEN-27 unbuilt -->

| `Forecaster` from an open source | |
|---|---|
| `fidelity()` | `Measured`, as for Electricity Maps (AC/PROV-18) | <!-- AC/OPEN-28 unbuilt -->
| `served_by()` | The candidate, e.g. `neso` | <!-- AC/OPEN-29 unbuilt -->
| `horizon()` | What the publisher reaches, which can be shorter than asked for | <!-- AC/OPEN-30 -->

## What it never does

**It never asks Electricity Maps for a grid an open source served.** Open
sources do not spend a key's allowance. <!-- AC/OPEN-32 unbuilt -->

**It never mixes publishers within one grid**: one run's intensity readouts
and its forecast come from one publisher. <!-- AC/OPEN-33 unbuilt -->

**It never converts a publisher's figure** from one basis to another. <!-- AC/OPEN-34 unbuilt -->

**Pinned intensity contacts none of them** (AC/PROV-15). <!-- AC/OPEN-35 unbuilt -->

## Testing

**Each source is tested against recorded answers at a base URL the test
sets**, so no test above the provider level reaches the network. One live
check per source confirms its answer still parses. <!-- AC/OPEN-36 live-only -->

## Why

- **Open sources follow Electricity Maps, not lead it,** so a keyed run keeps
  one basis from its readout down to the country average.
- **Operators before aggregators** because the operator's figure is the one
  the aggregator republishes, later.
- **No conversion between bases** because a factor to turn direct into
  lifecycle figures is a model, and the run would then call a model measured.
- **An estimated location is read to a region**, though the postcode is the
  ISP's guess and can land in a neighbouring region, because a near region is
  still closer to the host's grid than the national figure.
- **Only `neso` receives a postcode**, the district-level guess the
  geolocation service already returned, because NESO maps a postcode and
  nothing coarser to a region.
- **Northern Ireland is declined** because the Carbon Intensity API covers
  Great Britain only.

## Anchors

```anchor
carbontracker_providers::ProviderRegistry
carbontracker_providers::Estimate
carbontracker_providers::Forecaster
carbontracker_providers::Verdict
```

# The documentation — carbontracker.info and the README

What the site promises its readers, and what keeps it true. Its addresses are
the seam: papers since 2020 link carbontracker.info, and an address once
published keeps answering. Every example on it runs, and every printout on it
is the output of running it. <!-- AC/DOC-1 unbuilt -->

## Addresses

| Path | Holds |
|---|---|
| `/` | What carbontracker is, installing, three examples (a training loop, a command, a report for a paper) |
| `/install/` | Every platform, and what each measures |
| `/start/python/`, `/start/cli/` | A first run, start to finish |
| `/guides/<topic>/` | Training loops, spans, servers, profiler mode, clusters and Slurm, projects, external usage, coding agents, sync, reporting for a paper |
| `/concepts/<topic>/` | Measuring and fidelity; segments and attribution; carbon intensity; what is estimated, and what is left out |
| `/reference/cli/`, `/reference/python/`, `/reference/config/`, `/reference/record/`, `/reference/api/` | Every command, name, key, record line and call |
| `/migrate/` | From 2.x |
| `/validation/` | How close the readings are, with the experiments |
| `/cite/` | How to cite carbontracker, and the version |
| `/changes/` | What changed, per release |
| `/2/` | The 2.x site, as it was |

**Every address the 2.x site had still answers**, with its page under `/2/`
or a redirect to the page that replaces it. <!-- AC/DOC-2 unbuilt -->

**A page that moves leaves a redirect.** No published address answers
404. <!-- AC/DOC-3 unbuilt -->

**Every page names the version it documents.** A `/2/` page says it is for
2.x, and links the page that replaces it. <!-- AC/DOC-4 unbuilt -->

## True by construction

**Every code example runs in CI against the release it documents.** <!-- AC/DOC-5 unbuilt -->

**Every printout is the output of its example**, made by running it with its
inputs pinned (stated power and intensity, a fixed clock), never typed by
hand. <!-- AC/DOC-6 unbuilt -->

**The README's examples are the site's, checked the same way.** <!-- AC/DOC-7 unbuilt -->

**Every command and flag `--help` lists has a reference entry, and every entry
is in `--help`.** <!-- AC/DOC-8 unbuilt -->

**Every name in `carbontracker.__all__` has a reference entry, and every entry
is exported.** <!-- AC/DOC-9 unbuilt -->

**Every config key, record line and API call has a reference entry, and every
entry exists.** <!-- AC/DOC-10 unbuilt -->

**Every claim of accuracy links the experiment behind it**: the hardware, the
version, the script, the date. <!-- AC/DOC-11 unbuilt -->

**Every estimator factor is listed with its source** (AC/EXT-11). <!-- AC/DOC-12 unbuilt -->

**Every link on the site resolves**, checked in CI. <!-- AC/DOC-13 unbuilt -->

## Measured or estimated

**`/concepts/estimates/` lists every number carbontracker reports and how it is
arrived at**: measured, modelled, stated or assumed, local or external. <!-- AC/DOC-14 unbuilt -->

**The same page says what no number includes**: the hardware's manufacture,
networks and storage, a data centre's overheads beyond its PUE, and other
programs' draw on a device measured as a whole. <!-- AC/DOC-15 unbuilt -->

## Light

**A page loads nothing from another site and sets no cookies.** <!-- AC/DOC-16 unbuilt -->

**A page reads in full without JavaScript.** Only search needs it. <!-- AC/DOC-17 unbuilt -->

**A page, figures included, transfers under 300 kB.** <!-- AC/DOC-18 unbuilt -->

**Every figure has a light and a dark theme, and a text description.** <!-- AC/DOC-19 unbuilt -->

## Why

- **Addresses are the seam** because papers link the site, and a paper cannot
  be edited.
- **Printouts are generated** because hand-written ones drift: the README's
  had to be matched to real runs by hand before release.
- **Coverage both ways**, help to reference and back, so the reference can
  neither miss a flag nor describe one that is gone.
- **What is left out is said**, because a total is only as honest as its
  boundary.
- **Light**, because a tool for counting emissions wastes none.

## Anchors

```anchor
carbontracker_cli::run
carbontracker_py::Tracker
```

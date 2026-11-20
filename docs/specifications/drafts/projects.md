# Projects and configuration — `carbontracker init`, the two files

A project gathers runs from many machines under one id, so that its usage adds
up to one number: a paper's runs on a laptop and on a cluster are one
project's. Settings come from two files. The machine's file holds keys and is
never shared. The project's file holds nothing secret and is committed with the
code, so every checkout of the code is in the project. `carbontracker init`
writes both, through a wizard on a terminal. <!-- AC/CFG-1 unbuilt -->

![Monday to Friday, one project on three machines. The project file is committed on Monday, so every checkout carries project paper-ct3. The laptop and Bob upload each run as it ends; Hendrix's nodes have no internet, so its runs are synced from the login node. A killed run is listed, not summed; r2 uploaded twice counts once. 12.93 kWh in all.](figures/projects-sync.svg)

## The two files

| File | Where | Holds | Shared |
|---|---|---|---|
| The machine's | `$XDG_CONFIG_HOME/carbontracker/config.toml`, else `~/.config/carbontracker/config.toml` | Keys, the server login, the machine's name, defaults for every run here | Never |
| The project's | `carbontracker.toml`, in the project's root | The project's name and id, defaults for its runs, where it syncs | Committed with the code |

**A run's project file is the first `carbontracker.toml` found from the working
directory upward.** `--project PATH` or `CARBONTRACKER_PROJECT=PATH` names one
instead, for a job that runs outside the checkout; `--no-project` runs outside
any project. <!-- AC/CFG-2 unbuilt -->

**A project file never holds a key or a token.** One found there is refused,
naming the file and the machine's file, where it belongs. <!-- AC/CFG-3 unbuilt -->

**An unknown key is warned about, naming the file and the key, and the run goes
on**, so a project file written by a newer carbontracker still works on an
older one. <!-- AC/CFG-4 unbuilt -->

**A known key with a bad value is refused as its flag is**
([06](../06-cli.md), AC/FE-41): exit 2 or `ValueError`, naming the file, before
anything is probed. <!-- AC/CFG-5 unbuilt -->

**A path in a project file is relative to that file's directory**, so
`log_dir = "logs"` gathers runs started from any subdirectory in one
place. <!-- AC/CFG-6 unbuilt -->

| Key | Means | In |
|---|---|---|
| `project.name` | The project's name, for people | Project; machine, as the default project |
| `project.id` | The project, for programs | Project; machine, as the default project |
| `run.log_dir` | Where runs are saved | Both |
| `run.zone`, `run.country`, `run.coordinates`, `run.global` | Where the grid is; at most one | Both |
| `run.components`, `run.interval`, `run.pue` | As the flags | Both |
| `machine.name` | This machine, for people; the host name by default | Machine |
| `keys.electricity_maps` | The Electricity Maps key | Machine |
| `sync.server`, `sync.auto` | Where the project syncs, and whether each run uploads as it ends ([sync](sync.md)) | Project |

## Which value wins

**Each setting is taken from the first that has it: an argument or flag, the
environment, the project's file, the machine's file, the default.** Keys keep
their own order (AC/FE-42): argument, environment, machine's file. <!-- AC/CFG-7 unbuilt -->

**A Python argument the caller did not pass takes the configured value; one the
caller passed wins, even `None`.** `CarbonTracker(log_dir=None)` writes nothing,
whatever the project's file says. <!-- AC/CFG-8 unbuilt -->

**Python and the CLI resolve the same settings from the same files.**
`carbontracker config --json` and `carbontracker.config()` give the same
values from the same places. <!-- AC/CFG-9 unbuilt -->

## A project

**A project is its id.** The `init` that creates a project makes the id at
random, 128 bits, and it never changes. Its first six characters are its short
form, which is what people see. <!-- AC/CFG-10 unbuilt -->

**Every run records its project's id and name, the machine's name and the
user**, and on a cluster the scheduler, the cluster's name and the job id
(`SLURM_JOB_ID`, `SLURM_CLUSTER_NAME`). <!-- AC/CFG-11 unbuilt -->

**A renamed project keeps its runs.** Runs are grouped by id, under the newest
name. <!-- AC/CFG-12 unbuilt -->

**Two projects with one name are two projects.** `report` shows each with its
short id, and warns. <!-- AC/CFG-13 unbuilt -->

**`init --join ID` puts a checkout in an existing project** by writing a
project file with that id, for a checkout that does not have the file. <!-- AC/CFG-14 unbuilt -->

## `report` across machines

**`report` with no path reads the project's log directory**, including runs
pulled from the server ([sync](sync.md)). Outside a project it is exit 2, as
today. <!-- AC/CFG-15 unbuilt -->

**`--project ID|NAME` keeps one project's runs. `--by project|machine|user|month`
groups the runs, with a total per group.** <!-- AC/CFG-16 unbuilt -->

**A run read twice counts once.** A run is known by its id. Two copies with
the same bytes are one run. When one copy is the start of the other, as a copy
taken while the run went on is, the longer one is read. Copies that differ in
any other way are named, and the run is left out of the total. <!-- AC/CFG-17 unbuilt -->

**A run of no project is listed as such**, and never counted towards a
project. <!-- AC/CFG-18 unbuilt -->

## `init`

**On a terminal, `init` is a wizard. Elsewhere it takes flags**, `init
--project NAME [--join ID] [--machine] [--log-dir DIR] [--country CC] …`, and
reads the key, if any, as one line of stdin. <!-- AC/CFG-19 unbuilt -->

| Step | Asks | Starts at |
|---|---|---|
| Scope | This project, or this machine | This project inside a git checkout, else this machine |
| Project | A new project and its name, or one to join by id | New, named after the directory |
| Runs | Where runs are saved | `logs`, beside the project file |
| Location | From this IP, a country, a zone or coordinates, showing what the IP gives | From this IP |
| Key | The Electricity Maps key, hidden, checked with the service | None |
| Machine | Its name, the components measured, the PUE | The host name, all, 1.58 |
| Check | What `check` shows with these settings | — |
| Review | Each file, and exactly the lines that change in it | — |

**Nothing is written until the review is confirmed.** Leaving before that, by
Esc or Ctrl-C, leaves every file as it was, byte for byte. <!-- AC/CFG-20 unbuilt -->

**Each step starts at the value in effect, and says where it comes from.** <!-- AC/CFG-21 unbuilt -->

**Editing a file keeps its comments, its order, and every key `init` did not
ask about.** <!-- AC/CFG-22 unbuilt -->

**The key keeps every promise of [06](../06-cli.md)'s `init`** (AC/FE-55 to
AC/FE-57): hidden, never a flag, owner-only file. A key the service declines
is said so before saving, and is saved only if the user says so. <!-- AC/CFG-23 live-only -->

**The wizard works at 80 by 24, with the keyboard alone, and with `NO_COLOR`
set.** <!-- AC/CFG-24 unbuilt -->

**A new project file begins with a comment saying it holds nothing secret and
belongs in version control.** <!-- AC/CFG-25 unbuilt -->

**`init` ends by printing the files it wrote, then the `check` rows.** <!-- AC/CFG-26 unbuilt -->

## `config`

**`carbontracker config` prints each setting in effect, its value, and where it
came from**: the flag, the environment variable, the file's path, or the
default. Keys are shown by their last four characters. `--json` prints the same
for programs. It exits 0 and writes nothing. <!-- AC/CFG-27 unbuilt -->

## What it never does

**A run never writes a config file.** Only `init` and `login` do. <!-- AC/CFG-28 unbuilt -->

**A config file never makes anything leave the machine.** Sending is
[sync](sync.md)'s, and only when asked. <!-- AC/CFG-29 unbuilt -->

## Why

- **Two files**, because a key must never be committed and a project's settings
  must be.
- **The project's file travels with the code**, so a cluster job is in the
  project without a flag.
- **Ids, not names**, because two students both call their project "paper".
- **Unknown keys warn**, so a shared project file never breaks a collaborator
  on an older version.
- **A run read twice counts once**, because copying logs between machines is
  how a project's runs get gathered.
- **The id that travels with the code is the point, not the wizard**:
  CodeCarbon already has a config wizard and global and local files, but no
  project that gathers runs across machines without a server.
- **`carbontracker.toml` at the root, not `.carbontracker/config.toml`** as the
  prototypes had it: it is meant to be committed, like `pyproject.toml`.

## Anchors

```anchor
carbontracker_cli::run
carbontracker_run::Settings
carbontracker_run::Project
carbontracker_run::read_report
carbontracker_py::Tracker
```

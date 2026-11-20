# The CLI — `carbontracker`

What users type. The CLI parses its arguments, builds a `RunConfig`, and lets
`carbontracker-run` do the work; the crate graph in [01](01-system.md) keeps it
from reaching the engine or a provider any other way. The same binary ships
with the Python package ([10](10-python.md)).

```
carbontracker [OPTIONS] <COMMAND>...         track a whole command
carbontracker check [OPTIONS]                show what would be measured
carbontracker init                           save an API key (a menu on a terminal)
carbontracker init slurm [SCRIPT]            run a Slurm script under carbontracker
carbontracker report [--spans] [--csv [-o FILE]] <PATH>...   sum the runs recorded
```

## Tracking a command

**During a run all of carbontracker's output goes to stderr**, so the
command's stdout passes through untouched. <!-- AC/FE-2 -->

**The command's stdin is its own**: carbontracker never reads it during a
run. <!-- AC/FE-59 -->

| The command | carbontracker exits with |
|---|---|
| exits with code `c` | `c` | <!-- AC/FE-28 -->
| is killed by signal `n` | `128 + n` | <!-- AC/FE-29 -->
| is not found / cannot be run | `127` / `126`, naming the program on stderr | <!-- AC/FE-30 -->

**A failure to finish the session is reported on stderr and never changes the
exit code.** Tracking never decides how the command went. <!-- AC/FE-31 untested -->

**Ctrl-C reaches the command, not carbontracker.** carbontracker stays to
print the closing block and finish the record, then exits `130`. <!-- AC/FE-32 -->

**A SIGTERM sent to carbontracker is passed on to the command**, and the run
is finished as for Ctrl-C. <!-- AC/FE-33 -->

**Options go before the command.** Everything from the command's name on is
the command's own, so `carbontracker --country DK python -u train.py --global`
passes `--global` to Python. <!-- AC/FE-34 -->

**A command named like a subcommand runs after `--`**: `carbontracker -- check`
runs a program called `check`. <!-- AC/FE-35 -->

**With no command, the usage goes to stderr and the exit code is 2.** <!-- AC/FE-36 -->

## Options

Every option applies to tracking and to `check`.

| Group | Flags | Default |
|---|---|---|
| Location | `--zone`, `--country`, `--coordinates`, `--global` — at most one | estimated from this IP | <!-- AC/FE-3 -->
| Fixed values | `--power`, `--cpu-power`, `--gpu-power` (watts), `--intensity` (gCO2eq/kWh) | probed | <!-- AC/FE-4 -->
| Measuring | `--components`, `--interval`, `--pue` | `all`, 1 s, 1.58 | <!-- AC/FE-5 -->
| Output | `--log-dir`, `--no-log`, `-v`/`--verbose` | `./logs` | <!-- AC/FE-6 -->
| Keys | `--api-keys` | environment, then config file | <!-- AC/FE-7 -->

**`--power` is the whole machine**, so it is refused with `--cpu-power` or
`--gpu-power`. `--cpu-power` or `--gpu-power` alone replaces that component
and leaves the rest probed. <!-- AC/FE-37 -->

**`--no-log` is refused with `--log-dir`.** <!-- AC/FE-38 -->

**2.4.7's spellings `--log_dir` and `--api_keys` are the same flags.** <!-- AC/FE-40 -->

## Bad arguments

**Every bad argument exits 2 with a message on stderr naming what is wrong**,
before anything is probed, and the command is never started. <!-- AC/FE-8 -->

**Every value is checked before probing.** A value is refused when: <!-- AC/FE-41 -->

| Flag | Refused when |
|---|---|
| `--zone` | empty |
| `--country` | not two letters |
| `--coordinates` | not `LAT,LON`, or outside ±90, ±180 |
| `--power`, `--cpu-power`, `--gpu-power` | not a number, or negative |
| `--intensity` | not a number above zero |
| `--interval` | not a number of seconds above zero |
| `--pue` | below 1 |
| `--components` | not `all`, `cpu`, `gpu` or `cpu,gpu` |
| `--api-keys` | not a JSON object (or a Python dict); a provider name it does not know, listing the ones it does; two keys for one provider |

**Flags that 2.4.7 took are refused by name**, saying what to use instead,
rather than with "unexpected argument": `--parse` and `--report` point to
`carbontracker report`; `--sim-cpu`, `--sim-cpu-tdp`, `--sim-cpu-util` to
`--cpu-power`; `--sim-gpu`, `--sim-gpu-watts`, `--sim-gpu-util` to
`--gpu-power`. <!-- AC/FE-39 -->

## Keys

**A key given with `--api-keys` wins over `CARBONTRACKER_EM_KEY`, which wins
over the config file `init` writes.** <!-- AC/FE-42 -->

**A config file others can read gets a warning on stderr**, and the run goes
on. <!-- AC/FE-43 -->

**`CARBONTRACKER_EM_URL` replaces the address Electricity Maps is asked at**,
for probing and for the key check, so a proxy or a mirror can stand in. <!-- AC/FE-64 -->

## What the run prints

**Before the command starts, stderr lists what is measured**: a line for each
power source, the intensity and the forecast, each with its fidelity, then
`ready. Starting <command>`. <!-- AC/FE-44 -->

**After the command ends, however it ends, stderr gets the closing block**:
`<command> summary:` with its time, energy, intensity, CO2eq and what that
equals in km by car, and the run's folder when there is one. <!-- AC/FE-45 -->

**`--verbose` adds every probe step, event and readout**, each with its time.
Without it, a run prints only what is measured, warnings, errors and the
closing block. <!-- AC/FE-46 -->

## The run folder

**Each run gets a folder of its own:
`<log-dir>/run-<YYYYMMDD_HHMMSS>-<id>/`, holding `run-<id>.ct` and
`output.log`**, where `<id>` is eight lowercase letters and digits and the
time is local, as a user browsing the folder reads it. The record is
[05](05-record-v1.md). <!-- AC/FE-47 -->

**`output.log` holds what the console printed at normal verbosity**, whether
or not `--verbose` was given. <!-- AC/FE-48 -->

**Runs started together in one directory never share a folder.** <!-- AC/FE-49 -->

**A log directory that cannot be written exits 2, naming it**, before probing
and before the command starts. <!-- AC/FE-50 -->

**`--no-log` writes no file and no folder.** <!-- AC/FE-51 -->

## `check`

**On a terminal, `check` opens a full-screen view**: the emission rate right
now (power × PUE × intensity, in gCO2eq/h), then one line each for power,
location, intensity and forecast, with the value, how it is obtained and the
provider serving it. The values follow each readout until the user presses
`q`, Esc or Ctrl-C. <!-- AC/FE-77 -->

**A kind nothing serves shows `none`** and points to `check -v`. <!-- AC/FE-78 -->

**`check -v` on a terminal lists every candidate as the probe decides it**:
serving, standing by (a better candidate serves), declined, or not asked
for, with its reason, and the selected row's full reason at the bottom. <!-- AC/FE-79 -->

**When the view closes, what it last showed is printed to stdout** as plain
text, so it stays in the terminal's scrollback. <!-- AC/FE-80 -->

**The view opens only when stdin and stdout are both terminals.** Otherwise
`check` prints the rows below and exits. <!-- AC/FE-81 -->

**Piped, `check` prints to stdout one row per value it would use**: power, location,
intensity and forecast, each with how it is obtained (`live`, `stated`,
`modelled` or `fallback`) and what it is: the power components, the place, the
kind of intensity and forecast. Power gets one row per way of obtaining it.
Then, for each keyed service, where its key came from, and the providers
serving. It exits 0. A location row is left out when the intensity is
stated. <!-- AC/FE-52 -->

**A key whose provider declined is warned about** below the rows, naming the
key by its hint, giving the reason the provider declined, and pointing to
`carbontracker init`. <!-- AC/FE-61 -->

**`check -v` adds every candidate's verdict** (serving, or why not) after the
rows. <!-- AC/FE-60 -->

**A key is shown only by its last four characters**, never whole. <!-- AC/FE-53 -->

**`check` writes no file and runs no command.** The view reads the sources
for as long as it is open, and nothing else. <!-- AC/FE-54 -->

## `init`

**On a terminal, `init` offers a menu**: add or change the Electricity Maps
key (naming the saved one by its hint), attach carbontracker to a Slurm
script, check what would be measured, or quit. It comes back to the menu
after each until the user quits. <!-- AC/FE-65 -->

**On a terminal, `init` runs full screen**: the key is typed masked, the
Slurm script is picked from a list or typed as a path, the wrapper is shown
before it is written, and `check` opens in its view. When `init` closes, what
it did is printed to stdout. <!-- AC/FE-82 -->

**`init` asks for the Electricity Maps key**: with a hidden prompt on a
terminal, otherwise one line from stdin. It is never a flag, so it never lands
in shell history. <!-- AC/FE-55 -->

**The key is checked with Electricity Maps before it is saved.** A key that
works is saved, and the zones it reaches are named. <!-- AC/FE-66 -->

**A refused key is never saved**: on a terminal the key is asked for again;
otherwise `init` exits 2. <!-- AC/FE-67 -->

**When Electricity Maps cannot be reached, the key is saved unchecked** with
a warning naming why. On a terminal the user chooses between saving it
unchecked and trying again. <!-- AC/FE-68 -->

**`init` saves the key, then runs `check`** so the user sees it take effect,
including the warning when the key does not reach this machine's zone. <!-- AC/FE-9 -->

**The config file is readable by its owner only** (mode `600` on Unix). <!-- AC/FE-56 -->

**An empty key exits 2 and saves nothing.** <!-- AC/FE-57 -->

## `init slurm`

**`init slurm SCRIPT` writes a wrapper next to the script**, named
`<stem>.ct.<ext>` (`train.sbatch` gives `train.ct.sbatch`) or `<name>.ct`
without an extension, executable, and prints the `sbatch` line that submits
it. <!-- AC/FE-69 -->

**The wrapper keeps the `#SBATCH` lines Slurm reads** (those before the first
command) **and the script's interpreter**, then runs the script by its
absolute path with that interpreter, under carbontracker by its absolute
path, passing the job's arguments on. <!-- AC/FE-70 -->

**Runs of the wrapper are logged to `--log-dir`**, relative to the folder the
job is submitted from. <!-- AC/FE-71 -->

**A wrapper `init` wrote before is replaced; any other file at that path is
left alone**, and `init slurm` exits 2 naming it. <!-- AC/FE-72 -->

**A script that is itself a wrapper exits 2.** <!-- AC/FE-73 -->

**A script without `#SBATCH` lines is warned about**, and the wrapper is
still written. <!-- AC/FE-74 -->

**Without SCRIPT, `init slurm` lists the Slurm scripts under the current
folder**, newest first, with each one's job name, partition and time, marking
those already wrapped. On a terminal it asks which to wrap; otherwise it exits
2 after the list, asking for the script by name. <!-- AC/FE-75 -->

**`init slurm` never changes the script and never submits a job.** <!-- AC/FE-76 -->

## `report` — reading runs back

**`report` reads every `.ct` file under the paths it is given**, searching
folders recursively, and prints to stdout, or saves CSV with `--csv`. It reads only what runs recorded:
it never measures, predicts or calls a provider. <!-- AC/FE-21 -->

**The total is what was measured.** It sums the `session` line of each run.
A run without one (crashed or killed) is listed as incomplete and left out
of the total. A prediction is never summed. <!-- AC/FE-22 -->

**Absent emissions are never zero.** A run with no intensity adds its energy
to the total and no emissions, and the report says how many runs that was. <!-- AC/FE-23 -->

**A file that cannot be read is named, with the reason**, and left out: a
newer format, or a line that does not decode. A last line without its newline
was cut off by a killed writer and is ignored. <!-- AC/FE-24 -->

**`--spans` groups settled spans by path** (`epoch/eval`: the names from the
root down), across runs, with each group's count and its total, mean, minimum
and maximum energy, and its total emissions. <!-- AC/FE-25 -->

**`--csv` saves one row per run or per span group**, values at full
precision in the record's units, an empty cell where a value is absent, and
no total row. <!-- AC/FE-26 -->

**The CSV goes to `runs.csv`, or `spans.csv` with `--spans`, in the folder the
runs were read from (the first path, or the folder holding it), or to the file
`-o FILE` names.** It is never read back as a run. A file already there is
overwritten, so a rerun gives fresh numbers, and the saved path is named on
stderr. Nothing goes to stdout. <!-- AC/FE-62 -->

**Each run is shown by the command it tracked**: for a CLI run, the command
after carbontracker's options, never carbontracker's own argv; for a Python
run, the script's argv. The table names the program by its file name; the CSV
keeps it whole. The table's times are UTC, and its header says so. <!-- AC/FE-63 -->

**A path that does not exist exits 2, naming it.** <!-- AC/FE-58 -->

## Why

- **Output on stderr and the command's exit code** so that `carbontracker` can
  be dropped in front of any command in a pipeline without changing its
  behaviour.
- **Bad arguments exit 2 before probing** so that a typo costs nothing and
  never half-starts a run.
- **Removed flags are refused by name** so that a 2.4.7 script fails with the
  fix in the message, not a clap hint.
- **Only `session` lines are summed** so that a total in a paper is never
  part prediction; 2.4.7 summed predictions for runs that stopped early.
- **The key is never a flag** so that it never lands in shell history.
- **An unreachable API saves the key unchecked** so that a login node without
  internet, or a scripted setup, still gets the key; `check` says if it fails.
- **A missing zone is a warning, not a refusal** so that a free key still
  serves the zone it reaches, and the country aggregate when that is reachable.
- **The wrapper calls carbontracker by absolute path** so that the job finds
  it on the compute node without activating an environment first.
- **`check` reads the sources live on a terminal** so that the user sees real
  numbers moving, not only which provider was chosen; piped, it stays a
  one-shot snapshot that scripts and tests can read.
- **The wrapper runs the script by absolute path** because Slurm runs a copy
  of the batch script from its spool directory.

## Anchors

```anchor
carbontracker_cli::run
carbontracker_providers::check_key
carbontracker_providers::KeyCheck
carbontracker_providers::URL_VARIABLE
carbontracker_run::Folder
carbontracker_run::Report
carbontracker_run::read_report
```

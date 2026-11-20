# Projects and run names — naming runs, and reading a project's runs back

A run can name the project it belongs to, and itself. Both are written into
its record. Each run of a project that saves a run folder is linked from that
project's folder on this machine, so `carbontracker report --project NAME`
reads every run of the project, wherever each saved its logs. Nothing leaves
the machine, and no one is contacted. <!-- AC/PRJ-1 -->

```python
tracker = CarbonTracker(epochs=10, log_dir="logs", project="saintslab/codeq", name="sweep-bits-4")
```

```
carbontracker --project saintslab/codeq --name sweep-bits-4 python train.py
carbontracker report --project saintslab/codeq
```

## Naming a run

| Argument | Means | Default |
|---|---|---|
| `project=` on `CarbonTracker` and `@track` | The project the run belongs to | none | <!-- AC/PRJ-2 -->
| `name=` on `CarbonTracker` and `@track` | The run's name, for people | none | <!-- AC/PRJ-3 -->
| `--project NAME` on a tracked command | The project the run belongs to | none | <!-- AC/PRJ-23 -->
| `--name TEXT` on a tracked command | The run's name, for people | none | <!-- AC/PRJ-24 -->

In Python both are keyword-only ([10](10-python.md), AC/PY-2). On the CLI they
go before the command, as every tracking flag does; `check` and `init` take
neither, and `report --project` is a filter of its own.

**A project name is one or more parts joined by `/`, each part letters,
digits, `.`, `_` and `-`, beginning with a letter or a digit**: `codeq`,
`saintslab/codeq`. Any other project raises `ValueError` before anything is
probed, whatever `ignore_errors` says. <!-- AC/PRJ-4 -->

**A run name is any text with something other than spaces in it and no
control character.** Any other name raises `ValueError` before anything is
probed, whatever `ignore_errors` says. <!-- AC/PRJ-5 -->

**On the CLI, a project or name refused above exits 2 naming it**, before
anything is probed, and the command is never started ([06](06-cli.md),
AC/FE-8). <!-- AC/PRJ-25 -->

**The record's `run` header holds `project` and `name` as given**; a run
without one has no such field ([05](05-record-v1.md)). <!-- AC/PRJ-6 -->

## The project's folder

**Each project has a folder on this machine:
`$XDG_DATA_HOME/carbontracker/projects/<project>/`, else
`~/.local/share/carbontracker/projects/<project>/`**, where the parts of
`saintslab/codeq` are nested folders. <!-- AC/PRJ-7 -->

**A run with a project and a run folder links its run folder from the
project's folder when it starts**: a symbolic link named as the run folder,
to the run folder's absolute path. A run still going, or killed, is therefore
found, and reported as incomplete. <!-- AC/PRJ-8 -->

**A run that saves no run folder (`log_dir=None`, `--no-log`) links nothing.** <!-- AC/PRJ-9 -->

**A link that cannot be made is a warning on stderr naming the folder, and
tracking goes on**, never a tracking failure, and never a change to the
command's exit code. <!-- AC/PRJ-10 -->

On Windows, where a symbolic link needs privileges, no link is made; a
project's runs are reported by their paths, with `--project` keeping the
project's.

## Reading a project's runs

**`carbontracker report --project NAME` with no path reads the runs linked
from NAME's folder.** <!-- AC/PRJ-11 -->

**With paths, `--project NAME` reads those paths and keeps only NAME's runs**,
in the runs, the spans and the total. <!-- AC/PRJ-12 -->

**`--project saintslab` also keeps the runs of `saintslab/codeq`**, and of
every project below it, but not those of `saintslab-old`. <!-- AC/PRJ-13 -->

**`--project NAME` with no path, for a project with no folder on this
machine, exits 2 naming the project.** <!-- AC/PRJ-14 -->

**A project name `--project` refuses (AC/PRJ-4) exits 2 naming it**, before
anything is read. <!-- AC/PRJ-15 -->

**Without `--project`, `report` still needs a path**: the usage goes to stderr
and the exit code is 2. <!-- AC/PRJ-16 -->

**A link whose run folder is gone, deleted or moved, is named with the reason
and left out**; the rest of the report prints as usual. <!-- AC/PRJ-17 -->

**A record reached both through a link and through a path is read once.** <!-- AC/PRJ-18 -->

**In the runs table, a run with a name is shown by its name**, in the column
that otherwise shows its command (AC/FE-63), headed `Run`. <!-- AC/PRJ-19 -->

**The runs CSV has a `project` and a `name` column**, empty where the run has
none. <!-- AC/PRJ-20 -->

**`report(*paths, project=None)` in Python follows the same rules**:
`report(project="codeq")` reads the project's folder, a project with no folder
raises `FileNotFoundError`, and each `Run` carries its `project` and
`name`. <!-- AC/PRJ-21 -->

## What it never does

**`report` never makes, changes or removes a link**, a broken one included.
Removing a project's folder removes its links, never its runs. <!-- AC/PRJ-22 -->

## Why

- **Symbolic links, not an index file**, so that runs starting at once never
  share a file to lock, `ls` shows a project's runs, and a deleted run folder
  is all it takes to drop a run.
- **Linked when the run starts**, so that a killed run shows as incomplete
  instead of vanishing from the project's total.
- **Names, not ids**, so that the project reads as it does in code, and the
  `org/name` form carries over to a server unchanged.
- **A failed link is a warning**, because naming a project must never cost a
  run.
- **`--project` matches whole parts**, so that an organisation's total is one
  command, and a project named as a prefix of another is never counted in it.

## Anchors

```anchor
carbontracker_py::Tracker
carbontracker_cli::run
carbontracker_run::Folder
carbontracker_run::read_report
carbontracker_run::project_folder
carbontracker_run::check_project
```

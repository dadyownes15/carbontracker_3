# Sync — the server, `login`, `sync` and `pull`

A server keeps a project's records in one place, so that its runs from every
machine and every collaborator add up to one total. It stores each record as it
was written, and computes nothing that `carbontracker report` does not: a
project's total on the server is `report` over the same files. It holds
records, not metrics, and finished work, not live streams. <!-- AC/SYNC-1 unbuilt -->

The figure in [projects](projects.md) shows one project synced from three
machines.

## The server holds records

**A record is stored byte for byte**, and reading it back gives exactly the
bytes uploaded. <!-- AC/SYNC-2 unbuilt -->

**A project's report on the server is `carbontracker report --json` over its
records**: the same total, the same incomplete and unreadable runs, the same
paper sentence. <!-- AC/SYNC-3 unbuilt -->

**A run is known by the run id in its header, 128 bits.** <!-- AC/SYNC-4 unbuilt -->

| An upload of a run that is | Answers |
|---|---|
| New | Stored: `201` | <!-- AC/SYNC-5 unbuilt -->
| The bytes already held | Nothing changes: `200` | <!-- AC/SYNC-6 unbuilt -->
| Longer, beginning with the bytes held: the run went on | Replaced: `200` | <!-- AC/SYNC-7 unbuilt -->
| Different in any other way | Refused, `409`; both copies stay where they are | <!-- AC/SYNC-8 unbuilt -->
| Not a record, or of a format newer than the server reads | Refused, `422`, saying which | <!-- AC/SYNC-9 unbuilt -->
| A run whose header names another project | Refused, `409` | <!-- AC/SYNC-10 unbuilt -->

**A run of no project is uploaded only when it is assigned to one** with `sync
--project`. The server keeps the assignment beside the record, never in
it. <!-- AC/SYNC-11 unbuilt -->

**An excluded run stays in sight.** An owner may exclude a run, with a reason.
It leaves the total and stays listed, with who excluded it, when, and why. An
exclusion can be undone. Nothing leaves a project silently. <!-- AC/SYNC-12 unbuilt -->

## People

| Role | Reads | Uploads | Adds people, excludes runs |
|---|---|---|---|
| Owner | Yes | Yes | Yes |
| Member | Yes | Yes | No |
| Viewer | Yes | No | No |

<!-- AC/SYNC-13 unbuilt -->

**Every run shows who uploaded it and from which machine.** <!-- AC/SYNC-14 unbuilt -->

**A member who leaves leaves their runs behind**: usage done for a project
stays in its total. <!-- AC/SYNC-15 unbuilt -->

**Only a project's people see its records**, which hold command lines, host
names and locations. <!-- AC/SYNC-16 unbuilt -->

**An owner may publish a project's report**: anyone with its link then reads
the totals and the paper sentence, but not the records, so that a paper can
cite where its number came from. <!-- AC/SYNC-17 unbuilt -->

## The commands

| Command | Does |
|---|---|
| `carbontracker login [URL]` | Signs in through a browser: prints a link and a code, and saves the token to the machine's file. `--token-stdin` on a machine with no browser | <!-- AC/SYNC-18 unbuilt -->
| `carbontracker logout` | Forgets the token | <!-- AC/SYNC-19 unbuilt -->
| `carbontracker sync [--project ID] [PATH]...` | Uploads the runs the server does not hold yet, from the project's log directory or the paths given | <!-- AC/SYNC-20 unbuilt -->
| `carbontracker pull` | Downloads the project's runs from other machines into `<log_dir>/pulled/` | <!-- AC/SYNC-21 unbuilt -->
| `carbontracker people [add EMAIL --role ROLE \| remove EMAIL]` | Lists or changes the project's people | <!-- AC/SYNC-22 unbuilt -->

**A token is never a flag**, so it never lands in shell history. <!-- AC/SYNC-23 unbuilt -->

**`sync` uploads every run, finished or not.** A run still going is uploaded
as far as it got, and its longer copy replaces it at the next sync, so an
interrupted machine loses nothing and nothing counts twice. <!-- AC/SYNC-24 unbuilt -->

**A second `sync` with nothing new sends no record**, and says so. <!-- AC/SYNC-25 unbuilt -->

**With `sync.auto`, a run uploads itself as it finishes**, waiting at most
5 s. A run that could not be uploaded waits for the next `sync`. Uploading never
changes the command's exit code, and adds one line to its output at
most. <!-- AC/SYNC-26 unbuilt -->

**Before a project's first upload from a machine, `sync` says what a record
holds that others see**: commands, host names and location. It asks on a
terminal; elsewhere it needs `--yes`. <!-- AC/SYNC-27 unbuilt -->

**`pull`, then `report`, prints the server's total.** <!-- AC/SYNC-28 unbuilt -->

**Every command but `login`, `sync`, `pull` and `people` works with no server
and no network.** <!-- AC/SYNC-29 unbuilt -->

## The API

JSON over HTTPS, under `/v1`, with the token as a bearer token. Within `/v1`
the API only grows, by the rules the record grows by ([05](../05-record-v1.md)). <!-- AC/SYNC-30 unbuilt -->

| Call | Does |
|---|---|
| `GET /v1/me` | Who the token is |
| `GET /v1/projects` | The projects the token's user is in |
| `PUT /v1/projects/{project}` | Creates a project with the id its project file holds, or renames it |
| `GET /v1/projects/{project}` | Its name, people and totals |
| `GET /v1/projects/{project}/runs` | Its runs: id, machine, uploader, size, digest, complete or not, excluded or not |
| `PUT /v1/projects/{project}/runs/{run}` | Uploads a record; the body is the `.ct` file |
| `HEAD /v1/projects/{project}/runs/{run}` | The digest held, as its `ETag`, so that `sync` can skip it |
| `GET /v1/projects/{project}/runs/{run}` | The record |
| `PUT`, `DELETE /v1/projects/{project}/runs/{run}/exclusion` | Excludes a run, with a reason; undoes it |
| `GET /v1/projects/{project}/report` | The report, as `report --json`; `?spans=1`, `?by=machine` |
| `PUT`, `DELETE /v1/projects/{project}/people/{email}` | Adds a person with a role; removes one |

**The server takes the project's id from its file**, so a project made by
`init` keeps its id on every machine and on the server. <!-- AC/SYNC-31 unbuilt -->

## The page

**Each project has one page**: its total, local and external apart; totals by
machine, person and month; its runs, with the incomplete and the excluded
listed; its spans; the paper sentence to copy; and the CSV. The numbers are the
report's. <!-- AC/SYNC-32 unbuilt -->

## Running a server

**A server is one program, one SQLite file and one folder of records**, so a
lab can run its own. `carbontracker-server --data DIR`. <!-- AC/SYNC-33 unbuilt -->

## What it never does

**It never changes a record.** <!-- AC/SYNC-34 unbuilt -->

**carbontracker sends nothing to a server unless the user has logged in and
asked**, by a command or by `sync.auto`. <!-- AC/SYNC-35 unbuilt -->

## Why

- **Records, not metrics**, so that every later version can read the runs
  again, and anyone can check the total from the files.
- **The report's code computes the server's totals**, so the website and the
  paper never disagree.
- **A longer copy replaces a shorter one**, so a run uploaded midway is never
  lost and never counted twice.
- **Exclusions stay in sight**, so a project's total can't be quietly trimmed.
- **Finished work first**: live streams are what wandb does well, and a total
  for a paper does not need them.
- **Not another dashboard**: CodeCarbon's already groups runs by organisation
  and project. This server adds a total anyone can check against the records.
- **One process and SQLite**, so self-hosting is a lab's afternoon, not a
  platform.

## Anchors

```anchor
carbontracker_cli::run
carbontracker_run::Report
carbontracker_run::read_report
```

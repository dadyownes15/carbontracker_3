# Coding agents — the tools measured here, the model estimated elsewhere

`carbontracker agent -- claude` runs a coding agent as a tracked command. The
machine is measured as for any command. The agent's hooks open a span per turn
and per tool call, so a test run or a build is billed to the tool call that
started it. The model runs elsewhere: each model call's tokens become an
external entry ([external](external.md)) on the turn that made it. The record
has both, apart, in one run. <!-- AC/AGT-1 unbuilt -->

![One turn of a coding agent over 64 s. Hooks open the turn and a span per tool; the laptop draws 38 W while pytest runs in the Bash tool and 9 W otherwise. Three model calls run at the provider; each one's tokens arrive a few seconds after it ends, the last after the turn stopped, and land on turn 1 by its prompt id. 0.403 Wh measured, 6.60 Wh modelled, 7.01 Wh in all.](figures/agent.svg)

```
carbontracker agent [OPTIONS] -- <AGENT> [ARGS]...   run an agent, tracked
carbontracker agent install <AGENT> [--user]         add carbontracker's hooks to the agent's settings
carbontracker agent hook <EVENT>                     what a hook runs; the event comes on stdin
carbontracker span start NAME [--parent ID]          open a span on the enclosing run; prints its id
carbontracker span stop ID                           close it
```

## Running an agent

**`agent` takes [06](../06-cli.md)'s tracking options, and keeps its promises
about output, stdin, exit codes, Ctrl-C and SIGTERM** (AC/FE-2, AC/FE-28 to
AC/FE-33, AC/FE-59). <!-- AC/AGT-2 unbuilt -->

**An agent's session is one run**, in the project the working directory is in,
as any run. <!-- AC/AGT-3 unbuilt -->

**Without the hooks, the run has the session alone**, with every model call on
it. With them, it has a span per turn and per tool call. <!-- AC/AGT-4 unbuilt -->

**`agent install` adds carbontracker's hooks and changes nothing else** in the
agent's settings: the project's by default, the user's with `--user`. Run
again, it changes nothing. <!-- AC/AGT-5 unbuilt -->

## Inside a tracked command

**A tracked command, and everything it starts, can reach its run.** The run
sets `CARBONTRACKER_RUN` in the command's environment; `carbontracker span`,
`carbontracker external` and the hooks find the run through it. <!-- AC/AGT-6 unbuilt -->

**With no run to reach, `span`, `external` and `agent hook` do nothing and exit
0**, with no output and no file, so the hooks stay harmless outside
`carbontracker agent`. <!-- AC/AGT-7 unbuilt -->

**A cut sent from inside a command is stamped when its sender starts**, before
anything else it does, so tracker latency never moves it. <!-- AC/AGT-8 unbuilt -->

## Spans from hooks

| Claude Code hook | Span |
|---|---|
| `UserPromptSubmit` | Opens `turn N`, keyed by the prompt id |
| `PreToolUse` | Opens `tool: <tool name>` under the turn, keyed by the tool call's id |
| `PostToolUse`, `PostToolUseFailure` | Closes the tool call's span |
| `SubagentStart`, `SubagentStop` | Opens and closes `agent: <agent type>` under the turn |
| `Stop`, `StopFailure` | Closes the turn |
| `SessionEnd` | Finishes the run |

<!-- AC/AGT-9 unbuilt -->

**A hook never blocks the agent.** It is installed to run in the background,
and always exits 0. <!-- AC/AGT-10 unbuilt -->

**A hook closes the span its own id opened**, never the latest one, so
parallel tool calls each keep their own span. <!-- AC/AGT-11 unbuilt -->

**A span whose closing hook never comes is closed with its turn, or with the
session**, and carries a warning. <!-- AC/AGT-12 unbuilt -->

## Model calls

**Tokens come only from interfaces the agent documents**: never from its
transcript, which its makers call internal. For Claude Code, the
`claude_code.api_request` events of its OpenTelemetry export. `agent` turns the
export on for that session alone, through the agent's environment, and points
it at the run. <!-- AC/AGT-13 unbuilt -->

**Each model call is counted once, by its request id.** <!-- AC/AGT-14 unbuilt -->

**Each model call is one external entry on the turn whose prompt id it
carries**, its work the call's input, cache-write, cache-read and output
tokens, priced by `llm-tokens` for its model. <!-- AC/AGT-15 unbuilt -->

**A call that arrives after its turn has closed still lands on that turn**
(AC/EXT-19). <!-- AC/AGT-16 unbuilt -->

**A subagent's calls count, on the turn that started the subagent.** <!-- AC/AGT-17 unbuilt -->

**The run waits for the agent's last export before it finishes**, at most one
export interval and 5 s more, so no call is lost to the delay. <!-- AC/AGT-18 unbuilt -->

**A call to a model the factor table lacks is recorded unpriced, with its
tokens** (AC/EXT-13), and the closing block says how many there were. <!-- AC/AGT-19 unbuilt -->

**An export the user already had keeps getting everything.** When the agent's
environment already names an OpenTelemetry endpoint, the run passes on all it
receives to that endpoint, unchanged. <!-- AC/AGT-20 unbuilt -->

## The adapters

| Agent | Spans from | Tokens from |
|---|---|---|
| Claude Code (`claude`) | Its hooks | `claude_code.api_request` events, one per call |
| Codex CLI (`codex`) | Its hooks | The `token_usage` of its `PostModelRequest` hook |
| A Python agent on the Anthropic or OpenAI SDK | `tracker.span` | Each response's `usage` |

**An adapter is chosen by the agent's program name.** Any other program runs as
a plain tracked command, with a warning that its model calls are not
counted. <!-- AC/AGT-21 unbuilt -->

**`carbontracker.agents.instrument(client)` records each response's `usage` as
an external entry on the innermost open span**; a streamed response's, from its
last event. <!-- AC/AGT-22 unbuilt -->

## The closing block

**After the agent exits, stderr gets the closing block of [06](../06-cli.md),
with the agent's parts**: turns and tool calls; local energy, measured; model
calls and tokens per model; the external part, modelled, with its range; and
the calls left unpriced. <!-- AC/AGT-23 unbuilt -->

```
CarbonTracker: claude summary:
	Time:       0:01:04 · 1 turn, 2 tool calls
	Local:      0.403 Wh, measured
	Remote:     6.60 Wh (2.2–19.8), modelled by llm-tokens@2026.11
	            3 calls to anthropic/claude-sonnet-4-5: 5 600 input, 57 800 cache read, 1 110 output tokens
	Energy:     7.01 Wh
	CO2eq:      1.95 g
```

**`report --spans` groups an agent's spans like any other**: `turn/tool: Bash`
across turns and runs, local and external apart. <!-- AC/AGT-24 unbuilt -->

## What it never does

**A record never holds a prompt, a reply, code, or a tool's input or output.**
It holds names, ids, token counts and usage. `agent` never turns on an export
of the prompts. <!-- AC/AGT-25 unbuilt -->

**It never changes the agent's settings outside `agent install`.** <!-- AC/AGT-26 unbuilt -->

## Why

- **Documented interfaces only**, so an agent's update can't silently change
  the count. Claude Code's transcript format is internal by its own docs.
- **Counted once by request id**, because the same call can be reported more
  than once.
- **The turn by prompt id, not by time**, because telemetry arrives seconds
  after the call.
- **A span per tool call**, because the test runs and builds an agent starts
  are where the local energy goes, and where a user can change something.
- **The tools measured, not only the tokens counted**: the agent trackers
  that exist (claude-carbon, Carbonlog, TokenAtlas and a dozen more) count
  tokens alone, and none measures what the agent runs.
- **Nothing a user typed is recorded**, so that a record can be shared with
  collaborators and uploaded ([sync](sync.md)).

## Anchors

```anchor
carbontracker_cli::run
carbontracker_run::External
carbontracker_run::RunHandle
carbontracker_py::Tracker
```

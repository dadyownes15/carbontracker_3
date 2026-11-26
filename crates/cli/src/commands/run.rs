//! `carbontracker [options] <command>`: running the command as one span,
//! with its own stdio, and passing on its exit code.
//!
//! Ctrl-C: the terminal sends SIGINT to the whole foreground process group,
//! so the command gets it directly. carbontracker ignores it while the
//! command runs, and forwards SIGTERM, then finishes normally.

use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::process::ExitStatus;

use carbontracker_run::presentation::{self, prefixed};
use carbontracker_run::{standard_sinks, Console, Folder, Layout, Outcome, RunConfig, RunHandle, SourceSelection, Verbosity, Work};
use tokio::process::{Child, Command};

use crate::args::{self, Args};
use crate::say;

/// Tracks one command. Configuration errors are returned before probing,
/// for exit code 2.
pub fn track(args: &Args, command: &[OsString]) -> Result<i32, String> {
    let probe = args.probe(&args.keys()?)?;
    let tracker = args.tracker()?;
    // Before probing, so an unwritable directory fails at once.
    let folder = args
        .log_dir()
        .map(|dir| Folder::claim(dir, None).map_err(|error| format!("cannot write logs in {}: {error}", dir.display())))
        .transpose()?;
    if let (Some(folder), Some(project)) = (&folder, args.project())
        && let Err(error) = folder.link_to_project(project)
    {
        say(&presentation::warning(&error));
    }

    let name = args::span_name(command);
    let verbosity = if args.verbose { Verbosity::Everything } else { Verbosity::Normal };
    let colour = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let console = Console { out: Box::new(io::stderr()), verbosity, colour, layout: Layout::Summary { title: name.clone() } };
    let sinks = standard_sinks(console, folder.as_ref()).map_err(|error| error.to_string())?;
    let run = carbontracker_run::start(RunConfig { probe, tracker, frontend: "cli",
        project: args.project().map(str::to_owned),
        name: args.name().map(str::to_owned),
        command: args::hide_keys(std::env::args()),
        sinks,
    }, |_, _| ())
        .map_err(|error| error.to_string())?;

    // The command alone needs a runtime: to wait on it and on signals.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start: {error}"))?;
    let outcome = runtime.block_on(in_span(&run, &name, command));
    drop(runtime);

    Ok(close(outcome, run.finish(outcome), |line| say(&line)))
}

/// The exit code once the session is finished: the command's, whether or not
/// the session finished. A failure is said, never returned. AC/FE-31
fn close<T, E: std::fmt::Display>(outcome: Outcome, finished: Result<T, E>, mut say: impl FnMut(String)) -> i32 {
    if let Err(error) = finished {
        say(presentation::error(&format!("the session didn't finish: {error}")));
    }
    exit_code(outcome)
}

/// Runs `command` as a span named `name` and returns how it ended. A
/// command that can't start exits with 127 (not found) or 126.
async fn in_span(run: &RunHandle, name: &str, command: &[OsString]) -> Outcome {
    let span = run.start_span(name, None, SourceSelection::All);
    let outcome = spawn_and_wait(command).await;
    run.stop_span(span, Work::default());
    outcome
}

/// The exit code for `outcome`: the command's code, or 128 + n for
/// signal n.
fn exit_code(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Exited { code } => code,
        Outcome::Signalled { signal } => 128 + signal,
        _ => 1,
    }
}

#[cfg(unix)]
async fn spawn_and_wait(command: &[OsString]) -> Outcome {
    use tokio::signal::unix::{signal, SignalKind};

    // Registered before the command starts, so a Ctrl-C never kills
    // carbontracker before it can finish.
    let (Ok(mut interrupt), Ok(mut terminate)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate()))
    else {
        eprintln!("{}", prefixed(&presentation::error("cannot handle signals")));
        return Outcome::Exited { code: 1 };
    };
    let mut child = match spawn(command) {
        Ok(child) => child,
        Err(code) => return Outcome::Exited { code },
    };
    loop {
        tokio::select! {
            status = child.wait() => return outcome(status),
            // The command got it too, through the process group.
            _ = interrupt.recv() => {}
            _ = terminate.recv() => forward_terminate(&child),
        }
    }
}

#[cfg(unix)]
fn forward_terminate(child: &Child) {
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) on the child's pid has no memory effects.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
}

#[cfg(not(unix))]
async fn spawn_and_wait(command: &[OsString]) -> Outcome {
    let mut child = match spawn(command) {
        Ok(child) => child,
        Err(code) => return Outcome::Exited { code },
    };
    loop {
        tokio::select! {
            status = child.wait() => return outcome(status),
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

/// Start the command with inherited stdio, or say why it can't start.
fn spawn(command: &[OsString]) -> Result<Child, i32> {
    let (program, arguments) = command.split_first().expect("clap requires a command");
    Command::new(program).args(arguments).spawn().map_err(|error| {
        let program = program.to_string_lossy();
        if error.kind() == io::ErrorKind::NotFound {
            eprintln!("{}", prefixed(&presentation::error(&format!("command not found: {program}"))));
            127
        } else {
            eprintln!("{}", prefixed(&presentation::error(&format!("cannot run {program}: {error}"))));
            126
        }
    })
}

/// Converts an exit status to an [Outcome].
fn outcome(status: io::Result<ExitStatus>) -> Outcome {
    let Ok(status) = status else { return Outcome::Exited { code: 1 } };
    if let Some(code) = status.code() {
        return Outcome::Exited { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Outcome::Signalled { signal };
        }
    }
    Outcome::Exited { code: 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC/FE-31
    #[test]
    fn a_session_that_fails_to_finish_is_reported_and_keeps_the_commands_exit_code() {
        for (outcome, code) in [(Outcome::Exited { code: 0 }, 0), (Outcome::Exited { code: 3 }, 3), (Outcome::Signalled { signal: 15 }, 143)] {
            let mut said = Vec::new();
            let exit = close(outcome, Err::<(), _>("the engine stopped"), |line| said.push(line));
            assert_eq!(exit, code);
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].contains("the session didn't finish: the engine stopped"), "{said:?}");
        }
    }

    #[test]
    fn a_session_that_finishes_says_nothing_more() {
        let mut said = Vec::new();
        assert_eq!(close(Outcome::Exited { code: 3 }, Ok::<(), &str>(()), |line| said.push(line)), 3);
        assert!(said.is_empty(), "{said:?}");
    }
}

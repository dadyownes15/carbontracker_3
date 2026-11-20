//! `carbontracker [options] <command>` tracks a whole command;
//! `carbontracker check` shows what would be measured; `carbontracker init`
//! saves an API key, or attaches carbontracker to a Slurm script;
//! `carbontracker report` sums the runs recorded.
//!
//! [run] implements the CLI; the binary calls it. During a run, all output
//! goes to stderr so the command's stdout is untouched, and the exit code is
//! the command's.

mod args;
mod commands;
mod screen;

use std::ffi::OsString;

use carbontracker_run::presentation;
use clap::Parser;

use args::{Action, Args, InitWhat};

/// Run the CLI on `args` (the program name first), returning the exit code.
pub fn run<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => match args::removed(&error) {
            Some(message) => return fail(&message, 2),
            None => {
                let _ = error.print();
                return error.exit_code();
            }
        },
    };
    let Some(action) = args.action() else {
        return fail("no command given: carbontracker [OPTIONS] <COMMAND>... (see --help)", 2);
    };
    let outcome = match action {
        Action::Check => commands::check(&args),
        Action::Init { what: None } => commands::init(&args),
        Action::Init { what: Some(InitWhat::Slurm { script }) } => commands::init_slurm(&args, script.as_deref()),
        Action::Report { spans, csv, output, paths } => commands::report(spans, csv, output, &paths),
        Action::Run(command) => commands::track(&args, &command),
    };
    outcome.unwrap_or_else(|message| fail(&message, 2))
}

/// Say what went wrong, and return `code`.
fn fail(message: &str, code: i32) -> i32 {
    say(&presentation::error(message));
    code
}

/// Prints a prefixed line to stderr.
fn say(line: &str) {
    eprintln!("{}", presentation::prefixed(line));
}

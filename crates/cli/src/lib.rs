//! `carbontracker [options] <command>` tracks a whole command;
//! `carbontracker check` shows what would be measured; `carbontracker init`
//! saves an API key; `carbontracker report` sums the runs recorded.
//!
//! [run] implements the CLI; the binary calls it. During a run, all output
//! goes to stderr so the command's stdout is untouched, and the exit code is
//! the command's.

mod args;
mod commands;

use std::ffi::OsString;

use carbontracker_run::presentation;
use clap::Parser;

use args::{Action, Args};

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
    // Saved before checking, so the check reads the new key.
    if let Action::Init = action
        && let Err(message) = commands::init()
    {
        return fail(&message, 2);
    }

    let outcome = match action {
        Action::Check | Action::Init => commands::check(&args),
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

//! Reads one replay case as JSON on stdin and writes what the engine did as
//! JSON on stdout. See `carbontracker::replay::live`.

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut case = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut case) {
        eprintln!("replay-live: cannot read stdin: {error}");
        return ExitCode::from(2);
    }
    match carbontracker::replay::live::run_json(&case) {
        Ok(trace) => {
            println!("{trace}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("replay-live: {error}");
            ExitCode::from(2)
        }
    }
}

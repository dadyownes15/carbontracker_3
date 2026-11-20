//! `carbontracker init`: the Electricity Maps key, checked with Electricity
//! Maps before it is saved. On a terminal, [screen::init] runs instead.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use carbontracker_providers::config_file as config;
use carbontracker_providers::{check_key, ApiKey, KeyCheck, KeyedService};

use super::check;
use crate::args::Args;
use crate::screen::{self, init_app::zone_summary};

/// Where `init` asks and answers: stdin and stdout in use, a script in tests.
pub trait Console {
    /// A line typed after `prompt`, or `None` at the end of input.
    fn line(&mut self, prompt: &str) -> io::Result<Option<String>>;
    /// A line that is not echoed on a terminal.
    fn secret(&mut self, prompt: &str) -> io::Result<Option<String>>;
    /// Prints to stdout.
    fn say(&mut self, text: &str);
    /// Prints to stderr.
    fn warn(&mut self, text: &str);
}

/// stdin and stdout; hidden input only on a terminal.
pub struct Std {
    pub terminal: bool,
}

impl Std {
    pub fn new() -> Self {
        Self { terminal: io::stdin().is_terminal() }
    }
}

impl Console for Std {
    fn line(&mut self, prompt: &str) -> io::Result<Option<String>> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut line = String::new();
        let read = io::stdin().lock().read_line(&mut line)?;
        Ok((read > 0).then_some(line))
    }

    fn secret(&mut self, prompt: &str) -> io::Result<Option<String>> {
        if self.terminal {
            rpassword::prompt_password(prompt).map(Some)
        } else {
            let mut line = String::new();
            let read = io::stdin().lock().read_line(&mut line)?;
            Ok((read > 0).then_some(line))
        }
    }

    fn say(&mut self, text: &str) {
        println!("{text}");
    }

    fn warn(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

/// On a terminal, the full-screen app; otherwise one line from stdin is the
/// key, which is checked, saved, and followed by `check`.
pub fn init(args: &Args) -> Result<i32, String> {
    let path = config::path().ok_or("no config directory: set HOME")?;
    if screen::available() {
        return screen::init(args, path);
    }
    piped(&mut Std::new(), &path, &check_key)?;
    check(args)
}

/// The key from one line, checked and saved. An error exits 2.
fn piped(console: &mut dyn Console, config: &Path, check_key: &dyn Fn(&ApiKey) -> KeyCheck) -> Result<(), String> {
    let key = console.secret("").map_err(cannot_read)?.unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Err("no key given".to_owned());
    }
    match check_key(&ApiKey::new(key)) {
        KeyCheck::Works(zones) => {
            save(config, key)?;
            console.say(&format!("Key works: {}. Saved to {}", zone_summary(&zones), config.display()));
        }
        KeyCheck::Refused(why) => return Err(format!("{why}; nothing saved")),
        KeyCheck::Unreachable(why) => {
            save(config, key)?;
            console.warn(&format!("! could not check the key ({why}); saved unchecked to {}", config.display()));
        }
    }
    Ok(())
}

fn save(path: &Path, key: &str) -> Result<(), String> {
    config::save(path, KeyedService::ElectricityMaps, key)
}

fn cannot_read(error: io::Error) -> String {
    format!("cannot read the input: {error}")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Answers from a script; collects what was printed.
    #[derive(Default)]
    pub(crate) struct Scripted {
        pub(crate) input: VecDeque<&'static str>,
        pub(crate) out: String,
        pub(crate) err: String,
    }

    impl Scripted {
        pub(crate) fn new(input: &[&'static str]) -> Self {
            Self { input: input.iter().copied().collect(), ..Self::default() }
        }
    }

    impl Console for Scripted {
        fn line(&mut self, prompt: &str) -> io::Result<Option<String>> {
            self.out.push_str(prompt);
            Ok(self.input.pop_front().map(str::to_owned))
        }

        fn secret(&mut self, prompt: &str) -> io::Result<Option<String>> {
            self.line(prompt)
        }

        fn say(&mut self, text: &str) {
            self.out.push_str(text);
            self.out.push('\n');
        }

        fn warn(&mut self, text: &str) {
            self.err.push_str(text);
            self.err.push('\n');
        }
    }
}

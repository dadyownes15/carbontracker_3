//! `carbontracker init`: the Electricity Maps key, checked with Electricity
//! Maps before it is saved, and a menu on a terminal.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use carbontracker_providers::config_file as config;
use carbontracker_providers::{check_key, ApiKey, KeyCheck, KeyedService};

use super::{check, slurm};
use crate::args::Args;

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

/// What `init` relies on, swapped in tests.
pub struct Setup<'a> {
    pub config: PathBuf,
    pub check_key: &'a dyn Fn(&ApiKey) -> KeyCheck,
    pub check: &'a mut dyn FnMut() -> Result<i32, String>,
    pub slurm: &'a mut dyn FnMut(&mut dyn Console) -> Result<i32, String>,
}

/// On a terminal, the menu; otherwise one line from stdin is the key, which
/// is checked, saved, and followed by `check`.
pub fn init(args: &Args) -> Result<i32, String> {
    let path = config::path().ok_or("no config directory: set HOME")?;
    let mut console = Std::new();
    let terminal = console.terminal;
    let mut run_check = || check(args);
    let mut run_slurm = |console: &mut dyn Console| slurm::slurm(args, None, console, true);
    let mut setup = Setup { config: path, check_key: &check_key, check: &mut run_check, slurm: &mut run_slurm };
    if terminal {
        menu(&mut console, &mut setup)
    } else {
        piped(&mut console, &setup)?;
        (setup.check)()
    }
}

/// The key from one line, checked and saved. An error exits 2.
fn piped(console: &mut dyn Console, setup: &Setup) -> Result<(), String> {
    let key = console.secret("").map_err(cannot_read)?.unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Err("no key given".to_owned());
    }
    let raw = key;
    let key = ApiKey::new(raw);
    match (setup.check_key)(&key) {
        KeyCheck::Works(zones) => {
            save(&setup.config, raw)?;
            console.say(&format!("Key works: {}. Saved to {}", zone_summary(&zones), setup.config.display()));
        }
        KeyCheck::Refused(why) => return Err(format!("{why}; nothing saved")),
        KeyCheck::Unreachable(why) => {
            save(&setup.config, raw)?;
            console.warn(&format!("! could not check the key ({why}); saved unchecked to {}", setup.config.display()));
        }
    }
    Ok(())
}

/// The menu, until the user quits or input ends.
fn menu(console: &mut dyn Console, setup: &mut Setup) -> Result<i32, String> {
    loop {
        let saved = config::load(&setup.config)
            .ok()
            .and_then(|keys| keys.get(&KeyedService::ElectricityMaps).map(ApiKey::hint));
        let key_item = match saved {
            Some(hint) => format!("Change Electricity Maps API key (now {hint})"),
            None => "Add Electricity Maps API key".to_owned(),
        };
        console.say(&format!(
            "\ncarbontracker init\n  1) {key_item}\n  2) Attach carbontracker to a Slurm script\n  \
             3) Check what would be measured\n  4) Quit"
        ));
        let Some(choice) = console.line("Choose 1-4: ").map_err(cannot_read)? else { return Ok(0) };
        match choice.trim() {
            "1" => {
                if ask_key(console, setup)? {
                    (setup.check)()?;
                }
            }
            "2" => {
                if let Err(message) = (setup.slurm)(console) {
                    console.warn(&format!("! {message}"));
                }
            }
            "3" => {
                (setup.check)()?;
            }
            "4" | "q" => return Ok(0),
            _ => console.say("Choose 1, 2, 3 or 4."),
        }
    }
}

/// Ask for a key until one is saved (`true`) or the user goes back.
fn ask_key(console: &mut dyn Console, setup: &Setup) -> Result<bool, String> {
    loop {
        console.say("Paste your key; it is checked with Electricity Maps before it is saved.");
        let Some(key) = console.secret("Electricity Maps API key (Enter to go back): ").map_err(cannot_read)? else {
            return Ok(false);
        };
        let key = key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        let raw = key;
        let key = ApiKey::new(raw);
        loop {
            console.say("Checking the key with Electricity Maps…");
            match (setup.check_key)(&key) {
                KeyCheck::Works(zones) => {
                    save(&setup.config, raw)?;
                    console.say(&format!("Key works: {}. Saved to {}", zone_summary(&zones), setup.config.display()));
                    return Ok(true);
                }
                KeyCheck::Refused(why) => {
                    console.say(&format!("{why}; nothing saved. Try another key."));
                    break;
                }
                KeyCheck::Unreachable(why) => {
                    console.say(&format!("Could not check the key ({why}). It may still be fine."));
                    let answer = console
                        .line("s to save it unchecked, r to try again, Enter to go back: ")
                        .map_err(cannot_read)?
                        .unwrap_or_default();
                    match answer.trim() {
                        "s" => {
                            save(&setup.config, raw)?;
                            console.warn(&format!("! saved unchecked to {}", setup.config.display()));
                            return Ok(true);
                        }
                        "r" => {}
                        _ => return Ok(false),
                    }
                }
            }
        }
    }
}

fn save(path: &Path, key: &str) -> Result<(), String> {
    config::save(path, KeyedService::ElectricityMaps, key)
}

fn cannot_read(error: io::Error) -> String {
    format!("cannot read the input: {error}")
}

/// `2 zones (DK-DK1, DK-DK2)`, naming at most five.
fn zone_summary(zones: &[String]) -> String {
    const SHOWN: usize = 5;
    if zones.is_empty() {
        return "no zones".to_owned();
    }
    let noun = if zones.len() == 1 { "zone" } else { "zones" };
    let mut list = zones.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if zones.len() > SHOWN {
        list.push_str(&format!(", and {} more", zones.len() - SHOWN));
    }
    format!("{} {noun} ({list})", zones.len())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};
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

    /// The key in the config file at `path`.
    fn saved(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let line = text.lines().find_map(|line| line.strip_prefix("electricity_maps = "))?;
        Some(line.trim_matches('"').to_owned())
    }

    /// Runs the menu on `input`, with key checks answered by `answers` in turn.
    fn run(input: &[&'static str], answers: Vec<KeyCheck>) -> (Scripted, Option<String>, usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let answers = RefCell::new(VecDeque::from(answers));
        let check_key = |_: &ApiKey| answers.borrow_mut().pop_front().expect("one key check too many");
        let checks = Cell::new(0);
        let slurms = Cell::new(0);
        let mut run_check = || {
            checks.set(checks.get() + 1);
            Ok(0)
        };
        let mut run_slurm = |_: &mut dyn Console| {
            slurms.set(slurms.get() + 1);
            Ok(0)
        };
        let mut setup = Setup { config: path.clone(), check_key: &check_key, check: &mut run_check, slurm: &mut run_slurm };
        let mut console = Scripted::new(input);
        assert_eq!(menu(&mut console, &mut setup), Ok(0));
        (console, saved(&path), checks.get(), slurms.get())
    }

    /// AC/FE-65, AC/FE-9
    #[test]
    fn the_menu_offers_each_action_and_comes_back_until_quit() {
        let works = KeyCheck::Works(vec!["DK-DK1".into()]);
        let (console, saved, checks, slurms) = run(&["1", "secret-c3d4", "2", "3", "4"], vec![works]);
        assert!(console.out.contains("1) Add Electricity Maps API key"), "{}", console.out);
        assert!(console.out.contains("2) Attach carbontracker to a Slurm script"));
        assert!(console.out.contains("3) Check what would be measured"));
        assert!(console.out.contains("4) Quit"));
        assert!(console.out.contains("1) Change Electricity Maps API key (now …c3d4)"), "{}", console.out);
        assert!(console.out.contains("Key works: 1 zone (DK-DK1)"));
        assert!(!console.out.contains("secret"), "{}", console.out);
        assert_eq!(saved.as_deref(), Some("secret-c3d4"));
        assert_eq!((checks, slurms), (2, 1), "a check after saving and one chosen; one Slurm");
    }

    /// AC/FE-65
    #[test]
    fn the_menu_ends_with_its_input() {
        let (_, saved, checks, _) = run(&[], vec![]);
        assert_eq!((saved, checks), (None, 0));
    }

    /// AC/FE-67
    #[test]
    fn on_a_terminal_a_refused_key_is_asked_for_again() {
        let refused = KeyCheck::Refused("Electricity Maps refused the key (401 Unauthorized)".into());
        let works = KeyCheck::Works(vec!["DE".into(), "DK-DK1".into()]);
        let (console, saved, checks, _) = run(&["1", "bad-0000", "good-1111", "4"], vec![refused, works]);
        assert!(console.out.contains("refused the key (401 Unauthorized); nothing saved. Try another key."));
        assert_eq!(saved.as_deref(), Some("good-1111"));
        assert_eq!(checks, 1);
    }

    /// AC/FE-67
    #[test]
    fn a_refused_key_never_lands_in_the_file() {
        let refused = KeyCheck::Refused("Electricity Maps refused the key (401 Unauthorized)".into());
        let (_, saved, checks, _) = run(&["1", "bad-0000", "", "4"], vec![refused]);
        assert_eq!((saved, checks), (None, 0));
    }

    /// AC/FE-68
    #[test]
    fn on_a_terminal_an_unreachable_api_offers_to_try_again_or_save_unchecked() {
        let down = || KeyCheck::Unreachable("unavailable: Electricity Maps: timed out".into());
        let (console, saved, checks, _) = run(&["1", "key-2222", "r", "s", "4"], vec![down(), down()]);
        assert_eq!(console.out.matches("Could not check the key (unavailable: Electricity Maps: timed out)").count(), 2);
        assert!(console.err.contains("saved unchecked"), "{}", console.err);
        assert_eq!(saved.as_deref(), Some("key-2222"));
        assert_eq!(checks, 1);

        let (_, saved, _, _) = run(&["1", "key-2222", "", "4"], vec![down()]);
        assert_eq!(saved, None, "going back saves nothing");
    }

    #[test]
    fn many_zones_are_summarised() {
        let zones: Vec<String> = ["DE", "DK-DK1", "DK-DK2", "FR", "GB", "NL", "SE-SE3"].map(String::from).to_vec();
        assert_eq!(zone_summary(&zones), "7 zones (DE, DK-DK1, DK-DK2, FR, GB, and 2 more)");
    }
}

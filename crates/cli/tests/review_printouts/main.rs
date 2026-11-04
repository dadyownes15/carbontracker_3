//! Every standard printout, for a person to read: written to
//! `target/review/printouts.html` in the terminal's font and colours, a tab
//! per frontend and per CLI command. Nothing here checks the wording.
//!
//! Commands run the real binary with colour forced, as on a terminal; runs
//! hand fixed items to the real console sink ([runs]). Regenerate with
//! `cargo review-printouts`.

mod page;
mod report;
mod runs;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use page::{Case, Group, Tab};

#[test]
fn review_printouts() {
    let mut run = runs::cli();
    run.push(runs::sink_failed_at_end());
    let mut python = runs::python();
    python.push(runs::ignored_error());
    let tabs = [
        Tab {
            title: "CLI",
            about: "carbontracker: output on stderr, the summary layout.",
            groups: vec![
                Group { title: "run", about: "carbontracker [OPTIONS] <COMMAND>…", cases: run },
                Group { title: "check", about: "carbontracker check: what would be measured, and each key.", cases: check() },
                Group { title: "init", about: "carbontracker init: saving the Electricity Maps key.", cases: init() },
                Group { title: "report", about: "carbontracker report: the runs recorded, summed, on stdout or saved as CSV.", cases: report() },
                Group { title: "help & errors", about: "Help, and arguments clap or the CLI refuses.", cases: help_and_errors() },
            ],
        },
        Tab {
            title: "Python",
            about: "CarbonTracker(epochs=…): carbontracker 2.4.7's layout, with the prediction.",
            groups: vec![Group { title: "tracking", about: "A tracker's console output, per argument.", cases: python }],
        },
    ];
    let path = review_dir().join("printouts.html");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, page::render(&tabs)).unwrap();
    println!("printouts: {}", path.display());
}

/// Stated power and intensity: nothing is probed and nothing goes online.
const PINNED: [&str; 4] = ["--power", "300", "--intensity", "120"];

fn pinned_check<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    [&PINNED[..], extra, &["check"]].concat()
}

fn check() -> Vec<Case> {
    vec![
        Shell::new().run("No key", "Nothing saved, nothing in the environment.", &pinned_check(&[]), None),
        Shell::new().env("CARBONTRACKER_EM_KEY", "em-env-key-4821").run(
            "Key in the environment",
            "CARBONTRACKER_EM_KEY set, with a stated zone.",
            &pinned_check(&["--zone", "DK-DK2"]),
            None,
        ),
        Shell::new().run(
            "Key given as a flag",
            "--api-keys wins over the environment and the config file.",
            &pinned_check(&["--api-keys", r#"{"electricity_maps": "em-flag-key-9076"}"#]),
            None,
        ),
        Shell::new().saved_key().readable_config().run(
            "Config file readable by others",
            "The key saved by init, then chmod 644.",
            &pinned_check(&[]),
            None,
        ),
    ]
}

fn init() -> Vec<Case> {
    let pinned_init = [&PINNED[..], &["init"]].concat();
    vec![
        Shell::new().run(
            "Saving a key",
            "The key piped in; on a terminal it is asked for with hidden input. Then the check, pinned here.",
            &pinned_init,
            Some("em-saved-key-1234\n"),
        ),
        Shell::new().run("No key given", "An empty line.", &pinned_init, Some("\n")),
    ]
}

fn report() -> Vec<Case> {
    let research = || Shell::new().records(&report::research());
    let profile = || Shell::new().records(&report::profile());
    vec![
        research().run(
            "A research project",
            "Two CLI runs and three Python runs, and a file from a newer release: no intensity, a SIGTERM, a killed run.",
            &["report", "logs"],
            None,
        ),
        research().run("As CSV", "Saves runs.csv; what it leaves out goes to stderr.", &["report", "--csv", "logs"], None),
        research().run("Its spans", "Python's epochs and each CLI command, grouped across runs.", &["report", "--spans", "logs"], None),
        profile().run(
            "A profiled pipeline",
            "Three runs of 1 000 items, four steps each inside batch: energy in joules, no intensity.",
            &["report", "--spans", "profile"],
            None,
        ),
        profile().run(
            "Its spans as CSV",
            "Saves spans.csv, one row per path.",
            &["report", "--spans", "--csv", "profile"],
            None,
        ),
        Shell::new().run("An empty folder", "Nothing recorded yet.", &["report", "."], None),
        Shell::new().run("A missing path", "Exits 2.", &["report", "nowhere"], None),
        Shell::new().run("2.4.7's --parse", "Refused by name.", &["--parse", "logs"], None),
    ]
}

fn help_and_errors() -> Vec<Case> {
    vec![
        Shell::new().run("--help", "The full help.", &["--help"], None),
        Shell::new().run("No command", "carbontracker alone.", &[], None),
        Shell::new().run("An unknown flag", "Clap's own error.", &["--bogus", "true"], None),
        Shell::new().run("A removed flag", "carbontracker 2.4.7's --sim-cpu.", &["--sim-cpu", "Xeon", "true"], None),
        Shell::new().run("Two locations", "At most one location may be given.", &["--zone", "DK", "--country", "DK", "true"], None),
        Shell::new().run("A bad value", "A negative intensity.", &["--intensity", "-5", "true"], None),
        Shell::new().run(
            "A command that doesn't exist",
            "The run starts, the command fails to spawn; its summary's figures change with timing.",
            &[&PINNED[..], &["--no-log", "does-not-exist"]].concat(),
            None,
        ),
    ]
}

/// A fresh home for the binary: no config file and no key of the user's.
struct Shell {
    home: tempfile::TempDir,
    env: Vec<(&'static str, &'static str)>,
}

impl Shell {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap(), env: Vec::new() }
    }

    fn env(mut self, key: &'static str, value: &'static str) -> Self {
        self.env.push((key, value));
        self
    }

    /// Saves a key the way a user would, with init.
    fn saved_key(self) -> Self {
        self.output(&[&PINNED[..], &["init"]].concat(), Some("em-saved-key-1234\n"));
        self
    }

    /// Writes record files into the home, at their paths.
    fn records(self, files: &[(&str, String)]) -> Self {
        for (path, text) in files {
            let path = self.home.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        self
    }

    fn readable_config(self) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let path = self.home.path().join(".config/carbontracker/config.toml");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        self
    }

    fn run(self, title: &'static str, about: &'static str, args: &[&str], stdin: Option<&str>) -> Case {
        let command = std::iter::once("carbontracker")
            .chain(args.iter().copied())
            .map(|arg| if arg.contains([' ', '"', '{']) { format!("'{arg}'") } else { arg.to_owned() })
            .collect::<Vec<_>>()
            .join(" ");
        let command = match stdin {
            Some("\n") => format!("echo '' | {command}"),
            Some(input) => format!("echo {} | {command}", input.trim_end()),
            None => command,
        };
        Case { title, about, command: Some(command), text: self.output(args, stdin) }
    }

    /// stdout and stderr as they interleave on a terminal, the home shown
    /// as `~`.
    fn output(&self, args: &[&str], stdin: Option<&str>) -> String {
        let mut child = Command::new("sh")
            .args(["-c", r#"exec "$0" "$@" 2>&1"#, env!("CARGO_BIN_EXE_carbontracker")])
            .args(args)
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CARBONTRACKER_EM_KEY")
            .env("CLICOLOR_FORCE", "1")
            .envs(self.env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or_default().as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        String::from_utf8(output.stdout).unwrap().replace(&self.home.path().display().to_string(), "~")
    }
}

/// The workspace's `target/review`.
fn review_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    target.join("review")
}

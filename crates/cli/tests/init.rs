//! `carbontracker init` and `init slurm` end to end, with Electricity Maps
//! replaced by a local fake server and never the user's config file.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// The binary, with no key from the environment, no config file but the
/// test's own, and Electricity Maps at `em_url`. AC/TST-1
fn bare(dir: &Path, em_url: &str) -> Command {
    let mut command = Command::cargo_bin("carbontracker").unwrap();
    command
        .current_dir(dir)
        .env_remove("CARBONTRACKER_EM_KEY")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("CARBONTRACKER_EM_URL", em_url);
    command
}

/// With power and intensity pinned, so `check` asks no one.
fn pinned(dir: &Path, em_url: &str) -> Command {
    let mut command = bare(dir, em_url);
    command.args(["--power", "10", "--intensity", "100"]);
    command
}

fn config(dir: &Path) -> PathBuf {
    dir.join("config/carbontracker/config.toml")
}

/// A request the fake server received: its path and its `auth-token`.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A fake Electricity Maps answering every request by its path with
/// `answer`, returning its base URL and what it saw.
fn fake(answer: fn(&str) -> (u16, String)) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v3", listener.local_addr().unwrap());
    let seen = Seen::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let path = request.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap().to_owned();
            let mut token = None;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("auth-token")
                {
                    token = Some(value.trim().to_owned());
                }
            }
            log.lock().unwrap().push((path.clone(), token));
            let (status, body) = answer(&path);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, seen)
}

/// An address nothing listens on.
fn unreachable() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v3", listener.local_addr().unwrap());
    drop(listener);
    url
}

fn two_zones(path: &str) -> (u16, String) {
    match path {
        "/v3/zones" => (200, r#"{"DK-DK1": {"zoneName": "West Denmark"}, "DK-DK2": {"zoneName": "East Denmark"}}"#.into()),
        _ => (404, "{}".into()),
    }
}

/// AC/FE-66, AC/FE-64, AC/FE-9
#[test]
fn a_key_that_works_is_saved_with_the_zones_it_reaches_then_checked() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = fake(two_zones);
    pinned(dir.path(), &url)
        .arg("init")
        .write_stdin("secret-c3d4\n")
        .assert()
        .success()
        .stdout(contains("Key works: 2 zones (DK-DK1, DK-DK2)"))
        .stdout(contains("Saved to"))
        .stdout(contains("  key        electricity_maps …c3d4, from the config file"))
        .stdout(contains("secret").not())
        .stderr(contains("secret").not());
    assert_eq!(*seen.lock().unwrap(), [("/v3/zones".to_owned(), Some("secret-c3d4".to_owned()))]);
    assert_eq!(std::fs::read_to_string(config(dir.path())).unwrap(), "[keys]\nelectricity_maps = \"secret-c3d4\"\n");
}

/// AC/FE-67
#[test]
fn a_refused_key_exits_2_and_is_never_saved() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _) = fake(|_| (401, r#"{"error": "unauthorized"}"#.into()));
    pinned(dir.path(), &url)
        .arg("init")
        .write_stdin("secret-c3d4\n")
        .assert()
        .code(2)
        .stderr(contains("Electricity Maps refused the key"))
        .stderr(contains("nothing saved"));
    assert!(!config(dir.path()).exists());
}

/// AC/FE-68
#[test]
fn an_unreachable_api_saves_the_key_unchecked_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path(), &unreachable())
        .arg("init")
        .write_stdin("secret-c3d4\n")
        .assert()
        .success()
        .stderr(contains("could not check the key"))
        .stderr(contains("saved unchecked"))
        .stdout(contains("  key        electricity_maps …c3d4, from the config file"));
    assert!(config(dir.path()).exists());
}

/// AC/FE-64
#[test]
fn probing_asks_electricity_maps_at_the_address_given() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = fake(|path| match path {
        "/v3/carbon-intensity/latest" => {
            (200, r#"{"zone": "DK-DK1", "carbonIntensity": 210, "datetime": "2026-11-20T10:00:00.000Z"}"#.into())
        }
        _ => (404, "{}".into()),
    });
    bare(dir.path(), &url)
        .env("CARBONTRACKER_EM_KEY", "secret-a1b2")
        .args(["--power", "10", "--zone", "DK-DK1", "check"])
        .assert()
        .success()
        .stdout(contains("Electricity Maps for DK-DK1"));
    assert!(seen.lock().unwrap().iter().any(|(path, _)| path == "/v3/carbon-intensity/latest"));
}

const SCRIPT: &str = "#!/bin/bash -l\n#SBATCH --job-name=train\n#SBATCH --partition=gpu\n#SBATCH --time=02:00:00\n\n\
                      # still the header\nmodule load python\n#SBATCH --ignored\npython -u train.py \"$@\"\n";

fn carbontracker() -> PathBuf {
    assert_cmd::cargo::cargo_bin("carbontracker").canonicalize().unwrap()
}

fn slurm(dir: &Path) -> Command {
    let mut command = bare(dir, &unreachable());
    command.args(["init", "slurm"]);
    command
}

/// AC/FE-69, AC/FE-70, AC/FE-71, AC/FE-76
#[test]
fn init_slurm_writes_a_wrapper_next_to_the_script_that_runs_it_under_carbontracker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("jobs")).unwrap();
    std::fs::write(root.join("jobs/train.sbatch"), SCRIPT).unwrap();

    slurm(&root)
        .args(["jobs/train.sbatch", "--log-dir", "runs"])
        .assert()
        .success()
        .stdout(contains("Wrote jobs/train.ct.sbatch"))
        .stdout(contains("sbatch jobs/train.ct.sbatch"));

    let wrapper = std::fs::read_to_string(root.join("jobs/train.ct.sbatch")).unwrap();
    assert!(
        wrapper.starts_with(
            "#!/bin/bash -l\n#SBATCH --job-name=train\n#SBATCH --partition=gpu\n#SBATCH --time=02:00:00\n"
        ),
        "{wrapper}"
    );
    assert!(!wrapper.contains("--ignored"), "{wrapper}");
    let exec = format!(
        "exec {} --log-dir runs -- /bin/bash -l {} \"$@\"\n",
        carbontracker().display(),
        root.join("jobs/train.sbatch").display()
    );
    assert!(wrapper.ends_with(&exec), "{wrapper}\nwanted {exec}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("jobs/train.ct.sbatch")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable: {mode:o}");
    }
    assert_eq!(std::fs::read_to_string(root.join("jobs/train.sbatch")).unwrap(), SCRIPT, "the script is unchanged");
}

/// AC/FE-69
#[test]
fn a_script_without_an_extension_gets_a_ct_suffix() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("job"), SCRIPT).unwrap();
    slurm(dir.path()).arg("job").assert().success();
    assert!(dir.path().join("job.ct").exists());
}

/// AC/FE-72
#[test]
fn a_wrapper_from_before_is_replaced_but_any_other_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("train.sh"), SCRIPT).unwrap();
    slurm(dir.path()).arg("train.sh").assert().success();
    slurm(dir.path()).args(["train.sh", "--log-dir", "elsewhere"]).assert().success();
    let wrapper = std::fs::read_to_string(dir.path().join("train.ct.sh")).unwrap();
    assert!(wrapper.contains("--log-dir elsewhere"), "{wrapper}");

    std::fs::write(dir.path().join("eval.sh"), SCRIPT).unwrap();
    std::fs::write(dir.path().join("eval.ct.sh"), "mine\n").unwrap();
    slurm(dir.path()).arg("eval.sh").assert().code(2).stderr(contains("eval.ct.sh"));
    assert_eq!(std::fs::read_to_string(dir.path().join("eval.ct.sh")).unwrap(), "mine\n");
}

/// AC/FE-73
#[test]
fn a_wrapper_is_not_wrapped_again() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("train.sh"), SCRIPT).unwrap();
    slurm(dir.path()).arg("train.sh").assert().success();
    slurm(dir.path()).arg("train.ct.sh").assert().code(2).stderr(contains("wrapper"));
    assert!(!dir.path().join("train.ct.ct.sh").exists());
}

/// AC/FE-74
#[test]
fn a_script_without_sbatch_lines_is_warned_about_and_still_wrapped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.sh"), "#!/bin/sh\necho hi\n").unwrap();
    slurm(dir.path()).arg("plain.sh").assert().success().stderr(contains("no #SBATCH lines"));
    assert!(dir.path().join("plain.ct.sh").exists());
}

/// AC/FE-75
#[test]
fn without_a_script_the_slurm_scripts_here_are_listed_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for folder in ["jobs", ".venv", "deep/a/b/c"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    std::fs::write(root.join("jobs/old.sh"), SCRIPT.replace("train", "old")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(root.join("jobs/new.sh"), SCRIPT.replace("train", "new")).unwrap();
    std::fs::write(root.join(".venv/hidden.sh"), SCRIPT).unwrap();
    std::fs::write(root.join("deep/a/b/c/far.sh"), SCRIPT).unwrap();
    std::fs::write(root.join("notes.txt"), "no directives\n").unwrap();
    slurm(root).arg("jobs/old.sh").assert().success();

    let listed = slurm(root)
        .assert()
        .code(2)
        .stderr(contains("carbontracker init slurm <SCRIPT>"))
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8(listed).unwrap();
    let new = listed.find("jobs/new.sh").expect(&listed);
    let old = listed.find("jobs/old.sh").expect(&listed);
    assert!(new < old, "newest first: {listed}");
    assert!(listed.contains("new · gpu · 02:00:00"), "{listed}");
    let old_line = listed.lines().find(|line| line.contains("jobs/old.sh")).unwrap();
    assert!(old_line.contains("(wrapped)"), "{listed}");
    for absent in ["hidden.sh", "far.sh", "notes.txt", "old.ct.sh"] {
        assert!(!listed.contains(absent), "{absent} listed: {listed}");
    }
}

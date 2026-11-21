//! A key never leaves the config file: not in a message, not in an error,
//! not in any file a run writes. Each case gives the binary a sentinel key by
//! one entry point and searches everything it printed and wrote.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

const KEY: &str = "SENTINEL-key-0123456789";

/// The binary, with no key but the case's own, its config and data folders
/// in `dir`, and Electricity Maps where nothing listens. AC/TST-1
fn bare(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("carbontracker").unwrap();
    command
        .current_dir(dir)
        .env_remove("CARBONTRACKER_EM_KEY")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("CARBONTRACKER_EM_URL", "http://127.0.0.1:9/v3");
    command
}

fn config(dir: &Path) -> PathBuf {
    dir.join("config/carbontracker/config.toml")
}

fn write_config(dir: &Path, text: &str) {
    std::fs::create_dir_all(config(dir).parent().unwrap()).unwrap();
    std::fs::write(config(dir), text).unwrap();
}

/// Every file under `dir`, the config file aside.
fn written(dir: &Path) -> Vec<PathBuf> {
    fn walk(at: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.retain(|path| *path != config(dir));
    found
}

/// Runs `command` and fails if the key shows up in its output or in any file
/// under `dir` but the config file.
fn assert_kept(dir: &Path, command: &mut Command) {
    let output = command.output().unwrap();
    for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(KEY), "the key is on {stream}:\n{text}");
    }
    for path in written(dir) {
        let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!text.contains(KEY), "the key is in {}:\n{text}", path.display());
    }
}

fn tracked(dir: &Path) -> Command {
    let mut command = bare(dir);
    command.args(["-v", "--global", "--power", "10", "true"]);
    command
}

/// AC/FE-53, AC/FE-84
#[test]
fn a_malformed_api_keys_flag_is_refused_without_repeating_it() {
    for flag in [format!(r#"{{"electricity_maps": "{KEY}""#), format!(r#"["{KEY}"]"#), format!("electricity_maps={KEY}")] {
        let dir = tempfile::tempdir().unwrap();
        assert_kept(dir.path(), bare(dir.path()).args(["check", "--global", "--api-keys", &flag]));
    }
}

/// AC/FE-84
#[test]
fn an_api_keys_flag_with_a_second_or_unknown_name_is_refused_without_repeating_it() {
    for flag in [
        format!(r#"{{"electricity_maps": "{KEY}", "electricitymaps": "{KEY}"}}"#),
        format!(r#"{{"no_such_service": "{KEY}"}}"#),
    ] {
        let dir = tempfile::tempdir().unwrap();
        assert_kept(dir.path(), bare(dir.path()).args(["check", "--global", "--api-keys", &flag]));
    }
}

/// AC/FE-85, AC/REC-24
#[test]
fn a_key_given_by_flag_is_in_no_file_a_run_writes() {
    for flag in ["--api-keys", "--api_keys"] {
        let dir = tempfile::tempdir().unwrap();
        let keys = format!(r#"{{"electricity_maps": "{KEY}"}}"#);
        let mut command = bare(dir.path());
        command.args(["-v", "--global", "--power", "10", flag, &keys, "true"]);
        assert_kept(dir.path(), &mut command);
        let dir = tempfile::tempdir().unwrap();
        let mut command = bare(dir.path());
        command.args(["-v", "--global", "--power", "10", &format!("{flag}={keys}"), "true"]);
        assert_kept(dir.path(), &mut command);
    }
}

/// AC/FE-84, AC/FE-85
#[test]
fn a_key_from_the_environment_is_in_no_output_or_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_kept(dir.path(), tracked(dir.path()).env("CARBONTRACKER_EM_KEY", KEY));
}

/// AC/FE-84, AC/FE-85
#[test]
fn a_key_from_the_config_file_is_in_no_output_or_other_file() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &format!("[keys]\nelectricity_maps = \"{KEY}\"\n"));
    assert_kept(dir.path(), &mut tracked(dir.path()));
    assert_kept(dir.path(), bare(dir.path()).args(["check", "-v", "--global"]));
}

/// AC/FE-84
#[test]
fn a_config_file_that_cannot_be_read_is_refused_without_repeating_its_keys() {
    for text in [
        format!("[keys]\nelectricity_maps = \"{KEY}\n"),
        format!("[keys]\nno_such_service = \"{KEY}\"\n"),
        format!("[keys]\nelectricity_maps = [\"{KEY}\"]\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), &text);
        assert_kept(dir.path(), bare(dir.path()).args(["check", "--global"]));
    }
}

/// AC/FE-84
#[test]
fn a_key_typed_into_init_is_saved_and_never_printed() {
    let dir = tempfile::tempdir().unwrap();
    assert_kept(dir.path(), bare(dir.path()).args(["init", "--global"]).write_stdin(format!("{KEY}\n")));
    let saved = std::fs::read_to_string(config(dir.path())).unwrap();
    assert!(saved.contains(KEY), "{saved}");
}

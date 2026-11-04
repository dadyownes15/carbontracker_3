//! The `carbontracker` binary end to end. Pinned, or with the location
//! stated, so nothing touches the network; never the user's config file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// The binary, with no key from the environment and no config file but the
/// test's own. AC/TST-1
fn bare(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("carbontracker").unwrap();
    command
        .current_dir(dir)
        .env_remove("CARBONTRACKER_EM_KEY")
        // No config file: the user's own must not leak into a test.
        .env("XDG_CONFIG_HOME", dir.join("config"));
    command
}

/// The binary with power and intensity pinned: no probing at all.
fn pinned(dir: &Path) -> Command {
    let mut command = bare(dir);
    command.args(["--power", "10", "--intensity", "100"]);
    command
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| entries.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn stderr(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
}

fn stdout(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

/// The only run folder in `logs`.
fn only_folder(logs: &Path) -> PathBuf {
    let [folder] = files(logs).try_into().expect("one run folder");
    logs.join(folder)
}

/// The record in a run folder, line by line.
fn record(folder: &Path) -> Vec<serde_json::Value> {
    let name = files(folder).into_iter().find(|name| name.ends_with(".ct")).expect("a record");
    std::fs::read_to_string(folder.join(name))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Whether `line`, after the prefix, starts with a time `HH:MM:SS`.
fn timed(line: &str) -> bool {
    let rest = line.strip_prefix("CarbonTracker: ").unwrap_or("");
    let bytes = rest.as_bytes();
    bytes.len() > 8
        && [0, 1, 3, 4, 6, 7].iter().all(|&at| bytes[at].is_ascii_digit())
        && bytes[2] == b':'
        && bytes[5] == b':'
}

/// AC/FE-28, AC/FE-45
#[test]
fn the_exit_code_is_the_commands_and_the_closing_block_follows() {
    let dir = tempfile::tempdir().unwrap();
    let run = pinned(dir.path()).args(["sh", "-c", "exit 3"]).assert().code(3);
    let stderr = stderr(&run);
    let closing = stderr.find("sh -c exit 3 summary:\n").expect("the closing block");
    for row in ["Time:", "Energy:", "kWh", "Intensity:", "gCO2eq/kWh", "CO2eq:", "km travelled by car", "Logs:       ./logs/run-"] {
        assert!(stderr[closing..].contains(row), "{row} missing: {stderr}");
    }
}

/// AC/FE-29
#[cfg(unix)]
#[test]
fn a_command_killed_by_a_signal_exits_128_plus_its_number() {
    let dir = tempfile::tempdir().unwrap();
    let run = pinned(dir.path()).args(["sh", "-c", "kill -TERM $$"]).assert().code(128 + 15);
    assert!(stderr(&run).contains(" summary:\n"), "{}", stderr(&run));
    let lines = record(&only_folder(&dir.path().join("logs")));
    let outcome = lines.iter().find(|line| line["kind"] == "outcome").expect("an outcome line");
    assert_eq!(*outcome, serde_json::json!({"at": outcome["at"], "kind": "outcome", "ended": "signalled", "signal": 15}));
}

/// AC/FE-2
#[test]
fn the_commands_stdout_passes_through_untouched() {
    let dir = tempfile::tempdir().unwrap();
    // Verbose and logging, so carbontracker has the most to say.
    let run = pinned(dir.path()).args(["-v", "sh", "-c", "printf 'one\\ntwo\\n'"]).assert().success().stdout("one\ntwo\n");
    assert!(stderr(&run).contains("ready. Starting"), "{}", stderr(&run));
}

/// AC/FE-59
#[test]
fn the_commands_stdin_is_its_own() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["--no-log", "cat"]).write_stdin("first\nsecond\n").assert().success().stdout("first\nsecond\n");
}

/// AC/FE-47, AC/FE-22, AC/RUN-22, AC/REC-1, AC/REC-15
#[test]
fn each_run_gets_a_folder_in_logs_with_its_record_and_output() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["sh", "-c", "sleep 0.2"]).assert().success().stderr(contains("Logs:       ./logs/run-"));

    let folder = only_folder(&dir.path().join("logs"));
    // run-YYYYMMDD_HHMMSS-<id>, the id eight lowercase letters and digits.
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    let (stamp, id) = name.strip_prefix("run-").unwrap().rsplit_once('-').unwrap();
    let stamp = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d_%H%M%S").expect(&name);
    let local = chrono::Local::now().naive_local();
    assert!((local - stamp).num_seconds().abs() < 60, "the time is local: {name}");
    assert!(id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()), "{name}");
    assert_eq!(files(&folder), ["output.log".to_owned(), format!("run-{id}.ct")]);

    let lines = record(&folder);
    let run = &lines[0];
    assert_eq!((&run["kind"], &run["format"], &run["frontend"]), (&"run".into(), &1.into(), &"cli".into()));
    let command: Vec<String> = serde_json::from_value(run["command"].clone()).unwrap();
    assert!(command.ends_with(&["sh".to_owned(), "-c".to_owned(), "sleep 0.2".to_owned()]), "{command:?}");
    assert_eq!((&lines[1]["kind"], &lines[2]["kind"]), (&"probe".into(), &"catalog".into()));
    let outcome = lines.iter().find(|line| line["kind"] == "outcome").expect("an outcome line");
    assert_eq!((&outcome["ended"], &outcome["code"]), (&"exited".into(), &0.into()));
    assert_eq!(lines.last().unwrap()["kind"], "session");

    let output = std::fs::read_to_string(folder.join("output.log")).unwrap();
    assert!(output.contains("sh -c sleep 0.2 summary:"), "{output}");
}

/// AC/FE-48, AC/FE-46
#[test]
fn verbose_adds_timed_steps_but_output_log_keeps_to_normal() {
    let dir = tempfile::tempdir().unwrap();
    let verbose = pinned(dir.path()).args(["-v", "sh", "-c", "sleep 0.2"]).assert().success();
    let console = stderr(&verbose);
    let steps: Vec<&str> =
        console.lines().filter(|line| line.contains("probing static") || line.contains("readout: ")).collect();
    assert!(steps.iter().any(|line| line.contains("probing static")), "{console}");
    assert!(steps.iter().any(|line| line.contains("readout: ")), "{console}");
    assert!(steps.iter().all(|line| timed(line)), "{steps:?}");
    assert!(console.contains("sh -c sleep 0.2 started"), "{console}");

    let output = std::fs::read_to_string(only_folder(&dir.path().join("logs")).join("output.log")).unwrap();
    assert!(output.contains("sh -c sleep 0.2 summary:"), "{output}");
    assert!(!output.contains("readout: ") && !output.contains("probing"), "{output}");

    let normal = pinned(dir.path()).args(["--no-log", "sh", "-c", "sleep 0.2"]).assert().success();
    let console = stderr(&normal);
    assert!(console.lines().all(|line| !timed(line)), "{console}");
    assert!(!console.contains("readout: ") && !console.contains("probing"), "{console}");
    assert!(console.contains(" summary:\n"), "{console}");
}

/// AC/FE-44
#[test]
fn before_the_command_stderr_lists_what_is_measured() {
    let dir = tempfile::tempdir().unwrap();
    let run = pinned(dir.path()).args(["--no-log", "sh", "-c", "echo out >&2"]).assert().success();
    let console = stderr(&run);
    let ready = console.find("CarbonTracker: ready. Starting sh -c echo out >&2\n").expect("ready");
    let started = console.find("\nout\n").expect("the command's own line");
    assert!(ready < started, "{console}");
    let before = &console[..ready];
    for (kind, fidelity) in [("power ", "stated"), ("intensity ", "stated"), ("forecast ", "stated")] {
        let line = before.lines().find(|line| line.starts_with(&format!("CarbonTracker: {kind}"))).unwrap_or_else(|| panic!("{kind}: {console}"));
        assert!(line.contains(fidelity), "{line}");
    }
}

/// AC/FE-51, AC/RUN-22
#[test]
fn no_log_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["--no-log", "true"]).assert().success();
    assert!(files(dir.path()).is_empty());
}

/// AC/FE-49
#[test]
fn runs_started_together_never_share_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("carbontracker");
    let children: Vec<_> = (0..4)
        .map(|_| {
            std::process::Command::new(&binary)
                .current_dir(dir.path())
                .env_remove("CARBONTRACKER_EM_KEY")
                .env("XDG_CONFIG_HOME", dir.path().join("config"))
                .args(["--power", "10", "--intensity", "100", "true"])
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let folders = files(&dir.path().join("logs"));
    assert_eq!(folders.len(), 4, "{folders:?}");
    for folder in folders {
        assert_eq!(files(&dir.path().join("logs").join(folder)).len(), 2);
    }
}

/// AC/FE-50, AC/FE-8
#[test]
fn a_log_directory_that_cannot_be_written_exits_2_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("taken"), "a file, not a folder").unwrap();
    let run = pinned(dir.path()).args(["--log-dir", "taken/logs", "touch", "ran"]).assert().code(2);
    let stderr = stderr(&run);
    assert!(stderr.contains("taken/logs"), "{stderr}");
    assert!(!stderr.contains("gathering sources"), "nothing is probed: {stderr}");
    assert!(!dir.path().join("ran").exists());
}

/// AC/FE-40, AC/FE-6
#[test]
fn underscore_spellings_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path())
        .args(["--log_dir", "out", "--api_keys", "{'electricitymaps': 'k'}", "true"])
        .assert()
        .success()
        .stderr(contains("can be read by others").not());
    assert_eq!(files(&dir.path().join("out")).len(), 1, "one run folder");
}

/// AC/FE-39, AC/FE-8
#[test]
fn removed_flags_exit_2_naming_their_replacement() {
    let dir = tempfile::tempdir().unwrap();
    for (flag, instead) in [
        ("--parse", "carbontracker report"),
        ("--report", "carbontracker report"),
        ("--sim-cpu", "--cpu-power"),
        ("--sim-cpu-tdp", "--cpu-power"),
        ("--sim-cpu-util", "--cpu-power"),
        ("--sim-gpu", "--gpu-power"),
        ("--sim-gpu-watts", "--gpu-power"),
        ("--sim-gpu-util", "--gpu-power"),
    ] {
        let run = pinned(dir.path()).args([flag, "1", "touch", "ran"]).assert().code(2);
        let stderr = stderr(&run);
        assert!(stderr.contains(&format!("{flag} was removed in carbontracker 3: ")), "{stderr}");
        assert!(stderr.contains(instead) && !stderr.contains("unexpected argument"), "{stderr}");
    }
    assert!(!dir.path().join("ran").exists());
}

/// AC/FE-41, AC/FE-8, AC/FE-3, AC/FE-37, AC/FE-38
#[test]
fn every_bad_value_exits_2_naming_it_before_probing() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["--zone="], "--zone"),
        (&["--country", "Denmark"], "--country"),
        (&["--coordinates", "55.68"], "--coordinates"),
        (&["--coordinates", "95,0"], "--coordinates"),
        (&["--coordinates", "0,181"], "--coordinates"),
        (&["--power", "abc"], "--power"),
        (&["--power=-1"], "--power"),
        (&["--cpu-power=-1"], "--cpu-power"),
        (&["--gpu-power", "lots"], "--gpu-power"),
        (&["--intensity", "0"], "--intensity"),
        (&["--intensity", "abc"], "--intensity"),
        (&["--interval", "0"], "--interval"),
        (&["--interval", "abc"], "--interval"),
        (&["--pue", "0.5"], "--pue"),
        (&["--components", "tpu"], "--components"),
        (&["--api-keys", "electricitymaps=k"], "--api-keys"),
        (&["--api-keys", "{'carbonintensityGB': 'k'}"], "Supported providers: electricity_maps, electricitymaps"),
        (&["--api-keys", "{'electricitymaps': 'a', 'electricity_maps': 'b'}"], "more than one key"),
        (&["--zone", "DK-DK2", "--global"], "--zone"),
        (&["--country", "DK", "--coordinates", "55.68,12.57"], "--coordinates"),
        (&["--power", "1", "--cpu-power", "1"], "--cpu-power"),
        (&["--power", "1", "--gpu-power", "1"], "--gpu-power"),
        (&["--no-log", "--log-dir", "out"], "--log-dir"),
    ];
    for (flags, named) in cases {
        // A location is stated, or the intensity pinned, so a flag let
        // through would never reach the network.
        let location = ["--zone", "--country", "--coordinates"].iter().any(|flag| flags[0].starts_with(flag));
        let base: &[&str] = if location { &["--intensity", "100"] } else { &["--global"] };
        let run = bare(dir.path()).args(base).args(*flags).args(["touch", "ran"]).assert().code(2);
        let stderr = stderr(&run);
        assert!(stderr.contains(named), "{flags:?} should name {named}: {stderr}");
        assert!(!stderr.contains("gathering sources"), "{flags:?} probed: {stderr}");
        assert!(!dir.path().join("ran").exists(), "{flags:?} ran the command");
    }
}

/// AC/FE-37, AC/FE-4
#[test]
fn a_cpu_power_replaces_the_cpu_and_leaves_the_rest_probed() {
    let dir = tempfile::tempdir().unwrap();
    bare(dir.path())
        .args(["--no-log", "-v", "--global", "--cpu-power", "50", "true"])
        .assert()
        .success()
        .stderr(contains("static-cpu: serving stated static (cpu)"))
        .stderr(contains("generic-cpu: covered by a better candidate"));
}

/// AC/FE-37, AC/FE-4
#[test]
fn a_gpu_power_replaces_the_gpu() {
    let dir = tempfile::tempdir().unwrap();
    bare(dir.path())
        .args(["--no-log", "-v", "--global", "--gpu-power", "300", "true"])
        .assert()
        .success()
        .stderr(contains("static-gpu: serving stated static (gpu)"))
        .stderr(contains("readout: static (gpu) 300 W"));
}

/// AC/FE-52, AC/FE-53, AC/FE-54
#[test]
fn check_shows_how_each_value_is_obtained_and_where_the_key_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let check = pinned(dir.path())
        .env("CARBONTRACKER_EM_KEY", "secret-a1b2")
        .arg("check")
        .assert()
        .success()
        .stderr(contains("secret").not());
    let stdout = stdout(&check);
    for row in [
        "  power      stated     system\n",
        "  intensity  stated     the given value\n",
        "  forecast   stated     the given value, 24 h\n",
        "  key        electricity_maps …a1b2, from CARBONTRACKER_EM_KEY\n",
        "  providers  static\n",
    ] {
        assert!(stdout.contains(row), "{row:?} missing: {stdout}");
    }
    assert!(stdout.find("  key ") < stdout.find("  providers "), "key above providers: {stdout}");
    assert!(!stdout.contains("secret"), "{stdout}");
    assert!(!stdout.contains("static: serving"), "verdicts only with -v: {stdout}");
    assert!(!stdout.contains('\x1b'), "no colour when piped: {stdout:?}");
    assert!(files(dir.path()).is_empty(), "check writes nothing: {:?}", files(dir.path()));
}

/// AC/FE-60
#[test]
fn check_verbose_adds_every_verdict() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path())
        .args(["check", "-v"])
        .assert()
        .success()
        .stdout(contains("  power      stated     system"))
        .stdout(contains("static: serving"))
        .stdout(contains("ip-geolocation: not asked for"));
}

/// Writes the config file `init` would, with `mode`.
fn config_file(dir: &Path, key: &str, mode: u32) {
    let path = dir.join("config/carbontracker/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("[keys]\nelectricity_maps = \"{key}\"\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = mode;
}

/// AC/FE-42, AC/FE-7
#[test]
fn a_flag_key_wins_over_the_environment_which_wins_over_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    config_file(dir.path(), "file-1111", 0o600);
    let check = |env: bool, flag: bool| {
        let mut command = pinned(dir.path());
        if env {
            command.env("CARBONTRACKER_EM_KEY", "envv-2222");
        }
        if flag {
            command.args(["--api-keys", r#"{"electricity_maps": "flag-3333"}"#]);
        }
        stdout(&command.arg("check").assert().success())
    };
    assert!(check(true, true).contains("electricity_maps …3333, from --api-keys"));
    assert!(check(true, false).contains("electricity_maps …2222, from CARBONTRACKER_EM_KEY"));
    assert!(check(false, false).contains("electricity_maps …1111, from the config file"));
}

/// AC/FE-43
#[cfg(unix)]
#[test]
fn a_config_file_others_can_read_is_warned_about_and_the_run_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    config_file(dir.path(), "file-1111", 0o644);
    pinned(dir.path())
        .args(["--no-log", "sh", "-c", "exit 0"])
        .assert()
        .success()
        .stderr(contains("config.toml can be read by others: chmod 600"))
        .stderr(contains(" summary:\n"));
}

/// AC/FE-9, AC/FE-55, AC/FE-56
#[test]
fn init_saves_the_key_from_stdin_then_checks() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path())
        .arg("init")
        .write_stdin("secret-c3d4\n")
        .assert()
        .success()
        .stdout(contains("Saved to"))
        .stdout(contains("  power      stated     system"))
        .stdout(contains("  key        electricity_maps …c3d4, from the config file"))
        .stdout(contains("secret").not());

    let path = dir.path().join("config/carbontracker/config.toml");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[keys]\nelectricity_maps = \"secret-c3d4\"\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    pinned(dir.path()).arg("check").assert().stdout(contains("…c3d4, from the config file"));
}

/// AC/FE-55
#[test]
fn init_takes_no_key_as_a_flag() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["init", "--key", "secret-c3d4"]).write_stdin("").assert().code(2);
    assert!(!dir.path().join("config").exists());
}

/// AC/FE-57
#[test]
fn an_empty_key_exits_2_and_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).arg("init").write_stdin("  \n").assert().code(2).stderr(contains("no key given"));
    assert!(!dir.path().join("config/carbontracker/config.toml").exists());
}

/// AC/FE-35
#[test]
fn a_command_named_check_runs_after_a_double_dash() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["--no-log", "--", "check"]).assert().code(127).stderr(contains("command not found: check"));
}

/// AC/FE-36
#[test]
fn no_command_prints_the_usage_on_stderr_and_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    for args in [&[][..], &["--no-log"][..]] {
        let run = bare(dir.path()).args(args).assert().code(2).stdout("");
        assert!(stderr(&run).contains("carbontracker [OPTIONS]"), "{args:?}: {}", stderr(&run));
    }
}

/// AC/FE-30, AC/FE-45
#[test]
fn a_missing_command_exits_127() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path())
        .args(["--no-log", "no-such-command-carbontracker"])
        .assert()
        .code(127)
        .stderr(contains("command not found: no-such-command-carbontracker"))
        .stderr(contains("no-such-command-carbontracker summary:"));
}

/// AC/FE-30
#[cfg(unix)]
#[test]
fn a_command_that_cannot_run_exits_126() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("not-executable"), "#!/bin/sh\n").unwrap();
    pinned(dir.path())
        .args(["--no-log", "./not-executable"])
        .assert()
        .code(126)
        .stderr(contains("cannot run ./not-executable"));
}

/// AC/FE-3, AC/FE-8
#[test]
fn an_invalid_location_exits_2_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    pinned(dir.path()).args(["--no-log", "--coordinates", "95,0", "touch", "ran"]).assert().code(2);
    assert!(!dir.path().join("ran").exists());
}

/// Starts the pinned binary on `command` in its own process group, as a
/// terminal's foreground job, and waits until the command has started.
#[cfg(unix)]
fn started(dir: &Path, command: &str) -> std::process::Child {
    use std::os::unix::process::CommandExt;

    let binary = assert_cmd::cargo::cargo_bin("carbontracker");
    let child = std::process::Command::new(binary)
        .current_dir(dir)
        .env_remove("CARBONTRACKER_EM_KEY")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .args(["--power", "10", "--intensity", "100", "sh", "-c", command])
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let started = dir.join("started");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !started.exists() {
        assert!(std::time::Instant::now() < deadline, "the command never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// AC/FE-32, AC/RUN-21
#[cfg(unix)]
#[test]
fn ctrl_c_reaches_the_command_and_the_run_is_still_accounted() {
    let dir = tempfile::tempdir().unwrap();
    let child = started(dir.path(), "touch started; exec sleep 30");
    // Like a terminal's Ctrl-C: SIGINT to the whole group.
    let group = format!("-{}", child.id());
    std::process::Command::new("kill").args(["-INT", "--", &group]).status().unwrap();

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(stderr.contains(" summary:"), "{stderr}");
    let lines = record(&only_folder(&dir.path().join("logs")));
    let outcome = lines.iter().find(|line| line["kind"] == "outcome").expect("an outcome line");
    assert_eq!((&outcome["ended"], &outcome["signal"]), (&"signalled".into(), &2.into()));
    assert_eq!(lines.last().unwrap()["kind"], "session");
}

/// AC/FE-33
#[cfg(unix)]
#[test]
fn a_sigterm_to_carbontracker_is_passed_on_to_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let child = started(dir.path(), "touch started; exec sleep 30");
    // carbontracker alone, not its group: only forwarding reaches the command.
    std::process::Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(128 + 15), "{stderr}");
    assert!(stderr.contains(" summary:"), "{stderr}");
    let lines = record(&only_folder(&dir.path().join("logs")));
    assert_eq!(lines.last().unwrap()["kind"], "session");
}

/// Runs a pinned command, so `logs/` gets a real record.
fn tracked(dir: &Path, command: &str) {
    let _ = pinned(dir).args(["sh", "-c", command]).assert();
}

/// AC/FE-21, AC/FE-22
#[test]
fn report_sums_the_runs_in_a_folder_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    tracked(dir.path(), "exit 4");
    let run = bare(dir.path()).args(["report", "logs"]).assert().success();
    let stdout = stdout(&run);
    assert!(stdout.contains("exited 0") && stdout.contains("exited 4"), "{stdout}");
    assert!(stdout.contains("Total (2 runs)"), "{stdout}");
    assert!(stdout.contains("The training of models in this work is estimated to use"), "{stdout}");
}

/// AC/FE-26, AC/FE-62
#[test]
fn report_csv_saves_one_row_per_run_in_runs_csv_next_to_the_logs() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    tracked(dir.path(), "exit 0");
    let run = bare(dir.path()).args(["report", "--csv", "logs"]).assert().success();
    assert_eq!(stdout(&run), "");
    assert!(stderr(&run).contains("runs.csv"), "{}", stderr(&run));
    let csv = std::fs::read_to_string(dir.path().join("logs/runs.csv")).unwrap();
    assert_eq!(csv.lines().count(), 3, "{csv}");
    assert!(!dir.path().join("runs.csv").exists());
    assert!(csv.starts_with("file,started,"), "{csv}");
}

/// AC/FE-62
#[test]
fn report_spans_csv_saves_spans_csv_over_an_old_one() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    std::fs::write(dir.path().join("logs/spans.csv"), "stale\n").unwrap();
    let run = bare(dir.path()).args(["report", "--spans", "--csv", "logs"]).assert().success();
    assert!(stderr(&run).contains("spans.csv"), "{}", stderr(&run));
    let csv = std::fs::read_to_string(dir.path().join("logs/spans.csv")).unwrap();
    assert!(csv.starts_with("path,count,"), "{csv}");
    assert!(!dir.path().join("logs/runs.csv").exists());
}

/// AC/FE-62
#[test]
fn report_csv_saves_to_the_path_given_with_o() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    let run = bare(dir.path()).args(["report", "--csv", "-o", "mine.csv", "logs"]).assert().success();
    assert!(stderr(&run).contains("mine.csv"), "{}", stderr(&run));
    let csv = std::fs::read_to_string(dir.path().join("mine.csv")).unwrap();
    assert!(csv.starts_with("file,started,"), "{csv}");
    assert!(!dir.path().join("logs/runs.csv").exists());
}

/// AC/FE-62
#[test]
fn a_saved_csv_is_not_read_back_as_a_run() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    bare(dir.path()).args(["report", "--csv", "logs"]).assert().success();
    let run = bare(dir.path()).args(["report", "logs"]).assert().success();
    assert!(stdout(&run).contains("Total (1 run)"), "{}", stdout(&run));
}

/// AC/FE-25
#[test]
fn report_spans_groups_by_path() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    let run = bare(dir.path()).args(["report", "--spans", "logs"]).assert().success();
    let stdout = stdout(&run);
    assert!(stdout.starts_with("Span "), "{stdout}");
    assert!(stdout.contains("sh -c exit 0"), "{stdout}");
}

/// AC/FE-24
#[test]
fn report_names_a_file_it_cannot_read_and_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    tracked(dir.path(), "exit 0");
    std::fs::write(dir.path().join("logs/broken.ct"), "not json\n").unwrap();
    bare(dir.path())
        .args(["report", "logs"])
        .assert()
        .success()
        .stdout(contains("Total (1 run)").and(contains("broken.ct could not be read")));
}

/// AC/FE-58
#[test]
fn report_on_a_missing_path_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    bare(dir.path()).args(["report", "nowhere"]).assert().code(2).stderr(contains("nowhere"));
}

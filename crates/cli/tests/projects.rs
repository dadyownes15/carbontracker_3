//! `carbontracker report --project` end to end, over runs saved as the Python
//! package saves them: a record whose header names its project, linked from
//! the project's folder. Pinned, never the user's config or data folder.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

/// The binary, with no key, and config and data folders of the test's own.
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

fn stdout(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

fn output(assert: &assert_cmd::assert::Assert) -> String {
    let out = assert.get_output();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// The project's folder under the test's data folder.
fn project_folder(dir: &Path, project: &str) -> PathBuf {
    dir.join("data/carbontracker/projects").join(project)
}

/// A pinned run of `exit <code>` saved in `dir/<logs>`, its header naming
/// `project` and `name` as given, and linked from the project's folder, as
/// a Python run with `project=` and `name=` is. Returns its run folder.
fn run(dir: &Path, logs: &str, code: u8, project: Option<&str>, name: Option<&str>) -> PathBuf {
    let logs = dir.join(logs);
    let before: Vec<_> = folders(&logs);
    let _ = bare(dir)
        .args(["--power", "10", "--intensity", "100", "--log-dir"])
        .arg(&logs)
        .args(["sh", "-c", &format!("exit {code}")])
        .assert();
    let folder = folders(&logs).into_iter().find(|folder| !before.contains(folder)).expect("a new run folder");
    let record = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "ct"))
        .unwrap();
    let text = std::fs::read_to_string(&record).unwrap();
    let (header, rest) = text.split_once('\n').unwrap();
    let mut header: serde_json::Value = serde_json::from_str(header).unwrap();
    if let Some(project) = project {
        header["project"] = project.into();
    }
    if let Some(name) = name {
        header["name"] = name.into();
    }
    std::fs::write(&record, format!("{header}\n{rest}")).unwrap();
    if let Some(project) = project {
        link(dir, project, &folder);
    }
    folder
}

#[cfg(unix)]
fn link(dir: &Path, project: &str, folder: &Path) {
    let links = project_folder(dir, project);
    std::fs::create_dir_all(&links).unwrap();
    std::os::unix::fs::symlink(folder.canonicalize().unwrap(), links.join(folder.file_name().unwrap())).unwrap();
}

#[cfg(not(unix))]
fn link(_dir: &Path, _project: &str, _folder: &Path) {}

fn folders(logs: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(logs)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).filter(|path| path.is_dir()).collect())
        .unwrap_or_default()
}

/// AC/PRJ-1, AC/PRJ-11
#[cfg(unix)]
#[test]
fn report_project_reads_the_projects_runs_from_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "laptop/logs", 0, Some("codeq"), None);
    run(dir.path(), "cluster/logs", 4, Some("codeq"), None);
    run(dir.path(), "other/logs", 0, Some("paper"), None);
    std::fs::create_dir(dir.path().join("elsewhere")).unwrap();

    let report = bare(dir.path())
        .current_dir(dir.path().join("elsewhere"))
        .args(["report", "--project", "codeq"])
        .assert()
        .success();
    let stdout = stdout(&report);
    assert!(stdout.contains("exited 0") && stdout.contains("exited 4"), "{stdout}");
    assert!(stdout.contains("Total (2 runs)"), "{stdout}");
}

/// AC/PRJ-12
#[test]
fn report_project_with_paths_keeps_only_the_projects_runs() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, Some("codeq"), None);
    run(dir.path(), "logs", 4, Some("paper"), None);
    run(dir.path(), "logs", 5, None, None);

    let report = bare(dir.path()).args(["report", "--project", "codeq", "logs"]).assert().success();
    let stdout = stdout(&report);
    assert!(stdout.contains("exited 3"), "{stdout}");
    assert!(!stdout.contains("exited 4") && !stdout.contains("exited 5"), "{stdout}");
    assert!(stdout.contains("Total (1 run)"), "{stdout}");
}

/// AC/PRJ-13
#[cfg(unix)]
#[test]
fn report_project_keeps_the_projects_below_it_and_not_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, Some("saintslab/codeq"), None);
    run(dir.path(), "logs", 4, Some("saintslab/paper"), None);
    run(dir.path(), "logs", 5, Some("saintslab-old"), None);

    for paths in [&[][..], &["logs"][..]] {
        let report = bare(dir.path()).args(["report", "--project", "saintslab"]).args(paths).assert().success();
        let stdout = stdout(&report);
        assert!(stdout.contains("exited 3") && stdout.contains("exited 4"), "{stdout}");
        assert!(!stdout.contains("exited 5"), "{stdout}");
    }
}

/// AC/PRJ-14
#[test]
fn report_project_with_no_folder_exits_2_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    bare(dir.path()).args(["report", "--project", "nothing"]).assert().code(2).stderr(contains("nothing"));
}

/// AC/PRJ-15
#[test]
fn report_refuses_a_project_name_it_cannot_take() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["../escape", "/abs", "a//b", ".hidden", "with space", ""] {
        bare(dir.path()).args(["report", "--project", name, "."]).assert().code(2).stderr(contains("project"));
    }
}

/// AC/PRJ-16
#[test]
fn report_without_project_or_log_folder_asks_for_a_path() {
    let dir = tempfile::tempdir().unwrap();
    bare(dir.path()).args(["report"]).assert().code(2).stderr(contains("carbontracker report PATH"));
}

/// AC/PRJ-17, AC/PRJ-22
#[cfg(unix)]
#[test]
fn a_link_to_a_deleted_run_is_named_and_left_in_place() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, Some("codeq"), None);
    let gone = run(dir.path(), "logs", 4, Some("codeq"), None);
    std::fs::remove_dir_all(&gone).unwrap();

    let report = bare(dir.path()).args(["report", "--project", "codeq"]).assert().success();
    let text = output(&report);
    let name = gone.file_name().unwrap().to_string_lossy().into_owned();
    assert!(text.contains(&name), "{text}");
    assert!(text.contains("Total (1 run)"), "{text}");
    let link = project_folder(dir.path(), "codeq").join(&name);
    assert!(std::fs::symlink_metadata(&link).is_ok(), "the broken link was removed");
}

/// AC/PRJ-18
#[cfg(unix)]
#[test]
fn a_record_reached_by_its_link_and_its_path_is_read_once() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, Some("codeq"), None);
    let report = bare(dir.path())
        .args(["report", "logs"])
        .arg(project_folder(dir.path(), "codeq"))
        .assert()
        .success();
    assert!(stdout(&report).contains("Total (1 run)"), "{}", stdout(&report));
}

/// AC/PRJ-19
#[test]
fn the_runs_table_shows_a_run_by_its_name() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, None, Some("sweep-bits-4"));
    run(dir.path(), "logs", 4, None, None);

    let report = bare(dir.path()).args(["report", "logs"]).assert().success();
    let stdout = stdout(&report);
    let header = stdout.lines().next().unwrap();
    assert!(header.contains("Run") && !header.contains("Command"), "{stdout}");
    let named = stdout.lines().find(|line| line.contains("exited 3")).unwrap();
    assert!(named.contains("sweep-bits-4") && !named.contains("sh -c"), "{stdout}");
    let unnamed = stdout.lines().find(|line| line.contains("exited 4")).unwrap();
    assert!(unnamed.contains("sh"), "{stdout}");
}

/// AC/PRJ-20
#[test]
fn the_runs_csv_has_project_and_name_columns() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), "logs", 3, Some("saintslab/codeq"), Some("sweep-bits-4"));
    run(dir.path(), "logs", 4, None, None);

    bare(dir.path()).args(["report", "--csv", "-o", "runs.csv", "logs"]).assert().success();
    let csv = std::fs::read_to_string(dir.path().join("runs.csv")).unwrap();
    let mut rows = csv.lines().map(|line| line.split(',').collect::<Vec<_>>());
    let header = rows.next().unwrap();
    let column = |name: &str| header.iter().position(|cell| *cell == name).unwrap_or_else(|| panic!("{csv}"));
    let (project, name, code) = (column("project"), column("name"), column("code"));
    let rows: Vec<_> = rows.collect();
    let named = rows.iter().find(|row| row[code] == "3").unwrap();
    assert_eq!((named[project], named[name]), ("saintslab/codeq", "sweep-bits-4"));
    let unnamed = rows.iter().find(|row| row[code] == "4").unwrap();
    assert_eq!((unnamed[project], unnamed[name]), ("", ""));
}

/// The record's header in the only run folder under `logs`.
fn header(logs: &Path) -> serde_json::Value {
    let [folder] = folders(logs).try_into().expect("one run folder");
    let record = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "ct"))
        .unwrap();
    let text = std::fs::read_to_string(record).unwrap();
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}

fn tracking(dir: &Path) -> Command {
    let mut command = bare(dir);
    command.args(["--power", "10", "--intensity", "100"]);
    command
}

/// AC/PRJ-6, AC/PRJ-23, AC/PRJ-24
#[test]
fn a_tracked_command_takes_its_project_and_name() {
    let dir = tempfile::tempdir().unwrap();
    tracking(dir.path())
        .args(["--project", "saintslab/codeq", "--name", "sweep bits 4", "sh", "-c", "exit 3"])
        .assert()
        .code(3);
    let header = header(&dir.path().join("logs"));
    assert_eq!((header["project"].as_str(), header["name"].as_str()), (Some("saintslab/codeq"), Some("sweep bits 4")));
}

/// AC/PRJ-6
#[test]
fn a_tracked_command_without_them_has_no_such_field() {
    let dir = tempfile::tempdir().unwrap();
    tracking(dir.path()).args(["true"]).assert().success();
    let header = header(&dir.path().join("logs"));
    assert!(header.get("project").is_none() && header.get("name").is_none(), "{header}");
}

/// AC/PRJ-8, AC/PRJ-23
#[cfg(unix)]
#[test]
fn a_tracked_command_is_linked_from_its_projects_folder() {
    let dir = tempfile::tempdir().unwrap();
    tracking(dir.path()).args(["--project", "codeq", "true"]).assert().success();
    let [folder] = folders(&dir.path().join("logs")).try_into().unwrap();
    let link = project_folder(dir.path(), "codeq").join(folder.file_name().unwrap());
    assert_eq!(std::fs::read_link(&link).unwrap(), folder.canonicalize().unwrap());

    let report = bare(dir.path()).args(["report", "--project", "codeq"]).assert().success();
    assert!(stdout(&report).contains("Total (1 run)"), "{}", stdout(&report));
}

/// AC/PRJ-25
#[test]
fn a_refused_project_or_name_exits_2_and_never_starts_the_command() {
    let dir = tempfile::tempdir().unwrap();
    for (flag, value) in [("--project", "../escape"), ("--project", ""), ("--name", " "), ("--name", "two\nlines")] {
        let run = tracking(dir.path()).args([flag, value, "touch", "started"]).assert().code(2);
        assert!(output(&run).contains(flag.trim_start_matches('-')), "{}", output(&run));
        assert!(!dir.path().join("started").exists(), "{flag} {value:?} started the command");
    }
}

/// AC/PRJ-9
#[test]
fn a_tracked_command_with_no_log_links_nothing() {
    let dir = tempfile::tempdir().unwrap();
    tracking(dir.path()).args(["--no-log", "--project", "codeq", "true"]).assert().success();
    assert!(!dir.path().join("data").exists());
}

/// AC/PRJ-10
#[cfg(unix)]
#[test]
fn a_link_that_cannot_be_made_warns_and_keeps_the_commands_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data"), "a file where the data folder goes").unwrap();
    let run = tracking(dir.path()).args(["--project", "codeq", "sh", "-c", "exit 3"]).assert().code(3);
    let text = output(&run);
    assert!(text.contains("WARNING") && text.contains(&project_folder(dir.path(), "codeq").display().to_string()), "{text}");
}

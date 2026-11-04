//! Reading records back: [Report] over the lines runs wrote, and
//! [read_report] over the files under a path.

use std::path::PathBuf;

use carbontracker_run::{Report, read_report};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/records/v1/python_monitored.jsonl");

/// A run's lines, as a writer would leave them: one JSON object per line.
struct Lines(Vec<Value>);

impl Lines {
    fn started(at: &str, command: &str) -> Self {
        Self(vec![json!({
            "at": at, "kind": "run", "format": 1, "carbontracker": "3.0.0", "frontend": "cli",
            "command": command.split(' ').collect::<Vec<_>>(),
            "config": {"interval_s": 1.0, "pue": 1.0, "components": null, "location": null},
            "epochs": null,
        })])
    }

    fn span(mut self, id: u64, name: &str, parent: Option<u64>, kwh: f64, grams: Option<f64>) -> Self {
        self.0.push(json!({"at": "2026-10-25T10:00:00Z", "kind": "span_started", "span": id, "name": name,
                           "parent": parent, "billed": "all"}));
        self.0.push(json!({
            "at": "2026-10-25T10:00:00Z", "kind": "span_settled", "span": id, "name": name, "duration_s": 1.0,
            "energy_kwh": kwh, "emissions_g": grams, "exclusive_energy_kwh": kwh, "exclusive_emissions_g": grams,
            "per_source": {}, "samples": {}, "warnings": [],
        }));
        self
    }

    fn line(mut self, line: Value) -> Self {
        self.0.push(line);
        self
    }

    fn exited(self, code: i32) -> Self {
        self.line(json!({"at": "2026-10-25T11:00:00Z", "kind": "outcome", "ended": "exited", "code": code}))
    }

    fn session(self, duration_s: f64, kwh: f64, grams: Option<f64>) -> Self {
        let usage = json!({"energy_kwh": kwh, "emissions_g": grams});
        self.line(json!({
            "at": "2026-10-25T11:00:00Z", "kind": "session", "duration_s": duration_s, "energy_kwh": kwh,
            "emissions_g": grams, "claimed": usage, "unclaimed": {"energy_kwh": 0.0, "emissions_g": grams.map(|_| 0.0)},
            "per_source": {}, "samples": {}, "intensities": [], "warnings": [],
        }))
    }

    fn text(&self) -> String {
        self.0.iter().map(|line| format!("{line}\n")).collect()
    }
}

fn report(files: &[(&str, String)]) -> Report {
    let mut report = Report::default();
    for (name, text) in files {
        report.add(name, text);
    }
    report
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

/// AC/REC-5, AC/FE-21
#[test]
fn the_v1_fixture_reads_as_one_complete_run() {
    let text = std::fs::read_to_string(FIXTURE).unwrap();
    let report = report(&[("python_monitored.jsonl", text)]);
    assert!(report.unreadable.is_empty(), "{:?}", report.unreadable);
    assert_eq!(report.runs.len(), 1);
    let run = &report.runs[0];
    assert_eq!(run.command, ["train.py", "--lr", "0.1"]);
    assert_eq!(run.ended().as_deref(), Some("monitored"));
    let total = report.total();
    assert_eq!(total.runs, 1);
    assert!(close(total.energy_kwh, 0.0249));
    assert_eq!(total.emissions_g, Some(3.2));
    let spans = report.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].path.as_str(), spans[0].count), ("epoch", 2));
}

/// AC/FE-22
#[test]
fn the_total_sums_session_lines_and_leaves_incomplete_runs_out() {
    let a = Lines::started("2026-10-25T10:00:00Z", "python train.py").exited(0).session(3600.0, 3.41, Some(612.0));
    let b = Lines::started("2026-10-26T10:00:00Z", "python train.py").exited(0).session(1800.0, 1.0, Some(100.0));
    let crashed = Lines::started("2026-10-27T10:00:00Z", "python train.py").span(1, "epoch", None, 0.5, Some(50.0));
    let report = report(&[("a.ct", a.text()), ("crashed.ct", crashed.text()), ("b.ct", b.text())]);

    let total = report.total();
    assert_eq!(total.runs, 2);
    assert!(close(total.duration_s, 5400.0));
    assert!(close(total.energy_kwh, 4.41));
    assert_eq!(total.emissions_g, Some(712.0));
    let incomplete: Vec<_> = report.incomplete().map(|run| run.file.as_str()).collect();
    assert_eq!(incomplete, ["crashed.ct"]);
}

#[test]
fn runs_are_in_the_order_they_started() {
    let late = Lines::started("2026-10-27T10:00:00Z", "late").session(1.0, 1.0, None);
    let early = Lines::started("2026-10-25T10:00:00Z", "early").session(1.0, 1.0, None);
    let report = report(&[("late.ct", late.text()), ("early.ct", early.text())]);
    let files: Vec<_> = report.runs.iter().map(|run| run.file.as_str()).collect();
    assert_eq!(files, ["early.ct", "late.ct"]);
}

/// AC/FE-23
#[test]
fn a_run_without_intensity_adds_energy_and_no_emissions() {
    let known = Lines::started("2026-10-25T10:00:00Z", "a").session(1.0, 2.0, Some(300.0));
    let unknown = Lines::started("2026-10-26T10:00:00Z", "b").session(1.0, 1.1, None);
    let total = report(&[("a.ct", known.text()), ("b.ct", unknown.text())]).total();
    assert!(close(total.energy_kwh, 3.1));
    assert_eq!(total.emissions_g, Some(300.0));
    assert_eq!(total.without_emissions, 1);

    let only_unknown = Lines::started("2026-10-26T10:00:00Z", "b").session(1.0, 1.1, None);
    let total = report(&[("b.ct", only_unknown.text())]).total();
    assert_eq!(total.emissions_g, None);
    assert_eq!(total.without_emissions, 1);
}

/// AC/REC-2, AC/FE-24
#[test]
fn a_newer_format_is_unreadable_and_named() {
    let mut newer = Lines::started("2026-10-25T10:00:00Z", "a").session(1.0, 1.0, None);
    newer.0[0]["format"] = json!(2);
    let report = report(&[("newer.ct", newer.text())]);
    assert!(report.runs.is_empty());
    assert_eq!(report.unreadable.len(), 1);
    assert_eq!(report.unreadable[0].file, "newer.ct");
    assert!(report.unreadable[0].reason.contains("format 2"), "{}", report.unreadable[0].reason);
}

/// AC/FE-24
#[test]
fn a_line_that_does_not_decode_makes_the_file_unreadable() {
    let text = format!("{}not json\n", Lines::started("2026-10-25T10:00:00Z", "a").text());
    let report = report(&[("bad.ct", text)]);
    assert!(report.runs.is_empty());
    assert!(report.unreadable[0].reason.contains("line 2"), "{}", report.unreadable[0].reason);
}

/// AC/FE-24
#[test]
fn a_file_without_a_header_is_unreadable() {
    let report = report(&[("empty.ct", String::new())]);
    assert!(report.runs.is_empty());
    assert_eq!(report.unreadable[0].file, "empty.ct");
}

/// AC/FE-24
#[test]
fn a_last_line_cut_off_is_ignored() {
    let lines = Lines::started("2026-10-25T10:00:00Z", "a").span(1, "epoch", None, 0.5, None);
    let text = format!("{}{{\"at\":\"2026-10-25T11:00:00Z\",\"kind\":\"sess", lines.text());
    let report = report(&[("killed.ct", text)]);
    assert!(report.unreadable.is_empty(), "{:?}", report.unreadable);
    assert_eq!(report.incomplete().count(), 1);
    assert_eq!(report.spans()[0].count, 1);
}

/// AC/REC-3
#[test]
fn unknown_kinds_are_skipped() {
    let lines = Lines::started("2026-10-25T10:00:00Z", "a")
        .line(json!({"at": "2026-10-25T10:30:00Z", "kind": "from_the_future", "x": 1}))
        .session(1.0, 1.0, None);
    let report = report(&[("a.ct", lines.text())]);
    assert!(report.unreadable.is_empty());
    assert_eq!(report.total().runs, 1);
}

/// AC/FE-25
#[test]
fn spans_are_grouped_by_path_across_runs() {
    let a = Lines::started("2026-10-25T10:00:00Z", "a")
        .span(1, "train", None, 3.0, Some(30.0))
        .span(2, "forward", Some(1), 1.0, Some(10.0))
        .span(3, "eval", None, 1.0, Some(10.0))
        .span(4, "forward", Some(3), 0.5, Some(5.0));
    let b = Lines::started("2026-10-26T10:00:00Z", "b")
        .span(1, "train", None, 5.0, None)
        .span(2, "forward", Some(1), 3.0, None);
    let report = report(&[("a.ct", a.text()), ("b.ct", b.text())]);

    let spans = report.spans();
    let paths: Vec<_> = spans.iter().map(|group| group.path.as_str()).collect();
    assert_eq!(paths, ["train", "train/forward", "eval", "eval/forward"]);
    let forward = &spans[1];
    assert_eq!(forward.count, 2);
    assert!(close(forward.energy_kwh, 4.0));
    assert!(close(forward.mean_kwh(), 2.0));
    assert!(close(forward.min_kwh, 1.0));
    assert!(close(forward.max_kwh, 3.0));
    assert_eq!(forward.emissions_g, Some(10.0));
    assert_eq!(forward.without_emissions, 1);
}

/// AC/FE-21
#[test]
fn read_report_finds_ct_files_in_folders_once_each() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run-20261025-a1b2c3d4");
    std::fs::create_dir_all(&run).unwrap();
    let text = Lines::started("2026-10-25T10:00:00Z", "a").session(1.0, 1.0, None).text();
    std::fs::write(run.join("run-a1b2c3d4.ct"), &text).unwrap();
    std::fs::write(run.join("output.log"), "not a record").unwrap();

    let paths = [root.path().to_path_buf(), run.join("run-a1b2c3d4.ct")];
    let report = read_report(&paths).unwrap();
    assert_eq!(report.runs.len(), 1);
    assert!(report.unreadable.is_empty());
}

/// AC/FE-58
#[test]
fn read_report_fails_on_a_path_that_does_not_exist() {
    let error = read_report(&[PathBuf::from("/no/such/folder")]).unwrap_err();
    assert!(error.to_string().contains("/no/such/folder"), "{error}");
}

/// AC/FE-26
#[test]
fn the_runs_csv_has_one_row_per_run_at_full_precision() {
    use carbontracker_run::presentation::report::runs_csv;
    let complete = Lines::started("2026-10-25T10:00:00Z", "python train.py --tags a,b")
        .exited(0)
        .session(3600.5, 0.123456789012, None);
    let crashed = Lines::started("2026-10-26T10:00:00Z", "python train.py");
    let csv = runs_csv(&report(&[("a.ct", complete.text()), ("b.ct", crashed.text())]));
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines[0], "file,started,frontend,command,ended,code,signal,duration_s,energy_kwh,emissions_g");
    assert_eq!(lines[1], "a.ct,2026-10-25T10:00:00+00:00,cli,\"python train.py --tags a,b\",exited,0,,3600.5,0.123456789012,");
    assert_eq!(lines[2], "b.ct,2026-10-26T10:00:00+00:00,cli,python train.py,,,,,,");
    assert_eq!(lines.len(), 3, "no total row");
}

/// AC/FE-26
#[test]
fn the_spans_csv_has_one_row_per_path() {
    use carbontracker_run::presentation::report::spans_csv;
    let run = Lines::started("2026-10-25T10:00:00Z", "a")
        .span(1, "epoch", None, 0.25, Some(2.0))
        .span(2, "epoch", None, 0.75, None);
    let csv = spans_csv(&report(&[("a.ct", run.text())]));
    assert_eq!(
        csv,
        "path,count,energy_kwh,mean_kwh,min_kwh,max_kwh,emissions_g,without_emissions\nepoch,2,1,0.5,0.25,0.75,2,1\n"
    );
}

/// AC/FE-63
#[test]
fn the_table_and_the_csv_show_the_tracked_command_never_the_wrappers_argv() {
    use carbontracker_run::presentation::report::{runs, runs_csv};
    let cli = Lines::started("2026-10-25T10:00:00Z", "/usr/local/bin/carbontracker --global /usr/bin/python3 train.py")
        .span(1, "/usr/bin/python3 train.py", None, 0.5, Some(1.0))
        .exited(0)
        .session(1.0, 0.5, Some(1.0));
    let mut python = Lines::started("2026-10-26T10:00:00Z", "/opt/venv/bin/python train.py").session(1.0, 0.5, Some(1.0));
    python.0[0]["frontend"] = json!("python");
    let report = report(&[("a.ct", cli.text()), ("b.ct", python.text())]);

    let table = runs(&report);
    assert!(!table.contains("carbontracker --global"), "{table}");
    let rows: Vec<_> = table.lines().filter(|line| line.contains("train.py")).collect();
    assert_eq!(rows.len(), 2, "{table}");
    assert!(rows.iter().all(|row| row.contains("  python") && !row.contains("/")), "the program by its name: {table}");

    let csv = runs_csv(&report);
    let commands: Vec<_> = csv.lines().skip(1).map(|line| line.split(',').nth(3).unwrap()).collect();
    assert_eq!(commands, ["/usr/bin/python3 train.py", "/opt/venv/bin/python train.py"]);
}

/// AC/FE-63
#[test]
fn the_table_says_its_times_are_utc() {
    use carbontracker_run::presentation::report::runs;
    let run = Lines::started("2026-10-25T10:00:00Z", "true").exited(0).session(1.0, 0.5, Some(1.0));
    let table = runs(&report(&[("a.ct", run.text())]));
    assert!(table.starts_with("Started (UTC)"), "{table}");
    assert!(table.contains("2026-10-25 10:00"), "{table}");
}

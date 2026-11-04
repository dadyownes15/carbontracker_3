//! Records for `carbontracker report` to read: a research project's training
//! runs, and a profiled pipeline.

use serde_json::{Value, json};

/// One run's record, built line by line.
pub struct Record {
    lines: Vec<Value>,
    spans: u64,
}

impl Record {
    pub fn started(at: &str, frontend: &str, command: &[&str]) -> Self {
        let header = json!({
            "at": at, "kind": "run", "format": 1, "carbontracker": "3.0.0", "frontend": frontend,
            "command": command, "config": {"interval_s": 1.0, "pue": 1.58, "components": null, "location": null},
            "epochs": null,
        });
        Self { lines: vec![header], spans: 0 }
    }

    /// A settled span, returning its id for children.
    pub fn span(&mut self, name: &str, parent: Option<u64>, kwh: f64, grams: Option<f64>) -> u64 {
        self.spans += 1;
        let id = self.spans;
        let at = self.lines[0]["at"].clone();
        self.lines.push(json!({"at": at, "kind": "span_started", "span": id, "name": name, "parent": parent, "billed": "all"}));
        self.lines.push(json!({
            "at": at, "kind": "span_settled", "span": id, "name": name, "duration_s": 1.0, "energy_kwh": kwh,
            "emissions_g": grams, "exclusive_energy_kwh": kwh, "exclusive_emissions_g": grams, "per_source": {},
            "samples": {}, "warnings": [],
        }));
        id
    }

    pub fn ended(mut self, outcome: Value, duration_s: f64, kwh: f64, grams: Option<f64>) -> Self {
        let at = self.lines[0]["at"].clone();
        let mut outcome = outcome;
        outcome["at"] = at.clone();
        outcome["kind"] = json!("outcome");
        self.lines.push(outcome);
        let usage = json!({"energy_kwh": kwh, "emissions_g": grams});
        self.lines.push(json!({
            "at": at, "kind": "session", "duration_s": duration_s, "energy_kwh": kwh, "emissions_g": grams,
            "claimed": usage, "unclaimed": {"energy_kwh": 0.0, "emissions_g": grams.map(|_| 0.0)},
            "per_source": {}, "samples": {}, "intensities": [], "warnings": [],
        }));
        self
    }

    pub fn text(&self) -> String {
        self.lines.iter().map(|line| format!("{line}\n")).collect()
    }
}

/// Five training runs: two tracked by the CLI (one stopped by Slurm's
/// SIGTERM), three by Python with epochs (one without intensity, one killed
/// before its session line); and a file from a newer release.
pub fn research() -> Vec<(&'static str, String)> {
    let epochs = |record: &mut Record, kwh: f64, grams: Option<f64>, count: usize| {
        for _ in 0..count {
            record.span("epoch", None, kwh, grams);
        }
    };

    let mut a = Record::started("2026-10-25T10:37:01Z", "cli", &["carbontracker", "--country", "DK", "python", "train.py", "--lr", "1e-3"]);
    a.span("python train.py --lr 1e-3", None, 3.41, Some(612.0));
    let a = a.ended(json!({"ended": "exited", "code": 0}), 22_320.0, 3.41, Some(612.0));

    let mut b = Record::started("2026-10-26T09:02:44Z", "python", &["train.py", "--lr", "3e-4"]);
    epochs(&mut b, 0.372, Some(65.5), 10);
    let b = b.ended(json!({"ended": "completed", "epochs_done": 10}), 24_000.0, 3.72, Some(655.0));

    let mut c = Record::started("2026-10-27T14:15:40Z", "python", &["train.py", "--lr", "1e-4"]);
    epochs(&mut c, 0.22, None, 5);
    let c = c.ended(json!({"ended": "completed", "epochs_done": 5}), 7_380.0, 1.10, None);

    let mut d = Record::started("2026-10-28T08:30:12Z", "cli", &["carbontracker", "python", "sweep.py"]);
    d.span("python sweep.py", None, 6.88, Some(1_204.0));
    let d = d.ended(json!({"ended": "signalled", "signal": 15}), 42_900.0, 6.88, Some(1_204.0));

    let mut e = Record::started("2026-10-29T22:10:05Z", "python", &["train.py", "--lr", "3e-5"]);
    epochs(&mut e, 0.35, Some(60.0), 2);

    let newer = "{\"at\":\"2027-01-04T09:00:00Z\",\"kind\":\"run\",\"format\":2}\n".to_owned();
    vec![
        ("logs/run-20261025_103701-a1b2c3d4/run-a1b2c3d4.ct", a.text()),
        ("logs/run-20261026_090244-e5f6a7b8/run-e5f6a7b8.ct", b.text()),
        ("logs/run-20261027_141540-c9d0e1f2/run-c9d0e1f2.ct", c.text()),
        ("logs/run-20261028_083012-a3b4c5d6/run-a3b4c5d6.ct", d.text()),
        ("logs/run-20261029_221005-e7f8a9b0/run-e7f8a9b0.ct", e.text()),
        ("logs/run-20270104_090000-c1d2e3f4/run-c1d2e3f4.ct", newer),
    ]
}

/// Three runs of a profiled pipeline: four steps per item, inside `batch`.
pub fn profile() -> Vec<(&'static str, String)> {
    const J: f64 = 1.0 / 3_600_000.0;
    let run = |at: &str, items: usize, scale: f64| {
        let mut record = Record::started(at, "python", &["profile.py"]);
        for item in 0..items {
            let wobble = 1.0 + 0.1 * ((item % 7) as f64 - 3.0) / 3.0;
            let batch = record.span("batch", None, 0.95 * scale * wobble * J, None);
            record.span("tokenize", Some(batch), 0.0137 * scale * wobble * J, None);
            record.span("forward", Some(batch), 0.304 * scale * wobble * J, None);
            record.span("loss", Some(batch), 0.006 * scale * wobble * J, None);
            record.span("backward", Some(batch), 0.600 * scale * wobble * J, None);
        }
        record.ended(json!({"ended": "completed"}), 30.0, 0.95 * scale * items as f64 * J, None)
    };
    vec![
        ("profile/run-a/run-a.ct", run("2026-10-30T10:00:00Z", 1000, 1.0).text()),
        ("profile/run-b/run-b.ct", run("2026-10-30T11:00:00Z", 1000, 1.05).text()),
        ("profile/run-c/run-c.ct", run("2026-10-30T12:00:00Z", 1000, 0.97).text()),
    ]
}

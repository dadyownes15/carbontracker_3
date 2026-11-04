//! Text for runs read back: the [runs] and [spans] tables, their CSV
//! ([runs_csv], [spans_csv]), and 2.4.7's [citation] sentence.
//!
//! ```text
//! Started (UTC)     Command                Duration  Energy     CO2eq  Ended
//! 2026-10-25 10:37  python train.py --lr…   6:12:00  3.41 kWh   612 g  exited 0
//! 2026-10-29 22:10  python train.py         —        —          —      incomplete ¹
//!
//! Total (1 run)                             6:12:00  3.41 kWh   612 g
//! ```

use std::path::Path;
use std::time::Duration;

use super::{clock, equivalents, significant};
use crate::domain::record::{Ended, Outcome};
use crate::domain::report::{Report, Run, Total};

/// Shown where a value is absent.
const ABSENT: &str = "—";

/// The widest command shown in the runs table, in characters.
const COMMAND_WIDTH: usize = 40;

/// The runs, one row each in the order they started, then the total and
/// what was left out of it.
pub fn runs(report: &Report) -> String {
    if report.runs.is_empty() {
        return with_notes("No runs found.".to_owned(), notes(report));
    }
    let total = report.total();
    let mut rows: Vec<Vec<String>> = report.runs.iter().map(run_row).collect();
    rows.push(Vec::new());
    rows.push(vec![
        format!("Total ({})", plural(total.runs, "run")),
        String::new(),
        clock(Duration::from_secs_f64(total.duration_s)),
        kwh(total.energy_kwh),
        total.emissions_g.map_or_else(|| ABSENT.to_owned(), grams),
        String::new(),
    ]);
    let header = ["Started (UTC)", "Command", "Duration", "Energy", "CO2eq", "Ended"];
    with_notes(table(&header, &rows, &[false, false, true, true, true, false]), notes(report))
}

/// The settled spans grouped by path, in the order the paths were first
/// seen. Energy is in joules when every group used under 1 Wh.
pub fn spans(report: &Report) -> String {
    let groups = report.spans();
    if groups.is_empty() {
        return with_notes("No settled spans found.".to_owned(), span_notes(report));
    }
    let joules = groups.iter().all(|group| group.max_kwh < 0.001);
    let energy = |kwh_value: f64| if joules { format!("{} J", significant(kwh_value * 3_600_000.0)) } else { kwh(kwh_value) };
    let rows: Vec<Vec<String>> = groups
        .iter()
        .map(|group| {
            vec![
                group.path.clone(),
                group.count.to_string(),
                energy(group.energy_kwh),
                energy(group.mean_kwh()),
                energy(group.min_kwh),
                energy(group.max_kwh),
                group.emissions_g.map_or_else(|| ABSENT.to_owned(), grams),
            ]
        })
        .collect();
    let header = ["Span", "Count", "Total", "Mean", "Min", "Max", "CO2eq"];
    with_notes(table(&header, &rows, &[false, true, true, true, true, true, true]), span_notes(report))
}

/// One row per run, values at full precision in the record's units, an
/// empty cell where a value is absent.
pub fn runs_csv(report: &Report) -> String {
    let mut csv = String::from("file,started,frontend,command,ended,code,signal,duration_s,energy_kwh,emissions_g\n");
    for run in &report.runs {
        let outcome = run.outcome.as_ref();
        let session = run.session.as_ref();
        let cells = [
            run.file.clone(),
            run.started.to_rfc3339(),
            run.frontend.clone(),
            tracked(run),
            run.ended().unwrap_or_default(),
            number(outcome.and_then(|outcome| outcome.code)),
            number(outcome.and_then(|outcome| outcome.signal)),
            number(session.map(|session| session.duration_s)),
            number(session.map(|session| session.energy_kwh)),
            number(session.and_then(|session| session.emissions_g)),
        ];
        csv_row(&mut csv, &cells);
    }
    csv
}

/// One row per span group, values at full precision in the record's units.
pub fn spans_csv(report: &Report) -> String {
    let mut csv = String::from("path,count,energy_kwh,mean_kwh,min_kwh,max_kwh,emissions_g,without_emissions\n");
    for group in report.spans() {
        let cells = [
            group.path.clone(),
            group.count.to_string(),
            group.energy_kwh.to_string(),
            group.mean_kwh().to_string(),
            group.min_kwh.to_string(),
            group.max_kwh.to_string(),
            number(group.emissions_g),
            group.without_emissions.to_string(),
        ];
        csv_row(&mut csv, &cells);
    }
    csv
}

/// carbontracker 2.4.7's sentence for a paper, over `total`.
pub fn citation(total: &Total) -> String {
    let mut sentence = format!(
        "The training of models in this work is estimated to use {} kWh of electricity",
        significant(total.energy_kwh)
    );
    match total.emissions_g {
        Some(grams) => {
            sentence.push_str(&format!(" contributing to {} kg of CO2eq. ", significant(grams / 1000.0)));
            let equivalents: Vec<String> =
                equivalents(grams).into_iter().map(|(amount, unit)| format!("{} {unit}", significant(amount))).collect();
            if !equivalents.is_empty() {
                sentence.push_str(&format!("This is equivalent to {}. ", equivalents.join(" or ")));
            }
        }
        None => sentence.push_str(". "),
    }
    sentence.push_str("Measured by carbontracker (https://github.com/saintslab/carbontracker).");
    sentence
}

/// What the runs table leaves out of its total, and why; also printed after
/// the CSV and the sentence, which have no room for it.
pub fn notes(report: &Report) -> Vec<String> {
    let total = report.total();
    let mut notes = Vec::new();
    if total.without_emissions > 0 {
        let (verb, its) = if total.without_emissions == 1 { ("had", "its") } else { ("had", "their") };
        notes.push(format!(
            "{} {verb} no grid intensity: {its} energy is in the total, {its} CO2eq is not.",
            plural(total.without_emissions, "run")
        ));
    }
    let incomplete = report.incomplete().count();
    if incomplete > 0 {
        notes.push(format!(
            "{} never ended normally (no session line) and {} left out of the total.",
            plural(incomplete, "run"),
            if incomplete == 1 { "is" } else { "are" }
        ));
    }
    notes.extend(unreadable(report));
    notes
}

/// What the spans table leaves out, and why.
pub fn span_notes(report: &Report) -> Vec<String> {
    let mut notes = Vec::new();
    // Groups with no emissions at all show `—`; a note is for mixed groups.
    let without: u64 = report
        .spans()
        .iter()
        .filter(|group| group.emissions_g.is_some())
        .map(|group| group.without_emissions)
        .sum();
    if without > 0 {
        notes.push(format!(
            "{} had no grid intensity: their energy is counted, their CO2eq is not.",
            plural(without as usize, "span")
        ));
    }
    notes.extend(unreadable(report));
    notes
}

fn unreadable(report: &Report) -> Vec<String> {
    report.unreadable.iter().map(|file| format!("{} could not be read: {}", file.file, file.reason)).collect()
}

/// The command a run tracked: for the CLI its span, never carbontracker's own
/// argv.
fn tracked(run: &Run) -> String {
    match (&run.first_root_span, run.frontend.as_str()) {
        (Some(tracked), "cli") => tracked.clone(),
        _ => run.command.join(" "),
    }
}

/// `command` with its program by file name.
fn short(command: &str) -> String {
    let (program, rest) = command.split_once(' ').unwrap_or((command, ""));
    let name = Path::new(program).file_name().map_or_else(|| program.into(), |name| name.to_string_lossy());
    if rest.is_empty() { name.into_owned() } else { format!("{name} {rest}") }
}

fn run_row(run: &Run) -> Vec<String> {
    let command = short(&tracked(run));
    let command = if command.chars().count() > COMMAND_WIDTH {
        format!("{}…", command.chars().take(COMMAND_WIDTH - 1).collect::<String>())
    } else {
        command
    };
    let started = run.started.format("%Y-%m-%d %H:%M").to_string();
    match &run.session {
        Some(session) => vec![
            started,
            command,
            clock(Duration::from_secs_f64(session.duration_s)),
            kwh(session.energy_kwh),
            session.emissions_g.map_or_else(|| ABSENT.to_owned(), grams),
            run.outcome.as_ref().map_or_else(|| ABSENT.to_owned(), ended),
        ],
        None => vec![started, command, ABSENT.to_owned(), ABSENT.to_owned(), ABSENT.to_owned(), "incomplete".to_owned()],
    }
}

/// How a run ended, e.g. `exited 0` or `signal 15`.
fn ended(outcome: &Outcome) -> String {
    match (&outcome.ended, outcome.code, outcome.signal) {
        (Ended::Exited, Some(code), _) => format!("exited {code}"),
        (Ended::Signalled, _, Some(signal)) => format!("signal {signal}"),
        (ended, _, _) => ended.name().to_owned(),
    }
}

fn kwh(value: f64) -> String {
    format!("{} kWh", significant(value))
}

fn grams(value: f64) -> String {
    format!("{} g", significant(value))
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 { format!("1 {noun}") } else { format!("{count} {noun}s") }
}

fn number(value: Option<impl ToString>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Columns padded to their widest cell; an empty row is a blank line.
fn table(header: &[&str], rows: &[Vec<String>], right: &[bool]) -> String {
    let width = |column: usize| {
        rows.iter()
            .filter_map(|row| row.get(column))
            .map(|cell| cell.chars().count())
            .chain([header[column].chars().count()])
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..header.len()).map(width).collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let pad = widths[column] - cell.chars().count();
                if right[column] { format!("{}{cell}", " ".repeat(pad)) } else { format!("{cell}{}", " ".repeat(pad)) }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut lines = vec![line(header.to_vec())];
    for row in rows {
        lines.push(if row.is_empty() { String::new() } else { line(row.iter().map(String::as_str).collect()) });
    }
    lines.join("\n")
}

fn with_notes(text: String, notes: Vec<String>) -> String {
    if notes.is_empty() {
        return text;
    }
    let notes: Vec<String> = notes.iter().map(|text| super::note(text)).collect();
    format!("{text}\n\n{}", notes.join("\n"))
}

fn csv_row(csv: &mut String, cells: &[String]) {
    let quoted: Vec<String> = cells
        .iter()
        .map(|cell| {
            if cell.contains([',', '"', '\n']) { format!("\"{}\"", cell.replace('"', "\"\"")) } else { cell.clone() }
        })
        .collect();
    csv.push_str(&quoted.join(","));
    csv.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_cell_with_a_comma_is_quoted() {
        let mut csv = String::new();
        csv_row(&mut csv, &["a,b".to_owned(), "say \"hi\"".to_owned(), "plain".to_owned()]);
        assert_eq!(csv, "\"a,b\",\"say \"\"hi\"\"\",plain\n");
    }

    #[test]
    fn the_citation_without_emissions_names_only_energy() {
        let total = Total { runs: 1, duration_s: 1.0, energy_kwh: 1.5, emissions_g: None, without_emissions: 1 };
        assert_eq!(
            citation(&total),
            "The training of models in this work is estimated to use 1.50 kWh of electricity. \
             Measured by carbontracker (https://github.com/saintslab/carbontracker)."
        );
    }
}

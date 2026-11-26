//! `carbontracker report`: the runs recorded under some paths, summed.

use std::path::{Path, PathBuf};

use carbontracker_run::presentation::report;
use carbontracker_run::{check_project, project_folder, read_report};

/// Print the runs table and the citation, or the spans table, to stdout; or
/// save either as CSV to `output`, by default `runs.csv` or `spans.csv` in the
/// folder the runs were read from. The saved path and what the CSV leaves out go to stderr.
/// With `project`, only its runs; with no paths, those linked from its folder.
/// With neither, the runs in `log_folder`.
pub fn report(
    spans: bool,
    csv: bool,
    output: Option<PathBuf>,
    project: Option<&str>,
    mut paths: Vec<PathBuf>,
    log_folder: &Path,
) -> Result<i32, String> {
    if let Some(project) = project {
        check_project(project)?;
        if paths.is_empty() {
            let folder = project_folder(project).ok_or("no home directory to find projects in")?;
            if !folder.is_dir() {
                return Err(format!("no runs of project {project} on this machine: {} is not there", folder.display()));
            }
            paths.push(folder);
        }
    }
    if paths.is_empty() && project.is_none() {
        if !log_folder.is_dir() {
            return Err(format!(
                "no runs to read: {} is not there. Give a path: carbontracker report PATH",
                log_folder.display()
            ));
        }
        paths.push(log_folder.to_path_buf());
    }
    let paths = &paths[..];
    let read = read_report(paths, project).map_err(|error| error.to_string())?;
    match (spans, csv) {
        (false, false) => {
            println!("{}", report::runs(&read));
            let total = read.total();
            if total.runs > 0 {
                println!("\n{}", report::citation(&total));
            }
        }
        (true, false) => println!("{}", report::spans(&read)),
        (spans, true) => {
            let name = if spans { "spans.csv" } else { "runs.csv" };
            let path = output.unwrap_or_else(|| beside_the_logs(paths).join(name));
            let text = if spans { report::spans_csv(&read) } else { report::runs_csv(&read) };
            std::fs::write(&path, text).map_err(|error| format!("can't write {}: {error}", path.display()))?;
            let notes = if spans { report::span_notes(&read) } else { report::notes(&read) };
            for note in notes {
                crate::say(&carbontracker_run::presentation::note(&note));
            }
            crate::say(&format!("Saved {}", path.display()));
        }
    }
    Ok(0)
}

/// The folder the runs were read from: the first path if it is a folder, else
/// the folder holding it.
fn beside_the_logs(paths: &[PathBuf]) -> PathBuf {
    let Some(first) = paths.first() else { return PathBuf::new() };
    if first.is_dir() { first.clone() } else { first.parent().map(Path::to_path_buf).unwrap_or_default() }
}

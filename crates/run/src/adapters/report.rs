//! [read_report]: the records under the paths given, read into a [Report].

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::report::{Report, Unreadable};

/// Reads every `.ct` file under `paths`, searching folders recursively and
/// following links, each file once; with `project`, keeps only the runs of
/// that project and the projects below it. Fails on a path that cannot be
/// listed; a file that cannot be read, or a link to nothing, is named in the
/// report.
pub fn read_report(paths: &[PathBuf], project: Option<&str>) -> io::Result<Report> {
    let mut files = BTreeMap::new();
    let mut report = Report::only(project);
    for path in paths {
        collect(path, &mut files, &mut report.unreadable)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
    }
    for (_, file) in files {
        let name = file.display().to_string();
        match std::fs::read_to_string(&file) {
            Ok(text) => report.add(&name, &text),
            Err(error) => report.unreadable.push(Unreadable { file: name, reason: error.to_string() }),
        }
    }
    Ok(report)
}

/// Adds the `.ct` files at `path`, keyed by their canonical path so each is
/// read once, keeping the path as given for display.
fn collect(path: &Path, files: &mut BTreeMap<PathBuf, PathBuf>, gone: &mut Vec<Unreadable>) -> io::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?.path();
            if std::fs::metadata(&entry).is_err() && std::fs::symlink_metadata(&entry).is_ok() {
                gone.push(Unreadable {
                    file: entry.display().to_string(),
                    reason: "links to a run folder that was deleted or moved".to_owned(),
                });
                continue;
            }
            collect(&entry, files, gone)?;
        }
    } else if path.extension().is_some_and(|extension| extension == "ct") {
        files.entry(path.canonicalize()?).or_insert_with(|| path.to_path_buf());
    }
    Ok(())
}

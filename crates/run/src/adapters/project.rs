//! A project's folder on this machine: a symbolic link to each of its runs'
//! folders, so its runs are read back from one place.

use std::io;
use std::path::{Path, PathBuf};

use super::Folder;

/// Whether `name` can name a project: parts joined by `/`, each of ASCII
/// letters, digits, `.`, `_` and `-`, beginning with a letter or a digit.
///
/// # Errors
///
/// Why it cannot.
pub fn check_project(name: &str) -> Result<(), String> {
    let part_ok = |part: &str| {
        part.chars().next().is_some_and(|first| first.is_ascii_alphanumeric())
            && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if name.split('/').all(part_ok) {
        Ok(())
    } else {
        Err(format!(
            "project {name:?} is not a project name: parts of letters, digits, '.', '_' and '-', \
             each beginning with a letter or digit, joined by '/', e.g. saintslab/codeq"
        ))
    }
}

/// The folder of the project `name`: `$XDG_DATA_HOME/carbontracker/projects/<name>`,
/// else `~/.local/share/carbontracker/projects/<name>`; on Windows under
/// `%LOCALAPPDATA%`. `None` if there is no such directory. `name` must pass
/// [check_project].
pub fn project_folder(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let mut folder = base?.join("carbontracker").join("projects");
    folder.extend(name.split('/'));
    Some(folder)
}

impl Folder {
    /// Links this run folder from the folder of `project`, creating it.
    /// Off Unix, where a link needs privileges, does nothing.
    ///
    /// # Errors
    ///
    /// The project's folder, if the link could not be made there.
    pub fn link_to_project(&self, project: &str) -> Result<(), String> {
        let Some(folder) = project_folder(project) else {
            return Err("no home directory to keep the project's folder in".to_owned());
        };
        link(self.path(), &folder).map_err(|error| format!("cannot link this run from {}: {error}", folder.display()))
    }
}

#[cfg(unix)]
fn link(run: &Path, folder: &Path) -> io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let target = run.canonicalize()?;
    let name = run.file_name().ok_or_else(|| io::Error::other("a run folder has a name"))?;
    std::os::unix::fs::symlink(target, folder.join(name))
}

#[cfg(not(unix))]
fn link(_run: &Path, _folder: &Path) -> io::Result<()> {
    Ok(())
}

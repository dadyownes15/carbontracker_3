//! [Folder]: one run's folder, `[<prefix>_]run-<time>-<id>/`, holding its
//! record and its console output.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};

/// A run's folder, created when claimed.
#[derive(Debug, Clone)]
pub struct Folder {
    path: PathBuf,
    id: String,
}

impl Folder {
    /// Creates a new run folder in `dir`, creating `dir` too. The id is
    /// random, so runs started in the same second, in any process, get their
    /// own folders.
    ///
    /// # Errors
    ///
    /// If `dir` can't be written.
    pub fn claim(dir: &Path, prefix: Option<&str>) -> io::Result<Folder> {
        fs::create_dir_all(dir)?;
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let prefix = prefix
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| format!("{prefix}_"))
            .unwrap_or_default();
        loop {
            let id = random_id();
            let path = dir.join(format!("{prefix}run-{stamp}-{id}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Folder { path, id }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The record: `run-<id>.ct`.
    pub fn record(&self) -> PathBuf {
        self.path.join(format!("run-{}.ct", self.id))
    }

    /// The console's text at normal verbosity.
    pub fn output_log(&self) -> PathBuf {
        self.path.join("output.log")
    }
}

/// Eight random lowercase letters and digits.
fn random_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.write_u32(std::process::id());
    let mut bits = hasher.finish();
    (0..8)
        .map(|_| {
            let letter = ALPHABET[(bits % ALPHABET.len() as u64) as usize] as char;
            bits /= ALPHABET.len() as u64;
            letter
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_claimed_at_once_are_two_folders() {
        let dir = tempfile::tempdir().unwrap();
        let first = Folder::claim(dir.path(), Some("rank0")).unwrap();
        let second = Folder::claim(dir.path(), Some("rank0")).unwrap();
        assert_ne!(first.path(), second.path());
        let name = first
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("rank0_run-")
                && name.len() == "rank0_run-20261027_141203-k3x9q2m7".len(),
            "{name}"
        );
        assert_eq!(
            first.record().file_name().unwrap().to_string_lossy(),
            format!("run-{}.ct", &name[name.len() - 8..])
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(Folder::claim(dir.path(), None).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}

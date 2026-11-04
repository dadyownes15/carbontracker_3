//! The config file `carbontracker init` writes: API keys, readable only by
//! the user. [keys] combines it with the environment and a frontend's own,
//! for [GridConfig::api_keys](crate::GridConfig).
//!
//! ```toml
//! [keys]
//! electricity_maps = "…"
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::config::{ApiKey, KeyedService};

/// The environment variable holding the Electricity Maps key.
pub const KEY_VARIABLE: &str = "CARBONTRACKER_EM_KEY";

/// The keys from the config file, then [KEY_VARIABLE], then `given` (by any
/// name of a [KeyedService]), each winning over the one before; and the
/// config file's path if others can read it, for the frontend to warn
/// about. Empty keys are left out, so they never hide one before them.
///
/// # Errors
///
/// An unreadable config file, or a name that is no service.
pub fn keys(
    given: impl IntoIterator<Item = (String, String)>,
) -> Result<(BTreeMap<KeyedService, ApiKey>, Option<PathBuf>), String> {
    keys_from(path(), std::env::var(KEY_VARIABLE).ok(), given)
}

/// [keys], from the config file at `path` and the variable's `variable`.
fn keys_from(
    path: Option<PathBuf>,
    variable: Option<String>,
    given: impl IntoIterator<Item = (String, String)>,
) -> Result<(BTreeMap<KeyedService, ApiKey>, Option<PathBuf>), String> {
    let mut saved = BTreeMap::new();
    let mut readable = None;
    if let Some(path) = path {
        saved = load(&path)?;
        if readable_by_others(&path) {
            readable = Some(path);
        }
    }
    let keys = merge(saved, variable, given)?;
    Ok((keys, readable))
}

fn merge(
    saved: BTreeMap<KeyedService, ApiKey>,
    variable: Option<String>,
    given: impl IntoIterator<Item = (String, String)>,
) -> Result<BTreeMap<KeyedService, ApiKey>, String> {
    let mut keys = saved;
    if let Some(key) = variable.filter(|key| !key.is_empty()) {
        keys.insert(KeyedService::ElectricityMaps, ApiKey::new(key));
    }
    for (name, key) in given {
        let service = KeyedService::from_name(&name).ok_or_else(|| format!("no service named {name}"))?;
        if !key.is_empty() {
            keys.insert(service, ApiKey::new(key));
        }
    }
    Ok(keys)
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct File {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

/// Where the config file lives: `$XDG_CONFIG_HOME/carbontracker/config.toml`
/// (default `~/.config/…`, on macOS too) on Unix, a home directory a
/// cluster's compute nodes share; `%APPDATA%\carbontracker\config.toml` on
/// Windows. `None` if there is no such directory.
pub fn path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("carbontracker").join("config.toml"))
}

/// The keys in the file at `path`; none if there is no file.
pub fn load(path: &Path) -> Result<BTreeMap<KeyedService, ApiKey>, String> {
    let Some(file) = read(path)? else { return Ok(BTreeMap::new()) };
    file.keys
        .into_iter()
        .filter(|(_, key)| !key.is_empty())
        .map(|(name, key)| match KeyedService::from_name(&name) {
            Some(service) => Ok((service, ApiKey::new(key))),
            None => Err(format!("{}: no service named {name}", path.display())),
        })
        .collect()
}

/// Store `key` for `service` in the file at `path`, keeping its other keys.
/// On Unix the file is made readable only by the user (`0600`); on Windows
/// the profile's default ACL already keeps it to the user.
pub fn save(path: &Path, service: KeyedService, key: &str) -> Result<(), String> {
    let mut file = read(path)?.unwrap_or_default();
    // Under the name shown, replacing any other spelling of it.
    file.keys.retain(|name, _| KeyedService::from_name(name) != Some(service));
    file.keys.insert(service.name().to_owned(), key.to_owned());
    let text = toml::to_string(&file).expect("a table of strings serialises");

    let fail = |error: io::Error| format!("cannot write {}: {error}", path.display());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    write_private(path, &text).map_err(fail)
}

/// Whether anyone but the owner can read the file at `path`. Always false
/// off Unix, where the profile's ACL decides.
pub fn readable_by_others(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o044 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

fn read(path: &Path) -> Result<Option<File>, String> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map(Some).map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // `mode` only applies to a new file; tighten an old one before writing.
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(key: &str) -> BTreeMap<KeyedService, ApiKey> {
        BTreeMap::from([(KeyedService::ElectricityMaps, ApiKey::new(key))])
    }

    fn given(name: &str, key: &str) -> Vec<(String, String)> {
        vec![(name.to_owned(), key.to_owned())]
    }

    /// The Electricity Maps key chosen, by its last four characters.
    fn chosen(keys: Result<BTreeMap<KeyedService, ApiKey>, String>) -> Option<String> {
        keys.unwrap().get(&KeyedService::ElectricityMaps).map(ApiKey::hint)
    }

    /// AC/PROV-47, AC/PY-5
    #[test]
    fn the_variable_wins_over_the_config_file() {
        assert_eq!(chosen(merge(saved("file"), None, Vec::new())), Some("…file".to_owned()));
        assert_eq!(chosen(merge(saved("file"), Some("vari".to_owned()), Vec::new())), Some("…vari".to_owned()));
    }

    /// AC/PROV-47, AC/PY-5
    #[test]
    fn given_keys_win_over_the_variable_and_the_config_file() {
        let keys = merge(saved("file"), Some("vari".to_owned()), given("electricitymaps", "args"));
        assert_eq!(chosen(keys), Some("…args".to_owned()));
        assert_eq!(chosen(merge(BTreeMap::new(), None, given("electricity_maps", "args"))), Some("…args".to_owned()));
    }

    /// AC/PROV-47
    #[test]
    fn an_empty_key_never_hides_one_before_it() {
        let keys = merge(saved("file"), Some(String::new()), given("electricitymaps", ""));
        assert_eq!(chosen(keys), Some("…file".to_owned()));
        assert_eq!(chosen(merge(BTreeMap::new(), None, Vec::new())), None);
    }

    #[test]
    fn an_unknown_name_is_refused() {
        let refused = merge(BTreeMap::new(), None, given("carbonintensityGB", "k"));
        assert_eq!(refused.unwrap_err(), "no service named carbonintensityGB");
    }

    #[test]
    fn a_saved_key_loads_back_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("carbontracker").join("config.toml");
        assert_eq!(load(&path), Ok(BTreeMap::new()));

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[keys]\nelectricitymaps = \"old\"\n").unwrap();
        save(&path, KeyedService::ElectricityMaps, "new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "[keys]\nelectricity_maps = \"new\"\n");
        assert_eq!(load(&path), Ok([(KeyedService::ElectricityMaps, ApiKey::new("new"))].into()));
    }

    #[test]
    fn an_unknown_service_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[keys]\nwattTime = \"k\"\n").unwrap();
        assert!(load(&path).unwrap_err().contains("no service named wattTime"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(readable_by_others(&path));

        save(&path, KeyedService::ElectricityMaps, "k").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!readable_by_others(&path));
    }

    /// AC/PROV-47
    #[cfg(unix)]
    #[test]
    fn a_config_file_others_can_read_is_reported_for_the_frontend() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[keys]\nelectricity_maps = \"file\"\n").unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let (keys, readable) = keys_from(Some(path.clone()), None, Vec::new()).unwrap();
        assert_eq!(keys.get(&KeyedService::ElectricityMaps).map(ApiKey::hint), Some("…file".to_owned()));
        assert_eq!(readable, Some(path.clone()));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let (_, readable) = keys_from(Some(path.clone()), Some("vari".to_owned()), Vec::new()).unwrap();
        assert_eq!(readable, None);
        let (keys, _) = keys_from(Some(path), Some("vari".to_owned()), given("electricity_maps", "args")).unwrap();
        assert_eq!(keys.get(&KeyedService::ElectricityMaps).map(ApiKey::hint), Some("…args".to_owned()));
    }
}

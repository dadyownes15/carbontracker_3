//! `carbontracker init`: saving the Electricity Maps key.

use std::io::{self, IsTerminal};

use carbontracker_providers::config_file as config;
use carbontracker_providers::KeyedService;

/// Ask for the Electricity Maps key and save it in the config file: with a
/// hidden prompt on a terminal, otherwise one line from stdin. Never a
/// flag, so never in shell history.
pub fn init() -> Result<(), String> {
    let path = config::path().ok_or("no config directory: set HOME")?;
    let key = if io::stdin().is_terminal() {
        rpassword::prompt_password("Electricity Maps API key: ")
    } else {
        let mut line = String::new();
        io::stdin().read_line(&mut line).map(|_| line)
    }
    .map_err(|error| format!("cannot read the key: {error}"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("no key given".to_owned());
    }
    config::save(&path, KeyedService::ElectricityMaps, key)?;
    println!("Saved to {}", path.display());
    Ok(())
}

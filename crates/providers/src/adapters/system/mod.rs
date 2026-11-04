//! This host: its CPU, its processes, where it is, and the config file
//! holding its API keys.

pub mod config_file;
pub(crate) mod cpu;
pub(crate) mod ip_geolocation;
pub(crate) mod process;

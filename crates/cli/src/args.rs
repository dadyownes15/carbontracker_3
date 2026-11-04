//! The command line: `carbontracker [options] <command>`, `check`, `init`
//! and `report`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use carbontracker::{GramsCo2ePerKwh, GridLocation, PowerUsageEffectiveness, TrackerConfig, Watts};
use carbontracker_providers::{
    ApiKey, Coordinates, CountryCode, GridConfig, KeyedService, Measure, Pinned, ProbeConfig,
    ProviderRegistry,
};
use carbontracker_providers::config_file::{self as config, KEY_VARIABLE};
use carbontracker_run::presentation;
use clap::error::{ContextKind, ContextValue};
use clap::{ArgGroup, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "carbontracker",
    about = "Track the energy use and carbon emissions of a command.",
    allow_external_subcommands = true,
    subcommand_value_name = "COMMAND",
    arg_required_else_help = true,
    group(ArgGroup::new("location").multiple(false)),
    after_help = "Examples:\n  carbontracker python train.py\n  carbontracker --country DK -- ./run.sh --epochs 10\n  \
                  carbontracker check\n  carbontracker report ./logs\n\n\
                  The Electricity Maps key is read from CARBONTRACKER_EM_KEY, or from the file `carbontracker init` \
                  writes."
)]
pub struct Args {
    #[command(subcommand)]
    action: Option<Action>,

    /// Electricity Maps zone, e.g. DK-DK2.
    #[arg(long, global = true, value_name = "ZONE", group = "location", value_parser = zone,
          help_heading = "Location (at most one; default: estimated from this IP)")]
    zone: Option<GridLocation>,

    /// ISO 3166 alpha-2 country code, e.g. DK.
    #[arg(long, global = true, value_name = "CC", group = "location", value_parser = country,
          help_heading = "Location (at most one; default: estimated from this IP)")]
    country: Option<GridLocation>,

    /// Latitude and longitude, e.g. 55.68,12.57.
    #[arg(long, global = true, value_name = "LAT,LON", group = "location", value_parser = coordinates,
          allow_hyphen_values = true, help_heading = "Location (at most one; default: estimated from this IP)")]
    coordinates: Option<GridLocation>,

    /// The world average; contacts no one.
    #[arg(long, global = true, group = "location",
          help_heading = "Location (at most one; default: estimated from this IP)")]
    global: bool,

    /// Whole-machine draw in watts.
    #[arg(long, global = true, value_name = "WATTS", value_parser = watts,
          conflicts_with_all = ["cpu_power", "gpu_power"], help_heading = "Fixed values (skip probing)")]
    power: Option<Watts>,

    /// CPU draw in watts; the rest is probed.
    #[arg(long, global = true, value_name = "WATTS", value_parser = watts, help_heading = "Fixed values (skip probing)")]
    cpu_power: Option<Watts>,

    /// GPU draw in watts; the rest is probed.
    #[arg(long, global = true, value_name = "WATTS", value_parser = watts, help_heading = "Fixed values (skip probing)")]
    gpu_power: Option<Watts>,

    /// Grid intensity in gCO2/kWh; no network.
    #[arg(long, global = true, value_name = "G/KWH", value_parser = intensity, help_heading = "Fixed values (skip probing)")]
    intensity: Option<GramsCo2ePerKwh>,

    /// all, cpu, gpu, or cpu,gpu.
    #[arg(long, global = true, value_name = "LIST", default_value = "all", value_parser = str::parse::<Measure>,
          help_heading = "Measuring")]
    components: Measure,

    /// Seconds between readouts.
    #[arg(long, global = true, value_name = "SECONDS", default_value_t = 1.0, help_heading = "Measuring")]
    interval: f64,

    /// Power usage effectiveness; 1.58 is Uptime Institute's 2023 average.
    #[arg(long, global = true, default_value_t = PowerUsageEffectiveness::UPTIME_2023.get(), help_heading = "Measuring")]
    pue: f64,

    /// Log directory; each run gets a folder in it.
    #[arg(long, alias = "log_dir", global = true, value_name = "DIR", default_value = "./logs",
          help_heading = "Output")]
    log_dir: PathBuf,

    /// Write no files.
    #[arg(long, global = true, conflicts_with = "log_dir", help_heading = "Output")]
    no_log: bool,

    /// Every probe step, and every event and readout.
    #[arg(short, long, global = true, help_heading = "Output")]
    pub verbose: bool,

    /// API keys as JSON, e.g. {"electricity_maps": "KEY"}; wins over the
    /// environment and the config file.
    #[arg(long, alias = "api_keys", global = true, value_name = "KEYS", help_heading = "Keys")]
    api_keys: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show what would be measured, and what wasn't and why.
    Check,
    /// Save an API key.
    Init,
    /// Sum the runs recorded in .ct files, on stdout or as CSV.
    Report {
        /// Group settled spans by path, across runs.
        #[arg(long)]
        spans: bool,
        /// Save one CSV row per run to runs.csv beside the logs, or per span group to spans.csv with --spans.
        #[arg(long)]
        csv: bool,
        /// Save the CSV to FILE instead.
        #[arg(short, long, value_name = "FILE", requires = "csv")]
        output: Option<PathBuf>,
        /// Record files, or folders to search for them.
        #[arg(value_name = "PATH", required = true)]
        paths: Vec<PathBuf>,
    },
    #[command(external_subcommand)]
    Run(Vec<OsString>),
}

/// The API keys found, each with where it came from.
pub type Keys = BTreeMap<KeyedService, (ApiKey, Origin)>;

/// Where a key came from; each wins over the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Environment,
    ConfigFile,
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Flag => "--api-keys",
            Self::Environment => KEY_VARIABLE,
            Self::ConfigFile => "the config file",
        })
    }
}

impl Args {
    /// What to do: the subcommand, or a command after `--`. `None` if
    /// neither was given.
    pub fn action(&self) -> Option<Action> {
        self.action.clone()
    }

    /// The registry to probe with, from the flags and `keys`.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong, for exit code 2.
    pub fn registry(&self, keys: &Keys) -> Result<ProviderRegistry, String> {
        ProviderRegistry::new(self.probe_config(keys)).map_err(|error| error.to_string())
    }

    /// What to probe for, from the flags and `keys`.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong, for exit code 2.
    pub fn probe(&self, keys: &Keys) -> Result<ProbeConfig, String> {
        let config = self.probe_config(keys);
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    fn probe_config(&self, keys: &Keys) -> ProbeConfig {
        ProbeConfig {
            measure: self.components.clone(),
            grid: GridConfig {
                location: self
                    .zone
                    .clone()
                    .or_else(|| self.country.clone())
                    .or_else(|| self.coordinates.clone())
                    .or(self.global.then_some(GridLocation::Global)),
                api_keys: keys.iter().map(|(service, (key, _))| (*service, key.clone())).collect(),
                ..GridConfig::default()
            },
            pinned: Pinned {
                power: self.power,
                intensity: self.intensity,
                cpu_power: self.cpu_power,
                gpu_power: self.gpu_power,
            },
            processes: None,
        }
    }

    /// The tracker's config: readout interval and PUE.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong, for exit code 2.
    pub fn tracker(&self) -> Result<TrackerConfig, String> {
        if !(self.interval.is_finite() && self.interval > 0.0) {
            return Err("--interval must be a number of seconds above zero".to_owned());
        }
        Ok(TrackerConfig {
            readout_interval: Duration::from_secs_f64(self.interval),
            pue: PowerUsageEffectiveness::new(self.pue).ok_or("--pue must be at least 1")?,
            ..TrackerConfig::default()
        })
    }

    /// Where to write a run's files; `None` with `--no-log`.
    pub fn log_dir(&self) -> Option<&Path> {
        (!self.no_log).then_some(self.log_dir.as_path())
    }

    /// The keys from the config file, the environment and `--api-keys`, each
    /// winning over the one before. Warns on stderr if others can read the
    /// config file.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong, for exit code 2.
    pub fn keys(&self) -> Result<Keys, String> {
        let mut keys = Keys::new();

        if let Some(path) = config::path() {
            if config::readable_by_others(&path) {
                eprintln!("{}", presentation::prefixed(&presentation::config_readable(&path)));
            }
            keys.extend(config::load(&path)?.into_iter().map(|(service, key)| (service, (key, Origin::ConfigFile))));
        }
        if let Some(key) = std::env::var(KEY_VARIABLE).ok().filter(|key| !key.is_empty()) {
            keys.insert(KeyedService::ElectricityMaps, (ApiKey::new(key), Origin::Environment));
        }
        if let Some(text) = &self.api_keys {
            keys.extend(api_keys(text)?.into_iter().map(|(service, key)| (service, (key, Origin::Flag))));
        }
        Ok(keys)
    }
}

/// Flags earlier versions took that are gone, and what to use instead.
const REMOVED: [(&str, &str); 8] = [
    ("--parse", "sum the runs with carbontracker report <PATH>"),
    ("--report", "PDF reports are gone; carbontracker report <PATH> prints a table, or saves CSV with --csv"),
    ("--sim-cpu", "state the CPU's draw with --cpu-power <WATTS>"),
    ("--sim-cpu-tdp", "state the CPU's draw with --cpu-power <WATTS>"),
    ("--sim-cpu-util", "state the CPU's draw, utilisation included, with --cpu-power <WATTS>"),
    ("--sim-gpu", "state the GPU's draw with --gpu-power <WATTS>"),
    ("--sim-gpu-watts", "state the GPU's draw with --gpu-power <WATTS>"),
    ("--sim-gpu-util", "state the GPU's draw, utilisation included, with --gpu-power <WATTS>"),
];

/// What to say instead of clap's "unexpected argument" when `error` is about
/// a removed flag.
pub fn removed(error: &clap::Error) -> Option<String> {
    let Some(ContextValue::String(argument)) = error.get(ContextKind::InvalidArg) else { return None };
    let flag = argument.split('=').next()?;
    REMOVED
        .iter()
        .find(|(old, _)| *old == flag)
        .map(|(old, instead)| format!("{old} was removed in carbontracker 3: {instead}"))
}

/// A command as one line, for naming its span.
pub fn span_name(command: &[OsString]) -> String {
    command.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join(" ")
}

fn zone(text: &str) -> Result<GridLocation, String> {
    let zone = text.trim();
    if zone.is_empty() {
        return Err("expected a zone, e.g. DK-DK2".to_owned());
    }
    Ok(GridLocation::Zone { zone_id: zone.to_owned() })
}

fn country(text: &str) -> Result<GridLocation, String> {
    let code = CountryCode::new(text.trim()).ok_or("expected two letters, e.g. DK")?;
    Ok(GridLocation::Country { country_code: code.as_str().to_owned() })
}

fn coordinates(text: &str) -> Result<GridLocation, String> {
    let expected = "expected LAT,LON within ±90 and ±180, e.g. 55.68,12.57";
    let (latitude, longitude) = text.split_once(',').ok_or(expected)?;
    let number = |part: &str| part.trim().parse::<f64>().map_err(|_| expected);
    let at = Coordinates::new(number(latitude)?, number(longitude)?).ok_or(expected)?;
    Ok(GridLocation::Coordinates { latitude: at.latitude(), longitude: at.longitude() })
}

fn watts(text: &str) -> Result<Watts, String> {
    let value: f64 = text.parse().map_err(|_| "expected a number of watts")?;
    Watts::new(value).map_err(|error| error.to_string())
}

fn intensity(text: &str) -> Result<GramsCo2ePerKwh, String> {
    let value: f64 = text.parse().map_err(|_| "expected a number of gCO2/kWh")?;
    if value <= 0.0 {
        return Err("expected a number above zero".to_owned());
    }
    GramsCo2ePerKwh::new(value).map_err(|_| "expected a number above zero".to_owned())
}

/// Keys as JSON, or a Python dict with single quotes, by any name of a
/// [KeyedService]. Empty keys are left out.
fn api_keys(text: &str) -> Result<BTreeMap<KeyedService, ApiKey>, String> {
    let parsed: BTreeMap<String, String> = serde_json::from_str(text)
        .or_else(|_| serde_json::from_str(&text.replace('\'', "\"")))
        .map_err(|_| format!("--api-keys {text:?}: expected {{\"{}\": \"KEY\"}}", KeyedService::ALL[0].name()))?;

    let unknown: Vec<&str> =
        parsed.keys().map(String::as_str).filter(|name| KeyedService::from_name(name).is_none()).collect();
    if !unknown.is_empty() {
        let supported: Vec<&str> = KeyedService::ALL.iter().flat_map(|service| service.names()).copied().collect();
        return Err(format!(
            "No matching provider for API keys: {}. Supported providers: {}.",
            unknown.join(", "),
            supported.join(", ")
        ));
    }

    let mut keys = BTreeMap::new();
    for (name, key) in parsed.into_iter().filter(|(_, key)| !key.is_empty()) {
        let service = KeyedService::from_name(&name).expect("checked above");
        if keys.insert(service, ApiKey::new(key)).is_some() {
            return Err(format!("--api-keys gives {} more than one key", service.name()));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("carbontracker").chain(line.iter().copied()))
    }

    fn run(line: &[&str]) -> Vec<OsString> {
        match parse(line).unwrap().action() {
            Some(Action::Run(command)) => command,
            other => panic!("{other:?}"),
        }
    }

    /// AC/FE-3, AC/FE-41
    #[test]
    fn a_location_is_read_in_the_terms_of_its_flag() {
        assert_eq!(zone("DK-DK2"), Ok(GridLocation::Zone { zone_id: "DK-DK2".into() }));
        assert_eq!(country("dk"), Ok(GridLocation::Country { country_code: "DK".into() }));
        assert!(country("Denmark").is_err());
        assert_eq!(
            coordinates("55.68, 12.57"),
            Ok(GridLocation::Coordinates { latitude: 55.68, longitude: 12.57 })
        );
        assert!(coordinates("95,0").is_err());
        assert!(coordinates("north,south").is_err());
    }

    /// AC/FE-3
    #[test]
    fn at_most_one_location() {
        assert!(parse(&["--zone", "DK-DK2", "--country", "DK", "true"]).is_err());
        assert!(parse(&["--country", "DK", "--global", "true"]).is_err());
    }

    /// AC/FE-34
    #[test]
    fn options_go_before_the_command_or_after_check() {
        assert_eq!(run(&["--country", "DK", "python", "-u", "train.py", "--global"]), ["python", "-u", "train.py", "--global"]);
        let args = parse(&["check", "--country", "DK"]).unwrap();
        assert!(matches!(args.action(), Some(Action::Check)));
        assert_eq!(args.country, Some(GridLocation::Country { country_code: "DK".into() }));
    }

    /// AC/FE-35
    #[test]
    fn a_command_named_like_a_subcommand_follows_a_double_dash() {
        assert!(matches!(parse(&["init"]).unwrap().action(), Some(Action::Init)));
        assert_eq!(run(&["--", "init", "--force"]), ["init", "--force"]);
    }

    /// AC/FE-37, AC/FE-4
    #[test]
    fn a_whole_machine_power_excludes_a_cpu_or_gpu_power() {
        assert!(parse(&["--power", "100", "--cpu-power", "50", "true"]).is_err());
        let args = parse(&["--cpu-power", "50", "--gpu-power", "300", "true"]).unwrap();
        let pinned = args.probe_config(&Keys::new()).pinned;
        assert_eq!((pinned.cpu_power.unwrap().get(), pinned.gpu_power.unwrap().get()), (50.0, 300.0));
    }

    /// AC/FE-5, AC/FE-41
    #[test]
    fn components_are_parsed_by_the_providers() {
        let args = parse(&["--components", "cpu,gpu", "true"]).unwrap();
        assert_eq!(args.probe_config(&Keys::new()).measure, "cpu,gpu".parse().unwrap());
        assert!(parse(&["--components", "tpu", "true"]).is_err());
    }

    /// AC/FE-40
    #[test]
    fn underscore_spellings_are_the_same_flags() {
        let args = parse(&["--log_dir", "out", "--api_keys", "{'electricitymaps': 'k'}", "true"]).unwrap();
        assert_eq!(args.log_dir(), Some(Path::new("out")));
        assert_eq!(args.keys().unwrap()[&KeyedService::ElectricityMaps].1, Origin::Flag);
    }

    /// AC/FE-39
    #[test]
    fn removed_flags_name_their_replacement() {
        let error = parse(&["--sim-cpu", "Xeon", "true"]).unwrap_err();
        assert!(removed(&error).unwrap().contains("--cpu-power"), "{error}");
        let error = parse(&["--sim-gpu-watts=300", "true"]).unwrap_err();
        assert!(removed(&error).unwrap().contains("--gpu-power"), "{error}");
        assert_eq!(removed(&parse(&["--bogus", "true"]).unwrap_err()), None);
    }

    /// AC/FE-7, AC/FE-41
    #[test]
    fn api_keys_take_json_or_a_python_dict() {
        let electricity_maps = |key: &str| Ok([(KeyedService::ElectricityMaps, ApiKey::new(key))].into());
        assert_eq!(api_keys(r#"{"electricity_maps": "k"}"#), electricity_maps("k"));
        assert_eq!(api_keys("{'electricitymaps': 'k'}"), electricity_maps("k"));
        assert_eq!(api_keys("{'electricitymaps': ''}"), Ok(BTreeMap::new()));
        assert!(api_keys("{'carbonintensityGB': 'k'}").unwrap_err().contains("Supported providers"));
        assert!(api_keys("{'electricitymaps': 'a', 'electricity_maps': 'b'}").unwrap_err().contains("more than one"));
        assert!(api_keys("electricitymaps=k").is_err());
    }

    /// AC/FE-5, AC/FE-6
    #[test]
    fn measuring_and_output_have_their_defaults() {
        let args = parse(&["true"]).unwrap();
        let tracker = args.tracker().unwrap();
        assert_eq!((tracker.readout_interval, tracker.pue.get()), (Duration::from_secs(1), 1.58));
        assert_eq!(args.probe_config(&Keys::new()).measure, "all".parse().unwrap());
        assert_eq!(args.log_dir(), Some(Path::new("./logs")));
        assert!(!args.verbose);

        let args = parse(&["--interval", "2.5", "--pue", "1.2", "-v", "--no-log", "true"]).unwrap();
        let tracker = args.tracker().unwrap();
        assert_eq!((tracker.readout_interval, tracker.pue.get()), (Duration::from_millis(2500), 1.2));
        assert_eq!(args.log_dir(), None);
        assert!(args.verbose);
    }

    /// AC/FE-41
    #[test]
    fn an_intensity_must_be_above_zero() {
        assert!(intensity("0").is_err());
        assert!(intensity("-1").is_err());
        assert!(intensity("nan").is_err());
        assert_eq!(intensity("0.5").unwrap().get(), 0.5);
    }

    #[test]
    fn the_span_is_named_after_the_whole_command() {
        assert_eq!(span_name(&["python".into(), "train.py".into()]), "python train.py");
    }
}

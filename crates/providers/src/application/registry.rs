//! [ProviderRegistry]: the four chains, power, location, intensity and
//! forecast, run in that order.

use std::sync::Arc;
use std::time::Duration;

use carbontracker::{Fidelity, GridLocation, PowerDomain, ProviderId, Sample, SourceDescription, SourceKind};
use chrono::Utc;

use crate::adapters::intensity::constant::{constant_intensity, WORLD_AVERAGE_G_CO2_PER_KWH};
use crate::adapters::intensity::country_average::probe_country_average;
use crate::adapters::intensity::electricity_maps::{
    probe_electricity_maps, probe_electricity_maps_forecast, Api, ELECTRICITY_MAPS_URL,
};
use crate::adapters::power::constant::{constant_device, constant_power};
use crate::adapters::power::generic_cpu::probe_generic_cpu;
#[cfg(target_os = "macos")]
use crate::adapters::power::ioreport::{probe_ioreport, IOREPORT_DOMAINS};
#[cfg(target_os = "macos")]
use crate::adapters::power::macmon::{probe_macmon, MACMON_DOMAINS};
use crate::adapters::power::nvml::probe_nvml;
#[cfg(target_os = "macos")]
use crate::adapters::power::powermetrics::{probe_powermetrics, POWERMETRICS_DOMAINS};
#[cfg(target_os = "linux")]
use crate::adapters::power::rapl::probe_rapl;
use crate::adapters::system::ip_geolocation::{estimate_by_ip, IP_GEOLOCATION, IP_GEOLOCATION_URL};
use crate::application::chain::Chain;
use crate::application::forecaster::{Fetch, Forecaster};
use crate::application::resolution::{ProbeProgress, Resolution};
use crate::domain::config::{KeyedService, ProbeConfig, ProbeConfigError};
use crate::domain::location::Location;
use crate::domain::verdict::Verdict;
use crate::ports::Outcome;

// Fixed per candidate so ids are stable across runs and a probe can mint
// `SourceId`s before knowing whether it wins.
pub(crate) const STATIC_POWER: ProviderId = ProviderId(0);
#[cfg(target_os = "linux")]
pub(crate) const RAPL: ProviderId = ProviderId(1);
pub(crate) const NVML: ProviderId = ProviderId(2);
#[cfg(target_os = "macos")]
pub(crate) const MACMON: ProviderId = ProviderId(3);
#[cfg(target_os = "macos")]
pub(crate) const POWERMETRICS: ProviderId = ProviderId(4);
pub(crate) const GENERIC_CPU: ProviderId = ProviderId(5);
pub(crate) const STATIC_INTENSITY: ProviderId = ProviderId(6);
pub(crate) const ELECTRICITY_MAPS: ProviderId = ProviderId(7);
pub(crate) const GLOBAL_AVERAGE: ProviderId = ProviderId(8);
pub(crate) const COUNTRY_AVERAGE: ProviderId = ProviderId(10);
pub(crate) const STATIC_CPU: ProviderId = ProviderId(13);
pub(crate) const STATIC_GPU: ProviderId = ProviderId(14);
/// NVML GPUs without an energy counter, read by their power instead.
pub(crate) const NVML_POWER: ProviderId = ProviderId(15);
pub(crate) const IOREPORT: ProviderId = ProviderId(16);

/// The default provider registry: the ranked candidates above, probed
/// against what [ProbeConfig] asks for.
pub struct ProviderRegistry {
    config: ProbeConfig,
    endpoints: Endpoints,
}

/// Where the network candidates ask, and how long a request may take.
/// Swapped for a fake server in tests.
#[derive(Debug, Clone)]
pub(crate) struct Endpoints {
    pub(crate) ip_geolocation: String,
    /// Base URL; the candidates append the endpoint's path.
    pub(crate) electricity_maps: String,
    /// Bounds every request, while probing and on later readouts, so a slow
    /// service never hangs probing or the tracker thread.
    pub(crate) timeout: Duration,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            ip_geolocation: IP_GEOLOCATION_URL.to_owned(),
            electricity_maps: ELECTRICITY_MAPS_URL.to_owned(),
            timeout: Duration::from_secs(2),
        }
    }
}

impl ProviderRegistry {
    /// # Errors
    ///
    /// [ProbeConfigError] if `config` is invalid; nothing is probed.
    pub fn new(config: ProbeConfig) -> Result<Self, ProbeConfigError> {
        config.validate()?;
        Ok(Self { config, endpoints: Endpoints::default() })
    }

    #[cfg(test)]
    pub(crate) fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Probe this host. Never fails: a candidate that can't serve is declined
    /// and recorded in the [Resolution].
    pub async fn probe(&self) -> Resolution {
        self.probe_with(|_| {}).await
    }

    /// [Self::probe], reporting progress to `on_progress` as it goes.
    pub async fn probe_with(&self, mut on_progress: impl FnMut(ProbeProgress)) -> Resolution {
        let config = &self.config;
        let net = Net::new(&self.endpoints, config);
        let mut chain = Chain::new(&config.measure, &mut on_progress);
        probe_power(&mut chain, config).await;
        let location = probe_location(&mut chain, config, &net).await;
        // The forecast must be for the grid the run is billed against.
        let grid = probe_intensity(&mut chain, config, location.as_ref(), &net).await;
        let forecaster = probe_forecast(&mut chain, config, location.as_ref(), &grid, &net).await;
        chain.result.forecaster = Some(forecaster);
        chain.result.location = location;
        chain.result
    }
}

/// What the network candidates share: one client, bounded by the timeout.
pub(crate) struct Net {
    client: reqwest::Client,
    endpoints: Endpoints,
    electricity_maps: Option<Api>,
}

impl Net {
    fn new(endpoints: &Endpoints, config: &ProbeConfig) -> Self {
        // No idle connections: probing may run on a runtime that is dropped
        // right after (the Python bindings do), and a pooled connection would
        // stay tied to it. Requests are rare, so pooling buys nothing.
        let client = reqwest::Client::builder()
            .timeout(endpoints.timeout)
            .pool_max_idle_per_host(0)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let electricity_maps = config
            .grid
            .api_keys
            .get(&KeyedService::ElectricityMaps)
            .map(|key| Api::new(&client, &endpoints.electricity_maps, key));
        Self { client, endpoints: endpoints.clone(), electricity_maps }
    }
}

/// The intensity the chain resolved: its value now, and the grid it measures.
struct Grid {
    g_per_kwh: f64,
    location: GridLocation,
}

/// The grid an outcome's first source describes, if it is a grid source.
fn grid_of(outcome: &Outcome) -> Option<&GridLocation> {
    outcome.descriptions.iter().find_map(|(_, description)| match description {
        SourceDescription::Intensity { location, .. } => Some(location),
        SourceDescription::Power { .. } => None,
    })
}

/// A stated location is used as given; otherwise the estimators run, best
/// first. Fills no coverage slot: it is an input to the grid chains.
async fn probe_location(chain: &mut Chain<'_>, config: &ProbeConfig, net: &Net) -> Option<Location> {
    if let Some(stated) = &config.grid.location {
        let location = Location::Stated(stated.clone());
        chain.record("stated-location", Verdict::Serving(vec![location.to_string()]));
        chain.record(IP_GEOLOCATION, Verdict::NotWanted);
        return Some(location);
    }

    // A pinned run touches no network.
    if config.pinned.intensity.is_some() {
        chain.record(IP_GEOLOCATION, Verdict::NotWanted);
        return None;
    }

    chain.probing(IP_GEOLOCATION);
    let Some(estimate) = estimate_by_ip(&net.client, &net.endpoints.ip_geolocation).await else {
        chain.record(IP_GEOLOCATION, Verdict::Declined);
        return None;
    };
    chain.record(IP_GEOLOCATION, Verdict::Serving(vec![estimate.to_string()]));
    Some(Location::Estimated(estimate))
}

async fn probe_power(chain: &mut Chain<'_>, config: &ProbeConfig) {
    // Static power replaces all probing.
    if let Some(watts) = config.pinned.power {
        chain.probing("static");
        chain.take(STATIC_POWER, "static", Some(constant_power(STATIC_POWER, watts.get())));
        return;
    }

    // Before every probe of their domain, which they then replace.
    let pinned = [
        (STATIC_CPU, "static-cpu", PowerDomain::Cpu, config.pinned.cpu_power),
        (STATIC_GPU, "static-gpu", PowerDomain::Gpu, config.pinned.gpu_power),
    ];
    for (id, name, domain, watts) in pinned {
        let Some(watts) = watts else { continue };
        if chain.worth_probing(name, &[domain]) {
            chain.take(id, name, Some(constant_device(id, "static", domain, watts.get(), Fidelity::Stated)));
            chain.pinned.push(domain);
        }
    }

    #[cfg(target_os = "linux")]
    if chain.worth_probing("rapl", &[PowerDomain::Cpu, PowerDomain::Ram]) {
        chain.take(RAPL, "rapl", probe_rapl(RAPL));
    }

    // Before SoC probes: otherwise an integrated `Gpu` slot would hide discrete cards.
    // One look at the driver finds both: the GPUs with an energy counter, and the rest.
    if chain.worth_probing("nvml-energy", &[PowerDomain::Gpu]) {
        let found = probe_nvml(NVML, NVML_POWER, config.processes.as_deref());
        chain.take(NVML, "nvml-energy", found.energy);
        chain.probing("nvml-power");
        chain.take(NVML_POWER, "nvml-power", found.power);
    } else {
        chain.record("nvml-power", Verdict::NotWanted);
    }

    // IOReport's running totals, read when asked: a counter, so a cut is
    // billed exactly. macmon then serves only the system rail.
    #[cfg(target_os = "macos")]
    if chain.worth_probing("ioreport", &IOREPORT_DOMAINS) {
        chain.take(IOREPORT, "ioreport", probe_ioreport(IOREPORT));
    }

    // macmon needs no root and is fast; powermetrics (~2.3s/sample) is the
    // Intel-Mac fallback where IOReport has no power channel.
    #[cfg(target_os = "macos")]
    if chain.worth_probing("macmon", &MACMON_DOMAINS) {
        chain.take(MACMON, "macmon", probe_macmon(MACMON).await);
    }

    #[cfg(target_os = "macos")]
    if chain.worth_probing("powermetrics", &POWERMETRICS_DOMAINS) {
        chain.take(POWERMETRICS, "powermetrics", probe_powermetrics(POWERMETRICS).await);
    }

    if chain.worth_probing("generic-cpu", &[PowerDomain::Cpu]) {
        chain.take(GENERIC_CPU, "generic-cpu", Some(probe_generic_cpu(GENERIC_CPU)));
    }
}

/// Returns the resolved intensity, for the forecast chain to match.
async fn probe_intensity(
    chain: &mut Chain<'_>,
    config: &ProbeConfig,
    location: Option<&Location>,
    net: &Net,
) -> Grid {
    if let Some(fixed) = config.pinned.intensity.map(|intensity| intensity.get()) {
        let stated = config.grid.location.clone().unwrap_or(GridLocation::Global);
        chain.probing("static");
        let pinned = constant_intensity(STATIC_INTENSITY, fixed, stated.clone(), Fidelity::Stated);
        chain.take(STATIC_INTENSITY, "static", Some(pinned));
        return Grid { g_per_kwh: fixed, location: stated };
    }

    chain.probing("electricity-maps");
    let live = probe_electricity_maps(ELECTRICITY_MAPS, net.electricity_maps.clone(), location).await;
    let mut resolved = resolved_grid(live.as_ref());
    chain.take(ELECTRICITY_MAPS, "electricity-maps", live.map(|(outcome, _)| outcome));

    // Explicit checks: a country's or `Global` coverage never collides with a zone's.
    if chain.already_has(SourceKind::Intensity) {
        chain.record("country-average", Verdict::Covered);
    } else {
        chain.probing("country-average");
        let table = probe_country_average(COUNTRY_AVERAGE, location);
        resolved = resolved_grid(table.as_ref());
        chain.take(COUNTRY_AVERAGE, "country-average", table.map(|(outcome, _)| outcome));
    }

    if chain.already_has(SourceKind::Intensity) {
        chain.record("global-average", Verdict::Covered);
    } else {
        chain.probing("global-average");
        let floor = constant_intensity(
            GLOBAL_AVERAGE,
            WORLD_AVERAGE_G_CO2_PER_KWH,
            GridLocation::Global,
            Fidelity::Assumed,
        );
        chain.take(GLOBAL_AVERAGE, "global-average", Some(floor));
    }

    resolved.unwrap_or(Grid { g_per_kwh: WORLD_AVERAGE_G_CO2_PER_KWH, location: GridLocation::Global })
}

fn resolved_grid(probed: Option<&(Outcome, f64)>) -> Option<Grid> {
    let (outcome, g_per_kwh) = probed?;
    Some(Grid { g_per_kwh: *g_per_kwh, location: grid_of(outcome)?.clone() })
}

/// The forecast for the grid the run is billed against: pinned, published,
/// or the resolved intensity held flat. Never fails.
async fn probe_forecast(
    chain: &mut Chain<'_>,
    config: &ProbeConfig,
    location: Option<&Location>,
    grid: &Grid,
    net: &Net,
) -> Forecaster {
    let horizon = config.grid.forecast_horizon;

    if let Some(fixed) = config.pinned.intensity.map(|intensity| intensity.get()) {
        chain.probing("static");
        return serve(chain, flat(fixed, grid.location.clone(), horizon, Fidelity::Stated, "static"));
    }

    chain.probing("electricity-maps-forecast");
    let live = probe_electricity_maps_forecast(net.electricity_maps.clone(), location, horizon)
        .await
        // Never told about one grid and billed against another.
        .filter(|probed| GridLocation::Zone { zone_id: probed.zone.clone() } == grid.location);
    if let Some(probed) = live {
        let forecaster = Forecaster::new(
            grid.location.clone(),
            Fidelity::Measured,
            probed.horizon,
            "electricity-maps-forecast",
            probed.fetch,
        )
        .primed(probed.curve);
        let forecaster = serve(chain, forecaster);
        chain.record("projection", Verdict::Covered);
        return forecaster;
    }
    chain.record("electricity-maps-forecast", Verdict::Declined);

    chain.probing("projection");
    // The global average held flat is a guess, not a projection of anything measured.
    let fidelity = if grid.location == GridLocation::Global && grid.g_per_kwh == WORLD_AVERAGE_G_CO2_PER_KWH {
        Fidelity::Assumed
    } else {
        Fidelity::Modelled
    };
    serve(chain, flat(grid.g_per_kwh, grid.location.clone(), horizon, fidelity, "projection"))
}

/// Record `forecaster` as serving under its candidate's name.
fn serve(chain: &mut Chain<'_>, forecaster: Forecaster) -> Forecaster {
    let hours = forecaster.horizon().as_secs_f64() / 3600.0;
    let label = format!("{} forecast for {}, {hours} h", forecaster.fidelity(), forecaster.location());
    chain.record(forecaster.served_by(), Verdict::Serving(vec![label]));
    forecaster
}

/// A forecast that is one value all the way out: two points, from now to
/// `horizon` ahead, so the curve covers the interval.
pub(crate) fn flat(
    g_per_kwh: f64,
    location: GridLocation,
    horizon: Duration,
    fidelity: Fidelity,
    served_by: &'static str,
) -> Forecaster {
    let fetch: Fetch = Arc::new(move || {
        let now = Utc::now();
        let end = now + chrono::Duration::from_std(horizon).unwrap_or(chrono::Duration::zero());
        Some(vec![Sample::new(now, g_per_kwh), Sample::new(end, g_per_kwh)])
    });
    Forecaster::new(location, fidelity, horizon, served_by, fetch)
}

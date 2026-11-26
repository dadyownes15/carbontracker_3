//! What the check view shows, built from a run's items: the candidates as
//! the probe decides them, then each value as readouts land. No terminal and
//! no threads; [super::check_view] draws it and [super::run] feeds it.

use std::collections::HashMap;
use std::sync::Arc;

use carbontracker::{Observation, SampleKind, SourceDescription, SourceId, Timestamp};
use carbontracker_providers::Verdict;
use carbontracker_run::presentation::PowerPart;
use carbontracker_run::{presentation, ForecastSource};

use crate::commands::{How, hours, key_warning};

/// One thing the run reported, as the view needs it.
#[derive(Debug, Clone)]
pub enum Msg {
    /// The probe is trying this candidate.
    Probing(&'static str),
    /// The probe decided this candidate.
    Decided(&'static str, Verdict),
    /// The run started with these sources and this forecast.
    Started { sources: Vec<Source>, forecast: Option<ForecastSource> },
    /// A readout landed.
    Readout(Arc<HashMap<SourceId, Observation>>),
    /// The run could not start, or stopped with an error.
    Failed(String),
}

/// A source the run reads, and the candidate serving it.
#[derive(Debug, Clone)]
pub struct Source {
    pub id: SourceId,
    pub description: SourceDescription,
    pub served_by: &'static str,
}

/// What became of a candidate, in the view's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Probing,
    Serving,
    Standby,
    Declined(String),
    NotAsked,
}

impl Status {
    pub fn word(&self) -> &'static str {
        match self {
            Self::Probing => "probing",
            Self::Serving => "serving",
            Self::Standby => "standby",
            Self::Declined(_) => "declined",
            Self::NotAsked => "not asked",
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Probing => "being probed",
            Self::Serving => "chosen",
            Self::Standby => "a better candidate serves",
            Self::Declined(reason) => reason,
            Self::NotAsked => "nothing it offers was asked for",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: &'static str,
    pub status: Status,
}

/// One line of the compact view: power, location, intensity or forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct KindLine {
    pub label: &'static str,
    /// The value, `…` while waiting for it, or `none`.
    pub value: String,
    pub how: Option<How>,
    /// Who serves it, or what to do when nothing does; one line per
    /// provider.
    pub by: String,
    /// Why its last read failed, naming the source, until it reads again.
    pub failure: Option<String>,
}

/// A key the run was given, and the candidate it unlocks.
#[derive(Debug, Clone)]
pub struct KeyNote {
    pub service: &'static str,
    /// The key's last four characters, e.g. `…1234`.
    pub hint: String,
    pub candidate: &'static str,
    /// Where it came from, e.g. `the config file`.
    pub origin: String,
}

/// The latest a source reported.
#[derive(Debug, Clone, Default)]
struct Latest {
    /// The last sample, for a total's slope.
    last: Option<(Timestamp, f64)>,
    /// Watts or gCO2eq/kWh.
    value: Option<f64>,
    /// Why the last read failed; cleared by the next sample.
    failed: Option<String>,
}

/// Said where a kind has no value and the verbose view explains why.
const SEE_VERBOSE: &str = "check -v shows why";

pub struct Live {
    pub candidates: Vec<Candidate>,
    sources: Vec<Source>,
    forecast: Option<ForecastSource>,
    started: bool,
    latest: HashMap<SourceId, Latest>,
    pue: f64,
    keys: Vec<KeyNote>,
    pub error: Option<String>,
}

impl Live {
    pub fn new(pue: f64) -> Self {
        Self {
            candidates: Vec::new(),
            sources: Vec::new(),
            forecast: None,
            started: false,
            latest: HashMap::new(),
            pue,
            keys: Vec::new(),
            error: None,
        }
    }

    /// The keys the run was given, to warn about one whose candidate declined.
    pub fn with_keys(mut self, keys: Vec<KeyNote>) -> Self {
        self.keys = keys;
        self
    }

    /// One warning per key whose candidate declined, with the reason.
    pub fn warnings(&self) -> Vec<String> {
        self.keys
            .iter()
            .filter_map(|key| {
                let candidate = self.candidates.iter().find(|candidate| candidate.name == key.candidate)?;
                let Status::Declined(reason) = &candidate.status else { return None };
                Some(key_warning(key.service, &key.hint, key.candidate, reason))
            })
            .collect()
    }

    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Probing(name) => self.set(name, Status::Probing),
            Msg::Decided(name, verdict) => {
                let status = match verdict {
                    Verdict::Serving(_) => Status::Serving,
                    Verdict::Declined(reason) => Status::Declined(reason),
                    Verdict::Covered => Status::Standby,
                    Verdict::NotWanted => Status::NotAsked,
                };
                self.set(name, status);
            }
            Msg::Started { sources, forecast } => {
                self.sources = sources;
                self.forecast = forecast;
                self.started = true;
            }
            Msg::Readout(outcomes) => {
                for source in &self.sources {
                    match outcomes.get(&source.id) {
                        Some(Observation::Samples(samples)) => {
                            let latest = self.latest.entry(source.id).or_default();
                            latest.failed = None;
                            for sample in samples {
                                latest.take(source.description.sample_kind(), *sample.observed_at(), sample.value());
                            }
                        }
                        Some(Observation::Failed(failure)) => {
                            self.latest.entry(source.id).or_default().failed = Some(failure.to_string());
                        }
                        _ => {}
                    }
                }
            }
            Msg::Failed(error) => self.error = Some(error),
        }
    }

    fn set(&mut self, name: &'static str, status: Status) {
        match self.candidates.iter_mut().find(|candidate| candidate.name == name) {
            Some(candidate) => candidate.status = status,
            None => self.candidates.push(Candidate { name, status }),
        }
    }

    /// Whether the run has started, so probing is over.
    pub fn started(&self) -> bool {
        self.started
    }

    fn power_sources(&self) -> impl Iterator<Item = &Source> {
        self.sources.iter().filter(|source| matches!(source.description, SourceDescription::Power { .. }))
    }

    fn intensity_source(&self) -> Option<&Source> {
        self.sources.iter().find(|source| matches!(source.description, SourceDescription::Intensity { .. }))
    }

    fn value(&self, source: &Source) -> Option<f64> {
        self.latest.get(&source.id).and_then(|latest| latest.value)
    }

    /// The whole power draw in watts, once every power source has reported.
    pub fn watts(&self) -> Option<f64> {
        let mut total = 0.0;
        let mut any = false;
        for source in self.power_sources() {
            total += self.value(source)?;
            any = true;
        }
        any.then_some(total)
    }

    /// The latest grid intensity in gCO2eq/kWh.
    pub fn intensity(&self) -> Option<f64> {
        self.intensity_source().and_then(|source| self.value(source))
    }

    /// The emission rate now, in gCO2eq/h: power × PUE × intensity.
    pub fn rate(&self) -> Option<f64> {
        Some(self.watts()? * self.pue * self.intensity()? / 1000.0)
    }

    /// Power, location, intensity and forecast, then each key given; the
    /// location is left out when the intensity is stated.
    pub fn lines(&self) -> Vec<KindLine> {
        if !self.started {
            return ["power", "location", "intensity", "forecast"]
                .map(|label| KindLine { label, value: "…".into(), how: None, by: "probing".into(), failure: None })
                .to_vec();
        }
        let mut lines = vec![self.power_line()];
        let intensity = self.intensity_source();
        let stated = intensity.is_some_and(|source| How::of(source.description.fidelity()) == How::Stated);
        if !stated {
            lines.push(self.location_line());
        }
        lines.push(self.intensity_line());
        lines.push(self.forecast_line());
        lines.extend(self.keys.iter().map(|key| KindLine {
            label: "key",
            value: key.hint.clone(),
            how: None,
            by: format!("{}, from {}", key.service, key.origin),
            failure: None,
        }));
        lines
    }

    fn none(label: &'static str) -> KindLine {
        KindLine { label, value: "none".into(), how: None, by: SEE_VERBOSE.into(), failure: None }
    }

    fn power_line(&self) -> KindLine {
        let sources: Vec<&Source> = self.power_sources().collect();
        if sources.is_empty() {
            return Self::none("power");
        }
        let how = sources.iter().map(|source| How::of(source.description.fidelity())).max();
        let parts: Vec<PowerPart> = sources
            .iter()
            .filter_map(|source| match source.description {
                SourceDescription::Power { domain, fidelity, .. } => {
                    Some(PowerPart { provider: source.served_by, domain, fidelity, watts: self.value(source) })
                }
                SourceDescription::Intensity { .. } => None,
            })
            .collect();
        let by = presentation::power_what(&parts, true);
        let failure = self.failures(&sources);
        let value = match self.watts() {
            Some(watts) => stale(format!("{watts:.1} W"), failure.is_some()),
            None => "…".to_owned(),
        };
        KindLine { label: "power", value, how, by, failure }
    }

    fn location_line(&self) -> KindLine {
        let place = self.intensity_source().and_then(|source| match &source.description {
            SourceDescription::Intensity { location, .. } => Some(presentation::grid(location)),
            SourceDescription::Power { .. } => None,
        });
        // The serving candidate behind no source is the location's.
        let serving = self.sources.iter().map(|source| source.served_by).chain(self.forecast.as_ref().map(|f| f.served_by));
        let served: Vec<&str> = serving.collect();
        let by = self
            .candidates
            .iter()
            .find(|candidate| candidate.status == Status::Serving && !served.contains(&candidate.name))
            .map(|candidate| candidate.name);
        match by {
            Some(name) => {
                // A stated location has no provider to name.
                let (how, by) = if name.starts_with("stated") { (How::Stated, "") } else { (How::Live, name) };
                let value = place.unwrap_or_else(|| "…".into());
                let by = if by.is_empty() { String::new() } else { format!("({by})") };
                KindLine { label: "location", value, how: Some(how), by, failure: None }
            }
            None => KindLine {
                label: "location",
                value: "none".into(),
                how: Some(How::Fallback),
                by: format!("the world average applies · {SEE_VERBOSE}"),
                failure: None,
            },
        }
    }

    fn intensity_line(&self) -> KindLine {
        let Some(source) = self.intensity_source() else { return Self::none("intensity") };
        let failure = self.failures(&[source]);
        let value = match self.intensity() {
            Some(g) => stale(format!("{g:.0} gCO2eq/kWh"), failure.is_some()),
            None => "…".to_owned(),
        };
        let SourceDescription::Intensity { location, fidelity } = &source.description else { return Self::none("intensity") };
        let by = presentation::provided(presentation::intensity_what(location, *fidelity), Some(source.served_by));
        KindLine { label: "intensity", value, how: Some(How::of(*fidelity)), by, failure }
    }

    fn forecast_line(&self) -> KindLine {
        let Some(forecast) = &self.forecast else { return Self::none("forecast") };
        KindLine {
            label: "forecast",
            value: format!("{} h ahead", hours(forecast.horizon)),
            how: Some(How::of(forecast.fidelity)),
            by: presentation::provided(presentation::forecast_what(forecast), Some(forecast.served_by)),
            failure: None,
        }
    }

    /// Each failing source among `sources` and why, or `None`.
    fn failures(&self, sources: &[&Source]) -> Option<String> {
        let failures: Vec<String> = sources
            .iter()
            .filter_map(|source| {
                let failed = self.latest.get(&source.id)?.failed.as_ref()?;
                Some(format!("{}: {failed}", named(source)))
            })
            .collect();
        (!failures.is_empty()).then(|| failures.join("; "))
    }

    /// The emission line: `Emitting 12.3 gCO2eq/h right now`.
    pub fn headline(&self) -> String {
        match (&self.error, self.rate()) {
            (Some(error), _) => format!("cannot check: {error}"),
            (None, Some(rate)) => format!("Emitting {rate:.1} gCO2eq/h right now (PUE {})", self.pue),
            (None, None) if self.started => match self.lines().into_iter().find_map(|line| line.failure) {
                Some(failure) => format!("No rate yet: {failure}"),
                None => "Waiting for the first readout…".to_owned(),
            },
            (None, None) => "Probing this machine…".to_owned(),
        }
    }

    /// The view as plain text, for after it closes.
    pub fn snapshot(&self, verbose: bool) -> String {
        let mut out = format!("{}\n", self.headline());
        let lines = self.lines();
        let width = value_width(&lines);
        for line in lines {
            let how = line.how.map_or("", How::word);
            let mut by = line.by.lines();
            out.push_str(&format!("  {:<10} {:<width$} {:<9} {}\n", line.label, line.value, how, by.next().unwrap_or("")));
            for more in by {
                out.push_str(&format!("  {:<10} {:<width$} {:<9} {more}\n", "", "", ""));
            }
            if let Some(failure) = &line.failure {
                out.push_str(&format!("  {:<10} ! failed: {failure}\n", ""));
            }
        }
        for warning in self.warnings() {
            out.push_str(&format!("\n  {warning}\n"));
        }
        if verbose {
            out.push('\n');
            for candidate in &self.candidates {
                out.push_str(&format!(
                    "  {:<26} {:<10} {}\n",
                    candidate.name,
                    candidate.status.word(),
                    candidate.status.reason()
                ));
            }
        }
        out
    }
}

/// The value column's width: the longest value, and never under 20.
pub fn value_width(lines: &[KindLine]) -> usize {
    lines.iter().map(|line| line.value.chars().count()).max().unwrap_or(0).max(20)
}

/// `value`, marked stale when its source's last read failed.
fn stale(value: String, failed: bool) -> String {
    if failed { format!("{value} (stale)") } else { value }
}


/// A source by what it reads, marked with its provider: `gpu (nvml)`,
/// `DK-DK2 (electricity-maps)`, `GB direct CO2 (neso)`.
fn named(source: &Source) -> String {
    presentation::provided(presentation::part(&source.description), Some(source.served_by))
}

impl Latest {
    fn take(&mut self, kind: SampleKind, at: Timestamp, value: f64) {
        match kind {
            SampleKind::Instant | SampleKind::Held => self.value = Some(value),
            SampleKind::Total => {
                if let Some((before, total)) = self.last {
                    let seconds = (at - before).num_milliseconds() as f64 / 1000.0;
                    if seconds > 0.0 {
                        self.value = Some((value - total) / seconds);
                    }
                }
            }
        }
        self.last = Some((at, value));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use carbontracker::{Failure, Fidelity, GridLocation, PowerDomain, PowerScope, ProviderId, Sample};
    use chrono::TimeZone;

    use super::*;

    fn id(provider: usize) -> SourceId {
        SourceId::new(ProviderId(provider), 0)
    }

    pub(crate) fn power(provider: usize, by: &'static str, domain: PowerDomain, kind: SampleKind, fidelity: Fidelity) -> Source {
        Source {
            id: id(provider),
            description: SourceDescription::Power {
                device_name: by.into(),
                domain,
                scope: PowerScope::DeviceTotal,
                sample_kind: kind,
                fidelity,
            },
            served_by: by,
        }
    }

    pub(crate) fn intensity(provider: usize, by: &'static str, fidelity: Fidelity) -> Source {
        Source {
            id: id(provider),
            description: SourceDescription::Intensity {
                location: GridLocation::Zone { zone_id: "DK-DK2".into() },
                fidelity,
            },
            served_by: by,
        }
    }

    pub(crate) fn forecast() -> ForecastSource {
        ForecastSource {
            location: GridLocation::Zone { zone_id: "DK-DK2".into() },
            horizon: Duration::from_secs(24 * 3600),
            fidelity: Fidelity::Modelled,
            served_by: "projection",
        }
    }

    fn at(second: u32) -> Timestamp {
        chrono::Utc.with_ymd_and_hms(2026, 11, 20, 10, 0, second).unwrap()
    }

    pub(crate) fn readout(values: &[(usize, u32, f64)]) -> Msg {
        let mut outcomes = HashMap::new();
        for (provider, second, value) in values {
            outcomes.insert(id(*provider), Observation::Samples(vec![Sample::new(at(*second), *value)]));
        }
        Msg::Readout(Arc::new(outcomes))
    }

    /// A probed machine: rapl and nvml power, live intensity, a projected forecast.
    pub(crate) fn probed() -> Live {
        let mut live = Live::new(1.5);
        live.apply(Msg::Decided("rapl", Verdict::Serving(vec!["cpu".into()])));
        live.apply(Msg::Decided("nvml", Verdict::Serving(vec!["gpu".into()])));
        live.apply(Msg::Decided("powermetrics", Verdict::Declined("macOS only".into())));
        live.apply(Msg::Decided("ip-geolocation", Verdict::Serving(vec![])));
        live.apply(Msg::Decided("electricity-maps", Verdict::Serving(vec![])));
        live.apply(Msg::Decided("country-average", Verdict::Covered));
        live.apply(Msg::Decided("static", Verdict::NotWanted));
        live.apply(Msg::Started {
            sources: vec![
                power(1, "rapl", PowerDomain::Cpu, SampleKind::Total, Fidelity::Measured),
                power(2, "nvml", PowerDomain::Gpu, SampleKind::Instant, Fidelity::Measured),
                intensity(3, "electricity-maps", Fidelity::Measured),
            ],
            forecast: Some(forecast()),
        });
        live
    }

    /// AC/FE-77
    #[test]
    fn the_rate_is_power_times_pue_times_intensity_once_every_source_reported() {
        let mut live = probed();
        assert_eq!(live.rate(), None);
        // rapl is a running total: 100 J over 2 s is 50 W.
        live.apply(readout(&[(1, 0, 1000.0), (2, 0, 150.0), (3, 0, 200.0)]));
        assert_eq!(live.rate(), None, "a total needs two samples");
        live.apply(readout(&[(1, 2, 1100.0)]));
        assert_eq!(live.watts(), Some(200.0));
        // 200 W × 1.5 × 200 g/kWh = 60 g/h.
        assert_eq!(live.rate(), Some(60.0));
        assert_eq!(live.headline(), "Emitting 60.0 gCO2eq/h right now (PUE 1.5)");
    }

    /// AC/FE-77
    #[test]
    fn each_kind_shows_its_value_how_and_sources_marked_with_their_provider() {
        let mut live = probed();
        live.apply(readout(&[(1, 0, 1000.0), (2, 0, 150.0), (3, 0, 200.0)]));
        live.apply(readout(&[(1, 2, 1100.0), (2, 2, 160.0), (3, 2, 210.0)]));
        let lines = live.lines();
        let line = |label: &str| lines.iter().find(|line| line.label == label).unwrap().clone();
        assert_eq!(line("power"), KindLine {
            label: "power",
            value: "210.0 W".into(),
            how: Some(How::Live),
            by: "cpu 50.0 W (rapl)\ngpu 160.0 W (nvml)".into(),
            failure: None,
        });
        assert_eq!(line("location").value, "DK-DK2");
        assert_eq!(line("location").by, "(ip-geolocation)");
        assert_eq!(line("intensity").value, "210 gCO2eq/kWh");
        assert_eq!(line("intensity").by, "DK-DK2 (electricity-maps)");
        assert_eq!(line("forecast"), KindLine {
            label: "forecast",
            value: "24 h ahead".into(),
            how: Some(How::Fallback),
            by: "projects the current intensity (projection)".into(),
            failure: None,
        });
    }

    /// AC/FE-77
    #[test]
    fn power_names_each_component_once_grouped_under_its_provider() {
        let source = |index, served_by, domain| Source {
            id: SourceId::new(ProviderId(if served_by == "ioreport" { 1 } else { 2 }), index),
            description: SourceDescription::Power {
                device_name: "Apple M3".into(),
                domain,
                scope: PowerScope::DeviceTotal,
                sample_kind: SampleKind::Instant,
                fidelity: Fidelity::Measured,
            },
            served_by,
        };
        let mut live = Live::new(1.0);
        live.apply(Msg::Started {
            sources: vec![
                source(0, "ioreport", PowerDomain::Cpu),
                source(1, "ioreport", PowerDomain::Gpu),
                source(2, "ioreport", PowerDomain::Gpu),
                source(0, "macmon", PowerDomain::System),
            ],
            forecast: None,
        });
        let read = |provider, index, watts| (SourceId::new(ProviderId(provider), index), watts);
        let readings = [read(1, 0, 20.0), read(1, 1, 5.0), read(1, 2, 3.0)];
        let outcomes = readings.map(|(id, watts)| {
            (id, Observation::Samples(vec![Sample::new(at(0), watts)]))
        });
        live.apply(Msg::Readout(Arc::new(HashMap::from(outcomes))));
        let power = live.lines().into_iter().find(|line| line.label == "power").unwrap();
        assert_eq!(power.by, "cpu 20.0 W, gpu 8.0 W (ioreport)\nsystem … (macmon)");
    }

    #[test]
    fn the_forecast_horizon_is_shown_in_rounded_hours() {
        let mut live = Live::new(1.0);
        let forecast = ForecastSource { horizon: Duration::from_secs_f64(86_342.768), ..forecast() };
        live.apply(Msg::Started { sources: vec![], forecast: Some(forecast) });
        let line = live.lines().into_iter().find(|line| line.label == "forecast").unwrap();
        assert_eq!(line.value, "24 h ahead");
    }

    /// AC/FE-78
    #[test]
    fn a_kind_nothing_serves_shows_none_and_points_to_verbose() {
        let mut live = Live::new(1.0);
        live.apply(Msg::Started { sources: vec![intensity(3, "static", Fidelity::Stated)], forecast: None });
        let lines = live.lines();
        let power = lines.iter().find(|line| line.label == "power").unwrap();
        assert_eq!((power.value.as_str(), power.by.as_str()), ("none", "check -v shows why"));
        let forecast = lines.iter().find(|line| line.label == "forecast").unwrap();
        assert_eq!(forecast.value, "none");
        assert!(lines.iter().all(|line| line.label != "location"), "a stated intensity needs no location");
    }

    /// AC/FE-79
    #[test]
    fn every_candidate_keeps_its_status_and_reason_as_decided() {
        let mut live = Live::new(1.0);
        live.apply(Msg::Probing("rapl"));
        assert_eq!(live.candidates[0].status, Status::Probing);
        live.apply(Msg::Decided("rapl", Verdict::Declined("no readable zones".into())));
        assert_eq!(live.candidates.len(), 1, "a candidate is one row");
        let live = probed();
        let words: Vec<(&str, &str)> =
            live.candidates.iter().map(|candidate| (candidate.name, candidate.status.word())).collect();
        assert!(words.contains(&("powermetrics", "declined")));
        assert!(words.contains(&("country-average", "standby")));
        assert!(words.contains(&("static", "not asked")));
        let declined = live.candidates.iter().find(|candidate| candidate.name == "powermetrics").unwrap();
        assert_eq!(declined.status.reason(), "macOS only");
    }

    /// AC/FE-80
    #[test]
    fn the_snapshot_is_what_the_view_showed_as_plain_text() {
        let mut live = probed();
        live.apply(readout(&[(1, 0, 1000.0), (2, 0, 150.0), (3, 0, 200.0)]));
        live.apply(readout(&[(1, 2, 1100.0)]));
        let plain = live.snapshot(false);
        assert!(plain.starts_with("Emitting 60.0 gCO2eq/h right now"), "{plain}");
        assert!(plain.contains("  intensity  200 gCO2eq/kWh       live      DK-DK2 (electricity-maps)\n"), "{plain}");
        let power: Vec<&str> = plain.lines().skip_while(|line| !line.trim_start().starts_with("power")).take(2).collect();
        let column = power[0].find("cpu").expect("the first provider on the power line");
        assert_eq!(power[1].find("gpu"), Some(column), "each provider on its own line, in one column: {plain}");
        assert!(power[1][..column].trim().is_empty(), "{plain}");
        assert!(!plain.contains("powermetrics"), "candidates only with -v: {plain}");
        assert!(!plain.contains('\x1b'));
        let verbose = live.snapshot(true);
        assert!(verbose.contains("  powermetrics               declined   macOS only\n"), "{verbose}");
    }

    pub(crate) fn em_key() -> KeyNote {
        KeyNote { service: "electricity_maps", hint: "…1234".into(), candidate: "electricity-maps", origin: "the config file".into() }
    }

    /// AC/FE-61
    #[test]
    fn a_refused_key_is_warned_about_once_however_many_zones_were_asked() {
        let mut live = Live::new(1.0).with_keys(vec![em_key()]);
        let reason = "zone DK-DK2: access denied: Electricity Maps refused the key (401 Unauthorized); \
                      zone DK: access denied: Electricity Maps refused the key (401 Unauthorized)";
        live.apply(Msg::Decided("electricity-maps", Verdict::Declined(reason.into())));
        let [warning] = live.warnings().try_into().unwrap();
        assert_eq!(
            warning,
            "! Electricity Maps refused key …1234 (401 Unauthorized)\n    run `carbontracker init` to change it"
        );
        assert!(live.snapshot(false).contains("refused key …1234"), "kept after closing");
    }

    /// AC/FE-77
    #[test]
    fn a_stated_location_names_no_provider() {
        let mut live = Live::new(1.0);
        live.apply(Msg::Decided("stated-location", Verdict::Serving(vec!["DK-DK2".into()])));
        live.apply(Msg::Started { sources: vec![intensity(3, "energinet", Fidelity::Measured)], forecast: Some(forecast()) });
        let lines = live.lines();
        let location = lines.iter().find(|line| line.label == "location").unwrap();
        assert_eq!((location.value.as_str(), location.how, location.by.as_str()), ("DK-DK2", Some(How::Stated), ""));
        let intensity = lines.iter().find(|line| line.label == "intensity").unwrap();
        assert_eq!(intensity.by, "DK-DK2 (energinet)");
    }

    /// AC/FE-61
    #[test]
    fn no_warning_without_a_key_or_when_the_provider_serves() {
        let mut keyless = Live::new(1.0);
        keyless.apply(Msg::Decided("electricity-maps", Verdict::Declined("no Electricity Maps key".into())));
        assert!(keyless.warnings().is_empty());
        let mut serving = Live::new(1.0).with_keys(vec![em_key()]);
        serving.apply(Msg::Decided("electricity-maps", Verdict::Serving(vec![])));
        assert!(serving.warnings().is_empty());
    }

    fn failed(provider: usize) -> Msg {
        Msg::Readout(Arc::new(HashMap::from([(id(provider), Observation::Failed(Failure::TimedOut))])))
    }

    /// AC/FE-83
    #[test]
    fn a_failed_read_shows_on_its_line_and_marks_the_value_stale_until_it_reads_again() {
        let mut live = probed();
        live.apply(readout(&[(1, 0, 1000.0), (2, 0, 150.0), (3, 0, 200.0)]));
        live.apply(failed(3));
        let line = |live: &Live| live.lines().into_iter().find(|line| line.label == "intensity").unwrap();
        assert_eq!(line(&live).value, "200 gCO2eq/kWh (stale)");
        assert_eq!(line(&live).failure.as_deref(), Some("DK-DK2 (electricity-maps): the read timed out"));
        assert!(live.snapshot(false).contains("DK-DK2 (electricity-maps): the read timed out"), "{}", live.snapshot(false));
        live.apply(readout(&[(3, 5, 210.0)]));
        assert_eq!(line(&live).value, "210 gCO2eq/kWh");
        assert_eq!(line(&live).failure, None);
    }

    /// AC/FE-83
    #[test]
    fn a_source_that_never_read_shows_why() {
        let mut live = probed();
        live.apply(failed(2));
        let power = live.lines().into_iter().find(|line| line.label == "power").unwrap();
        assert_eq!(power.value, "…");
        assert_eq!(power.failure.as_deref(), Some("gpu (nvml): the read timed out"));
        assert!(live.headline().contains("gpu (nvml): the read timed out"), "{}", live.headline());
    }

    #[test]
    fn a_stale_value_keeps_the_columns_aligned() {
        let mut live = probed();
        live.apply(readout(&[(1, 0, 1000.0), (2, 0, 150.0), (3, 0, 200.0)]));
        live.apply(readout(&[(1, 2, 1100.0)]));
        live.apply(failed(3));
        let plain = live.snapshot(false);
        let column = |label: &str| {
            let line = plain.lines().find(|line| line.trim_start().starts_with(label)).unwrap();
            line.find(" live ").or_else(|| line.find(" fallback ")).expect(line)
        };
        assert_eq!(column("power"), column("intensity"), "{plain}");
        assert_eq!(column("power"), column("forecast"), "{plain}");
    }

    #[test]
    fn before_the_run_starts_every_kind_is_being_probed() {
        let live = Live::new(1.0);
        assert!(live.lines().iter().all(|line| line.value == "…"));
        assert_eq!(live.headline(), "Probing this machine…");
    }
}

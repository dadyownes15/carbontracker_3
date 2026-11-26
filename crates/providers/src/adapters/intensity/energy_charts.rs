//! Energy-Charts (Fraunhofer ISE): live grid intensity by country, and its
//! forecast for the same country, from the `co2eq` endpoint.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, Sample, SourceId};
use carbontracker::{Fidelity, GridLocation, SourceDescription, Timestamp};

use crate::adapters::intensity::electricity_maps::cut_to_horizon;
use crate::application::forecaster::Fetch;
use crate::domain::location::{CountryCode, Location};
use crate::ports::{Outcome, Probed};

/// Energy-Charts publishes every 15 minutes and limits requests per client.
const ENERGY_CHARTS_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// How long a request made from a read may wait on the service.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const ENERGY_CHARTS_URL: &str = "https://api.energy-charts.info";
/// The candidate's name: Energy-Charts publishes CO2 equivalents.
pub(crate) const ENERGY_CHARTS: &str = "energy-charts (CO2eq)";

/// The countries Energy-Charts publishes `co2eq` for, and its code for each.
const COUNTRIES: [(&str, &str); 37] = [
    ("AT", "at"), ("BA", "ba"), ("BE", "be"), ("BG", "bg"), ("CH", "ch"), ("CY", "cy"), ("CZ", "cz"),
    ("DE", "de"), ("DK", "dk"), ("EE", "ee"), ("ES", "es"), ("FI", "fi"), ("FR", "fr"), ("GB", "uk"),
    ("GE", "ge"), ("GR", "gr"), ("HR", "hr"), ("HU", "hu"), ("IE", "ie"), ("IT", "it"), ("LT", "lt"),
    ("LU", "lu"), ("LV", "lv"), ("MD", "md"), ("ME", "me"), ("MK", "mk"), ("NL", "nl"), ("NO", "no"),
    ("PL", "pl"), ("PT", "pt"), ("RO", "ro"), ("RS", "rs"), ("SE", "se"), ("SI", "si"), ("SK", "sk"),
    ("UA", "ua"), ("XK", "xk"),
];

/// Where the location is read: the country, and Energy-Charts' code for it.
/// A zone finer than a country is read as its country.
pub(crate) fn country_of(location: Option<&Location>) -> Result<(CountryCode, &'static str), String> {
    let location = location.ok_or("no location")?;
    let country = location.country().ok_or_else(|| format!("{location} names no country"))?;
    COUNTRIES
        .iter()
        .find(|(code, _)| *code == country.as_str())
        .map(|(_, asked_as)| (country, *asked_as))
        .ok_or_else(|| format!("Energy-Charts publishes no co2eq for {country}"))
}

/// `{"unix_seconds": […], "co2eq": […], "co2eq_forecast": […]}`: each
/// interval's start, its figure if published, and its forecast if any.
#[derive(serde::Deserialize)]
struct Answer {
    unix_seconds: Vec<i64>,
    co2eq: Vec<Option<f64>>,
    #[serde(default)]
    co2eq_forecast: Vec<Option<f64>>,
}

impl Answer {
    fn parse(body: &str) -> Result<Self, ProviderError> {
        let answer: Self = serde_json::from_str(body)
            .map_err(|_| ProviderError::Invalid("a co2eq answer that does not parse".to_owned()))?;
        let lengths_match = answer.co2eq.len() == answer.unix_seconds.len()
            && (answer.co2eq_forecast.is_empty() || answer.co2eq_forecast.len() == answer.unix_seconds.len());
        if !lengths_match {
            return Err(ProviderError::Invalid("a co2eq answer with series of unequal length".to_owned()));
        }
        Ok(answer)
    }

    fn series<'a>(&'a self, values: &'a [Option<f64>]) -> impl Iterator<Item = Sample> + 'a {
        self.unix_seconds.iter().zip(values).filter_map(|(seconds, value)| {
            Some(Sample::new(DateTime::<Utc>::from_timestamp(*seconds, 0)?, (*value)?))
        })
    }

    /// The figure for the latest interval that has begun.
    fn latest(&self, now: DateTime<Utc>) -> Result<Sample, ProviderError> {
        self.series(&self.co2eq)
            .filter(|sample| *sample.observed_at() <= now)
            .max_by_key(|sample| *sample.observed_at())
            .ok_or_else(|| ProviderError::Invalid("no co2eq for an interval that has begun".to_owned()))
    }

    fn forecast(&self) -> Vec<Sample> {
        self.series(&self.co2eq_forecast).collect()
    }
}

fn url(base_url: &str) -> String {
    format!("{base_url}/co2eq")
}

/// The only parameter sent: no key, nothing finer than the country.
fn params(asked_as: &str) -> [(&'static str, &str); 1] {
    [("country", asked_as)]
}

fn unreachable(error: reqwest::Error) -> ProviderError {
    ProviderError::Unavailable(crate::adapters::http::failure("Energy-Charts", &error))
}

/// The error for a status that refuses the request, if it does.
fn refusal(status: reqwest::StatusCode) -> Option<ProviderError> {
    match status.as_u16() {
        200..=299 => None,
        429 => Some(ProviderError::RateLimited(format!("Energy-Charts was asked too often ({status})"))),
        _ => Some(ProviderError::Unavailable(format!("Energy-Charts answered {status}"))),
    }
}

async fn fetch(client: &reqwest::Client, base_url: &str, asked_as: &str) -> Result<Answer, ProviderError> {
    let response = client.get(url(base_url)).query(&params(asked_as)).send().await.map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    Answer::parse(&response.text().await.map_err(unreachable)?)
}

/// Blocking fetch, for reads and the forecaster. Builds its own client, so
/// it never runs on, or outlives, an async runtime.
fn blocking_fetch(base_url: &str, asked_as: &str) -> Result<Answer, ProviderError> {
    let client = reqwest::blocking::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(unreachable)?;
    let response = client.get(url(base_url)).query(&params(asked_as)).send().map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    Answer::parse(&response.text().map_err(unreachable)?)
}

/// The intensity of the location's country; declines without a request
/// outside the countries Energy-Charts publishes.
pub(crate) async fn probe_energy_charts(
    id: ProviderId,
    client: &reqwest::Client,
    base_url: &str,
    location: Option<&Location>,
) -> Probed<(Outcome, f64)> {
    let (country, asked_as) = country_of(location)?;
    let current = fetch(client, base_url, asked_as)
        .await
        .and_then(|answer| answer.latest(Utc::now()))
        .map_err(|error| format!("{country}: {error}"))?;
    let g_per_kwh = current.value();

    let source = SourceId::new(id, 0);
    let provider = EnergyCharts {
        base_url: base_url.to_owned(),
        asked_as,
        source,
        // The confirming fetch above answers the first read.
        primed: Some(current),
        last: None,
    };
    let description = SourceDescription::Intensity {
        location: GridLocation::Country { country_code: country.to_string() },
        fidelity: Fidelity::Measured,
    };
    Ok((Outcome::single(source, description, provider), g_per_kwh))
}

/// The latest intensity of one country.
struct EnergyCharts {
    base_url: String,
    asked_as: &'static str,
    source: SourceId,
    /// The probe's confirming fetch, answered once instead of refetching.
    primed: Option<Sample>,
    /// The start of the interval last reported.
    last: Option<Timestamp>,
}

impl Provider for EnergyCharts {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let sample = match self.primed.take() {
            Some(sample) => sample,
            None => blocking_fetch(&self.base_url, self.asked_as)?.latest(Utc::now())?,
        };
        let mut response = ProviderResponse::new();
        if self.last.replace(*sample.observed_at()) == Some(*sample.observed_at()) {
            response.insert(self.source, Reading::Unchanged);
        } else {
            response.samples(self.source, vec![sample]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(ENERGY_CHARTS_COOLDOWN)
    }
}

/// What the forecast probe confirmed: the country, the curve it fetched, how
/// far the curve reaches, and how to fetch it again.
pub(crate) struct EnergyChartsForecast {
    pub(crate) country: CountryCode,
    pub(crate) curve: Vec<Sample>,
    pub(crate) horizon: Duration,
    pub(crate) fetch: Fetch,
}

/// The forecast for the location's country, up to `horizon` or as far as
/// Energy-Charts reaches. Declines when no point lies ahead.
pub(crate) async fn probe_energy_charts_forecast(
    client: &reqwest::Client,
    base_url: &str,
    location: Option<&Location>,
    horizon: Duration,
) -> Probed<EnergyChartsForecast> {
    let (country, asked_as) = country_of(location)?;
    let curve = fetch(client, base_url, asked_as).await.map_err(|error| format!("{country}: {error}"))?.forecast();
    let now = Utc::now();
    let reach = curve
        .last()
        .and_then(|last| (*last.observed_at() - now).to_std().ok())
        .filter(|reach| !reach.is_zero())
        .ok_or_else(|| format!("the forecast for {country} has no point ahead"))?;

    let base_url = base_url.to_owned();
    let fetch: Fetch = Arc::new(move || {
        let curve = blocking_fetch(&base_url, asked_as).ok()?.forecast();
        Some(cut_to_horizon(&curve, Utc::now(), horizon))
    });
    Ok(EnergyChartsForecast { country, curve: cut_to_horizon(&curve, now, horizon), horizon: horizon.min(reach), fetch })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::location::{Coordinates, Estimate};
    use carbontracker::GridLocation;

    fn stated(location: GridLocation) -> Result<String, String> {
        country_of(Some(&Location::Stated(location))).map(|(country, _)| country.to_string())
    }

    fn zone(id: &str) -> GridLocation {
        GridLocation::Zone { zone_id: id.to_owned() }
    }

    fn country(code: &str) -> GridLocation {
        GridLocation::Country { country_code: code.to_owned() }
    }

    /// AC/OPEN-8 AC/OPEN-9 AC/OPEN-11
    #[test]
    fn a_stated_zone_or_country_is_read_as_its_country() {
        assert_eq!(stated(zone("DE")), Ok("DE".to_owned()));
        assert_eq!(stated(zone("SE-SE3")), Ok("SE".to_owned()));
        assert_eq!(stated(country("de")), Ok("DE".to_owned()));
    }

    #[test]
    fn great_britain_is_asked_for_by_energy_charts_code() {
        assert_eq!(country_of(Some(&Location::Stated(country("GB")))).map(|(_, code)| code), Ok("uk"));
    }

    /// AC/OPEN-10
    #[test]
    fn an_estimate_is_read_as_the_country_it_lies_in() {
        let estimate = Estimate { country: CountryCode::new("FR").unwrap(), coordinates: Coordinates::new(48.86, 2.35), by: "test" };
        let (country, code) = country_of(Some(&Location::Estimated(estimate))).unwrap();
        assert_eq!((country.as_str(), code), ("FR", "fr"));
    }

    /// AC/OPEN-10
    #[test]
    #[ignore = "known: stated coordinates are not placed in a country; no reverse geocoder"]
    fn stated_coordinates_are_read_as_the_country_they_lie_in() {
        assert_eq!(stated(GridLocation::Coordinates { latitude: 52.5, longitude: 13.4 }), Ok("DE".to_owned()));
    }

    /// AC/OPEN-6
    #[test]
    fn a_country_outside_the_table_is_declined_saying_which() {
        let reason = stated(country("US")).unwrap_err();
        assert!(reason.contains("US"), "{reason}");
        let reason = stated(zone("US-CAL-CISO")).unwrap_err();
        assert!(reason.contains("US"), "{reason}");
    }

    #[test]
    fn a_location_naming_no_country_is_declined() {
        assert!(stated(GridLocation::Global).is_err());
        assert!(country_of(None).is_err());
    }
}

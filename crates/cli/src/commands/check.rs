//! `carbontracker check`: the available sources and the keys given.

use carbontracker_providers::KeyedService;

use crate::args::Args;

/// The check view on a terminal; otherwise what it shows after its first
/// reading, printed once.
pub fn check(args: &Args) -> Result<i32, String> {
    if crate::screen::available() {
        return crate::screen::check(args);
    }
    crate::screen::check_once(args)
}

pub(crate) use carbontracker_run::presentation::How;

/// A duration in hours with at most one decimal: `24`, `1.5`.
pub(crate) fn hours(duration: std::time::Duration) -> String {
    let text = format!("{:.1}", duration.as_secs_f64() / 3600.0);
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

/// The candidate a keyed service's key unlocks.
pub(crate) fn candidate_of(service: KeyedService) -> &'static str {
    match service {
        KeyedService::ElectricityMaps => "electricity-maps",
    }
}

/// The warning for a key whose candidate declined, and why. Only a refused
/// key points to `init`: when the service could not be reached, a new key
/// would not help.
pub(crate) fn key_warning(service: &str, hint: &str, candidate: &str, reason: &str) -> String {
    match refusal(reason) {
        Some((by, status)) => format!("! {by} refused key {hint} {status}\n    run `carbontracker init` to change it"),
        None => format!("! {service} key {hint} found, but {candidate} declined: {reason}"),
    }
}

/// Who refused the key and with what status, from the providers' words for
/// a 401 or 403, e.g. `zone DK: access denied: Electricity Maps refused the
/// key (401 Unauthorized)`. Every zone asked gives the same refusal, so the
/// first is enough.
fn refusal(reason: &str) -> Option<(&str, &str)> {
    let first = reason.split("; ").next()?;
    let (before, status) = first.split_once(" refused the key ")?;
    let by = before.rsplit(": ").next()?;
    Some((by, status))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC/FE-61
    #[test]
    fn a_key_whose_service_cannot_be_reached_names_the_reason_once() {
        let reason = "unavailable: cannot connect to Electricity Maps: no network, or it is down";
        assert_eq!(
            key_warning("electricity_maps", "…1234", "electricity-maps", reason),
            format!("! electricity_maps key …1234 found, but electricity-maps declined: {reason}")
        );
    }

    #[test]
    fn hours_keep_at_most_one_decimal() {
        let hours = |seconds: f64| hours(std::time::Duration::from_secs_f64(seconds));
        assert_eq!(hours(86_342.768), "24");
        assert_eq!(hours(24.0 * 3600.0), "24");
        assert_eq!(hours(5400.0), "1.5");
        assert_eq!(hours(900.0), "0.2");
    }
}

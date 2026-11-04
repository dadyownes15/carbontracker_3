//! IP geolocation: estimates where the host is from its public IP, through
//! ipinfo.io (no token needed at low volume).

use crate::domain::location::{Coordinates, CountryCode, Estimate};

pub(crate) const IP_GEOLOCATION: &str = "ip-geolocation";

pub(crate) const IP_GEOLOCATION_URL: &str = "https://ipinfo.io/json";

/// One request, no retry; the client's timeout bounds it. Any failure is no
/// estimate.
pub(crate) async fn estimate_by_ip(client: &reqwest::Client, url: &str) -> Option<Estimate> {
    let body = client.get(url).send().await.ok()?.error_for_status().ok()?.text().await.ok()?;
    parse_ipinfo(&body)
}

/// `{"country": "DK", "loc": "55.6759,12.5655", …}`. An answer without a
/// valid country is no answer; bad or missing coordinates are dropped.
pub(crate) fn parse_ipinfo(body: &str) -> Option<Estimate> {
    let answer = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let country = CountryCode::new(answer.get("country")?.as_str()?)?;
    let coordinates = answer.get("loc").and_then(|loc| loc.as_str()).and_then(|loc| {
        let (latitude, longitude) = loc.split_once(',')?;
        Coordinates::new(latitude.trim().parse().ok()?, longitude.trim().parse().ok()?)
    });
    Some(Estimate { country, coordinates, by: IP_GEOLOCATION })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_gives_country_and_coordinates() {
        let estimate = parse_ipinfo(r#"{"ip":"192.0.2.1","country":"DK","loc":"55.6759,12.5655"}"#).unwrap();
        assert_eq!(estimate.country.as_str(), "DK");
        assert_eq!(estimate.coordinates, Coordinates::new(55.6759, 12.5655));
        assert_eq!(estimate.by, IP_GEOLOCATION);
    }

    #[test]
    fn a_lowercase_country_is_uppercased() {
        let estimate = parse_ipinfo(r#"{"country":"dk"}"#).unwrap();
        assert_eq!(estimate.country.as_str(), "DK");
    }

    #[test]
    fn missing_or_bad_coordinates_keep_the_country() {
        for body in [r#"{"country":"DK"}"#, r#"{"country":"DK","loc":"north"}"#, r#"{"country":"DK","loc":"95,12"}"#] {
            let estimate = parse_ipinfo(body).unwrap();
            assert_eq!(estimate.coordinates, None, "{body}");
        }
    }

    #[test]
    fn an_answer_without_a_valid_country_is_no_answer() {
        for body in [r#"{"ip":"127.0.0.1","bogon":true}"#, r#"{"country":"DNK"}"#, r#"{"country":7}"#, "not json"] {
            assert_eq!(parse_ipinfo(body), None, "{body}");
        }
    }
}

//! [StaticProvider]: sources whose value never changes.

use carbontracker::{Cooldown, Provider, ProviderError, ProviderResponse, Sample, SourceId};
use chrono::Utc;

/// Serves sources whose value never changes: a stated draw or intensity, or
/// a modelled figure such as a country average. Each source has one value,
/// given when it is built; its description says where the value came from.
/// It is read once: its first read reports each value, stamped at that read,
/// and the series holds it from then on. Its read never fails.
#[derive(Debug, Clone)]
pub struct StaticProvider {
    values: Vec<(SourceId, f64)>,
}

impl StaticProvider {
    /// Serves each source with its value. Every source must be of one kind:
    /// power, or intensity.
    pub fn new(values: impl IntoIterator<Item = (SourceId, f64)>) -> Self {
        Self { values: values.into_iter().collect() }
    }
}

impl Provider for StaticProvider {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let at = Utc::now();
        let mut response = ProviderResponse::new();
        for &(source, value) in &self.values {
            response.samples(source, vec![Sample::new(at, value)]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::Never
    }
}

#[cfg(test)]
mod tests {
    use carbontracker::{ProviderId, Reading};

    use super::*;

    /// AC/PROV-25 AC/PROV-32
    #[test]
    fn it_reports_each_value_stamped_at_the_read_and_never_fails() {
        let (power, other) = (SourceId::new(ProviderId(1), 0), SourceId::new(ProviderId(1), 1));
        let mut provider = StaticProvider::new([(power, 42.0), (other, 7.5)]);

        let before = Utc::now();
        let response = provider.read().unwrap();
        let after = Utc::now();
        for (source, value) in [(power, 42.0), (other, 7.5)] {
            let Some(Reading::Samples(samples)) = response.get(source) else { panic!("{response:?}") };
            assert_eq!(samples.len(), 1);
            assert_eq!(samples[0].value(), value);
            assert!(before <= *samples[0].observed_at() && *samples[0].observed_at() <= after, "stamped at the read");
        }
        assert_eq!(provider.cooldown(), Cooldown::Never);
        assert!(provider.read().is_ok(), "a later read never fails");
    }
}

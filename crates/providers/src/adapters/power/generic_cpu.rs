//! Generic CPU: TDP table estimate (always available, lowest fidelity).

use std::collections::HashMap;
use std::sync::LazyLock;


use crate::adapters::constant::StaticProvider;
use crate::adapters::system::cpu::cpu_brand;
use crate::ports::Outcome;
use carbontracker::{ProviderId, SourceId};
use carbontracker::SampleKind;
use carbontracker::{Fidelity, PowerDomain, PowerScope, SourceDescription};

/// Assumed average load when all we have is a nameplate TDP.
const ASSUMED_CPU_UTILISATION: f64 = 0.5;

/// `cpu-power.csv`: CPU model names and their TDP in watts, from
/// carbontracker 2.
const TABLE: &str = include_str!("../../../data/cpu-power.csv");

struct TdpTable {
    by_model: HashMap<String, f64>,
    /// For CPUs not in the table.
    average: f64,
}

static TDPS: LazyLock<TdpTable> = LazyLock::new(|| {
    let rows: Vec<(&str, f64)> = TABLE
        .lines()
        .skip(1)
        .filter_map(|row| {
            let (name, tdp) = row.split_once(',')?;
            Some((name, tdp.trim().parse().ok()?))
        })
        .collect();
    let average = rows.iter().map(|(_, tdp)| tdp).sum::<f64>() / rows.len() as f64;
    let mut by_model = HashMap::new();
    for (name, tdp) in rows {
        // Variants that differ only in a note, e.g. "(95W)", keep the first.
        by_model.entry(model_key(name)).or_insert(tdp);
    }
    TdpTable { by_model, average }
});

pub(crate) fn probe_generic_cpu(id: ProviderId) -> Outcome {
    generic_cpu(id, cpu_brand())
}

/// The modelled draw of the CPU named `brand`.
fn generic_cpu(id: ProviderId, brand: Option<String>) -> Outcome {
    let watts = brand.as_deref().and_then(table_tdp).unwrap_or(TDPS.average) * ASSUMED_CPU_UTILISATION;
    let sid = SourceId::new(id, 0);
    Outcome::single(
        sid,
        SourceDescription::Power {
            device_name: brand.unwrap_or_else(|| "Unknown CPU".to_owned()),
            domain: PowerDomain::Cpu,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Modelled,
        },
        StaticProvider::new([(sid, watts)]),
    )
}

/// The table's TDP for `brand`, if it lists the model.
fn table_tdp(brand: &str) -> Option<f64> {
    TDPS.by_model.get(&model_key(brand)).copied()
}

/// The model in a CPU name, lowercased, so the OS's brand string and the
/// table's name agree: "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz" and
/// "AMD Ryzen 9 5950X 16-Core Processor" become "intel core i7-9750h" and
/// "amd ryzen 9 5950x".
fn model_key(name: &str) -> String {
    let name = name.to_lowercase().replace(['®', '™'], " ");
    let name = name.split('@').next().unwrap_or_default();
    // "… with Radeon Graphics"
    let name = name.split(" with ").next().unwrap_or_default();

    // Notes in parentheses: "(R)", "(TM)", "(OEM Only)".
    let mut bare = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                bare.push(' ');
            }
            _ if depth == 0 => bare.push(c),
            _ => {}
        }
    }

    bare.split_whitespace()
        .filter(|word| !matches!(*word, "cpu" | "processor" | "gen"))
        .filter(|word| !is_count(word, "-core") && !is_ordinal(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// "16-core"
fn is_count(word: &str, suffix: &str) -> bool {
    word.strip_suffix(suffix).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// "12th", as in "12th Gen Intel(R) Core(TM) i7-12700K"
fn is_ordinal(word: &str) -> bool {
    ["st", "nd", "rd", "th"].iter().any(|suffix| is_count(word, suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_parses_every_row() {
        let rows = TABLE.lines().skip(1).filter(|row| row.split_once(',').is_some_and(|(_, tdp)| tdp.parse::<f64>().is_ok()));
        assert_eq!(rows.count(), TABLE.lines().count() - 1);
    }

    #[test]
    fn brand_strings_match_their_table_row() {
        for (brand, tdp) in [
            ("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz", 45.0),
            ("Intel(R) Xeon(R) Gold 6130 CPU @ 2.10GHz", 125.0),
            ("Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz", 120.0),
            ("AMD Ryzen 9 5950X 16-Core Processor", 105.0),
            ("AMD EPYC 7742 64-Core Processor", 225.0),
            ("AMD Ryzen 7 5800H with Radeon Graphics", 54.0),
            ("Apple M1", 10.0),
        ] {
            assert_eq!(table_tdp(brand), Some(tdp), "{brand}");
        }
    }

    #[test]
    fn a_model_does_not_match_its_variants() {
        // i7-6700 is listed before i7-6700K; neither stands in for the other.
        assert_eq!(table_tdp("Intel(R) Core(TM) i7-6700 CPU @ 3.40GHz"), Some(65.0));
        assert_eq!(table_tdp("Intel(R) Core(TM) i7-6700K CPU @ 4.00GHz"), Some(95.0));
        assert_eq!(table_tdp("Intel(R) Core(TM) i7-670"), None);
    }

    #[test]
    fn a_generation_prefix_is_ignored() {
        assert_eq!(model_key("12th Gen Intel(R) Core(TM) i7-12700K"), "intel core i7-12700k");
    }

    /// AC/PROV-46 AC/PROV-32
    #[test]
    fn a_listed_cpu_is_modelled_at_half_its_tdp_as_a_static_instant() {
        let mut outcome = generic_cpu(ProviderId(5), Some("AMD Ryzen 9 5950X 16-Core Processor".to_owned()));

        let [(source, description)] = &outcome.descriptions[..] else { panic!("one CPU source") };
        assert!(matches!(
            description,
            SourceDescription::Power { domain: PowerDomain::Cpu, sample_kind: SampleKind::Instant, fidelity: Fidelity::Modelled, .. }
        ));
        let response = outcome.provider.read().unwrap();
        assert!(matches!(response.get(*source), Some(carbontracker::Reading::Samples(samples)) if samples[0].value() == 52.5), "105 W TDP at half load");
        assert_eq!(outcome.provider.cooldown(), carbontracker::Cooldown::Never);
    }

    /// AC/PROV-46
    #[test]
    fn an_unlisted_cpu_draws_half_the_table_average() {
        assert_eq!(table_tdp("Definitely Not A CPU"), None);
        assert!((30.0..40.0).contains(&(TDPS.average * ASSUMED_CPU_UTILISATION)));
    }
}

use super::*;

/// A CPU at a constant 10 W on provider 1, its grid at 100 g/kWh on
/// provider 9, PUE 1.5, then `events`.
fn case(events: serde_json::Value) -> Output {
    let case = serde_json::json!({
        "pue": 1.5,
        "sources": [
            {"kind": "power", "id": "1.0", "domain": "cpu", "sample": "instant"},
            {"kind": "intensity", "id": "9.0"}
        ],
        "pairs": {"1.0": "9.0"},
        "events": events,
    });
    let trace = run_json(&case.to_string()).expect("a valid case");
    serde_json::from_str::<serde_json::Value>(&trace).expect("json");
    run(&serde_json::from_value(case).unwrap()).unwrap()
}

/// Provider 1 reads 10 W and provider 9 reads 100 g/kWh, at `us`.
fn reads(cpu: usize, grid: usize, us: u64) -> serde_json::Value {
    serde_json::json!([
        {"at_us": us, "op": "land", "request": cpu, "samples": {"1.0": [[us, 10.0]]}},
        {"at_us": us, "op": "land", "request": grid, "samples": {"9.0": [[us, 100.0]]}}
    ])
}

fn events(parts: &[serde_json::Value]) -> serde_json::Value {
    serde_json::Value::Array(parts.iter().flat_map(|part| part.as_array().cloned().unwrap_or_else(|| vec![part.clone()])).collect())
}

const S: u64 = 1_000_000;

#[test]
fn a_span_over_constant_power_is_billed_watts_times_width_times_pue() {
    let output = case(events(&[
        reads(1, 2, 0),
        serde_json::json!({"at_us": S, "op": "start", "span": 1, "relies": {"1": 3, "9": 4}}),
        reads(3, 4, S),
        serde_json::json!({"at_us": 3 * S, "op": "stop", "span": 1, "relies": {"1": 5, "9": 6}}),
        reads(5, 6, 3 * S),
        serde_json::json!({"at_us": 4 * S, "op": "finish", "relies": {"1": 7, "9": 8}}),
        reads(7, 8, 4 * S),
    ]));
    // 10 W × 2 s × 1.5.
    let span = output.ledgers[&1]["1.0"];
    assert!((span.energy_j - 30.0).abs() < 1e-9, "{span:?}");
    // 30 J is 30 / 3.6e6 kWh, at 100 g/kWh.
    assert!((span.emissions_g.unwrap() - 30.0 / 3.6e6 * 100.0).abs() < 1e-12);
    // The root holds the other 2 s.
    assert!((output.ledgers[&0]["1.0"].energy_j - 30.0).abs() < 1e-9);
    assert!((output.session.total_energy_j - 60.0).abs() < 1e-9);
    assert!(output.session.drained);
    assert_eq!(output.settled.iter().map(|s| s.span).collect::<Vec<_>>(), vec![1]);
    let cuts: Vec<(u64, u64)> = output.segments.iter().map(|s| (s.from_us, s.to_us)).collect();
    assert_eq!(cuts, vec![(0, S), (S, 3 * S), (3 * S, 4 * S)]);
}

#[test]
fn a_session_with_only_finish_bills_the_root_from_open_to_finish() {
    let output = case(events(&[
        reads(1, 2, 0),
        serde_json::json!({"at_us": 2 * S, "op": "finish", "relies": {"1": 3, "9": 4}}),
        reads(3, 4, 2 * S),
    ]));
    assert!((output.ledgers[&0]["1.0"].energy_j - 30.0).abs() < 1e-9, "{:?}", output.ledgers);
    assert!(output.session.drained);
}

#[test]
fn a_stop_settles_when_the_readout_it_relies_on_lands_not_before() {
    let output = case(events(&[
        reads(1, 2, 0),
        serde_json::json!({"at_us": S, "op": "start", "span": 1, "relies": {"1": 3, "9": 4}}),
        reads(3, 4, S),
        serde_json::json!({"at_us": 2 * S, "op": "stop", "span": 1, "relies": {"1": 5, "9": 6}}),
        serde_json::json!({"at_us": 2 * S, "op": "land", "request": 6, "samples": {"9.0": [[2 * S, 100.0]]}}),
        serde_json::json!({"at_us": 5 * S, "op": "land", "request": 5, "samples": {"1.0": [[2 * S, 10.0]]}}),
    ]));
    assert_eq!(output.settled.len(), 1);
    assert_eq!((output.settled[0].span, output.settled[0].at_us), (1, 5 * S));
    assert!(!output.session.drained, "no finish was sent");
}

#[test]
fn a_cut_relying_on_a_readout_already_landed_accounts_at_once() {
    let output = case(events(&[
        reads(1, 2, 0),
        serde_json::json!({"at_us": S, "op": "start", "span": 1, "relies": {"1": 1, "9": 2}}),
        serde_json::json!({"at_us": 2 * S, "op": "stop", "span": 1, "relies": {"1": 1, "9": 2}}),
    ]));
    assert_eq!(output.settled.iter().map(|s| (s.span, s.at_us)).collect::<Vec<_>>(), vec![(1, 2 * S)]);
}

#[test]
fn a_refused_command_is_reported_with_its_event() {
    let output = case(events(&[serde_json::json!({"at_us": 0, "op": "stop", "span": 7})]));
    assert_eq!(output.refused.len(), 1);
    assert_eq!(output.refused[0].event, 0);
}

#[test]
fn a_case_naming_an_undeclared_source_is_an_error() {
    let case = serde_json::json!({
        "pue": 1.0, "sources": [], "events": [{"at_us": 0, "op": "land", "request": 1, "samples": {"3.0": [[0, 1.0]]}}]
    });
    assert_eq!(run_json(&case.to_string()), Err("the case gives samples of 3.0, which it does not declare".to_owned()));
}

#[test]
fn a_settled_span_reports_its_samples_and_warnings() {
    let output = case(events(&[
        reads(1, 2, 0),
        serde_json::json!({"at_us": S, "op": "start", "span": 1, "relies": {"1": 3, "9": 4}}),
        reads(3, 4, S),
        serde_json::json!({"at_us": 2 * S, "op": "start", "span": 2, "relies": {"1": 5, "9": 6}}),
        reads(5, 6, 2 * S),
        // A read inside span 2 only.
        serde_json::json!({"at_us": 2 * S + 500, "op": "land", "request": 9, "samples": {"1.0": [[2 * S + 500, 10.0]]}}),
        serde_json::json!({"at_us": 3 * S, "op": "stop", "span": 1, "relies": {"1": 7, "9": 8}}),
        serde_json::json!({"at_us": 3 * S, "op": "stop", "span": 2, "relies": {"1": 7, "9": 8}}),
        reads(7, 8, 3 * S),
    ]));
    let report = |span| output.spans.iter().find(|r| r.span == span).expect("reported");
    // Span 1 holds the samples at 1 s, 2 s and 2 s + 500 us; span 2 the last two.
    assert_eq!(report(1).samples["1.0"], 3);
    assert_eq!(report(2).samples["1.0"], 2);
    assert!(!report(2).warnings.contains(&"thin_coverage".to_owned()), "{:?}", report(2).warnings);
}

//! Counterexamples QuickCheck found, replayed through `replay-live` so they
//! run without GHC. Each case is the JSON QuickCheck wrote to
//! `quickcheck/failures/`, kept in `replay-cases/`, with a test stating the
//! rule it broke. Run with `cargo test -p carbontracker --features replay
//! --test replay_cases`. AC/TST-37

use carbontracker::replay::live::{Input, Output, run};

fn replay(name: &str) -> Output {
    let path = format!("{}/tests/replay-cases/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let case = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let input: Input = serde_json::from_str(&case).unwrap_or_else(|error| panic!("{path}: {error}"));
    run(&input).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// AC/MEAS-9, AC/MEAS-14: provider 1 panics at 4.34 s and is retired. Two
/// cuts at 4.340297 s, after it, sent it a request each instead of relying
/// on the first answer.
#[test]
fn cuts_to_a_retired_provider_send_it_at_most_one_request() {
    let output = replay("retired-provider-cuts");
    let ours = || output.requests.iter().filter(|request| request.provider == 1);
    let retired_at = ours()
        .filter(|request| request.failure.as_deref().is_some_and(|failure| failure.starts_with("the read panicked")))
        .filter_map(|request| request.answered_us)
        .min()
        .expect("provider 1 panics");
    let sent: Vec<usize> = ours()
        .filter(|request| request.asker == "cut" && request.sent_us >= retired_at)
        .map(|request| request.id)
        .collect();
    assert!(sent.len() <= 1, "requests sent to the retired provider: {sent:?}");
}

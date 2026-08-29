use std::time::Duration;

use stcli_core::EcmaRegexWorker;

#[test]
fn binary_regex_worker_round_trips_ecmascript_matches() {
    let worker = EcmaRegexWorker::new(env!("CARGO_BIN_EXE_stcli"), Duration::from_secs(2));

    assert!(
        worker
            .is_match(r"(\w)\1(?= door)", "i", "Library has a bookk door")
            .unwrap()
    );
    assert!(!worker.is_match("^library$", "i", "library door").unwrap());
}

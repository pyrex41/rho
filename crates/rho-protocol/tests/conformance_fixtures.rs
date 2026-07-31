use rho_protocol::{EventStreamValidator, RunEvent, RunRequest, ValidationError};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()))
}

fn validate_stream(name: &str) -> Result<(), ValidationError> {
    let mut validator = EventStreamValidator::default();
    for (index, line) in fixture(name).lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: RunEvent = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{name}: invalid JSON on line {}: {error}", index + 1));
        validator.push(&event)?;
    }
    validator.finish()
}

#[test]
fn valid_request_fixture_is_accepted() {
    let request: RunRequest = serde_json::from_str(&fixture("valid_request.json")).unwrap();
    assert_eq!(request.validate(), Ok(()));
}

#[test]
fn wrong_request_protocol_fixture_is_rejected() {
    let request: RunRequest =
        serde_json::from_str(&fixture("invalid_request_wrong_protocol.json")).unwrap();
    assert_eq!(
        request.validate(),
        Err(ValidationError::UnsupportedProtocol("rho.run/v2".into()))
    );
}

#[test]
fn valid_event_stream_fixture_is_accepted() {
    assert_eq!(validate_stream("valid_completed_stream.jsonl"), Ok(()));
}

#[test]
fn invalid_event_stream_fixtures_are_rejected_for_the_documented_reason() {
    let cases = [
        (
            "invalid_stream_wrong_protocol.jsonl",
            ValidationError::UnsupportedProtocol("rho.run/v2".into()),
        ),
        (
            "invalid_stream_non_monotonic_seq.jsonl",
            ValidationError::NonMonotonicSequence {
                previous: 2,
                received: 2,
            },
        ),
        (
            "invalid_stream_mixed_run_ids.jsonl",
            ValidationError::RunIdChanged,
        ),
        (
            "invalid_stream_missing_terminal.jsonl",
            ValidationError::MissingTerminal,
        ),
        (
            "invalid_stream_post_terminal.jsonl",
            ValidationError::EventAfterTerminal,
        ),
        (
            "invalid_stream_malformed_terminal_payload.jsonl",
            ValidationError::InvalidPayload("run.failed.data"),
        ),
    ];

    for (name, expected) in cases {
        assert_eq!(validate_stream(name), Err(expected), "fixture {name}");
    }
}

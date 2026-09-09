#![cfg(feature = "json")]

use pyre::server::context::{ContextMessage, ContextRequest};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixtures {
    valid_requests: Vec<Case>,
    invalid_requests: Vec<Case>,
    valid_messages: Vec<Case>,
    invalid_messages: Vec<Case>,
    raw_requests: Vec<RawCase>,
    raw_messages: Vec<RawCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    name: String,
    json: String,
    valid: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    value: Value,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!("fixtures/database-context.json")).unwrap()
}

fn roundtrip<T: DeserializeOwned + Serialize>(cases: Vec<Case>) {
    for case in cases {
        let decoded: T = serde_json::from_value(case.value.clone())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            case.value,
            "{}",
            case.name
        );
        let decoded: T = serde_json::from_str(&case.value.to_string())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            case.value,
            "{}",
            case.name
        );
    }
}

fn reject<T: DeserializeOwned>(cases: Vec<Case>) {
    for case in cases {
        assert!(
            serde_json::from_value::<T>(case.value.clone()).is_err(),
            "{}",
            case.name
        );
        assert!(
            serde_json::from_str::<T>(&case.value.to_string()).is_err(),
            "{}",
            case.name
        );
    }
}

#[test]
fn shared_wire_fixtures() {
    let fixtures = fixtures();
    roundtrip::<ContextRequest>(fixtures.valid_requests);
    roundtrip::<ContextMessage>(fixtures.valid_messages);
    reject::<ContextRequest>(fixtures.invalid_requests);
    reject::<ContextMessage>(fixtures.invalid_messages);
}

#[test]
fn shared_raw_json_fixtures() {
    fn check<T: DeserializeOwned>(cases: Vec<RawCase>) {
        for case in cases {
            // Match JSON.parse: materialize the JSON data model before validation.
            let result =
                serde_json::from_str::<Value>(&case.json).and_then(serde_json::from_value::<T>);
            assert_eq!(result.is_ok(), case.valid, "{}", case.name);
        }
    }
    let fixtures = fixtures();
    check::<ContextRequest>(fixtures.raw_requests);
    check::<ContextMessage>(fixtures.raw_messages);
}

#[test]
fn integer_json_numbers_accept_decimal_and_exponent_notation() {
    let request =
        serde_json::from_str::<ContextRequest>(r#"{"protocolVersion":1.0,"databaseId":"db"}"#)
            .unwrap();
    assert_eq!(request.protocol_version, 1);
    for number in ["1", "1.0", "1e0"] {
        let message = format!(
            r#"{{"type":"context","protocolVersion":1,"databaseId":"db","contextId":"ctx","schemaId":"s","cacheScope":"c","authorityRevision":"a","databaseEpoch":"e","session":{{}},"expiresAt":{number}}}"#
        );
        let decoded: ContextMessage = serde_json::from_str(&message).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["expiresAt"],
            json!(1)
        );
    }
}

#[test]
fn every_envelope_field_is_required_and_identifiers_are_nonblank_strings() {
    let fixtures = fixtures();
    for (is_request, cases) in [
        (true, fixtures.valid_requests),
        (false, fixtures.valid_messages),
    ] {
        for case in cases {
            for (field, value) in case.value.as_object().unwrap() {
                let mut missing = case.value.clone();
                missing.as_object_mut().unwrap().remove(field);
                let invalid = |value: Value| {
                    if is_request {
                        serde_json::from_value::<ContextRequest>(value).is_err()
                    } else {
                        serde_json::from_value::<ContextMessage>(value).is_err()
                    }
                };
                assert!(invalid(missing), "{}: missing {field}", case.name);
                if matches!(
                    field.as_str(),
                    "databaseId"
                        | "contextId"
                        | "schemaId"
                        | "cacheScope"
                        | "authorityRevision"
                        | "databaseEpoch"
                ) {
                    assert!(value.is_string());
                    for blank in [
                        json!(""),
                        json!(" \t\r\n"),
                        json!("\u{2003}\u{00a0}"),
                        Value::Null,
                        json!(7),
                    ] {
                        let mut changed = case.value.clone();
                        changed[field] = blank;
                        assert!(invalid(changed), "{}: invalid {field}", case.name);
                    }
                }
            }
        }
    }
}

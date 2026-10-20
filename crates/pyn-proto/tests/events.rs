use pyn_proto::{Event, EventData, KNOWN_EVENT_TYPES};
use serde_json::Value;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/events/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn every_known_type_matches_its_golden_body() {
    for name in KNOWN_EVENT_TYPES {
        let text = fixture(name);
        let event: Event = serde_json::from_str(&text).unwrap();
        assert_eq!(event.data.event_type(), name);
        assert!(!matches!(event.data, EventData::Other { .. }), "{name}");
        assert_eq!(event.version, 1);
        let again: Value = serde_json::to_value(&event).unwrap();
        let golden: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(again, golden, "{name}");
    }
}

#[test]
fn envelope_serializes_in_documented_order() {
    let event: Event = serde_json::from_str(&fixture("account.enabled")).unwrap();
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"id":"evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D","type":"account.enabled","created_at":"2026-03-04T05:06:07Z","server":"https://pyn.example.com","version":1,"data":{"actor":"root","user":"alice"}}"#
    );
}

#[test]
fn unknown_type_round_trips() {
    let text = fixture("owner.frozen");
    let event: Event = serde_json::from_str(&text).unwrap();
    assert_eq!(event.event_type, "owner.frozen");
    assert!(matches!(event.data, EventData::Other { .. }));
    let again: Value = serde_json::to_value(&event).unwrap();
    assert_eq!(again, serde_json::from_str::<Value>(&text).unwrap());
}

#[test]
fn known_type_ignores_added_fields() {
    let mut body: Value = serde_json::from_str(&fixture("account.enabled")).unwrap();
    body["data"]["later"] = Value::Bool(true);
    let event: Event = serde_json::from_value(body).unwrap();
    assert!(matches!(event.data, EventData::AccountEnabled(_)));
}

#[test]
fn known_type_with_a_bad_payload_is_an_error() {
    let mut body: Value = serde_json::from_str(&fixture("account.enabled")).unwrap();
    body["data"] = Value::Null;
    assert!(serde_json::from_value::<Event>(body).is_err());
}

#[test]
fn constructor_fills_type_and_version() {
    let event: Event = serde_json::from_str(&fixture("owner.limit_reached")).unwrap();
    let built = Event::new(
        event.id.clone(),
        event.created_at,
        event.server.clone(),
        event.data.clone(),
    );
    assert_eq!(built, event);
}

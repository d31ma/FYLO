use std::io::Cursor;

use fylo_machine::{
    ApplicationConfig, FrameLimits, NamespaceName, NamespaceSeparation, serve_application,
};
use serde_json::{Value, json};

fn frames(input: &str, namespace: Option<&str>) -> Vec<Value> {
    // These tests cover the namespace code itself, ahead of slice L1-5.
    NamespaceSeparation::enable_for_tests();
    let application = ApplicationConfig {
        namespace: namespace.map(|name| NamespaceName::parse(name).unwrap()),
        ..ApplicationConfig::default()
    };
    let mut reader = Cursor::new(input.as_bytes().to_vec());
    let mut output = Vec::new();
    serve_application(
        &mut reader,
        &mut output,
        None,
        FrameLimits::default(),
        application,
        false,
        true,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_handshake_reports_public_without_a_session_namespace() {
    let frame = &frames("{\"op\":\"handshake\"}\n", None)[0];
    assert_eq!(frame["result"]["storage"]["namespace"], json!("public"));
}

#[test]
fn the_handshake_reports_the_session_namespace() {
    let frame = &frames("{\"op\":\"handshake\"}\n", Some("slice-12"))[0];
    assert_eq!(frame["result"]["storage"]["namespace"], json!("slice-12"));
}

#[test]
fn a_valid_namespace_field_is_accepted() {
    let frame = &frames("{\"op\":\"handshake\",\"namespace\":\"billing\"}\n", None)[0];
    assert_eq!(frame["ok"], json!(true));
}

#[test]
fn a_bad_namespace_field_fails_the_frame_with_enamespace() {
    let input = "{\"op\":\"getDoc\",\"collection\":\"users\",\"id\":\"4UUB32VGUDW\",\"namespace\":\"Billing\"}\n";
    let frame = &frames(input, None)[0];
    assert_eq!(frame["ok"], json!(false));
    assert_eq!(frame["error"]["code"], json!("ENAMESPACE"));
    let message = frame["error"]["message"].as_str().unwrap();
    assert!(message.contains("`Billing`"), "{message}");
}

#[test]
fn a_namespace_field_that_is_not_a_string_is_a_bad_request() {
    let frame = &frames("{\"op\":\"handshake\",\"namespace\":7}\n", None)[0];
    assert_eq!(frame["error"]["code"], json!("EBADREQUEST"));
}

#[test]
fn the_session_keeps_answering_after_a_refused_namespace() {
    let input = "{\"op\":\"handshake\",\"namespace\":\"../up\"}\n{\"op\":\"handshake\"}\n";
    let frames = frames(input, None);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["error"]["code"], json!("ENAMESPACE"));
    assert_eq!(frames[1]["ok"], json!(true));
}

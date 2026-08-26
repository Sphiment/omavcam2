//! The walking skeleton's own tests: the socket protocol, socket activation,
//! and the harness proving it can see what the stubs were called with.

mod common;

use common::Fixture;
use serde_json::json;

#[test]
fn status_prints_the_state_and_exits_zero() {
    let f = Fixture::start();
    let out = f.cli(&["status"]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(out.status.success(), "status failed: {stdout}");
    assert!(stdout.contains("phone: none"), "{stdout}");
    assert!(stdout.contains("capture: none"), "{stdout}");
}

#[test]
fn a_second_manual_daemon_does_not_steal_the_live_socket() {
    let f = Fixture::start();

    let second = f
        .daemon_command(env!("CARGO_BIN_EXE_vcamd"))
        .arg("daemon")
        .output()
        .unwrap();

    assert_eq!(second.status.code(), Some(2));
    let mut client = f.connect();
    assert_eq!(client.request("status")["ok"], json!(true));
}

#[test]
fn extra_cli_arguments_are_rejected_instead_of_ignored() {
    let f = Fixture::start();

    let out = f.cli(&["status", "surprise"]);

    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("usage:"));
}

#[test]
fn status_starts_the_daemon_on_demand_via_socket_activation() {
    let f = Fixture::activated();
    // Nothing is running yet: the socket exists, the daemon does not.
    let out = f.cli(&["status"]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(
        out.status.success(),
        "status failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("phone: none"), "{stdout}");
    assert!(
        f.argv().iter().any(|line| line == "adb start-server"),
        "the activated daemon ran its startup probe, so it really started"
    );
}

#[test]
fn the_first_activated_status_includes_an_already_attached_phone() {
    let f = Fixture::activated_with_devices(&[("39281FDJH0031T", "device", Some("Pixel_7"))]);

    let out = f.cli(&["status"]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Pixel 7"), "{stdout}");
    assert!(stdout.contains("connected"), "{stdout}");
}

#[test]
fn the_harness_records_argv() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();
    client.request("refresh");

    assert!(
        f.argv().iter().any(|line| line == "adb start-server"),
        "expected an adb call, got {:?}",
        f.argv()
    );
}

#[test]
fn a_second_client_sees_the_same_state() {
    let f = Fixture::start();
    let mut first = f.connect();
    let mut second = f.connect();

    let (a, b) = (first.recv_state(), second.recv_state());
    assert_eq!(a["state"], b["state"]);
    assert_eq!(a["rev"], b["rev"]);
}

#[test]
fn every_client_is_pushed_the_whole_state_when_it_changes() {
    let f = Fixture::start();
    let mut watcher = f.connect();
    let mut actor = f.connect();
    let before = watcher.recv_state();
    actor.recv_state();
    assert_eq!(before["state"]["adb_ok"], json!(true));

    f.script_exit("adb", 1); // the world changes under the daemon
    actor.request("refresh");

    // The watcher asked for nothing and polls nothing, yet gets the new state.
    let after = watcher.recv_state();
    assert_eq!(after["state"]["adb_ok"], json!(false));
    assert!(
        after["rev"].as_u64().unwrap() > before["rev"].as_u64().unwrap(),
        "revision must increase: {before} then {after}"
    );
    let state = after["state"].as_object().expect("state is an object");
    assert!(state.contains_key("adb_ok"), "state is pushed whole");
    assert!(state.contains_key("connection"), "state is pushed whole");
    assert!(state.contains_key("capture"), "state is pushed whole");
    assert_eq!(after["v"], json!(4));
}

#[test]
fn a_response_names_the_revision_that_reflects_it() {
    let f = Fixture::start();
    let mut client = f.connect();
    let initial = client.recv_state();

    f.script_exit("adb", 1);
    let response = client.request("refresh");

    // The state carrying the request's effect is already in hand by the time
    // the response names its revision, so nothing has to be asked for twice.
    assert_eq!(response["rev"], client.last_state["rev"]);
    assert!(response["rev"].as_u64().unwrap() > initial["rev"].as_u64().unwrap());
    assert_eq!(client.last_state["state"]["adb_ok"], json!(false));
}

#[test]
fn a_request_that_changes_nothing_does_not_burn_a_revision() {
    let f = Fixture::start();
    let mut client = f.connect();
    let initial = client.recv_state();

    let response = client.request("refresh");

    assert_eq!(response["ok"], json!(true));
    assert_eq!(
        response["rev"], initial["rev"],
        "the revision counts changes, not requests"
    );
}

#[test]
fn reconnecting_after_a_daemon_restart_needs_no_resync() {
    let mut f = Fixture::start();
    let mut before = f.connect();
    before.recv_state();

    f.restart();

    // A fresh connection is handed the whole state unprompted; there is no
    // resync request to send.
    let mut after = f.connect();
    let state = after.recv_state();
    assert!(state["state"].is_object());
    assert!(state["state"]["capture"].is_null());
}

#[test]
fn an_unknown_protocol_version_is_rejected_clearly() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    client.send_raw(&json!({"v": 99, "id": "x", "kind": "status"}).to_string());
    let response = client.recv();

    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["id"], json!("x"), "the id still comes back");
    assert_eq!(response["error"]["code"], json!("unsupported_version"));
}

#[test]
fn an_unknown_request_is_an_error_not_a_hang() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    let response = client.request("teleport");
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("unknown_request"));
}

#[test]
fn a_message_past_the_bound_is_rejected() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    let huge = "x".repeat(200 * 1024);
    client.send_raw(&json!({"v": 4, "id": "x", "kind": huge}).to_string());

    let response = client.recv();
    assert_eq!(response["error"]["code"], json!("message_too_large"));
}

#[test]
fn a_newline_one_byte_past_the_bound_is_rejected() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();
    let prefix = r#"{"v":4,"id":"x","kind":""#;
    let suffix = r#""}"#;
    let raw = format!(
        "{prefix}{}{suffix}",
        "x".repeat(64 * 1024 - prefix.len() - suffix.len())
    );
    assert_eq!(raw.len(), 64 * 1024);

    client.send_raw(&raw);

    let response = client.recv();
    assert_eq!(response["error"]["code"], json!("message_too_large"));
}

#[test]
fn the_cli_exit_code_reflects_a_failed_request() {
    let f = Fixture::start();
    f.script_exit("adb", 1);

    let out = f.cli(&["refresh"]);
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("adb_unavailable"), "{stderr}");
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("adb: unavailable"),
        "the state that reflects the failed request is still printed"
    );
}

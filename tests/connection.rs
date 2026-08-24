//! Getting a phone attached over USB: what adb reports, which phone is
//! selected, and the rule that omavcam never guesses between two of them.

mod common;

use common::Fixture;
use serde_json::json;
use std::time::{Duration, Instant};

/// A phone that is used, and one charging off the same laptop.
const PIXEL: &str = "39281FDJH0031T";
const GALAXY: &str = "R5CT10ABCDE";

fn is(state: &str) -> impl Fn(&serde_json::Value) -> bool + '_ {
    move |s: &serde_json::Value| s["connection"]["state"] == json!(state)
}

#[test]
fn one_attached_phone_is_selected_automatically_and_named() {
    let f = Fixture::start();
    let mut client = f.connect();

    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);

    let state = client.await_state("the phone to connect", is("connected"));
    assert_eq!(state["connection"]["phone"]["serial"], json!(PIXEL));
    assert_eq!(state["connection"]["phone"]["name"], json!("Pixel 7"));

    let stdout = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(
        stdout.contains("Pixel 7"),
        "status names the phone: {stdout}"
    );
}

#[test]
fn an_unaccepted_debugging_prompt_is_not_the_same_as_no_phone() {
    let f = Fixture::start();
    let mut client = f.connect();

    // adb reports an unauthorised phone with no model: it will not answer.
    f.script_devices(&[(PIXEL, "unauthorized", None)]);

    let state = client.await_state("the phone to report unauthorised", is("unauthorised"));
    assert_eq!(state["connection"]["phone"]["serial"], json!(PIXEL));

    let stdout = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(stdout.contains("unauthorised"), "{stdout}");
    assert!(
        stdout.contains("debugging prompt"),
        "the advice is to look at the phone, not the cable: {stdout}"
    );
}

#[test]
fn two_attached_phones_are_not_guessed_between() {
    let f = Fixture::start();
    let mut client = f.connect();

    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);

    let state = client.await_state("both phones to be offered", is("unselected"));
    let available = state["connection"]["available"].as_array().unwrap().clone();
    let serials: Vec<&str> = available
        .iter()
        .map(|p| p["serial"].as_str().unwrap())
        .collect();
    assert_eq!(serials, vec![PIXEL, GALAXY]);

    let stdout = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(
        stdout.contains(PIXEL) && stdout.contains(GALAXY),
        "{stdout}"
    );
}

#[test]
fn an_unauthorised_second_phone_is_never_selected_over_an_authorised_one() {
    let f = Fixture::start();
    let mut client = f.connect();

    // The case that drives all of this: anything taking the first entry from
    // `adb devices` points the webcam at the phone that is merely charging.
    f.script_devices(&[
        (GALAXY, "unauthorized", None),
        (PIXEL, "device", Some("Pixel_7")),
    ]);

    client.await_state("neither phone to be picked", is("unselected"));
    assert!(
        !f.argv().iter().any(|line| line.contains(" -s ")),
        "no phone was connected to at all: {:?}",
        f.argv()
    );
}

#[test]
fn a_selected_phone_survives_a_daemon_restart() {
    let mut f = Fixture::start();
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    let mut client = f.connect();
    client.await_state("the choice to be offered", is("unselected"));

    let response = client.request_with("select", json!({"serial": GALAXY}));
    assert_eq!(response["ok"], json!(true), "{response}");
    let state = client.await_state("the chosen phone to connect", is("connected"));
    assert_eq!(state["connection"]["phone"]["serial"], json!(GALAXY));

    f.restart();

    let mut after = f.connect();
    let state = after.await_state("the choice to be remembered", is("connected"));
    assert_eq!(
        state["connection"]["phone"]["serial"],
        json!(GALAXY),
        "the choice is remembered, not made again"
    );
}

#[test]
fn the_last_used_phone_is_reselected_when_it_reappears_among_several() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the lone phone to connect", is("connected"));

    // The one that was charging is plugged in too. Choosing was a one-time act.
    f.script_devices(&[
        (GALAXY, "device", Some("Galaxy_S21")),
        (PIXEL, "device", Some("Pixel_7")),
    ]);
    client.request("refresh");

    let state = client.state();
    assert_eq!(state["connection"]["state"], json!("connected"));
    assert_eq!(state["connection"]["phone"]["serial"], json!(PIXEL));
}

#[test]
fn the_selected_phone_vanishing_does_not_switch_to_another() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to connect", is("connected"));

    // Unplugged, and something else is on the desk. Silently repointing a
    // webcam at a different room is worse than asking — but saying "no phone"
    // with a phone on the desk hides it and offers nothing to click, so the
    // one that is here is offered as a choice instead (#23).
    f.script_devices(&[(GALAXY, "device", Some("Galaxy_S21"))]);

    let state = client.await_state("the phone that is here to be offered", is("unselected"));
    assert_eq!(
        state["connection"]["available"][0]["serial"],
        json!(GALAXY),
        "the phone on the desk is named rather than hidden"
    );
    assert!(
        !f.argv().iter().any(|line| line.contains(GALAXY)),
        "offered, but never touched until it is chosen: {:?}",
        f.argv()
    );
}

#[test]
fn unplugging_the_phone_returns_to_no_phone() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to connect", is("connected"));

    f.script_devices(&[]);

    // No restart, no request: the daemon notices by itself.
    client.await_state("no phone", is("no_phone"));
}

#[test]
fn an_offline_phone_is_not_left_connected() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to connect", is("connected"));

    f.script_devices(&[(PIXEL, "offline", Some("Pixel_7"))]);
    f.script_exit_for("adb", "get-state", 1);
    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("connecting"));
}

#[test]
fn every_adb_call_that_addresses_a_phone_names_its_serial() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to connect", is("connected"));

    let targeted: Vec<String> = f
        .argv()
        .into_iter()
        .filter_map(|line| line.strip_prefix("adb ").map(str::to_string))
        // `start-server` and `devices` are the two adb calls that have no
        // phone to name. Everything else names one.
        .filter(|args| !args.starts_with("start-server") && !args.starts_with("devices"))
        .collect();

    assert!(!targeted.is_empty(), "adb was asked about the phone at all");
    for args in &targeted {
        assert!(
            args.starts_with(&format!("-s {PIXEL} ")),
            "untargeted adb call: adb {args}"
        );
    }
}

#[test]
fn selecting_a_phone_that_is_not_attached_is_a_clear_error() {
    let f = Fixture::start();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("the phone to connect", is("connected"));

    let response = client.request_with("select", json!({"serial": "nosuchphone"}));

    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("no_such_phone"));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains(PIXEL),
        "the error says what is attached: {response}"
    );
}

#[test]
fn a_stalled_adb_scan_times_out_instead_of_wedging_the_daemon() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("adb");
    let started = Instant::now();

    let response = client.request_with("select", json!({"serial": PIXEL}));

    f.script_release("adb");
    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("adb_unavailable"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the command deadline did not bound the request"
    );
    assert_eq!(client.request("status")["ok"], json!(true));
}

#[test]
fn the_cli_can_select_a_phone() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    client.await_state("the choice to be offered", is("unselected"));

    let out = f.cli(&["select", GALAXY]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(out.status.success(), "select failed: {stdout}");
    assert!(
        stdout.contains("Galaxy S21"),
        "the state it printed reflects the choice: {stdout}"
    );
}

#[test]
fn select_with_no_serial_says_which_phones_are_attached() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to connect", is("connected"));

    // The way out of a dead end: a remembered phone that is unplugged reports
    // no phone rather than offering the one that is there, so this is where
    // its serial comes from.
    let out = f.cli(&["select"]);
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(PIXEL), "{stderr}");
}

/// The attached phones are a fact about the world, not a property of one
/// connection state: a client can only offer the choice at a moment omavcam is
/// not asking for one if it is told what is there even while a phone is
/// selected (#18).
#[test]
fn every_connection_state_carries_the_attached_phones() {
    let f = Fixture::start();
    let mut client = f.connect();

    // Reported by adb in the opposite order to the one they come back in: the
    // list is sorted by serial, so no client sees two phones swap places.
    f.script_devices(&[
        (GALAXY, "unauthorized", None),
        (PIXEL, "device", Some("Pixel_7")),
    ]);

    let state = client.await_state("both phones to be listed", |s| {
        s["attached"].as_array().is_some_and(|a| a.len() == 2)
    });
    assert_eq!(state["attached"][0]["phone"]["serial"], json!(PIXEL));
    assert_eq!(state["attached"][0]["authorised"], json!(true));
    // Listed, because a phone that needs one tap is not a phone that is
    // missing — but named as one that will not answer.
    assert_eq!(state["attached"][1]["phone"]["serial"], json!(GALAXY));
    assert_eq!(state["attached"][1]["authorised"], json!(false));

    // ...and it survives the phase changing under it.
    client.request_with("select", json!({"serial": PIXEL}));
    let state = client.await_state("the phone to connect", is("connected"));
    assert_eq!(
        state["attached"].as_array().map(Vec::len),
        Some(2),
        "the other phone is still on the desk: {state}"
    );
}

/// The state is compared whole to decide whether anything happened, so a list
/// whose order follows adb's would push identical state to every client and
/// spend a revision on it.
#[test]
fn adb_reordering_its_output_is_not_a_change() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    client.await_state("the choice to be offered", is("unselected"));
    let settled = client.request("status")["rev"].clone();

    f.script_devices(&[
        (GALAXY, "device", Some("Galaxy_S21")),
        (PIXEL, "device", Some("Pixel_7")),
    ]);
    std::thread::sleep(Duration::from_millis(200));

    assert_eq!(
        client.request("status")["rev"],
        settled,
        "the same phones in another order are the same phones"
    );
}

/// Choosing the phone that is already chosen changes nothing, and the panel
/// that offers a picker in every state is the thing most likely to ask.
#[test]
fn selecting_the_selected_phone_burns_no_revision() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    client.await_state("the choice to be offered", is("unselected"));

    let first = client.request_with("select", json!({"serial": PIXEL}));
    let again = client.request_with("select", json!({"serial": PIXEL}));

    assert_eq!(first["ok"], json!(true));
    assert_eq!(again["ok"], json!(true));
    assert_eq!(
        again["rev"], first["rev"],
        "the revision counts changes, not requests"
    );
}

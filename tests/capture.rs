//! Starting and stopping a capture: what scrcpy is launched with, which node it
//! writes to, and what the state says when it stops — asked for or not.

mod common;

use std::fs;

use common::{Client, Fixture};
use serde_json::{json, Value};

const PIXEL: &str = "39281FDJH0031T";
const GALAXY: &str = "R5CT10ABCDE";

fn running(state: &Value) -> bool {
    !state["capture"].is_null()
}

fn stopped(state: &Value) -> bool {
    state["capture"].is_null()
}

/// A daemon with one phone connected and a scrcpy that stays up once launched.
fn ready() -> (Fixture, Client) {
    let f = Fixture::start();
    f.script_hold("scrcpy");
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("the phone to connect", |s| {
        s["connection"]["state"] == json!("connected")
    });
    (f, client)
}

#[test]
fn start_launches_a_capture_against_the_selected_phone() {
    let (f, mut client) = ready();

    let response = client.request("start");
    assert_eq!(response["ok"], json!(true), "{response}");

    let state = client.await_state("the capture to be running", running);
    assert_eq!(state["capture"]["phone"]["serial"], json!(PIXEL));
    assert_eq!(state["capture"]["node"], json!("/dev/video42"));

    let calls = f.await_argv("scrcpy", 1);
    assert_eq!(calls.len(), 1, "one capture, one scrcpy: {calls:?}");
    for arg in [
        &format!("-s {PIXEL}"),
        "--video-source=camera",
        "--v4l2-sink=/dev/video42",
    ] {
        assert!(
            calls[0].contains(arg),
            "scrcpy was not given {arg}: {calls:?}"
        );
    }
}

#[test]
fn the_virtual_camera_is_found_by_its_card_label() {
    let (f, mut client) = ready();

    // `video_nr` is a request, not a guarantee: another device can hold the
    // number, and the label is the only thing that identifies the node.
    f.script_virtual_camera(Some("video7"));

    assert_eq!(client.request("start")["ok"], json!(true));
    let state = client.await_state("the capture to be running", running);
    assert_eq!(state["capture"]["node"], json!("/dev/video7"));
    let calls = f.await_argv("scrcpy", 1);
    assert!(calls[0].contains("--v4l2-sink=/dev/video7"), "{calls:?}");
}

#[test]
fn no_video_node_path_is_hardcoded() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("/dev/video"),
            "{} names a video node; the node is found by its card_label",
            path.display()
        );
    }
}

#[test]
fn the_virtual_cameras_controls_are_set_before_anything_writes_to_it() {
    let (f, mut client) = ready();

    client.request("start");
    client.await_state("the capture to be running", running);

    f.await_argv("scrcpy", 1);
    let argv = f.argv();
    let ctl = argv
        .iter()
        .position(|line| line.starts_with("v4l2-ctl "))
        .unwrap_or_else(|| panic!("the controls were never set: {argv:?}"));
    let scrcpy = argv
        .iter()
        .position(|line| line.starts_with("scrcpy "))
        .unwrap();
    assert!(ctl < scrcpy, "the controls are set first: {argv:?}");
    assert!(argv[ctl].contains("/dev/video42"), "{}", argv[ctl]);
    for control in ["keep_format", "sustain_framerate", "timeout"] {
        assert!(
            argv[ctl].contains(control),
            "{control} unset: {}",
            argv[ctl]
        );
    }
}

#[test]
fn starting_with_no_phone_selected_is_refused_by_name() {
    let f = Fixture::start();
    f.script_hold("scrcpy");
    // Two phones attached, so nothing is selected: omavcam never guesses.
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    let mut client = f.connect();
    client.await_state("the choice to be offered", |s| {
        s["connection"]["state"] == json!("unselected")
    });

    let response = client.request("start");

    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("no_phone"));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("select"),
        "the error says what to do next: {response}"
    );
    assert!(client.state()["capture"].is_null());
    assert!(
        !f.argv().iter().any(|line| line.starts_with("scrcpy ")),
        "nothing was launched: {:?}",
        f.argv()
    );

    let out = f.cli(&["start"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no_phone"), "{stderr}");
    assert!(!stderr.contains("panicked"), "not an error dump: {stderr}");
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("capture: none"),
        "the state that refused it is still printed"
    );
}

#[test]
fn stop_ends_the_capture() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);

    let response = client.request("stop");

    assert_eq!(response["ok"], json!(true), "{response}");
    client.await_state("the capture to be stopped", stopped);
    // Releasing the hold cannot revive it: the process is already gone.
    f.script_release("scrcpy");
    assert!(client.state()["capture"].is_null());
}

#[test]
fn stopping_a_capture_that_is_not_running_is_a_no_op() {
    let (_f, mut client) = ready();

    let response = client.request("stop");

    assert_eq!(response["ok"], json!(true), "{response}");
    assert!(client.state()["capture"].is_null());
}

#[test]
fn starting_a_capture_that_is_already_running_leaves_it_alone() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);

    let response = client.request("start");

    assert_eq!(response["ok"], json!(true), "{response}");
    assert!(!client.state()["capture"].is_null());
    assert_eq!(
        f.await_argv("scrcpy", 1).len(),
        1,
        "the running capture was not replaced: {:?}",
        f.argv()
    );
}

#[test]
fn a_capture_dying_on_its_own_moves_the_state_to_stopped() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);

    // scrcpy exits without being asked to — the phone went, or the process was
    // killed. A switch that still claims to be on is worse than no switch.
    f.script_release("scrcpy");

    client.await_state("the capture to be stopped", stopped);
    let stdout = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(stdout.contains("capture: none"), "{stdout}");
}

#[test]
fn a_restart_keeps_the_frame_size_it_started_with() {
    let (f, mut client) = ready();
    client.request("start");
    let first = client.await_state("the capture to be running", running)["capture"]["size"].clone();
    f.await_argv("scrcpy", 1);

    client.request("stop");
    client.await_state("the capture to be stopped", stopped);
    client.request("start");
    let again =
        client.await_state("the capture to be running again", running)["capture"]["size"].clone();

    assert_eq!(first, again, "the size is fixed for the capture's lifetime");
    let sizes: Vec<String> = f
        .await_argv("scrcpy", 2)
        .iter()
        .map(|call| {
            call.split_whitespace()
                .find_map(|arg| arg.strip_prefix("--camera-size="))
                .unwrap_or("unset")
                .to_string()
        })
        .collect();
    assert_eq!(sizes.len(), 2);
    assert_eq!(
        sizes[0], sizes[1],
        "a restart at a different size freezes whatever is watching (ADR-0010)"
    );
    assert_eq!(json!(sizes[0]), first, "the state names the size in use");
}

#[test]
fn a_missing_module_is_a_packaging_problem_and_not_a_crash() {
    let (f, mut client) = ready();
    f.script_virtual_camera(None);

    let response = client.request("start");

    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("no_virtual_camera"));
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("v4l2loopback"), "{message}");
    assert!(
        message.to_lowercase().contains("install"),
        "it names what to install: {message}"
    );
    // The daemon is still there and still answers.
    assert_eq!(client.request("status")["ok"], json!(true));
}

#[test]
fn the_daemon_never_loads_the_module_itself() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);
    f.script_virtual_camera(None);
    client.request("stop");
    client.request("start");

    assert!(
        !f.argv().iter().any(|line| line.starts_with("modprobe")),
        "a user daemon has no capabilities to load one with (ADR-0008): {:?}",
        f.argv()
    );
}

#[test]
fn the_cli_starts_and_stops_the_capture() {
    let (f, _client) = ready();

    let out = f.cli(&["start"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "start failed: {stdout}");
    assert!(stdout.contains("Pixel 7"), "{stdout}");
    assert!(stdout.contains("/dev/video42"), "{stdout}");

    let out = f.cli(&["stop"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "stop failed: {stdout}");
    assert!(stdout.contains("capture: none"), "{stdout}");
}

#[test]
fn starting_again_after_the_capture_died_launches_a_new_one() {
    // The daemon has not polled since, so nothing has reaped the dead scrcpy:
    // the request is what has to notice, or `start` answers ok and runs nothing.
    let f = Fixture::slow_poll();
    f.script_hold("scrcpy");
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.request("refresh");
    client.await_state("the phone to connect", |s| {
        s["connection"]["state"] == json!("connected")
    });

    client.request("start");
    client.await_state("the capture to be running", running);
    f.await_argv("scrcpy", 1);
    f.script_release("scrcpy");
    f.await_argv("released", 1);
    f.script_hold("scrcpy");

    let response = client.request("start");

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(
        f.await_argv("scrcpy", 2).len(),
        2,
        "the dead capture was replaced, not reported as running"
    );
    assert!(!client.state()["capture"].is_null());
}

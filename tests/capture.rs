//! Starting and stopping a capture: what scrcpy is launched with, which node it
//! writes to, and what the state says when it stops — asked for or not.

mod common;

use std::fs;
use std::sync::{Arc, Barrier};

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
        "--no-control",
        "--window-title=vcamd preview",
    ] {
        assert!(
            calls[0].contains(arg),
            "scrcpy was not given {arg}: {calls:?}"
        );
    }
    assert!(!calls[0].contains("--no-window"), "{calls:?}");
    assert_eq!(state["capture"]["preview"], json!(true));

    let argv = f.argv();
    let rule = argv
        .iter()
        .position(|line| line.starts_with("hyprctl eval "))
        .unwrap_or_else(|| panic!("the preview rule was never applied: {argv:?}"));
    let scrcpy = argv
        .iter()
        .position(|line| line.starts_with("scrcpy "))
        .unwrap();
    assert!(
        rule < scrcpy,
        "the rule must exist before the window maps: {argv:?}"
    );
    for setting in [
        "float = true",
        "pin = true",
        "no_initial_focus = true",
        "keep_aspect_ratio = true",
        "center = true",
        "no_close_for = 2147483647",
    ] {
        assert!(
            argv[rule].contains(setting),
            "{setting} missing: {}",
            argv[rule]
        );
    }
    // no_focus makes the window unpickable by the compositor, so Super+drag
    // goes inert and the preview can never be moved (ADR-0013).
    assert!(
        !argv[rule].contains("no_focus"),
        "no_focus is back; the preview is undraggable again: {}",
        argv[rule]
    );
    assert!(
        argv.iter().any(|line| {
            line.starts_with("hyprctl eval ") && line.contains("vcamd reconnecting")
        }),
        "the transient reconnect preview has no rule: {argv:?}",
    );
    assert!(!calls[0].contains("--window-x"), "{calls:?}");
    let probes = argv
        .iter()
        .filter(|line| line.starts_with("hyprctl clients "))
        .count();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        f.argv()
            .iter()
            .filter(|line| line.starts_with("hyprctl clients "))
            .count(),
        probes,
        "window discovery is a startup wait, not a permanent poll"
    );
}

#[test]
fn preview_hides_and_returns_without_disturbing_the_capture() {
    let (f, mut client) = ready();
    f.script_monitors(&[[-3840, 0, 3840, 2160], [0, 0, 1920, 1080]]);
    client.request_with("start", json!({"rounding": 8, "border_size": 2}));
    client.await_state("the preview to be visible", |state| {
        state["capture"]["preview"] == json!(true)
    });
    f.await_argv("scrcpy", 1);

    let response = client.request_with(
        "preview",
        json!({"visible": false, "rounding": 8, "border_size": 2}),
    );
    assert_eq!(response["ok"], json!(true), "{response}");
    let hidden = client.await_state("the preview to be hidden", |state| {
        state["capture"]["preview"] == json!(false)
    });
    assert!(!hidden["capture"].is_null(), "hiding is not stopping");
    assert!(
        f.argv()
            .iter()
            .any(|line| { line.contains("hl.dsp.window.move") && line.contains("x = -4481") }),
        "the preview was not moved off-screen: {:?}",
        f.argv()
    );

    let response = client.request_with(
        "preview",
        json!({"visible": true, "rounding": 8, "border_size": 2}),
    );
    assert_eq!(response["ok"], json!(true), "{response}");
    client.await_state("the preview to return", |state| {
        state["capture"]["preview"] == json!(true)
    });
    assert_eq!(
        f.await_argv("scrcpy", 1).len(),
        1,
        "showing the same window must not replace the capture"
    );
    let calls = f.argv();
    assert!(
        calls.iter().any(|line| {
            line.contains("hl.dsp.window.move")
                && line.contains("x = 120")
                && line.contains("y = 80")
        }),
        "the preview did not return to its saved position: {calls:?}"
    );
    let themed = calls
        .iter()
        .rev()
        .find(|line| line.starts_with("hyprctl eval "))
        .unwrap();
    assert!(themed.contains("rounding = 8"), "{themed}");
    assert!(themed.contains("border_size = 2"), "{themed}");
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
fn duplicate_public_labels_are_a_packaging_error_not_a_guess() {
    let (f, mut client) = ready();
    f.script_virtual_cameras(&["video7", "video42"]);

    let response = client.request("start");

    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("no_virtual_camera"));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("more than one"),
        "{response}"
    );
    assert!(
        !f.argv().iter().any(|call| call.starts_with("scrcpy ")),
        "no arbitrary node was chosen: {:?}",
        f.argv()
    );
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
    for control in ["keep_format=0", "sustain_framerate=1", "timeout=0"] {
        assert!(
            argv[ctl].contains(control),
            "{control} unset: {}",
            argv[ctl]
        );
    }
}

#[test]
fn an_immediate_scrcpy_failure_is_a_failed_start() {
    let f = Fixture::start();
    f.script_exit("scrcpy", 2);
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("the phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });

    let response = client.request("start");

    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("capture_failed"));
    assert!(client.state()["capture"].is_null());
}

#[test]
fn starting_with_no_phone_selected_is_refused_by_name() {
    let f = Fixture::start();
    f.script_hold("scrcpy");
    // Two phones attached, so nothing is selected: vcamd never guesses.
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
fn simultaneous_start_requests_launch_only_one_capture() {
    let (f, mut first) = ready();
    let mut second = f.connect();
    let barrier = Arc::new(Barrier::new(2));

    let (a, b) = std::thread::scope(|scope| {
        let first_barrier = Arc::clone(&barrier);
        let a = scope.spawn(move || {
            first_barrier.wait();
            first.request("start")
        });
        let second_barrier = Arc::clone(&barrier);
        let b = scope.spawn(move || {
            second_barrier.wait();
            second.request("start")
        });
        (a.join().unwrap(), b.join().unwrap())
    });

    assert_eq!(a["ok"], json!(true), "{a}");
    assert_eq!(b["ok"], json!(true), "{b}");
    let calls = f.await_argv("scrcpy", 1);
    assert_eq!(
        calls.len(),
        1,
        "one capture process owns the node: {calls:?}"
    );
}

#[test]
fn selecting_another_phone_stops_the_running_capture() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);

    let response = client.request_with("select", json!({"serial": GALAXY}));

    assert_eq!(response["ok"], json!(true), "{response}");
    assert!(client.state()["capture"].is_null());
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(GALAXY)
    );
}

#[test]
fn selecting_an_absent_remembered_wired_phone_does_not_stop_capture() {
    let (f, mut client) = ready();
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    assert_eq!(
        client.request_with("select", json!({"serial": GALAXY}))["ok"],
        json!(true)
    );
    assert_eq!(
        client.request_with("select", json!({"serial": PIXEL}))["ok"],
        json!(true)
    );
    client.request("start");
    client.await_state("the capture to run", running);
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);

    let response = client.request_with("select", json!({"serial": GALAXY}));

    assert_eq!(response["error"]["code"], json!("no_such_phone"));
    assert_eq!(client.state()["capture"]["phone"]["serial"], json!(PIXEL));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn a_capture_dying_on_its_own_is_restarted_without_dropping_the_logical_capture() {
    let (f, mut client) = ready();
    client.request("start");
    let before = client.await_state("the capture to be running", running);

    // Model a writer dying before adb's next device scan.
    f.script_preview_absent();
    f.script_release("scrcpy");
    client.await_state("the capture to reconnect", |state| {
        state["connection"]["state"] == json!("reconnecting")
    });
    assert_eq!(client.state()["capture"]["size"], before["capture"]["size"]);
    assert_eq!(client.state()["capture"]["preview"], json!(true));
    assert!(
        f.argv().iter().any(|line| {
            line.starts_with("hyprctl eval ")
                && line.contains("vcamd reconnecting")
                && line.contains("move = { 120, 80 }")
        }),
        "the reconnect status did not inherit the preview position: {:?}",
        f.argv()
    );

    f.script_hold("scrcpy");
    f.script_preview_window([120, 80]);
    client.await_state("the capture to resume", |state| {
        state["connection"]["state"] == json!("connected")
    });
    f.await_argv("scrcpy", 2);
    assert_eq!(client.state()["capture"]["preview"], json!(true));
    let argv = f.argv();
    let replacement = argv
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("scrcpy "))
        .nth(1)
        .unwrap()
        .0;
    let hidden = argv[..replacement]
        .iter()
        .rposition(|line| {
            line.contains("hl.dsp.window.move") && line.contains("vcamd reconnecting")
        })
        .expect("the reconnect status was not hidden before replacement");
    let hidden_rule = argv[..hidden]
        .iter()
        .rposition(|line| {
            line.starts_with("hyprctl eval ")
                && line.contains("vcamd reconnecting")
                && line.contains("move = { -641, 0 }")
        })
        .expect("the reconnect status had no persistent offscreen rule");
    let preview_rule = argv[..replacement]
        .iter()
        .rposition(|line| {
            line.starts_with("hyprctl eval ")
                && line.contains("vcamd-preview")
                && line.contains("move = { 120, 80 }")
        })
        .expect("the replacement preview had no saved-position rule");
    assert!(hidden_rule < hidden && hidden < preview_rule && preview_rule < replacement);
    assert!(
        argv[hidden + 1..replacement]
            .iter()
            .all(|line| !(line.starts_with("hyprctl eval ")
                && line.contains("vcamd reconnecting"))),
        "the hidden reconnect status was remapped before replacement: {argv:?}"
    );
}

#[test]
fn a_hidden_preview_maps_offscreen_when_the_writer_is_replaced() {
    let (f, mut client) = ready();
    client.request("start");
    client.request_with("preview", json!({"visible": false}));
    f.script_devices(&[]);
    client.request("refresh");
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.request("refresh");
    client.await_state("the hidden capture to resume", |state| {
        state["connection"]["state"] == json!("connected")
    });

    f.await_argv("scrcpy", 2);
    let argv = f.argv();
    let replacement = argv
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("scrcpy "))
        .nth(1)
        .unwrap()
        .0;
    assert!(
        argv[..replacement].iter().rev().any(|line| {
            line.starts_with("hyprctl eval ")
                && line.contains("vcamd-preview")
                && line.contains("move = { -641, 0 }")
        }),
        "the hidden replacement had no offscreen compositor rule: {argv:?}"
    );
    assert_eq!(client.state()["capture"]["preview"], json!(false));
}

#[test]
fn a_failed_replacement_never_claims_the_capture_is_connected() {
    let f = Fixture::slow_poll();
    f.script_hold("scrcpy");
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.request("refresh");
    client.await_state("the phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices(&[]);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("reconnecting"));

    f.script_release("scrcpy");
    f.script_exit("scrcpy", 2);
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let before = client.states.len();
    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("reconnecting"));
    assert!(
        client.states[before..]
            .iter()
            .all(|state| state["connection"]["state"] != json!("connected")),
        "a dead replacement was briefly advertised as connected: {:?}",
        &client.states[before..]
    );
}

#[test]
fn losing_and_returning_wired_phone_keeps_the_capture_and_applied_frame_size() {
    let (f, mut client) = ready();
    client.request("start");
    let before = client.await_state("the capture to be running", running);
    assert_eq!(before["capture"]["size"], json!("1280x720"));
    assert_eq!(
        client.request_with(
            "set",
            json!({"setting": "resolution", "value": "1920x1080"}),
        )["ok"],
        json!(true)
    );
    f.script_consumer(true);

    f.script_devices(&[(GALAXY, "device", Some("Galaxy_S21"))]);
    let lost = client.await_state("the selected phone to reconnect", |state| {
        state["connection"]["state"] == json!("reconnecting")
    });
    assert_eq!(lost["connection"]["phone"]["serial"], json!(PIXEL));
    assert_eq!(lost["capture"], before["capture"]);
    assert_eq!(lost["attached"][0]["phone"]["serial"], json!(GALAXY));

    f.script_devices(&[
        (GALAXY, "device", Some("Galaxy_S21")),
        (PIXEL, "device", Some("Pixel_7")),
    ]);
    let resumed = client.await_state("the same capture to resume", |state| {
        state["connection"]["state"] == json!("connected")
            && state["capture"]["phone"]["serial"] == json!(PIXEL)
    });
    assert_eq!(resumed["capture"]["size"], json!("1280x720"));
    let calls = f.await_argv("scrcpy", 2);
    assert!(calls[1].contains(&format!("-s {PIXEL}")), "{calls:?}");
    assert!(calls[1].contains("--camera-size=1280x720"), "{calls:?}");
    assert!(!calls.iter().any(|call| call.contains(GALAXY)), "{calls:?}");
}

#[test]
fn stop_while_reconnecting_cancels_the_restart_and_releases_the_node() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("the capture to be running", running);
    f.script_devices(&[]);
    client.await_state("the capture to reconnect", |state| {
        state["connection"]["state"] == json!("reconnecting")
    });

    assert_eq!(client.request("stop")["ok"], json!(true));
    assert!(client.state()["capture"].is_null());
    assert_ne!(client.state()["connection"]["state"], json!("reconnecting"));
    assert!(client.states.iter().all(|state| {
        !(state["capture"].is_null() && state["connection"]["state"] == json!("reconnecting"))
    }));
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    client.await_state("the phone to reconnect without capture", |state| {
        state["connection"]["state"] == json!("connected")
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn applied_preview_style_is_part_of_whole_state_and_revision() {
    let (_f, mut client) = ready();
    let started = client.request_with("start", json!({"rounding": 8, "border_size": 2}));
    assert_eq!(
        client.state()["preview_style"],
        json!({"rounding": 8, "border_size": 2})
    );

    let restyled = client.request_with(
        "preview",
        json!({"visible": true, "rounding": 9, "border_size": 3}),
    );
    assert!(restyled["rev"].as_u64() > started["rev"].as_u64());
    assert_eq!(
        client.state()["preview_style"],
        json!({"rounding": 9, "border_size": 3})
    );
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
    std::thread::sleep(std::time::Duration::from_millis(50));
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

#[test]
fn stay_awake_is_refused_because_the_preview_requires_control_off() {
    let (f, mut client) = ready();

    let response = client.request_with("start", json!({"stay_awake": true}));

    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"]["code"], json!("preview_conflict"));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--no-control"),
        "{response}"
    );
    assert!(client.state()["capture"].is_null());
    assert!(
        !f.argv().iter().any(|line| line.starts_with("scrcpy ")),
        "the unsafe capture was not launched: {:?}",
        f.argv()
    );

    let out = f.cli(&["start", "--stay-awake"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("preview_conflict"), "{stderr}");
    assert!(stderr.contains("--no-control"), "{stderr}");
}

#[test]
fn a_capture_that_never_asked_keeps_control_off() {
    let (f, mut client) = ready();

    client.request("start");
    client.await_state("the capture to be running", running);

    assert_eq!(client.state()["capture"]["stay_awake"], json!(false));
    let call = &f.await_argv("scrcpy", 1)[0];
    assert!(call.contains("--no-control"), "{call}");
    assert!(!call.contains("--stay-awake"), "{call}");
    assert!(!call.contains("--no-window"), "{call}");
}

#[test]
fn a_failed_start_offers_the_camera_owner_tip() {
    let f = Fixture::start();
    f.script_exit("scrcpy", 2);
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("the phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });

    let out = f.cli(&["start"]);
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("holding the camera"), "{stderr}");
    assert!(!stderr.contains("--stay-awake"), "{stderr}");
}

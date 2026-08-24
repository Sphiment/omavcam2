//! Camera settings through the daemon protocol: capabilities, pending Apply,
//! persistence, consumer safety, and rollback.

mod common;

use common::{Client, Fixture};
use serde_json::{json, Value};

const PIXEL: &str = "39281FDJH0031T";
const GALAXY: &str = "R5CT10ABCDE";

fn ready() -> (Fixture, Client) {
    let f = Fixture::start();
    f.script_hold("scrcpy");
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("camera settings", |state| {
        state["settings"]["phone"] == json!(PIXEL)
    });
    (f, client)
}

fn set(client: &mut Client, setting: &str, value: Value) -> Value {
    client.request_with("set", json!({"setting": setting, "value": value}))
}

#[test]
fn capabilities_are_parsed_and_aspect_ratio_filters_resolutions() {
    let (f, mut client) = ready();
    let settings = client.state()["settings"].clone();

    assert_eq!(settings["lenses"][0]["id"], json!("0"));
    assert_eq!(settings["lenses"][0]["facing"], json!("back"));
    assert_eq!(settings["lenses"][0]["sensor_size"], json!("4080x3060"));
    assert_eq!(
        settings["lenses"][0]["frame_rates"],
        json!([15, 20, 24, 30])
    );
    assert_eq!(settings["lenses"][0]["zoom_min"], json!(1.0));
    assert_eq!(settings["lenses"][0]["zoom_max"], json!(8.0));

    assert_eq!(
        set(&mut client, "aspect_ratio", json!("4:3"))["ok"],
        json!(true)
    );
    assert_eq!(
        client.state()["settings"]["offered_resolutions"],
        json!(["640x480"])
    );
    assert_eq!(
        set(&mut client, "resolution", json!("1280x720"))["error"]["code"],
        json!("invalid_setting")
    );
    assert_eq!(
        set(&mut client, "zoom", json!(8.1))["error"]["code"],
        json!("invalid_setting")
    );
    assert!(f.argv().iter().any(|line| line.starts_with("scrcpy-list ")));
}

#[test]
fn malformed_camera_dimensions_are_reported_without_killing_the_daemon() {
    let f = Fixture::start();
    f.script_camera_capabilities(
        "--camera-id=0 (back, 0x0, fps={30}, zoom-range=[1, 8])\n  - 0x0\n",
    );
    f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
    let mut client = f.connect();
    client.await_state("connected phone", |state| {
        state["connection"]["state"] == json!("connected")
    });

    assert!(client.state()["settings"].is_null());
    assert_eq!(client.request("status")["ok"], json!(true));
}

#[test]
fn malformed_camera_characteristics_are_rejected() {
    for capabilities in [
        "--camera-id=0 (, 1920x1080, fps={30}, zoom-range=[1, 8])\n  - 1280x720\n",
        "--camera-id=0 (back, 1920x1080, fps={0}, zoom-range=[1, 8])\n  - 1280x720\n",
        "--camera-id=0 (back, 1920x1080, fps={30}, zoom-range=[NaN, 8])\n  - 1280x720\n",
        "--camera-id=0 (back, 1920x1080, fps={30}, zoom-range=[8, 1])\n  - 1280x720\n",
        "--camera-id=0 (back, 1920x1080, fps={30}, zoom-range=[-1, 8])\n  - 1280x720\n",
        "--camera-id=0 (back, 1x1080, fps={30}, zoom-range=[1, 8])\n  - 1280x720\n",
        "--camera-id=0 (back, 1920x1080, fps={30}, zoom-range=[1, 8])\n  - 1x1\n",
    ] {
        let f = Fixture::start();
        f.script_camera_capabilities(capabilities);
        f.script_devices(&[(PIXEL, "device", Some("Pixel_7"))]);
        let mut client = f.connect();
        client.await_state("connected phone", |state| {
            state["connection"]["state"] == json!("connected")
        });
        assert!(client.state()["settings"].is_null(), "{capabilities}");
    }
}

#[test]
fn switching_phones_clears_stale_settings_when_the_new_probe_fails() {
    let (f, mut client) = ready();
    assert_eq!(client.state()["settings"]["phone"], json!(PIXEL));
    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    f.script_exit_for("scrcpy", "list", 1);

    assert_eq!(
        client.request_with("select", json!({"serial": GALAXY}))["ok"],
        json!(true)
    );
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(GALAXY)
    );
    assert!(client.state()["settings"].is_null());
}

#[test]
fn adb_failure_clears_idle_settings_but_preserves_the_reconnecting_phone() {
    let (f, mut client) = ready();
    f.script_exit_for("adb", "devices", 1);

    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("no_phone"));
    assert!(client.state()["settings"].is_null());

    f.script_exit_for("adb", "devices", 0);
    client.request("refresh");
    client.await_state("settings to return", |state| {
        state["settings"]["phone"] == json!(PIXEL)
    });
    assert_eq!(client.request("start")["ok"], json!(true));
    f.script_exit_for("adb", "start-server", 1);

    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("reconnecting"));
    assert_eq!(client.state()["settings"]["phone"], json!(PIXEL));
    assert_eq!(client.state()["capture"]["phone"]["serial"], json!(PIXEL));
}

#[test]
fn changes_are_pending_until_applied_and_discard_leaves_capture_alone() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);

    assert_eq!(set(&mut client, "zoom", json!(2.5))["ok"], json!(true));
    let settings = client.state()["settings"].clone();
    assert_eq!(settings["applied"]["zoom"], json!(1.0));
    assert_eq!(settings["pending"]["zoom"], json!(2.5));
    assert_eq!(settings["has_pending_changes"], json!(true));

    assert_eq!(client.request("discard")["ok"], json!(true));
    assert_eq!(client.state()["settings"]["pending"]["zoom"], json!(1.0));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn apply_uses_only_compatible_camera_flags_and_persists_per_phone() {
    let (mut f, mut client) = ready();
    for (name, value) in [
        ("lens", json!("1")),
        ("aspect_ratio", json!("16:9")),
        ("resolution", json!("1920x1080")),
        ("frame_rate", json!(24)),
        ("zoom", json!(3.0)),
    ] {
        assert_eq!(set(&mut client, name, value)["ok"], json!(true));
    }
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert!(
        client.state()["capture"].is_null(),
        "Apply does not launch a stopped capture"
    );

    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    let call = &f.await_argv("scrcpy", 1)[0];
    for argument in [
        "--camera-id=1",
        "--camera-size=1920x1080",
        "--camera-fps=24",
        "--camera-zoom=3",
    ] {
        assert!(call.contains(argument), "missing {argument}: {call}");
    }
    for forbidden in ["--camera-facing", "--camera-ar", "--max-size", " -m "] {
        assert!(!call.contains(forbidden), "forbidden {forbidden}: {call}");
    }

    client.request("stop");
    f.restart();
    let mut client = f.connect();
    client.await_state("restored settings", |state| {
        state["settings"]["applied"]["lens"] == json!("1")
    });
    assert_eq!(
        client.state()["settings"]["pending"],
        client.state()["settings"]["applied"]
    );

    f.script_devices(&[
        (PIXEL, "device", Some("Pixel_7")),
        (GALAXY, "device", Some("Galaxy_S21")),
    ]);
    assert_eq!(
        client.request_with("select", json!({"serial": GALAXY}))["ok"],
        json!(true)
    );
    client.await_state("the other phone's defaults", |state| {
        state["settings"]["phone"] == json!(GALAXY)
    });
    assert_eq!(client.state()["settings"]["applied"]["lens"], json!("0"));
    assert_eq!(
        client.request_with("select", json!({"serial": PIXEL}))["ok"],
        json!(true)
    );
    client.await_state("the first phone's settings", |state| {
        state["settings"]["phone"] == json!(PIXEL)
    });
    assert_eq!(client.state()["settings"]["applied"]["lens"], json!("1"));
}

#[test]
fn crop_is_normalized_clamped_and_even_after_a_resolution_change() {
    let (f, mut client) = ready();
    assert_eq!(
        set(
            &mut client,
            "crop",
            json!({"x": 0.73, "y": 0.0, "width": 0.4, "height": 0.501})
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(set(&mut client, "lens", json!("1"))["ok"], json!(true));
    assert!(client.state()["settings"]["pending"]["crops"]["1"].is_null());
    assert_eq!(set(&mut client, "lens", json!("0"))["ok"], json!(true));
    assert_eq!(
        client.state()["settings"]["pending"]["crops"]["0"],
        json!({"x": 0.73, "y": 0.0, "width": 0.4, "height": 0.501})
    );
    assert_eq!(
        set(&mut client, "resolution", json!("1920x1080"))["ok"],
        json!(true)
    );
    assert_eq!(client.request("apply")["ok"], json!(true));
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());

    let call = &f.await_argv("scrcpy", 1)[0];
    assert!(call.contains("--crop=520:540:1400:0"), "{call}");
    assert_eq!(client.state()["capture"]["size"], json!("520x540"));
    assert_eq!(
        client.state()["settings"]["applied"]["crops"]["0"],
        json!({"x": 0.73, "y": 0.0, "width": 0.4, "height": 0.501})
    );
}

#[test]
fn size_change_is_refused_for_a_consumer_but_live_safe_apply_restarts() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);
    f.script_consumer(true);

    set(&mut client, "resolution", json!("1920x1080"));
    let refused = client.request("apply");
    assert_eq!(refused["error"]["code"], json!("camera_in_use"));
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("close"));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1, "capture was untouched");

    client.request("discard");
    set(&mut client, "zoom", json!(2.0));
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(
        f.await_argv("scrcpy", 2).len(),
        2,
        "same-size capture restarted"
    );
    assert_eq!(client.state()["settings"]["applied"]["zoom"], json!(2.0));

    f.script_consumer(false);
    set(&mut client, "resolution", json!("1920x1080"));
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(
        f.await_argv("scrcpy", 3).len(),
        3,
        "size change is allowed with no consumer"
    );
    assert_eq!(client.state()["capture"]["size"], json!("1920x1080"));
}

#[test]
fn uncertain_consumer_inspection_refuses_a_size_change() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);
    f.script_uncertain_consumer(true);
    f.script_exit("v4l2-ctl", 2);
    set(&mut client, "resolution", json!("1920x1080"));

    let refused = client.request("apply");

    f.script_uncertain_consumer(false);
    f.script_exit("v4l2-ctl", 0);
    assert_eq!(refused["error"]["code"], json!("consumer_check_failed"));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn bounded_probe_can_prove_an_opaque_process_is_not_a_consumer() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);
    f.script_uncertain_consumer(true);
    set(&mut client, "resolution", json!("1920x1080"));

    assert_eq!(client.request("apply")["ok"], json!(true));

    f.script_uncertain_consumer(false);
    assert_eq!(f.await_argv("scrcpy", 2).len(), 2);
    assert_eq!(client.state()["capture"]["size"], json!("1920x1080"));
}

#[test]
fn an_opaque_consumer_probe_cannot_hang_apply() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);
    f.script_uncertain_consumer(true);
    f.script_hold("v4l2-ctl");
    set(&mut client, "resolution", json!("1920x1080"));

    let refused = client.request("apply");

    f.script_release("v4l2-ctl");
    f.script_uncertain_consumer(false);
    assert_eq!(refused["error"]["code"], json!("consumer_check_failed"));
    assert!(refused["error"]["message"]
        .as_str()
        .unwrap()
        .contains("did not finish within"));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn cli_status_enumerates_camera_capabilities() {
    let (f, _client) = ready();

    let output = String::from_utf8(f.cli(&["status"]).stdout).unwrap();

    for detail in [
        "lens 0 — back, sensor 4080x3060",
        "resolutions 1920x1080, 1280x720, 640x480",
        "fps 15, 20, 24, 30",
        "zoom [1, 8]",
        "offered resolutions: 1920x1080, 1280x720",
    ] {
        assert!(output.contains(detail), "missing {detail:?}: {output}");
    }
}

#[test]
fn cli_sets_applies_and_discards_settings() {
    let (f, mut client) = ready();

    let output = f.cli(&["set", "zoom", "2"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    client.await_state("pending CLI setting", |state| {
        state["settings"]["pending"]["zoom"] == json!(2.0)
    });
    assert!(String::from_utf8_lossy(&output.stdout).contains("pending Apply"));

    let output = f.cli(&["discard"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    client.await_state("discarded CLI setting", |state| {
        state["settings"]["pending"]["zoom"] == json!(1.0)
    });
}

#[test]
fn failed_apply_restarts_the_previous_capture_and_reports_the_rejection() {
    let (f, mut client) = ready();
    client.request("start");
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);
    f.script_fail_once("scrcpy", "--camera-zoom=2", 2);

    set(&mut client, "zoom", json!(2.0));
    let response = client.request("apply");

    assert_eq!(
        response["error"]["code"],
        json!("capture_failed"),
        "{response}"
    );
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("previous capture restarted"));
    let state = client.state();
    assert_eq!(state["settings"]["applied"]["zoom"], json!(1.0));
    assert_eq!(state["settings"]["pending"]["zoom"], json!(1.0));
    assert!(state["settings"]["rejected"]
        .as_str()
        .unwrap()
        .contains("zoom"));
    assert!(!state["capture"].is_null());
    let calls = f.await_argv("scrcpy", 3);
    assert!(calls[1].contains("--camera-zoom=2"), "{calls:?}");
    assert!(calls[2].contains("--camera-zoom=1"), "{calls:?}");
}

#[test]
fn apply_preserves_preview_visibility_and_position() {
    let (f, mut client) = ready();
    f.script_preview_window([333, 222]);
    client.request_with("start", json!({"rounding": 8, "border_size": 2}));
    client.await_state("capture", |state| !state["capture"].is_null());
    f.await_argv("scrcpy", 1);

    set(&mut client, "zoom", json!(2.0));
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(client.state()["capture"]["preview"], json!(true));
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
                && line.contains("omavcam-preview")
                && line.contains("move = { 333, 222 }")
        }),
        "Apply had no saved-position compositor rule: {argv:?}"
    );

    client.request_with(
        "preview",
        json!({"visible": false, "rounding": 8, "border_size": 2}),
    );
    set(&mut client, "zoom", json!(3.0));
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(client.state()["capture"]["preview"], json!(false));
    f.await_argv("scrcpy", 3);
    let argv = f.argv();
    let replacement = argv
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("scrcpy "))
        .nth(2)
        .unwrap()
        .0;
    assert!(
        argv[..replacement].iter().rev().any(|line| {
            line.starts_with("hyprctl eval ")
                && line.contains("omavcam-preview")
                && line.contains("move = { -641, 0 }")
        }),
        "Apply mapped the hidden preview without an offscreen compositor rule: {argv:?}"
    );

    client.request_with(
        "preview",
        json!({"visible": true, "rounding": 8, "border_size": 2}),
    );
    assert!(f.argv().iter().rev().any(|line| {
        line.contains("hl.dsp.window.move") && line.contains("x = 333") && line.contains("y = 222")
    }));
}

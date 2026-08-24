mod common;

use common::Fixture;
use serde_json::json;

const PAIR_ADDRESS: &str = "192.168.1.40:37123";
const CONNECT_ADDRESS: &str = "192.168.1.40:42877";
const NEW_CONNECT_ADDRESS: &str = "192.168.1.40:43991";
const CODE: &str = "123456";
const WIRED: &str = "R5CT10ABCDE";
const STABLE_ID: &str = "RF8M90ABCDEF";

#[test]
fn pairing_and_connecting_use_their_own_endpoints() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for(
        "adb",
        "connect",
        &format!("connected to {CONNECT_ADDRESS}\n"),
    );
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(
        client.state()["known"][0]["phone"]["name"],
        json!("Pixel 7")
    );
    let calls = f.argv();
    assert!(
        calls
            .iter()
            .any(|call| call == &format!("adb pair {PAIR_ADDRESS} {CODE}")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call == &format!("adb connect {CONNECT_ADDRESS}")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call == &format!("adb -s {CONNECT_ADDRESS} get-state")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("tcpip")),
        "{calls:?}"
    );
    for call in calls.iter().filter_map(|call| call.strip_prefix("adb ")) {
        if ["start-server", "devices", "pair", "connect", "disconnect"]
            .iter()
            .any(|global| call.starts_with(global))
        {
            continue;
        }
        assert!(
            call.starts_with(&format!("-s {CONNECT_ADDRESS} ")),
            "untargeted adb call: adb {call}"
        );
    }
}

#[test]
fn a_paired_phone_reconnects_after_a_daemon_restart_without_pairing_again() {
    let mut f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );

    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    f.restart();

    let mut after = f.connect();
    after.await_state("the paired phone to reconnect", |state| {
        state["connection"]["state"] == json!("connected")
    });
    let calls = f.argv();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("adb pair "))
            .count(),
        1,
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .filter(|call| call == &&format!("adb connect {CONNECT_ADDRESS}"))
            .count()
            >= 2,
        "{calls:?}"
    );
}

#[test]
fn a_running_wireless_capture_reconnects_without_pairing_or_switching_phones() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices(&[(WIRED, "device", Some("Galaxy_S21"))]);
    f.script_exit_for("adb", "connect", 1);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("reconnecting"));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );

    f.script_exit_for("adb", "connect", 0);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request("refresh");
    client.await_state("the wireless capture to resume", |state| {
        state["connection"]["state"] == json!("connected")
            && state["capture"]["phone"]["serial"] == json!(CONNECT_ADDRESS)
    });
    let calls = f.await_argv("scrcpy", 2);
    assert!(
        calls[1].contains(&format!("-s {CONNECT_ADDRESS}")),
        "{calls:?}"
    );
    assert_eq!(
        f.argv()
            .iter()
            .filter(|call| call.starts_with("adb pair "))
            .count(),
        1
    );
}

#[test]
fn an_offline_wireless_transport_is_actively_reconnected() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    client.request("start");
    let connects_before = f
        .argv()
        .iter()
        .filter(|call| call == &&format!("adb connect {CONNECT_ADDRESS}"))
        .count();
    f.script_devices(&[(CONNECT_ADDRESS, "offline", Some("Pixel_7"))]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);

    assert_eq!(client.request("refresh")["ok"], json!(true));

    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert!(!client.state()["capture"].is_null());
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
    let connects_after = f
        .argv()
        .iter()
        .filter(|call| call == &&format!("adb connect {CONNECT_ADDRESS}"))
        .count();
    assert!(connects_after > connects_before, "{:?}", f.argv());
}

#[test]
fn a_wireless_transport_that_remains_offline_is_unreachable() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    f.script_devices(&[(CONNECT_ADDRESS, "offline", Some("Pixel_7"))]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "offline", Some("Pixel_7"))]);

    assert_eq!(client.request("refresh")["ok"], json!(true));

    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
}

#[test]
fn learning_a_stable_wireless_id_keeps_the_durable_id_and_settings() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", "");
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    assert_eq!(
        client.request_with("set", json!({"setting": "zoom", "value": 2.0}))["ok"],
        json!(true)
    );
    assert_eq!(client.request("apply")["ok"], json!(true));
    assert_eq!(client.state()["known"][0]["id"], json!(CONNECT_ADDRESS));

    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(client.request("refresh")["ok"], json!(true));

    assert_eq!(client.state()["known"][0]["id"], json!(CONNECT_ADDRESS));
    assert_eq!(client.state()["known"][0]["hardware_id"], json!(STABLE_ID));
    assert_eq!(client.state()["settings"]["applied"]["zoom"], json!(2.0));
}

#[test]
fn a_wrong_pairing_code_is_named() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_devices(&[(WIRED, "device", Some("Galaxy_S21"))]);
    client.request("refresh");
    client.await_state("the wired phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });
    client.request("begin_pairing");
    f.script_output_for("adb", "pair", "Failed: Wrong password\n");
    f.script_exit_for("adb", "pair", 1);

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(response["error"]["code"], json!("wrong_code"), "{response}");
    assert_eq!(
        client.state()["connection"],
        json!({"state": "pairing_failed", "reason": "wrong_code"})
    );
    assert_eq!(
        client.state()["attached"][0]["phone"]["serial"],
        json!(WIRED)
    );
}

#[test]
fn a_wrong_pairing_address_is_named_without_running_malformed_adb() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": "not-an-endpoint",
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(
        response["error"]["code"],
        json!("wrong_pair_address"),
        "{response}"
    );
    assert_eq!(
        client.state()["connection"],
        json!({"state": "pairing_failed", "reason": "wrong_address"})
    );
    assert!(!f.argv().iter().any(|call| call.starts_with("adb pair ")));
}

#[test]
fn an_unreachable_pairing_host_names_the_network() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "failed to connect: Connection timed out\n");
    f.script_exit_for("adb", "pair", 1);

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(
        response["error"]["code"],
        json!("unreachable"),
        "{response}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("different network"),
        "{response}"
    );
    assert_eq!(
        client.state()["connection"],
        json!({"state": "pairing_failed", "reason": "unreachable"})
    );
}

#[test]
fn a_paired_phone_with_a_changed_port_is_unreachable_then_reconnected_without_pairing() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(
        client.request_with("set", json!({"setting": "zoom", "value": 2.0}))["ok"],
        json!(true)
    );
    assert_eq!(client.request("apply")["ok"], json!(true));

    f.script_devices(&[]);
    f.script_exit_for("adb", "connect", 1);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    let status = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(status.contains("same network"), "{status}");
    assert!(status.contains("do not pair again"), "{status}");
    let mut observer = f.connect();
    observer.recv_state();

    f.script_exit_for("adb", "connect", 0);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    observer.recv_state();
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(client.state()["settings"]["applied"]["zoom"], json!(2.0));
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(NEW_CONNECT_ADDRESS)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(client.state()["known"][0]["id"], json!(CONNECT_ADDRESS));
    assert_eq!(client.state()["known"][0]["hardware_id"], json!(STABLE_ID));
    assert_eq!(
        client.state()["known"][0]["phone"]["name"],
        json!("Pixel 7")
    );
    let calls = f.argv();
    assert!(
        calls
            .iter()
            .any(|call| call == &format!("adb connect {NEW_CONNECT_ADDRESS}")),
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("adb pair "))
            .count(),
        1,
        "{calls:?}"
    );
}

#[test]
fn the_current_connect_address_is_an_idempotent_no_op() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));
    let before_calls = f.argv();
    let before_rev = client.last_state["rev"].clone();
    f.script_exit_for("adb", "shell", 1);

    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(response["rev"], before_rev);
    assert_eq!(f.argv(), before_calls);
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
}

#[test]
fn a_rejected_new_address_is_disconnected_after_post_connect_failures() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    f.script_exit_for("adb", "devices", 1);
    let scan_failed = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );
    assert_eq!(scan_failed["error"]["code"], json!("adb_unavailable"));

    f.script_exit_for("adb", "devices", 0);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "offline", Some("Pixel_7"))]);
    let unusable = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );
    assert_eq!(unusable["error"]["code"], json!("unreachable"));

    assert_eq!(
        f.argv()
            .iter()
            .filter(|call| call == &&format!("adb disconnect {NEW_CONNECT_ADDRESS}"))
            .count(),
        2
    );
    assert_eq!(
        client.state()["known"][0]["connect_address"],
        json!(CONNECT_ADDRESS)
    );
    assert_eq!(client.state()["attached"], json!([]));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
}

#[test]
fn a_changed_wireless_port_retargets_the_same_logical_capture() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices(&[]);
    f.script_exit_for("adb", "connect", 1);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("reconnecting"));
    assert!(!client.state()["capture"].is_null());

    f.script_exit_for("adb", "connect", 0);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(NEW_CONNECT_ADDRESS)
    );
    assert_eq!(client.state()["capture"]["size"], json!("1280x720"));
    let calls = f.await_argv("scrcpy", 2);
    assert!(
        calls[1].contains(&format!("-s {NEW_CONNECT_ADDRESS}")),
        "{calls:?}"
    );
    assert_eq!(
        f.argv()
            .iter()
            .filter(|call| call.starts_with("adb pair "))
            .count(),
        1
    );
}

#[test]
fn a_changed_port_cannot_retarget_capture_to_a_different_phone_while_reconnecting() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    client.request("start");

    f.script_devices(&[]);
    f.script_exit_for("adb", "connect", 1);
    client.request("refresh");
    f.script_exit_for("adb", "connect", 0);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Galaxy_S21"))]);
    f.script_output_for("adb", "shell", "A_DIFFERENT_PHONE\n");

    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["error"]["code"], json!("wrong_phone"));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
    assert_eq!(
        client.state()["known"][0]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn wired_and_wireless_phones_share_the_registry_and_selection() {
    let mut f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    f.script_devices(&[
        (CONNECT_ADDRESS, "device", Some("Pixel_7")),
        (WIRED, "device", Some("Galaxy_S21")),
    ]);
    let response = client.request_with("select", json!({"serial": WIRED}));

    assert_eq!(response["ok"], json!(true), "{response}");
    let known = client.state()["known"].as_array().unwrap().clone();
    assert_eq!(known.len(), 2, "{known:?}");
    assert!(known.iter().any(|known| {
        known["phone"]["name"] == json!("Pixel 7") && known["transport"] == json!("wireless")
    }));
    assert!(known.iter().any(|known| {
        known["phone"]["name"] == json!("Galaxy S21") && known["transport"] == json!("wired")
    }));

    f.restart();
    let mut after = f.connect();
    let state = after.await_state("the wired selection to survive", |state| {
        state["connection"]["state"] == json!("connected")
    });
    assert_eq!(state["connection"]["phone"]["serial"], json!(WIRED));
    assert_eq!(state["known"].as_array().unwrap().len(), 2);
}

#[test]
fn the_same_phone_is_one_registry_entry_across_transports() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    client.request("refresh");
    client.await_state("the wired phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);

    client.request("begin_pairing");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    let state = client.state();
    let known = state["known"].as_array().unwrap();
    assert_eq!(known.len(), 1, "{known:?}");
    assert_eq!(known[0]["id"], json!(STABLE_ID));
    assert_eq!(known[0]["transport"], json!("wireless"));
    assert_eq!(known[0]["connect_address"], json!(CONNECT_ADDRESS));

    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with("select", json!({"serial": STABLE_ID}))["ok"],
        json!(true)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(
        client.state()["known"][0]["connect_address"],
        json!(CONNECT_ADDRESS)
    );

    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
}

#[test]
fn a_paired_phone_can_be_forgotten() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    assert_eq!(
        client.request_with("set", json!({"setting": "zoom", "value": 2.0}))["ok"],
        json!(true)
    );
    assert_eq!(client.request("apply")["ok"], json!(true));
    f.script_devices(&[]);

    let response = client.request_with("forget", json!({"serial": CONNECT_ADDRESS}));

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["known"], json!([]));
    assert_eq!(
        client.state()["connection"]["state"],
        json!("needs_pairing")
    );
    assert!(f
        .argv()
        .iter()
        .any(|call| call == &format!("adb disconnect {CONNECT_ADDRESS}")));

    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.state()["settings"]["applied"]["zoom"], json!(1.0));
}

#[test]
fn an_unreachable_known_wireless_phone_can_still_be_selected() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    f.script_devices(&[(WIRED, "device", Some("Galaxy_S21"))]);
    client.request_with("select", json!({"serial": WIRED}));
    f.script_exit_for("adb", "connect", 1);

    let response = client.request_with("select", json!({"serial": CONNECT_ADDRESS}));

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    assert_eq!(
        client.state()["connection"]["phone"]["name"],
        json!("Pixel 7")
    );
}

#[test]
fn the_cli_enters_pairing_and_accepts_both_endpoints() {
    let f = Fixture::slow_poll();
    let pairing = f.cli(&["pair"]);
    assert!(pairing.status.success());
    let guidance = String::from_utf8(pairing.stdout).unwrap();
    assert!(
        guidance.contains("Pair device with pairing code"),
        "{guidance}"
    );
    assert!(guidance.contains("different"), "{guidance}");

    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let paired = f.cli(&["pair", PAIR_ADDRESS, CODE, CONNECT_ADDRESS]);
    let stdout = String::from_utf8(paired.stdout).unwrap();

    assert!(paired.status.success(), "{stdout}");
    assert!(stdout.contains("Pixel 7"), "{stdout}");
    let listed = String::from_utf8(f.cli(&["phones"]).stdout).unwrap();
    assert!(listed.contains("Pixel 7"), "{listed}");
    assert!(listed.contains("wireless"), "{listed}");
}

#[test]
fn pairing_guidance_is_a_phase_not_a_polling_blip() {
    let f = Fixture::start();
    let mut client = f.connect();
    f.script_devices(&[(WIRED, "device", Some("Galaxy_S21"))]);
    client.await_state("the wired phone to connect", |state| {
        state["connection"]["state"] == json!("connected")
    });
    assert_eq!(client.request("begin_pairing")["ok"], json!(true));

    std::thread::sleep(std::time::Duration::from_millis(150));

    client.request("status");
    assert_eq!(
        client.state()["connection"]["state"],
        json!("needs_pairing")
    );
}

#[test]
fn connect_output_alone_does_not_make_an_absent_phone_connected() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_devices(&[]);

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(
        response["error"]["code"],
        json!("unreachable"),
        "{response}"
    );
    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
}

#[test]
fn a_failed_scan_after_connect_is_reported_as_adb_unavailable() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_exit_for("adb", "devices", 1);

    let response = client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );

    assert_eq!(response["error"]["code"], json!("adb_unavailable"));
    assert_eq!(client.state()["adb_ok"], json!(false));
    assert_eq!(client.state()["connection"]["state"], json!("no_phone"));
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(client.state()["known"][0]["id"], json!(CONNECT_ADDRESS));
}

#[test]
fn an_offline_known_wireless_phone_uses_transport_recovery() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    let connects_before = f
        .argv()
        .iter()
        .filter(|call| call == &&format!("adb connect {CONNECT_ADDRESS}"))
        .count();
    f.script_devices(&[(CONNECT_ADDRESS, "offline", Some("Pixel_7"))]);
    f.script_exit_for("adb", "connect", 1);

    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    assert!(
        f.argv()
            .iter()
            .filter(|call| call == &&format!("adb connect {CONNECT_ADDRESS}"))
            .count()
            > connects_before,
        "offline recovery never ran adb connect: {:?}",
        f.argv()
    );
}

#[test]
fn automatic_recovery_rejects_a_different_phone_at_the_saved_endpoint() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );

    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Galaxy_S21"))]);
    f.script_output_for("adb", "shell", "A_DIFFERENT_PHONE\n");
    client.request("refresh");
    // Even if adb still lists an endpoint after disconnect, the normal resolve
    // path must not bypass the identity check on its next pass.
    client.request("refresh");

    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    assert_eq!(client.state()["known"][0]["hardware_id"], json!(STABLE_ID));
    assert_eq!(
        client.state()["known"][0]["phone"]["name"],
        json!("Pixel 7")
    );
    assert!(f
        .argv()
        .iter()
        .any(|call| call == &format!("adb disconnect {CONNECT_ADDRESS}")));
}

#[test]
fn a_provisional_phone_id_survives_late_identity_and_port_discovery() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_exit_for("adb", "shell", 1);
    f.script_devices(&[]);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    let provisional = client.state()["known"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(provisional, CONNECT_ADDRESS);

    f.script_devices(&[]);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );
    assert_eq!(response["error"]["code"], json!("phone_identity_failed"));
    assert!(f
        .argv()
        .iter()
        .any(|call| call == &format!("adb disconnect {NEW_CONNECT_ADDRESS}")));
    assert_eq!(
        client.state()["known"][0]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );

    f.script_exit_for("adb", "shell", 0);
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["known"][0]["id"], json!(provisional));
    assert_eq!(client.state()["known"][0]["hardware_id"], json!(STABLE_ID));
    assert_eq!(
        client.state()["known"][0]["connect_address"],
        json!(NEW_CONNECT_ADDRESS)
    );

    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with("select", json!({"serial": STABLE_ID}))["ok"],
        json!(true)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(client.state()["known"][0]["id"], json!(provisional));
}

#[test]
fn late_identity_merges_a_provisional_wireless_and_existing_wired_record() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    client.request("refresh");
    client.await_state("the wired phone", |state| {
        state["connection"]["state"] == json!("connected")
    });
    client.request("begin_pairing");

    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_exit_for("adb", "shell", 1);
    f.script_devices_on_connect(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 2);

    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with("select", json!({"serial": STABLE_ID}))["ok"],
        json!(true)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 2);

    f.script_exit_for("adb", "shell", 0);
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(client.state()["known"][0]["id"], json!(STABLE_ID));
    assert_eq!(client.state()["known"][0]["hardware_id"], json!(STABLE_ID));
    assert_eq!(
        client.state()["known"][0]["phone"]["serial"],
        json!(NEW_CONNECT_ADDRESS)
    );
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(NEW_CONNECT_ADDRESS)
    );
}

#[test]
fn updating_an_unselected_phone_does_not_repoint_the_capture() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
    f.script_devices(&[
        (CONNECT_ADDRESS, "device", Some("Pixel_7")),
        (WIRED, "device", Some("Galaxy_S21")),
    ]);
    assert_eq!(
        client.request_with("select", json!({"serial": WIRED}))["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices_on_connect(&[
        (NEW_CONNECT_ADDRESS, "device", Some("Pixel_7")),
        (WIRED, "device", Some("Galaxy_S21")),
    ]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(WIRED)
    );
    assert_eq!(client.state()["capture"]["phone"]["serial"], json!(WIRED));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
    assert!(client.state()["known"]
        .as_array()
        .unwrap()
        .iter()
        .any(|known| {
            known["phone"]["serial"] == json!(NEW_CONNECT_ADDRESS)
                && known["hardware_id"] == json!(STABLE_ID)
        }));
}

#[test]
fn a_changed_port_cannot_retarget_capture_to_a_different_phone() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_hold("scrcpy");
    f.script_output_for("adb", "pair", "Successfully paired\n");
    f.script_output_for("adb", "connect", "connected\n");
    f.script_output_for("adb", "shell", &format!("{STABLE_ID}\n"));
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    assert_eq!(
        client.request_with(
            "pair",
            json!({
                "pair_address": PAIR_ADDRESS,
                "code": CODE,
                "connect_address": CONNECT_ADDRESS,
            }),
        )["ok"],
        json!(true)
    );
    assert_eq!(client.request("start")["ok"], json!(true));

    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Galaxy_S21"))]);
    f.script_output_for("adb", "shell", "A_DIFFERENT_PHONE\n");
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["error"]["code"], json!("wrong_phone"));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
    assert_eq!(
        client.state()["known"][0]["phone"]["serial"],
        json!(CONNECT_ADDRESS)
    );
    assert!(f
        .argv()
        .iter()
        .any(|call| call == &format!("adb disconnect {NEW_CONNECT_ADDRESS}")));
    assert_eq!(f.await_argv("scrcpy", 1).len(), 1);
}

#[test]
fn selecting_an_absent_known_wired_phone_does_not_stop_the_capture() {
    let f = Fixture::slow_poll();
    let mut client = f.connect();
    f.script_devices(&[
        (STABLE_ID, "device", Some("Pixel_7")),
        (WIRED, "device", Some("Galaxy_S21")),
    ]);
    assert_eq!(
        client.request_with("select", json!({"serial": WIRED}))["ok"],
        json!(true)
    );
    assert_eq!(
        client.request_with("select", json!({"serial": STABLE_ID}))["ok"],
        json!(true)
    );
    f.script_devices(&[(STABLE_ID, "device", Some("Pixel_7"))]);
    client.request("refresh");
    f.script_hold("scrcpy");
    assert_eq!(client.request("start")["ok"], json!(true));

    let output = f.cli(&["select", WIRED]);
    client.request("status");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        client.state()["capture"]["phone"]["serial"],
        json!(STABLE_ID)
    );
}

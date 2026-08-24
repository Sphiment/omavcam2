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
    f.script_exit_for("adb", "connect", 1);
    client.request("refresh");
    assert_eq!(client.state()["connection"]["state"], json!("unreachable"));
    let status = String::from_utf8(f.cli(&["status"]).stdout).unwrap();
    assert!(status.contains("same network"), "{status}");
    assert!(status.contains("do not pair again"), "{status}");

    f.script_exit_for("adb", "connect", 0);
    f.script_devices_on_connect(&[(NEW_CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    let response = client.request_with(
        "connect",
        json!({"serial": CONNECT_ADDRESS, "connect_address": NEW_CONNECT_ADDRESS}),
    );

    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(client.state()["connection"]["state"], json!("connected"));
    assert_eq!(
        client.state()["connection"]["phone"]["serial"],
        json!(NEW_CONNECT_ADDRESS)
    );
    assert_eq!(client.state()["known"].as_array().unwrap().len(), 1);
    assert_eq!(client.state()["known"][0]["id"], json!(STABLE_ID));
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
fn wired_and_wireless_phones_share_the_registry_and_selection() {
    let mut f = Fixture::slow_poll();
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
    f.script_devices(&[(CONNECT_ADDRESS, "device", Some("Pixel_7"))]);
    client.request_with(
        "pair",
        json!({
            "pair_address": PAIR_ADDRESS,
            "code": CODE,
            "connect_address": CONNECT_ADDRESS,
        }),
    );
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

//! The Omarchy plugin: it installs by being cloned, and it runs nothing.
//!
//! Both are structural promises rather than behaviour, so they are checked by
//! reading what the clone would contain. The QML itself is exercised by the
//! shell, which no test here can stand in for.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn manifest() -> Value {
    let text = fs::read_to_string(repo().join("manifest.json")).expect("manifest.json");
    serde_json::from_str(&text).expect("manifest.json is JSON")
}

/// Every file the plugin ships, with `//` comments stripped: this file's own
/// prose says what the plugin does not do, and prose must not read as doing it.
fn plugin_code() -> Vec<(PathBuf, String)> {
    fs::read_dir(repo().join("plugin"))
        .expect("plugin/")
        .map(|entry| entry.expect("plugin/ entry").path())
        .map(|path| {
            let text = fs::read_to_string(&path).expect("plugin file is text");
            let code = text
                .lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");
            (path, code)
        })
        .collect()
}

/// `omarchy plugin add` clones the repo and hands the folder to
/// `omarchy-plugin-validate`, which refuses an entry point it cannot find. A
/// missing file is the one way this plugin could fail to install, because
/// there is nothing to build.
#[test]
fn the_manifest_names_files_the_clone_has() {
    let manifest = manifest();
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["kinds"], serde_json::json!(["bar-widget"]));

    let entry_points = manifest["entryPoints"].as_object().expect("entryPoints");
    // The kind is a promise to supply something to load, and the shell looks
    // for it under a fixed key.
    let bar_widget = entry_points["barWidget"].as_str().expect("barWidget");
    for path in entry_points.values() {
        let path = path.as_str().expect("entry point is a path");
        assert!(
            !path.starts_with('/') && !path.contains(".."),
            "entry point must be a safe relative path: {path}"
        );
        assert!(
            repo().join(path).is_file(),
            "entry point is missing from the clone: {path}"
        );
        // Everything the shell loads has to live where the test below looks,
        // or a second entry point could ship code nothing checks.
        assert!(
            path.starts_with("plugin/"),
            "the plugin's files live under plugin/: {path}"
        );
    }
    assert!(
        bar_widget.ends_with(".qml"),
        "nothing to build: {bar_widget}"
    );
}

/// The rule the whole rewrite exists to establish: the plugin opens the
/// socket, renders what it is pushed, and sends requests. Every `scrcpy`,
/// `adb` and `hyprctl` invocation lives in the daemon, and `modprobe` is not
/// even the daemon's — the package loads the module at install time
/// (ADR-0008).
///
/// `Process` and `execDetached` are the only two ways QML can run anything, so
/// their absence is what makes an invocation impossible; the binary names are
/// listed as well to say so out loud. `adb` is not among them because the
/// widget renders the daemon's own `adb_ok`, and a word on screen is not a
/// command.
#[test]
fn the_plugin_asks_the_daemon_and_runs_nothing_itself() {
    let code = plugin_code();
    assert!(!code.is_empty(), "the plugin ships no files");
    for (path, code) in code {
        for forbidden in ["Process", "execDetached", "scrcpy", "modprobe", "hyprctl"] {
            assert!(
                !code.contains(forbidden),
                "{}: a client never runs {forbidden} — that belongs in the daemon",
                path.display()
            );
        }
    }
}

#[test]
fn the_panel_toggles_the_preview_with_the_omarchy_theme_tokens() {
    let panel = fs::read_to_string(repo().join("plugin/Panel.qml")).unwrap();

    assert!(panel.contains("label: \"Preview\""), "no preview control");
    assert!(
        panel.contains("Style.cornerRadius"),
        "rounding is not themed"
    );
    assert!(
        panel.contains("Style.normalBorderWidth"),
        "border width is not themed"
    );
    assert!(
        panel.contains("send(\"preview\""),
        "the panel never asks the daemon"
    );
    assert!(
        panel.contains("daemonState.preview_style"),
        "the panel does not compare its theme with the applied whole state"
    );
    assert!(
        panel.contains("if (message.ok) syncPreviewStyle()"),
        "a refused preview style would be retried forever"
    );
    assert!(
        panel.contains("known.transport === \"wireless\"")
            && panel.contains("item.phone.serial === known.hardware_id"),
        "an unplugged wired registry entry cannot be selected"
    );
    assert!(
        panel.contains("FloatingWindow")
            && panel.contains("title: \"omavcam reconnecting\"")
            && panel.contains("visible: root.reconnecting && root.previewing"),
        "the floating preview does not show reconnecting"
    );
    assert!(
        panel.contains("activeColor: root.reconnecting ? root.urgent"),
        "the reconnect warning uses the ordinary capture color"
    );
}

/// Phone names, serials and the tool errors quoted back in refusals are all
/// written by the device, and the shell's shared components render text as
/// `Text.AutoText` — Qt decides for itself whether a string is markup. So the
/// plugin must not hand a device's angle brackets on: a model name of
/// `<img src="http://evil/x">` would otherwise be a phone choosing what the bar
/// renders and what it fetches.
///
/// Two halves, and the first is the one that matters, because `PanelHero`,
/// `Toggle` and `Button` belong to the shell and cannot be told to stay plain.
#[test]
fn nothing_a_phone_wrote_can_become_markup() {
    for (path, code) in plugin_code() {
        let name = path.display();

        // The daemon's every word is sanitised as it is parsed.
        if code.contains("JSON.parse") {
            assert!(
                code.contains("plain(JSON.parse(line))"),
                "{name}: the daemon's state must be sanitised where it is parsed"
            );
            assert!(
                code.contains(r#"replace(/[<>]/g, "")"#),
                "{name}: sanitising is stripping the brackets Qt reads as markup"
            );
        }

        // And every Text this plugin owns says plainly that it is plain.
        let owned = code.matches("Text {").count();
        let pinned = code.matches("textFormat: Text.PlainText").count();
        assert_eq!(
            owned, pinned,
            "{name}: {owned} Text elements but {pinned} pinned to PlainText"
        );
    }
}

/// A fresh machine has no engine to talk to, and the widget is the first thing
/// on screen. It cannot install anything itself, so what it owes the user is
/// the command — a missing binary is a prompt, not a stack trace (#16).
#[test]
fn the_widget_offers_the_install_it_cannot_perform() {
    let panel = fs::read_to_string(repo().join("plugin/Panel.qml")).expect("Panel.qml");

    assert!(
        panel.contains("omavcam-git"),
        "an unanswered socket has to name the package that supplies the engine"
    );
    // The specific names come from the daemon, so the widget renders them
    // rather than keeping a list of tools that would drift from the daemon's.
    assert!(
        panel.contains("daemonState.missing"),
        "the missing dependencies the daemon names are what the panel offers"
    );
}

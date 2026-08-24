//! Packaging: what the AUR package installs, and what omavcam says when
//! something it needs is not installed.
//!
//! The package files are read rather than built — an `-git` PKGBUILD cannot be
//! run here without cloning and compiling — so what is checked is the promise
//! each file makes: the module is configured at install time (ADR-0008), every
//! node it creates is `exclusive_caps=1` (ADR-0012), and uninstalling leaves
//! nothing loaded.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::Fixture;
use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn packaging(file: &str) -> String {
    fs::read_to_string(repo().join("packaging").join(file))
        .unwrap_or_else(|e| panic!("packaging/{file}: {e}"))
}

/// The `options v4l2loopback ...` line, as `(parameter, value)` pairs. Values
/// keep their quotes, because a `card_label` holding a space depends on them
/// surviving into the line the kernel parses.
fn module_parameters() -> Vec<(String, String)> {
    let conf = packaging("omavcam.modprobe.conf");
    let line = conf
        .lines()
        .find(|line| line.starts_with("options v4l2loopback "))
        .expect("an options line for v4l2loopback");
    // Split the way the kernel's own parser does: on whitespace, except
    // inside quotes. Splitting naively is what a label with a space breaks.
    let mut quoted = false;
    line.trim_start_matches("options v4l2loopback ")
        .split(|c: char| {
            if c == '"' {
                quoted = !quoted;
            }
            c.is_whitespace() && !quoted
        })
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').expect("parameter=value");
            (name.to_string(), value.to_string())
        })
        .collect()
}

fn parameter(name: &str) -> String {
    module_parameters()
        .into_iter()
        .find(|(parameter, _)| parameter == name)
        .unwrap_or_else(|| panic!("the module line sets {name}"))
        .1
}

/// The whole engine: the binary both the daemon and the CLI are, the units
/// that make it appear on demand, and the two files that configure the module
/// the daemon has no capability to load itself.
#[test]
fn the_package_installs_the_engine_and_the_modules_configuration() {
    let pkgbuild = packaging("PKGBUILD");
    for installed in [
        "\"$pkgdir/usr/bin/omavcam\"",
        "\"$pkgdir/usr/lib/systemd/user/omavcam.socket\"",
        "\"$pkgdir/usr/lib/systemd/user/omavcam.service\"",
        "\"$pkgdir/usr/lib/modules-load.d/omavcam.conf\"",
        "\"$pkgdir/usr/lib/modprobe.d/omavcam.conf\"",
    ] {
        assert!(
            pkgbuild.contains(installed),
            "the package installs nothing at {installed}"
        );
    }
    // Connecting to the socket is what starts the daemon, so a socket nothing
    // enabled is an engine that never exists.
    assert!(
        pkgbuild.contains("sockets.target.wants/omavcam.socket"),
        "the socket has to be enabled, or the widget talks to nothing"
    );
    for dependency in ["scrcpy", "android-tools", "v4l-utils", "v4l2loopback-dkms"] {
        assert!(
            pkgbuild.contains(&format!("'{dependency}'")),
            "{dependency} is needed to run, so the package depends on it"
        );
    }
}

/// A node created with `exclusive_caps=0` advertises capture and output at
/// once: browsers refuse to list it and readers fail at `STREAMON`. Every node
/// this package creates has it, including the internal one (ADR-0012).
#[test]
fn every_node_is_created_with_exclusive_caps() {
    let nodes: usize = parameter("devices").parse().expect("devices is a count");
    assert!(
        nodes >= 2,
        "the uncropped capture needs a node of its own (ADR-0009)"
    );

    let exclusive: Vec<String> = parameter("exclusive_caps")
        .split(',')
        .map(str::to_string)
        .collect();
    assert_eq!(exclusive.len(), nodes, "one value per node: {exclusive:?}");
    assert!(
        exclusive.iter().all(|value| value == "1"),
        "exclusive_caps=1 is not optional on any node (ADR-0012): {exclusive:?}"
    );
}

/// The daemon finds its node by `card_label` and nothing else, so the label
/// the package writes and the label the code looks up are one fact in two
/// files.
#[test]
fn the_public_node_carries_the_label_the_daemon_looks_up() {
    let labels: Vec<String> = parameter("card_label")
        .split(',')
        .map(|label| label.trim_matches('"').to_string())
        .collect();
    let nodes: usize = parameter("devices").parse().unwrap();
    assert_eq!(labels.len(), nodes, "every node is labelled: {labels:?}");
    assert_eq!(
        labels[0], "omavcam",
        "the public node is the one applications pick"
    );
    assert!(
        labels[1].contains("studio"),
        "the second node says what it is for, because it cannot be hidden (ADR-0012): {:?}",
        labels[1]
    );
    // The kernel splits this value on `,` without honouring quotes, and only
    // strips them when the whole value begins with one. A label with a space
    // therefore cannot be written here at all: quoted, the quote characters
    // land in the label; unquoted, the space ends the parameter.
    for label in &labels {
        assert!(
            !label.contains(' ') && !label.contains('"'),
            "a label the kernel cannot parse back into what was meant: {label:?}"
        );
    }
}

/// The module is loaded from boot by a file, never by omavcam: a
/// `systemd --user` service has no capabilities at all (ADR-0008).
#[test]
fn the_module_is_loaded_at_install_time_and_by_nothing_in_the_code() {
    let modules_load = packaging("omavcam.modules-load.conf");
    assert!(
        modules_load
            .lines()
            .any(|line| line.trim() == "v4l2loopback"),
        "modules-load.d names the module: {modules_load}"
    );

    for source in fs::read_dir(repo().join("src")).expect("src/") {
        let path = source.expect("src entry").path();
        let code = fs::read_to_string(&path).expect("source is text");
        assert!(
            !code.contains("Command::new(\"modprobe\")"),
            "{}: the daemon cannot load a module, and never tries",
            path.display()
        );
    }
}

/// Removing the package removes the units with it; the module it loaded is
/// the one thing pacman cannot take out of a running kernel by itself.
#[test]
fn uninstalling_leaves_no_loaded_module_behind() {
    let install = packaging("omavcam.install");
    let post_remove = install
        .split_once("post_remove()")
        .expect("a post_remove hook")
        .1;
    assert!(
        post_remove.contains("modprobe -r v4l2loopback"),
        "post_remove unloads the module: {post_remove}"
    );
}

/// The install command in the README is a URL, and the file it names is
/// uploaded under a fixed name by the workflow. Nothing else keeps those two
/// spellings together, and a mismatch is a 404 for everyone installing.
#[test]
fn the_readme_installs_the_asset_the_workflow_publishes() {
    let asset = "omavcam-git-x86_64.pkg.tar.zst";
    let workflow = fs::read_to_string(repo().join(".github/workflows/package.yml"))
        .expect(".github/workflows/package.yml");
    let readme = fs::read_to_string(repo().join("README.md")).expect("README.md");

    assert!(
        workflow.contains(&format!(
            "gh release upload \"$GITHUB_REF_NAME\" \"out/{asset}\""
        )),
        "the workflow uploads {asset}"
    );
    assert!(
        readme.contains(&format!("releases/latest/download/{asset}")),
        "the README installs the asset the workflow publishes"
    );
}

/// A tool that is not installed is named, with the package that supplies it,
/// before anything has been asked to fail. This is what a client turns into an
/// offer to install.
#[test]
fn a_missing_tool_is_named_with_the_package_that_supplies_it() {
    let mut fixture = Fixture::new();
    fixture.script_missing("scrcpy");
    fixture.spawn();
    let mut client = fixture.connect();

    let missing = client.await_state("scrcpy to be reported missing", |state| {
        state["missing"]
            .as_array()
            .is_some_and(|missing| !missing.is_empty())
    })["missing"]
        .clone();

    assert_eq!(
        missing,
        json!([{"what": "scrcpy", "install": "sudo pacman -S --needed scrcpy"}]),
        "only the tool that is actually gone, and the command that gets it"
    );
}

/// The module not being loaded is the same kind of problem as a missing
/// package, and is reported the same way rather than waiting for a capture.
#[test]
fn a_missing_virtual_camera_is_an_install_and_says_so() {
    let fixture = Fixture::start();
    fixture.script_virtual_camera(None);
    let mut client = fixture.connect();
    client.await_state("the virtual camera to be reported missing", |state| {
        state["missing"]
            .as_array()
            .is_some_and(|missing| !missing.is_empty())
    });

    let out = fixture.cli(&["status"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("the virtual camera"), "{stdout}");
    assert!(
        stdout.contains("sudo pacman -S --needed v4l2loopback-dkms"),
        "the install is offered, not just the diagnosis: {stdout}"
    );
    // The usual cause is a module installed with omavcam and not loaded since,
    // which pacman alone does not fix.
    assert!(
        stdout.contains("modprobe v4l2loopback"),
        "the offered command loads the module too: {stdout}"
    );
}

/// The guard on the two above: a complete install reports nothing missing, or
/// every client would offer an install forever.
#[test]
fn a_complete_install_is_missing_nothing() {
    let fixture = Fixture::start();
    let mut client = fixture.connect();
    let state: Value = client.recv_state()["state"].clone();

    assert_eq!(state["missing"], json!([]), "{state}");
}

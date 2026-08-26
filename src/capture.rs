//! The capture: one scrcpy process reading the phone's camera and writing the
//! virtual camera. The daemon owns the process; this module knows where the
//! node is, how it must be configured, and what scrcpy is launched with.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::command;
use crate::protocol::Missing;
use crate::settings::{self, CameraSettings};

/// The AUR package that ships the daemon and the module configuration. Named
/// in every message about something being missing, because "install the
/// package" is not an instruction anyone can follow.
pub const PACKAGE: &str = "omavcam-git";

/// The `card_label` the package's `modprobe.d` file gives the virtual camera,
/// and the only thing that identifies it. `video_nr` is a request, not a
/// guarantee — another device can already hold the number (ADR-0008).
pub const CARD_LABEL: &str = "omavcam";

/// scrcpy's window is the preview. Its title is the stable selector shared by
/// every rule and compositor operation.
pub const PREVIEW_TITLE: &str = "omavcam preview";
const RECONNECT_TITLE: &str = "omavcam reconnecting";

const PREVIEW_SELECTOR: &str = "title:^(omavcam preview)$";
const RECONNECT_SELECTOR: &str = "title:^(omavcam reconnecting)$";
const PREVIEW_WIDTH: u32 = 640;

/// Where the kernel lists video devices. Overridable so tests can hand the
/// daemon a directory of their own.
fn v4l2_dir() -> PathBuf {
    std::env::var("OMAVCAM_V4L2_DIR")
        .unwrap_or_else(|_| "/sys/class/video4linux".into())
        .into()
}

/// The virtual camera's path, found by its label. The error is the whole
/// message a user sees: a missing module is a packaging failure, not something
/// the daemon can repair at runtime (ADR-0008).
pub fn find_node() -> Result<String, String> {
    let directory = v4l2_dir();
    let mut labelled: Vec<String> = fs::read_dir(&directory)
        .map_err(|e| {
            format!(
                "could not inspect {}: {e}. The v4l2loopback module is not loaded; \
                 install {PACKAGE}, which creates and labels its nodes at boot",
                directory.display()
            )
        })?
        .filter_map(Result::ok)
        .filter(|node| {
            fs::read_to_string(node.path().join("name")).is_ok_and(|name| name.trim() == CARD_LABEL)
        })
        .map(|node| node.file_name().to_string_lossy().into_owned())
        .collect();
    // Directory order is not defined, and the same node every time matters more
    // than which one it is.
    labelled.sort();

    match labelled.as_slice() {
        [node] => Ok(format!("/dev/{node}")),
        [] => Err(format!(
            "no video node is labelled {CARD_LABEL:?}: the v4l2loopback module is not loaded. \
             Install {PACKAGE}, which ships the modules-load.d and modprobe.d files that load \
             and label it — a user daemon cannot load a module itself."
        )),
        nodes => Err(format!(
            "more than one video node is labelled {CARD_LABEL:?} ({}); \
             repair the package's v4l2loopback configuration rather than guessing",
            nodes.join(", ")
        )),
    }
}

/// What a capture needs and this machine does not have. Each entry names the
/// tool the way the user knows it and the package that supplies it, so a
/// client can offer the install rather than waiting for a capture to fail
/// (#16). Nothing here is repairable at runtime: the tools are packages, and
/// the module is loaded at install time (ADR-0008).
pub fn missing() -> Vec<Missing> {
    let mut missing: Vec<Missing> = [
        ("adb", "android-tools"),
        ("scrcpy", "scrcpy"),
        ("v4l2-ctl", "v4l-utils"),
        // Not just the preview's: every capture applies the window rules
        // before scrcpy is launched, so a missing hyprctl is a failed start.
        ("hyprctl", "hyprland"),
    ]
    .into_iter()
    .filter(|(tool, _)| !on_path(tool))
    .map(|(what, package)| Missing {
        what: what.to_string(),
        install: format!("sudo pacman -S --needed {package}"),
    })
    .collect();
    // The node, not the module: a module present but unconfigured leaves the
    // same hole, and the label is the only thing that proves the package's
    // modprobe.d is in force.
    //
    // One command for both causes, because the daemon cannot tell them apart
    // and the user should not have to: `--needed` is a no-op when the module
    // is already installed, which is the usual case — it was installed with
    // omavcam and has not been loaded since.
    if find_node().is_err() {
        missing.push(Missing {
            what: "the virtual camera".to_string(),
            install: "sudo pacman -S --needed v4l2loopback-dkms && sudo modprobe v4l2loopback"
                .to_string(),
        });
    }
    missing
}

/// Whether PATH holds something executable by that name — the same lookup the
/// spawn would do, done before there is anything to lose by failing.
fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(tool))
                .is_ok_and(|file| file.is_file() && file.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Set the controls that decide what a watching application sees when frames
/// stop: let its open consumer pin the format, and repeat the last real frame
/// after the writer is gone (ADR-0010). Best effort — the capture works without
/// them, it just survives a restart less gracefully.
///
// ponytail: v4l2-ctl rather than the ioctls it wraps, which would mean libc and
// a hand-written v4l2_ext_control. These are per-device V4L2 controls, not
// module parameters, so an ordinary user can set them.
pub fn set_controls(node: &str) {
    // An open consumer pins the format by itself. Leaving keep_format at 1
    // would also pin it while nobody is watching, making #9's permitted size
    // changes silently fail forever — and it breaks the very first capture:
    // measured on hardware, `keep_format=1` set here pins the *idle* node's
    // 640x480 BGR4 default, scrcpy's 1280x720 never takes, and the node feeds
    // 640x480 while the state claims 720p. With it at 0 the same run gives
    // 1280x720 YU12.
    // `sustain_framerate` rereads the last queued frame. A non-zero `timeout`
    // would replace it with v4l2loopback's black timeout buffer instead.
    let controls = "keep_format=0,sustain_framerate=1,timeout=0";
    let mut process = Command::new("v4l2-ctl");
    process.args(["-d", node, "-c", controls]);
    match command::status(process) {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "omavcam: v4l2-ctl refused {node}'s controls ({status}); \
             the capture will run without them"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => eprintln!(
            "omavcam: could not run v4l2-ctl ({e}); install v4l-utils — \
             the capture will run without {node}'s controls"
        ),
        Err(e) => eprintln!(
            "omavcam: could not configure {node} with v4l2-ctl ({e}); \
             the capture will run without its controls"
        ),
    }
}

/// Launch one process that writes the virtual camera and draws its own preview.
/// Control is always off, or that window forwards input to the phone.
///
pub fn spawn(serial: &str, node: &str, settings: &CameraSettings) -> std::io::Result<Child> {
    if node_is_capture(node)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("{node} already has a producer"),
        ));
    }
    let output_size = settings::output_size(settings);
    let (width, height) = output_size
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .ok_or_else(|| std::io::Error::other(format!("invalid output size {output_size}")))?;
    let window_width = PREVIEW_WIDTH;
    let window_height = (window_width * height / width).max(1);
    let mut process = Command::new("scrcpy");
    process.args([
        "-s",
        serial,
        "--video-source=camera",
        &format!("--camera-id={}", settings.lens),
        &format!("--camera-size={}", settings.resolution),
        &format!("--camera-fps={}", settings.frame_rate),
        &format!("--camera-zoom={}", settings.zoom),
        &format!("--v4l2-sink={node}"),
        "--no-audio",
        "--no-control",
        &format!("--window-title={PREVIEW_TITLE}"),
        &format!("--window-width={window_width}"),
        &format!("--window-height={window_height}"),
    ]);
    if let Some((width, height, x, y)) = settings::crop_pixels(settings) {
        process.arg(format!("--crop={width}:{height}:{x}:{y}"));
    }
    let mut child = process.stdin(Stdio::null()).spawn()?;

    // `spawn` only proves that the executable existed. `exclusive_caps=1`
    // changes the node from output-only to capture-capable when scrcpy really
    // attaches, which is the observable point at which applications can use
    // it. Waiting for that avoids claiming Running during adb/server startup
    // and is the same distinction #9 needs to roll a failed Apply back.
    let startup_ms = std::env::var("OMAVCAM_STARTUP_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5000);
    let deadline = Instant::now() + Duration::from_millis(startup_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(std::io::Error::other(format!(
                    "scrcpy exited during startup with {status}"
                )))
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        match node_is_capture(node) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("scrcpy did not make {node} capture-capable within {startup_ms}ms"),
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }

    if let Err(e) = wait_for_preview() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

/// Apply before the window maps, so it never flashes tiled, misplaced, or
/// focused. Placement belongs to the compositor: SDL3 ignores client window
/// coordinates under native Wayland.
pub fn apply_preview_rule(
    rounding: u64,
    border_size: u64,
    position: Option<[i64; 2]>,
) -> std::io::Result<()> {
    let placement = position.map_or_else(
        || "center = true".to_string(),
        |[x, y]| format!("move = {{ {x}, {y} }}"),
    );
    apply_preview_rule_with(rounding, border_size, Some(&placement))
}

/// Reapply live theme values without moving an existing preview.
pub fn apply_preview_style(rounding: u64, border_size: u64) -> std::io::Result<()> {
    apply_preview_rule_with(rounding, border_size, None)
}

fn apply_preview_rule_with(
    rounding: u64,
    border_size: u64,
    placement: Option<&str>,
) -> std::io::Result<()> {
    // Hyprland's only close guard is measured in milliseconds and stored as a
    // signed int. Its maximum protects a continuously open preview for just
    // under 25 days; reapplying the live style rearms it.
    // ponytail: replace this ceiling if Hyprland adds an indefinite guard.
    let placement = placement.map_or(String::new(), |placement| format!("{placement}, "));
    let rule = format!(
        "o.window({{ title = \"^({PREVIEW_TITLE})$\" }}, \
         {{ name = \"omavcam-preview\", float = true, pin = true, no_dim = true, \
         no_initial_focus = true, keep_aspect_ratio = true, \
         {placement}no_close_for = 2147483647, rounding = {rounding}, border_size = {border_size}, \
         opacity = \"1 1\", tag = \"-default-opacity\" }})"
    );
    hyprctl(&["eval", &rule])
}

pub fn apply_reconnect_rule(
    rounding: u64,
    border_size: u64,
    position: Option<[i64; 2]>,
) -> std::io::Result<()> {
    let placement = position.map_or_else(
        || "center = true".to_string(),
        |[x, y]| format!("move = {{ {x}, {y} }}"),
    );
    let rule = format!(
        "o.window({{ title = \"^({RECONNECT_TITLE})$\" }}, \
         {{ name = \"omavcam-reconnecting\", float = true, pin = true, no_dim = true, \
         no_focus = true, no_initial_focus = true, keep_aspect_ratio = true, \
         {placement}, rounding = {rounding}, border_size = {border_size}, \
         opacity = \"1 1\", tag = \"-default-opacity\" }})"
    );
    hyprctl(&["eval", &rule])
}

pub fn preview_position() -> std::io::Result<[i64; 2]> {
    preview_client()?
        .get("at")
        .and_then(Value::as_array)
        .filter(|at| at.len() == 2)
        .and_then(|at| Some([at[0].as_i64()?, at[1].as_i64()?]))
        .ok_or_else(|| std::io::Error::other("Hyprland reported no preview position"))
}

pub fn move_preview(at: [i64; 2]) -> std::io::Result<()> {
    move_window(PREVIEW_SELECTOR, at)
}

fn move_window(selector: &str, at: [i64; 2]) -> std::io::Result<()> {
    hyprctl(&[
        "dispatch",
        &format!(
            "hl.dsp.window.move({{ window = \"{selector}\", x = {}, y = {} }})",
            at[0], at[1]
        ),
    ])
}

pub fn center_preview() -> std::io::Result<()> {
    center_window(PREVIEW_SELECTOR)
}

fn center_window(selector: &str) -> std::io::Result<()> {
    hyprctl(&[
        "dispatch",
        &format!("hl.dsp.window.center({{ window = \"{selector}\" }})"),
    ])
}

pub fn initial_hidden_position() -> std::io::Result<[i64; 2]> {
    hidden_position(i64::from(PREVIEW_WIDTH))
}

pub fn hide_reconnect_preview(at: [i64; 2]) -> std::io::Result<()> {
    move_window(RECONNECT_SELECTOR, at)
}

pub fn show_reconnect_preview(position: Option<[i64; 2]>) -> std::io::Result<()> {
    match position {
        Some(position) => move_window(RECONNECT_SELECTOR, position),
        None => center_window(RECONNECT_SELECTOR),
    }
}

/// Move the preview wholly left of every monitor. A fixed negative coordinate
/// can be visible on a monitor arranged to the left of the primary one.
pub fn hide_preview() -> std::io::Result<()> {
    let preview = preview_client()?;
    let width = preview["size"]
        .as_array()
        .and_then(|size| size.first())
        .and_then(Value::as_i64)
        .ok_or_else(|| std::io::Error::other("Hyprland reported no preview width"))?;
    move_preview(hidden_position(width)?)
}

fn hidden_position(width: i64) -> std::io::Result<[i64; 2]> {
    let mut process = Command::new("hyprctl");
    process.args(["monitors", "-j"]);
    let output = command::output(process)?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "hyprctl monitors failed ({})",
            output.status
        )));
    }
    let monitors: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let left = monitors
        .as_array()
        .and_then(|monitors| {
            monitors
                .iter()
                .filter_map(|monitor| monitor["x"].as_i64())
                .min()
        })
        .ok_or_else(|| std::io::Error::other("Hyprland reported no monitors"))?;
    Ok([left.saturating_sub(width).saturating_sub(1), 0])
}

fn wait_for_preview() -> std::io::Result<()> {
    let wait_ms = std::env::var("OMAVCAM_PREVIEW_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5000);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        match preview_client() {
            Ok(_) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("preview window did not appear within {wait_ms}ms: {e}"),
                ))
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn preview_client() -> std::io::Result<Value> {
    let mut process = Command::new("hyprctl");
    process.args(["clients", "-j"]);
    let output = command::output(process)?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "hyprctl clients failed ({})",
            output.status
        )));
    }
    let clients: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    clients
        .as_array()
        .and_then(|clients| {
            clients
                .iter()
                .find(|client| client["title"] == PREVIEW_TITLE)
        })
        .cloned()
        .ok_or_else(|| std::io::Error::other("preview window is not mapped"))
}

fn hyprctl(args: &[&str]) -> std::io::Result<()> {
    let mut process = Command::new("hyprctl");
    process.args(args);
    let status = command::status(process)?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "hyprctl {} failed ({status})",
            args.first().unwrap_or(&"")
        )))
    }
}

/// Whether another process has the virtual camera open. scrcpy itself is the
/// writer and is excluded; any remaining fd is an application that has pinned
/// the frame size (ADR-0010).
pub fn has_consumer(node: &str, writer_pid: u32) -> std::io::Result<bool> {
    // ponytail: a /proc scan on the rare size-changing Apply; track fd events
    // only if machines with thousands of processes make this measurable.
    let proc = PathBuf::from(std::env::var("OMAVCAM_PROC_DIR").unwrap_or_else(|_| "/proc".into()));
    let mut uncertain = false;
    for process in fs::read_dir(proc)? {
        let process = match process {
            Ok(process) => process,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        let Ok(pid) = process.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == writer_pid {
            continue;
        }
        // The packaged node is root:video, so consumers under another uid are
        // credible too. Scan every readable fd table and probe the device when
        // any table is opaque.
        let fds = match fs::read_dir(process.path().join("fd")) {
            Ok(fds) => fds,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        for fd in fds {
            let fd = match fd {
                Ok(fd) => fd,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    uncertain = true;
                    continue;
                }
            };
            match fs::read_link(fd.path()) {
                Ok(target) if target == std::path::Path::new(node) => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => uncertain = true,
            }
        }
    }
    if !uncertain {
        return Ok(false);
    }

    // Some same-user sandbox processes deliberately hide their fd tables.
    // Ask the device whether a second reader can stream so unrelated opaque
    // processes do not block Apply. The shared command deadline kills a
    // stalled probe; anything inconclusive remains an error, never "free".
    let mut process = Command::new("v4l2-ctl");
    process.args([
        "-d",
        node,
        "--stream-mmap",
        "--stream-count=1",
        "--stream-poll",
    ]);
    let output = command::output(process)?;
    if output.status.success() {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Device or resource busy") {
        return Ok(true);
    }
    Err(std::io::Error::other(format!(
        "could not inspect {node}'s consumers: v4l2-ctl stream probe failed ({}){}",
        output.status,
        if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        }
    )))
}

fn node_is_capture(node: &str) -> std::io::Result<bool> {
    let mut process = Command::new("v4l2-ctl");
    process.args(["-d", node, "-D"]);
    let output = command::output(process)?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "v4l2-ctl could not inspect {node} ({})",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).contains("Video Capture"))
}

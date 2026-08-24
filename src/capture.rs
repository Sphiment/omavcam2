//! The capture: one scrcpy process reading the phone's camera and writing the
//! virtual camera. The daemon owns the process; this module knows where the
//! node is, how it must be configured, and what scrcpy is launched with.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::command;
use crate::settings::{self, CameraSettings};

/// The `card_label` the package's `modprobe.d` file gives the virtual camera,
/// and the only thing that identifies it. `video_nr` is a request, not a
/// guarantee — another device can already hold the number (ADR-0008).
pub const CARD_LABEL: &str = "omavcam";

/// scrcpy's window is the preview. Its title is the stable selector shared by
/// every rule and compositor operation.
pub const PREVIEW_TITLE: &str = "omavcam preview";

const PREVIEW_SELECTOR: &str = "title:^(omavcam preview)$";
/// How long the node keeps delivering frames after its writer stops. Enough
/// that an application which times out on stalled input keeps its camera when
/// a capture is restarted.
const TIMEOUT_MS: u32 = 1000;

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
                 install the omavcam package, which creates and labels its nodes at boot",
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
             Install the omavcam package, which ships the modules-load.d and modprobe.d files \
             that load and label it — a user daemon cannot load a module itself."
        )),
        nodes => Err(format!(
            "more than one video node is labelled {CARD_LABEL:?} ({}); \
             repair the package's v4l2loopback configuration rather than guessing",
            nodes.join(", ")
        )),
    }
}

/// Set the controls that decide what a watching application sees when frames
/// stop: let its open consumer pin the format, and keep delivering frames after
/// the writer is gone (ADR-0010). Best effort — the capture works without them,
/// it just survives a restart less gracefully.
///
// ponytail: v4l2-ctl rather than the ioctls it wraps, which would mean libc and
// a hand-written v4l2_ext_control. These are per-device V4L2 controls, not
// module parameters, so an ordinary user can set them. `timeout_image_io` is
// left alone: it loads a still into the timeout buffer, and there is no still
// to load until the last-frame ticket.
pub fn set_controls(node: &str) {
    // An open consumer pins the format by itself. Leaving keep_format at 1
    // would also pin it while nobody is watching, making #9's permitted size
    // changes silently fail forever — and it breaks the very first capture:
    // measured on hardware, `keep_format=1` set here pins the *idle* node's
    // 640x480 BGR4 default, scrcpy's 1280x720 never takes, and the node feeds
    // 640x480 while the state claims 720p. With it at 0 the same run gives
    // 1280x720 YU12.
    let controls = format!("keep_format=0,sustain_framerate=1,timeout={TIMEOUT_MS}");
    let mut process = Command::new("v4l2-ctl");
    process.args(["-d", node, "-c", &controls]);
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
pub fn spawn(
    serial: &str,
    node: &str,
    settings: &CameraSettings,
    rounding: u64,
    border_size: u64,
) -> std::io::Result<Child> {
    if node_is_capture(node)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("{node} already has a producer"),
        ));
    }
    apply_preview_rule(rounding, border_size)?;
    let output_size = settings::output_size(settings);
    let (width, height) = output_size
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .ok_or_else(|| std::io::Error::other(format!("invalid output size {output_size}")))?;
    let window_width = 640;
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

    if let Err(e) = wait_for_preview().and_then(|()| center_preview()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

/// Apply before the window maps, so it never flashes tiled or takes focus.
/// Reapplying the named rule updates theme values on a window already open.
pub fn apply_preview_rule(rounding: u64, border_size: u64) -> std::io::Result<()> {
    // Hyprland's only close guard is measured in milliseconds and stored as a
    // signed int. Its maximum keeps the capture safe for just under 25 days.
    // ponytail: replace this with an indefinite compositor primitive if one is
    // added; owning an adb power-setting restore is a larger failure mode.
    let rule = format!(
        "o.window({{ title = \"^(omavcam preview)$\" }}, \
         {{ name = \"omavcam-preview\", float = true, pin = true, no_dim = true, \
         no_focus = true, no_initial_focus = true, keep_aspect_ratio = true, \
         no_close_for = 2147483647, rounding = {rounding}, border_size = {border_size}, \
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
    hyprctl(&[
        "dispatch",
        &format!(
            "hl.dsp.window.move({{ window = \"{PREVIEW_SELECTOR}\", x = {}, y = {} }})",
            at[0], at[1]
        ),
    ])
}

pub fn center_preview() -> std::io::Result<()> {
    hyprctl(&[
        "dispatch",
        &format!("hl.dsp.window.center({{ window = \"{PREVIEW_SELECTOR}\" }})"),
    ])
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
    let proc = std::env::var("OMAVCAM_PROC_DIR").unwrap_or_else(|_| "/proc".into());
    Ok(fs::read_dir(proc)?.filter_map(Result::ok).any(|process| {
        process
            .file_name()
            .to_string_lossy()
            .parse::<u32>()
            .ok()
            .is_some_and(|pid| {
                pid != writer_pid
                    && fs::read_dir(process.path().join("fd"))
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .any(|fd| {
                            fs::read_link(fd.path())
                                .is_ok_and(|target| target == std::path::Path::new(node))
                        })
            })
    }))
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

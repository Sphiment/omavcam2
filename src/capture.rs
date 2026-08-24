//! The capture: one scrcpy process reading the phone's camera and writing the
//! virtual camera. The daemon owns the process; this module knows where the
//! node is, how it must be configured, and what scrcpy is launched with.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// The `card_label` the package's `modprobe.d` file gives the virtual camera,
/// and the only thing that identifies it. `video_nr` is a request, not a
/// guarantee — another device can already hold the number (ADR-0008).
pub const CARD_LABEL: &str = "omavcam";

/// The frame size every capture is launched at. An application that opens the
/// node pins the format, so a writer arriving later at a different size
/// delivers nothing — silently, and forever (ADR-0010). One constant is what
/// makes every restart match.
///
// ponytail: a constant until #7 makes resolution a setting. Then the size the
// running capture recorded is what a restart has to reuse, not this.
pub const SIZE: &str = "1280x720";

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
    let mut labelled: Vec<String> = fs::read_dir(v4l2_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|node| {
            fs::read_to_string(node.path().join("name")).is_ok_and(|name| name.trim() == CARD_LABEL)
        })
        .map(|node| node.file_name().to_string_lossy().into_owned())
        .collect();
    // Directory order is not defined, and the same node every time matters more
    // than which one it is.
    labelled.sort();

    labelled
        .first()
        .map(|node| format!("/dev/{node}"))
        .ok_or_else(|| {
            format!(
                "no video node is labelled {CARD_LABEL:?}: the v4l2loopback module is not loaded. \
             Install the omavcam package, which ships the modules-load.d and modprobe.d files \
             that load and label it — a user daemon cannot load a module itself."
            )
        })
}

/// Set the controls that decide what a watching application sees when frames
/// stop: keep the format across writers, and keep delivering frames after the
/// writer is gone (ADR-0010). Best effort — the capture works without them, it
/// just survives a restart less gracefully.
///
// ponytail: v4l2-ctl rather than the ioctls it wraps, which would mean libc and
// a hand-written v4l2_ext_control. These are per-device V4L2 controls, not
// module parameters, so an ordinary user can set them. `timeout_image_io` is
// left alone: it loads a still into the timeout buffer, and there is no still
// to load until the last-frame ticket.
pub fn set_controls(node: &str) {
    let controls = format!("keep_format=1,sustain_framerate=1,timeout={TIMEOUT_MS}");
    match Command::new("v4l2-ctl")
        .args(["-d", node, "-c", &controls])
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "omavcam: v4l2-ctl refused {node}'s controls ({status}); \
             the capture will run without them"
        ),
        Err(e) => eprintln!(
            "omavcam: could not run v4l2-ctl ({e}); install v4l-utils — \
             the capture will run without {node}'s controls"
        ),
    }
}

/// Launch the capture. `--no-control` keeps clicks and keystrokes off the
/// phone; the preview window belongs to a later ticket (ADR-0013), so this one
/// draws nothing.
///
// ponytail: `--camera-size` is refused by scrcpy if the lens does not offer
// exactly that size, and the capture then dies on launch. #7 has to ask
// `--list-camera-sizes` once it lets anyone choose a size.
pub fn spawn(serial: &str, node: &str) -> std::io::Result<Child> {
    Command::new("scrcpy")
        .args([
            "-s",
            serial,
            "--video-source=camera",
            &format!("--camera-size={SIZE}"),
            &format!("--v4l2-sink={node}"),
            "--no-audio",
            "--no-control",
            "--no-window",
        ])
        .stdin(Stdio::null())
        .spawn()
}

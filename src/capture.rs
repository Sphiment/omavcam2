//! The capture: one scrcpy process reading the phone's camera and writing the
//! virtual camera. The daemon owns the process; this module knows where the
//! node is, how it must be configured, and what scrcpy is launched with.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::command;
use crate::settings::{self, CameraSettings};

/// The `card_label` the package's `modprobe.d` file gives the virtual camera,
/// and the only thing that identifies it. `video_nr` is a request, not a
/// guarantee — another device can already hold the number (ADR-0008).
pub const CARD_LABEL: &str = "omavcam";

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

/// Launch the capture. The preview window belongs to a later ticket (ADR-0013),
/// so this one draws nothing.
///
/// `stay_awake` is scrcpy's own `--stay-awake`, which sets the phone's
/// "stay on while plugged in" setting and puts it back when the capture ends —
/// even when the process is killed outright, because the device-side server is
/// what restores it. Verified on hardware. scrcpy refuses the flag while
/// control is disabled, so that capture is launched with control on.
///
/// Control is otherwise off: ADR-0013 needs `--no-control` once the preview
/// exists, or the window forwards clicks and keystrokes to the phone. There is
/// no window here (`--no-window`), so nothing can forward anything, and the
/// two only collide when the preview ticket lands.
///
pub fn spawn(
    serial: &str,
    node: &str,
    stay_awake: bool,
    settings: &CameraSettings,
) -> std::io::Result<Child> {
    if node_is_capture(node)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("{node} already has a producer"),
        ));
    }
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
        "--no-window",
    ]);
    if let Some((width, height, x, y)) = settings::crop_pixels(settings) {
        process.arg(format!("--crop={width}:{height}:{x}:{y}"));
    }
    let mut child = process
        .args(match stay_awake {
            true => ["--stay-awake"],
            false => ["--no-control"],
        })
        .stdin(Stdio::null())
        .spawn()?;

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
            Ok(true) => return Ok(child),
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
}

/// Whether another process has the virtual camera open. scrcpy itself is the
/// writer and is excluded; any remaining fd is an application that has pinned
/// the frame size (ADR-0010).
pub fn has_consumer(node: &str, writer_pid: u32) -> std::io::Result<bool> {
    // ponytail: a /proc scan on the rare size-changing Apply; track fd events
    // only if machines with thousands of processes make this measurable.
    let proc = PathBuf::from(std::env::var("OMAVCAM_PROC_DIR").unwrap_or_else(|_| "/proc".into()));
    let uid = fs::metadata("/proc/self")?.uid();
    let mut uncertain = false;
    for process in fs::read_dir(&proc)? {
        let process = match process {
            Ok(process) => process,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        let Some(pid) = process.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        if pid == writer_pid {
            continue;
        }
        let metadata = match process.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        // Other users cannot open this logind-owned camera. An unreadable
        // same-user fd directory, however, is uncertainty and must fail closed.
        if metadata.uid() != uid {
            continue;
        }
        let fd_path = process.path().join("fd");
        let fds = match fs::read_dir(&fd_path) {
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

    // Some same-user sandbox processes deliberately hide their fd directory.
    // Ask the device whether a second reader can allocate buffers so those
    // unrelated processes do not block Apply; any inconclusive answer remains
    // an error rather than becoming "no consumer".
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
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("Device or resource busy") {
        return Ok(true);
    }
    let detail = if error.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", error.trim())
    };
    Err(std::io::Error::other(format!(
        "v4l2-ctl could not resolve unreadable process state ({}){detail}",
        output.status
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

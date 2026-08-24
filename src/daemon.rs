//! The daemon: one long-lived process that owns the state and pushes it.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::phones::{self, Registry};
use crate::protocol::{
    error_message, ok_message, socket_path, state_message, Capture, Connection, KnownPhone,
    PairingFailure, Phone, PreviewStyle, State, Transport, MAX_MESSAGE, VERSION,
};
use crate::settings::{self, CameraSettings, SettingsState};
use crate::{capture, command, protocol};

/// How often the daemon asks adb what is attached. adb has no way to tell us —
/// this one cannot discover devices at all (ADR-0006) — so it is asked.
///
// ponytail: a poll, because `adb devices` is a local socket call and a second
// of lag on a plug is imperceptible. adb's `track-devices` host service if the
// wakeups ever matter.
fn poll_interval() -> Duration {
    let ms: u64 = std::env::var("OMAVCAM_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    // A floor, so nothing can turn the poll into a loop spawning `adb` as fast
    // as it can.
    Duration::from_millis(ms.max(10))
}

/// Everything one running daemon holds.
struct Daemon {
    rev: u64,
    state: State,
    clients: HashMap<u64, Arc<Mutex<UnixStream>>>,
    next_client: u64,
    state_dir: PathBuf,
    registry: Registry,
    /// The running scrcpy, if there is one. The state's logical capture stays
    /// present while this is absent during reconnection.
    capture: Option<Child>,
    /// Where the preview was before it was hidden. Size stays on the same
    /// scrcpy window, so only its position needs restoring.
    preview_position: Option<[i64; 2]>,
}

type Shared = Arc<Mutex<Daemon>>;

/// Every state-changing operation passes through one turnstile. Client threads
/// still read independently, but two panels cannot race `start` against `stop`
/// or select two different phones and both receive a successful answer.
static TRANSITION: Mutex<()> = Mutex::new(());

fn transition() -> MutexGuard<'static, ()> {
    TRANSITION.lock().unwrap_or_else(PoisonError::into_inner)
}

const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Make this the state and push the whole thing to everyone. Returns the
/// revision it is now at — unchanged if the state was already this, so the
/// revision counts changes rather than requests.
///
// Writes happen under the state lock so revisions stay ordered. Each client has
// a bounded write timeout; per-client queues are the upgrade if volume grows.
fn publish(shared: &Shared, state: State) -> u64 {
    let mut inner = shared.lock().unwrap();
    if inner.state == state {
        return inner.rev;
    }
    inner.rev += 1;
    inner.state = state;
    let msg = state_message(inner.rev, &inner.state);
    let dead: Vec<u64> = inner
        .clients
        .iter()
        .filter(|(_, c)| write_line(c, &msg).is_err())
        .map(|(id, _)| *id)
        .collect();
    for id in dead {
        inner.clients.remove(&id);
    }
    inner.rev
}

fn write_line(client: &Arc<Mutex<UnixStream>>, msg: &str) -> std::io::Result<()> {
    let mut stream = client.lock().unwrap();
    stream.write_all(msg.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn probe_adb() -> bool {
    let mut process = Command::new("adb");
    process.arg("start-server");
    matches!(command::status(process), Ok(status) if status.success())
}

/// Work out the connection from what adb reports, publish it, and connect to
/// the selected phone. Called by the poll thread and by requests that should
/// answer with the state they caused.
///
/// Callers hold the transition lock, so a scan and the state derived from it
/// cannot interleave with a selection or capture lifecycle operation. Returns
/// whether adb answered successfully.
fn refresh_connection_locked(shared: &Shared) -> bool {
    let mut attached = match phones::scan() {
        Ok(attached) => attached,
        Err(e) => {
            eprintln!("omavcam: could not scan phones: {e}");
            // adb cannot see anything, so neither can we. Leaving the last
            // list standing would offer a picker full of phones nothing has
            // confirmed are still there.
            publish_adb_failure(shared);
            return false;
        }
    };

    let pairing = match &shared.lock().unwrap().state.connection {
        state @ (Connection::NeedsPairing | Connection::PairingFailed { .. }) => {
            Some(state.clone())
        }
        _ => None,
    };
    if let Some(connection) = pairing {
        let listed = attached.iter().map(Into::into).collect();
        publish_connection(shared, connection, listed);
        return true;
    }

    let mut verified_wireless = None;
    let reconnect = {
        let inner = shared.lock().unwrap();
        inner.registry.selected.as_deref().and_then(|selected| {
            (!attached
                .iter()
                .any(|phone| phone.serial == selected && phone.adb_state == "device"))
            .then(|| {
                inner.registry.phones.iter().find(|known| {
                    known.phone.serial == selected && known.transport == Transport::Wireless
                })
            })
            .flatten()
            .cloned()
        })
    };
    if let Some(known) = reconnect {
        let address = known
            .connect_address
            .clone()
            .unwrap_or_else(|| known.phone.serial.clone());
        if !phones::connect_wireless(&address) {
            let listed = attached.iter().map(Into::into).collect();
            publish_connection(
                shared,
                reconnecting_or(
                    shared,
                    Connection::Unreachable {
                        phone: known.phone,
                        connect_address: address,
                    },
                ),
                listed,
            );
            return true;
        }
        attached = match phones::scan() {
            Ok(attached) => attached,
            Err(e) => {
                eprintln!("omavcam: could not scan phones after connecting: {e}");
                publish_adb_failure(shared);
                return false;
            }
        };
        let found = attached
            .iter()
            .find(|phone| phone.serial == address && phone.adb_state == "device")
            .cloned();
        let Some(found) = found else {
            let listed = attached.iter().map(Into::into).collect();
            publish_connection(
                shared,
                reconnecting_or(
                    shared,
                    Connection::Unreachable {
                        phone: known.phone,
                        connect_address: address,
                    },
                ),
                listed,
            );
            return true;
        };
        let stable_id = phones::stable_id(&address);
        if known.hardware_id.is_some() && stable_id.as_deref() != known.hardware_id.as_deref() {
            phones::disconnect_wireless(&address);
            let listed = attached.iter().map(Into::into).collect();
            publish_connection(
                shared,
                Connection::Unreachable {
                    phone: known.phone,
                    connect_address: address,
                },
                listed,
            );
            return true;
        }
        if stable_id.is_some() {
            verified_wireless = Some(address.clone());
        }
        remember_wireless(
            shared,
            &address,
            &found.name,
            stable_id.as_deref(),
            None,
            true,
        );
    }

    let (connection, remember) = {
        let inner = shared.lock().unwrap();
        phones::resolve(&attached, inner.registry.selected.as_deref())
    };
    if let Some(serial) = remember {
        remember_selection(shared, serial);
    }

    // A phone already connected and still reported as usable is not asked
    // again. Every other transition to a known wireless endpoint verifies the
    // physical phone before targeted commands can call it Connected.
    let previous_connection = shared.lock().unwrap().state.connection.clone();
    let already_connected = match (&connection, &previous_connection) {
        (Connection::Connecting { phone }, Connection::Connected { phone: same }) => {
            phone == same
                && attached
                    .iter()
                    .any(|item| item.serial == phone.serial && item.adb_state == "device")
        }
        _ => false,
    };
    if !already_connected {
        if let Connection::Connecting { phone } = &connection {
            let known = shared
                .lock()
                .unwrap()
                .registry
                .phones
                .iter()
                .find(|known| {
                    known.transport == Transport::Wireless && known.phone.serial == phone.serial
                })
                .cloned();
            if let Some(known) = known {
                if verified_wireless.as_deref() != Some(phone.serial.as_str()) {
                    let stable_id = phones::stable_id(&phone.serial);
                    if known.hardware_id.is_some()
                        && stable_id.as_deref() != known.hardware_id.as_deref()
                    {
                        phones::disconnect_wireless(&phone.serial);
                        let listed: Vec<protocol::Attached> =
                            attached.iter().map(Into::into).collect();
                        publish_connection(
                            shared,
                            Connection::Unreachable {
                                phone: known.phone,
                                connect_address: phone.serial.clone(),
                            },
                            listed,
                        );
                        return true;
                    }
                    if let Some(stable_id) = stable_id {
                        remember_wireless(
                            shared,
                            &phone.serial,
                            &phone.name,
                            Some(&stable_id),
                            None,
                            true,
                        );
                    }
                }
            }
        }
    }
    remember_attached(shared, &attached);
    let connection = match connection {
        Connection::Connecting { phone } if already_connected => Connection::Connected { phone },
        connection => connection,
    };
    let listed: Vec<protocol::Attached> = attached.iter().map(Into::into).collect();
    publish_connection(
        shared,
        reconnecting_or(shared, connection.clone()),
        listed.clone(),
    );

    let confirmed = match connection {
        Connection::Connected { phone } => Some(phone),
        Connection::Connecting { phone } if phones::connect(&phone.serial) => {
            let needs_writer = {
                let inner = shared.lock().unwrap();
                inner.state.capture.is_some() && inner.capture.is_none()
            };
            publish_connection(
                shared,
                if needs_writer {
                    Connection::Reconnecting {
                        phone: phone.clone(),
                    }
                } else {
                    Connection::Connected {
                        phone: phone.clone(),
                    }
                },
                listed.clone(),
            );
            Some(phone)
        }
        // If it did not answer we stay in Connecting (or Reconnecting for a
        // logical capture) and the next pass tries again.
        _ => None,
    };
    if let Some(phone) = confirmed {
        if let Err(error) = ensure_settings_locked(shared, &phone.serial) {
            eprintln!(
                "omavcam: could not inspect {}'s cameras: {error}",
                phone.name
            );
        }
        let needs_writer = {
            let inner = shared.lock().unwrap();
            inner.state.capture.is_some() && inner.capture.is_none()
        };
        if needs_writer {
            resume_capture_locked(shared, &phone);
            if shared.lock().unwrap().capture.is_some() {
                publish_connection(shared, Connection::Connected { phone }, listed);
            }
        }
    }
    true
}

fn remember_attached(shared: &Shared, attached: &[phones::Attached]) {
    let mut changed = false;
    {
        let mut inner = shared.lock().unwrap();
        let selected = inner.registry.selected.clone();
        for attached in attached.iter().filter(|phone| {
            phone.adb_state == "device" && selected.as_deref() == Some(&phone.serial)
        }) {
            if let Some(known) = inner.registry.phones.iter_mut().find(|known| {
                known.phone.serial == attached.serial
                    || known.id == attached.serial
                    || known.hardware_id.as_deref() == Some(&attached.serial)
            }) {
                if known.phone.name != attached.name {
                    known.phone.name = attached.name.clone();
                    changed = true;
                }
                if known.phone.serial != attached.serial {
                    known.phone.serial = attached.serial.clone();
                    changed = true;
                }
            } else {
                inner.registry.phones.push(KnownPhone {
                    id: attached.serial.clone(),
                    hardware_id: Some(attached.serial.clone()),
                    phone: Phone::from(attached),
                    transport: Transport::Wired,
                    connect_address: None,
                });
                changed = true;
            }
        }
        if changed {
            inner
                .registry
                .phones
                .sort_by(|a, b| a.phone.serial.cmp(&b.phone.serial));
        }
    }
    if changed {
        save_registry(shared);
    }
}

fn publish_connection(shared: &Shared, connection: Connection, attached: Vec<protocol::Attached>) {
    let known = shared.lock().unwrap().registry.phones.clone();
    let settings = match &connection {
        Connection::Connected { phone } | Connection::Reconnecting { phone } => shared
            .lock()
            .unwrap()
            .state
            .settings
            .clone()
            .filter(|settings| settings.phone == phone.serial),
        _ => None,
    };
    let state = State {
        adb_ok: true,
        connection,
        attached,
        known,
        settings,
        // Re-checked on every pass rather than once at startup: a user who
        // installs the missing package while the daemon is up should see the
        // offer disappear without restarting anything.
        missing: capture::missing(),
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
}

/// adb could not describe the desk. A logical capture is the one exception to
/// NoPhone: retain that phone's applied state while its writer reconnects.
fn publish_adb_failure(shared: &Shared) {
    let connection = reconnecting_or(shared, Connection::NoPhone);
    let (previous, known) = {
        let inner = shared.lock().unwrap();
        (inner.state.clone(), inner.registry.phones.clone())
    };
    let settings = match &connection {
        Connection::Reconnecting { phone } => previous
            .settings
            .filter(|settings| settings.phone == phone.serial),
        _ => None,
    };
    publish(
        shared,
        State {
            adb_ok: false,
            connection,
            attached: Vec::new(),
            known,
            settings,
            missing: capture::missing(),
            ..previous
        },
    );
}

fn reconnecting_or(shared: &Shared, connection: Connection) -> Connection {
    match connection {
        Connection::Connected { .. } => connection,
        _ => {
            let has_visible_writer = {
                let inner = shared.lock().unwrap();
                inner.capture.is_some()
                    && inner
                        .state
                        .capture
                        .as_ref()
                        .is_some_and(|capture| capture.preview)
            };
            if has_visible_writer {
                prepare_reconnect_preview(shared);
            }
            let (phone, child) = {
                let mut inner = shared.lock().unwrap();
                (
                    inner
                        .state
                        .capture
                        .as_ref()
                        .map(|capture| capture.phone.clone()),
                    inner.capture.take(),
                )
            };
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
            phone
                .map(|phone| Connection::Reconnecting { phone })
                .unwrap_or(connection)
        }
    }
}

fn prepare_reconnect_preview(shared: &Shared) {
    let (visible, style, saved) = {
        let inner = shared.lock().unwrap();
        (
            inner
                .state
                .capture
                .as_ref()
                .is_some_and(|capture| capture.preview),
            inner.state.preview_style.clone(),
            inner.preview_position,
        )
    };
    if !visible {
        return;
    }
    let position = capture::preview_position().ok().or(saved);
    if let Some(position) = position {
        shared.lock().unwrap().preview_position = Some(position);
    }
    if let Err(error) = capture::apply_reconnect_rule(style.rounding, style.border_size, position) {
        eprintln!("omavcam: could not place the reconnect preview: {error}");
    }
}

fn resume_capture_locked(shared: &Shared, phone: &Phone) {
    let (public, settings, style, position) = {
        let inner = shared.lock().unwrap();
        if inner.capture.is_some() {
            return;
        }
        let Some(public) = inner
            .state
            .capture
            .clone()
            .filter(|capture| capture.phone.serial == phone.serial)
        else {
            return;
        };
        let Some(settings) = inner
            .state
            .settings
            .as_ref()
            .filter(|settings| settings.phone == phone.serial)
            .map(|settings| settings.applied.clone())
        else {
            return;
        };
        (
            public,
            settings,
            inner.state.preview_style.clone(),
            inner.preview_position,
        )
    };
    if settings::output_size(&settings) != public.size {
        eprintln!(
            "omavcam: refusing to reconnect {} at a different frame size",
            phone.name
        );
        return;
    }
    if public.preview {
        let hidden = match capture::initial_hidden_position() {
            Ok(hidden) => hidden,
            Err(error) => {
                eprintln!("omavcam: could not place the reconnect preview offscreen: {error}");
                return;
            }
        };
        if let Err(error) =
            capture::apply_reconnect_rule(style.rounding, style.border_size, Some(hidden))
                .and_then(|()| capture::hide_reconnect_preview(hidden))
        {
            eprintln!("omavcam: could not hide the reconnect preview: {error}");
            let _ = capture::apply_reconnect_rule(style.rounding, style.border_size, position);
            return;
        }
    }
    let started = spawn_replacement(&phone.serial, &public, &settings, &style, position);
    match started {
        Ok(child) => shared.lock().unwrap().capture = Some(child),
        Err(error) => {
            if public.preview {
                if let Err(restore_error) =
                    capture::apply_reconnect_rule(style.rounding, style.border_size, position)
                        .and_then(|()| capture::show_reconnect_preview(position))
                {
                    eprintln!("omavcam: could not restore the reconnect preview: {restore_error}");
                }
            }
            eprintln!("omavcam: could not reconnect {}: {error}", phone.name);
            let attached = shared.lock().unwrap().state.attached.clone();
            publish_connection(
                shared,
                Connection::Reconnecting {
                    phone: phone.clone(),
                },
                attached,
            );
        }
    }
}

fn spawn_replacement(
    serial: &str,
    public: &Capture,
    settings: &CameraSettings,
    style: &PreviewStyle,
    position: Option<[i64; 2]>,
) -> std::io::Result<Child> {
    let position = if public.preview {
        position
    } else {
        Some(capture::initial_hidden_position()?)
    };
    capture::apply_preview_rule(style.rounding, style.border_size, position)?;
    capture::spawn(serial, &public.node, settings)
}

fn publish_capture(shared: &Shared, capture: Option<Capture>) {
    let state = State {
        capture,
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
}

fn publish_settings(shared: &Shared, settings: SettingsState) {
    let state = State {
        settings: Some(settings),
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
}

/// Load the connected phone's capabilities once, then pair them with that
/// phone's persisted settings. A phone switch replaces the whole settings
/// state; lens ids never leak between phones.
fn ensure_settings_locked(shared: &Shared, serial: &str) -> Result<(), String> {
    if shared
        .lock()
        .unwrap()
        .state
        .settings
        .as_ref()
        .is_some_and(|settings| settings.phone == serial)
    {
        return Ok(());
    }
    let saved = {
        let inner = shared.lock().unwrap();
        inner
            .registry
            .settings
            .get(&settings_key(&inner.registry, serial))
            .cloned()
    };
    let lenses = settings::inspect(serial)?;
    publish_settings(
        shared,
        SettingsState::new(serial.to_string(), lenses, saved),
    );
    Ok(())
}

fn persist_settings(
    shared: &Shared,
    serial: &str,
    settings: CameraSettings,
) -> std::io::Result<()> {
    let (state_dir, registry, previous) = {
        let mut inner = shared.lock().unwrap();
        let key = settings_key(&inner.registry, serial);
        let previous = inner.registry.settings.insert(key.clone(), settings);
        (
            inner.state_dir.clone(),
            inner.registry.clone(),
            (key, previous),
        )
    };
    if let Err(error) = phones::save(&state_dir, &registry) {
        let mut inner = shared.lock().unwrap();
        match previous.1 {
            Some(settings) => {
                inner.registry.settings.insert(previous.0, settings);
            }
            None => {
                inner.registry.settings.remove(&previous.0);
            }
        }
        return Err(error);
    }
    Ok(())
}

fn settings_key(registry: &Registry, serial: &str) -> String {
    registry
        .phones
        .iter()
        .find(|known| known.phone.serial == serial)
        .map(|known| known.id.clone())
        .unwrap_or_else(|| serial.to_string())
}

/// Launch a capture against the connected phone. Returns the error code and
/// message for a client, or nothing when it started.
fn start_capture(
    shared: &Shared,
    stay_awake: bool,
    rounding: u64,
    border_size: u64,
) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    // scrcpy refuses --stay-awake with --no-control. The preview makes control
    // non-negotiable, and owning an adb setting restore would be less robust
    // than scrcpy's device-side restoration (issue #7).
    if stay_awake {
        return Err((
            "preview_conflict",
            "the preview requires scrcpy --no-control, which cannot be combined with --stay-awake; start without --stay-awake"
                .to_string(),
        ));
    }
    // Answer from what is true now, not from whatever the last poll saw. Reap
    // first so this refresh can immediately resume a writer that just died.
    reap_capture_locked(shared);
    let adb_ok = refresh_connection_locked(shared);
    if shared.lock().unwrap().state.capture.is_some() {
        return Ok(());
    }
    if !adb_ok {
        return Err(("adb_unavailable", "adb devices failed".to_string()));
    }

    let phone = match shared.lock().unwrap().state.connection.clone() {
        Connection::Connected { phone } => phone,
        other => return Err(("no_phone", refusal_reason(&other))),
    };
    ensure_settings_locked(shared, &phone.serial)
        .map_err(|error| ("capabilities_failed", error))?;
    let settings = shared
        .lock()
        .unwrap()
        .state
        .settings
        .as_ref()
        .expect("settings were ensured")
        .applied
        .clone();
    // The node is only looked up, never loaded: a `systemd --user` service has
    // no capabilities to load a module with (ADR-0008).
    let node = capture::find_node().map_err(|e| ("no_virtual_camera", e))?;
    capture::set_controls(&node);
    capture::apply_preview_rule(rounding, border_size, None).map_err(|e| {
        (
            "capture_failed",
            format!("could not prepare the preview: {e}"),
        )
    })?;
    capture::apply_reconnect_rule(rounding, border_size, None).map_err(|e| {
        (
            "capture_failed",
            format!("could not prepare the reconnect preview: {e}"),
        )
    })?;

    match capture::spawn(&phone.serial, &node, &settings) {
        Ok(child) => {
            let position = capture::preview_position().ok();
            let mut inner = shared.lock().unwrap();
            inner.capture = Some(child);
            inner.preview_position = position;
            drop(inner);
            let state = State {
                capture: Some(Capture {
                    phone,
                    node,
                    size: settings::output_size(&settings),
                    stay_awake: false,
                    preview: true,
                }),
                preview_style: PreviewStyle {
                    rounding,
                    border_size,
                },
                ..shared.lock().unwrap().state.clone()
            };
            publish(shared, state);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err((
            "capture_failed",
            format!("could not launch scrcpy: {e}; install scrcpy and try again"),
        )),
        Err(e) => Err(("capture_failed", format!("scrcpy did not start: {e}"))),
    }
}

/// Why a capture cannot start, in the words the connection is already using.
fn refusal_reason(connection: &Connection) -> String {
    match connection {
        Connection::Unselected { available } => format!(
            "{} phones are attached and none is selected; choose one with: omavcam select <serial>",
            available.len()
        ),
        Connection::Unauthorised { phone } => format!(
            "{} has not accepted the debugging prompt; accept it on the phone",
            phone.name
        ),
        Connection::Connecting { phone } => format!("still connecting to {}", phone.name),
        Connection::Reconnecting { phone } => format!("reconnecting to {}", phone.name),
        Connection::NeedsPairing => {
            "wireless pairing is needed; run: omavcam pair".to_string()
        }
        Connection::PairingFailed { reason } => pairing_message(reason).to_string(),
        Connection::Unreachable {
            phone,
            connect_address,
        } => format!(
            "{} is unreachable at {connect_address}; wake it, check both devices are on the same network, and re-read the connect address — do not pair again",
            phone.name
        ),
        // No phone at all, and nothing to select from.
        _ => "no phone is attached; plug one in and select it with: omavcam select <serial>"
            .to_string(),
    }
}

/// End the capture. Stopping one that is not running is a no-op: the switch
/// reads off either way.
fn stop_capture(shared: &Shared) {
    let _turn = transition();
    stop_capture_locked(shared);
    refresh_connection_locked(shared);
}

fn stop_capture_locked(shared: &Shared) {
    let child = {
        let mut inner = shared.lock().unwrap();
        inner.preview_position = None;
        inner.capture.take()
    };
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    let previous = shared.lock().unwrap().state.clone();
    let connection = if matches!(previous.connection, Connection::Reconnecting { .. }) {
        Connection::NoPhone
    } else {
        previous.connection.clone()
    };
    publish(
        shared,
        State {
            connection,
            capture: None,
            ..previous
        },
    );
}

/// Losing the writer retains the logical capture: the open consumer keeps the
/// node and format pinned while the watcher restarts this exact capture.
fn reap_capture_locked(shared: &Shared) {
    let exited = {
        let mut inner = shared.lock().unwrap();
        match inner.capture.as_mut().map(Child::try_wait) {
            Some(Ok(Some(_))) => true,
            // A capture we cannot ask about is one we can no longer manage.
            Some(Err(_)) => true,
            _ => false,
        }
    };
    if exited {
        prepare_reconnect_preview(shared);
        let mut inner = shared.lock().unwrap();
        inner.capture = None;
        let phone = inner
            .state
            .capture
            .as_ref()
            .map(|capture| capture.phone.clone());
        drop(inner);
        if let Some(phone) = phone {
            let attached = shared.lock().unwrap().state.attached.clone();
            publish_connection(shared, Connection::Reconnecting { phone }, attached);
        }
    }
}

fn set_preview(
    shared: &Shared,
    visible: bool,
    rounding: u64,
    border_size: u64,
) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    reap_capture_locked(shared);
    let mut public = shared.lock().unwrap().state.capture.clone().ok_or((
        "no_capture",
        "start a capture before opening its preview".to_string(),
    ))?;
    capture::apply_preview_style(rounding, border_size).map_err(|e| {
        (
            "preview_failed",
            format!("could not theme the preview: {e}"),
        )
    })?;
    capture::apply_reconnect_rule(
        rounding,
        border_size,
        shared.lock().unwrap().preview_position,
    )
    .map_err(|e| {
        (
            "preview_failed",
            format!("could not theme the reconnect preview: {e}"),
        )
    })?;
    let style = PreviewStyle {
        rounding,
        border_size,
    };
    let state = State {
        preview_style: style,
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
    if public.preview == visible {
        return Ok(());
    }

    if visible {
        let position = shared.lock().unwrap().preview_position;
        match position {
            Some(position) => capture::move_preview(position),
            None => capture::center_preview(),
        }
        .map_err(|e| ("preview_failed", format!("could not show the preview: {e}")))?;
    } else {
        let position = capture::preview_position().map_err(|e| {
            (
                "preview_failed",
                format!("could not locate the preview: {e}"),
            )
        })?;
        capture::hide_preview()
            .map_err(|e| ("preview_failed", format!("could not hide the preview: {e}")))?;
        shared.lock().unwrap().preview_position = Some(position);
    }
    public.preview = visible;
    publish_capture(shared, Some(public));
    Ok(())
}

/// Remember which phone is in use, so choosing is a one-time act.
fn remember_selection(shared: &Shared, serial: String) {
    let (state_dir, registry) = {
        let mut inner = shared.lock().unwrap();
        if inner.registry.selected.as_deref() == Some(serial.as_str()) {
            return;
        }
        inner.registry.selected = Some(serial);
        (inner.state_dir.clone(), inner.registry.clone())
    };
    if let Err(e) = phones::save(&state_dir, &registry) {
        // Worth continuing: the selection holds until the daemon restarts.
        eprintln!("omavcam: could not remember the selected phone: {e}");
    }
}

fn save_registry(shared: &Shared) {
    let (state_dir, registry) = {
        let inner = shared.lock().unwrap();
        (inner.state_dir.clone(), inner.registry.clone())
    };
    if let Err(e) = phones::save(&state_dir, &registry) {
        eprintln!("omavcam: could not remember phones: {e}");
    }
}

fn remember_wireless(
    shared: &Shared,
    address: &str,
    name: &str,
    stable_id: Option<&str>,
    previous_address: Option<&str>,
    select: bool,
) -> Phone {
    let phone = Phone {
        serial: address.to_string(),
        name: name.to_string(),
    };
    {
        let mut inner = shared.lock().unwrap();
        if select {
            inner.registry.selected = Some(address.to_string());
        }
        let address_id = inner
            .registry
            .phones
            .iter()
            .find(|known| {
                known.phone.serial == address
                    || previous_address == Some(known.phone.serial.as_str())
            })
            .map(|known| known.id.clone());
        let physical_id = stable_id.and_then(|id| {
            inner
                .registry
                .phones
                .iter()
                .find(|known| known.hardware_id.as_deref() == Some(id) || known.id == id)
                .map(|known| known.id.clone())
        });
        let canonical_id = physical_id.or_else(|| address_id.clone());
        let mut known = if let Some(id) = canonical_id {
            let index = inner
                .registry
                .phones
                .iter()
                .position(|known| known.id == id)
                .unwrap();
            inner.registry.phones.remove(index)
        } else {
            KnownPhone {
                id: address.to_string(),
                hardware_id: None,
                phone: phone.clone(),
                transport: Transport::Wireless,
                connect_address: Some(address.to_string()),
            }
        };
        if let Some(id) = address_id.filter(|id| id != &known.id) {
            if let Some(index) = inner
                .registry
                .phones
                .iter()
                .position(|other| other.id == id)
            {
                inner.registry.phones.remove(index);
            }
        }
        known.phone = phone.clone();
        known.transport = Transport::Wireless;
        known.connect_address = Some(address.to_string());
        // A wireless phone can have been remembered from `adb devices` before
        // pairing, where its endpoint was the only serial there was. An
        // endpoint recorded as identity is no identity at all.
        if known.hardware_id.as_deref() == Some(address) {
            known.hardware_id = None;
        }
        if let Some(id) = stable_id {
            known.hardware_id = Some(id.to_string());
        }
        inner.registry.phones.retain(|other| {
            other.phone.serial != address
                && stable_id.is_none_or(|id| other.hardware_id.as_deref() != Some(id))
        });
        inner.registry.phones.push(known);
        inner.registry.phones.sort_by(|a, b| a.id.cmp(&b.id));
    }
    save_registry(shared);
    phone
}

fn pairing_message(reason: &PairingFailure) -> &'static str {
    match reason {
        PairingFailure::WrongCode => "the pairing code is wrong; re-read the six-digit code",
        PairingFailure::WrongAddress => {
            "the pairing address is wrong; use the address beside the six-digit code"
        }
        PairingFailure::Unreachable => {
            "the pairing address is unreachable; the phone may be asleep or on a different network"
        }
    }
}

fn unreachable_message() -> &'static str {
    "the paired phone is unreachable; wake it, check both devices are on the same network, and re-read the connect address from the main wireless debugging screen"
}

fn pair_phone(
    shared: &Shared,
    pair_address: &str,
    code: &str,
    connect_address: &str,
) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    if !phones::valid_endpoint(connect_address) {
        return Err((
            "wrong_connect_address",
            "the connect address is wrong; re-read it from the main wireless debugging screen"
                .to_string(),
        ));
    }
    if let Err(reason) = phones::pair(pair_address, code) {
        let attached = shared.lock().unwrap().state.attached.clone();
        publish_connection(
            shared,
            Connection::PairingFailed {
                reason: reason.clone(),
            },
            attached,
        );
        let code = match reason {
            PairingFailure::WrongCode => "wrong_code",
            PairingFailure::WrongAddress => "wrong_pair_address",
            PairingFailure::Unreachable => "unreachable",
        };
        return Err((code, pairing_message(&reason).to_string()));
    }

    let capture_phone = shared
        .lock()
        .unwrap()
        .state
        .capture
        .as_ref()
        .map(|capture| capture.phone.serial.clone());
    if capture_phone
        .as_deref()
        .is_some_and(|running| running != connect_address)
    {
        stop_capture_locked(shared);
    }
    let provisional = remember_wireless(shared, connect_address, connect_address, None, None, true);
    if !phones::connect_wireless(connect_address) {
        let attached = shared.lock().unwrap().state.attached.clone();
        publish_connection(
            shared,
            Connection::Unreachable {
                phone: provisional,
                connect_address: connect_address.to_string(),
            },
            attached,
        );
        return Err(("unreachable", unreachable_message().to_string()));
    }

    let attached = phones::scan().map_err(|error| {
        eprintln!("omavcam: could not scan phones after connecting: {error}");
        publish_adb_failure(shared);
        ("adb_unavailable", error.to_string())
    })?;
    let listed: Vec<protocol::Attached> = attached.iter().map(Into::into).collect();
    let found = attached
        .iter()
        .find(|phone| phone.serial == connect_address && phone.adb_state == "device")
        .ok_or_else(|| {
            publish_connection(
                shared,
                Connection::Unreachable {
                    phone: provisional.clone(),
                    connect_address: connect_address.to_string(),
                },
                listed.clone(),
            );
            ("unreachable", unreachable_message().to_string())
        })?;
    let stable_id = phones::stable_id(connect_address);
    let connected = remember_wireless(
        shared,
        connect_address,
        &found.name,
        stable_id.as_deref(),
        None,
        true,
    );
    publish_connection(shared, Connection::Connecting { phone: connected }, listed);
    refresh_connection_locked(shared);
    if matches!(
        &shared.lock().unwrap().state.connection,
        Connection::Connected { phone } if phone.serial == connect_address
    ) {
        Ok(())
    } else {
        Err(("unreachable", unreachable_message().to_string()))
    }
}

fn begin_pairing(shared: &Shared) {
    let _turn = transition();
    let attached = shared.lock().unwrap().state.attached.clone();
    publish_connection(shared, Connection::NeedsPairing, attached);
}

fn update_connect_address(
    shared: &Shared,
    serial: &str,
    connect_address: &str,
) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    if !phones::valid_endpoint(connect_address) {
        return Err((
            "wrong_connect_address",
            "the connect address is wrong; re-read it from the main wireless debugging screen"
                .to_string(),
        ));
    }
    let known = shared
        .lock()
        .unwrap()
        .registry
        .phones
        .iter()
        .find(|known| known.phone.serial == serial && known.transport == Transport::Wireless)
        .cloned()
        .ok_or_else(|| {
            (
                "no_such_phone",
                format!("no paired phone {serial:?} is known"),
            )
        })?;
    if known
        .connect_address
        .as_deref()
        .unwrap_or(&known.phone.serial)
        == connect_address
    {
        return Ok(());
    }
    if !phones::connect_wireless(connect_address) {
        let attached = shared.lock().unwrap().state.attached.clone();
        publish_connection(
            shared,
            reconnecting_or(
                shared,
                Connection::Unreachable {
                    phone: known.phone,
                    connect_address: connect_address.to_string(),
                },
            ),
            attached,
        );
        return Err(("unreachable", unreachable_message().to_string()));
    }
    let attached = phones::scan().map_err(|error| {
        phones::disconnect_wireless(connect_address);
        eprintln!("omavcam: could not scan phones after connecting: {error}");
        publish_adb_failure(shared);
        ("adb_unavailable", error.to_string())
    })?;
    let mut listed: Vec<protocol::Attached> = attached.iter().map(Into::into).collect();
    let Some(found) = attached
        .iter()
        .find(|phone| phone.serial == connect_address && phone.adb_state == "device")
    else {
        if phones::disconnect_wireless(connect_address) {
            listed.retain(|phone| phone.phone.serial != connect_address);
        }
        publish_connection(
            shared,
            reconnecting_or(
                shared,
                Connection::Unreachable {
                    phone: known.phone.clone(),
                    connect_address: connect_address.to_string(),
                },
            ),
            listed,
        );
        return Err(("unreachable", unreachable_message().to_string()));
    };
    let stable_id = match phones::stable_id(connect_address) {
        Some(stable_id) => stable_id,
        None => {
            phones::disconnect_wireless(connect_address);
            return Err((
                "phone_identity_failed",
                "could not verify that the new connect address belongs to the paired phone"
                    .to_string(),
            ));
        }
    };
    if known
        .hardware_id
        .as_deref()
        .is_some_and(|expected| expected != stable_id)
    {
        phones::disconnect_wireless(connect_address);
        return Err((
            "wrong_phone",
            "the new connect address belongs to a different phone; selection was not changed"
                .to_string(),
        ));
    }

    let (capture_uses_old_address, was_selected) = {
        let inner = shared.lock().unwrap();
        let physical_serial = inner
            .registry
            .phones
            .iter()
            .find(|known| known.hardware_id.as_deref() == Some(&stable_id))
            .map(|known| known.phone.serial.as_str());
        let same_phone = |candidate: &str| {
            candidate == serial || physical_serial.is_some_and(|physical| candidate == physical)
        };
        (
            inner
                .state
                .capture
                .as_ref()
                .is_some_and(|capture| same_phone(&capture.phone.serial)),
            inner.registry.selected.as_deref().is_some_and(same_phone),
        )
    };
    if capture_uses_old_address {
        // The phone proved its identity at the new endpoint, so the logical
        // capture follows it. Only the writer bound to the old serial dies;
        // the refresh below resumes it at the new one (ADR: reconnect, #11).
        let child = shared.lock().unwrap().capture.take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let mut inner = shared.lock().unwrap();
        let old_serial = inner
            .state
            .capture
            .as_ref()
            .map(|capture| capture.phone.serial.clone());
        if let Some(capture) = inner.state.capture.as_mut() {
            capture.phone = Phone {
                serial: connect_address.to_string(),
                name: found.name.clone(),
            };
        }
        if let Some(settings) = inner
            .state
            .settings
            .as_mut()
            .filter(|settings| Some(&settings.phone) == old_serial.as_ref())
        {
            settings.phone = connect_address.to_string();
        }
    }
    let phone = remember_wireless(
        shared,
        connect_address,
        &found.name,
        Some(&stable_id),
        Some(serial),
        was_selected,
    );
    if was_selected {
        publish_connection(shared, Connection::Connecting { phone }, listed);
    }
    refresh_connection_locked(shared);
    if !was_selected
        || matches!(
            &shared.lock().unwrap().state.connection,
            Connection::Connected { phone } if phone.serial == connect_address
        )
    {
        Ok(())
    } else {
        Err(("unreachable", unreachable_message().to_string()))
    }
}

fn forget_phone(shared: &Shared, serial: &str) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    let forgotten = {
        let mut inner = shared.lock().unwrap();
        let Some(index) = inner
            .registry
            .phones
            .iter()
            .position(|known| known.phone.serial == serial)
        else {
            return Err(("no_such_phone", format!("no known phone {serial:?}")));
        };
        let forgotten = inner.registry.phones.remove(index);
        inner.registry.settings.remove(&forgotten.id);
        if inner.registry.selected.as_deref() == Some(serial) {
            inner.registry.selected = None;
        }
        forgotten
    };
    save_registry(shared);
    if forgotten.transport == Transport::Wireless {
        phones::disconnect_wireless(
            forgotten
                .connect_address
                .as_deref()
                .unwrap_or(&forgotten.phone.serial),
        );
    }
    if shared
        .lock()
        .unwrap()
        .state
        .capture
        .as_ref()
        .is_some_and(|capture| capture.phone.serial == serial)
    {
        stop_capture_locked(shared);
    }

    if shared.lock().unwrap().registry.selected.is_none()
        && phones::scan().is_ok_and(|attached| attached.is_empty())
    {
        publish_connection(shared, Connection::NeedsPairing, Vec::new());
    } else {
        refresh_connection_locked(shared);
    }
    Ok(())
}

fn refresh_adb(shared: &Shared) -> bool {
    let _turn = transition();
    if !probe_adb() {
        publish_adb_failure(shared);
        return false;
    }
    refresh_connection_locked(shared)
}

fn select_phone(shared: &Shared, serial: &str) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    let attached = phones::scan().map_err(|e| ("adb_unavailable", e.to_string()))?;
    let is_known = shared
        .lock()
        .unwrap()
        .registry
        .phones
        .iter()
        .any(|known| known.phone.serial == serial && known.transport == Transport::Wireless);
    if !is_known && !attached.iter().any(|phone| phone.serial == serial) {
        // Say what *is* attached: the caller cannot always see it. A remembered
        // phone that is unplugged reports `NoPhone`, so this error is also how
        // someone finds the serial of another phone on the desk.
        let serials: Vec<&str> = attached.iter().map(|phone| phone.serial.as_str()).collect();
        let known = match serials.is_empty() {
            true => "none".to_string(),
            false => serials.join(", "),
        };
        let message = match serial.is_empty() {
            true => format!("name a phone; attached: {known}"),
            false => format!("no phone {serial:?} is attached; attached: {known}"),
        };
        return Err(("no_such_phone", message));
    }

    let capture_phone = shared
        .lock()
        .unwrap()
        .state
        .capture
        .as_ref()
        .map(|capture| capture.phone.serial.clone());
    if capture_phone
        .as_deref()
        .is_some_and(|running| running != serial)
    {
        stop_capture_locked(shared);
    }
    remember_selection(shared, serial.to_string());
    if matches!(
        shared.lock().unwrap().state.connection,
        Connection::NeedsPairing | Connection::PairingFailed { .. }
    ) {
        let listed = attached.iter().map(Into::into).collect();
        publish_connection(shared, Connection::NoPhone, listed);
    }
    refresh_connection_locked(shared);
    Ok(())
}

fn set_setting(shared: &Shared, name: &str, value: &Value) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    if !refresh_connection_locked(shared) {
        return Err(("adb_unavailable", "adb devices failed".to_string()));
    }
    let phone = match shared.lock().unwrap().state.connection.clone() {
        Connection::Connected { phone } => phone,
        other => return Err(("no_phone", refusal_reason(&other))),
    };
    ensure_settings_locked(shared, &phone.serial)
        .map_err(|error| ("capabilities_failed", error))?;
    let mut settings = shared
        .lock()
        .unwrap()
        .state
        .settings
        .clone()
        .expect("settings were ensured");
    settings
        .change(name, value)
        .map_err(|error| ("invalid_setting", error))?;
    publish_settings(shared, settings);
    Ok(())
}

fn discard_settings(shared: &Shared) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    let mut settings = shared.lock().unwrap().state.settings.clone().ok_or((
        "no_phone",
        "no connected phone has camera settings".to_string(),
    ))?;
    settings.discard();
    publish_settings(shared, settings);
    Ok(())
}

fn apply_settings(shared: &Shared) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    if !refresh_connection_locked(shared) {
        return Err(("adb_unavailable", "adb devices failed".to_string()));
    }
    reap_capture_locked(shared);
    let phone = match shared.lock().unwrap().state.connection.clone() {
        Connection::Connected { phone } => phone,
        other => return Err(("no_phone", refusal_reason(&other))),
    };
    ensure_settings_locked(shared, &phone.serial)
        .map_err(|error| ("capabilities_failed", error))?;
    let mut view = shared
        .lock()
        .unwrap()
        .state
        .settings
        .clone()
        .expect("settings were ensured");
    if !view.has_pending_changes {
        return Ok(());
    }

    let running = {
        let inner = shared.lock().unwrap();
        inner
            .state
            .capture
            .clone()
            .zip(inner.capture.as_ref().map(Child::id))
    };
    if let Some((capture_state, writer_pid)) = &running {
        let size_changes =
            settings::output_size(&view.pending) != settings::output_size(&view.applied);
        let has_consumer = size_changes
            .then(|| capture::has_consumer(&capture_state.node, *writer_pid))
            .transpose()
            .map_err(|error| {
                (
                    "consumer_check_failed",
                    format!(
                        "could not inspect who is using {}: {error}",
                        capture_state.node
                    ),
                )
            })?
            .unwrap_or(false);
        if has_consumer {
            let message = format!(
                "{} is in use; close the application using it before changing frame size from {} to {}",
                capture_state.node,
                settings::output_size(&view.applied),
                settings::output_size(&view.pending)
            );
            view.note_rejection(message.clone());
            publish_settings(shared, view);
            return Err(("camera_in_use", message));
        }
    }

    let restart_position = running.as_ref().map_or(Ok(None), |(capture, _)| {
        if capture.preview {
            capture::preview_position().map(Some).map_err(|error| {
                (
                    "preview_failed",
                    format!("could not locate the preview before Apply: {error}"),
                )
            })
        } else {
            Ok(shared.lock().unwrap().preview_position)
        }
    })?;
    let previous = view.applied.clone();
    let pending = view.pending.clone();
    persist_settings(shared, &phone.serial, pending.clone()).map_err(|error| {
        (
            "apply_not_persisted",
            format!("could not persist Apply: {error}"),
        )
    })?;

    let Some((capture_state, _)) = running else {
        view.applied();
        publish_settings(shared, view);
        return Ok(());
    };

    let child = shared.lock().unwrap().capture.take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    let style = shared.lock().unwrap().state.preview_style.clone();
    match spawn_replacement(
        &phone.serial,
        &capture_state,
        &pending,
        &style,
        restart_position,
    ) {
        Ok(child) => {
            shared.lock().unwrap().capture = Some(child);
            view.applied();
            let state = State {
                capture: Some(Capture {
                    phone,
                    node: capture_state.node,
                    size: settings::output_size(&pending),
                    stay_awake: false,
                    preview: capture_state.preview,
                }),
                settings: Some(view),
                ..shared.lock().unwrap().state.clone()
            };
            publish(shared, state);
            Ok(())
        }
        Err(error) => {
            let rejected = format!("{} ({error})", serde_json::to_string(&pending).unwrap());
            let persistence_error = persist_settings(shared, &phone.serial, previous.clone()).err();
            match spawn_replacement(
                &phone.serial,
                &capture_state,
                &previous,
                &style,
                restart_position,
            ) {
                Ok(child) => {
                    shared.lock().unwrap().capture = Some(child);
                    let message = match persistence_error {
                        Some(ref error) => format!(
                            "scrcpy rejected {rejected}; previous capture restarted, but its settings could not be restored on disk: {error}"
                        ),
                        None => format!("scrcpy rejected {rejected}; previous capture restarted"),
                    };
                    view.reject(message.clone());
                    let state = State {
                        capture: Some(capture_state),
                        settings: Some(view),
                        ..shared.lock().unwrap().state.clone()
                    };
                    publish(shared, state);
                    Err((
                        if persistence_error.is_some() {
                            "rollback_failed"
                        } else {
                            "capture_failed"
                        },
                        message,
                    ))
                }
                Err(rollback_error) => {
                    let message = format!(
                        "scrcpy rejected {rejected}; restarting the previous capture also failed: {rollback_error}"
                    );
                    view.reject(message.clone());
                    let state = State {
                        capture: None,
                        settings: Some(view),
                        ..shared.lock().unwrap().state.clone()
                    };
                    publish(shared, state);
                    Err(("rollback_failed", message))
                }
            }
        }
    }
}

/// Systemd hands the listening socket in on fd 3 when it activates us;
/// otherwise (tests, running by hand) bind the path ourselves.
fn listener() -> std::io::Result<UnixListener> {
    let activated = std::env::var("LISTEN_FDS").as_deref() == Ok("1")
        && std::env::var("LISTEN_PID").ok() == Some(std::process::id().to_string());
    if activated {
        return Ok(unsafe { UnixListener::from_raw_fd(3) });
    }
    let path = socket_path();
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(bind_error) if bind_error.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&path).is_ok() {
                return Err(bind_error);
            }
            fs::remove_file(&path)?;
            UnixListener::bind(&path)?
        }
        Err(e) => return Err(e),
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn state_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OMAVCAM_STATE_DIR") {
        return p.into();
    }
    let base = std::env::var("XDG_STATE_HOME")
        .unwrap_or_else(|_| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(base).join("omavcam")
}

/// Logs go to stderr, which is journald's for a systemd service.
pub fn run() -> std::io::Result<()> {
    let dir = state_dir();
    fs::create_dir_all(&dir)?;
    let listener = listener()?;
    let adb_ok = probe_adb();
    let registry = phones::load(&dir);
    let shared: Shared = Arc::new(Mutex::new(Daemon {
        rev: 1,
        state: State {
            adb_ok,
            known: registry.phones.clone(),
            missing: capture::missing(),
            ..Default::default()
        },
        clients: HashMap::new(),
        next_client: 0,
        registry,
        state_dir: dir.clone(),
        capture: None,
        preview_position: None,
    }));
    // The first client, especially the one that socket-activated us, must not
    // observe a placeholder NoPhone state and exit before the watcher runs.
    if adb_ok {
        let _turn = transition();
        refresh_connection_locked(&shared);
    }
    eprintln!("omavcam: listening, state dir {}", dir.display());

    // The world changes without anyone asking: a phone is plugged in, or
    // unplugged mid-call, or scrcpy dies. Nothing polls the daemon, so the
    // daemon polls adb — and reaps the capture on the same pass rather than
    // running a thread of its own for one `try_wait`.
    let watcher = Arc::clone(&shared);
    thread::spawn(move || loop {
        {
            let _turn = transition();
            refresh_connection_locked(&watcher);
            reap_capture_locked(&watcher);
        }
        thread::sleep(poll_interval());
    });

    for stream in listener.incoming() {
        let stream = stream?;
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            if let Err(e) = serve(&shared, stream) {
                eprintln!("omavcam: client ended: {e}");
            }
        });
    }
    Ok(())
}

fn serve(shared: &Shared, stream: UnixStream) -> std::io::Result<()> {
    stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;
    let reader = BufReader::new(stream.try_clone()?);
    let client = Arc::new(Mutex::new(stream));

    // Register and send the current state under one lock, so a publish racing
    // this can neither be missed nor delivered twice.
    let id = {
        let mut inner = shared.lock().unwrap();
        let id = inner.next_client;
        inner.next_client += 1;
        inner.clients.insert(id, Arc::clone(&client));
        // Still holding the lock: a publish must not slip a higher revision in
        // ahead of this snapshot, or this client would see revisions go
        // backwards. A write failure here is left to the read loop, which
        // deregisters on the way out.
        let msg = state_message(inner.rev, &inner.state);
        let _ = write_line(&client, &msg);
        id
    };

    let result = read_requests(shared, &client, reader);
    shared.lock().unwrap().clients.remove(&id);
    result
}

fn read_requests(
    shared: &Shared,
    client: &Arc<Mutex<UnixStream>>,
    mut reader: BufReader<UnixStream>,
) -> std::io::Result<()> {
    loop {
        let mut line = Vec::new();
        // Reads at most one byte past the bound, so an endless line cannot
        // make us allocate without limit.
        let n = (&mut reader)
            .take(MAX_MESSAGE as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > MAX_MESSAGE {
            let rev = shared.lock().unwrap().rev;
            let msg = error_message(
                &Value::Null,
                rev,
                "message_too_large",
                &format!("request exceeds {MAX_MESSAGE} bytes"),
            );
            let _ = write_line(client, &msg);
            return Ok(());
        }
        if !line.ends_with(b"\n") {
            return Ok(());
        }
        let response = handle(shared, &line);
        write_line(client, &response)?;
    }
}

/// Parse loosely enough to always answer with the caller's request id, even
/// when the version is wrong or the kind is unknown.
fn handle(shared: &Shared, line: &[u8]) -> String {
    let current_rev = || shared.lock().unwrap().rev;

    let request: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => return error_message(&Value::Null, current_rev(), "bad_json", &e.to_string()),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    match request.get("v").and_then(Value::as_u64) {
        Some(v) if v == VERSION as u64 => {}
        other => {
            return error_message(
                &id,
                current_rev(),
                "unsupported_version",
                &format!("daemon speaks protocol {VERSION}, client sent {other:?}"),
            )
        }
    }
    if id.is_null() {
        return error_message(
            &id,
            current_rev(),
            "missing_id",
            "every request needs an id",
        );
    }

    match request.get("kind").and_then(Value::as_str) {
        Some("status") => ok_message(&id, current_rev()),
        Some("refresh") => {
            let adb_ok = refresh_adb(shared);
            if adb_ok {
                ok_message(&id, current_rev())
            } else {
                error_message(
                    &id,
                    current_rev(),
                    "adb_unavailable",
                    "adb start-server failed",
                )
            }
        }
        Some("select") => {
            let serial = request.get("serial").and_then(Value::as_str).unwrap_or("");
            match select_phone(shared, serial) {
                Ok(()) => ok_message(&id, current_rev()),
                Err((code, message)) => error_message(&id, current_rev(), code, &message),
            }
        }
        Some("set") => {
            let setting = request.get("setting").and_then(Value::as_str).unwrap_or("");
            let value = request.get("value").unwrap_or(&Value::Null);
            match set_setting(shared, setting, value) {
                Ok(()) => ok_message(&id, current_rev()),
                Err((code, message)) => error_message(&id, current_rev(), code, &message),
            }
        }
        Some("start") => {
            let (rounding, border_size) = preview_style(&request);
            match start_capture(
                shared,
                request
                    .get("stay_awake")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                rounding,
                border_size,
            ) {
                Ok(()) => ok_message(&id, current_rev()),
                Err((code, message)) => error_message(&id, current_rev(), code, &message),
            }
        }
        Some("preview") => {
            let Some(visible) = request.get("visible").and_then(Value::as_bool) else {
                return error_message(
                    &id,
                    current_rev(),
                    "bad_request",
                    "preview needs a boolean visible field",
                );
            };
            let (rounding, border_size) = preview_style(&request);
            match set_preview(shared, visible, rounding, border_size) {
                Ok(()) => ok_message(&id, current_rev()),
                Err((code, message)) => error_message(&id, current_rev(), code, &message),
            }
        }
        Some("apply") => match apply_settings(shared) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("discard") => match discard_settings(shared) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("pair") => match pair_phone(
            shared,
            request
                .get("pair_address")
                .and_then(Value::as_str)
                .unwrap_or(""),
            request.get("code").and_then(Value::as_str).unwrap_or(""),
            request
                .get("connect_address")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("begin_pairing") => {
            begin_pairing(shared);
            ok_message(&id, current_rev())
        }
        Some("connect") => match update_connect_address(
            shared,
            request.get("serial").and_then(Value::as_str).unwrap_or(""),
            request
                .get("connect_address")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("forget") => match forget_phone(
            shared,
            request.get("serial").and_then(Value::as_str).unwrap_or(""),
        ) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("stop") => {
            stop_capture(shared);
            ok_message(&id, current_rev())
        }
        other => error_message(
            &id,
            current_rev(),
            "unknown_request",
            &format!("no such request kind: {other:?}"),
        ),
    }
}

/// Omarchy's live Style tokens arrive with preview requests. Clamp to the
/// compositor's supported range before interpolating them into a Lua rule.
fn preview_style(request: &Value) -> (u64, u64) {
    let rounding = request
        .get("rounding")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(20);
    let border_size = request
        .get("border_size")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .min(20);
    (rounding, border_size)
}

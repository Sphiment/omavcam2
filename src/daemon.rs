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
    error_message, ok_message, socket_path, state_message, Capture, Connection, State, MAX_MESSAGE,
    VERSION,
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
    /// The running scrcpy, if there is one. The state's `capture` is what this
    /// looks like to a client; the two are set together.
    capture: Option<Child>,
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
    let attached = match phones::scan() {
        Ok(attached) => attached,
        Err(e) => {
            eprintln!("omavcam: could not scan phones: {e}");
            // adb cannot see anything, so neither can we. Leaving the last
            // list standing would offer a picker full of phones nothing has
            // confirmed are still there.
            let state = State {
                adb_ok: false,
                connection: Connection::NoPhone,
                attached: Vec::new(),
                ..shared.lock().unwrap().state.clone()
            };
            publish(shared, state);
            return false;
        }
    };
    let (connection, remember) = {
        let inner = shared.lock().unwrap();
        phones::resolve(&attached, inner.registry.selected.as_deref())
    };
    if let Some(serial) = remember {
        remember_selection(shared, serial);
    }

    // A phone already connected and still reported as usable is not asked
    // again. `offline` devices remain listed by adb, so identity alone is not
    // enough to preserve Connected.
    let connection = match (&connection, &shared.lock().unwrap().state.connection) {
        (Connection::Connecting { phone }, Connection::Connected { phone: same })
            if phone == same
                && attached
                    .iter()
                    .any(|item| item.serial == phone.serial && item.adb_state == "device") =>
        {
            Connection::Connected {
                phone: phone.clone(),
            }
        }
        _ => connection,
    };
    let listed: Vec<protocol::Attached> = attached.iter().map(Into::into).collect();
    publish_connection(shared, connection.clone(), listed.clone());

    if let Connection::Connecting { phone } = connection {
        if phones::connect(&phone.serial) {
            publish_connection(shared, Connection::Connected { phone }, listed);
        }
        // If it did not answer we stay in Connecting and the next pass tries
        // again — a phone that is booting or half-asleep needs no error.
    }
    let connected = shared.lock().unwrap().state.connection.clone();
    if let Connection::Connected { phone } = connected {
        if let Err(error) = ensure_settings_locked(shared, &phone.serial) {
            eprintln!(
                "omavcam: could not inspect {}'s cameras: {error}",
                phone.name
            );
        }
    }
    true
}

fn publish_connection(shared: &Shared, connection: Connection, attached: Vec<protocol::Attached>) {
    let state = State {
        adb_ok: true,
        connection,
        attached,
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
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
    let saved = shared
        .lock()
        .unwrap()
        .registry
        .settings
        .get(serial)
        .cloned();
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
        let previous = inner.registry.settings.insert(serial.to_string(), settings);
        (inner.state_dir.clone(), inner.registry.clone(), previous)
    };
    if let Err(error) = phones::save(&state_dir, &registry) {
        let mut inner = shared.lock().unwrap();
        match previous {
            Some(settings) => {
                inner.registry.settings.insert(serial.to_string(), settings);
            }
            None => {
                inner.registry.settings.remove(serial);
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Launch a capture against the connected phone. Returns the error code and
/// message for a client, or nothing when it started.
fn start_capture(shared: &Shared, stay_awake: bool) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    // Answer from what is true now, not from whatever the last poll saw: the
    // phone may have gone, and the capture may already have died with it.
    if !refresh_connection_locked(shared) {
        return Err(("adb_unavailable", "adb devices failed".to_string()));
    }
    reap_capture_locked(shared);
    if shared.lock().unwrap().capture.is_some() {
        return Ok(());
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

    match capture::spawn(&phone.serial, &node, stay_awake, &settings) {
        Ok(child) => {
            shared.lock().unwrap().capture = Some(child);
            publish_capture(
                shared,
                Some(Capture {
                    phone,
                    node,
                    size: settings::output_size(&settings),
                    stay_awake,
                }),
            );
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
}

fn stop_capture_locked(shared: &Shared) {
    let child = { shared.lock().unwrap().capture.take() };
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    publish_capture(shared, None);
}

/// scrcpy dying on its own — the phone unplugged, the process killed — must
/// leave a switch that says off rather than one that claims to be on.
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
        shared.lock().unwrap().capture = None;
        publish_capture(shared, None);
    }
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

fn refresh_adb(shared: &Shared) -> bool {
    let _turn = transition();
    if !probe_adb() {
        let state = State {
            adb_ok: false,
            connection: Connection::NoPhone,
            attached: Vec::new(),
            ..shared.lock().unwrap().state.clone()
        };
        publish(shared, state);
        return false;
    }
    refresh_connection_locked(shared)
}

fn select_phone(shared: &Shared, serial: &str) -> Result<(), (&'static str, String)> {
    let _turn = transition();
    let attached = phones::scan().map_err(|e| ("adb_unavailable", e.to_string()))?;
    if !attached.iter().any(|phone| phone.serial == serial) {
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

    let previous = view.applied.clone();
    let pending = view.pending.clone();
    persist_settings(shared, &phone.serial, pending.clone()).map_err(|error| {
        (
            "settings_not_saved",
            format!("could not save settings: {error}"),
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
    match capture::spawn(
        &phone.serial,
        &capture_state.node,
        capture_state.stay_awake,
        &pending,
    ) {
        Ok(child) => {
            shared.lock().unwrap().capture = Some(child);
            view.applied();
            let state = State {
                capture: Some(Capture {
                    phone,
                    node: capture_state.node,
                    size: settings::output_size(&pending),
                    stay_awake: capture_state.stay_awake,
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
            match capture::spawn(
                &phone.serial,
                &capture_state.node,
                capture_state.stay_awake,
                &previous,
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
    let shared: Shared = Arc::new(Mutex::new(Daemon {
        rev: 1,
        state: State {
            adb_ok,
            ..Default::default()
        },
        clients: HashMap::new(),
        next_client: 0,
        registry: phones::load(&dir),
        state_dir: dir.clone(),
        capture: None,
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
        Some("apply") => match apply_settings(shared) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("discard") => match discard_settings(shared) {
            Ok(()) => ok_message(&id, current_rev()),
            Err((code, message)) => error_message(&id, current_rev(), code, &message),
        },
        Some("start") => match start_capture(
            shared,
            request
                .get("stay_awake")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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

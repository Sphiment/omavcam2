//! The daemon: one long-lived process that owns the state and pushes it.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::phones::{self, Registry};
use crate::protocol::{
    error_message, ok_message, socket_path, state_message, Connection, State, MAX_MESSAGE, VERSION,
};

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
}

type Shared = Arc<Mutex<Daemon>>;

/// Make this the state and push the whole thing to everyone. Returns the
/// revision it is now at — unchanged if the state was already this, so the
/// revision counts changes rather than requests.
///
// ponytail: writes happen under the state lock, so one wedged client stalls
// the daemon. A per-client queue thread if that ever bites.
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
    matches!(Command::new("adb").arg("start-server").status(), Ok(s) if s.success())
}

/// Work out the connection from what adb reports, publish it, and connect to
/// the selected phone. Called by the poll thread and by requests that should
/// answer with the state they caused.
///
/// One at a time, so a `select` and a poll landing together do not each
/// publish their own view of the desk. Poisoning is ignored deliberately: this
/// serialises work, and a panic in one pass is no reason for phone handling to
/// stop working for the life of the daemon. One daemon per process, so the
/// lock can be a static.
fn refresh_connection(shared: &Shared) {
    static PASS: Mutex<()> = Mutex::new(());
    let _turn = PASS.lock().unwrap_or_else(PoisonError::into_inner);

    let attached = phones::scan();
    let (connection, remember) = {
        let inner = shared.lock().unwrap();
        phones::resolve(&attached, inner.registry.selected.as_deref())
    };
    if let Some(serial) = remember {
        remember_selection(shared, serial);
    }

    // A phone already connected and still attached is not asked again; the
    // scan said everything there is to know.
    let connection = match (&connection, &shared.lock().unwrap().state.connection) {
        (Connection::Connecting { phone }, Connection::Connected { phone: same })
            if phone == same =>
        {
            Connection::Connected {
                phone: phone.clone(),
            }
        }
        _ => connection,
    };
    publish_connection(shared, connection.clone());

    if let Connection::Connecting { phone } = connection {
        if phones::connect(&phone.serial) {
            publish_connection(shared, Connection::Connected { phone });
        }
        // If it did not answer we stay in Connecting and the next pass tries
        // again — a phone that is booting or half-asleep needs no error.
    }
}

fn publish_connection(shared: &Shared, connection: Connection) {
    let state = State {
        connection,
        ..shared.lock().unwrap().state.clone()
    };
    publish(shared, state);
}

/// Remember which phone is in use, so choosing is a one-time act.
fn remember_selection(shared: &Shared, serial: String) {
    let mut inner = shared.lock().unwrap();
    if inner.registry.selected.as_deref() == Some(serial.as_str()) {
        return;
    }
    inner.registry.selected = Some(serial);
    if let Err(e) = phones::save(&inner.state_dir, &inner.registry) {
        // Worth continuing: the selection holds until the daemon restarts.
        eprintln!("omavcam: could not remember the selected phone: {e}");
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
    let _ = fs::remove_file(&path);
    UnixListener::bind(path)
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
    let shared: Shared = Arc::new(Mutex::new(Daemon {
        rev: 1,
        state: State {
            adb_ok: probe_adb(),
            ..Default::default()
        },
        clients: HashMap::new(),
        next_client: 0,
        registry: phones::load(&dir),
        state_dir: dir.clone(),
    }));
    eprintln!("omavcam: listening, state dir {}", dir.display());

    // The world changes without anyone asking: a phone is plugged in, or
    // unplugged mid-call. Nothing polls the daemon, so the daemon polls adb.
    let watcher = Arc::clone(&shared);
    thread::spawn(move || loop {
        refresh_connection(&watcher);
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
        if !line.ends_with(b"\n") {
            if line.len() > MAX_MESSAGE {
                let rev = shared.lock().unwrap().rev;
                let msg = error_message(
                    &Value::Null,
                    rev,
                    "message_too_large",
                    &format!("request exceeds {MAX_MESSAGE} bytes"),
                );
                let _ = write_line(client, &msg);
            }
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
            let adb_ok = probe_adb();
            let state = State {
                adb_ok,
                ..shared.lock().unwrap().state.clone()
            };
            publish(shared, state);
            refresh_connection(shared);
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
            let attached = phones::scan();
            if !attached.iter().any(|a| a.serial == serial) {
                // Say what *is* attached: the caller cannot always see it. A
                // remembered phone that is unplugged reports `NoPhone` rather
                // than offering whatever else is on the desk, so this error is
                // how someone finds the serial of the other one.
                let serials: Vec<&str> = attached.iter().map(|a| a.serial.as_str()).collect();
                let known = match serials.is_empty() {
                    true => "none".to_string(),
                    false => serials.join(", "),
                };
                let message = match serial.is_empty() {
                    true => format!("name a phone; attached: {known}"),
                    false => format!("no phone {serial:?} is attached; attached: {known}"),
                };
                return error_message(&id, current_rev(), "no_such_phone", &message);
            }
            remember_selection(shared, serial.to_string());
            // Answer with the state the choice produced, not the one before it.
            refresh_connection(shared);
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

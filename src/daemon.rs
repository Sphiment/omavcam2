//! The daemon: one long-lived process that owns the state and pushes it.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

use crate::protocol::{
    error_message, ok_message, socket_path, state_message, State, MAX_MESSAGE, VERSION,
};

struct Inner {
    rev: u64,
    state: State,
    clients: HashMap<u64, Arc<Mutex<UnixStream>>>,
    next_client: u64,
}

type Shared = Arc<Mutex<Inner>>;

/// Replace the state, bump the revision, push the whole thing to everyone.
/// Returns the new revision.
///
// ponytail: writes happen under the state lock, so one wedged client stalls
// the daemon. A per-client queue thread if that ever bites.
fn publish(shared: &Shared, state: State) -> u64 {
    let mut inner = shared.lock().unwrap();
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

/// The daemon's only subprocess so far. Later tickets target every adb call
/// with `-s <serial>`; `start-server` is the one that has no device to name.
fn probe_adb() -> bool {
    matches!(Command::new("adb").arg("start-server").status(), Ok(s) if s.success())
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
    let shared: Shared = Arc::new(Mutex::new(Inner {
        rev: 1,
        state: State {
            adb_ok: probe_adb(),
            ..Default::default()
        },
        clients: HashMap::new(),
        next_client: 0,
    }));
    eprintln!("omavcam: listening, state dir {}", dir.display());

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
            let rev = publish(shared, state);
            if adb_ok {
                ok_message(&id, rev)
            } else {
                error_message(&id, rev, "adb_unavailable", "adb start-server failed")
            }
        }
        other => error_message(
            &id,
            current_rev(),
            "unknown_request",
            &format!("no such request kind: {other:?}"),
        ),
    }
}

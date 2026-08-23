mod daemon;
mod protocol;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

use protocol::{socket_path, State, VERSION};

/// Long enough that a busy daemon is never cut off, short enough that a broken
/// one leaves a message rather than a hung terminal. Under socket activation
/// `connect()` succeeds whether or not the daemon can actually start, so this
/// is the timeout that catches a daemon failing to come up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("daemon") => match daemon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omavcam: {e}");
                ExitCode::from(2)
            }
        },
        Some(kind @ ("status" | "refresh")) => request(kind),
        other => {
            if let Some(other) = other {
                eprintln!("omavcam: unknown command: {other}");
            }
            eprintln!("usage: omavcam [status|refresh|daemon]");
            ExitCode::from(2)
        }
    }
}

/// Send one request, print the state it produced, and exit on whether the
/// daemon said it succeeded.
fn request(kind: &str) -> ExitCode {
    let (state, response) = match call(kind) {
        Ok(pair) => pair,
        Err(e) => {
            // The socket unit is what makes the daemon appear on demand, so
            // both a missing socket and a daemon that will not start are
            // installation problems, not "the daemon isn't running".
            eprintln!(
                "omavcam: no reply from {}: {e}\n\
                 the daemon is socket-activated; check: systemctl --user status omavcam.socket omavcam.service",
                socket_path().display()
            );
            return ExitCode::from(2);
        }
    };

    println!("{}", render(&state));
    if response["ok"] == Value::Bool(true) {
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "omavcam: {}: {}",
        response["error"]["code"].as_str().unwrap_or("error"),
        response["error"]["message"].as_str().unwrap_or(""),
    );
    ExitCode::FAILURE
}

/// What `omavcam status` prints.
fn render(state: &State) -> String {
    let field = |v: &Option<Value>| match v {
        None => "none".to_string(),
        Some(v) => v.to_string(),
    };
    format!(
        "adb: {}\nphone: {}\ncapture: {}",
        if state.adb_ok { "ok" } else { "unavailable" },
        field(&state.phone),
        field(&state.capture),
    )
}

/// Returns the state at or after the revision the response names, so what we
/// print is the state that reflects the request rather than whatever arrived
/// first.
fn call(kind: &str) -> std::io::Result<(State, Value)> {
    let stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let id = "1";
    writeln!(&stream, "{}", json!({"v": VERSION, "id": id, "kind": kind}))?;

    let mut latest: Option<(u64, State)> = None;
    let mut response: Option<Value> = None;
    let mut line = String::new();
    loop {
        if let (Some((rev, state)), Some(r)) = (&latest, &response) {
            if *rev >= r["rev"].as_u64().unwrap_or(0) {
                return Ok((state.clone(), r.clone()));
            }
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "daemon closed the connection",
            ));
        }
        let msg: Value = serde_json::from_str(&line)?;
        match msg["type"].as_str() {
            Some("state") => {
                let state = serde_json::from_value(msg["state"].clone())?;
                latest = Some((msg["rev"].as_u64().unwrap_or(0), state));
            }
            Some("response") if msg["id"] == json!(id) => response = Some(msg),
            _ => {}
        }
    }
}

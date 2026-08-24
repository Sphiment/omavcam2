mod capture;
mod daemon;
mod phones;
mod protocol;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

use protocol::{socket_path, Connection, Phone, State, VERSION};

/// Long enough that a busy daemon is never cut off, short enough that a broken
/// one leaves a message rather than a hung terminal. Under socket activation
/// `connect()` succeeds whether or not the daemon can actually start, so this
/// is the timeout that catches a daemon failing to come up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("daemon"), _) => match daemon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omavcam: {e}");
                ExitCode::from(2)
            }
        },
        (Some(kind @ ("status" | "refresh" | "start" | "stop")), _) => request(kind, json!({})),
        // A bare `select` goes to the daemon too: its error is what lists the
        // phones that are attached.
        (Some("select"), serial) => {
            request("select", json!({"serial": serial.unwrap_or_default()}))
        }
        (other, _) => {
            if let Some(other) = other {
                eprintln!("omavcam: unknown command: {other}");
            }
            eprintln!("usage: omavcam [status|refresh|select <serial>|start|stop|daemon]");
            ExitCode::from(2)
        }
    }
}

/// Send one request, print the state it produced, and exit on whether the
/// daemon said it succeeded.
fn request(kind: &str, args: Value) -> ExitCode {
    let (state, response) = match call(kind, args) {
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

/// What `omavcam status` prints. Each connection state gets the advice that
/// belongs to it: which phone to look at, or which command to run next.
fn render(state: &State) -> String {
    let phone = |p: &Phone| format!("{} ({})", p.name, p.serial);
    let connection = match &state.connection {
        Connection::NoPhone => "phone: none".to_string(),
        Connection::Unselected { available } => {
            let mut lines = format!("phone: {} attached, none selected\n", available.len());
            for p in available {
                lines += &format!("  {}\n", phone(p));
            }
            lines + "choose one with: omavcam select <serial>"
        }
        Connection::Unauthorised { phone: p } => format!(
            "phone: {} — unauthorised\naccept the debugging prompt on the phone",
            phone(p)
        ),
        Connection::Connecting { phone: p } => format!("phone: {} — connecting", phone(p)),
        Connection::Connected { phone: p } => format!("phone: {} — connected", phone(p)),
    };
    format!(
        "adb: {}\n{connection}\ncapture: {}",
        if state.adb_ok { "ok" } else { "unavailable" },
        match &state.capture {
            None => "none".to_string(),
            Some(c) => format!("{} from {} to {}", c.size, phone(&c.phone), c.node),
        },
    )
}

/// Returns the state at or after the revision the response names, so what we
/// print is the state that reflects the request rather than whatever arrived
/// first.
fn call(kind: &str, args: Value) -> std::io::Result<(State, Value)> {
    let stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let id = "1";
    let mut request = json!({"v": VERSION, "id": id, "kind": kind});
    for (key, value) in args.as_object().into_iter().flatten() {
        request[key] = value.clone();
    }
    writeln!(&stream, "{request}")?;

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

mod capture;
mod command;
mod daemon;
mod phones;
mod protocol;
mod settings;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

use protocol::{socket_path, Connection, PairingFailure, Phone, State, Transport, VERSION};

/// Long enough that a busy daemon is never cut off, short enough that a broken
/// one leaves a message rather than a hung terminal. Under socket activation
/// `connect()` succeeds whether or not the daemon can actually start, so this
/// is the timeout that catches a daemon failing to come up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command] if command == "daemon" => match daemon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omavcam: {e}");
                ExitCode::from(2)
            }
        },
        [kind] if matches!(kind.as_str(), "status" | "refresh" | "start" | "stop") => {
            request(kind, json!({}))
        }
        [kind] if matches!(kind.as_str(), "apply" | "discard") => request(kind, json!({})),
        [command, setting, value] if command == "set" => match setting_value(setting, value) {
            Ok(value) => request(
                "set",
                json!({"setting": setting.replace('-', "_"), "value": value}),
            ),
            Err(message) => {
                eprintln!("omavcam: invalid setting: {message}");
                ExitCode::from(2)
            }
        },
        [command] if command == "phones" => request("status", json!({})),
        [command] if command == "pair" => request("begin_pairing", json!({})),
        [command, pair_address, code, connect_address] if command == "pair" => request(
            "pair",
            json!({
                "pair_address": pair_address,
                "code": code,
                "connect_address": connect_address,
            }),
        ),
        [command, serial, connect_address] if command == "connect" => request(
            "connect",
            json!({"serial": serial, "connect_address": connect_address}),
        ),
        [command, serial] if command == "forget" => request("forget", json!({"serial": serial})),
        // Opt-in, because it changes a setting on someone's phone.
        [command, flag] if command == "start" && flag == "--stay-awake" => {
            request("start", json!({"stay_awake": true}))
        }
        // A bare `select` goes to the daemon too: its error is what lists the
        // phones that are attached.
        [command] if command == "select" => request("select", json!({"serial": ""})),
        [command, serial] if command == "select" => request("select", json!({"serial": serial})),
        other => {
            if let Some(command) = other.first() {
                eprintln!("omavcam: invalid command: {command}");
            }
            eprintln!(
                "usage: omavcam [status|phones|refresh|select <serial>|pair [<pair-address> <code> <connect-address>]|connect <serial> <connect-address>|forget <serial>|start [--stay-awake]|stop|set <lens|resolution|frame-rate|aspect-ratio|zoom|crop> <value>|apply|discard|daemon]"
            );
            ExitCode::from(2)
        }
    }
}

fn setting_value(setting: &str, value: &str) -> Result<Value, String> {
    match setting {
        "frame-rate" => value
            .parse::<u32>()
            .map(Value::from)
            .map_err(|_| "frame-rate must be a positive integer".to_string()),
        "zoom" => value
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| "zoom must be a number".to_string()),
        "crop" if value == "none" => Ok(Value::Null),
        "crop" => {
            let numbers = value
                .split(':')
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "crop must be normalized x:y:width:height or none".to_string())?;
            match numbers.as_slice() {
                [x, y, width, height] => {
                    Ok(json!({"x": x, "y": y, "width": width, "height": height}))
                }
                _ => Err("crop must be normalized x:y:width:height or none".to_string()),
            }
        }
        "frame_rate" | "aspect_ratio" => Err(format!("use {}", setting.replace('_', "-"))),
        "lens" | "resolution" | "aspect-ratio" => Ok(Value::String(value.to_string())),
        other => Err(format!("no such setting {other:?}")),
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
    let code = response["error"]["code"].as_str().unwrap_or("error");
    eprintln!(
        "omavcam: {code}: {}",
        response["error"]["message"].as_str().unwrap_or(""),
    );
    // Not part of the error: the daemon cannot tell why scrcpy gave up, and
    // both tips are things only the person holding the phone can act on. The
    // phone does not need unlocking — measured, both lenses capture fine with
    // the screen off and the lockscreen up, and a running capture survives the
    // phone being locked. What it does not survive is being *face*-unlocked:
    // the recognition service takes a camera for itself, and the per-system
    // limit means it blocks any lens, not only the one it wants.
    if code == "capture_failed" {
        eprintln!(
            "tip: another app on the phone may be holding the camera — close it and try again; \
             face unlock is one of them, so unlock with a PIN while a capture is running\n\
             tip: `omavcam start --stay-awake` keeps the screen on while the phone is plugged \
             in, so the screen turning off mid-capture cannot disconnect the camera"
        );
    }
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
        Connection::NeedsPairing => "phone: wireless pairing needed\nopen Developer options → Wireless debugging → Pair device with pairing code; the pairing address beside the six-digit code and the connect address on the main screen are different\nthen run: omavcam pair <pair-address> <code> <connect-address>".to_string(),
        Connection::PairingFailed { reason } => format!(
            "phone: wireless pairing failed — {}",
            match reason {
                PairingFailure::WrongCode => "wrong code",
                PairingFailure::WrongAddress => "wrong pairing address",
                PairingFailure::Unreachable => "unreachable; check both devices are on the same network",
            }
        ),
        Connection::Unreachable {
            phone: p,
            connect_address,
        } => format!(
            "phone: {} — unreachable at {connect_address}\nwake it and check both devices are on the same network; if the phone rebooted or wireless debugging was toggled, re-read the connect address from its main wireless debugging screen — do not pair again",
            phone(p)
        ),
    };
    let known = if state.known.is_empty() {
        "known phones: none".to_string()
    } else {
        let mut lines = "known phones:".to_string();
        for known in &state.known {
            lines += &format!(
                "\n  {} — {}",
                phone(&known.phone),
                match known.transport {
                    Transport::Wired => "wired",
                    Transport::Wireless => "wireless",
                }
            );
        }
        lines
    };
    let settings = state.settings.as_ref().map_or_else(
        || "settings: unavailable".to_string(),
        |settings| {
            let pending = &settings.pending;
            format!(
                "settings: lens {}, {}, {}fps, {}, zoom {}{}{}",
                pending.lens,
                pending.resolution,
                pending.frame_rate,
                pending.aspect_ratio,
                pending.zoom,
                if settings.has_pending_changes {
                    " — pending Apply"
                } else {
                    " — applied"
                },
                settings
                    .rejected
                    .as_ref()
                    .map(|message| format!("\nrejected: {message}"))
                    .unwrap_or_default()
            )
        },
    );
    format!(
        "adb: {}\n{connection}\n{known}\ncapture: {}\n{settings}",
        if state.adb_ok { "ok" } else { "unavailable" },
        match &state.capture {
            None => "none".to_string(),
            Some(c) => format!(
                "{} from {} to {}{}",
                c.size,
                phone(&c.phone),
                c.node,
                if c.stay_awake {
                    " — staying awake"
                } else {
                    ""
                }
            ),
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
        let peer_version = msg.get("v").and_then(Value::as_u64);
        if peer_version != Some(VERSION as u64) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("daemon speaks protocol {peer_version:?}, client expects {VERSION}"),
            ));
        }
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

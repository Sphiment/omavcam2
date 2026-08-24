//! The wire protocol: line-delimited JSON over a unix socket.
//!
//! Two message types travel from the daemon to a client:
//!
//! ```text
//! {"type":"state","v":1,"rev":4,"state":{...}}
//! {"type":"response","v":1,"id":"7","rev":4,"ok":true}
//! {"type":"response","v":1,"id":"7","rev":4,"ok":false,"error":{"code":"...","message":"..."}}
//! ```
//!
//! and one from a client to the daemon:
//!
//! ```text
//! {"v":1,"id":"7","kind":"status"}
//! {"v":1,"id":"8","kind":"select","serial":"39281FDJH0031T"}
//! {"v":1,"id":"9","kind":"start"}
//! ```
//!
//! The daemon pushes the *whole* state, unprompted, to every connected client
//! whenever it changes, and once on connect. Nothing polls. Every response
//! carries the revision at which the request's effect is visible, so a client
//! knows which pushed state reflects its request.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Bumped whenever the shape below changes incompatibly. A client sending
/// anything else is rejected with an error rather than misparsed.
pub const VERSION: u32 = 1;

/// Longest accepted request line, newline included. A client cannot make the
/// daemon allocate past this.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// Everything the daemon knows. Pushed whole, never as a delta, so a client
/// that reconnects after a daemon restart is correct immediately.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Whether `adb start-server` last succeeded.
    pub adb_ok: bool,
    pub connection: Connection,
    /// The running capture, or nothing. There is no third state: a capture
    /// that has stopped, however it stopped, is a capture that is not there.
    pub capture: Option<Capture>,
}

/// One running stream from a phone into the virtual camera. Everything about
/// it is fixed at launch — changing any of it means replacing the capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub phone: Phone,
    /// The virtual camera being written, found by its `card_label`.
    pub node: String,
    /// The frame size, fixed for this capture's lifetime: a restart at another
    /// size freezes whatever is watching (ADR-0010).
    pub size: String,
}

/// One Android device, identified by the serial adb reports, under a name a
/// person recognises — adb's `model:`, or the serial when adb has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phone {
    pub serial: String,
    pub name: String,
}

/// How far the connection to a phone has got. A phase with its own states and
/// its own advice, not a precondition that holds or doesn't (ADR-0007).
///
/// The wireless states — `NeedsPairing`, `PairingFailed`, `Unreachable` — and
/// `Reconnecting` join this enum in later tickets. On the wire each variant is
/// `{"state":"connected","phone":{...}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    #[default]
    NoPhone,
    /// Several attached and none chosen. omavcam does not pick for you.
    Unselected {
        available: Vec<Phone>,
    },
    /// Selected, but the debugging prompt on the phone was never accepted.
    Unauthorised {
        phone: Phone,
    },
    Connecting {
        phone: Phone,
    },
    Connected {
        phone: Phone,
    },
}

pub fn state_message(rev: u64, state: &State) -> String {
    json!({"type": "state", "v": VERSION, "rev": rev, "state": state}).to_string()
}

pub fn ok_message(id: &Value, rev: u64) -> String {
    json!({"type": "response", "v": VERSION, "id": id, "rev": rev, "ok": true}).to_string()
}

pub fn error_message(id: &Value, rev: u64, code: &str, message: &str) -> String {
    json!({
        "type": "response", "v": VERSION, "id": id, "rev": rev, "ok": false,
        "error": {"code": code, "message": message},
    })
    .to_string()
}

/// `$OMAVCAM_SOCKET`, else the runtime dir. Tests set the override; the socket
/// unit's `ListenStream=%t/omavcam.sock` matches the default.
pub fn socket_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("OMAVCAM_SOCKET") {
        return p.into();
    }
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::Path::new(&run).join("omavcam.sock")
}

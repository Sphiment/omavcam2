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
    // ponytail: `phone` lands in #4 and `capture` in #5. They are on the wire
    // as nulls now so the shape a client parses does not change under it.
    pub phone: Option<Value>,
    pub capture: Option<Value>,
}

impl State {
    /// What `omavcam status` prints.
    pub fn render(&self) -> String {
        format!(
            "adb: {}\nphone: {}\ncapture: {}",
            if self.adb_ok { "ok" } else { "unavailable" },
            if self.phone.is_none() { "none" } else { "?" },
            if self.capture.is_none() { "none" } else { "?" },
        )
    }
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

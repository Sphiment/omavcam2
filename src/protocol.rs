//! The wire protocol: line-delimited JSON over a unix socket.
//!
//! Two message types travel from the daemon to a client:
//!
//! ```text
//! {"type":"state","v":4,"rev":4,"state":{...}}
//! {"type":"response","v":4,"id":"7","rev":4,"ok":true}
//! {"type":"response","v":4,"id":"7","rev":4,"ok":false,"error":{"code":"...","message":"..."}}
//! ```
//!
//! and one from a client to the daemon:
//!
//! ```text
//! {"v":4,"id":"7","kind":"status"}
//! {"v":4,"id":"8","kind":"select","serial":"39281FDJH0031T"}
//! {"v":4,"id":"9","kind":"start"}
//! {"v":4,"id":"10","kind":"set","setting":"zoom","value":2}
//! {"v":4,"id":"11","kind":"apply"}
//! ```
//!
//! The daemon pushes the *whole* state, unprompted, to every connected client
//! whenever it changes, and once on connect. Nothing polls. Every response
//! carries the revision at which the request's effect is visible, so a client
//! knows which pushed state reflects its request.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::settings::SettingsState;

/// Bumped whenever the shape below changes incompatibly. A client sending
/// anything else is rejected with an error rather than misparsed.
pub const VERSION: u32 = 4;

/// Longest accepted request line, newline included. A client cannot make the
/// daemon allocate past this.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// Everything the daemon knows. Pushed whole, never as a delta, so a client
/// that reconnects after a daemon restart is correct immediately.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Whether the latest adb server probe or device scan succeeded.
    pub adb_ok: bool,
    pub connection: Connection,
    /// The requested capture, retained while its writer reconnects. The
    /// connection phase says whether it is currently feeding frames.
    pub capture: Option<Capture>,
    /// Every phone adb can see, whatever phase the connection is in. A fact
    /// about the world rather than a property of one connection state, which
    /// is what lets a client offer the choice at a moment when omavcam is not
    /// asking for one.
    #[serde(default)]
    pub attached: Vec<Attached>,
    /// Camera capabilities and the selected phone's applied and pending
    /// settings. Absent until a connected phone has answered scrcpy's probe.
    #[serde(default)]
    pub settings: Option<SettingsState>,
    /// Every phone remembered in the one wired/wireless registry.
    #[serde(default)]
    pub known: Vec<KnownPhone>,
    /// The compositor values actually applied to the preview. Clients compare
    /// their live theme with these rather than assuming a one-shot sync held.
    #[serde(default)]
    pub preview_style: PreviewStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewStyle {
    pub rounding: u64,
    pub border_size: u64,
}

impl Default for PreviewStyle {
    fn default() -> Self {
        Self {
            rounding: 0,
            border_size: 1,
        }
    }
}

/// One phone adb reports, and whether adb will talk to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attached {
    pub phone: Phone,
    /// False while the debugging prompt has not been accepted. Such a phone is
    /// still listed: hiding it makes a phone that needs one tap look like a
    /// phone that is not plugged in.
    pub authorised: bool,
}

/// One requested stream from a phone into the virtual camera. Everything about
/// it is fixed at launch — changing any of it means replacing the capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub phone: Phone,
    /// The virtual camera being written, found by its `card_label`.
    pub node: String,
    /// The frame size, fixed for this capture's lifetime: a restart at another
    /// size freezes whatever is watching (ADR-0010).
    pub size: String,
    /// Kept in protocol v2 for clients that already read it. Preview captures
    /// require `--no-control`, so new captures always report false: scrcpy
    /// refuses `--stay-awake` with that flag.
    pub stay_awake: bool,
    /// Whether scrcpy's own window is on-screen. Hiding moves that same
    /// window; it never replaces the capture.
    #[serde(default)]
    pub preview: bool,
}

/// One Android device, identified by the serial adb reports, under a name a
/// person recognises — adb's `model:`, or the serial when adb has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phone {
    pub serial: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Wired,
    Wireless,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownPhone {
    /// Durable identity for per-phone settings. A provisional wireless entry
    /// starts with its endpoint here; learning the device ID replaces that
    /// value while migrating settings, after which port changes leave it alone.
    #[serde(default)]
    pub id: String,
    pub phone: Phone,
    pub transport: Transport,
    /// Wireless debugging's connect endpoint. Pairing uses a separate,
    /// transient endpoint and is deliberately never persisted here.
    pub connect_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingFailure {
    WrongCode,
    WrongAddress,
    Unreachable,
}

/// How far the connection to a phone has got. A phase with its own states and
/// its own advice, not a precondition that holds or doesn't (ADR-0007).
///
/// On the wire each variant is `{"state":"connected","phone":{...}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    #[default]
    NoPhone,
    /// Several attached and none chosen. omavcam does not pick for you.
    ///
    /// `available` is the same phones as the state's `attached`, and is kept
    /// only because removing it would break every client that reads it — the
    /// one thing the protocol version exists to prevent. It goes when the
    /// version next moves.
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
    /// A logical capture still owns the virtual camera while its selected
    /// phone or writer is being recovered.
    Reconnecting {
        phone: Phone,
    },
    NeedsPairing,
    PairingFailed {
        reason: PairingFailure,
    },
    Unreachable {
        phone: Phone,
        connect_address: String,
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

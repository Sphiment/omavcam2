//! Which phones adb can see, which one is selected, and what that adds up to.
//!
//! The selection rules are ADR-0007's, and the one that matters is that omavcam
//! never guesses: the second phone on the desk is usually charging, not waiting
//! to be a webcam.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::command;
use crate::protocol::{Connection, Phone};

/// One line of `adb devices -l`: the serial, adb's own word for its state, and
/// the model if adb knows it — an unauthorised phone reports none.
#[derive(Debug, Clone, PartialEq)]
pub struct Attached {
    pub serial: String,
    pub adb_state: String,
    pub name: String,
}

/// The phones adb can see right now. This and `adb start-server` are the only
/// adb calls with no phone to name; every other one is targeted with `-s`.
pub fn scan() -> std::io::Result<Vec<Attached>> {
    let mut process = Command::new("adb");
    process.args(["devices", "-l"]);
    let out = command::output(process)?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "adb devices failed with {}",
            out.status
        )));
    }
    let mut phones = parse(&String::from_utf8_lossy(&out.stdout));
    // adb's order is not a promise, and this list reaches the state, which is
    // compared whole to decide whether anything changed. Unsorted, two phones
    // could swap places between polls and push an identical state to every
    // client while burning a revision for it.
    phones.sort_by(|a, b| a.serial.cmp(&b.serial));
    Ok(phones)
}

/// Ask the selected phone directly whether it is there. This is the step that
/// turns `Connecting` into `Connected`, and the first targeted adb call in the
/// project: its answer is the exit status, not the output, so it stays honest
/// even when adb has something chatty to say.
pub fn connect(serial: &str) -> bool {
    let mut process = Command::new("adb");
    process.args(["-s", serial, "get-state"]);
    matches!(
        command::status(process),
        Ok(status) if status.success()
    )
}

fn parse(output: &str) -> Vec<Attached> {
    output
        .lines()
        // The header and adb's own chatter, dropped by what they say rather
        // than by where they are: the banner goes to stderr on this adb, but
        // one that puts it on stdout would shift the header down a line and
        // "List of devices attached" would parse as a phone named "List".
        .filter(|line| !line.starts_with("List of devices"))
        .filter(|line| !line.starts_with('*')) // "* daemon started successfully"
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?.to_string();
            let adb_state = fields.next()?.to_string();
            let name = fields
                .find_map(|field| field.strip_prefix("model:"))
                .map(|model| model.replace('_', " "))
                .unwrap_or_else(|| serial.clone());
            Some(Attached {
                serial,
                adb_state,
                name,
            })
        })
        .collect()
}

/// What the attached phones mean, given the remembered selection. Returns the
/// connection state and, when a phone was selected by being the only one there,
/// the serial to remember.
///
/// `Connecting` here means "selected, and adb says it is usable" — the daemon
/// confirms it with `connect` before calling it `Connected`.
pub fn resolve(attached: &[Attached], selected: Option<&str>) -> (Connection, Option<String>) {
    if attached.is_empty() {
        return (Connection::NoPhone, None);
    }
    let remembered = selected.and_then(|s| attached.iter().find(|a| a.serial == s));
    let (phone, remember) = match remembered {
        Some(phone) => (phone, false),
        // A phone was chosen and is not here. Another one being attached does
        // not make it the one: silently repointing a webcam at a different room
        // is worse than reporting no phone.
        None if selected.is_some() => return (Connection::NoPhone, None),
        // One phone is the phone. Two and the user picks, because the extra one
        // is usually charging.
        None if attached.len() == 1 => (&attached[0], true),
        None => {
            let available = attached.iter().map(Phone::from).collect();
            return (Connection::Unselected { available }, None);
        }
    };

    let connection = if phone.adb_state == "unauthorized" {
        Connection::Unauthorised {
            phone: Phone::from(phone),
        }
    } else {
        Connection::Connecting {
            phone: Phone::from(phone),
        }
    };
    // Only a phone that answers is worth remembering. Remembering one that has
    // never accepted the debugging prompt would let the charging phone win the
    // next time both are attached.
    let remember = (remember && phone.adb_state == "device").then(|| phone.serial.clone());
    (connection, remember)
}

impl From<&Attached> for Phone {
    fn from(attached: &Attached) -> Phone {
        Phone {
            serial: attached.serial.clone(),
            name: attached.name.clone(),
        }
    }
}

/// What a client is told about an attached phone. `adb_state` does not travel:
/// it is adb's own vocabulary, and the only part of it a client can act on is
/// whether the phone will answer.
impl From<&Attached> for crate::protocol::Attached {
    fn from(attached: &Attached) -> crate::protocol::Attached {
        crate::protocol::Attached {
            phone: Phone::from(attached),
            authorised: attached.adb_state != "unauthorized",
        }
    }
}

/// What omavcam remembers about phones between runs: today the selected one,
/// later the phones wireless pairing knows about. One registry, not two.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub selected: Option<String>,
}

fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("phones.json")
}

/// A missing registry is an empty one. A damaged one is reported before falling
/// back: refusing to start is excessive, but silently forgetting a phone makes
/// a persistence failure look like user error.
pub fn load(state_dir: &Path) -> Registry {
    let path = registry_path(state_dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Registry::default(),
        Err(e) => {
            eprintln!("omavcam: could not read {}: {e}", path.display());
            return Registry::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("omavcam: could not parse {}: {e}", path.display());
        Registry::default()
    })
}

pub fn save(state_dir: &Path, registry: &Registry) -> std::io::Result<()> {
    let path = registry_path(state_dir);
    let temporary = state_dir.join(".phones.json.tmp");
    let text = serde_json::to_vec_pretty(registry)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&text)?;
    file.sync_all()?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached(serial: &str, adb_state: &str) -> Attached {
        Attached {
            serial: serial.into(),
            adb_state: adb_state.into(),
            name: serial.into(),
        }
    }

    #[test]
    fn parses_what_adb_prints() {
        let phones = parse(
            // adb prints its banner before the header, if it prints it here at
            // all — so neither line may be counted as a phone.
            "* daemon not running; starting now at tcp:5037\n\
             * daemon started successfully\n\
             List of devices attached\n\
             39281FDJH0031T\tdevice usb:1-4 product:panther model:Pixel_7 device:panther\n\
             R5CT10ABCDE\tunauthorized usb:1-2 transport_id:2\n\
             \n",
        );
        assert_eq!(phones.len(), 2);
        assert_eq!(phones[0].name, "Pixel 7");
        assert_eq!(phones[0].adb_state, "device");
        // No model to go on, so the serial is the name.
        assert_eq!(phones[1].name, "R5CT10ABCDE");
    }

    #[test]
    fn selects_by_the_rules() {
        let one = [attached("a", "device")];
        let two = [attached("a", "device"), attached("b", "device")];

        assert_eq!(resolve(&[], None).0, Connection::NoPhone);
        // A lone phone is selected, and remembered for when it is not alone.
        assert_eq!(resolve(&one, None).1, Some("a".to_string()));
        assert!(matches!(
            resolve(&two, None).0,
            Connection::Unselected { .. }
        ));
        assert!(matches!(
            resolve(&two, Some("b")).0,
            Connection::Connecting { .. }
        ));
        // Remembered phone gone: no phone, not the other one — and that holds
        // when the other one is the only phone on the desk. A single attached
        // phone is auto-selected only while nothing has been chosen before;
        // after that, choosing again is the user's to do.
        assert_eq!(resolve(&two, Some("z")).0, Connection::NoPhone);
        assert_eq!(resolve(&one, Some("z")).0, Connection::NoPhone);
        assert!(matches!(
            resolve(&[attached("a", "unauthorized")], None).0,
            Connection::Unauthorised { .. }
        ));
        // ...and an unauthorised phone is never what gets remembered.
        assert_eq!(resolve(&[attached("a", "unauthorized")], None).1, None);
    }

    #[test]
    fn the_registry_accepts_fields_added_after_an_older_file_was_written() {
        let registry: Registry = serde_json::from_str("{}").unwrap();
        assert_eq!(registry.selected, None);
    }
}

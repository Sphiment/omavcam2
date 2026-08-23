//! The harness the rest of the project's tests are built on: a real daemon,
//! a temp state dir, and a directory of stub `adb`, `scrcpy` and `modprobe`
//! executables ahead of the real ones on PATH. The stub directory *is* the
//! fake — there is no process-runner trait to inject.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    dir: PathBuf,
    socket: PathBuf,
    log: PathBuf,
    stub_dir: PathBuf,
    bin_dir: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn start() -> Fixture {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omavcam-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let (stub_dir, bin_dir) = (dir.join("stub"), dir.join("bin"));
        fs::create_dir_all(&stub_dir).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();

        let stub = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stub");
        for tool in ["adb", "scrcpy", "modprobe"] {
            fs::copy(&stub, bin_dir.join(tool)).unwrap();
        }

        let mut fixture = Fixture {
            socket: dir.join("omavcam.sock"),
            log: dir.join("argv.log"),
            stub_dir,
            bin_dir,
            daemon: None,
            dir,
        };
        fs::write(&fixture.log, "").unwrap();
        fixture.spawn();
        fixture
    }

    /// Start the daemon the way the socket unit would, minus systemd.
    fn spawn(&mut self) {
        let path = format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        self.daemon = Some(
            Command::new(env!("CARGO_BIN_EXE_omavcam"))
                .arg("daemon")
                .env("PATH", path)
                .env("OMAVCAM_SOCKET", &self.socket)
                .env("OMAVCAM_STATE_DIR", self.dir.join("state"))
                .env("OMAVCAM_STUB_LOG", &self.log)
                .env("OMAVCAM_STUB_DIR", &self.stub_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );

        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(&self.socket).is_err() {
            assert!(Instant::now() < deadline, "daemon never took the socket");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn restart(&mut self) {
        self.stop();
        self.spawn();
    }

    fn stop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    fn connect(&self) -> Client {
        let stream = UnixStream::connect(&self.socket).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        Client {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            next_id: 0,
            last_state: Value::Null,
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_omavcam"))
            .args(args)
            .env("PATH", path)
            .env("OMAVCAM_SOCKET", &self.socket)
            .output()
            .unwrap()
    }

    /// What the stubs were actually called with, one line per call.
    fn argv(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn script_exit(&self, tool: &str, code: i32) {
        fs::write(self.stub_dir.join(format!("{tool}.code")), code.to_string()).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u32,
    /// The most recent pushed state, so a test that reads past one can still
    /// see it. The daemon publishes before it responds.
    last_state: Value,
}

impl Client {
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap();
        assert!(n > 0, "daemon closed the connection");
        let msg: Value = serde_json::from_str(&line).unwrap();
        if msg["type"] == json!("state") {
            self.last_state = msg.clone();
        }
        msg
    }

    fn recv_state(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == json!("state") {
                return msg;
            }
        }
    }

    fn send_raw(&mut self, raw: &str) {
        writeln!(self.stream, "{raw}").unwrap();
    }

    /// Sends a request and returns its response, ignoring states pushed on the
    /// way past.
    fn request(&mut self, kind: &str) -> Value {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.send_raw(&json!({"v": 1, "id": id, "kind": kind}).to_string());
        loop {
            let msg = self.recv();
            if msg["type"] == json!("response") {
                assert_eq!(msg["id"], json!(id), "response carries its request id");
                return msg;
            }
        }
    }
}

#[test]
fn status_prints_the_state_and_exits_zero() {
    let f = Fixture::start();
    let out = f.cli(&["status"]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(out.status.success(), "status failed: {stdout}");
    assert!(stdout.contains("phone: none"), "{stdout}");
    assert!(stdout.contains("capture: none"), "{stdout}");
}

#[test]
fn the_harness_records_argv() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();
    client.request("refresh");

    assert!(
        f.argv().iter().any(|line| line == "adb start-server"),
        "expected an adb call, got {:?}",
        f.argv()
    );
}

#[test]
fn a_second_client_sees_the_same_state() {
    let f = Fixture::start();
    let mut first = f.connect();
    let mut second = f.connect();

    let (a, b) = (first.recv_state(), second.recv_state());
    assert_eq!(a["state"], b["state"]);
    assert_eq!(a["rev"], b["rev"]);
}

#[test]
fn every_client_is_pushed_the_whole_state_when_it_changes() {
    let f = Fixture::start();
    let mut watcher = f.connect();
    let mut actor = f.connect();
    let before = watcher.recv_state();
    actor.recv_state();

    actor.request("refresh");

    // The watcher asked for nothing and polls nothing, yet gets the new state.
    let after = watcher.recv_state();
    assert!(
        after["rev"].as_u64().unwrap() > before["rev"].as_u64().unwrap(),
        "revision must increase: {before} then {after}"
    );
    assert!(after["state"]["phone"].is_null(), "state is pushed whole");
    assert_eq!(after["v"], json!(1));
}

#[test]
fn a_response_names_the_revision_that_reflects_it() {
    let f = Fixture::start();
    let mut client = f.connect();
    let initial = client.recv_state();

    let response = client.request("refresh");
    assert_eq!(response["ok"], json!(true));

    // The state carrying the request's effect is already in hand by the time
    // the response names its revision, so nothing has to be asked for twice.
    assert_eq!(response["rev"], client.last_state["rev"]);
    assert!(response["rev"].as_u64().unwrap() > initial["rev"].as_u64().unwrap());
}

#[test]
fn reconnecting_after_a_daemon_restart_needs_no_resync() {
    let mut f = Fixture::start();
    let mut before = f.connect();
    before.recv_state();

    f.restart();

    // A fresh connection is handed the whole state unprompted; there is no
    // resync request to send.
    let mut after = f.connect();
    let state = after.recv_state();
    assert!(state["state"].is_object());
    assert!(state["state"]["capture"].is_null());
}

#[test]
fn an_unknown_protocol_version_is_rejected_clearly() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    client.send_raw(&json!({"v": 99, "id": "x", "kind": "status"}).to_string());
    let response = client.recv();

    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["id"], json!("x"), "the id still comes back");
    assert_eq!(response["error"]["code"], json!("unsupported_version"));
}

#[test]
fn an_unknown_request_is_an_error_not_a_hang() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    let response = client.request("teleport");
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("unknown_request"));
}

#[test]
fn a_message_past_the_bound_is_rejected() {
    let f = Fixture::start();
    let mut client = f.connect();
    client.recv_state();

    let huge = "x".repeat(200 * 1024);
    client.send_raw(&json!({"v": 1, "id": "x", "kind": huge}).to_string());

    let response = client.recv();
    assert_eq!(response["error"]["code"], json!("message_too_large"));
}

#[test]
fn the_cli_exit_code_reflects_a_failed_request() {
    let f = Fixture::start();
    f.script_exit("adb", 1);

    let out = f.cli(&["refresh"]);
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("adb_unavailable"), "{stderr}");
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("adb: unavailable"),
        "the state that reflects the failed request is still printed"
    );
}

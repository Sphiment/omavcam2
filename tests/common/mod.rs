//! The harness the rest of the project's tests are built on: a real daemon, a
//! temp state dir, a fake `/sys/class/video4linux` to look the virtual camera
//! up in, and a directory of stub `adb`, `scrcpy`, `v4l2-ctl` and `modprobe`
//! executables ahead of the real ones on PATH. The stub directory *is* the
//! fake — there is no process-runner trait to inject.

// Not every test file uses every corner of the harness.
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: u32 = 2;

pub struct Fixture {
    dir: PathBuf,
    poll_ms: String,
    socket: PathBuf,
    log: PathBuf,
    stub_dir: PathBuf,
    bin_dir: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    /// A daemon already running, which is what systemd leaves behind once
    /// something has connected. Most tests want this.
    pub fn start() -> Fixture {
        let mut fixture = Fixture::new();
        fixture.spawn();
        fixture
    }

    /// A daemon that polls adb once and then effectively never again, so
    /// nothing is noticed behind the test's back and a request has to find out
    /// for itself.
    pub fn slow_poll() -> Fixture {
        let mut fixture = Fixture::new();
        fixture.poll_ms = "3600000".to_string();
        fixture.spawn();
        fixture
    }

    /// Nothing running, and the daemon handed to `systemd-socket-activate` —
    /// a socket unit minus the unit file. It binds the socket, and on the
    /// first connection execs the daemon with the listener on fd 3.
    pub fn activated() -> Fixture {
        let mut fixture = Fixture::new();
        fixture.activate();
        fixture
    }

    /// Socket activation with the desk already populated, which exercises the
    /// first state a freshly started daemon gives its triggering client.
    pub fn activated_with_devices(attached: &[(&str, &str, Option<&str>)]) -> Fixture {
        let mut fixture = Fixture::new();
        fixture.script_devices(attached);
        fixture.activate();
        fixture
    }

    fn activate(&mut self) {
        // systemd-socket-activate hands the child a curated environment — PATH
        // survives, anything of ours does not — so the daemon's own variables
        // have to go through --setenv.
        self.daemon = Some(
            self.daemon_command("systemd-socket-activate")
                .args(["-l", self.socket.to_str().unwrap()])
                .args([
                    format!(
                        "--setenv=OMAVCAM_STATE_DIR={}",
                        self.dir.join("state").display()
                    ),
                    format!("--setenv=OMAVCAM_STUB_LOG={}", self.log.display()),
                    format!("--setenv=OMAVCAM_STUB_DIR={}", self.stub_dir.display()),
                    format!(
                        "--setenv=OMAVCAM_V4L2_DIR={}",
                        self.dir.join("sys").display()
                    ),
                    "--setenv=OMAVCAM_STARTUP_MS=500".to_string(),
                    "--setenv=OMAVCAM_COMMAND_MS=100".to_string(),
                    format!("--setenv=OMAVCAM_POLL_MS={}", self.poll_ms),
                ])
                .args([env!("CARGO_BIN_EXE_omavcam"), "daemon"])
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + TIMEOUT;
        while !self.socket.exists() {
            assert!(Instant::now() < deadline, "socket never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn new() -> Fixture {
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
        for tool in ["adb", "scrcpy", "v4l2-ctl", "modprobe"] {
            fs::copy(&stub, bin_dir.join(tool)).unwrap();
        }

        let fixture = Fixture {
            poll_ms: "25".to_string(),
            socket: dir.join("omavcam.sock"),
            log: dir.join("argv.log"),
            stub_dir,
            bin_dir,
            daemon: None,
            dir,
        };
        fs::write(&fixture.log, "").unwrap();
        fixture.script_virtual_camera(Some("video42"));
        fixture
    }

    /// Start the daemon the way the socket unit would, minus systemd.
    pub fn spawn(&mut self) {
        self.daemon = Some(
            self.daemon_command(env!("CARGO_BIN_EXE_omavcam"))
                .arg("daemon")
                .spawn()
                .unwrap(),
        );

        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(&self.socket).is_err() {
            assert!(Instant::now() < deadline, "daemon never took the socket");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The daemon's environment: the stubs ahead of the real tools, a temp
    /// state dir, and its own process group so the whole thing can be killed
    /// even when something else exec'd it.
    pub fn daemon_command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("PATH", self.path())
            .env("OMAVCAM_SOCKET", &self.socket)
            .env("OMAVCAM_STATE_DIR", self.dir.join("state"))
            .env("OMAVCAM_STUB_LOG", &self.log)
            .env("OMAVCAM_STUB_DIR", &self.stub_dir)
            .env("OMAVCAM_V4L2_DIR", self.dir.join("sys"))
            .env("OMAVCAM_STARTUP_MS", "500")
            .env("OMAVCAM_COMMAND_MS", "100")
            // The daemon polls adb for attached phones; tests should not wait a
            // real second for a plug or an unplug to be noticed.
            .env("OMAVCAM_POLL_MS", &self.poll_ms)
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    pub fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    pub fn restart(&mut self) {
        self.stop();
        self.spawn();
    }

    pub fn stop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            // Under activation the daemon is a grandchild, so take the group.
            let _ = Command::new("kill")
                .arg("--")
                .arg(format!("-{}", daemon.id()))
                .status();
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    pub fn connect(&self) -> Client {
        let stream = UnixStream::connect(&self.socket).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        Client {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            next_id: 0,
            last_state: Value::Null,
        }
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omavcam"))
            .args(args)
            .env("PATH", self.path())
            .env("OMAVCAM_SOCKET", &self.socket)
            .output()
            .unwrap()
    }

    /// What the stubs were actually called with, one line per call.
    pub fn argv(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Keeps the stub running once it is launched, the way scrcpy keeps
    /// running for the life of a capture.
    pub fn script_hold(&self, tool: &str) {
        fs::write(self.stub_dir.join(format!("{tool}.hold")), "").unwrap();
    }

    /// Lets a held stub exit on its own, which is what scrcpy dying without
    /// being asked to looks like from here.
    pub fn script_release(&self, tool: &str) {
        fs::remove_file(self.stub_dir.join(format!("{tool}.hold"))).unwrap();
    }

    /// The fake `/sys/class/video4linux` the daemon looks the virtual camera up
    /// in: a directory per node, each holding its `card_label` in `name`. The
    /// laptop's own webcam is always there, so a lookup that ignores the label
    /// finds the wrong node. `None` is the module not loaded at all.
    pub fn script_virtual_camera(&self, node: Option<&str>) {
        self.script_virtual_cameras(node.as_slice());
    }

    pub fn script_virtual_cameras(&self, nodes: &[&str]) {
        let sys = self.dir.join("sys");
        let _ = fs::remove_dir_all(&sys);
        fs::create_dir_all(sys.join("video0")).unwrap();
        fs::write(sys.join("video0/name"), "HP Wide Vision HD Camera\n").unwrap();
        for node in nodes {
            fs::create_dir_all(sys.join(node)).unwrap();
            fs::write(sys.join(node).join("name"), "omavcam\n").unwrap();
        }
    }

    /// The calls to one tool, once there are at least `n` of them. A stub
    /// records itself as it starts, which is a moment after the daemon spawned
    /// it and told everyone.
    pub fn await_argv(&self, tool: &str, n: usize) -> Vec<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let calls: Vec<String> = self
                .argv()
                .into_iter()
                .filter(|line| line.starts_with(&format!("{tool} ")))
                .collect();
            if calls.len() >= n {
                return calls;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {n} {tool} calls; the log holds {:?}",
                self.argv()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn script_exit(&self, tool: &str, code: i32) {
        fs::write(self.stub_dir.join(format!("{tool}.code")), code.to_string()).unwrap();
    }

    pub fn script_exit_for(&self, tool: &str, command: &str, code: i32) {
        fs::write(
            self.stub_dir.join(format!("{tool}.{command}.code")),
            code.to_string(),
        )
        .unwrap();
    }

    /// What `adb devices -l` prints from now on: one `(serial, adb's word for
    /// its state, model)` per attached phone. An unauthorised phone reports no
    /// model, which is why the model is optional here.
    pub fn script_devices(&self, attached: &[(&str, &str, Option<&str>)]) {
        let mut out = String::from("List of devices attached\n");
        for (serial, status, model) in attached {
            out.push_str(&format!("{serial}\t{status} usb:1-4"));
            if let Some(model) = model {
                out.push_str(&format!(" product:x model:{model} device:x"));
            }
            out.push_str(" transport_id:1\n");
        }
        fs::write(self.stub_dir.join("adb.out"), out).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u32,
    /// The most recent pushed state, so a test that reads past one can still
    /// see it. The daemon publishes before it responds.
    pub last_state: Value,
}

impl Client {
    pub fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap();
        assert!(n > 0, "daemon closed the connection");
        let msg: Value = serde_json::from_str(&line).unwrap();
        if msg["type"] == json!("state") {
            self.last_state = msg.clone();
        }
        msg
    }

    pub fn recv_state(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == json!("state") {
                return msg;
            }
        }
    }

    /// The most recent state the daemon pushed.
    pub fn state(&self) -> Value {
        self.last_state["state"].clone()
    }

    /// Blocks until the state the daemon has pushed satisfies `want`, and
    /// returns it. The world changes under the daemon (a phone is plugged in)
    /// rather than because a request was sent, so tests wait for a push.
    pub fn await_state(&mut self, what: &str, want: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if want(&self.last_state["state"]) {
                return self.last_state["state"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last state was {}",
                self.last_state["state"]
            );
            self.recv_state();
        }
    }

    /// A broken pipe is not a failure to send: an oversized message is answered
    /// and the connection closed before the last of it has been written, which
    /// is the daemon behaving as designed. What it answered is still readable.
    pub fn send_raw(&mut self, raw: &str) {
        match writeln!(self.stream, "{raw}") {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            other => other.unwrap(),
        }
    }

    /// Sends a request and returns its response, ignoring states pushed on the
    /// way past.
    pub fn request(&mut self, kind: &str) -> Value {
        self.request_with(kind, json!({}))
    }

    /// A request carrying arguments, like `select`'s serial.
    pub fn request_with(&mut self, kind: &str, extra: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let mut request = json!({"v": PROTOCOL_VERSION, "id": id, "kind": kind});
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        self.send_raw(&request.to_string());
        loop {
            let msg = self.recv();
            if msg["type"] == json!("response") {
                assert_eq!(msg["id"], json!(id), "response carries its request id");
                return msg;
            }
        }
    }
}

//! Concrete subprocess deadlines. PATH remains the test seam; this only keeps
//! one stalled external tool from wedging the daemon's transition loop forever.

use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn timeout() -> Duration {
    let ms = std::env::var("OMAVCAM_COMMAND_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10_000);
    Duration::from_millis(ms.max(10))
}

fn timed_out(program: &str, limit: Duration) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("{program} did not finish within {limit:?}"),
    )
}

pub fn status(mut command: Command) -> std::io::Result<ExitStatus> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command.spawn()?;
    let limit = timeout();
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(timed_out(&program, limit));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn output(mut command: Command) -> std::io::Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let limit = timeout();
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait_with_output();
            return Err(timed_out(&program, limit));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

//! Concrete subprocess deadlines. PATH remains the test seam; this only keeps
//! one stalled external tool from wedging the daemon's transition loop forever.

use std::io::Read;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Every external tool here is asked for a short answer - a device list, a
/// camera list, a state word - so output past this is a tool that has gone
/// wrong or a phone trying to exhaust us. Reading stops at the ceiling and the
/// deadline kills whatever is still writing.
const MAX_OUTPUT: u64 = 1 << 20;

/// Each pipe is drained on its own thread, which is what keeps the ceiling from
/// becoming a deadlock: a tool that fills the pipe buffer blocks on a write
/// nobody is reading, and `try_wait` would never see it exit.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(MAX_OUTPUT).read_to_end(&mut buffer);
        }
        buffer
    })
}

fn timeout() -> Duration {
    let ms = std::env::var("VCAMD_COMMAND_MS")
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
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let limit = timeout();
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
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
    };
    // The child has exited, so both pipes are closed and the readers are done.
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(bytes: usize) -> Output {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("head -c {bytes} /dev/zero")]);
        output(command).expect("the tool exited on its own")
    }

    /// More than a pipe buffer, less than the ceiling: all of it arrives. Before
    /// the pipes were drained on their own threads this deadlocked - the tool
    /// blocked writing, `try_wait` never saw it exit, and the call died at the
    /// deadline with whatever happened to be buffered.
    #[test]
    fn output_larger_than_a_pipe_buffer_is_read_whole() {
        let out = writes(256 * 1024);
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 256 * 1024);
    }

    /// Past the ceiling the reading stops. The last kilobyte still fits in the
    /// pipe, so the tool exits and this is the cap rather than the deadline.
    #[test]
    fn output_past_the_ceiling_is_capped() {
        let out = writes(MAX_OUTPUT as usize + 1024);
        assert_eq!(out.stdout.len() as u64, MAX_OUTPUT);
    }
}

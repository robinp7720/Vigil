//! Bounded, cancellable execution for short-lived desktop utilities.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

struct CommandChild(Child, bool);

impl Drop for CommandChild {
    fn drop(&mut self) {
        // Each utility owns a process group, including any helper descendants.
        // SAFETY: the positive child PID identifies the group created below.
        if !self.1 {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: the pipe owns a valid descriptor throughout these calls.
    unsafe {
        let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0; 8192];
    // Bound each pass so a continuously writing child cannot starve cancellation.
    for _ in 0..32 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if bytes.len() + count > OUTPUT_LIMIT {
                    bail!("command output exceeded 8 MiB");
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

pub(crate) fn output(
    command: &mut Command,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<Output> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("command cancelled");
    }
    let mut child = CommandChild(
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn desktop utility")?,
        false,
    );
    let mut stdout = child.0.stdout.take().expect("piped stdout");
    let mut stderr = child.0.stderr.take().expect("piped stderr");
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("command cancelled");
        }
        if Instant::now() >= deadline {
            bail!("command timed out after {timeout:?}");
        }
        let out_done = drain(&mut stdout, &mut out)?;
        let err_done = drain(&mut stderr, &mut err)?;
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if out_done
            && err_done
            && let Some(status) = status
        {
            // The child was reaped; do not signal a potentially reused PID.
            child.1 = true;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        let mut pipes = [
            libc::pollfd {
                fd: if out_done { -1 } else { stdout.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if err_done { -1 } else { stderr.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors remain owned by the pipes for this call.
        let result = unsafe { libc::poll(pipes.as_mut_ptr(), pipes.len() as _, 10) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_reaps_direct_child() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let error = output(
            Command::new("sh")
                .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "test"])
                .arg(&pid_file),
            Duration::from_millis(150),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let pid = std::fs::read_to_string(pid_file).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
    }

    #[test]
    fn runaway_output_is_bounded() {
        let error = output(
            Command::new("head").args(["-c", "9000000", "/dev/zero"]),
            Duration::from_secs(5),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeded"));
    }

    #[test]
    fn captures_both_pipes_and_exit_status() {
        let result = output(
            Command::new("sh").args(["-c", "printf hello; printf problem >&2; exit 7"]),
            Duration::from_secs(2),
            None,
        )
        .unwrap();
        assert_eq!(result.stdout, b"hello");
        assert_eq!(result.stderr, b"problem");
        assert_eq!(result.status.code(), Some(7));
    }
    #[test]
    fn deadline_kills_descendants_holding_pipes() {
        let start = Instant::now();
        let error = output(
            Command::new("sh").args(["-c", "sleep 30 & wait"]),
            Duration::from_millis(60),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn cancellation_interrupts_running_command() {
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(40));
                cancelled.store(true, Ordering::Relaxed);
            });
            let error = output(
                Command::new("sleep").arg("30"),
                Duration::from_secs(10),
                Some(&cancelled),
            )
            .unwrap_err();
            assert!(error.to_string().contains("cancelled"));
        });
    }
    #[test]
    fn drains_large_output_without_pipe_deadlock() {
        let result = output(
            Command::new("head").args(["-c", "200000", "/dev/zero"]),
            Duration::from_secs(2),
            None,
        )
        .unwrap();
        assert_eq!(result.stdout.len(), 200000);
    }
}

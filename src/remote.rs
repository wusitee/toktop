//! SSH snapshot transport. Only complete, validated snapshots are imported.

use crate::model::{Session, Tool};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub sessions: Vec<(Tool, Session)>,
}

fn nonblocking(input: &impl AsRawFd) -> io::Result<()> {
    // Linux fcntl constants; the descriptor remains owned by `input`.
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    const O_NONBLOCK: i32 = 0o4000;
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
    }
    let fd = input.as_raw_fd();
    // SAFETY: `fd` is a live pipe descriptor and both commands use valid arguments.
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags < 0 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_available(input: &mut impl Read, data: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
    let mut buf = [0; 8192];
    // Bound each pass so a noisy stdout cannot starve stderr or the deadline.
    for _ in 0..8 {
        match input.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(count) if data.len() + count <= limit => data.extend_from_slice(&buf[..count]),
            Ok(_) => return Err(io::Error::other("remote output exceeds size limit")),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn collect_output(
    mut child: Child,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let deadline = Instant::now() + timeout;
    let result = (|| -> Result<_, String> {
        nonblocking(&stdout)
            .and_then(|_| nonblocking(&stderr))
            .map_err(|e| e.to_string())?;
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done) = (false, false);
        loop {
            if Instant::now() >= deadline {
                return Err("SSH collection timed out".into());
            }
            if !out_done {
                out_done = read_available(&mut stdout, &mut output, 64 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
            }
            if !err_done {
                err_done = read_available(&mut stderr, &mut errors, 64 * 1024)
                    .map_err(|e| e.to_string())?;
            }
            let status = child.try_wait().map_err(|e| e.to_string())?;
            if let Some(status) = status
                && out_done
                && err_done
            {
                return Ok((status, output, errors));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

pub fn fetch(host: &str, tools: &[Tool]) -> Result<Snapshot, String> {
    if host.is_empty()
        || host.starts_with('-')
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-@".contains(&c))
    {
        return Err("invalid SSH host alias".into());
    }
    let command = format!(
        "toktop-collector --snapshot --tools={}",
        tools
            .iter()
            .map(|tool| tool.key())
            .collect::<Vec<_>>()
            .join(",")
    );
    let child = Command::new("ssh")
        .args([
            "-T",
            "-oBatchMode=yes",
            "-oConnectTimeout=10",
            "-oServerAliveInterval=15",
            "-oServerAliveCountMax=2",
            "-oControlMaster=no",
            "--",
            host,
            &command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let (status, output, errors) = collect_output(child, Duration::from_secs(60))?;
    if !status.success() {
        let text = String::from_utf8_lossy(&errors);
        let message: String = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect();
        return Err(if message.is_empty() {
            format!("ssh exited with {status}")
        } else {
            message
        });
    }
    let snapshot: Snapshot = serde_json::from_slice(&output)
        .map_err(|error| format!("invalid collector response: {error}"))?;
    if snapshot.version != 1 {
        return Err(format!(
            "unsupported collector protocol {}",
            snapshot.version
        ));
    }
    if snapshot
        .sessions
        .iter()
        .any(|(tool, _)| !tools.contains(tool))
    {
        return Err("collector returned a disabled tool".into());
    }
    if snapshot.sessions.iter().any(|(_, session)| {
        session
            .events
            .iter()
            .any(|event| event.model as usize >= session.models.len())
    }) {
        return Err("invalid model index in collector response".into());
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_covers_pipes_held_open_after_child_exits() {
        let child = Command::new("sh")
            .args(["-c", "sleep 2 & printf done"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        assert!(
            collect_output(child, Duration::from_millis(150))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

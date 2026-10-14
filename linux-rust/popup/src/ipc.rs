//! Talking to the LibrePods daemon over `$XDG_RUNTIME_DIR/librepods.sock`.
//!
//! * The watch stream: send `watch popup\n`, then read one JSON object per
//!   line forever. Reconnects silently every 2 s when the socket is
//!   missing or the daemon goes away.
//! * Commands (`connect`, `open`): a separate short-lived connection each,
//!   reading at most one reply line.
//!
//! Everything is non-blocking and driven by the main poll loop.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const RETRY: Duration = Duration::from_secs(2);
const MAX_LINE: usize = 1 << 20;

pub fn default_socket() -> PathBuf {
    // Same override as the daemon (tests/sim, second instances).
    if let Some(p) = std::env::var_os("LIBREPODS_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    dir.join("librepods.sock")
}

/// Split complete lines off the front of `buf`.
fn take_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        let s = String::from_utf8_lossy(&line).trim().to_string();
        if !s.is_empty() {
            out.push(s);
        }
    }
    if buf.len() > MAX_LINE {
        buf.clear();
    }
    out
}

pub struct Watch {
    path: PathBuf,
    stream: Option<UnixStream>,
    buf: Vec<u8>,
    retry_at: Option<Instant>,
    /// Logged once per outage instead of every 2 s.
    quiet: bool,
}

impl Watch {
    pub fn new(path: PathBuf) -> Self {
        Watch {
            path,
            stream: None,
            buf: Vec::new(),
            retry_at: Some(Instant::now()),
            quiet: false,
        }
    }

    pub fn fd(&self) -> Option<RawFd> {
        self.stream.as_ref().map(|s| s.as_raw_fd())
    }

    pub fn deadline(&self) -> Option<Instant> {
        if self.stream.is_some() { None } else { self.retry_at }
    }

    /// Connect when the retry time has come.
    pub fn tick(&mut self, now: Instant) {
        if self.stream.is_some() || self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        match self.open() {
            Ok(s) => {
                crate::vlog!("watching {}", self.path.display());
                self.stream = Some(s);
                self.buf.clear();
                self.quiet = false;
            }
            Err(e) => {
                if !self.quiet {
                    crate::vlog!("{}: {e} (retrying every {}s)", self.path.display(), RETRY.as_secs());
                    self.quiet = true;
                }
                self.retry_at = Some(now + RETRY);
            }
        }
    }

    fn open(&self) -> io::Result<UnixStream> {
        let mut s = UnixStream::connect(&self.path)?;
        s.set_write_timeout(Some(Duration::from_secs(1)))?;
        s.write_all(b"watch popup\n")?;
        s.set_nonblocking(true)?;
        Ok(s)
    }

    /// Read whatever arrived. Returns complete lines; on EOF or error the
    /// stream is dropped and a reconnect scheduled.
    pub fn read(&mut self, now: Instant) -> Vec<String> {
        let Some(s) = self.stream.as_mut() else {
            return Vec::new();
        };
        let mut chunk = [0u8; 8192];
        let mut closed = false;
        loop {
            match s.read(&mut chunk) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        let lines = take_lines(&mut self.buf);
        if closed {
            crate::vlog!("daemon closed the watch stream; reconnecting");
            self.stream = None;
            self.buf.clear();
            self.retry_at = Some(now + RETRY);
        }
        lines
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdKind {
    Connect,
    Open,
    /// `disconnect`, `mode …`, `ca …`: fire and forget.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Err(String),
    /// Closed or timed out without a reply line.
    None,
}

pub struct Cmd {
    pub kind: CmdKind,
    stream: UnixStream,
    buf: Vec<u8>,
    pub deadline: Instant,
}

/// Send one command line (`connect`, `open`, `mode anc`, `ca on`, …) on
/// its own connection.
pub fn send(path: &Path, kind: CmdKind, line: &str, timeout: Duration) -> io::Result<Cmd> {
    let mut s = UnixStream::connect(path)?;
    s.set_write_timeout(Some(Duration::from_secs(1)))?;
    s.write_all(format!("{}\n", line.trim()).as_bytes())?;
    s.set_nonblocking(true)?;
    Ok(Cmd {
        kind,
        stream: s,
        buf: Vec::new(),
        deadline: Instant::now() + timeout,
    })
}

impl Cmd {
    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Read the reply; `Some` once the command is finished.
    pub fn read(&mut self) -> Option<Reply> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if let Some(line) = take_lines(&mut self.buf).into_iter().next() {
                        return Some(parse_reply(&line));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // EOF: a final unterminated line still counts.
        let rest = String::from_utf8_lossy(&self.buf).trim().to_string();
        Some(if rest.is_empty() { Reply::None } else { parse_reply(&rest) })
    }
}

pub fn parse_reply(line: &str) -> Reply {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Reply::Err(format!("unexpected reply: {line}"));
    };
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        Reply::Ok
    } else {
        Reply::Err(
            v.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("request failed")
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn lines_split_and_partial_kept() {
        let mut b = b"{\"a\":1}\n\n{\"b\"".to_vec();
        assert_eq!(take_lines(&mut b), vec!["{\"a\":1}".to_string()]);
        assert_eq!(b, b"{\"b\"");
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply(r#"{"ok":true}"#), Reply::Ok);
        assert_eq!(parse_reply(r#"{"ok":false,"error":"out of range"}"#), Reply::Err("out of range".into()));
        assert!(matches!(parse_reply("garbage"), Reply::Err(_)));
    }

    #[test]
    fn watch_round_trip() {
        let dir = std::env::temp_dir().join(format!("lpp-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path).unwrap();
        let mut w = Watch::new(path.clone());
        w.tick(Instant::now());
        assert!(w.fd().is_some());
        let (mut srv, _) = l.accept().unwrap();
        let mut hello = [0u8; 12];
        srv.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"watch popup\n");
        srv.write_all(b"{\"event\":\"status\"}\n{\"event\":\"lid_open\"}\n").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(w.read(Instant::now()).len(), 2);
        drop(srv);
        std::thread::sleep(Duration::from_millis(50));
        w.read(Instant::now());
        assert!(w.fd().is_none(), "EOF drops the stream");
        assert!(w.deadline().is_some(), "and schedules a retry");
        let _ = std::fs::remove_file(&path);
    }
}

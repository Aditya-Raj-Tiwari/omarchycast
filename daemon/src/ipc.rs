//! Newline-delimited JSON over a unix socket.
//!
//! One request per line, one response per line. Quickshell's `Socket` plus a
//! `SplitParser` consumes this shape directly, and the connection stays open for
//! the life of the overlay so a keystroke costs a write, not a connect.

use crate::limits;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Only $XDG_RUNTIME_DIR is acceptable: it is created per-user with mode 0700.
/// The old fallback to the shared system temp directory put a user-trusted
/// socket where any local user could pre-create or replace the path.
pub fn socket_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set; refusing a world-shared socket path"))?;
    let meta = std::fs::metadata(&dir)?;
    // SAFETY: getuid is always safe.
    if meta.uid() != unsafe { libc::getuid() } {
        bail!("{} is not owned by this user", dir.display());
    }
    if meta.mode() & 0o077 != 0 {
        bail!("{} is accessible to other users (mode {:o})", dir.display(), meta.mode() & 0o7777);
    }
    Ok(dir.join("omarchycast.sock"))
}

/// Carries a caller-chosen request id so the overlay can discard a stale response
/// instead of rendering results for a query the user has already typed past.
///
/// The field is `rid`, not `id`: the request is flattened into this struct, and
/// `Activate` already has an `id` of its own. Two fields competing for the same
/// JSON key made every activation fail to parse.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub rid: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Request {
    Ping,
    Query { text: String },
    Activate { id: String, #[serde(default)] action: String },
    Config,
    SetConfig { config: crate::config::Config },
    Reindex,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub rid: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<crate::core::Item>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::config::Config>,
}

impl Response {
    pub fn ok() -> Self {
        Response { rid: 0, ok: true, error: None, items: None, config: None }
    }
    pub fn error(message: impl std::fmt::Display) -> Self {
        // Error strings can embed attacker-influenced text (a path, an id), so
        // they are bounded like every other field the UI displays.
        let text = limits::clamp_text(&message.to_string(), limits::MAX_ERROR_CHARS);
        Response { ok: false, error: Some(text), ..Response::ok() }
    }
}

/// Binds the socket, refusing to start if a live daemon already owns it.
pub fn listen() -> Result<UnixListener> {
    let path = socket_path()?;
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(anyhow!("another omarchycast daemon is already running"));
        }
        // Left over from a daemon that didn't shut down cleanly.
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    // The runtime dir is already 0700; this makes the socket's own policy
    // explicit rather than inherited.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Reads one `\n`-terminated line of at most `limits::MAX_REQUEST_BYTES`.
///
/// `BufRead::lines` would buffer an arbitrarily long line before handing it
/// over; this reads through a hard cap and refuses the connection when a line
/// exceeds it. Returns Ok(None) at end of stream.
fn read_bounded_line(reader: &mut BufReader<UnixStream>) -> Result<Option<String>> {
    let mut line: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return if line.is_empty() { Ok(None) } else { Ok(Some(String::from_utf8(line)?)) };
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok(Some(String::from_utf8(line)?));
                }
                if line.len() >= limits::MAX_REQUEST_BYTES {
                    bail!("request exceeds {} bytes", limits::MAX_REQUEST_BYTES);
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// The connecting process's uid via SO_PEERCRED. (`UnixStream::peer_cred` is
/// still unstable, so this asks the kernel directly.)
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd is a live socket owned by `stream`; the kernel writes at most
    // `len` bytes into `cred`, which is a plain-old-data struct we own.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if rc == 0 { Some(cred.uid) } else { None }
}

/// Guard that owns one slot in the shared client counter.
pub struct ClientSlot(Arc<AtomicUsize>);

impl ClientSlot {
    /// Claims a slot, or refuses when the daemon is already at capacity.
    pub fn claim(counter: &Arc<AtomicUsize>) -> Option<ClientSlot> {
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            if current >= limits::MAX_CLIENTS {
                return None;
            }
            match counter.compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Some(ClientSlot(counter.clone())),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Serves one client until it disconnects. Each connection gets its own thread;
/// there is realistically only ever one (the overlay).
pub fn serve_connection<F>(stream: UnixStream, handle: F)
where
    F: Fn(Request) -> Response,
{
    // Only this user's processes may speak to the daemon. The socket lives in a
    // 0700 directory, but peer credentials are checked anyway: defence against a
    // misconfigured runtime dir costs one getsockopt.
    if peer_uid(&stream) != Some(unsafe { libc::getuid() }) {
        return;
    }

    let Ok(write_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    let mut writer = write_half;

    loop {
        let line = match read_bounded_line(&mut reader) {
            Ok(Some(line)) => line,
            // Oversized or malformed input ends the connection rather than the daemon.
            Ok(None) | Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Envelope>(&line) {
            Ok(envelope) => {
                let rid = envelope.rid;
                let mut response = handle(envelope.request);
                response.rid = rid;
                response
            }
            Err(e) => Response::error(format!("malformed request: {e}")),
        };
        // Never let a serialisation failure silently drop the reply the client is
        // waiting on — send a well-formed error in its place.
        let mut encoded = serde_json::to_string(&response).unwrap_or_else(|e| {
            let mut fallback = Response::error(format!("response could not be encoded: {e}"));
            fallback.rid = response.rid;
            serde_json::to_string(&fallback)
                .unwrap_or_else(|_| r#"{"rid":0,"ok":false,"error":"encoding failed"}"#.to_string())
        });
        // A response that somehow ballooned is replaced by a bounded error —
        // the client is a UI reading line-by-line and must never face a flood.
        if encoded.len() > limits::MAX_RESPONSE_BYTES {
            let mut bounded = Response::error("response too large");
            bounded.rid = response.rid;
            encoded = serde_json::to_string(&bounded)
                .unwrap_or_else(|_| r#"{"rid":0,"ok":false,"error":"encoding failed"}"#.to_string());
        }
        encoded.push('\n');
        if writer.write_all(encoded.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

pub fn cleanup() {
    if let Ok(path) = socket_path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: the request id and an item id must not compete for the
    /// same JSON key, which previously broke every activation.
    #[test]
    fn activate_keeps_its_own_id_alongside_the_request_id() {
        let raw = r#"{"rid":7,"op":"activate","id":"calc:result","action":"primary"}"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("should parse");
        assert_eq!(envelope.rid, 7);
        match envelope.request {
            Request::Activate { id, action } => {
                assert_eq!(id, "calc:result");
                assert_eq!(action, "primary");
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn oversized_request_lines_are_refused() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (mut a, b) = UnixStream::pair().unwrap();
        let mut reader = std::io::BufReader::new(b);

        // One byte over the limit, no newline yet: must error, not buffer on.
        let big = vec![b'x'; limits::MAX_REQUEST_BYTES + 1];
        a.write_all(&big).unwrap();
        drop(a);
        assert!(super::read_bounded_line(&mut reader).is_err());
    }

    #[test]
    fn bounded_lines_within_the_limit_pass() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (mut a, b) = UnixStream::pair().unwrap();
        let mut reader = std::io::BufReader::new(b);
        a.write_all(b"{\"op\":\"ping\"}\n").unwrap();
        drop(a);
        assert_eq!(super::read_bounded_line(&mut reader).unwrap().unwrap(), "{\"op\":\"ping\"}");
    }

    #[test]
    fn error_text_is_bounded() {
        let long = "e".repeat(10_000);
        let r = Response::error(long);
        assert!(r.error.unwrap().chars().count() <= limits::MAX_ERROR_CHARS);
    }

    #[test]
    fn client_slots_enforce_the_cap() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut held = Vec::new();
        for _ in 0..limits::MAX_CLIENTS {
            held.push(ClientSlot::claim(&counter).expect("slot within cap"));
        }
        assert!(ClientSlot::claim(&counter).is_none(), "over-cap claim must fail");
        held.pop();
        assert!(ClientSlot::claim(&counter).is_some(), "released slot must be reusable");
    }

    #[test]
    fn query_round_trips() {
        let raw = r#"{"rid":3,"op":"query","text":"1920 * 0.85"}"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("should parse");
        assert_eq!(envelope.rid, 3);
        assert!(matches!(envelope.request, Request::Query { ref text } if text == "1920 * 0.85"));
    }
}

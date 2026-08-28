//! Newline-delimited JSON over a unix socket.
//!
//! One request per line, one response per line. Quickshell's `Socket` plus a
//! `SplitParser` consumes this shape directly, and the connection stays open for
//! the life of the overlay so a keystroke costs a write, not a connect.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("omarchycast.sock")
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
        Response { ok: false, error: Some(message.to_string()), ..Response::ok() }
    }
}

/// Binds the socket, refusing to start if a live daemon already owns it.
pub fn listen() -> Result<UnixListener> {
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(anyhow!("another omarchycast daemon is already running"));
        }
        // Left over from a daemon that didn't shut down cleanly.
        std::fs::remove_file(&path)?;
    }
    Ok(UnixListener::bind(&path)?)
}

/// Serves one client until it disconnects. Each connection gets its own thread;
/// there is realistically only ever one (the overlay).
pub fn serve_connection<F>(stream: UnixStream, handle: F)
where
    F: Fn(Request) -> Response,
{
    let Ok(write_half) = stream.try_clone() else { return };
    let reader = BufReader::new(stream);
    let mut writer = write_half;

    for line in reader.lines() {
        let Ok(line) = line else { break };
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
        encoded.push('\n');
        if writer.write_all(encoded.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
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
    fn query_round_trips() {
        let raw = r#"{"rid":3,"op":"query","text":"1920 * 0.85"}"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("should parse");
        assert_eq!(envelope.rid, 3);
        assert!(matches!(envelope.request, Request::Query { ref text } if text == "1920 * 0.85"));
    }
}

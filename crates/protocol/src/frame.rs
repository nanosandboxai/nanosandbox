//! NDJSON framing: one compact JSON message per line.
//!
//! This is the single framing used on the control socket (D7). Both the helper
//! and every SDK read and write frames with these helpers, so the wire format
//! has exactly one implementation.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{BufRead, Write};

use crate::error::ProtocolError;

/// Serialize `msg` and write it as a single newline-terminated line.
pub fn write_message<W, T>(writer: &mut W, msg: &T) -> Result<(), ProtocolError>
where
    W: Write,
    T: Serialize,
{
    let line = serde_json::to_string(msg)
        .map_err(|e| ProtocolError::internal(format!("serialize frame: {e}")))?;
    // Compact serialization never contains a raw newline, but guard anyway so a
    // frame can never be split across lines.
    debug_assert!(!line.contains('\n'));
    writer
        .write_all(line.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|e| ProtocolError::new(crate::error::ErrorCode::Transport, format!("write: {e}")))
}

/// Read one NDJSON frame. Returns `Ok(None)` on a clean EOF.
///
/// `scratch` is reused across calls to avoid per-frame allocation.
pub fn read_message<R, T>(reader: &mut R, scratch: &mut String) -> Result<Option<T>, ProtocolError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    scratch.clear();
    let n = reader.read_line(scratch).map_err(|e| {
        ProtocolError::new(crate::error::ErrorCode::Transport, format!("read: {e}"))
    })?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = scratch.trim();
    if trimmed.is_empty() {
        // Skip blank keep-alive lines by recursing once.
        return read_message(reader, scratch);
    }
    let value = serde_json::from_str(trimmed)
        .map_err(|e| ProtocolError::bad_request(format!("decode frame: {e}")))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::{Request, Response};
    use std::io::{BufReader, Cursor};

    #[test]
    fn round_trips_a_request() {
        let mut buf = Vec::new();
        write_message(&mut buf, &Request::Ping).unwrap();
        assert!(buf.ends_with(b"\n"));
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 1);

        let mut reader = BufReader::new(Cursor::new(buf));
        let mut scratch = String::new();
        let got: Request = read_message(&mut reader, &mut scratch).unwrap().unwrap();
        assert_eq!(got, Request::Ping);
    }

    #[test]
    fn eof_returns_none() {
        let mut reader = BufReader::new(Cursor::new(Vec::new()));
        let mut scratch = String::new();
        let got: Option<Request> = read_message(&mut reader, &mut scratch).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn skips_blank_lines() {
        let mut reader = BufReader::new(Cursor::new(b"\n\n{\"type\":\"ping\"}\n".to_vec()));
        let mut scratch = String::new();
        let got: Option<Request> = read_message(&mut reader, &mut scratch).unwrap();
        assert_eq!(got, Some(Request::Ping));
    }

    #[test]
    fn malformed_frame_is_bad_request() {
        let mut reader = BufReader::new(Cursor::new(b"not json\n".to_vec()));
        let mut scratch = String::new();
        let err = read_message::<_, Request>(&mut reader, &mut scratch).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::BadRequest);
    }

    #[test]
    fn response_round_trips() {
        let resp = Response::HelloOk(crate::hello::HelloResponse {
            protocol: crate::PROTOCOL_VERSION.to_string(),
            helper_version: "0.3.0".to_string(),
            capabilities: crate::Capability::all().to_vec(),
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();
        let mut reader = BufReader::new(Cursor::new(buf));
        let mut scratch = String::new();
        let got: Response = read_message(&mut reader, &mut scratch).unwrap().unwrap();
        assert_eq!(got, resp);
    }
}

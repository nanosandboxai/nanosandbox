//! HTTP utility functions for communicating with in-VM services.
//!
//! These are low-level HTTP helpers using raw TcpStream. They are used by the
//! sandbox layer to communicate with the agent-gateway process running inside VMs.

use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default TCP connect timeout in milliseconds.
const CONNECT_TIMEOUT_MS: u64 = 500;

/// Connect to addr with a timeout, avoiding the OS-default ~21s TCP timeout.
fn connect_with_timeout(addr: &str) -> Result<std::net::TcpStream, String> {
    let sock_addr: SocketAddr = addr.parse().map_err(|e| format!("invalid address '{}': {}", addr, e))?;
    std::net::TcpStream::connect_timeout(&sock_addr, Duration::from_millis(CONNECT_TIMEOUT_MS))
        .map_err(|e| e.to_string())
}

/// Minimal HTTP GET using raw TcpStream. Returns (status_code, body).
pub fn http_get(addr: &str, path: &str) -> Result<(u16, String), String> {
    let mut stream = connect_with_timeout(addr)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, addr
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_http_response(stream)
}

/// Minimal HTTP POST (non-SSE). Returns (status_code, body).
pub fn http_post(addr: &str, path: &str, json_body: &str) -> Result<(u16, String), String> {
    let mut stream = connect_with_timeout(addr)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, addr, json_body.len(), json_body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_http_response(stream)
}

/// Minimal HTTP DELETE using raw TcpStream. Returns (status_code, body).
pub fn http_delete(addr: &str, path: &str) -> Result<(u16, String), String> {
    let mut stream = connect_with_timeout(addr)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let request = format!(
        "DELETE {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, addr
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_http_response(stream)
}

/// HTTP POST that reads an SSE stream and calls on_output for each event.
/// Returns the exit code from the final "exit" SSE event.
pub fn http_post_sse<F>(
    addr: &str,
    path: &str,
    json_body: &str,
    on_output: F,
) -> Result<i32, String>
where
    F: Fn(&str, bool),
{
    debug!("[http_post_sse] Connecting to {} path={}", addr, path);
    let mut stream = connect_with_timeout(addr).map_err(|e| {
        error!("[http_post_sse] Connect failed: {}", e);
        e
    })?;
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;

    let request = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n{}",
        path, addr, json_body.len(), json_body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    debug!("[http_post_sse] Request sent, reading response...");

    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("Error reading status line: {}", e))?;
    let status_trimmed = status_line.trim();
    debug!("[http_post_sse] Status: {}", status_trimmed);

    if !status_trimmed.contains("200") {
        let mut body = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => body.push_str(&line),
                Err(_) => break,
            }
        }
        return Err(format!("HTTP error: {} body: {}", status_trimmed, body.trim()));
    }

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("Connection closed before headers finished".to_string()),
            Ok(_) => {
                if line.trim().is_empty() {
                    break;
                }
            }
            Err(e) => return Err(format!("Error reading headers: {}", e)),
        }
    }
    debug!("[http_post_sse] Headers done, reading SSE events...");

    let mut exit_code = 0i32;
    let mut event_count = 0u32;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                debug!("[http_post_sse] EOF after {} events, exit_code={}", event_count, exit_code);
                break;
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(data) = trimmed.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                        match event.get("type").and_then(|v| v.as_str()) {
                            Some("stdout") => {
                                event_count += 1;
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    on_output(text, false);
                                }
                            }
                            Some("stderr") => {
                                event_count += 1;
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    on_output(text, true);
                                }
                            }
                            Some("exit") => {
                                exit_code = event.get("code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                info!("[http_post_sse] Exit event: code={} after {} events", exit_code, event_count);
                                break;
                            }
                            Some("error") => {
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    error!("[http_post_sse] Error event: {}", text);
                                    on_output(text, true);
                                }
                                exit_code = -1;
                                break;
                            }
                            other => {
                                warn!("[http_post_sse] Unknown event type: {:?}", other);
                            }
                        }
                    } else {
                        warn!("[http_post_sse] Failed to parse JSON: {}", &data[..data.len().min(100)]);
                    }
                }
            }
            Err(e) => {
                error!("[http_post_sse] Read error: {}", e);
                break;
            }
        }
    }

    debug!("[http_post_sse] Done: exit_code={}, events={}", exit_code, event_count);
    Ok(exit_code)
}

fn read_http_response(stream: std::net::TcpStream) -> Result<(u16, String), String> {
    read_http_response_generic(stream)
}

fn read_http_response_generic<S: IoRead>(stream: S) -> Result<(u16, String), String> {
    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| e.to_string())?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(val) = line.strip_prefix("Content-Length:").or_else(|| line.strip_prefix("content-length:")) {
            content_length = val.trim().parse().ok();
        }
    }

    let body = if let Some(len) = content_length {
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        let mut body = String::new();
        let _ = reader.read_to_string(&mut body);
        body
    };

    Ok((status_code, body))
}



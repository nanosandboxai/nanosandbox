//! HTTP utility functions for communicating with in-VM services.
//!
//! These are low-level HTTP helpers using raw TcpStream. They are used by the
//! sandbox layer to communicate with the agent-gateway process running inside VMs.

use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default TCP connect timeout in milliseconds. Kept short so health-check
/// polling cycles fast during HCN NAT convergence on Windows.
const CONNECT_TIMEOUT_MS: u64 = 500;

/// Connect to addr with a timeout, avoiding the OS-default ~21s TCP timeout.
fn connect_with_timeout(addr: &str) -> Result<std::net::TcpStream, String> {
    let sock_addr: SocketAddr = addr.parse().map_err(|e| format!("invalid address '{}': {}", addr, e))?;
    std::net::TcpStream::connect_timeout(&sock_addr, Duration::from_millis(CONNECT_TIMEOUT_MS))
        .map_err(|e| e.to_string())
}

/// HTTP GET over an HvSocket stream (Windows only — bypasses HCN NAT).
#[cfg(target_os = "windows")]
pub fn http_get_hvsocket(vm_id: &str, path: &str) -> Result<(u16, String), String> {
    let mut stream = crate::hvsocket::HvSocketStream::connect(vm_id)
        .map_err(|e| format!("HvSocket connect failed: {}", e))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\n\r\n",
        path
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("HvSocket send failed: {}", e))?;

    read_http_response_generic(stream)
}

/// HTTP POST over an HvSocket stream (Windows only).
#[cfg(target_os = "windows")]
pub fn http_post_hvsocket(vm_id: &str, path: &str, json_body: &str) -> Result<(u16, String), String> {
    let mut stream = crate::hvsocket::HvSocketStream::connect(vm_id)
        .map_err(|e| format!("HvSocket connect failed: {}", e))?;

    let request = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, json_body.len(), json_body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_http_response_generic(stream)
}

/// HTTP DELETE over an HvSocket stream (Windows only).
#[cfg(target_os = "windows")]
pub fn http_delete_hvsocket(vm_id: &str, path: &str) -> Result<(u16, String), String> {
    let mut stream = crate::hvsocket::HvSocketStream::connect(vm_id)
        .map_err(|e| format!("HvSocket connect failed: {}", e))?;

    let request = format!(
        "DELETE {} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\n\r\n",
        path
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_http_response_generic(stream)
}

/// HTTP POST SSE over an HvSocket stream (Windows only).
#[cfg(target_os = "windows")]
pub fn http_post_sse_hvsocket<F>(
    vm_id: &str,
    path: &str,
    json_body: &str,
    on_output: F,
) -> Result<i32, String>
where
    F: Fn(&str, bool),
{
    debug!("[http_post_sse_hvsocket] Connecting to VM '{}' path={}", vm_id, path);
    let mut stream = crate::hvsocket::HvSocketStream::connect(vm_id)
        .map_err(|e| format!("HvSocket connect failed: {}", e))?;

    // Remove recv timeout for SSE streaming — agent can take minutes between
    // output chunks (thinking, API calls). The default 5s timeout would cause
    // the stream to error out during long pauses, silently losing all output.
    stream.set_recv_timeout(None)
        .map_err(|e| format!("Failed to clear recv timeout for SSE: {}", e))?;

    let request = format!(
        "POST {} HTTP/1.0\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n{}",
        path, json_body.len(), json_body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    read_sse_response(stream, on_output)
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

/// Read SSE events from a generic stream.
#[cfg(target_os = "windows")]
fn read_sse_response<S: IoRead, F>(stream: S, on_output: F) -> Result<i32, String>
where
    F: Fn(&str, bool),
{
    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("Error reading status line: {}", e))?;
    let status_trimmed = status_line.trim();
    debug!("[sse_hvsocket] Status: {}", status_trimmed);

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

    // Skip headers
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

    let mut exit_code = 0i32;
    let mut event_count = 0u32;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                info!("[sse_hvsocket] EOF after {} events, exit_code={}", event_count, exit_code);
                break;
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(data) = trimmed.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                        let ev_type = event.get("type").and_then(|v| v.as_str());
                        match ev_type {
                            Some("stdout") => {
                                event_count += 1;
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    // Log first event + periodic heartbeats so we can
                                    // confirm SSE events cross the HvSocket wire.
                                    if event_count == 1 || event_count % 20 == 0 {
                                        let preview = if text.len() > 200 {
                                            format!("{}...(truncated)", &text[..200])
                                        } else {
                                            text.to_string()
                                        };
                                        info!("[sse_hvsocket] stdout event #{}: {}", event_count, preview);
                                    }
                                    on_output(text, false);
                                }
                            }
                            Some("stderr") => {
                                event_count += 1;
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    if event_count == 1 || event_count % 20 == 0 {
                                        let preview = if text.len() > 200 {
                                            format!("{}...(truncated)", &text[..200])
                                        } else {
                                            text.to_string()
                                        };
                                        info!("[sse_hvsocket] stderr event #{}: {}", event_count, preview);
                                    }
                                    on_output(text, true);
                                }
                            }
                            Some("exit") => {
                                exit_code = event.get("code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                info!("[sse_hvsocket] Exit event: code={} after {} events", exit_code, event_count);
                                break;
                            }
                            Some("error") => {
                                if let Some(text) = event.get("data").and_then(|v| v.as_str()) {
                                    error!("[sse_hvsocket] Error event: {}", text);
                                    on_output(text, true);
                                }
                                exit_code = -1;
                                break;
                            }
                            other => {
                                warn!("[sse_hvsocket] Unknown event type: {:?}", other);
                            }
                        }
                    } else {
                        warn!("[sse_hvsocket] Failed to parse SSE JSON: {}", &data[..data.len().min(100)]);
                    }
                }
            }
            Err(e) => {
                error!("[sse_hvsocket] Read error after {} events: {}", event_count, e);
                break;
            }
        }
    }
    Ok(exit_code)
}

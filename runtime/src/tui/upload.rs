//! File and image upload to sandbox VMs.
//!
//! Transfers files from the host into sandbox VMs over SSH exec channels,
//! with clipboard image reading and pasted file path detection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::event::AppEvent;

/// Maximum file size for uploads (100 MB).
const MAX_UPLOAD_SIZE: u64 = 100 * 1024 * 1024;

/// Remote directory inside the VM where uploads are placed.
pub const UPLOAD_DIR: &str = "/workspace/.uploads";

/// Encode raw RGBA pixel data to a PNG byte buffer.
pub fn encode_rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(std::io::Cursor::new(&mut buf), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("PNG header: {}", e))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("PNG data: {}", e))?;
    }
    Ok(buf)
}

/// Read an image from the system clipboard and return it as PNG bytes.
///
/// Returns `(png_bytes, suggested_filename)`.
/// Runs blocking clipboard access — call from `spawn_blocking`.
pub fn read_clipboard_image() -> Result<(Vec<u8>, String), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard init: {}", e))?;
    let image = clipboard
        .get_image()
        .map_err(|e| format!("No image in clipboard: {}", e))?;
    let png_bytes = encode_rgba_to_png(
        image.width as u32,
        image.height as u32,
        image.bytes.as_ref(),
    )?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let filename = format!("clipboard-{}.png", timestamp);
    Ok((png_bytes, filename))
}

/// Read text from the system clipboard.
///
/// Runs blocking clipboard access — call from `spawn_blocking`.
pub fn read_clipboard_text() -> Result<String, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard init: {}", e))?;
    clipboard
        .get_text()
        .map_err(|e| format!("No text in clipboard: {}", e))
}

/// Detect host file paths in pasted text.
///
/// Returns paths that actually exist on the host filesystem.
pub fn detect_file_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .flat_map(|line| line.split('\t'))
        .map(|s| s.trim().trim_matches('\'').trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.exists() && p.is_file())
        .collect()
}

/// Minimal russh client handler for upload connections.
///
/// Accepts all server keys (localhost sandbox with ephemeral keys).
struct UploadSshHandler;

impl russh::client::Handler for UploadSshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Upload a file to the sandbox VM via SSH exec channel.
///
/// Opens a **separate** SSH connection (using the panel's stored port + key),
/// runs `mkdir -p <dir> && cat > <path>` on the remote, pipes the file data
/// to stdin, then closes the channel. This avoids relying on the SFTP
/// subsystem which may not be configured in all VM images.
///
/// Returns the number of bytes written.
pub async fn ssh_upload(
    ssh_port: u16,
    key_path: &Path,
    local_data: &[u8],
    remote_path: &str,
) -> Result<u64, String> {
    use russh::ChannelMsg;

    // Load private key.
    let key_data = tokio::fs::read_to_string(key_path)
        .await
        .map_err(|e| format!("Read SSH key: {}", e))?;
    let key_pair = russh::keys::decode_secret_key(&key_data, None)
        .map_err(|e| format!("Decode SSH key: {}", e))?;
    let key_with_hash =
        russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);

    // Connect.
    let config = Arc::new(russh::client::Config::default());
    let mut session =
        russh::client::connect(config, format!("127.0.0.1:{}", ssh_port), UploadSshHandler)
            .await
            .map_err(|e| format!("SSH connect: {}", e))?;

    // Authenticate.
    let auth_result = session
        .authenticate_publickey("developer", key_with_hash)
        .await
        .map_err(|e| format!("SSH auth: {}", e))?;
    if !matches!(auth_result, russh::client::AuthResult::Success) {
        return Err("SSH authentication failed".to_string());
    }

    // Determine the parent directory for mkdir -p.
    let parent_dir = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".to_string());

    // Open a session channel and exec the upload command.
    // `cat > path` reads binary data from stdin and writes it verbatim.
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("SSH channel open: {}", e))?;

    let cmd = format!(
        "mkdir -p '{}' && cat > '{}'",
        parent_dir.replace('\'', "'\\''"),
        remote_path.replace('\'', "'\\''"),
    );
    channel
        .exec(true, cmd)
        .await
        .map_err(|e| format!("SSH exec: {}", e))?;

    // Send the file data through the channel.
    channel
        .data(&local_data[..])
        .await
        .map_err(|e| format!("SSH data send: {}", e))?;

    // Signal EOF so `cat` finishes writing and exits.
    channel
        .eof()
        .await
        .map_err(|e| format!("SSH eof: {}", e))?;

    // Wait for the remote side to close and check exit status.
    let mut exit_status = None;
    let mut stderr_output = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::ExitStatus { exit_status: code } => {
                exit_status = Some(code);
            }
            ChannelMsg::ExtendedData { data, .. } => {
                stderr_output.extend_from_slice(&data);
            }
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }

    if let Some(code) = exit_status {
        if code != 0 {
            let stderr = String::from_utf8_lossy(&stderr_output);
            return Err(format!(
                "Remote command exited with status {}: {}",
                code,
                stderr.trim()
            ));
        }
    }

    Ok(local_data.len() as u64)
}

/// Spawn an async upload task for a host file.
///
/// Reads the file from disk, validates size, and uploads via SFTP.
/// Sends `UploadComplete` or `UploadFailed` events back to the TUI.
pub fn spawn_file_upload(
    ssh_port: u16,
    key_path: PathBuf,
    host_path: PathBuf,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let filename = host_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let remote_path = format!("{}/{}", UPLOAD_DIR, filename);

        // Read file from host.
        let data = match tokio::fs::read(&host_path).await {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(AppEvent::UploadFailed {
                    panel_idx,
                    error: format!("Read {}: {}", host_path.display(), e),
                });
                return;
            }
        };

        if data.len() as u64 > MAX_UPLOAD_SIZE {
            let _ = tx.send(AppEvent::UploadFailed {
                panel_idx,
                error: format!(
                    "File too large: {} ({} MB, max {} MB)",
                    filename,
                    data.len() / (1024 * 1024),
                    MAX_UPLOAD_SIZE / (1024 * 1024)
                ),
            });
            return;
        }

        let _ = tx.send(AppEvent::UploadStarted {
            panel_idx,
            filename: filename.clone(),
        });

        match ssh_upload(ssh_port, &key_path, &data, &remote_path).await {
            Ok(size) => {
                let _ = tx.send(AppEvent::UploadComplete {
                    panel_idx,
                    filename,
                    remote_path,
                    size,
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::UploadFailed {
                    panel_idx,
                    error: e,
                });
            }
        }
    });
}

/// Spawn an async upload task for raw bytes (e.g. clipboard image).
pub fn spawn_bytes_upload(
    ssh_port: u16,
    key_path: PathBuf,
    data: Vec<u8>,
    filename: String,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let remote_path = format!("{}/{}", UPLOAD_DIR, filename);

        if data.len() as u64 > MAX_UPLOAD_SIZE {
            let _ = tx.send(AppEvent::UploadFailed {
                panel_idx,
                error: format!(
                    "Data too large: {} ({} MB, max {} MB)",
                    filename,
                    data.len() / (1024 * 1024),
                    MAX_UPLOAD_SIZE / (1024 * 1024)
                ),
            });
            return;
        }

        let _ = tx.send(AppEvent::UploadStarted {
            panel_idx,
            filename: filename.clone(),
        });

        match ssh_upload(ssh_port, &key_path, &data, &remote_path).await {
            Ok(size) => {
                let _ = tx.send(AppEvent::UploadComplete {
                    panel_idx,
                    filename,
                    remote_path,
                    size,
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::UploadFailed {
                    panel_idx,
                    error: e,
                });
            }
        }
    });
}

/// Format a byte count as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_file_paths_empty() {
        assert!(detect_file_paths("").is_empty());
        assert!(detect_file_paths("hello world").is_empty());
    }

    #[test]
    fn test_detect_file_paths_relative_ignored() {
        assert!(detect_file_paths("./relative/path.txt").is_empty());
        assert!(detect_file_paths("some/path").is_empty());
    }

    #[test]
    fn test_detect_file_paths_nonexistent() {
        assert!(detect_file_paths("/nonexistent/path/to/file.xyz").is_empty());
    }

    #[test]
    fn test_detect_file_paths_real_file() {
        // Cargo.toml exists at the project root.
        let manifest = env!("CARGO_MANIFEST_DIR");
        let cargo_toml = format!("{}/Cargo.toml", manifest);
        let paths = detect_file_paths(&cargo_toml);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "Cargo.toml");
    }

    #[test]
    fn test_detect_file_paths_quoted() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let input = format!("'{}/Cargo.toml'", manifest);
        let paths = detect_file_paths(&input);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn test_detect_file_paths_multiple_lines() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let input = format!("{}/Cargo.toml\n{}/src/lib.rs", manifest, manifest);
        let paths = detect_file_paths(&input);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_encode_rgba_to_png_basic() {
        // 1x1 red pixel.
        let rgba = [255u8, 0, 0, 255];
        let png_bytes = encode_rgba_to_png(1, 1, &rgba).unwrap();
        // PNG magic number.
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn test_encode_rgba_to_png_larger() {
        // 10x10 transparent image.
        let rgba = vec![0u8; 10 * 10 * 4];
        let png_bytes = encode_rgba_to_png(10, 10, &rgba).unwrap();
        assert!(!png_bytes.is_empty());
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1048576), "1.0 MB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }
}

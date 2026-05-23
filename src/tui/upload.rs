//! File and image upload to sandbox VMs.
//!
//! TUI-specific glue for clipboard access and event-driven upload tasks.

use std::path::PathBuf;

use tokio::sync::mpsc;

use super::event::AppEvent;

pub use upload_core::*;

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

/// Spawn an async upload task for a host file.
///
/// Reads the file from disk, validates size, and uploads via SSH.
/// Sends `UploadComplete` or `UploadFailed` events back to the TUI.
pub fn spawn_file_upload(
    ssh_host: String,
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

        match ssh_upload(&ssh_host, ssh_port, &key_path, &data, &remote_path).await {
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
    ssh_host: String,
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

        match ssh_upload(&ssh_host, ssh_port, &key_path, &data, &remote_path).await {
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

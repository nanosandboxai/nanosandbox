//! File and image upload to sandbox VMs.
//!
//! Writes uploads host-side into the panel's virtiofs workspace mount, which
//! the guest sees at `/workspace`. Also handles clipboard image reading and
//! pasted file path detection.

use std::path::{Path, PathBuf};

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
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("Clipboard init: {}", e))?;
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
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("Clipboard init: {}", e))?;
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

/// Map a guest upload path (`/workspace/...`) onto the host mount root.
pub fn host_upload_path(mount_root: &Path, remote_path: &str) -> Option<PathBuf> {
    let rel = remote_path.strip_prefix("/workspace")?;
    Some(mount_root.join(rel.trim_start_matches('/')))
}

/// Write upload bytes into the panel's virtiofs workspace mount.
///
/// The guest sees the same bytes at `remote_path` because the mount is shared.
pub async fn fs_upload(
    mount_root: &Path,
    remote_path: &str,
    local_data: &[u8],
) -> Result<u64, String> {
    let host_path = host_upload_path(mount_root, remote_path)
        .ok_or_else(|| format!("unsupported upload path: {}", remote_path))?;
    if let Some(parent) = host_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Create {}: {}", parent.display(), e))?;
    }
    tokio::fs::write(&host_path, local_data)
        .await
        .map_err(|e| format!("Write {}: {}", host_path.display(), e))?;
    Ok(local_data.len() as u64)
}

/// Spawn an async upload task for a host file.
pub fn spawn_file_upload(
    mount_root: Option<PathBuf>,
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

        let Some(mount_root) = mount_root else {
            let _ = tx.send(AppEvent::UploadFailed {
                panel_idx,
                error: "No project mount for this panel; uploads require a mounted workspace."
                    .to_string(),
            });
            return;
        };

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

        match fs_upload(&mount_root, &remote_path, &data).await {
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
    mount_root: Option<PathBuf>,
    data: Vec<u8>,
    filename: String,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let remote_path = format!("{}/{}", UPLOAD_DIR, filename);

        let Some(mount_root) = mount_root else {
            let _ = tx.send(AppEvent::UploadFailed {
                panel_idx,
                error: "No project mount for this panel; uploads require a mounted workspace."
                    .to_string(),
            });
            return;
        };

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

        match fs_upload(&mount_root, &remote_path, &data).await {
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

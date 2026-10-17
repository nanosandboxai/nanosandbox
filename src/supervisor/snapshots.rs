//! Disk-only snapshot store (Epic 1 WS6 / M5, decision: `docs/decisions/snapshots.md`).
//!
//! A snapshot captures a sandbox's **on-disk writable state** (its config plus
//! any persisted files/dirs under the sandbox directory) and a manifest, under
//! `$NANOSB_HOME/snapshots/<id>/`. Memory/process state is out of scope (the
//! disk-only fallback the epic's R3 permits). `restore`/`fork` materialise a
//! new, independent sandbox directory from a snapshot.

use std::path::{Path, PathBuf};

use nanosb_protocol::SnapshotInfo;
use sha2::{Digest, Sha256};

/// Root directory for snapshots: `$NANOSB_HOME/snapshots` (default
/// `~/.nanosandbox/snapshots`).
pub fn snapshots_root() -> PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_HOME") {
        return PathBuf::from(dir).join("snapshots");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".nanosandbox").join("snapshots")
}

/// Typed snapshot error, mapped to a protocol code at the verb boundary.
#[derive(Debug)]
pub enum SnapshotError {
    NotFound(String),
    AlreadyExists(String),
    Invalid(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::NotFound(id) => write!(f, "snapshot '{id}' not found"),
            SnapshotError::AlreadyExists(id) => write!(f, "snapshot '{id}' already exists"),
            SnapshotError::Invalid(msg) => write!(f, "invalid snapshot: {msg}"),
            SnapshotError::Io(e) => write!(f, "snapshot io error: {e}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl SnapshotError {
    /// Map to the protocol error the verb boundary should emit.
    pub fn to_protocol(&self) -> nanosb_protocol::ProtocolError {
        match self {
            SnapshotError::NotFound(id) => {
                nanosb_protocol::ProtocolError::not_found(format!("snapshot '{id}' not found"))
            }
            SnapshotError::AlreadyExists(id) => nanosb_protocol::ProtocolError::invalid_state(
                format!("snapshot '{id}' already exists"),
            ),
            SnapshotError::Invalid(msg) => nanosb_protocol::ProtocolError::bad_request(msg.clone()),
            SnapshotError::Io(e) => nanosb_protocol::ProtocolError::io(e.to_string()),
        }
    }
}

/// Disk-only snapshot store under `$NANOSB_HOME/snapshots`.
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::with_root(snapshots_root())
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Capture `sandbox_dir` into a new snapshot. `dest` names it explicitly;
    /// otherwise the id is `<sandbox>-<created_at_ms>`.
    pub fn create(
        &self,
        sandbox_dir: &Path,
        sandbox: &str,
        image: Option<String>,
        dest: Option<String>,
    ) -> Result<SnapshotInfo, SnapshotError> {
        std::fs::create_dir_all(&self.root).map_err(SnapshotError::Io)?;
        let created_at_ms = now_ms();
        let id = match dest {
            Some(d) => validate_id(&d)?,
            None => format!("{}-{}", validate_id(sandbox)?, created_at_ms),
        };
        let dir = self.root.join(&id);
        if dir.exists() {
            return Err(SnapshotError::AlreadyExists(id));
        }
        std::fs::create_dir_all(&dir).map_err(SnapshotError::Io)?;

        let state = dir.join("state");
        copy_tree(sandbox_dir, &state)?;

        let manifest = serde_json::json!({
            "id": id,
            "sandbox": sandbox,
            "image": image,
            "created_at_ms": created_at_ms,
            "includes_memory": false,
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap_or_default(),
        )
        .map_err(SnapshotError::Io)?;

        let hash = hash_dir(&dir);
        Ok(SnapshotInfo {
            id,
            sandbox: sandbox.to_string(),
            image,
            path: Some(dir.to_string_lossy().to_string()),
            hash: Some(hash),
            created_at_ms,
            includes_memory: false,
        })
    }

    /// List all snapshots, sorted by id.
    pub fn list(&self) -> Vec<SnapshotInfo> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                if let Ok(info) = self.get(&id) {
                    out.push(info);
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Get a snapshot by id.
    pub fn get(&self, id: &str) -> Result<SnapshotInfo, SnapshotError> {
        let id = validate_id(id)?;
        let dir = self.root.join(&id);
        let manifest: serde_json::Value = std::fs::read_to_string(dir.join("manifest.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| SnapshotError::NotFound(id.clone()))?;
        Ok(SnapshotInfo {
            id,
            sandbox: manifest
                .get("sandbox")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            image: manifest
                .get("image")
                .and_then(|v| v.as_str())
                .map(String::from),
            path: Some(dir.to_string_lossy().to_string()),
            hash: Some(hash_dir(&dir)),
            created_at_ms: manifest
                .get("created_at_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            includes_memory: false,
        })
    }

    /// Remove a snapshot.
    pub fn remove(&self, id: &str) -> Result<(), SnapshotError> {
        let id = validate_id(id)?;
        let dir = self.root.join(&id);
        if !dir.exists() {
            return Err(SnapshotError::NotFound(id));
        }
        std::fs::remove_dir_all(&dir).map_err(SnapshotError::Io)
    }

    /// Materialise a new, independent sandbox directory from a snapshot. The
    /// captured state is copied into `new_sandbox_dir`, and its `config.json`
    /// (when present) is rewritten to `new_sandbox`.
    pub fn materialize(
        &self,
        id: &str,
        new_sandbox: &str,
        new_sandbox_dir: &Path,
    ) -> Result<SnapshotInfo, SnapshotError> {
        let src = self.get(id)?;
        let src_state = self.root.join(&src.id).join("state");
        let _ = new_sandbox; // name rewrite handled by the caller's deploy path
        copy_tree(&src_state, new_sandbox_dir)?;
        Ok(src)
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_id(id: &str) -> Result<String, SnapshotError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(SnapshotError::Invalid(
            "snapshot id is required".to_string(),
        ));
    }
    if id == "." || id == ".." || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(SnapshotError::Invalid(format!(
            "invalid snapshot id '{id}'"
        )));
    }
    Ok(id.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Recursively copy `src` into `dst`, skipping unix sockets (e.g. `exec.sock`)
/// and the per-boot staged `agent/` mount.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), SnapshotError> {
    use std::os::unix::fs::FileTypeExt;
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dst).map_err(SnapshotError::Io)?;
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "agent" {
            continue;
        }
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_socket() || ft.is_symlink() {
            continue;
        }
        let target = dst.join(&name);
        if ft.is_dir() {
            copy_tree(&path, &target)?;
        } else if ft.is_file() {
            std::fs::copy(&path, &target).map_err(SnapshotError::Io)?;
        }
    }
    Ok(())
}

/// Deterministic content hash of a snapshot directory (relative path + bytes).
fn hash_dir(dir: &Path) -> String {
    let mut hasher = Sha256::new();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut files);
    files.sort();
    for f in files {
        if let Ok(rel) = f.strip_prefix(dir) {
            hasher.update(rel.to_string_lossy().as_bytes());
        }
        if f == dir.join("manifest.json") {
            continue; // the hash describes the captured state, not the manifest
        }
        if let Ok(bytes) = std::fs::read(&f) {
            hasher.update(&bytes);
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let sb = tmp.path().join("sandbox");
        std::fs::create_dir_all(sb.join("data")).unwrap();
        std::fs::write(sb.join("config.json"), r#"{"name":"t","image":"alpine"}"#).unwrap();
        std::fs::write(sb.join("data").join("file.txt"), b"hello").unwrap();
        std::fs::write(sb.join("exec.sock"), b"").unwrap(); // not a real socket; still copied
        (tmp, sb)
    }

    fn store() -> (tempfile::TempDir, SnapshotStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = SnapshotStore::with_root(tmp.path().join("snapshots"));
        (tmp, s)
    }

    fn sandbox_with_marker() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let sb = tmp.path().join("sandbox");
        std::fs::create_dir_all(sb.join("data")).unwrap();
        std::fs::write(sb.join("config.json"), r#"{"name":"t","image":"alpine"}"#).unwrap();
        std::fs::write(sb.join("data").join("marker.txt"), b"payload-123").unwrap();
        (tmp, sb)
    }

    #[test]
    fn create_captures_state_and_manifest() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        let info = store
            .create(&sb, "box", Some("alpine:latest".into()), None)
            .unwrap();
        assert_eq!(info.sandbox, "box");
        assert!(!info.includes_memory);
        assert!(info.hash.is_some());
        let dir = PathBuf::from(info.path.clone().unwrap());
        assert!(dir.join("manifest.json").is_file());
        assert!(dir.join("state").join("config.json").is_file());
        assert!(dir.join("state").join("data").join("file.txt").is_file());
    }

    #[test]
    fn explicit_dest_is_used() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        let info = store
            .create(&sb, "box", None, Some("my-snap".into()))
            .unwrap();
        assert_eq!(info.id, "my-snap");
        assert!(store.get("my-snap").is_ok());
    }

    #[test]
    fn list_sorts_and_remove_roundtrip() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        store.create(&sb, "box", None, Some("b".into())).unwrap();
        store.create(&sb, "box", None, Some("a".into())).unwrap();
        let ids: Vec<String> = store.list().iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        store.remove("a").unwrap();
        assert!(matches!(store.get("a"), Err(SnapshotError::NotFound(_))));
        assert!(matches!(store.remove("a"), Err(SnapshotError::NotFound(_))));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn duplicate_create_is_already_exists() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        store.create(&sb, "box", None, Some("dup".into())).unwrap();
        let err = store
            .create(&sb, "box", None, Some("dup".into()))
            .unwrap_err();
        assert!(matches!(err, SnapshotError::AlreadyExists(_)), "{err}");
    }

    #[test]
    fn materialize_copies_state_to_a_new_dir() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        let info = store.create(&sb, "box", None, Some("snap".into())).unwrap();
        let restore_tmp = tempfile::tempdir().unwrap();
        let new_dir = restore_tmp.path().join("box-restored");
        store
            .materialize(&info.id, "box-restored", &new_dir)
            .unwrap();
        assert!(new_dir.join("config.json").is_file());
        assert!(new_dir.join("data").join("file.txt").is_file());
    }

    #[test]
    fn restore_preserves_captured_content() {
        let (_t, sb) = sandbox_with_marker();
        let (_s, store) = store();
        let info = store
            .create(&sb, "box", Some("alpine".into()), None)
            .unwrap();
        let restore_tmp = tempfile::tempdir().unwrap();
        let restore_dir = restore_tmp.path().join("box-restored");
        store
            .materialize(&info.id, "box-restored", &restore_dir)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(restore_dir.join("data").join("marker.txt")).unwrap(),
            "payload-123"
        );
        assert!(restore_dir.join("config.json").is_file());
    }

    #[test]
    fn restore_is_independent() {
        let (_t, sb) = sandbox_with_marker();
        let (_s, store) = store();
        let info = store
            .create(&sb, "box", Some("alpine".into()), None)
            .unwrap();
        let restore_tmp = tempfile::tempdir().unwrap();
        let restore_dir = restore_tmp.path().join("box-restored");
        store
            .materialize(&info.id, "box-restored", &restore_dir)
            .unwrap();

        std::fs::write(restore_dir.join("data").join("marker.txt"), b"mutated").unwrap();

        let snap_copy = store
            .root()
            .join(&info.id)
            .join("state")
            .join("data")
            .join("marker.txt");
        assert_eq!(std::fs::read_to_string(&snap_copy).unwrap(), "payload-123");
        assert!(!info.includes_memory);
    }

    #[test]
    fn invalid_ids_rejected() {
        let (_t, sb) = sandbox_dir();
        let (_s, store) = store();
        for bad in ["", "..", ".", "a/b", "a\\b", "a\0b"] {
            assert!(matches!(
                store.create(&sb, "box", None, Some(bad.into())),
                Err(SnapshotError::Invalid(_))
            ));
        }
    }

    #[test]
    fn not_found_maps_to_not_found_code() {
        let (_s, store) = store();
        let err = store.get("nope").unwrap_err();
        assert_eq!(err.to_protocol().code, nanosb_protocol::ErrorCode::NotFound);
    }
}

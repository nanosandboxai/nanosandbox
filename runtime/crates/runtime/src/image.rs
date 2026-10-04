//! OCI image management
//!
//! Handles pulling images from OCI registries, caching layers,
//! and extracting them to create container rootfs.

use crate::auth::CredentialStore;
use crate::config::RegistryConfig;
use crate::error::{Error, Result};
use flate2::read::GzDecoder;
use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::manifest::ImageIndexEntry;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tar::Archive;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

/// Unpack a tar archive to `dest`.
fn unpack_archive<R: Read>(archive: &mut Archive<R>, dest: &Path) -> std::io::Result<()> {
    archive.unpack(dest)
}

/// Set ownership xattrs on all files from a tar archive.
///
/// On macOS, virtiofs passthrough cannot use chown/fchownat (Linux-only).
/// Instead, libkrun's passthrough reads `user.containers.override_stat` xattrs
/// to override file ownership in stat_common(). This function writes that xattr
/// for every file in the tar, preserving the original UID/GID from the OCI image.
///
/// Called as a second pass after unpack_archive() extracts the files.
#[cfg(target_os = "macos")]
fn set_ownership_xattrs<R: Read>(archive: &mut Archive<R>, dest: &Path) {
    const XATTR_KEY: &str = "user.containers.override_stat";

    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read tar entries for xattr pass: {}", e);
            return;
        }
    };

    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = match entry.path() {
            Ok(p) => dest.join(p),
            Err(_) => continue,
        };

        if !path.exists() {
            continue;
        }

        let header = entry.header();
        let uid = header.uid().unwrap_or(0);
        let gid = header.gid().unwrap_or(0);
        let mode = header.mode().unwrap_or(0o644);

        let xattr_value = format!("{}:{}:{:o}", uid, gid, mode);
        if let Err(e) = xattr::set(&path, XATTR_KEY, xattr_value.as_bytes()) {
            tracing::trace!("Failed to set xattr on {}: {}", path.display(), e);
        }
    }
}

/// Get the current platform (os/arch)
fn current_platform() -> (&'static str, &'static str) {
    // All platforms run Linux containers via libkrun
    let os = "linux";

    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };

    (os, arch)
}

/// Type alias for platform resolver function
type PlatformResolver = Box<dyn Fn(&[ImageIndexEntry]) -> Option<String> + Send + Sync>;

/// Create a platform resolver function for OCI client
fn create_platform_resolver() -> PlatformResolver {
    Box::new(|entries: &[ImageIndexEntry]| {
        let (target_os, target_arch) = current_platform();
        debug!("Looking for platform: {}/{}", target_os, target_arch);

        // Find exact match only - no fallbacks
        for entry in entries {
            if let Some(ref platform) = entry.platform {
                if platform.os == target_os && platform.architecture == target_arch {
                    debug!("Found matching platform: {:?}", entry.digest);
                    return Some(entry.digest.clone());
                }
            }
        }

        None
    })
}

/// Image reference parsed from string
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRef {
    /// Registry (e.g., "ghcr.io", "docker.io")
    pub registry: String,
    /// Repository (e.g., "nanosandboxai/dd-agents")
    pub repository: String,
    /// Tag (e.g., "latest")
    pub tag: String,
    /// Digest (optional, e.g., "sha256:abc123...")
    pub digest: Option<String>,
}

impl ImageRef {
    /// Parse an image reference string
    pub fn parse(image: &str) -> Result<Self> {
        // Handle library images (e.g., "alpine" -> "library/alpine")
        // Check if this is a simple image name without registry.
        // A simple name has no `/` or `.` in the part before the first `:`.
        // But we must not treat "localhost:5050/img" as simple - check if
        // the part before `:` looks like a hostname (contains `/` after the port).
        let image_name_part = image.split(':').next().unwrap_or(image);
        let has_registry = image_name_part.contains('/')
            || image_name_part.contains('.')
            || image.starts_with("localhost");
        let image = if !has_registry {
            format!("library/{}", image)
        } else {
            image.to_string()
        };

        // Parse registry and rest
        let (registry, rest) = if image.contains('/')
            && image
                .split('/')
                .next()
                .is_some_and(|s| s.contains('.') || s.contains(':') || s == "localhost")
        {
            let parts: Vec<&str> = image.splitn(2, '/').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            ("docker.io".to_string(), image)
        };

        // Parse digest
        let (repo_tag, digest) = if rest.contains('@') {
            let parts: Vec<&str> = rest.splitn(2, '@').collect();
            (parts[0].to_string(), Some(parts[1].to_string()))
        } else {
            (rest, None)
        };

        // Parse tag
        let (repository, tag) = if repo_tag.contains(':') {
            let parts: Vec<&str> = repo_tag.splitn(2, ':').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (repo_tag, "latest".to_string())
        };

        Ok(Self {
            registry,
            repository,
            tag,
            digest,
        })
    }

    /// Get the full image reference string for OCI
    pub fn full_ref(&self) -> String {
        if let Some(ref digest) = self.digest {
            format!("{}/{}@{}", self.registry, self.repository, digest)
        } else {
            format!("{}/{}:{}", self.registry, self.repository, self.tag)
        }
    }

    /// Convert to oci_distribution Reference
    pub fn to_reference(&self) -> Result<Reference> {
        let ref_str = self.full_ref();
        Reference::try_from(ref_str.as_str())
            .map_err(|e| {
                error!("Invalid image reference {}: {}", self.full_ref(), e);
                Error::InvalidImageRef(format!("{}: {}", self.full_ref(), e))
            })
    }
}

/// Information about a cached image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Image reference
    pub reference: ImageRef,
    /// Size in bytes
    pub size: u64,
    /// When the image was pulled
    pub pulled_at: chrono::DateTime<chrono::Utc>,
    /// Layer digests
    pub layers: Vec<String>,
    /// Config digest
    pub config_digest: String,
}

/// Manages OCI images
#[allow(dead_code)]
pub struct ImageManager {
    /// Base cache directory (~/.nanosandbox)
    cache_dir: PathBuf,
    /// Default OCI distribution client (HTTPS)
    default_client: Client,
    /// Per-registry clients (for insecure registries)
    registry_clients: HashMap<String, Client>,
    /// Credential store for authenticated pulls
    credentials: Arc<CredentialStore>,
    /// Per-registry configurations (stored for future use)
    registry_configs: Vec<RegistryConfig>,
    /// Per-image pull coordination: serializes concurrent pulls of the same image
    /// so only the first caller downloads while others wait and use cached blobs.
    inflight: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl ImageManager {
    /// Create a new image manager with specified cache directory
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        Self::with_options(cache_dir, None, Vec::new())
    }

    /// Create a new image manager with credentials and registry configs
    pub fn with_options(
        cache_dir: PathBuf,
        credentials: Option<CredentialStore>,
        registry_configs: Vec<RegistryConfig>,
    ) -> Result<Self> {
        // Create cache subdirectories
        let blobs_dir = cache_dir.join("blobs").join("sha256");
        let extracted_dir = cache_dir.join("extracted");
        let manifests_dir = cache_dir.join("manifests");

        fs::create_dir_all(&blobs_dir)?;
        fs::create_dir_all(&extracted_dir)?;
        fs::create_dir_all(&manifests_dir)?;

        // Create default OCI client with platform resolver for multi-arch images
        let config = ClientConfig {
            protocol: ClientProtocol::Https,
            platform_resolver: Some(create_platform_resolver()),
            ..Default::default()
        };
        let default_client = Client::new(config);

        // Create per-registry clients for special configurations
        let mut registry_clients = HashMap::new();
        for reg_config in &registry_configs {
            let protocol = if reg_config.insecure {
                ClientProtocol::Http
            } else {
                ClientProtocol::Https
            };

            let client_config = ClientConfig {
                protocol,
                platform_resolver: Some(create_platform_resolver()),
                accept_invalid_certificates: reg_config.skip_tls_verify,
                ..Default::default()
            };
            registry_clients.insert(reg_config.host.clone(), Client::new(client_config));
        }

        // Auto-configure HTTP for localhost registries (common dev pattern)
        for port in &["5000", "5050", "5001"] {
            let host = format!("localhost:{}", port);
            registry_clients.entry(host).or_insert_with(|| {
                let localhost_config = ClientConfig {
                    protocol: ClientProtocol::Http,
                    platform_resolver: Some(create_platform_resolver()),
                    ..Default::default()
                };
                Client::new(localhost_config)
            });
        }

        // Load credentials or use provided
        let credentials = Arc::new(credentials.unwrap_or_else(|| {
            CredentialStore::load().unwrap_or_else(|_| CredentialStore::empty())
        }));

        Ok(Self {
            cache_dir,
            default_client,
            registry_clients,
            credentials,
            registry_configs,
            inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Create with default cache directory (~/.nanosandbox)
    pub fn with_default_cache() -> Result<Self> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".nanosandbox");

        Self::new(cache_dir)
    }

    /// Create with default cache and loaded credentials
    pub fn with_default_cache_and_auth() -> Result<Self> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".nanosandbox");

        let credentials = CredentialStore::load().ok();
        Self::with_options(cache_dir, credentials, Vec::new())
    }

    /// Get the client for a specific registry
    fn get_client(&self, registry: &str) -> &Client {
        self.registry_clients
            .get(registry)
            .unwrap_or(&self.default_client)
    }

    /// Get authentication for a registry
    fn get_auth(&self, registry: &str) -> RegistryAuth {
        self.credentials.get_auth(registry)
    }

    /// Get the cache directory
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Get the blobs directory
    pub fn blobs_dir(&self) -> PathBuf {
        self.cache_dir.join("blobs").join("sha256")
    }

    /// Get the extracted layers directory
    pub fn extracted_dir(&self) -> PathBuf {
        self.cache_dir.join("extracted")
    }

    /// Persist image metadata to the manifests cache.
    ///
    /// Writes an `ImageInfo` JSON file to `manifests/{registry}/{repo}/{tag}`.
    /// Uses atomic temp+rename for crash safety.
    fn save_manifest(&self, pulled: &PulledImage) -> Result<()> {
        let ref_ = &pulled.reference;
        let manifest_dir = self
            .cache_dir
            .join("manifests")
            .join(ref_.registry.replace('/', "_"))
            .join(ref_.repository.replace('/', "_"));

        fs::create_dir_all(&manifest_dir)?;

        let info = ImageInfo {
            reference: ref_.clone(),
            size: pulled.size,
            pulled_at: chrono::Utc::now(),
            layers: pulled.layers.clone(),
            config_digest: pulled.config_digest.clone(),
        };

        let content = serde_json::to_string_pretty(&info)?;
        let final_path = manifest_dir.join(&ref_.tag);
        let temp_path = manifest_dir.join(format!("{}.tmp.{}", ref_.tag, uuid::Uuid::new_v4()));

        fs::write(&temp_path, &content)?;
        fs::rename(&temp_path, &final_path)?;

        debug!("Saved manifest metadata: {}", ref_.full_ref());
        Ok(())
    }

    /// Verify that a file's SHA256 matches the expected digest.
    ///
    /// `expected_digest` should be in the form "sha256:hex..." or just "hex...".
    fn verify_blob_sha256(path: &Path, expected_digest: &str) -> Result<()> {
        let expected_hex = expected_digest
            .strip_prefix("sha256:")
            .unwrap_or(expected_digest);

        let mut file = File::open(path).map_err(|e| {
            error!("Failed to open blob for verification {:?}: {}", path, e);
            Error::ImagePullFailed(format!("Open blob for verification: {}", e))
        })?;

        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).map_err(|e| {
            error!("Failed to hash blob {:?}: {}", path, e);
            Error::ImagePullFailed(format!("Hash blob: {}", e))
        })?;

        let actual_hex = format!("{:x}", hasher.finalize());
        if actual_hex != expected_hex {
            error!(
                "Blob integrity check failed: expected sha256:{}, got sha256:{}",
                expected_hex, actual_hex
            );
            return Err(Error::ImagePullFailed(format!(
                "Blob integrity check failed: expected sha256:{}, got sha256:{}",
                expected_hex, actual_hex
            )));
        }

        Ok(())
    }

    /// Pull an image from a registry using manifest-first selective download.
    ///
    /// This optimized implementation:
    /// 1. Fetches the manifest (fast metadata operation)
    /// 2. Checks which layers are already cached locally
    /// 3. Downloads only missing layers in parallel
    /// 4. Downloads config blob if not cached
    ///
    /// For fully cached images this reduces pull time from minutes to seconds.
    pub async fn pull(&self, image: &str) -> Result<PulledImage> {
        let image_ref = ImageRef::parse(image)?;

        // Acquire per-image lock to serialize concurrent pulls of the same image.
        // If another task is already pulling this image, we wait until it finishes,
        // then proceed — the layer cache check below will find everything cached.
        let image_key = image_ref.full_ref();
        let per_image_lock = {
            let mut inflight = self.inflight.lock().await;
            inflight
                .entry(image_key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _pull_guard = per_image_lock.lock().await;

        let reference = image_ref.to_reference()?;
        let start = Instant::now();

        info!("Pulling image: {}", image_ref.full_ref());

        // Get appropriate client and auth for this registry
        let client = self.get_client(&image_ref.registry);
        let auth = self.get_auth(&image_ref.registry);

        match &auth {
            RegistryAuth::Anonymous => debug!("Using anonymous auth for {}", image_ref.registry),
            RegistryAuth::Basic(user, _) => {
                debug!(
                    "Using authenticated pull as {} for {}",
                    user, image_ref.registry
                )
            }
        }

        // Step 1: Pull manifest only (fast metadata operation)
        // This also handles multi-arch image index resolution via platform_resolver
        // Retry up to 3 times for transient registry/network errors
        let manifest_start = Instant::now();
        let mut last_err = None;
        let mut manifest_result = None;
        for attempt in 1..=3u32 {
            match client.pull_image_manifest(&reference, &auth).await {
                Ok(result) => {
                    manifest_result = Some(result);
                    break;
                }
                Err(e) => {
                    let err_msg = format!("{}", e);
                    if attempt < 3 {
                        debug!(
                            "Manifest fetch attempt {}/3 failed: {}, retrying...",
                            attempt, err_msg
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                            .await;
                    }
                    last_err = Some(err_msg);
                }
            }
        }
        let (manifest, manifest_digest) = manifest_result.ok_or_else(|| {
            let msg = last_err.unwrap_or_else(|| "unknown error".to_string());
            error!("Failed to fetch manifest for {}: {}", image_ref.full_ref(), msg);
            Error::ImagePullFailed(format!(
                "{}: {}",
                image_ref.full_ref(),
                msg
            ))
        })?;
        debug!("Manifest fetched in {:?}", manifest_start.elapsed());

        // Step 2: Check which layers are already cached locally
        let mut missing_layers = Vec::new();
        let mut layer_digests = Vec::new();
        let mut cached_count = 0;

        for layer_desc in &manifest.layers {
            let digest = &layer_desc.digest;
            let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
            layer_digests.push(digest.clone());

            let blob_path = self.blobs_dir().join(digest_short);
            if blob_path.exists() {
                debug!(
                    "Layer {} already cached ({} bytes)",
                    digest_short, layer_desc.size
                );
                cached_count += 1;
            } else {
                missing_layers.push(layer_desc.clone());
            }
        }

        info!(
            "{}/{} layers cached, {} to download",
            cached_count,
            manifest.layers.len(),
            missing_layers.len()
        );

        // Step 3: Download missing layers in parallel
        if !missing_layers.is_empty() {
            let download_start = Instant::now();
            info!("Downloading {} layers in parallel...", missing_layers.len());

            let mut handles = Vec::new();
            for layer_desc in missing_layers {
                let client_clone = client.clone();
                let reference_clone = reference.clone();
                let blobs_dir = self.blobs_dir();

                handles.push(tokio::spawn(async move {
                    let digest = &layer_desc.digest;
                    let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
                    let blob_path = blobs_dir.join(digest_short);
                    // Write to temp file first, then atomic rename for crash safety
                    let temp_path =
                        blobs_dir.join(format!("{}.dl.{}", digest_short, uuid::Uuid::new_v4()));

                    debug!(
                        "Downloading layer {} ({} bytes)...",
                        digest_short, layer_desc.size
                    );

                    // Ensure blobs directory exists (defensive: may have been cleaned
                    // between ImageManager::new() and this download task)
                    tokio::fs::create_dir_all(&blobs_dir)
                        .await
                        .map_err(|e| {
                            error!("Failed to create blobs dir {:?}: {}", blobs_dir, e);
                            Error::ImagePullFailed(format!("Create blobs dir: {}", e))
                        })?;

                    let mut file = tokio::fs::File::create(&temp_path)
                        .await
                        .map_err(|e| {
                            error!("Failed to create temp download file {:?}: {}", temp_path, e);
                            Error::ImagePullFailed(format!("Create temp file: {}", e))
                        })?;

                    client_clone
                        .pull_blob(&reference_clone, &layer_desc, &mut file)
                        .await
                        .map_err(|e| {
                            error!("Failed to download layer {}: {}", digest_short, e);
                            Error::ImagePullFailed(format!(
                                "Download layer {}: {}",
                                digest_short, e
                            ))
                        })?;

                    // Flush and sync to disk before rename (pull_blob doesn't flush)
                    file.shutdown().await.map_err(|e| {
                        error!("Failed to flush layer {} to disk: {}", digest_short, e);
                        Error::ImagePullFailed(format!("Flush layer {}: {}", digest_short, e))
                    })?;
                    drop(file);

                    // Check if blob already exists (another concurrent pull may have completed)
                    // This prevents race conditions when multiple sandboxes pull the same image.
                    if !blob_path.exists() {
                        // Atomic rename from temp to final path
                        tokio::fs::rename(&temp_path, &blob_path)
                            .await
                            .map_err(|e| {
                                let temp_exists = std::path::Path::new(&temp_path).exists();
                                let dir_exists = std::path::Path::new(&blobs_dir).exists();
                                error!(
                                    "Failed to rename blob {} (temp_exists={}, dir_exists={}): {}",
                                    digest_short, temp_exists, dir_exists, e
                                );
                                Error::ImagePullFailed(format!(
                                    "Rename blob {}: {} (temp_exists={}, dir_exists={})",
                                    digest_short, e, temp_exists, dir_exists
                                ))
                            })?;
                        debug!("Downloaded layer: {}", digest_short);
                    } else {
                        // Another concurrent download completed first, clean up our temp file
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        debug!(
                            "Layer {} already exists (concurrent download)",
                            digest_short
                        );
                    }

                    // Verify blob integrity against expected SHA256 digest
                    let verify_path = blob_path.clone();
                    let verify_digest = digest.to_string();
                    tokio::task::spawn_blocking(move || {
                        ImageManager::verify_blob_sha256(&verify_path, &verify_digest)
                    })
                    .await
                    .map_err(|e| {
                        error!("Blob verification task join failed for {}: {}", digest_short, e);
                        Error::ImagePullFailed(format!("Verification task: {}", e))
                    })??;

                    Ok::<_, Error>(())
                }));
            }

            // Await all parallel downloads
            for handle in handles {
                handle
                    .await
                    .map_err(|e| {
                        error!("Layer download task join error for {}: {}", image_ref.full_ref(), e);
                        Error::ImagePullFailed(format!("Task join error: {}", e))
                    })??;
            }

            info!("All layers downloaded in {:?}", download_start.elapsed());
        }

        // Step 4: Save config blob if not cached (atomic temp+rename)
        let config_digest = manifest.config.digest.clone();
        let config_digest_short = config_digest
            .strip_prefix("sha256:")
            .unwrap_or(&config_digest);
        let config_path = self.blobs_dir().join(config_digest_short);

        if !config_path.exists() {
            debug!("Downloading config blob: {}", config_digest_short);
            let temp_path = self.blobs_dir().join(format!(
                "{}.dl.{}",
                config_digest_short,
                uuid::Uuid::new_v4()
            ));
            let mut file = tokio::fs::File::create(&temp_path)
                .await
                .map_err(|e| {
                    error!("Failed to create temp config file for {}: {}", image_ref.full_ref(), e);
                    Error::ImagePullFailed(format!("Create temp config file: {}", e))
                })?;
            client
                .pull_blob(&reference, &manifest.config, &mut file)
                .await
                .map_err(|e| {
                    error!("Failed to pull config blob for {}: {}", image_ref.full_ref(), e);
                    Error::ImagePullFailed(format!("Pull config: {}", e))
                })?;
            file.shutdown()
                .await
                .map_err(|e| {
                    error!("Failed to flush config blob for {}: {}", image_ref.full_ref(), e);
                    Error::ImagePullFailed(format!("Flush config blob: {}", e))
                })?;
            drop(file);

            if !config_path.exists() {
                tokio::fs::rename(&temp_path, &config_path)
                    .await
                    .map_err(|e| {
                        error!(
                            "Failed to rename config blob {} for {}: {}",
                            config_digest_short, image_ref.full_ref(), e
                        );
                        Error::ImagePullFailed(format!(
                            "Rename config blob {}: {}",
                            config_digest_short, e
                        ))
                    })?;
            } else {
                let _ = tokio::fs::remove_file(&temp_path).await;
            }

            // Verify config blob integrity
            let verify_path = config_path.clone();
            let verify_digest = config_digest.clone();
            tokio::task::spawn_blocking(move || {
                Self::verify_blob_sha256(&verify_path, &verify_digest)
            })
            .await
            .map_err(|e| {
                error!("Config blob verification task failed for {}: {}", image_ref.full_ref(), e);
                Error::ImagePullFailed(format!("Verification task: {}", e))
            })??;
        }

        let total_size: u64 = manifest.layers.iter().map(|l| l.size as u64).sum();

        info!(
            "Image ready: {} layers ({} bytes), completed in {:?}",
            layer_digests.len(),
            total_size,
            start.elapsed()
        );

        // Step 5: Persist manifest metadata so exists()/list() work
        let pulled = PulledImage {
            reference: image_ref,
            layers: layer_digests,
            config_digest: manifest_digest,
            size: total_size,
        };

        if let Err(e) = self.save_manifest(&pulled) {
            warn!("Failed to save manifest metadata: {}", e);
        }

        Ok(pulled)
    }

    /// Check if an image exists locally (all layers cached)
    pub async fn exists(&self, image: &str) -> Result<bool> {
        let image_ref = ImageRef::parse(image)?;
        let manifest_path = self
            .cache_dir
            .join("manifests")
            .join(image_ref.registry.replace('/', "_"))
            .join(image_ref.repository.replace('/', "_"))
            .join(&image_ref.tag);

        Ok(manifest_path.exists())
    }

    /// Check if a layer blob exists in cache
    pub fn layer_exists(&self, digest: &str) -> bool {
        let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.blobs_dir().join(digest_short).exists()
    }

    /// Extract a layer to a destination directory
    pub fn extract_layer(&self, digest: &str, dest: &Path) -> Result<()> {
        let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
        let blob_path = self.blobs_dir().join(digest_short);

        if !blob_path.exists() {
            error!("Layer blob not found in cache: {}", digest);
            return Err(Error::LayerExtractionFailed(format!(
                "Layer blob not found: {}",
                digest
            )));
        }

        debug!("Extracting layer {} to {:?}", digest_short, dest);

        // Check if it's gzipped by reading magic bytes
        let mut header = [0u8; 2];
        {
            let mut peek_file = File::open(&blob_path)?;
            let _ = peek_file.read_exact(&mut header);
        }

        let is_gzipped = header[0] == 0x1f && header[1] == 0x8b;

        if is_gzipped {
            // Decompress gzip and extract tar
            let file = File::open(&blob_path)?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);
            archive.set_preserve_permissions(cfg!(unix));
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            unpack_archive(&mut archive, dest).map_err(|e| {
                error!("Failed to unpack gzip layer {}: {}", digest_short, e);
                Error::LayerExtractionFailed(format!("Failed to unpack gzip: {}", e))
            })?;

            // macOS: second pass to set ownership xattrs from tar headers
            #[cfg(target_os = "macos")]
            {
                let file2 = File::open(&blob_path)?;
                let decoder2 = GzDecoder::new(file2);
                let mut archive2 = Archive::new(decoder2);
                set_ownership_xattrs(&mut archive2, dest);
            }
        } else {
            // Try as plain tar
            let file = File::open(&blob_path)?;
            let mut archive = Archive::new(file);
            archive.set_preserve_permissions(cfg!(unix));
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            unpack_archive(&mut archive, dest).map_err(|e| {
                error!("Failed to unpack tar layer {}: {}", digest_short, e);
                Error::LayerExtractionFailed(format!("Failed to unpack tar: {}", e))
            })?;

            // macOS: second pass to set ownership xattrs from tar headers
            #[cfg(target_os = "macos")]
            {
                let file2 = File::open(&blob_path)?;
                let mut archive2 = Archive::new(file2);
                set_ownership_xattrs(&mut archive2, dest);
            }
        }

        Ok(())
    }

    /// Create a rootfs by extracting all layers in order with parallel decompression.
    ///
    /// This optimized implementation:
    /// 1. Checks for a cached "golden rootfs" keyed by manifest digest
    /// 2. On cache hit: clones the golden rootfs via APFS clonefile (CoW, near-instant)
    /// 3. On cache miss: decompresses layers in parallel, extracts sequentially,
    ///    then caches the result as a golden rootfs for future reuse
    ///
    /// The `manifest_digest` (if provided) is used as the cache key for the
    /// golden rootfs stored in `~/.nanosandbox/extracted/{digest}/`.
    pub fn create_rootfs(
        &self,
        layers: &[String],
        dest: &Path,
        manifest_digest: Option<&str>,
    ) -> Result<()> {
        let start = Instant::now();
        info!("Creating rootfs at {:?} from {} layers", dest, layers.len());

        // Fast path: use cached golden rootfs if available.
        if let Some(digest) = manifest_digest {
            let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
            let golden = self.extracted_dir().join(digest_short);

            if golden.exists() {
                info!(
                    "Using cached rootfs for {} ({} layers)",
                    &digest_short[..12.min(digest_short.len())],
                    layers.len()
                );

                clone_golden_to_sandbox(&golden, dest)?;
                info!("Rootfs cloned from cache in {:?}", start.elapsed());
                return Ok(());
            }
        }

        fs::create_dir_all(dest).map_err(|e| {
            error!("Failed to create rootfs dir {}: {}", dest.display(), e);
            Error::LayerExtractionFailed(format!("Create rootfs dir {}: {}", dest.display(), e))
        })?;

        let blobs_dir = self.blobs_dir();
        let num_layers = layers.len();

        // Phase 1: Decompress all gzipped layers in parallel
        // Decompressed tars are cached as {digest}.tar for future reuse
        let decompress_start = Instant::now();
        let tar_paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let handles: Vec<_> = layers
                .iter()
                .enumerate()
                .map(|(i, digest)| {
                    let blobs_dir = &blobs_dir;
                    scope.spawn(move || -> Result<PathBuf> {
                        let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
                        let blob_path = blobs_dir.join(digest_short);

                        if !blob_path.exists() {
                            error!("Layer blob not found during rootfs creation: {}", digest);
                            return Err(Error::LayerExtractionFailed(format!(
                                "Layer blob not found: {}",
                                digest
                            )));
                        }

                        // Check if layer is gzipped by reading magic bytes
                        let mut header = [0u8; 2];
                        {
                            let mut peek = File::open(&blob_path).map_err(|e| {
                                error!("Failed to open blob for peek {:?}: {}", blob_path, e);
                                Error::LayerExtractionFailed(format!(
                                    "Open blob {}: {}",
                                    blob_path.display(),
                                    e
                                ))
                            })?;
                            let _ = peek.read_exact(&mut header);
                        }
                        let is_gzipped = header[0] == 0x1f && header[1] == 0x8b;

                        if is_gzipped {
                            let tar_path = blobs_dir.join(format!("{}.tar", digest_short));

                            // Use cached decompressed tar if available
                            if !tar_path.exists() {
                                debug!(
                                    "Decompressing layer {}/{}: {}",
                                    i + 1,
                                    num_layers,
                                    digest_short
                                );
                                let in_file = File::open(&blob_path).map_err(|e| {
                                    error!(
                                        "Failed to open blob for decompression {:?}: {}",
                                        blob_path, e
                                    );
                                    Error::LayerExtractionFailed(format!(
                                        "Open blob for decompress {}: {}",
                                        blob_path.display(),
                                        e
                                    ))
                                })?;
                                let mut decoder = GzDecoder::new(in_file);
                                // Write to temp file for atomic rename
                                let temp_path = blobs_dir.join(format!(
                                    "{}.tar.tmp.{}",
                                    digest_short,
                                    uuid::Uuid::new_v4()
                                ));
                                let mut out_file = File::create(&temp_path).map_err(|e| {
                                    error!(
                                        "Failed to create temp tar file {:?}: {}",
                                        temp_path, e
                                    );
                                    Error::LayerExtractionFailed(format!(
                                        "Create temp tar {}: {}",
                                        temp_path.display(),
                                        e
                                    ))
                                })?;
                                std::io::copy(&mut decoder, &mut out_file).map_err(|e| {
                                    error!("Failed to decompress layer {}: {}", digest_short, e);
                                    Error::LayerExtractionFailed(format!(
                                        "Decompress layer {}: {}",
                                        digest_short, e
                                    ))
                                })?;
                                drop(out_file); // Close file before rename

                                // Check if tar already exists (concurrent decompress may have completed)
                                if !tar_path.exists() {
                                    // Atomic rename to final path
                                    fs::rename(&temp_path, &tar_path).map_err(|e| {
                                        error!(
                                            "Failed to rename decompressed tar {:?} -> {:?}: {}",
                                            temp_path, tar_path, e
                                        );
                                        Error::LayerExtractionFailed(format!(
                                            "Rename decompressed tar {} -> {}: {}",
                                            temp_path.display(),
                                            tar_path.display(),
                                            e
                                        ))
                                    })?;
                                } else {
                                    // Another concurrent decompress completed first, clean up temp file
                                    let _ = fs::remove_file(&temp_path);
                                    debug!(
                                        "Layer {} tar already exists (concurrent decompress)",
                                        digest_short
                                    );
                                }
                            } else {
                                debug!(
                                    "Layer {}/{} already decompressed: {}",
                                    i + 1,
                                    num_layers,
                                    digest_short
                                );
                            }

                            Ok(tar_path)
                        } else {
                            // Plain tar, use the blob directly
                            debug!(
                                "Layer {}/{} is plain tar: {}",
                                i + 1,
                                num_layers,
                                digest_short
                            );
                            Ok(blob_path)
                        }
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        error!("A decompression thread panicked during rootfs creation");
                        Err(Error::LayerExtractionFailed(
                            "Thread panicked during decompression".to_string(),
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;

        debug!(
            "Parallel decompression completed in {:?}",
            decompress_start.elapsed()
        );

        // Phase 2: Extract uncompressed tars sequentially (preserves layer ordering)
        let extract_start = Instant::now();
        for (i, tar_path) in tar_paths.iter().enumerate() {
            debug!(
                "Extracting layer {}/{}: {:?}",
                i + 1,
                num_layers,
                tar_path.file_name().unwrap_or_default()
            );
            let file = File::open(tar_path).map_err(|e| {
                error!("Failed to open tar layer {}/{} {:?}: {}", i + 1, num_layers, tar_path.file_name().unwrap_or_default(), e);
                Error::LayerExtractionFailed(format!("Open tar {}: {}", tar_path.display(), e))
            })?;
            let mut archive = Archive::new(file);
            archive.set_preserve_permissions(cfg!(unix));
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            unpack_archive(&mut archive, dest).map_err(|e| {
                error!("Failed to unpack tar layer {}/{} {:?}: {}", i + 1, num_layers, tar_path.file_name().unwrap_or_default(), e);
                Error::LayerExtractionFailed(format!("Failed to unpack tar: {}", e))
            })?;

            // macOS: second pass to set ownership xattrs from tar headers
            #[cfg(target_os = "macos")]
            {
                let file2 = File::open(tar_path).map_err(|e| {
                    Error::LayerExtractionFailed(format!("Reopen tar for xattr: {}", e))
                })?;
                let mut archive2 = Archive::new(file2);
                set_ownership_xattrs(&mut archive2, dest);
            }
        }

        debug!(
            "Sequential extraction completed in {:?}",
            extract_start.elapsed()
        );
        info!("Rootfs extracted in {:?}", start.elapsed());

        // Cache this rootfs as golden image for future reuse
        if let Some(digest) = manifest_digest {
            let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
            let golden = self.extracted_dir().join(digest_short);
            if !golden.exists() {
                let temp = self.extracted_dir().join(format!(
                    "{}.tmp.{}",
                    digest_short,
                    uuid::Uuid::new_v4()
                ));
                match clone_dir(dest, &temp) {
                    Ok(()) => {
                        if golden.exists() {
                            // Another process completed first — discard our copy
                            let _ = fs::remove_dir_all(&temp);
                        } else if let Err(e) = fs::rename(&temp, &golden) {
                            warn!("Failed to cache golden rootfs: {}", e);
                            let _ = fs::remove_dir_all(&temp);
                        } else {
                            info!(
                                "Cached golden rootfs: {}",
                                &digest_short[..12.min(digest_short.len())]
                            );
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create golden rootfs cache: {}", e);
                        let _ = fs::remove_dir_all(&temp);
                    }
                }
            }
        }

        info!("Rootfs ready in {:?}", start.elapsed());
        Ok(())
    }

}

/// Clone a golden rootfs to a per-sandbox rootfs directory.
///
/// On macOS: uses APFS clonefile (instant CoW), falls back to recursive copy.
///
/// On Linux: recursive copy.
fn clone_golden_to_sandbox(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| {
            Error::LayerExtractionFailed(format!(
                "Failed to remove dest before clone {}: {}",
                dest.display(),
                e
            ))
        })?;
    }

    #[cfg(target_os = "macos")]
    {
        if try_clonefile(src, dest) {
            return Ok(());
        }
    }

    copy_dir_recursive(src, dest)
}

/// Clone a directory using platform-optimal copy (always creates a real copy).
///
/// Used for cache creation where we need a real directory, not a junction.
fn clone_dir(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|e| {
            Error::LayerExtractionFailed(format!(
                "Failed to remove dest before clone {}: {}",
                dest.display(),
                e
            ))
        })?;
    }

    #[cfg(target_os = "macos")]
    {
        if try_clonefile(src, dest) {
            return Ok(());
        }
    }

    copy_dir_recursive(src, dest)
}

/// Attempt macOS APFS clonefile(2). Returns true on success.
#[cfg(target_os = "macos")]
fn try_clonefile(src: &Path, dest: &Path) -> bool {
    use std::ffi::CString;

    let src_c = match CString::new(src.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let dst_c = match CString::new(dest.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // clonefile(2) clones an entire directory tree with APFS CoW.
    // int clonefile(const char *src, const char *dst, int flags);
    extern "C" {
        fn clonefile(
            src: *const libc::c_char,
            dst: *const libc::c_char,
            flags: libc::c_int,
        ) -> libc::c_int;
    }

    let ret = unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if ret == 0 {
        debug!(
            "clonefile succeeded: {} -> {}",
            src.display(),
            dest.display()
        );
        true
    } else {
        let err = std::io::Error::last_os_error();
        debug!("clonefile failed ({}), falling back to recursive copy", err);
        false
    }
}

/// Recursive directory copy preserving permissions and symlinks.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).map_err(|e| {
        Error::LayerExtractionFailed(format!("Create dir {}: {}", dest.display(), e))
    })?;

    // Copy source directory permissions
    if let Ok(meta) = fs::metadata(src) {
        let _ = fs::set_permissions(dest, meta.permissions());
    }

    for entry in fs::read_dir(src)
        .map_err(|e| Error::LayerExtractionFailed(format!("Read dir {}: {}", src.display(), e)))?
    {
        let entry = entry.map_err(|e| {
            Error::LayerExtractionFailed(format!("Read entry in {}: {}", src.display(), e))
        })?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| {
            Error::LayerExtractionFailed(format!("Get file type {}: {}", src_path.display(), e))
        })?;

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(&src_path).map_err(|e| {
                Error::LayerExtractionFailed(format!("Read symlink {}: {}", src_path.display(), e))
            })?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dest_path).map_err(|e| {
                Error::LayerExtractionFailed(format!(
                    "Create symlink {} -> {}: {}",
                    dest_path.display(),
                    target.display(),
                    e
                ))
            })?;
        } else {
            fs::copy(&src_path, &dest_path).map_err(|e| {
                Error::LayerExtractionFailed(format!(
                    "Copy {} -> {}: {}",
                    src_path.display(),
                    dest_path.display(),
                    e
                ))
            })?;
        }
    }
    Ok(())
}

impl ImageManager {
    /// List cached images
    pub async fn list(&self) -> Result<Vec<ImageInfo>> {
        let manifests_dir = self.cache_dir.join("manifests");
        let mut images = Vec::new();

        if !manifests_dir.exists() {
            return Ok(images);
        }

        // Walk the manifests directory
        for registry_entry in fs::read_dir(&manifests_dir)? {
            let registry_entry = registry_entry?;
            if !registry_entry.file_type()?.is_dir() {
                continue;
            }

            for repo_entry in fs::read_dir(registry_entry.path())? {
                let repo_entry = repo_entry?;
                if !repo_entry.file_type()?.is_dir() {
                    continue;
                }

                for tag_entry in fs::read_dir(repo_entry.path())? {
                    let tag_entry = tag_entry?;
                    let manifest_path = tag_entry.path();

                    if let Ok(content) = fs::read_to_string(&manifest_path) {
                        if let Ok(info) = serde_json::from_str::<ImageInfo>(&content) {
                            images.push(info);
                        }
                    }
                }
            }
        }

        Ok(images)
    }

    /// Remove a cached image
    pub async fn remove(&self, image: &str) -> Result<()> {
        let image_ref = ImageRef::parse(image)?;

        // Remove manifest
        let manifest_path = self
            .cache_dir
            .join("manifests")
            .join(image_ref.registry.replace('/', "_"))
            .join(image_ref.repository.replace('/', "_"))
            .join(&image_ref.tag);

        if manifest_path.exists() {
            fs::remove_file(&manifest_path)?;
        }

        // Note: We don't remove blobs as they may be shared by other images.
        // Use `nanosb cache prune --all` to garbage-collect orphaned blobs.

        info!("Removed image: {}", image_ref.full_ref());
        Ok(())
    }

    /// Prune cached data to reclaim disk space.
    ///
    /// Default (`all=false`): removes orphaned bundles, decompressed `.tar` files,
    /// and stale temp files. Compressed blobs are kept for fast re-extraction.
    ///
    /// With `all=true`: also removes all blobs and manifests (full cache reset).
    pub fn prune(&self, all: bool) -> Result<PruneResult> {
        let mut result = PruneResult::default();

        // 1. Remove orphaned bundles (bundles with no registry entry, older than 1 hour)
        let bundles_dir = self.cache_dir.join("bundles");
        if bundles_dir.exists() {
            if let Ok(registry) = crate::registry::SandboxRegistry::new() {
                match registry.cleanup_orphaned_bundles(&bundles_dir) {
                    Ok(count) => {
                        // Re-scan to get byte count (cleanup already removed them)
                        result.orphaned_bundles = count;
                    }
                    Err(e) => warn!("Failed to cleanup orphaned bundles: {}", e),
                }
                // Also cleanup stale registry entries
                if let Err(e) = registry.cleanup_stale() {
                    warn!("Failed to cleanup stale registry entries: {}", e);
                }
            }
        }

        // 2. Remove decompressed .tar files and stale temp files
        let blobs_dir = self.blobs_dir();
        if blobs_dir.exists() {
            if let Ok(entries) = fs::read_dir(&blobs_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

                    if name.ends_with(".tar") {
                        debug!("Removing decompressed tar: {}", name);
                        if fs::remove_file(entry.path()).is_ok() {
                            result.decompressed_tars += 1;
                            result.decompressed_tars_bytes += size;
                        }
                    } else if name.contains(".dl.") || name.contains(".tar.tmp.") {
                        debug!("Removing stale temp file: {}", name);
                        if fs::remove_file(entry.path()).is_ok() {
                            result.stale_temps += 1;
                            result.stale_temps_bytes += size;
                        }
                    }
                }
            }
        }

        // 3. Remove stale golden rootfs temp dirs; if --all, remove all golden rootfs
        let extracted_dir = self.extracted_dir();
        if extracted_dir.exists() {
            if let Ok(entries) = fs::read_dir(&extracted_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_temp = name.contains(".tmp.");
                    if is_temp || all {
                        let size = dir_size(&entry.path());
                        if fs::remove_dir_all(entry.path()).is_ok() {
                            result.golden_rootfs += 1;
                            result.golden_rootfs_bytes += size;
                            if is_temp {
                                debug!("Removed stale golden rootfs temp: {}", name);
                            } else {
                                debug!("Removed golden rootfs cache: {}", name);
                            }
                        }
                    }
                }
            }
        }

        // 4. If --all, remove all blobs and manifests
        if all {
            if blobs_dir.exists() {
                if let Ok(entries) = fs::read_dir(&blobs_dir) {
                    for entry in entries.flatten() {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        if fs::remove_file(entry.path()).is_ok() {
                            result.blobs += 1;
                            result.blobs_bytes += size;
                        }
                    }
                }
            }

            let manifests_dir = self.cache_dir.join("manifests");
            if manifests_dir.exists() {
                result.manifests = count_files_recursive(&manifests_dir);
                let _ = fs::remove_dir_all(&manifests_dir);
                let _ = fs::create_dir_all(&manifests_dir);
            }
        }

        result.total_bytes = result.orphaned_bundles_bytes
            + result.decompressed_tars_bytes
            + result.stale_temps_bytes
            + result.golden_rootfs_bytes
            + result.blobs_bytes;

        info!(
            "Cache prune complete: reclaimed {} bytes",
            result.total_bytes
        );
        Ok(result)
    }
}

/// Result of a cache prune operation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PruneResult {
    pub orphaned_bundles: usize,
    pub orphaned_bundles_bytes: u64,
    pub decompressed_tars: usize,
    pub decompressed_tars_bytes: u64,
    pub stale_temps: usize,
    pub stale_temps_bytes: u64,
    pub golden_rootfs: usize,
    pub golden_rootfs_bytes: u64,
    pub blobs: usize,
    pub blobs_bytes: u64,
    pub manifests: usize,
    pub total_bytes: u64,
}

/// Calculate total size of a directory recursively
fn dir_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    if e.path().is_dir() {
                        dir_size(&e.path())
                    } else {
                        e.metadata().map(|m| m.len()).unwrap_or(0)
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Count files recursively in a directory
fn count_files_recursive(path: &Path) -> usize {
    fs::read_dir(path)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    if e.path().is_dir() {
                        count_files_recursive(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Result of pulling an image
#[derive(Debug, Clone)]
pub struct PulledImage {
    /// Image reference
    pub reference: ImageRef,
    /// Layer digests in order
    pub layers: Vec<String>,
    /// Config digest
    pub config_digest: String,
    /// Total size in bytes
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ref_parse() {
        let ref1 = ImageRef::parse("alpine").unwrap();
        assert_eq!(ref1.registry, "docker.io");
        assert_eq!(ref1.repository, "library/alpine");
        assert_eq!(ref1.tag, "latest");

        let ref2 = ImageRef::parse("ghcr.io/nanosandboxai/agents-registry/claude:v1.0").unwrap();
        assert_eq!(ref2.registry, "ghcr.io");
        assert_eq!(ref2.repository, "nanosandboxai/agents-registry/claude");
        assert_eq!(ref2.tag, "v1.0");

        let ref3 = ImageRef::parse("python:3.12-slim").unwrap();
        assert_eq!(ref3.registry, "docker.io");
        assert_eq!(ref3.repository, "library/python");
        assert_eq!(ref3.tag, "3.12-slim");

        let ref4 = ImageRef::parse("nginx").unwrap();
        assert_eq!(ref4.repository, "library/nginx");
    }

    #[test]
    fn test_image_ref_full_ref() {
        let image_ref = ImageRef::parse("alpine:3.19").unwrap();
        assert_eq!(image_ref.full_ref(), "docker.io/library/alpine:3.19");
    }

    #[tokio::test]
    async fn test_save_manifest_roundtrip() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();

        let pulled = PulledImage {
            reference: ImageRef::parse("localhost:5050/test/image:v1").unwrap(),
            layers: vec!["sha256:abc123".to_string(), "sha256:def456".to_string()],
            config_digest: "sha256:config789".to_string(),
            size: 1024,
        };

        manager.save_manifest(&pulled).unwrap();

        // exists() should now return true
        assert!(manager
            .exists("localhost:5050/test/image:v1")
            .await
            .unwrap());

        // list() should return the image
        let images = manager.list().await.unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].reference.repository, "test/image");
        assert_eq!(images[0].reference.tag, "v1");
        assert_eq!(images[0].size, 1024);
        assert_eq!(images[0].layers.len(), 2);
    }

    #[test]
    fn test_verify_blob_sha256_valid() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let blob_path = temp_dir.path().join("testblob");

        let content = b"hello world";
        std::fs::write(&blob_path, content).unwrap();

        // SHA256 of "hello world"
        let expected = "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        ImageManager::verify_blob_sha256(&blob_path, expected).unwrap();
    }

    #[test]
    fn test_verify_blob_sha256_invalid() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let blob_path = temp_dir.path().join("testblob");

        std::fs::write(&blob_path, b"hello world").unwrap();

        let wrong_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let result = ImageManager::verify_blob_sha256(&blob_path, wrong_digest);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("integrity check failed"));
    }

    #[test]
    fn test_prune_removes_tars_and_temps() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
        let blobs_dir = manager.blobs_dir();

        // Create mock files
        std::fs::write(blobs_dir.join("abc123.tar"), "decompressed").unwrap();
        std::fs::write(blobs_dir.join("def456.tar"), "decompressed2").unwrap();
        std::fs::write(blobs_dir.join("abc123.dl.12345"), "partial download").unwrap();
        std::fs::write(blobs_dir.join("abc123"), "compressed blob").unwrap();

        let result = manager.prune(false).unwrap();

        assert_eq!(result.decompressed_tars, 2);
        assert_eq!(result.stale_temps, 1);
        // Compressed blob should still exist
        assert!(blobs_dir.join("abc123").exists());
        // Tars and temps should be gone
        assert!(!blobs_dir.join("abc123.tar").exists());
        assert!(!blobs_dir.join("def456.tar").exists());
        assert!(!blobs_dir.join("abc123.dl.12345").exists());
    }

    #[test]
    fn test_prune_preserves_blobs_by_default() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
        let blobs_dir = manager.blobs_dir();

        std::fs::write(blobs_dir.join("abc123"), "compressed blob").unwrap();

        let result = manager.prune(false).unwrap();

        assert_eq!(result.blobs, 0);
        assert!(blobs_dir.join("abc123").exists());
    }

    #[test]
    fn test_prune_all_clears_everything() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manager = ImageManager::new(temp_dir.path().to_path_buf()).unwrap();
        let blobs_dir = manager.blobs_dir();

        // Create blob + tar + manifest
        std::fs::write(blobs_dir.join("abc123"), "compressed blob").unwrap();
        std::fs::write(blobs_dir.join("abc123.tar"), "decompressed").unwrap();

        let pulled = PulledImage {
            reference: ImageRef::parse("test:latest").unwrap(),
            layers: vec!["sha256:abc123".to_string()],
            config_digest: "sha256:cfg".to_string(),
            size: 100,
        };
        manager.save_manifest(&pulled).unwrap();

        let result = manager.prune(true).unwrap();

        // Everything should be removed
        assert!(result.blobs > 0);
        assert!(result.decompressed_tars > 0);
        assert!(result.manifests > 0);
        assert!(!blobs_dir.join("abc123").exists());
        assert!(!blobs_dir.join("abc123.tar").exists());
    }
}

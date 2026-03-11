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
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tar::Archive;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Marker file name to indicate a Windows layer was fully imported
#[cfg(target_os = "windows")]
const LAYER_COMPLETE_MARKER: &str = ".layer_complete";

/// Get the current platform (os/arch)
fn current_platform() -> (&'static str, &'static str) {
    // Detect OS - Windows needs "windows", Linux/macOS use "linux"
    // (macOS runs Linux containers via libkrun)
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };

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
    /// Repository (e.g., "devdone-labs/dd-agents")
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
            .map_err(|e| Error::InvalidImageRef(format!("{}: {}", self.full_ref(), e)))
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
                        tokio::time::sleep(std::time::Duration::from_millis(
                            500 * attempt as u64,
                        ))
                        .await;
                    }
                    last_err = Some(err_msg);
                }
            }
        }
        let (manifest, manifest_digest) = manifest_result.ok_or_else(|| {
            Error::ImagePullFailed(format!(
                "{}: {}",
                image_ref.full_ref(),
                last_err.unwrap_or_else(|| "unknown error".to_string())
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
            info!(
                "Downloading {} layers in parallel...",
                missing_layers.len()
            );

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
                        blobs_dir.join(format!("{}.dl.{}", digest_short, std::process::id()));

                    debug!(
                        "Downloading layer {} ({} bytes)...",
                        digest_short, layer_desc.size
                    );

                    // Ensure blobs directory exists (defensive: may have been cleaned
                    // between ImageManager::new() and this download task)
                    tokio::fs::create_dir_all(&blobs_dir).await.map_err(|e| {
                        Error::ImagePullFailed(format!("Create blobs dir: {}", e))
                    })?;

                    let mut file =
                        tokio::fs::File::create(&temp_path).await.map_err(|e| {
                            Error::ImagePullFailed(format!("Create temp file: {}", e))
                        })?;

                    client_clone
                        .pull_blob(&reference_clone, &layer_desc, &mut file)
                        .await
                        .map_err(|e| {
                            Error::ImagePullFailed(format!(
                                "Download layer {}: {}",
                                digest_short, e
                            ))
                        })?;

                    // Flush and sync to disk before rename (pull_blob doesn't flush)
                    file.shutdown().await.map_err(|e| {
                        Error::ImagePullFailed(format!("Flush layer {}: {}", digest_short, e))
                    })?;
                    drop(file);

                    // Atomic rename from temp to final path
                    tokio::fs::rename(&temp_path, &blob_path).await.map_err(|e| {
                        let temp_exists = std::path::Path::new(&temp_path).exists();
                        let dir_exists = std::path::Path::new(&blobs_dir).exists();
                        Error::ImagePullFailed(format!(
                            "Rename blob {}: {} (temp_exists={}, dir_exists={})",
                            digest_short, e, temp_exists, dir_exists
                        ))
                    })?;

                    debug!("Downloaded layer: {}", digest_short);
                    Ok::<_, Error>(())
                }));
            }

            // Await all parallel downloads
            for handle in handles {
                handle
                    .await
                    .map_err(|e| Error::ImagePullFailed(format!("Task join error: {}", e)))??;
            }

            info!(
                "All layers downloaded in {:?}",
                download_start.elapsed()
            );
        }

        // Step 4: Save config blob if not cached
        let config_digest = manifest.config.digest.clone();
        let config_digest_short = config_digest
            .strip_prefix("sha256:")
            .unwrap_or(&config_digest);
        let config_path = self.blobs_dir().join(config_digest_short);

        if !config_path.exists() {
            debug!("Downloading config blob: {}", config_digest_short);
            let mut file = tokio::fs::File::create(&config_path)
                .await
                .map_err(|e| Error::ImagePullFailed(format!("Create config file: {}", e)))?;
            client
                .pull_blob(&reference, &manifest.config, &mut file)
                .await
                .map_err(|e| Error::ImagePullFailed(format!("Pull config: {}", e)))?;
            file.shutdown().await.map_err(|e| {
                Error::ImagePullFailed(format!("Flush config blob: {}", e))
            })?;
        }

        let total_size: u64 = manifest.layers.iter().map(|l| l.size as u64).sum();

        info!(
            "Image ready: {} layers ({} bytes), completed in {:?}",
            layer_digests.len(),
            total_size,
            start.elapsed()
        );

        Ok(PulledImage {
            reference: image_ref,
            layers: layer_digests,
            config_digest: manifest_digest,
            size: total_size,
        })
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
            archive.set_preserve_permissions(true);
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            archive.unpack(dest).map_err(|e| {
                Error::LayerExtractionFailed(format!("Failed to unpack gzip: {}", e))
            })?;
        } else {
            // Try as plain tar
            let file = File::open(&blob_path)?;
            let mut archive = Archive::new(file);
            archive.set_preserve_permissions(true);
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            archive.unpack(dest).map_err(|e| {
                Error::LayerExtractionFailed(format!("Failed to unpack tar: {}", e))
            })?;
        }

        Ok(())
    }

    /// Create a rootfs by extracting all layers in order with parallel decompression.
    ///
    /// This optimized implementation:
    /// 1. Decompresses all gzipped layers in parallel using OS threads
    /// 2. Caches decompressed tars as `{digest}.tar` for future reuse
    /// 3. Extracts the uncompressed tars sequentially (maintains layer ordering)
    ///
    /// The parallel decompression phase handles the CPU-intensive gzip work,
    /// while sequential extraction ensures correct overlay semantics.
    pub fn create_rootfs(&self, layers: &[String], dest: &Path) -> Result<()> {
        let start = Instant::now();
        info!("Creating rootfs at {:?} from {} layers", dest, layers.len());

        fs::create_dir_all(dest).map_err(|e| {
            Error::LayerExtractionFailed(format!(
                "Create rootfs dir {}: {}", dest.display(), e
            ))
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
                        let digest_short =
                            digest.strip_prefix("sha256:").unwrap_or(digest);
                        let blob_path = blobs_dir.join(digest_short);

                        if !blob_path.exists() {
                            return Err(Error::LayerExtractionFailed(format!(
                                "Layer blob not found: {}",
                                digest
                            )));
                        }

                        // Check if layer is gzipped by reading magic bytes
                        let mut header = [0u8; 2];
                        {
                            let mut peek = File::open(&blob_path).map_err(|e| {
                                Error::LayerExtractionFailed(format!(
                                    "Open blob {}: {}", blob_path.display(), e
                                ))
                            })?;
                            let _ = peek.read_exact(&mut header);
                        }
                        let is_gzipped = header[0] == 0x1f && header[1] == 0x8b;

                        if is_gzipped {
                            let tar_path =
                                blobs_dir.join(format!("{}.tar", digest_short));

                            // Use cached decompressed tar if available
                            if !tar_path.exists() {
                                debug!(
                                    "Decompressing layer {}/{}: {}",
                                    i + 1,
                                    num_layers,
                                    digest_short
                                );
                                let in_file = File::open(&blob_path).map_err(|e| {
                                    Error::LayerExtractionFailed(format!(
                                        "Open blob for decompress {}: {}", blob_path.display(), e
                                    ))
                                })?;
                                let mut decoder = GzDecoder::new(in_file);
                                // Write to temp file for atomic rename
                                let temp_path = blobs_dir.join(format!(
                                    "{}.tar.tmp.{}",
                                    digest_short,
                                    std::process::id()
                                ));
                                let mut out_file = File::create(&temp_path).map_err(|e| {
                                    Error::LayerExtractionFailed(format!(
                                        "Create temp tar {}: {}", temp_path.display(), e
                                    ))
                                })?;
                                std::io::copy(&mut decoder, &mut out_file)
                                    .map_err(|e| {
                                        Error::LayerExtractionFailed(format!(
                                            "Decompress layer {}: {}",
                                            digest_short, e
                                        ))
                                    })?;
                                // Atomic rename to final path
                                fs::rename(&temp_path, &tar_path).map_err(|e| {
                                    Error::LayerExtractionFailed(format!(
                                        "Rename decompressed tar {} -> {}: {}",
                                        temp_path.display(), tar_path.display(), e
                                    ))
                                })?;
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
                Error::LayerExtractionFailed(format!(
                    "Open tar {}: {}", tar_path.display(), e
                ))
            })?;
            let mut archive = Archive::new(file);
            archive.set_preserve_permissions(true);
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            archive.unpack(dest).map_err(|e| {
                Error::LayerExtractionFailed(format!("Failed to unpack tar: {}", e))
            })?;
        }

        debug!(
            "Sequential extraction completed in {:?}",
            extract_start.elapsed()
        );
        info!("Rootfs created successfully in {:?}", start.elapsed());
        Ok(())
    }

    /// Import OCI layers for Windows containers using wclayer.exe
    ///
    /// **DEPRECATED**: This function is deprecated and no longer used by the main
    /// Windows runtime. The Windows containerd runtime (`ContainerdWindowsRuntime`)
    /// handles image pulling and layer management through containerd's snapshotter,
    /// which avoids the `ProcessBaseLayer` idempotency issues that plagued direct
    /// wclayer usage.
    ///
    /// This function is kept for reference only and may be removed in a future version.
    ///
    /// Windows containers require layers to be imported into a specific format
    /// using the wclayer tool from hcsshim. This converts standard OCI tar layers
    /// into the Windows container layer format required by HCS.
    ///
    /// Returns the list of imported layer folder paths in order (base to top).
    #[cfg(target_os = "windows")]
    #[deprecated(
        since = "0.2.0",
        note = "Use ContainerdWindowsRuntime which handles layers via containerd snapshotter"
    )]
    #[allow(deprecated)]
    pub async fn import_layers_for_windows(&self, layers: &[String]) -> Result<Vec<PathBuf>> {
        use crate::runtime::runhcs_setup::ensure_wclayer;
        use std::time::Duration;
        use tracing::warn;

        /// Maximum number of retry attempts for layer import on collision errors
        const MAX_IMPORT_RETRIES: u32 = 3;
        /// Initial backoff delay between retries (doubles each attempt)
        const INITIAL_BACKOFF_MS: u64 = 200;

        info!(
            "Importing {} layers for Windows using wclayer",
            layers.len()
        );

        // Ensure wclayer.exe is available
        let wclayer_path = ensure_wclayer().await?;
        debug!("Using wclayer at: {}", wclayer_path.display());

        let mut layer_folders = Vec::new();

        for (i, digest) in layers.iter().enumerate() {
            let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);
            let layer_dir = self.extracted_dir().join(digest_short);
            let blob_path = self.blobs_dir().join(digest_short);
            let lock_path = self.extracted_dir().join(format!("{}.lock", digest_short));

            // Acquire per-layer lock to prevent concurrent imports
            let _lock_guard = self.acquire_layer_lock(&lock_path)?;

            // Re-check if layer was imported while we waited for the lock
            if layer_dir.exists() && self.is_valid_windows_layer(&layer_dir) {
                debug!(
                    "Layer {}/{} already imported: {}",
                    i + 1,
                    layers.len(),
                    digest_short
                );
                layer_folders.push(layer_dir);
                continue;
            }

            // If directory exists but is invalid, use HCS-native cleanup first
            if layer_dir.exists() {
                debug!(
                    "Removing invalid/partial layer directory via wclayer remove: {}",
                    layer_dir.display()
                );
                self.wclayer_remove_layer(&wclayer_path, &layer_dir);
            }

            // Check if blob exists
            if !blob_path.exists() {
                return Err(Error::LayerExtractionFailed(format!(
                    "Layer blob not found: {}",
                    digest
                )));
            }

            debug!(
                "Importing layer {}/{}: {}",
                i + 1,
                layers.len(),
                digest_short
            );

            // NOTE: Do NOT pre-create layer_dir here - wclayer import creates it internally
            // via os.MkdirAll. Pre-creating interferes with HCS layer operations.

            // Build wclayer import command
            // wclayer import --layer <parent1> --layer <parent2> --input <blob> <dest>
            let mut args: Vec<String> = vec!["import".to_string()];

            // Add parent layers
            for parent in &layer_folders {
                args.push("--layer".to_string());
                args.push(parent.to_string_lossy().to_string());
            }

            // Add input blob and destination
            args.push("--input".to_string());
            args.push(blob_path.to_string_lossy().to_string());
            args.push(layer_dir.to_string_lossy().to_string());

            // Retry loop for handling "already exists" collisions
            let mut last_error: Option<String> = None;
            let mut attempt = 0;

            while attempt < MAX_IMPORT_RETRIES {
                attempt += 1;
                debug!("Running wclayer import (attempt {}/{}): {:?}", attempt, MAX_IMPORT_RETRIES, args);

                let output = std::process::Command::new(&wclayer_path)
                    .args(&args)
                    .output()
                    .map_err(|e| {
                        self.wclayer_remove_layer(&wclayer_path, &layer_dir);
                        Error::LayerExtractionFailed(format!("Failed to run wclayer: {}", e))
                    })?;

                if output.status.success() {
                    last_error = None;
                    break;
                }

                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                last_error = Some(stderr.clone());

                // Check if this is a retryable "already exists" error
                if Self::is_already_exists_error(&stderr) && attempt < MAX_IMPORT_RETRIES {
                    warn!(
                        "wclayer import hit 'already exists' collision (attempt {}/{}), cleaning up and retrying: {}",
                        attempt, MAX_IMPORT_RETRIES, stderr.trim()
                    );

                    // Use HCS-native cleanup (DestroyLayer) before retry
                    self.wclayer_remove_layer(&wclayer_path, &layer_dir);

                    // Exponential backoff before retry
                    let backoff_ms = INITIAL_BACKOFF_MS * (1 << (attempt - 1));
                    std::thread::sleep(Duration::from_millis(backoff_ms));
                } else {
                    // Non-retryable error or max retries reached
                    break;
                }
            }

            // Check if import ultimately failed
            if let Some(err) = last_error {
                self.wclayer_remove_layer(&wclayer_path, &layer_dir);
                return Err(Error::LayerExtractionFailed(format!(
                    "wclayer import failed for layer {} after {} attempts: {}",
                    digest_short, attempt, err
                )));
            }

            // Write completion marker to indicate successful import
            let marker_path = layer_dir.join(LAYER_COMPLETE_MARKER);
            fs::write(&marker_path, format!("imported: {}", digest_short)).map_err(|e| {
                // Clean up if we can't write the marker
                self.wclayer_remove_layer(&wclayer_path, &layer_dir);
                Error::LayerExtractionFailed(format!(
                    "Failed to write layer completion marker: {}",
                    e
                ))
            })?;

            layer_folders.push(layer_dir);
        }

        info!(
            "Imported {} layer folders for Windows container",
            layer_folders.len()
        );
        Ok(layer_folders)
    }

    /// Check if a directory contains a valid Windows container layer
    ///
    /// A layer is only considered valid if the completion marker file exists,
    /// indicating that the wclayer import completed successfully.
    #[cfg(target_os = "windows")]
    fn is_valid_windows_layer(&self, layer_dir: &Path) -> bool {
        layer_dir.exists() && layer_dir.join(LAYER_COMPLETE_MARKER).exists()
    }

    /// Use wclayer remove to properly destroy a Windows container layer via HCS DestroyLayer API.
    /// This is the correct way to clean up partial/invalid layers, not just filesystem deletion.
    /// Falls back to filesystem deletion if wclayer remove fails.
    #[cfg(target_os = "windows")]
    fn wclayer_remove_layer(&self, wclayer_path: &Path, layer_dir: &Path) {
        use tracing::warn;

        if !layer_dir.exists() {
            return;
        }

        // First, remove the completion marker if it exists
        let marker_path = layer_dir.join(LAYER_COMPLETE_MARKER);
        let _ = fs::remove_file(&marker_path);

        debug!("Running wclayer remove on: {}", layer_dir.display());

        // Call wclayer remove to properly destroy the layer via HCS
        let result = std::process::Command::new(wclayer_path)
            .args(["remove", &layer_dir.to_string_lossy()])
            .output();

        match result {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "wclayer remove failed (falling back to filesystem delete): {}",
                        stderr.trim()
                    );
                } else {
                    debug!("wclayer remove succeeded for: {}", layer_dir.display());
                }
            }
            Err(e) => {
                warn!(
                    "Failed to run wclayer remove (falling back to filesystem delete): {}",
                    e
                );
            }
        }

        // Always attempt filesystem cleanup as fallback/supplement
        // Some files may remain after wclayer remove, or wclayer remove may have failed
        if layer_dir.exists() {
            if let Err(e) = fs::remove_dir_all(layer_dir) {
                warn!(
                    "Failed to remove layer directory {}: {}",
                    layer_dir.display(),
                    e
                );
            }
        }
    }

    /// Check if an error message indicates an "already exists" collision
    /// that can be resolved by cleanup and retry.
    #[cfg(target_os = "windows")]
    fn is_already_exists_error(stderr: &str) -> bool {
        let stderr_lower = stderr.to_lowercase();
        // Match various forms of the "already exists" error:
        // - "Cannot create a file when that file already exists"
        // - Win32 error code 0xB7 (183) = ERROR_ALREADY_EXISTS
        // - "already exists" substring
        stderr_lower.contains("already exists")
            || stderr_lower.contains("0xb7")
            || stderr_lower.contains("(183)")
            || stderr_lower.contains("error_already_exists")
    }

    /// Acquire an exclusive lock on a layer to prevent concurrent imports.
    /// Returns a guard that releases the lock when dropped.
    #[cfg(target_os = "windows")]
    fn acquire_layer_lock(&self, lock_path: &Path) -> Result<LayerLockGuard> {
        use std::time::{Duration, Instant};
        use tracing::warn;

        const LOCK_TIMEOUT: Duration = Duration::from_secs(120);
        const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

        let start = Instant::now();

        loop {
            // Try to create the lock file exclusively
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
            {
                Ok(file) => {
                    // Successfully acquired lock
                    debug!("Acquired layer lock: {}", lock_path.display());
                    return Ok(LayerLockGuard {
                        lock_path: lock_path.to_path_buf(),
                        _file: file,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Lock is held by another process
                    if start.elapsed() > LOCK_TIMEOUT {
                        // Check if lock file is stale (older than timeout)
                        if let Ok(metadata) = fs::metadata(lock_path) {
                            if let Ok(modified) = metadata.modified() {
                                if modified.elapsed().unwrap_or(Duration::ZERO) > LOCK_TIMEOUT {
                                    warn!(
                                        "Removing stale lock file: {}",
                                        lock_path.display()
                                    );
                                    let _ = fs::remove_file(lock_path);
                                    continue;
                                }
                            }
                        }
                        return Err(Error::LayerExtractionFailed(format!(
                            "Timeout waiting for layer lock: {}",
                            lock_path.display()
                        )));
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(e) => {
                    return Err(Error::LayerExtractionFailed(format!(
                        "Failed to acquire layer lock {}: {}",
                        lock_path.display(),
                        e
                    )));
                }
            }
        }
    }
}

/// Guard that releases a layer lock when dropped
#[cfg(target_os = "windows")]
struct LayerLockGuard {
    lock_path: PathBuf,
    #[allow(dead_code)]
    _file: File,
}

#[cfg(target_os = "windows")]
impl Drop for LayerLockGuard {
    fn drop(&mut self) {
        // Remove the lock file when the guard is dropped
        if let Err(e) = fs::remove_file(&self.lock_path) {
            tracing::warn!(
                "Failed to remove layer lock file {}: {}",
                self.lock_path.display(),
                e
            );
        } else {
            tracing::debug!("Released layer lock: {}", self.lock_path.display());
        }
    }
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

        // Note: We don't remove blobs as they may be shared by other images
        // A garbage collection process could clean up orphaned blobs

        info!("Removed image: {}", image_ref.full_ref());
        Ok(())
    }
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

        let ref2 = ImageRef::parse("ghcr.io/devdone-labs/agents-registry/claude:v1.0").unwrap();
        assert_eq!(ref2.registry, "ghcr.io");
        assert_eq!(ref2.repository, "devdone-labs/agents-registry/claude");
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
}

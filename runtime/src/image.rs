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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tar::Archive;
use tracing::{debug, info};

/// Get the current platform (os/arch)
fn current_platform() -> (&'static str, &'static str) {
    // Container images always use "linux" as OS, even on macOS
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

        // Try to find exact match
        for entry in entries {
            if let Some(ref platform) = entry.platform {
                if platform.os == target_os && platform.architecture == target_arch {
                    debug!("Found matching platform: {:?}", entry.digest);
                    return Some(entry.digest.clone());
                }
            }
        }

        // Fallback: try to find any linux platform with matching arch
        for entry in entries {
            if let Some(ref platform) = entry.platform {
                if platform.os == "linux" && platform.architecture == target_arch {
                    debug!("Found fallback platform: {:?}", entry.digest);
                    return Some(entry.digest.clone());
                }
            }
        }

        // Last resort: return first available
        entries.first().map(|e| {
            debug!("Using first available platform: {:?}", e.digest);
            e.digest.clone()
        })
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
        // Check if this is a simple image name without registry (no dots in the name part)
        let image_name_part = image.split(':').next().unwrap_or(image);
        let image = if !image_name_part.contains('/') && !image_name_part.contains('.') {
            format!("library/{}", image)
        } else {
            image.to_string()
        };

        // Parse registry and rest
        let (registry, rest) = if image.contains('/')
            && image.split('/').next().is_some_and(|s| {
                s.contains('.') || s.contains(':') || s == "localhost"
            })
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

    /// Pull an image from a registry
    pub async fn pull(&self, image: &str) -> Result<PulledImage> {
        let image_ref = ImageRef::parse(image)?;
        let reference = image_ref.to_reference()?;

        info!("Pulling image: {}", image_ref.full_ref());

        // Get appropriate client and auth for this registry
        let client = self.get_client(&image_ref.registry);
        let auth = self.get_auth(&image_ref.registry);

        match &auth {
            RegistryAuth::Anonymous => debug!("Using anonymous auth for {}", image_ref.registry),
            RegistryAuth::Basic(user, _) => {
                debug!("Using authenticated pull as {} for {}", user, image_ref.registry)
            }
        }

        // Accept all standard OCI and Docker layer types
        let accepted_media_types = vec![
            "application/vnd.oci.image.layer.v1.tar",
            "application/vnd.oci.image.layer.v1.tar+gzip",
            "application/vnd.oci.image.layer.v1.tar+zstd",
            "application/vnd.docker.image.rootfs.diff.tar.gzip",
        ];

        // Use pull to get image data
        let image_data = client
            .pull(&reference, &auth, accepted_media_types)
            .await
            .map_err(|e| Error::ImagePullFailed(format!("{}: {}", image_ref.full_ref(), e)))?;

        let mut layer_digests = Vec::new();
        let mut total_size: u64 = 0;

        // Process and cache each layer
        for layer in &image_data.layers {
            let digest = &layer.sha256_digest();
            let digest_short = digest.strip_prefix("sha256:").unwrap_or(digest);

            // Check if layer already cached
            let blob_path = self.blobs_dir().join(digest_short);
            if blob_path.exists() {
                debug!("Layer {} already cached", digest_short);
            } else {
                debug!("Caching layer: {}", digest_short);
                let mut file = File::create(&blob_path)?;
                file.write_all(&layer.data)?;
            }

            layer_digests.push(digest.clone());
            total_size += layer.data.len() as u64;
        }

        // Save config
        let config_digest = image_data.digest.clone().unwrap_or_default();
        let config_digest_short = config_digest
            .strip_prefix("sha256:")
            .unwrap_or(&config_digest);

        if !image_data.config.data.is_empty() {
            let config_path = self.blobs_dir().join(config_digest_short);
            if !config_path.exists() {
                let mut file = File::create(&config_path)?;
                file.write_all(&image_data.config.data)?;
            }
        }

        info!(
            "Pulled {} layers ({} bytes)",
            layer_digests.len(),
            total_size
        );

        Ok(PulledImage {
            reference: image_ref,
            layers: layer_digests,
            config_digest,
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
            archive
                .unpack(dest)
                .map_err(|e| Error::LayerExtractionFailed(format!("Failed to unpack gzip: {}", e)))?;
        } else {
            // Try as plain tar
            let file = File::open(&blob_path)?;
            let mut archive = Archive::new(file);
            archive.set_preserve_permissions(true);
            archive.set_preserve_ownerships(false);
            archive.set_overwrite(true);
            archive
                .unpack(dest)
                .map_err(|e| Error::LayerExtractionFailed(format!("Failed to unpack tar: {}", e)))?;
        }

        Ok(())
    }

    /// Create a rootfs by extracting all layers in order
    pub fn create_rootfs(&self, layers: &[String], dest: &Path) -> Result<()> {
        info!("Creating rootfs at {:?} from {} layers", dest, layers.len());

        fs::create_dir_all(dest)?;

        for (i, digest) in layers.iter().enumerate() {
            debug!("Extracting layer {}/{}: {}", i + 1, layers.len(), digest);
            self.extract_layer(digest, dest)?;
        }

        info!("Rootfs created successfully");
        Ok(())
    }

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

        let ref2 = ImageRef::parse("ghcr.io/devdone-labs/dd-agents:v1.0").unwrap();
        assert_eq!(ref2.registry, "ghcr.io");
        assert_eq!(ref2.repository, "devdone-labs/dd-agents");
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

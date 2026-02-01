//! OCI image management

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        // Simple parser - TODO: Use proper OCI reference parsing
        let (registry, rest) = if image.contains('/') && image.split('/').next().map_or(false, |s| s.contains('.')) {
            let parts: Vec<&str> = image.splitn(2, '/').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            ("docker.io".to_string(), image.to_string())
        };

        let (repo_tag, digest) = if rest.contains('@') {
            let parts: Vec<&str> = rest.splitn(2, '@').collect();
            (parts[0].to_string(), Some(parts[1].to_string()))
        } else {
            (rest, None)
        };

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

    /// Get the full image reference string
    pub fn full_ref(&self) -> String {
        if let Some(ref digest) = self.digest {
            format!("{}/{}@{}", self.registry, self.repository, digest)
        } else {
            format!("{}/{}:{}", self.registry, self.repository, self.tag)
        }
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
}

/// Manages OCI images
pub struct ImageManager {
    /// Cache directory
    cache_dir: PathBuf,
}

impl ImageManager {
    /// Create a new image manager
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Create with default cache directory
    pub fn with_default_cache() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("nanosandbox")
            .join("images");

        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self { cache_dir })
    }

    /// Get the cache directory
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Pull an image from a registry
    pub async fn pull(&self, image: &str) -> Result<ImageRef> {
        let image_ref = ImageRef::parse(image)?;

        // TODO: Implement OCI image pulling
        // 1. Authenticate with registry (if needed)
        // 2. Fetch manifest
        // 3. Download layers
        // 4. Cache layers by digest

        Ok(image_ref)
    }

    /// Check if an image exists locally
    pub async fn exists(&self, image: &str) -> Result<bool> {
        let _image_ref = ImageRef::parse(image)?;
        // TODO: Check cache
        Ok(false)
    }

    /// List cached images
    pub async fn list(&self) -> Result<Vec<ImageInfo>> {
        // TODO: List from cache
        Ok(Vec::new())
    }

    /// Remove a cached image
    pub async fn remove(&self, _image: &str) -> Result<()> {
        // TODO: Remove from cache
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ref_parse() {
        let ref1 = ImageRef::parse("alpine").unwrap();
        assert_eq!(ref1.registry, "docker.io");
        assert_eq!(ref1.repository, "alpine");
        assert_eq!(ref1.tag, "latest");

        let ref2 = ImageRef::parse("ghcr.io/devdone-labs/dd-agents:v1.0").unwrap();
        assert_eq!(ref2.registry, "ghcr.io");
        assert_eq!(ref2.repository, "devdone-labs/dd-agents");
        assert_eq!(ref2.tag, "v1.0");

        let ref3 = ImageRef::parse("python:3.12-slim").unwrap();
        assert_eq!(ref3.registry, "docker.io");
        assert_eq!(ref3.repository, "python");
        assert_eq!(ref3.tag, "3.12-slim");
    }
}

//! Error types for Nanosandbox

use thiserror::Error;

/// Nanosandbox error type
#[derive(Debug, Error)]
pub enum Error {
    /// Image not found
    #[error("Image not found: {0}")]
    ImageNotFound(String),

    /// Failed to pull image
    #[error("Failed to pull image: {0}")]
    ImagePullFailed(String),

    /// Failed to parse image reference
    #[error("Invalid image reference: {0}")]
    InvalidImageRef(String),

    /// Layer extraction failed
    #[error("Failed to extract layer: {0}")]
    LayerExtractionFailed(String),

    /// Sandbox not found
    #[error("Sandbox not found: {0}")]
    SandboxNotFound(String),

    /// Sandbox creation failed
    #[error("Failed to create sandbox: {0}")]
    SandboxCreationFailed(String),

    /// Command execution failed
    #[error("Command execution failed: {0}")]
    ExecFailed(String),

    /// Runtime not available
    #[error("Runtime not available: {0}")]
    RuntimeNotAvailable(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// OCI distribution error
    #[error("OCI registry error: {0}")]
    OciRegistry(String),

    /// Operation timed out
    #[error("Operation timed out after {0} seconds")]
    Timeout(u32),

    /// Sandbox already exists
    #[error("Sandbox already exists: {0}")]
    SandboxAlreadyExists(String),

    /// Sandbox is in invalid state for operation
    #[error("Invalid sandbox state: {0}")]
    InvalidState(String),

    /// Platform not supported
    #[error("Platform not supported: {platform}. Supported platforms: Linux, macOS")]
    UnsupportedPlatform {
        /// The unsupported platform name
        platform: String,
    },

    /// Runtime binary not found
    #[error("Runtime binary '{binary}' not found")]
    RuntimeBinaryNotFound {
        /// Name of the missing binary
        binary: String,
    },

    /// KVM not available (Linux)
    #[error("KVM is not available")]
    KvmNotAvailable,

    /// Hypervisor framework not available (macOS)
    #[error("Hypervisor.framework is not available")]
    HypervisorFrameworkNotAvailable,

    /// Insufficient permissions to access a resource
    #[error("Insufficient permissions to access {resource}: {details}")]
    InsufficientPermissions {
        /// The resource that couldn't be accessed
        resource: String,
        /// Additional details
        details: String,
    },
}

/// Result type alias
pub type Result<T> = std::result::Result<T, Error>;

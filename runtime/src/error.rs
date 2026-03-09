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

    /// MCP operation not supported (sandbox not in persistent/gateway mode)
    #[error("MCP not supported: {0}")]
    McpNotSupported(String),

    /// MCP server operation failed
    #[error("MCP server error: {0}")]
    McpServerError(String),

    // ===== Runtime Prerequisite Errors =====
    /// Platform not supported
    #[error("Platform not supported: {platform}. Supported platforms: Windows, Linux, macOS")]
    UnsupportedPlatform {
        /// The unsupported platform name
        platform: String,
    },

    /// Runtime binary not found in PATH
    #[error("Runtime binary '{binary}' not found. {install_hint}")]
    RuntimeBinaryNotFound {
        /// Name of the missing binary
        binary: String,
        /// Installation instructions
        install_hint: String,
    },

    /// Windows Containers feature not enabled
    #[cfg(target_os = "windows")]
    #[error("Windows Containers feature is not enabled. Enable it via: Enable-WindowsOptionalFeature -Online -FeatureName Containers -All")]
    WindowsContainersNotEnabled,

    /// Hyper-V feature not enabled (required for Hyper-V isolation)
    #[cfg(target_os = "windows")]
    #[error("Hyper-V feature is not enabled. Enable it via: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All")]
    HyperVNotEnabled,

    /// HCS (Host Compute Service) not running
    #[cfg(target_os = "windows")]
    #[error("Host Compute Service (HCS) is not running. Start it via: Start-Service vmcompute")]
    HcsNotRunning,

    /// KVM not available (Linux)
    #[error("KVM is not available. Ensure /dev/kvm exists and you have permission to access it.")]
    KvmNotAvailable,

    /// Hypervisor framework not available (macOS)
    #[error("Hypervisor.framework is not available. Ensure you're running on Apple Silicon with macOS 11+.")]
    HypervisorFrameworkNotAvailable,

    /// Insufficient permissions to access a resource
    #[error("Insufficient permissions to access {resource}: {details}")]
    InsufficientPermissions {
        /// The resource that couldn't be accessed
        resource: String,
        /// Additional details
        details: String,
    },

    /// Multiple prerequisite checks failed
    #[error("Runtime prerequisites not met:\n{}", format_prerequisite_errors(.0))]
    PrerequisiteChecksFailed(Vec<PrerequisiteError>),
}

/// Individual prerequisite check error
#[derive(Debug, Clone)]
pub struct PrerequisiteError {
    /// Name of the check that failed
    pub check: String,
    /// Error message
    pub message: String,
    /// How to fix it
    pub fix_hint: Option<String>,
}

impl std::fmt::Display for PrerequisiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "- {}: {}", self.check, self.message)?;
        if let Some(hint) = &self.fix_hint {
            write!(f, "\n  Fix: {}", hint)?;
        }
        Ok(())
    }
}

/// Format multiple prerequisite errors for display
fn format_prerequisite_errors(errors: &[PrerequisiteError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Result type alias
pub type Result<T> = std::result::Result<T, Error>;

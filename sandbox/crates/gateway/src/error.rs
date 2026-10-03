/// Errors from gateway operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Gateway health check timed out after {0}s")]
    HealthTimeout(u64),

    #[error("VM process died before gateway became healthy")]
    VmDied,

    #[error("Gateway not available — sandbox must be in gateway mode")]
    NotAvailable,

    #[error("Exec failed: {0}")]
    ExecFailed(String),

    #[error("HTTP request failed: {0}")]
    HttpFailed(String),

    #[error("Secrets error: {0}")]
    SecretsError(String),

    #[error("SSH error: {0}")]
    SshError(String),
}

pub type Result<T> = std::result::Result<T, Error>;

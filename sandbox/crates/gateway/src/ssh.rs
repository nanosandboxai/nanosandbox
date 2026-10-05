//! SSH key generation utilities for sandbox access.
//!
//! Generates ephemeral ed25519 key pairs for SSH access into VMs.

use std::path::PathBuf;
use tracing::info;

/// Generate an ephemeral SSH key pair for a sandbox.
///
/// Returns `(private_key_path, public_key_content)`.
/// The caller is responsible for injecting the public key into the VM.
pub fn generate_ssh_keys(sandbox_id: &str) -> Result<(PathBuf, String), String> {
    let key_dir = std::env::temp_dir().join(format!("nanosb-{}-ssh", sandbox_id));
    std::fs::create_dir_all(&key_dir).map_err(|e| format!("mkdir ssh key dir: {}", e))?;
    let key_path = key_dir.join("id_ed25519");

    // Remove any leftover key from a previous run
    let _ = std::fs::remove_file(&key_path);
    let _ = std::fs::remove_file(key_path.with_extension("pub"));

    let mut cmd = std::process::Command::new("ssh-keygen");
    cmd.args([
        "-t", "ed25519",
        "-f", key_path.to_str().unwrap(),
        "-N", "",
        "-q",
    ]);

    let status = cmd.status().map_err(|e| format!("ssh-keygen spawn: {}", e))?;
    if !status.success() {
        return Err(format!("ssh-keygen exited with {}", status));
    }

    let pub_key = std::fs::read_to_string(key_path.with_extension("pub"))
        .map_err(|e| format!("read public key: {}", e))?;

    // Set private key permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }

    info!("SSH keys generated for sandbox '{}'", sandbox_id);
    Ok((key_path, pub_key))
}

/// Inject an SSH public key into a rootfs directory.
///
/// Writes the key to both root and developer user authorized_keys files.
pub fn inject_pubkey_into_rootfs(
    rootfs_path: &std::path::Path,
    pub_key: &str,
) -> Result<(), String> {
    // Root authorized_keys
    let auth_keys_dir = rootfs_path.join("root/.ssh");
    std::fs::create_dir_all(&auth_keys_dir).map_err(|e| format!("mkdir .ssh: {}", e))?;
    std::fs::write(auth_keys_dir.join("authorized_keys"), pub_key)
        .map_err(|e| format!("write authorized_keys: {}", e))?;

    // Developer user authorized_keys
    let dev_ssh_dir = rootfs_path.join("home/developer/.ssh");
    std::fs::create_dir_all(&dev_ssh_dir)
        .map_err(|e| format!("mkdir developer .ssh: {}", e))?;
    std::fs::write(dev_ssh_dir.join("authorized_keys"), pub_key)
        .map_err(|e| format!("write developer authorized_keys: {}", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&auth_keys_dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(
            auth_keys_dir.join("authorized_keys"),
            std::fs::Permissions::from_mode(0o600),
        );
        let _ = std::fs::set_permissions(&dev_ssh_dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(
            dev_ssh_dir.join("authorized_keys"),
            std::fs::Permissions::from_mode(0o600),
        );
    }

    Ok(())
}

/// Clean up SSH key files for a sandbox.
pub fn cleanup_ssh_keys(key_path: &std::path::Path) {
    if let Some(parent) = key_path.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

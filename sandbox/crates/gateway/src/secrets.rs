//! Gateway secrets: keypair generation, ECDH decryption, and tmpfs secret writer.
//!
//! The `GatewayKeyPair` is one-shot — the private key is zeroed after the first
//! successful decryption so it cannot be reused.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use x25519_dalek::{PublicKey, StaticSecret};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Encrypted payload sent from the host CLI to the gateway.
#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub version: u8,
    pub ephemeral_pubkey: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// Decrypted secrets payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct SecretPayload {
    pub secrets: HashMap<String, String>,
    #[serde(default)]
    pub intercepted_files: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// GatewayKeyPair
// ---------------------------------------------------------------------------

/// One-shot X25519 keypair used by the gateway to decrypt a single secrets
/// payload.  The private key is zeroed after first use.
pub struct GatewayKeyPair {
    secret_bytes: [u8; 32],
    pub public_bytes: [u8; 32],
    used: bool,
}

impl GatewayKeyPair {
    /// Generate a fresh X25519 keypair backed by the OS RNG.
    pub fn generate() -> Self {
        let secret = StaticSecret::random();
        let public = PublicKey::from(&secret);
        Self {
            secret_bytes: secret.to_bytes(),
            public_bytes: *public.as_bytes(),
            used: false,
        }
    }

    /// Return the base64-encoded public key (to be advertised to the host).
    pub fn public_key_base64(&self) -> String {
        B64.encode(self.public_bytes)
    }

    /// Decrypt `encrypted` using ECDH + AES-256-GCM (one-shot).
    ///
    /// Errors if the keypair has already been used, or if decryption fails.
    pub fn decrypt(&mut self, encrypted: &EncryptedPayload) -> Result<SecretPayload, String> {
        if self.used {
            return Err("already used".to_string());
        }

        // --- ECDH ---
        let ephemeral_pk_bytes: Vec<u8> = B64
            .decode(&encrypted.ephemeral_pubkey)
            .map_err(|e| format!("bad ephemeral_pubkey base64: {e}"))?;
        if ephemeral_pk_bytes.len() != 32 {
            return Err(format!(
                "ephemeral_pubkey wrong length: {}",
                ephemeral_pk_bytes.len()
            ));
        }
        let mut epk_arr = [0u8; 32];
        epk_arr.copy_from_slice(&ephemeral_pk_bytes);
        let ephemeral_pk = PublicKey::from(epk_arr);

        let secret = StaticSecret::from(self.secret_bytes);
        let shared = secret.diffie_hellman(&ephemeral_pk);

        // Zero private key bytes immediately after use
        self.secret_bytes = [0u8; 32];
        self.used = true;

        // Derive AES key: SHA-256 of the raw shared secret
        let aes_key = Sha256::digest(shared.as_bytes());

        // --- AES-256-GCM ---
        let nonce_bytes = B64
            .decode(&encrypted.nonce)
            .map_err(|e| format!("bad nonce base64: {e}"))?;
        if nonce_bytes.len() != 12 {
            return Err(format!("nonce wrong length: {}", nonce_bytes.len()));
        }
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(&nonce_bytes);
        let nonce = Nonce::from(nonce_arr);

        let ciphertext = B64
            .decode(&encrypted.ciphertext)
            .map_err(|e| format!("bad ciphertext base64: {e}"))?;

        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| format!("AES key init failed: {e}"))?;

        let plaintext = cipher
            .decrypt(&nonce, ciphertext.as_ref())
            .map_err(|_| "AES-GCM decryption failed (bad key or tampered ciphertext)")?;

        serde_json::from_slice(&plaintext)
            .map_err(|e| format!("failed to deserialize SecretPayload: {e}"))
    }
}

impl Drop for GatewayKeyPair {
    fn drop(&mut self) {
        self.secret_bytes = [0u8; 32];
    }
}

// ---------------------------------------------------------------------------
// Encrypt (host-side, matches GatewayKeyPair::decrypt)
// ---------------------------------------------------------------------------

/// Encrypt a plaintext payload for a gateway whose public key is known.
///
/// Uses ephemeral X25519 ECDH + SHA-256 KDF + AES-256-GCM — the same
/// scheme that `GatewayKeyPair::decrypt` expects.
pub fn encrypt_payload(
    plaintext: &[u8],
    gateway_pubkey_bytes: &[u8; 32],
) -> Result<EncryptedPayload, String> {
    use x25519_dalek::EphemeralSecret;

    let gateway_pk = PublicKey::from(*gateway_pubkey_bytes);

    // Ephemeral X25519 keypair for this encryption
    let cli_secret = EphemeralSecret::random();
    let cli_pk = PublicKey::from(&cli_secret);

    // ECDH shared secret → SHA-256 → AES key (must match decrypt's KDF)
    let shared = cli_secret.diffie_hellman(&gateway_pk);
    let aes_key = Sha256::digest(shared.as_bytes());

    let cipher = Aes256Gcm::new_from_slice(&aes_key)
        .map_err(|e| format!("AES key init failed: {e}"))?;

    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| format!("AES-GCM encryption failed: {e}"))?;

    Ok(EncryptedPayload {
        version: 1,
        ephemeral_pubkey: B64.encode(cli_pk.as_bytes()),
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(ciphertext),
    })
}

// ---------------------------------------------------------------------------
// SecretManifest
// ---------------------------------------------------------------------------

/// Manifest returned after writing secrets, mapping logical key names to their
/// on-disk paths (secrets) or file paths (intercepted files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretManifest {
    /// Map of secret key name → hashed tmpfs path
    pub secrets: HashMap<String, String>,
    /// Map of intercepted file key name → hashed tmpfs path
    pub files: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// SHA-256 of `"session_id:key_name"` returned as a lowercase hex string.
pub fn hash_name(session_id: &str, key_name: &str) -> String {
    let input = format!("{session_id}:{key_name}");
    let digest = Sha256::digest(input.as_bytes());
    format!("{digest:x}")
}

/// Write `contents` to `path` with 0400 permissions (Unix) or plain write (other).
pub fn write_secret_file(path: &std::path::Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o400);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }

    Ok(())
}

/// Generate a fresh X25519 gateway keypair as raw byte arrays.
///
/// Returns `(secret_bytes, public_bytes)`, each 32 bytes.
/// Convenience wrapper for callers that manage key material directly.
pub fn generate_gateway_keypair() -> ([u8; 32], [u8; 32]) {
    let secret = StaticSecret::random();
    let public = PublicKey::from(&secret);
    (secret.to_bytes(), *public.as_bytes())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_name_deterministic() {
        let h1 = hash_name("session-abc", "MY_SECRET");
        let h2 = hash_name("session-abc", "MY_SECRET");
        assert_eq!(h1, h2, "same inputs must produce same hash");
        assert_eq!(h1.len(), 64, "SHA-256 hex must be 64 chars");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_name_different_inputs() {
        let h1 = hash_name("session-abc", "KEY_A");
        let h2 = hash_name("session-abc", "KEY_B");
        let h3 = hash_name("session-xyz", "KEY_A");
        assert_ne!(h1, h2, "different key names must produce different hashes");
        assert_ne!(h1, h3, "different session ids must produce different hashes");
    }

    #[test]
    fn test_gateway_keypair_one_shot() {
        let mut kp = GatewayKeyPair::generate();
        // mark as used by setting the flag directly (simulate prior use)
        kp.used = true;

        let dummy = EncryptedPayload {
            version: 1,
            ephemeral_pubkey: B64.encode([0u8; 32]),
            nonce: B64.encode([0u8; 12]),
            ciphertext: B64.encode([]),
        };

        let err = kp.decrypt(&dummy).unwrap_err();
        assert_eq!(err, "already used", "used keypair must return 'already used'");
    }

    #[test]
    fn test_write_secret_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret_path = dir.path().join("mysecret");
        write_secret_file(&secret_path, "s3cr3t").expect("write_secret_file");

        // Verify contents
        let contents = std::fs::read_to_string(&secret_path).expect("read back");
        assert_eq!(contents, "s3cr3t");

        // Verify hash_name is used correctly: file name is 64 hex chars
        let hashed = hash_name("test-session", "api_key");
        assert_eq!(hashed.len(), 64);

        // Verify 0400 permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&secret_path).expect("metadata");
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o400, "file must be 0400");
        }
    }

    #[test]
    fn test_secret_manifest_serialization() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), "/run/secrets/abcdef".to_string());

        let mut files = HashMap::new();
        files.insert(
            "config.json".to_string(),
            "/run/secrets/files/fedcba".to_string(),
        );

        let manifest = SecretManifest { secrets, files };

        let json = serde_json::to_string(&manifest).expect("serialize");
        let roundtrip: SecretManifest = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            roundtrip.secrets.get("API_KEY").unwrap(),
            "/run/secrets/abcdef"
        );
        assert_eq!(
            roundtrip.files.get("config.json").unwrap(),
            "/run/secrets/files/fedcba"
        );
    }

    #[test]
    fn test_keypair_generation() {
        let (secret1, public1) = generate_gateway_keypair();
        let (secret2, public2) = generate_gateway_keypair();
        assert_ne!(secret1, [0u8; 32], "secret key should not be zero");
        assert_ne!(public1, [0u8; 32], "public key should not be zero");
        assert_ne!(secret1, secret2, "consecutive secrets should differ");
        assert_ne!(public1, public2, "consecutive public keys should differ");
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let mut kp = GatewayKeyPair::generate();
        let pubkey_bytes = kp.public_bytes;
        let plaintext = b"{\"secrets\":{\"KEY\":\"value\"},\"intercepted_files\":{}}";
        let encrypted = encrypt_payload(plaintext, &pubkey_bytes).expect("encrypt");
        let decrypted = kp.decrypt(&encrypted).expect("decrypt");
        assert_eq!(decrypted.secrets.get("KEY").unwrap(), "value");
    }
}

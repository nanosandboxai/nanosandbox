use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Decrypt a SOPS-encrypted YAML file and return its key/value pairs.
///
/// Resolves the file path (absolute, `~`-prefixed, or relative to `config_dir`),
/// calls `sops --decrypt` on the resolved path, and parses the resulting YAML.
/// The `sops` metadata key is silently skipped.
///
/// Returns an error if:
/// - the `sops` binary is not found on `$PATH`
/// - the resolved file does not exist
/// - `sops --decrypt` exits with a non-zero status
/// - the decrypted output contains nested YAML structures
pub fn decrypt_sops_file(
    file_path: &Path,
    config_dir: &Path,
) -> Result<HashMap<String, String>, String> {
    if !is_sops_available() {
        return Err(
            "sops binary not found on PATH. Install sops (https://github.com/getsops/sops) \
             to use encrypted secrets files."
                .to_string(),
        );
    }

    let resolved = resolve_path(file_path, config_dir);

    if !resolved.exists() {
        return Err(format!(
            "secrets file not found: {}",
            resolved.display()
        ));
    }

    let output = Command::new("sops")
        .arg("--decrypt")
        .arg(&resolved)
        .output()
        .map_err(|e| format!("failed to run sops: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "sops --decrypt failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let yaml_content = String::from_utf8(output.stdout)
        .map_err(|e| format!("sops output is not valid UTF-8: {e}"))?;

    parse_flat_yaml(&yaml_content)
}

/// Parse a flat YAML document into a `HashMap<String, String>`.
///
/// Skips the top-level `sops` key (SOPS metadata). Only scalar values
/// (strings, numbers, booleans, null) are accepted; nested mappings or
/// sequences cause an error.
fn parse_flat_yaml(content: &str) -> Result<HashMap<String, String>, String> {
    let value: serde_yaml::Value = serde_yaml::from_str(content)
        .map_err(|e| format!("failed to parse YAML: {e}"))?;

    let mapping = match &value {
        serde_yaml::Value::Mapping(m) => m,
        _ => return Err("expected a YAML mapping at the top level".to_string()),
    };

    let mut map = HashMap::new();

    for (k, v) in mapping {
        let key = match k {
            serde_yaml::Value::String(s) => s.clone(),
            other => format!("{other:?}"),
        };

        // Skip SOPS metadata key
        if key == "sops" {
            continue;
        }

        let scalar = scalar_to_string(v).ok_or_else(|| {
            format!(
                "key '{key}' has a nested structure; only scalar values (string, number, bool, \
                 null) are supported"
            )
        })?;

        map.insert(key, scalar);
    }

    Ok(map)
}

/// Convert a YAML scalar value to its string representation.
/// Returns `None` for mappings and sequences.
fn scalar_to_string(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => Some(String::new()),
        serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_) => None,
        // Tagged values — attempt to treat as string
        serde_yaml::Value::Tagged(tagged) => scalar_to_string(&tagged.value),
    }
}

/// Resolve `path` relative to `config_dir`.
///
/// - Absolute paths are returned unchanged.
/// - Paths starting with `~` have the tilde replaced with the user's home directory.
/// - All other paths are joined with `config_dir`.
fn resolve_path(path: &Path, config_dir: &Path) -> PathBuf {
    let path_str = path.to_string_lossy();

    if path.is_absolute() {
        return path.to_path_buf();
    }

    if path_str.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            let without_tilde = path_str.trim_start_matches('~').trim_start_matches('/');
            return home.join(without_tilde);
        }
    }

    config_dir.join(path)
}

/// Check whether the `sops` binary is available on `$PATH`.
pub fn is_sops_available() -> bool {
    Command::new("sops")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_flat_yaml_simple() {
        let yaml = "API_KEY: sk-test-123\nDB_PASSWORD: hunter2\n";
        let result = parse_flat_yaml(yaml).expect("should parse successfully");
        assert_eq!(result.get("API_KEY").map(String::as_str), Some("sk-test-123"));
        assert_eq!(result.get("DB_PASSWORD").map(String::as_str), Some("hunter2"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_flat_yaml_skips_sops_metadata() {
        let yaml = "\
API_KEY: sk-test-abc
sops:
    version: 3.7.3
    age:
        - recipient: age1abc
";
        let result = parse_flat_yaml(yaml).expect("should parse successfully");
        assert_eq!(result.get("API_KEY").map(String::as_str), Some("sk-test-abc"));
        assert!(!result.contains_key("sops"), "sops metadata key must be skipped");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_parse_flat_yaml_numeric_and_bool() {
        let yaml = "PORT: 5432\nDEBUG: true\n";
        let result = parse_flat_yaml(yaml).expect("should parse successfully");
        assert_eq!(result.get("PORT").map(String::as_str), Some("5432"));
        assert_eq!(result.get("DEBUG").map(String::as_str), Some("true"));
    }

    #[test]
    fn test_parse_flat_yaml_rejects_nested() {
        let yaml = "\
DATABASE:
    host: localhost
    port: 5432
";
        let err = parse_flat_yaml(yaml).expect_err("nested structure should return an error");
        assert!(
            err.contains("scalar value"),
            "error should mention 'scalar value', got: {err}"
        );
    }

    #[test]
    fn test_resolve_path_absolute() {
        let abs = Path::new("/etc/secrets.yml");
        let config_dir = Path::new("/home/user/.config/myapp");
        let resolved = resolve_path(abs, config_dir);
        assert_eq!(resolved, PathBuf::from("/etc/secrets.yml"));
    }

    #[test]
    fn test_resolve_path_relative() {
        let rel = Path::new("secrets.yml");
        let config_dir = Path::new("/home/user/.config/myapp");
        let resolved = resolve_path(rel, config_dir);
        assert_eq!(resolved, PathBuf::from("/home/user/.config/myapp/secrets.yml"));
    }

    #[test]
    fn test_decrypt_sops_file_missing() {
        let nonexistent = Path::new("/tmp/this_file_does_not_exist_sops_test_xyz.enc.yml");
        let config_dir = Path::new("/tmp");
        let err = decrypt_sops_file(nonexistent, config_dir)
            .expect_err("missing file should return an error");
        assert!(
            err.contains("not found"),
            "error should contain 'not found', got: {err}"
        );
    }
}

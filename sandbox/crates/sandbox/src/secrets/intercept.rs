use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Result of intercepting sensitive files from a cloned repository.
#[derive(Debug, Default)]
pub struct InterceptResult {
    /// Relative path -> file contents for each successfully intercepted file.
    pub intercepted: HashMap<String, String>,
    /// (relative path, error message) for files that could not be read.
    pub errors: Vec<(String, String)>,
}

/// Intercept sensitive files matching `patterns` inside `clone_dir`.
///
/// For each matched file: reads its contents, removes the original from disk,
/// appends its path to `.gitignore`, and records it in the result.
/// Already-intercepted files (overlapping patterns) are skipped.
/// If `patterns` is empty the function is a no-op.
pub fn intercept_sensitive_files(clone_dir: &Path, patterns: &[String]) -> InterceptResult {
    let mut result = InterceptResult::default();

    if patterns.is_empty() {
        debug!("intercept_sensitive_files: no patterns provided, skipping");
        return result;
    }

    for pattern in patterns {
        let full_pattern = clone_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let entries = match glob::glob(&pattern_str) {
            Ok(paths) => paths,
            Err(e) => {
                warn!("intercept: invalid glob pattern '{}': {}", pattern, e);
                continue;
            }
        };

        for entry in entries {
            let abs_path = match entry {
                Ok(p) => p,
                Err(e) => {
                    warn!("intercept: glob entry error: {}", e);
                    continue;
                }
            };

            // Only handle files, not directories
            if !abs_path.is_file() {
                continue;
            }

            let relative = match abs_path.strip_prefix(clone_dir) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => {
                    warn!(
                        "intercept: could not strip clone_dir prefix from '{}'",
                        abs_path.display()
                    );
                    continue;
                }
            };

            // Skip files already intercepted by an earlier pattern
            if result.intercepted.contains_key(&relative) {
                debug!("intercept: skipping already-intercepted file '{}'", relative);
                continue;
            }

            // Read contents
            let contents = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    let msg = e.to_string();
                    warn!("intercept: could not read '{}': {}", relative, msg);
                    result.errors.push((relative, msg));
                    continue;
                }
            };

            // Remove original from disk
            if let Err(e) = std::fs::remove_file(&abs_path) {
                warn!("intercept: could not remove '{}': {}", relative, e);
                // Still record as intercepted since we have the contents
            }

            // Add to .gitignore
            add_to_gitignore(clone_dir, &relative);

            info!("intercept: intercepted '{}'", relative);
            result.intercepted.insert(relative, contents);
        }
    }

    result
}

/// Append `relative_path` to `clone_dir/.gitignore` if it is not already listed.
fn add_to_gitignore(clone_dir: &Path, relative_path: &str) {
    let gitignore = clone_dir.join(".gitignore");

    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();

    // Check whether any line already matches exactly
    if existing.lines().any(|line| line.trim() == relative_path) {
        debug!("add_to_gitignore: '{}' already present", relative_path);
        return;
    }

    let entry = if existing.ends_with('\n') || existing.is_empty() {
        format!("{}\n", relative_path)
    } else {
        format!("\n{}\n", relative_path)
    };

    if let Err(e) = std::fs::write(
        &gitignore,
        format!("{}{}", existing, entry),
    ) {
        warn!("add_to_gitignore: could not write .gitignore: {}", e);
    }
}

/// Dry-run scan: find all files matching `patterns` inside `clone_dir` without
/// modifying anything.
pub fn scan_sensitive_files(clone_dir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut found = Vec::new();

    for pattern in patterns {
        let full_pattern = clone_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let entries = match glob::glob(&pattern_str) {
            Ok(paths) => paths,
            Err(e) => {
                warn!("scan: invalid glob pattern '{}': {}", pattern, e);
                continue;
            }
        };

        for entry in entries {
            match entry {
                Ok(p) if p.is_file() => found.push(p),
                Ok(_) => {}
                Err(e) => warn!("scan: glob entry error: {}", e),
            }
        }
    }

    found
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Create a standard test directory layout:
    /// .env, app.key, README.md, src.rs, config/credentials.json
    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempdir().expect("create tempdir");
        let base = dir.path();

        fs::write(base.join(".env"), "SECRET=hunter2\n").unwrap();
        fs::write(base.join("app.key"), "-----BEGIN RSA PRIVATE KEY-----\n").unwrap();
        fs::write(base.join("README.md"), "# Project\n").unwrap();
        fs::write(base.join("src.rs"), "fn main() {}\n").unwrap();

        let config_dir = base.join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("credentials.json"), r#"{"token":"abc"}"#).unwrap();

        dir
    }

    #[test]
    fn test_empty_patterns_is_noop() {
        let dir = setup_test_dir();
        let base = dir.path();

        let result = intercept_sensitive_files(base, &[]);

        assert!(result.intercepted.is_empty());
        assert!(result.errors.is_empty());
        // .env must still exist on disk
        assert!(base.join(".env").exists());
    }

    #[test]
    fn test_intercept_env_file() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec![".env".to_string()];
        let result = intercept_sensitive_files(base, &patterns);

        assert_eq!(result.intercepted.len(), 1);
        assert!(result.intercepted.contains_key(".env"));
        assert_eq!(result.intercepted[".env"], "SECRET=hunter2\n");
        assert!(result.errors.is_empty());

        // Original file must be gone
        assert!(!base.join(".env").exists());

        // README.md must still be present
        assert!(base.join("README.md").exists());
    }

    #[test]
    fn test_intercept_glob_pattern() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec!["*.key".to_string()];
        let result = intercept_sensitive_files(base, &patterns);

        assert_eq!(result.intercepted.len(), 1);
        assert!(result.intercepted.contains_key("app.key"));
        assert!(!base.join("app.key").exists());
    }

    #[test]
    fn test_intercept_nested_file() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec!["config/credentials.json".to_string()];
        let result = intercept_sensitive_files(base, &patterns);

        assert_eq!(result.intercepted.len(), 1);
        assert!(result.intercepted.contains_key("config/credentials.json"));
        assert!(!base.join("config/credentials.json").exists());
    }

    #[test]
    fn test_intercept_multiple_patterns() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec![
            ".env".to_string(),
            "*.key".to_string(),
            "config/credentials.json".to_string(),
        ];
        let result = intercept_sensitive_files(base, &patterns);

        assert_eq!(result.intercepted.len(), 3);
        assert!(result.intercepted.contains_key(".env"));
        assert!(result.intercepted.contains_key("app.key"));
        assert!(result.intercepted.contains_key("config/credentials.json"));
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_intercept_adds_to_gitignore() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec![".env".to_string()];
        intercept_sensitive_files(base, &patterns);

        let gitignore = fs::read_to_string(base.join(".gitignore")).expect(".gitignore created");
        assert!(
            gitignore.lines().any(|l| l.trim() == ".env"),
            ".gitignore should contain '.env', got: {:?}",
            gitignore
        );

        // Running again must not duplicate the entry
        intercept_sensitive_files(base, &[".env".to_string()]); // .env gone, no-op
        let gitignore2 = fs::read_to_string(base.join(".gitignore")).unwrap();
        let count = gitignore2.lines().filter(|l| l.trim() == ".env").count();
        assert_eq!(count, 1, "duplicate entries in .gitignore");
    }

    #[test]
    fn test_scan_dry_run_does_not_modify() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec![".env".to_string(), "*.key".to_string()];
        let found = scan_sensitive_files(base, &patterns);

        assert_eq!(found.len(), 2);

        // Files must still exist
        assert!(base.join(".env").exists());
        assert!(base.join("app.key").exists());

        // .gitignore must NOT have been created
        // (or if it existed beforehand it should be unchanged)
        if base.join(".gitignore").exists() {
            let gi = fs::read_to_string(base.join(".gitignore")).unwrap();
            assert!(!gi.contains(".env"), "scan should not modify .gitignore");
        }
    }

    #[test]
    fn test_nonexistent_pattern_is_ignored() {
        let dir = setup_test_dir();
        let base = dir.path();

        let patterns = vec!["does_not_exist_*.xyz".to_string()];
        let result = intercept_sensitive_files(base, &patterns);

        assert!(result.intercepted.is_empty());
        assert!(result.errors.is_empty());
    }
}

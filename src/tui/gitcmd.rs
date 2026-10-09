//! Hardened host-git commands for sandbox project clones.
//!
//! The sandbox agent has a read-write virtiofs mount over its clone of the
//! project repository.  This means the agent can rewrite the clone's
//! `.git/config`, `.git/hooks`, `.gitattributes`, etc.  The system `git`
//! honours `core.fsmonitor`, hooks, pager, editor, and many other config
//! keys from that config, so running `git status` / `git diff` / `git fetch`
//! in the clone without sanitisation is arbitrary code execution from the VM.
//!
//! Every function in this module is designed to eliminate those vectors.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A `git` command with all agent-controllable config vectors disabled.
///
/// The returned [`Command`] is pre-configured with `-c` flags that override
/// every known git-config key an attacker could use for code execution:
///
/// | Key | Why disabled |
/// |---|---|
/// | `core.fsmonitor` | Arbitrary command per file change |
/// | `core.hooksPath` | Hook directory (pre-commit, post-checkout, …) |
/// | `core.pager` | Arbitrary command for paged output |
/// | `core.editor` | Arbitrary command for editor invocation |
/// | `core.sshCommand` | Arbitrary command for SSH transport |
/// | `core.gitProxy` | Arbitrary command for network proxy |
/// | `core.askPass` | Arbitrary command for credential prompt |
/// | `core.alternateRefsCommand` | Arbitrary command for alternate refs |
/// | `core.attributesFile` | `.gitattributes` can specify filter drivers |
/// | `diff.external` | Arbitrary command for diffing |
/// | `uploadpack.packObjectsHook` | Arbitrary command during fetch/clone |
///
/// `--no-optional-locks` prevents background index-lock contention.
pub fn host_git() -> Command {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.pager=cat",
        "-c",
        "core.editor=false",
        "-c",
        "core.sshCommand=false",
        "-c",
        "core.gitProxy=",
        "-c",
        "core.askPass=",
        "-c",
        "core.alternateRefsCommand=",
        "-c",
        "core.attributesFile=/dev/null",
        "-c",
        "diff.external=",
        "-c",
        "uploadpack.packObjectsHook=",
        "--no-optional-locks",
    ]);
    c
}

/// True if `dir` has a real `.git` directory, or a `.git` file whose `gitdir:`
/// reference resolves **inside** `dir`.
///
/// Returns `false` for:
/// - A symlinked `.git` (agent redirection to an attacker-controlled repo).
/// - A `.git` file whose `gitdir:` target points outside `dir`.
/// - A missing `.git` entry.
pub fn has_real_git_dir(dir: &Path) -> bool {
    let git_path = dir.join(".git");

    let meta = match fs::symlink_metadata(&git_path) {
        Ok(m) => m,
        Err(_) => return false,
    };

    // Symlink → agent redirection vector.
    if meta.file_type().is_symlink() {
        return false;
    }

    // Real directory → legitimate bare or non-bare repo.
    if meta.is_dir() {
        return true;
    }

    // Regular file → "gitdir:" pointer (common with `git worktree`).
    if meta.is_file() {
        let content = match fs::read_to_string(&git_path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let target = content
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("gitdir: "))
            .map(str::trim)
            .unwrap_or("");

        if target.is_empty() {
            return false;
        }

        let resolved = dir.join(target);
        return match resolved.canonicalize() {
            Ok(canon) => canon.starts_with(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())),
            Err(_) => false,
        };
    }

    false
}

/// Namespaced ref that a sandbox's changes are fetched to in the source repo.
///
/// Example: `clone_namespaced_ref("sb-abc123")` → `refs/nanosb/sb-abc123`.
pub fn clone_namespaced_ref(sandbox_id: &str) -> String {
    format!("refs/nanosb/{}", sandbox_id)
}

/// Strip dangerous keys and sections from a clone's `.git/config`.
///
/// Writes a `.git/config.nanosb.bak` backup first, then removes every line
/// belonging to the following dangerous keys and sections:
///
/// **Keys:** `core.fsmonitor`, `core.pager`, `core.editor`, `core.sshCommand`,
/// `core.hooksPath`, `core.gitProxy`, `core.askPass`, `core.alternateRefsCommand`,
/// `core.attributesFile`.
///
/// **Sections:** `diff.*`, `filter.*`, `alias.*`, `uploadpack.*`, `include.*`.
///
/// The function is **idempotent**: calling it twice produces the same result as
/// calling it once.
pub fn sanitize_clone_config(clone_path: &Path) -> Result<(), String> {
    let config_path = clone_path.join(".git").join("config");
    let bak_path = clone_path.join(".git").join("config.nanosb.bak");

    // Read the current config.
    let content = fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read {}: {}", config_path.display(), e))?;

    // Write backup (overwrite if exists — idempotent).
    fs::write(&bak_path, &content)
        .map_err(|e| format!("failed to write backup {}: {}", bak_path.display(), e))?;

    // Dangerous section headers (prefix match — the `*` glob).
    // We match the opening `[section` part so that `[diff "driver"]` is caught.
    let dangerous_section_prefixes: &[&str] = &[
        "[diff",
        "[filter",
        "[alias",
        "[uploadpack",
        "[include",
    ];

    // Dangerous keys inside `[core]` (exact match on the key name).
    let dangerous_core_keys: &[&str] = &[
        "fsmonitor",
        "pager",
        "editor",
        "sshcommand",
        "sshCommand",
        "hooksPath",
        "gitProxy",
        "askPass",
        "alternateRefsCommand",
        "attributesFile",
    ];

    let mut in_dangerous_section = false;
    let mut in_core_section = false;
    let mut output = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Track section boundaries. Git section names are case-insensitive, so
        // compare lowercased (a `[CORE]` header must match `[core`).
        let lowered = trimmed.to_ascii_lowercase();
        if trimmed.starts_with('[') {
            in_dangerous_section = dangerous_section_prefixes
                .iter()
                .any(|p| lowered.starts_with(p));
            in_core_section = lowered.starts_with("[core");
            if in_dangerous_section {
                // Skip the section header itself.
                continue;
            }
            output.push(line);
            continue;
        }

        // Skip everything inside a dangerous section.
        if in_dangerous_section {
            continue;
        }

        // Skip dangerous keys inside `[core]`.
        if in_core_section {
            if let Some(eq) = trimmed.find('=') {
                let key_name = trimmed[..eq].trim();
                if dangerous_core_keys
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(key_name))
                {
                    continue;
                }
            }
        }

        output.push(line);
    }

    // Write the sanitised config.
    let mut out_content: Vec<u8> = output.join("\n").into_bytes();
    out_content.push(b'\n');

    let mut f = fs::File::create(&config_path)
        .map_err(|e| format!("failed to write {}: {}", config_path.display(), e))?;
    f.write_all(&out_content)
        .map_err(|e| format!("failed to write {}: {}", config_path.display(), e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    // ── host_git() ──────────────────────────────────────────────────────────

    #[test]
    fn host_git_contains_fsmonitor_false() {
        let cmd = host_git();
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        let idx = args.iter().position(|a| *a == "core.fsmonitor=false");
        assert!(idx.is_some(), "expected core.fsmonitor=false in args: {args:?}");
    }

    #[test]
    fn host_git_contains_hooks_path_dev_null() {
        let cmd = host_git();
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        let idx = args.iter().position(|a| *a == "core.hooksPath=/dev/null");
        assert!(
            idx.is_some(),
            "expected core.hooksPath=/dev/null in args: {args:?}"
        );
    }

    #[test]
    fn host_git_contains_no_optional_locks() {
        let cmd = host_git();
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(
            args.contains(&"--no-optional-locks"),
            "expected --no-optional-locks in args: {args:?}"
        );
    }

    // ── has_real_git_dir() ──────────────────────────────────────────────────

    #[test]
    fn has_real_git_dir_accepts_real_dir() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        assert!(has_real_git_dir(tmp.path()));
    }

    #[test]
    fn has_real_git_dir_rejects_symlink() {
        let real = TempDir::new().unwrap();
        let real_git = real.path().join(".git");
        fs::create_dir(&real_git).unwrap();

        let fake = TempDir::new().unwrap();
        let link = fake.path().join(".git");
        symlink(&real_git, &link).unwrap();

        assert!(!has_real_git_dir(fake.path()));
    }

    #[test]
    fn has_real_git_dir_rejects_gitfile_pointing_outside() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_git = outside.path().join(".git");
        fs::create_dir(&outside_git).unwrap();

        let gitfile = tmp.path().join(".git");
        fs::write(&gitfile, format!("gitdir: {}\n", outside_git.display())).unwrap();

        assert!(!has_real_git_dir(tmp.path()));
    }

    #[test]
    fn has_real_git_dir_accepts_gitfile_pointing_inside() {
        let tmp = TempDir::new().unwrap();
        let inner = tmp.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let inner_git = inner.join(".git");
        fs::create_dir(&inner_git).unwrap();

        // .git file at the root pointing to inner/.git
        let gitfile = tmp.path().join(".git");
        fs::write(&gitfile, format!("gitdir: inner/.git\n")).unwrap();

        assert!(has_real_git_dir(tmp.path()));
    }

    #[test]
    fn has_real_git_dir_returns_false_when_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!has_real_git_dir(tmp.path()));
    }

    // ── clone_namespaced_ref() ──────────────────────────────────────────────

    #[test]
    fn clone_namespaced_ref_format() {
        assert_eq!(clone_namespaced_ref("sb-abc"), "refs/nanosb/sb-abc");
    }

    // ── sanitize_clone_config() ─────────────────────────────────────────────

    #[test]
    fn sanitize_clone_config_removes_fsmonitor() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        fs::write(
            &config_path,
            r#"[core]
	fsmonitor = /tmp/evil
[user]
	name = Test User
	email = test@example.com
"#,
        )
        .unwrap();

        sanitize_clone_config(tmp.path()).unwrap();

        let sanitised = fs::read_to_string(&config_path).unwrap();
        assert!(
            !sanitised.contains("fsmonitor"),
            "fsmonitor should be removed, got: {sanitised}"
        );
        assert!(
            sanitised.contains("Test User"),
            "benign [user] section should survive, got: {sanitised}"
        );
    }

    #[test]
    fn sanitize_clone_config_is_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();
        let config_path = git_dir.join("config");
        // Uppercase section name must still be recognised (git is case-insensitive).
        fs::write(&config_path, "[CORE]\n\tfsmonitor = /tmp/evil\n").unwrap();
        sanitize_clone_config(tmp.path()).unwrap();
        let sanitised = fs::read_to_string(&config_path).unwrap();
        assert!(
            !sanitised.to_ascii_lowercase().contains("fsmonitor"),
            "uppercase [CORE] fsmonitor should be removed, got: {sanitised}"
        );
    }

    #[test]
    fn sanitize_clone_config_writes_backup() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        let original = "[core]\n\tfsmonitor = /tmp/evil\n";
        fs::write(&config_path, original).unwrap();

        sanitize_clone_config(tmp.path()).unwrap();

        let bak_path = git_dir.join("config.nanosb.bak");
        assert!(bak_path.exists(), "backup file should exist");
        let bak_content = fs::read_to_string(&bak_path).unwrap();
        assert_eq!(bak_content, original);
    }

    #[test]
    fn sanitize_clone_config_idempotent() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        fs::write(
            &config_path,
            r#"[core]
	fsmonitor = /tmp/evil
[user]
	name = Test User
"#,
        )
        .unwrap();

        sanitize_clone_config(tmp.path()).unwrap();
        let after_first = fs::read_to_string(&config_path).unwrap();

        sanitize_clone_config(tmp.path()).unwrap();
        let after_second = fs::read_to_string(&config_path).unwrap();

        assert_eq!(
            after_first, after_second,
            "second call should produce identical output"
        );
    }

    #[test]
    fn sanitize_clone_config_removes_dangerous_sections() {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir(&git_dir).unwrap();

        let config_path = git_dir.join("config");
        fs::write(
            &config_path,
            r#"[core]
	repositoryformatversion = 0
[diff "custom"]
	textconv = /tmp/evil
[filter "lfs"]
	clean = git-lfs clean %f
	smudge = git-lfs smudge %f
[alias]
	evil = !/tmp/evil
[uploadpack]
	packObjectsHook = /tmp/evil
[include]
	path = /tmp/evil-config
[user]
	name = Test User
"#,
        )
        .unwrap();

        sanitize_clone_config(tmp.path()).unwrap();

        let sanitised = fs::read_to_string(&config_path).unwrap();
        assert!(!sanitised.contains("[diff"), "diff section should be removed");
        assert!(
            !sanitised.contains("[filter"),
            "filter section should be removed"
        );
        assert!(
            !sanitised.contains("[alias"),
            "alias section should be removed"
        );
        assert!(
            !sanitised.contains("[uploadpack"),
            "uploadpack section should be removed"
        );
        assert!(
            !sanitised.contains("[include"),
            "include section should be removed"
        );
        assert!(
            sanitised.contains("Test User"),
            "benign [user] section should survive"
        );
        assert!(
            sanitised.contains("repositoryformatversion"),
            "benign core key should survive"
        );
    }
}

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
        "-c",
        "credential.helper=",
        "-c",
        "merge.verifySignatures=false",
        "-c",
        "gpg.program=false",
        // Block `ext::` remote helpers (arbitrary command execution via a remote URL).
        "-c",
        "protocol.ext.allow=never",
        "--no-optional-locks",
    ]);
    // Ignore the host user's global/system git config entirely: a poisoned
    // ~/.gitconfig (`url.*.insteadOf`, `core.hooksPath`, aliases, `includeIf`)
    // must not influence these operations.
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
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

    #[test]
    fn host_git_neutralizes_extra_vectors() {
        let cmd = host_git();
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        for needle in [
            "credential.helper=",
            "protocol.ext.allow=never",
            "gpg.program=false",
            "merge.verifySignatures=false",
        ] {
            assert!(args.contains(&needle), "expected {needle} in args");
        }
    }

    #[test]
    fn host_git_ignores_host_git_config() {
        let cmd = host_git();
        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()).unwrap_or_default(),
                )
            })
            .collect();
        assert!(envs
            .iter()
            .any(|(k, v)| k == "GIT_CONFIG_NOSYSTEM" && v == "1"));
        assert!(envs
            .iter()
            .any(|(k, v)| k == "GIT_CONFIG_GLOBAL" && v == "/dev/null"));
        assert!(envs.iter().any(|(k, _)| k == "GIT_TERMINAL_PROMPT"));
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

}

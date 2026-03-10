//! User settings with TOML config file load/save and tool detection.
//!
//! Settings are stored in `~/.nanosandbox/config.toml` and control git sync
//! behaviour and preferred tool configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Default‐value helper functions (used by serde)
// ---------------------------------------------------------------------------

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

fn default_editor() -> String {
    "auto".to_string()
}

// ---------------------------------------------------------------------------
// Settings structs
// ---------------------------------------------------------------------------

/// Controls automatic git synchronisation between the sandbox clone and the
/// source repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitSyncSettings {
    /// Whether to automatically sync branches back to the source repo.
    /// WARNING: when enabled this modifies local repository branches automatically.
    #[serde(default = "default_false")]
    pub auto_sync: bool,
    /// Show a notification when the sandbox auto-commits on teardown.
    #[serde(default = "default_true")]
    pub notify_on_commit: bool,
}

impl Default for GitSyncSettings {
    fn default() -> Self {
        Self {
            auto_sync: default_false(),
            notify_on_commit: default_true(),
        }
    }
}

/// Preferred tools for interacting with the sandbox from the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSettings {
    /// Preferred tool for the `/open` command. `"auto"` detects the first
    /// available tool from [`KNOWN_TOOLS`].
    #[serde(default = "default_editor")]
    pub editor: String,
    /// Optional custom command template. Use `{path}` as a placeholder for the
    /// file/directory path.
    #[serde(default)]
    pub custom_command: Option<String>,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            editor: default_editor(),
            custom_command: None,
        }
    }
}

/// Top-level user settings, persisted as TOML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UserSettings {
    /// Git synchronisation settings.
    #[serde(default)]
    pub gitsync: GitSyncSettings,
    /// Tool / editor settings.
    #[serde(default)]
    pub tools: ToolSettings,
}

impl UserSettings {
    /// Returns the default config file path: `~/.nanosandbox/config.toml`.
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".nanosandbox")
            .join("config.toml")
    }

    /// Load settings from the default config path.
    /// Returns defaults if the file is missing or cannot be parsed.
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    /// Load settings from a specific path.
    /// Returns defaults if the file is missing or cannot be parsed.
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to the default config path, creating parent directories
    /// if necessary.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::config_path())
    }

    /// Save settings to a specific path, creating parent directories if
    /// necessary.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::other(e)
        })?;
        std::fs::write(path, content)
    }
}

// ---------------------------------------------------------------------------
// Tool detection
// ---------------------------------------------------------------------------

/// Metadata for a known external tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    /// Human-readable name shown in UI.
    pub name: &'static str,
    /// Binary name to look up on `$PATH`.
    pub binary: &'static str,
    /// Whether the tool runs inside the terminal (TUI) as opposed to opening
    /// a separate GUI window.
    pub is_tui: bool,
    /// macOS application name for `open -a` fallback (GUI tools only).
    pub macos_app: Option<&'static str>,
}

/// Well-known tools that nanosandbox can open, ordered by detection priority.
pub const KNOWN_TOOLS: &[ToolInfo] = &[
    ToolInfo { name: "gitui", binary: "gitui", is_tui: true, macos_app: None },
    ToolInfo { name: "lazygit", binary: "lazygit", is_tui: true, macos_app: None },
    ToolInfo { name: "tig", binary: "tig", is_tui: true, macos_app: None },
    ToolInfo { name: "vscode", binary: "code", is_tui: false, macos_app: Some("Visual Studio Code") },
    ToolInfo { name: "cursor", binary: "cursor", is_tui: false, macos_app: Some("Cursor") },
    ToolInfo { name: "gitkraken", binary: "gitkraken", is_tui: false, macos_app: Some("GitKraken") },
    ToolInfo { name: "fork", binary: "fork", is_tui: false, macos_app: Some("Fork") },
];

/// Check whether a binary is available on `$PATH` using the `which` command.
pub fn is_tool_available(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve an editor preference to a `(binary, is_tui)` pair.
///
/// - `"auto"` — returns the first available tool from [`KNOWN_TOOLS`].
/// - Any other value — looks up by name or binary in [`KNOWN_TOOLS`]; if found
///   and available, returns it. If the name isn't in the known list but *is*
///   available on PATH, returns it as a non-TUI tool.
pub fn resolve_tool(editor_pref: &str) -> Option<(&'static str, bool)> {
    if editor_pref == "auto" {
        for tool in KNOWN_TOOLS {
            if is_tool_available(tool.binary) {
                return Some((tool.binary, tool.is_tui));
            }
        }
        return None;
    }

    // Look up by name or binary in the known list.
    for tool in KNOWN_TOOLS {
        if tool.name == editor_pref || tool.binary == editor_pref {
            if is_tool_available(tool.binary) {
                return Some((tool.binary, tool.is_tui));
            }
            return None;
        }
    }

    // Unknown tool name — caller should fall back to `custom_command`.
    None
}

/// Look up the macOS application name for a tool (by name or binary).
/// Returns `None` if the tool is not known or has no macOS app name.
pub fn macos_app_name(editor_pref: &str) -> Option<&'static str> {
    KNOWN_TOOLS.iter()
        .find(|t| t.name == editor_pref || t.binary == editor_pref)
        .and_then(|t| t.macos_app)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let s = UserSettings::default();
        assert!(!s.gitsync.auto_sync);
        assert!(s.gitsync.notify_on_commit);
        assert_eq!(s.tools.editor, "auto");
        assert!(s.tools.custom_command.is_none());
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let s = UserSettings::load_from(std::path::Path::new("/tmp/nonexistent_nanosb_cfg.toml"));
        assert_eq!(s, UserSettings::default());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("config.toml");

        let mut settings = UserSettings::default();
        settings.gitsync.auto_sync = true;
        settings.tools.editor = "lazygit".to_string();
        settings.tools.custom_command = Some("vim {path}".to_string());

        settings.save_to(&path).expect("save");
        let loaded = UserSettings::load_from(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn test_config_path() {
        let p = UserSettings::config_path();
        let p_str = p.to_string_lossy();
        assert!(p_str.ends_with("config.toml"), "path should end with config.toml: {p_str}");
        assert!(p_str.contains(".nanosandbox"), "path should contain .nanosandbox: {p_str}");
    }

    #[test]
    fn test_detect_tool_returns_false_for_fake() {
        assert!(!is_tool_available("definitely_not_a_real_tool_xyz_123"));
    }
}

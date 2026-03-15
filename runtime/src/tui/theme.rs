//! Built-in TUI colour themes.
//!
//! Each theme defines a palette of semantic colour roles used by the renderer.
//! Users switch themes with `/theme <name>` and the choice is persisted in
//! `~/.nanosandbox/config.toml`.

use ratatui::style::Color;
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Theme palette
// ---------------------------------------------------------------------------

/// A colour palette that controls every colour used by the TUI renderer.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Main background colour — painted on the entire frame.
    pub background: Color,
    /// Primary accent colour — focused borders, prompts, command highlights.
    pub accent: Color,
    /// Primary text colour — agent messages, filenames, panel names.
    pub text: Color,
    /// Muted/secondary text — unfocused borders, help hints, IDs.
    pub text_muted: Color,
    /// Success / positive — user messages, connected status, additions.
    pub success: Color,
    /// Warning / attention — streaming indicator, modified files, system msgs.
    pub warning: Color,
    /// Error / destructive — deleted files, failures.
    pub error: Color,
    /// Informational — renamed files.
    pub info: Color,
    /// Status-bar background.
    pub status_bar_bg: Color,
    /// Foreground for the autocomplete selected item.
    pub selection_fg: Color,
    /// Background for the autocomplete selected item.
    pub selection_bg: Color,
}

// ---------------------------------------------------------------------------
// Theme name enum
// ---------------------------------------------------------------------------

/// Identifies a built-in theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    /// Nanosandbox brand dark (coral red accent).
    Nanosandbox,
    /// Nanosandbox brand light (coral red accent, for light terminals).
    NanosandboxLight,
    /// Dracula — purple accent, vivid colours.
    Dracula,
    /// Catppuccin Mocha — warm pastels, mauve accent.
    Catppuccin,
    /// Tokyo Night — blue accent, soft pastels.
    TokyoNight,
    /// Nord — frost blue accent, cool arctic tones.
    Nord,
}

/// All theme names in display order.
pub const ALL_THEME_NAMES: &[&str] = &[
    "nanosandbox",
    "nanosandbox-light",
    "dracula",
    "catppuccin",
    "tokyo-night",
    "nord",
];

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Nanosandbox => "nanosandbox",
            Self::NanosandboxLight => "nanosandbox-light",
            Self::Dracula => "dracula",
            Self::Catppuccin => "catppuccin",
            Self::TokyoNight => "tokyo-night",
            Self::Nord => "nord",
        };
        f.write_str(s)
    }
}

impl FromStr for ThemeName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nanosandbox" => Ok(Self::Nanosandbox),
            "nanosandbox-light" => Ok(Self::NanosandboxLight),
            "dracula" => Ok(Self::Dracula),
            "catppuccin" => Ok(Self::Catppuccin),
            "tokyo-night" => Ok(Self::TokyoNight),
            "nord" => Ok(Self::Nord),
            other => Err(format!(
                "Unknown theme: '{}'. Available themes: {}",
                other,
                ALL_THEME_NAMES.join(", "),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Theme lookup
// ---------------------------------------------------------------------------

impl Theme {
    /// Look up a built-in theme by name.
    pub fn by_name(name: ThemeName) -> &'static Theme {
        match name {
            ThemeName::Nanosandbox => &NANOSANDBOX,
            ThemeName::NanosandboxLight => &NANOSANDBOX_LIGHT,
            ThemeName::Dracula => &DRACULA,
            ThemeName::Catppuccin => &CATPPUCCIN,
            ThemeName::TokyoNight => &TOKYO_NIGHT,
            ThemeName::Nord => &NORD,
        }
    }

    /// Resolve a theme name string, falling back to the default on error.
    pub fn resolve(name: &str) -> (&'static Theme, ThemeName) {
        let tn = ThemeName::from_str(name).unwrap_or(ThemeName::Nanosandbox);
        (Self::by_name(tn), tn)
    }
}

// ---------------------------------------------------------------------------
// Built-in palettes
// ---------------------------------------------------------------------------

/// Nanosandbox brand dark — coral red accent on dark background.
///
/// Uses only 256-color indexed palette so the theme renders correctly on all
/// terminals, including macOS Terminal.app (no truecolor), PuTTY, GNU screen,
/// and tmux without truecolor config.
///
/// Background uses `Indexed(16)` (fixed #000000 from the 6×6×6 cube) instead
/// of ANSI `Color::Black` because ANSI colors 0-15 can be remapped by terminal
/// themes — a "Solarized" or "Gruvbox" terminal scheme could turn ANSI Black
/// into dark gray or blue.  Indexed 16-231 are guaranteed fixed values.
static NANOSANDBOX: Theme = Theme {
    background: Color::Indexed(16),    // #000000 — fixed pure black (not remappable)
    accent: Color::Indexed(167),       // #D75F5F — closest 256-color to coral #E4584A
    text: Color::Indexed(231),         // #FFFFFF — fixed pure white (not remappable)
    text_muted: Color::Indexed(245),   // #8A8A8A — fixed medium gray
    success: Color::Green,
    warning: Color::Yellow,
    error: Color::Red,
    info: Color::Blue,
    status_bar_bg: Color::Indexed(238),// #444444 — fixed dark gray
    selection_fg: Color::Indexed(16),  // #000000
    selection_bg: Color::Indexed(167),
};

/// Nanosandbox brand light — coral red accent for light terminal backgrounds.
///
/// Uses fixed 256-color indexed palette like the dark theme for maximum
/// terminal compatibility.
static NANOSANDBOX_LIGHT: Theme = Theme {
    background: Color::Indexed(231),   // #FFFFFF — fixed pure white (not remappable)
    accent: Color::Indexed(167),       // #D75F5F — closest 256-color to coral #D4463A
    text: Color::Indexed(16),          // #000000 — fixed pure black
    text_muted: Color::Indexed(245),   // #8A8A8A — fixed medium gray
    success: Color::Green,
    warning: Color::Yellow,
    error: Color::Red,
    info: Color::Blue,
    status_bar_bg: Color::Indexed(252),// #D0D0D0 — fixed light gray
    selection_fg: Color::Indexed(231), // #FFFFFF
    selection_bg: Color::Indexed(167),
};

/// Dracula — purple accent, vivid colours on dark background.
static DRACULA: Theme = Theme {
    background: Color::Rgb(40, 42, 54),  // #282A36
    accent: Color::Rgb(189, 147, 249), // #BD93F9
    text: Color::Rgb(248, 248, 242),   // #F8F8F2
    text_muted: Color::Rgb(98, 114, 164), // #6272A4
    success: Color::Rgb(80, 250, 123), // #50FA7B
    warning: Color::Rgb(241, 250, 140), // #F1FA8C
    error: Color::Rgb(255, 85, 85),    // #FF5555
    info: Color::Rgb(139, 233, 253),   // #8BE9FD
    status_bar_bg: Color::Rgb(68, 71, 90), // #44475A
    selection_fg: Color::Rgb(40, 42, 54),
    selection_bg: Color::Rgb(189, 147, 249),
};

/// Catppuccin Mocha — warm pastels, mauve accent.
static CATPPUCCIN: Theme = Theme {
    background: Color::Rgb(30, 30, 46),  // #1E1E2E
    accent: Color::Rgb(203, 166, 247), // #CBA6F7
    text: Color::Rgb(205, 214, 244),   // #CDD6F4
    text_muted: Color::Rgb(88, 91, 112), // #585B70
    success: Color::Rgb(166, 227, 161), // #A6E3A1
    warning: Color::Rgb(249, 226, 175), // #F9E2AF
    error: Color::Rgb(243, 139, 168),  // #F38BA8
    info: Color::Rgb(137, 180, 250),   // #89B4FA
    status_bar_bg: Color::Rgb(24, 24, 37), // #181825
    selection_fg: Color::Rgb(30, 30, 46),  // #1E1E2E
    selection_bg: Color::Rgb(203, 166, 247),
};

/// Tokyo Night — blue accent, soft pastels on dark blue-gray.
static TOKYO_NIGHT: Theme = Theme {
    background: Color::Rgb(26, 27, 38),  // #1A1B26
    accent: Color::Rgb(122, 162, 247), // #7AA2F7
    text: Color::Rgb(192, 202, 245),   // #C0CAF5
    text_muted: Color::Rgb(86, 95, 137), // #565F89
    success: Color::Rgb(158, 206, 106), // #9ECE6A
    warning: Color::Rgb(224, 175, 104), // #E0AF68
    error: Color::Rgb(247, 118, 142),  // #F7768E
    info: Color::Rgb(125, 207, 255),   // #7DCFFF
    status_bar_bg: Color::Rgb(22, 22, 30), // #16161E
    selection_fg: Color::Rgb(26, 27, 38),  // #1A1B26
    selection_bg: Color::Rgb(122, 162, 247),
};

/// Nord — frost blue accent, cool arctic tones.
static NORD: Theme = Theme {
    background: Color::Rgb(46, 52, 64),  // #2E3440
    accent: Color::Rgb(136, 192, 208), // #88C0D0
    text: Color::Rgb(236, 239, 244),   // #ECEFF4
    text_muted: Color::Rgb(76, 86, 106), // #4C566A
    success: Color::Rgb(163, 190, 140), // #A3BE8C
    warning: Color::Rgb(235, 203, 139), // #EBCB8B
    error: Color::Rgb(191, 97, 106),   // #BF616A
    info: Color::Rgb(129, 161, 193),   // #81A1C1
    status_bar_bg: Color::Rgb(59, 66, 82), // #3B4252
    selection_fg: Color::Rgb(46, 52, 64),  // #2E3440
    selection_bg: Color::Rgb(136, 192, 208),
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_name_roundtrip() {
        for name_str in ALL_THEME_NAMES {
            let tn = ThemeName::from_str(name_str).expect(name_str);
            assert_eq!(&tn.to_string(), *name_str);
        }
    }

    #[test]
    fn test_unknown_theme_name() {
        let result = ThemeName::from_str("nonexistent");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("Unknown theme"));
        assert!(msg.contains("nonexistent"));
        assert!(msg.contains("nanosandbox"));
    }

    #[test]
    fn test_resolve_valid() {
        let (theme, name) = Theme::resolve("dracula");
        assert_eq!(name, ThemeName::Dracula);
        assert_eq!(theme.accent, Color::Rgb(189, 147, 249));
    }

    #[test]
    fn test_resolve_invalid_falls_back() {
        let (theme, name) = Theme::resolve("garbage");
        assert_eq!(name, ThemeName::Nanosandbox);
        assert_eq!(theme.accent, Color::Indexed(167));
    }

    #[test]
    fn test_by_name_returns_correct_palette() {
        let theme = Theme::by_name(ThemeName::Nord);
        assert_eq!(theme.accent, Color::Rgb(136, 192, 208));
    }

    #[test]
    fn test_all_theme_names_count() {
        assert_eq!(ALL_THEME_NAMES.len(), 6);
    }
}

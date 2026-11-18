//! Layout calculator for the single active agent panel.

/// Estimate the inner dimensions (cols, rows) the active agent panel will get
/// given the terminal size and sidebar state.
///
/// Used to allocate the SSH PTY at the correct size before the first
/// render cycle, avoiding the resize race where init commands would be
/// sent at 80×24 while the panel is actually narrower.
pub fn estimate_panel_inner_size(term_cols: u16, term_rows: u16, has_sidebar: bool) -> (u16, u16) {
    let chrome_rows = 4u16;
    let available_rows = term_rows.saturating_sub(chrome_rows);
    let available_cols = if has_sidebar {
        // Sidebar takes ~30%, panels get ~70%.
        (term_cols as u32 * 70 / 100) as u16
    } else {
        term_cols
    };

    // Inner area = available minus border (2 cols left+right, 2 rows top+bottom).
    let inner_cols = available_cols.saturating_sub(2);
    let inner_rows = available_rows.saturating_sub(2);

    (inner_cols.max(1), inner_rows.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tabbed_no_sidebar() {
        // Full width, minus chrome (padding + tab strip) and border.
        let (cols, rows) = estimate_panel_inner_size(160, 40, false);
        assert_eq!(cols, 158); // 160 - 2 border
        assert_eq!(rows, 34); // 40 - 4 chrome - 2 border
    }

    #[test]
    fn test_estimate_tabbed_with_sidebar() {
        // Sidebar takes 30%: 160*70/100 = 112 panel cols.
        let (cols, rows) = estimate_panel_inner_size(160, 40, true);
        assert_eq!(cols, 110); // 112 - 2 border
        assert_eq!(rows, 34); // 40 - 4 chrome - 2 border
    }

    #[test]
    fn test_estimate_tiny_terminal_clamps_to_min() {
        let (cols, rows) = estimate_panel_inner_size(1, 1, false);
        assert_eq!(cols, 1);
        assert_eq!(rows, 1);
    }
}

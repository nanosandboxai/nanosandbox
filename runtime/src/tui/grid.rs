//! Auto-grid layout calculator for agent panels.

/// Calculate grid dimensions (rows, cols) for a given panel count.
///
/// Uses a 2D grid so panels get reasonable width (~80 cols each on a
/// standard terminal) instead of all being crammed into one row.
///
/// The renderer distributes panels row-by-row, so the last row may have
/// fewer panels — each stretching to fill the available width.
pub fn grid_dimensions(panel_count: usize) -> (usize, usize) {
    match panel_count {
        0 => (0, 0),
        1 => (1, 1),
        2 => (1, 2),
        3 => (2, 2), // 2 top, 1 bottom full-width
        4 => (2, 2),
        5 => (2, 3), // 3 top, 2 bottom
        6 => (2, 3),
        n => {
            let cols = (n as f64).sqrt().ceil() as usize;
            let rows = (n + cols - 1) / cols;
            (rows, cols)
        }
    }
}

/// Estimate the inner dimensions (cols, rows) a panel will get given
/// the terminal size, panel count, sidebar state, and zoom mode.
///
/// Used to allocate the SSH PTY at the correct size before the first
/// render cycle, avoiding the resize race where init commands would be
/// sent at 80×24 while the panel is actually narrower.
pub fn estimate_panel_inner_size(
    term_cols: u16,
    term_rows: u16,
    panel_count: usize,
    has_sidebar: bool,
    is_zoomed: bool,
) -> (u16, u16) {
    if panel_count == 0 {
        return (80, 24);
    }

    // Subtract chrome: global input bar (1 row) + status bar (1 row).
    let available_rows = term_rows.saturating_sub(2);
    let available_cols = if has_sidebar {
        // Sidebar takes ~30%, panels get ~70%.
        (term_cols as u32 * 70 / 100) as u16
    } else {
        term_cols
    };

    if is_zoomed || panel_count == 1 {
        let inner_cols = available_cols.saturating_sub(2);
        let inner_rows = available_rows.saturating_sub(2);
        return (inner_cols.max(1), inner_rows.max(1));
    }

    let (grid_rows, grid_cols) = grid_dimensions(panel_count);
    if grid_rows == 0 || grid_cols == 0 {
        return (80, 24);
    }

    let cell_cols = available_cols / grid_cols as u16;
    let cell_rows = available_rows / grid_rows as u16;

    // Inner area = cell minus border (2 cols left+right, 2 rows top+bottom).
    let inner_cols = cell_cols.saturating_sub(2);
    let inner_rows = cell_rows.saturating_sub(2);

    (inner_cols.max(1), inner_rows.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_0_panels() {
        assert_eq!(grid_dimensions(0), (0, 0));
    }

    #[test]
    fn test_grid_1_panel() {
        assert_eq!(grid_dimensions(1), (1, 1));
    }

    #[test]
    fn test_grid_2_panels() {
        assert_eq!(grid_dimensions(2), (1, 2));
    }

    #[test]
    fn test_grid_3_panels() {
        assert_eq!(grid_dimensions(3), (2, 2));
    }

    #[test]
    fn test_grid_4_panels() {
        assert_eq!(grid_dimensions(4), (2, 2));
    }

    #[test]
    fn test_grid_5_panels() {
        assert_eq!(grid_dimensions(5), (2, 3));
    }

    #[test]
    fn test_grid_6_panels() {
        assert_eq!(grid_dimensions(6), (2, 3));
    }

    #[test]
    fn test_grid_7_panels() {
        assert_eq!(grid_dimensions(7), (3, 3));
    }

    #[test]
    fn test_grid_9_panels() {
        assert_eq!(grid_dimensions(9), (3, 3));
    }

    #[test]
    fn test_estimate_zoomed() {
        // Zoomed gives full terminal minus chrome.
        let (cols, rows) = estimate_panel_inner_size(160, 40, 4, false, true);
        assert_eq!(cols, 158); // 160 - 2 border
        assert_eq!(rows, 36); // 40 - 2 chrome - 2 border
    }

    #[test]
    fn test_estimate_4_panels_no_sidebar() {
        // 2x2 grid: each cell = 80x19, inner = 78x17.
        let (cols, rows) = estimate_panel_inner_size(160, 40, 4, false, false);
        assert_eq!(cols, 78); // 160/2 - 2
        assert_eq!(rows, 17); // (40-2)/2 - 2
    }

    #[test]
    fn test_estimate_with_sidebar() {
        // Sidebar takes 30%: 160*70/100 = 112 panel cols.
        // 2x2 grid: cell = 56, inner = 54.
        let (cols, rows) = estimate_panel_inner_size(160, 40, 4, true, false);
        assert_eq!(cols, 54);
        assert_eq!(rows, 17);
    }

    #[test]
    fn test_estimate_single_panel() {
        let (cols, rows) = estimate_panel_inner_size(160, 40, 1, false, false);
        assert_eq!(cols, 158);
        assert_eq!(rows, 36);
    }
}

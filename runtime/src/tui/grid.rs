//! Auto-grid layout calculator for agent panels.

/// Calculate grid dimensions (rows, cols) for a given panel count.
///
/// All panels are placed side by side horizontally (1 row, n columns).
/// Each panel gets an equal share of the horizontal space.
pub fn grid_dimensions(panel_count: usize) -> (usize, usize) {
    match panel_count {
        0 => (0, 0),
        n => (1, n),
    }
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
        assert_eq!(grid_dimensions(3), (1, 3));
    }

    #[test]
    fn test_grid_5_panels() {
        assert_eq!(grid_dimensions(5), (1, 5));
    }
}

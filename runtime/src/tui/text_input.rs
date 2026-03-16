//! Multiline text input buffer with cursor tracking and visual line wrapping.

use unicode_width::UnicodeWidthChar;

/// A single visual (wrapped) line, referencing byte offsets into the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    /// Byte offset in the source text where this visual line starts.
    pub byte_start: usize,
    /// Byte offset in the source text where this visual line ends (exclusive).
    pub byte_end: usize,
}

/// A multiline text input buffer with cursor tracking.
///
/// Stores text as a flat `String` with a byte-offset cursor position.
/// Soft-wrapping and visual-line computation are performed on demand
/// given an available width, making this struct independent of terminal
/// dimensions.
#[derive(Debug, Clone)]
pub struct TextInput {
    /// The full text buffer. May contain '\n' for manual newlines.
    text: String,
    /// Cursor position as a byte offset into `text`. Always on a char boundary.
    /// Range: `0..=text.len()`
    cursor: usize,
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

impl TextInput {
    /// Create a new empty text input.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    // === Accessors ===

    /// Get the full text content.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Get the cursor byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    // === Editing ===

    /// Insert a character at the cursor position and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a string at the cursor position.
    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Insert a newline at the cursor position.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Find the previous char boundary.
        let prev = self.prev_char_boundary();
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Delete the character at the cursor position (delete key).
    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_char_boundary();
        self.text.drain(self.cursor..next);
    }

    // === Horizontal movement ===

    /// Move the cursor one character to the left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_char_boundary();
        }
    }

    /// Move the cursor one character to the right.
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.next_char_boundary();
        }
    }

    /// Move the cursor to the start of the current logical line.
    pub fn move_home(&mut self) {
        // Search backwards for '\n' or start of text.
        let before = &self.text[..self.cursor];
        match before.rfind('\n') {
            Some(pos) => self.cursor = pos + 1,
            None => self.cursor = 0,
        }
    }

    /// Move the cursor to the end of the current logical line.
    pub fn move_end(&mut self) {
        let after = &self.text[self.cursor..];
        match after.find('\n') {
            Some(pos) => self.cursor += pos,
            None => self.cursor = self.text.len(),
        }
    }

    // === Vertical movement (width-dependent) ===

    /// Move the cursor up one visual line, preserving column where possible.
    pub fn move_up(&mut self, width: usize) {
        let width = width.max(1);
        let lines = self.visual_lines(width);
        let (row, col) = self.cursor_row_col(&lines);
        if row == 0 {
            return;
        }
        let target = &lines[row - 1];
        self.cursor = self.byte_at_visual_col(target, col, width);
    }

    /// Move the cursor down one visual line, preserving column where possible.
    pub fn move_down(&mut self, width: usize) {
        let width = width.max(1);
        let lines = self.visual_lines(width);
        let (row, col) = self.cursor_row_col(&lines);
        if row + 1 >= lines.len() {
            return;
        }
        let target = &lines[row + 1];
        self.cursor = self.byte_at_visual_col(target, col, width);
    }

    // === Bulk operations ===

    /// Clear the text and return the previous content. Resets cursor to 0.
    pub fn clear(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Replace the entire text. Cursor moves to the end.
    pub fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    // === Visual line computation ===

    /// Compute wrapped visual lines for the given available width.
    ///
    /// Each `VisualLine` references byte offsets into the source text.
    /// A width of 0 is treated as 1 to avoid infinite loops.
    pub fn visual_lines(&self, width: usize) -> Vec<VisualLine> {
        let width = width.max(1);
        let mut result = Vec::new();

        if self.text.is_empty() {
            result.push(VisualLine {
                byte_start: 0,
                byte_end: 0,
            });
            return result;
        }

        let mut line_start = 0;

        for logical_line in self.text.split('\n') {
            let logical_end = line_start + logical_line.len();

            if logical_line.is_empty() {
                result.push(VisualLine {
                    byte_start: line_start,
                    byte_end: line_start,
                });
            } else {
                let mut vis_start = line_start;
                let mut col_width = 0usize;

                for (i, ch) in logical_line.char_indices() {
                    let char_w = ch.width().unwrap_or(0).max(1);
                    if col_width + char_w > width && col_width > 0 {
                        // Wrap: emit current visual line, start new one.
                        result.push(VisualLine {
                            byte_start: vis_start,
                            byte_end: line_start + i,
                        });
                        vis_start = line_start + i;
                        col_width = char_w;
                    } else {
                        col_width += char_w;
                    }
                }

                // Emit the last segment of this logical line.
                result.push(VisualLine {
                    byte_start: vis_start,
                    byte_end: logical_end,
                });
            }

            // Skip past the '\n' delimiter.
            line_start = logical_end + 1;
        }

        result
    }

    /// Compute the (visual_row, visual_col) of the cursor given available width.
    pub fn cursor_visual_position(&self, width: usize) -> (usize, usize) {
        let lines = self.visual_lines(width);
        self.cursor_row_col(&lines)
    }

    /// Total number of visual lines for the given width.
    pub fn visual_line_count(&self, width: usize) -> usize {
        self.visual_lines(width).len()
    }

    // === Internal helpers ===

    /// Find the byte offset of the previous character boundary before the cursor.
    fn prev_char_boundary(&self) -> usize {
        let mut pos = self.cursor;
        if pos == 0 {
            return 0;
        }
        pos -= 1;
        while pos > 0 && !self.text.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    /// Find the byte offset of the next character boundary after the cursor.
    fn next_char_boundary(&self) -> usize {
        let mut pos = self.cursor + 1;
        while pos < self.text.len() && !self.text.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    }

    /// Find the (row, col) of the cursor within the given visual lines.
    /// `col` is measured in display-width columns.
    fn cursor_row_col(&self, lines: &[VisualLine]) -> (usize, usize) {
        for (i, vl) in lines.iter().enumerate() {
            // The cursor is "on" this line if it's within [byte_start, byte_end],
            // or on the last line if at text end.
            if self.cursor >= vl.byte_start && self.cursor <= vl.byte_end {
                // If the cursor equals byte_end, it could belong to this line
                // or the next. Prefer the next line unless this is the last one
                // or the cursor is right after the text content.
                if self.cursor == vl.byte_end && i + 1 < lines.len() {
                    let next = &lines[i + 1];
                    // If the next line starts at the same offset as cursor,
                    // cursor belongs to the next line (start of wrapped line).
                    if next.byte_start == self.cursor {
                        continue;
                    }
                }
                let segment = &self.text[vl.byte_start..self.cursor];
                let col: usize = segment
                    .chars()
                    .map(|c| c.width().unwrap_or(0).max(1))
                    .sum();
                return (i, col);
            }
        }
        // Fallback: cursor at end of last line.
        if let Some(last) = lines.last() {
            let segment = &self.text[last.byte_start..self.cursor.min(self.text.len())];
            let col: usize = segment
                .chars()
                .map(|c| c.width().unwrap_or(0).max(1))
                .sum();
            (lines.len() - 1, col)
        } else {
            (0, 0)
        }
    }

    /// Given a visual line and a target display column, find the byte offset
    /// within that line that corresponds to that column (clamped to line end).
    fn byte_at_visual_col(&self, vl: &VisualLine, target_col: usize, _width: usize) -> usize {
        let segment = &self.text[vl.byte_start..vl.byte_end];
        let mut col = 0usize;
        for (i, ch) in segment.char_indices() {
            let char_w = ch.width().unwrap_or(0).max(1);
            if col + char_w > target_col {
                return vl.byte_start + i;
            }
            col += char_w;
        }
        // Target column exceeds line length — clamp to end.
        vl.byte_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let ti = TextInput::new();
        assert!(ti.is_empty());
        assert_eq!(ti.text(), "");
        assert_eq!(ti.cursor(), 0);
    }

    #[test]
    fn test_insert_char() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        assert_eq!(ti.text(), "abc");
        assert_eq!(ti.cursor(), 3);
    }

    #[test]
    fn test_insert_at_middle() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        ti.move_left();
        ti.insert_char('X');
        assert_eq!(ti.text(), "abXc");
        assert_eq!(ti.cursor(), 3);
    }

    #[test]
    fn test_backspace_at_end() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        ti.backspace();
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor(), 2);
    }

    #[test]
    fn test_backspace_at_middle() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        ti.move_left(); // cursor at 2 (before 'c')
        ti.backspace(); // remove 'b'
        assert_eq!(ti.text(), "ac");
        assert_eq!(ti.cursor(), 1);
    }

    #[test]
    fn test_backspace_at_start() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.cursor = 0;
        ti.backspace();
        assert_eq!(ti.text(), "a");
        assert_eq!(ti.cursor(), 0);
    }

    #[test]
    fn test_delete_at_cursor() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        ti.cursor = 0;
        ti.delete();
        assert_eq!(ti.text(), "bc");
        assert_eq!(ti.cursor(), 0);
    }

    #[test]
    fn test_delete_at_end() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.delete(); // no-op at end
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor(), 2);
    }

    #[test]
    fn test_insert_newline() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_newline();
        assert_eq!(ti.text(), "ab\n");
        assert_eq!(ti.cursor(), 3);
    }

    #[test]
    fn test_move_left_right() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.insert_char('b');
        ti.insert_char('c');
        assert_eq!(ti.cursor(), 3);
        ti.move_left();
        assert_eq!(ti.cursor(), 2);
        ti.move_left();
        assert_eq!(ti.cursor(), 1);
        ti.move_right();
        assert_eq!(ti.cursor(), 2);
    }

    #[test]
    fn test_move_left_at_start() {
        let mut ti = TextInput::new();
        ti.move_left();
        assert_eq!(ti.cursor(), 0);
    }

    #[test]
    fn test_move_right_at_end() {
        let mut ti = TextInput::new();
        ti.insert_char('a');
        ti.move_right();
        assert_eq!(ti.cursor(), 1); // stays at end
    }

    #[test]
    fn test_move_home_end() {
        let mut ti = TextInput::new();
        for c in "hello".chars() {
            ti.insert_char(c);
        }
        ti.move_home();
        assert_eq!(ti.cursor(), 0);
        ti.move_end();
        assert_eq!(ti.cursor(), 5);
    }

    #[test]
    fn test_move_home_end_multiline() {
        let mut ti = TextInput::new();
        // "ab\ncd" with cursor after 'd'
        for c in "ab\ncd".chars() {
            ti.insert_char(c);
        }
        assert_eq!(ti.cursor(), 5);
        ti.move_home(); // start of "cd" line
        assert_eq!(ti.cursor(), 3);
        ti.move_end(); // end of "cd" line
        assert_eq!(ti.cursor(), 5);

        // Move to middle of first line
        ti.cursor = 1; // after 'a'
        ti.move_home();
        assert_eq!(ti.cursor(), 0);
        ti.move_end();
        assert_eq!(ti.cursor(), 2); // before '\n'
    }

    #[test]
    fn test_visual_lines_no_wrap() {
        let mut ti = TextInput::new();
        for c in "hello".chars() {
            ti.insert_char(c);
        }
        let lines = ti.visual_lines(10);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 5 });
    }

    #[test]
    fn test_visual_lines_wrap() {
        let mut ti = TextInput::new();
        for c in "abcdefgh".chars() {
            ti.insert_char(c);
        }
        let lines = ti.visual_lines(5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 5 });
        assert_eq!(lines[1], VisualLine { byte_start: 5, byte_end: 8 });
    }

    #[test]
    fn test_visual_lines_newline() {
        let mut ti = TextInput::new();
        for c in "ab\ncd".chars() {
            ti.insert_char(c);
        }
        let lines = ti.visual_lines(10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 2 });
        assert_eq!(lines[1], VisualLine { byte_start: 3, byte_end: 5 });
    }

    #[test]
    fn test_visual_lines_newline_and_wrap() {
        let mut ti = TextInput::new();
        for c in "abcdefgh\nij".chars() {
            ti.insert_char(c);
        }
        let lines = ti.visual_lines(5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 5 });
        assert_eq!(lines[1], VisualLine { byte_start: 5, byte_end: 8 });
        assert_eq!(lines[2], VisualLine { byte_start: 9, byte_end: 11 });
    }

    #[test]
    fn test_visual_lines_empty() {
        let ti = TextInput::new();
        let lines = ti.visual_lines(10);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 0 });
    }

    #[test]
    fn test_visual_lines_trailing_newline() {
        let mut ti = TextInput::new();
        for c in "ab\n".chars() {
            ti.insert_char(c);
        }
        let lines = ti.visual_lines(10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], VisualLine { byte_start: 0, byte_end: 2 });
        assert_eq!(lines[1], VisualLine { byte_start: 3, byte_end: 3 });
    }

    #[test]
    fn test_cursor_pos_simple() {
        let mut ti = TextInput::new();
        for c in "abc".chars() {
            ti.insert_char(c);
        }
        ti.cursor = 1;
        let (row, col) = ti.cursor_visual_position(10);
        assert_eq!(row, 0);
        assert_eq!(col, 1);
    }

    #[test]
    fn test_cursor_pos_wrapped() {
        let mut ti = TextInput::new();
        for c in "abcdefgh".chars() {
            ti.insert_char(c);
        }
        ti.cursor = 6; // 'g' on second visual line
        let (row, col) = ti.cursor_visual_position(5);
        assert_eq!(row, 1);
        assert_eq!(col, 1);
    }

    #[test]
    fn test_cursor_pos_after_newline() {
        let mut ti = TextInput::new();
        for c in "ab\ncd".chars() {
            ti.insert_char(c);
        }
        ti.cursor = 4; // 'd'
        let (row, col) = ti.cursor_visual_position(10);
        assert_eq!(row, 1);
        assert_eq!(col, 1);
    }

    #[test]
    fn test_move_up_visual_lines() {
        let mut ti = TextInput::new();
        for c in "abcdefgh".chars() {
            ti.insert_char(c);
        }
        // width=5: "abcde" | "fgh"
        ti.cursor = 6; // 'g', visual (1, 1)
        ti.move_up(5);
        assert_eq!(ti.cursor(), 1); // 'b', visual (0, 1)
    }

    #[test]
    fn test_move_down_visual_lines() {
        let mut ti = TextInput::new();
        for c in "abcdefgh".chars() {
            ti.insert_char(c);
        }
        // width=5: "abcde" | "fgh"
        ti.cursor = 1; // 'b', visual (0, 1)
        ti.move_down(5);
        assert_eq!(ti.cursor(), 6); // 'g', visual (1, 1)
    }

    #[test]
    fn test_move_up_at_top() {
        let mut ti = TextInput::new();
        for c in "abc".chars() {
            ti.insert_char(c);
        }
        ti.cursor = 1;
        ti.move_up(10); // already on first line
        assert_eq!(ti.cursor(), 1); // unchanged
    }

    #[test]
    fn test_move_down_at_bottom() {
        let mut ti = TextInput::new();
        for c in "abc".chars() {
            ti.insert_char(c);
        }
        ti.cursor = 1;
        ti.move_down(10); // already on last line
        assert_eq!(ti.cursor(), 1); // unchanged
    }

    #[test]
    fn test_move_up_clamps_column() {
        let mut ti = TextInput::new();
        for c in "abcde\nfg".chars() {
            ti.insert_char(c);
        }
        // width=10: "abcde" | "fg"
        ti.cursor = 4; // 'e', col 4 on line 0
        ti.move_down(10);
        // Target col 4 on line "fg" (len 2) → clamped to end = byte 8
        assert_eq!(ti.cursor(), 8);
    }

    #[test]
    fn test_clear_returns_text() {
        let mut ti = TextInput::new();
        for c in "hello".chars() {
            ti.insert_char(c);
        }
        let text = ti.clear();
        assert_eq!(text, "hello");
        assert!(ti.is_empty());
        assert_eq!(ti.cursor(), 0);
    }

    #[test]
    fn test_set_text() {
        let mut ti = TextInput::new();
        ti.set_text("new text".to_string());
        assert_eq!(ti.text(), "new text");
        assert_eq!(ti.cursor(), 8); // at end
    }

    #[test]
    fn test_multibyte_char() {
        let mut ti = TextInput::new();
        ti.insert_char('é'); // 2-byte UTF-8
        ti.insert_char('x');
        assert_eq!(ti.text(), "éx");
        assert_eq!(ti.cursor(), 3); // 'é'=2 bytes + 'x'=1 byte
        ti.move_left();
        assert_eq!(ti.cursor(), 2); // before 'x'
        ti.move_left();
        assert_eq!(ti.cursor(), 0); // before 'é'
        ti.move_right();
        assert_eq!(ti.cursor(), 2); // after 'é'
    }

    #[test]
    fn test_visual_line_count() {
        let mut ti = TextInput::new();
        for c in "abcdefghij".chars() {
            ti.insert_char(c);
        }
        assert_eq!(ti.visual_line_count(5), 2); // "abcde" | "fghij"
        assert_eq!(ti.visual_line_count(10), 1);
        assert_eq!(ti.visual_line_count(3), 4); // "abc" | "def" | "ghi" | "j"
    }
}

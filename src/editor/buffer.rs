/// Text buffer for the FRAM editor.
///
/// Maintains an in-memory copy of the FRAM contents as a text buffer
/// with cursor position, viewport scroll, and dirty tracking.

use core::cmp::Ord;
use core::iter::Iterator;
use core::result::Result::{self, Ok};

use crate::fram;

/// The editing buffer — same size as FRAM.
pub struct TextBuffer {
    /// Raw byte contents (loaded from FRAM, written back on save).
    pub data: [u8; fram::SIZE],
    /// Number of valid bytes in the buffer (actual content length).
    pub len: usize,
    /// Cursor line (0-indexed).
    pub cursor_row: usize,
    /// Cursor column within the current line (0-indexed).
    pub cursor_col: usize,
    /// First visible line in the viewport.
    pub scroll_row: usize,
    /// Whether the buffer has been modified since last save.
    pub dirty: bool,
    /// Status message to display (cleared after one render cycle).
    pub status_msg: [u8; 64],
    pub status_msg_len: usize,
}

impl TextBuffer {
    pub const fn new() -> Self {
        Self {
            data: [0; fram::SIZE],
            len: 0,
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            dirty: false,
            status_msg: [0; 64],
            status_msg_len: 0,
        }
    }

    /// Load buffer from FRAM. Validates content before accepting it.
    pub fn load_from_fram(&mut self) -> Result<(), crate::i2c::I2cError> {
        fram::read_all(&mut self.data)?;

        // Determine actual content length: find last non-0xFF / non-0x00 meaningful byte.
        // Treat 0xFF (erased FRAM) as empty.
        self.len = 0;
        for i in (0..fram::SIZE).rev() {
            if self.data[i] != 0xFF && self.data[i] != 0x00 {
                self.len = i + 1;
                break;
            }
        }

        // Validate: if the content contains non-text bytes, treat FRAM as empty.
        // Valid bytes: printable ASCII (0x20..0x7E), tab (0x09), newline (0x0A), CR (0x0D).
        let mut valid = true;
        for i in 0..self.len {
            let b = self.data[i];
            if !((b >= 0x20 && b <= 0x7E) || b == 0x09 || b == 0x0A || b == 0x0D) {
                valid = false;
                break;
            }
        }

        if !valid {
            // FRAM contains non-text data — start with empty buffer
            self.len = 0;
            for i in 0..fram::SIZE {
                self.data[i] = 0;
            }
        }

        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll_row = 0;
        self.dirty = false;
        Ok(())
    }

    /// Save buffer to FRAM.
    pub fn save_to_fram(&mut self) -> Result<(), crate::i2c::I2cError> {
        // Zero-fill the remainder of the buffer
        for i in self.len..fram::SIZE {
            self.data[i] = 0x00;
        }
        fram::write_all(&self.data)?;
        self.dirty = false;
        self.set_status(b"Saved to FRAM");
        Ok(())
    }

    /// Set a status message to display on the next render.
    pub fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status_msg.len());
        self.status_msg[..len].copy_from_slice(&msg[..len]);
        self.status_msg_len = len;
    }

    /// Get the number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        if self.len == 0 {
            return 1; // Always at least one line
        }
        let mut count = 1;
        for i in 0..self.len {
            if self.data[i] == b'\n' {
                count += 1;
            }
        }
        count
    }

    /// Get the byte offset of the start of the given line (0-indexed).
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut current_line = 0;
        for i in 0..self.len {
            if self.data[i] == b'\n' {
                current_line += 1;
                if current_line == line {
                    return i + 1;
                }
            }
        }
        self.len
    }

    /// Get the length of the given line (not including newline).
    pub fn line_len(&self, line: usize) -> usize {
        let start = self.line_start(line);
        if start >= self.len {
            return 0;
        }
        let mut end = start;
        while end < self.len && self.data[end] != b'\n' {
            end += 1;
        }
        end - start
    }

    /// Get a slice of the given line's content (not including newline).
    pub fn line_content(&self, line: usize) -> &[u8] {
        let start = self.line_start(line);
        if start >= self.len {
            return &[];
        }
        let len = self.line_len(line);
        let end = (start + len).min(self.len);
        &self.data[start..end]
    }

    /// Get the absolute byte offset of the cursor position.
    fn cursor_offset(&self) -> usize {
        let start = self.line_start(self.cursor_row);
        let col = self.cursor_col.min(self.line_len(self.cursor_row));
        start + col
    }

    /// Insert a byte at the cursor position.
    pub fn insert_byte(&mut self, b: u8) {
        if self.len >= fram::SIZE - 1 {
            self.set_status(b"Buffer full!");
            return;
        }
        let pos = self.cursor_offset();

        // Shift data right
        if pos < self.len {
            self.data.copy_within(pos..self.len, pos + 1);
        }
        self.data[pos] = b;
        self.len += 1;
        self.dirty = true;

        if b == b'\n' {
            self.cursor_row += 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col += 1;
        }
    }

    /// Delete the byte before the cursor (backspace).
    /// Returns `true` if a newline was removed (lines below shifted).
    pub fn backspace(&mut self) -> bool {
        let pos = self.cursor_offset();
        if pos == 0 {
            return false;
        }

        let deleted = self.data[pos - 1];

        // Shift data left
        if pos < self.len {
            self.data.copy_within(pos..self.len, pos - 1);
        }
        self.len -= 1;
        self.dirty = true;

        if deleted == b'\n' {
            // Move cursor to end of previous line
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.line_len(self.cursor_row);
            }
            true
        } else {
            if self.cursor_col > 0 {
                self.cursor_col -= 1;
            }
            false
        }
    }

    /// Delete the byte at the cursor position (delete key).
    /// Returns `true` if a newline was removed (lines below shifted).
    pub fn delete_at_cursor(&mut self) -> bool {
        let pos = self.cursor_offset();
        if pos >= self.len {
            return false;
        }

        let was_newline = self.data[pos] == b'\n';

        // Shift data left
        if pos + 1 < self.len {
            self.data.copy_within(pos + 1..self.len, pos);
        }
        self.len -= 1;
        self.dirty = true;
        was_newline
    }

    /// Move cursor up.
    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let ll = self.line_len(self.cursor_row);
            if self.cursor_col > ll {
                self.cursor_col = ll;
            }
        }
    }

    /// Move cursor down.
    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.line_count() {
            self.cursor_row += 1;
            let ll = self.line_len(self.cursor_row);
            if self.cursor_col > ll {
                self.cursor_col = ll;
            }
        }
    }

    /// Move cursor left.
    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_len(self.cursor_row);
        }
    }

    /// Move cursor right.
    pub fn move_right(&mut self) {
        let ll = self.line_len(self.cursor_row);
        if self.cursor_col < ll {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.line_count() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// Move cursor to the beginning of the line.
    pub fn home(&mut self) {
        self.cursor_col = 0;
    }

    /// Move cursor to the end of the line.
    pub fn end(&mut self) {
        self.cursor_col = self.line_len(self.cursor_row);
    }

    /// Move viewport up by `n` lines (page up).
    pub fn page_up(&mut self, visible_lines: usize) {
        if self.scroll_row > visible_lines {
            self.scroll_row -= visible_lines;
            self.cursor_row -= self.cursor_row.min(visible_lines);
        } else {
            self.scroll_row = 0;
            self.cursor_row = 0;
        }
        let ll = self.line_len(self.cursor_row);
        if self.cursor_col > ll {
            self.cursor_col = ll;
        }
    }

    /// Move viewport down by `n` lines (page down).
    pub fn page_down(&mut self, visible_lines: usize) {
        let total = self.line_count();
        self.scroll_row = (self.scroll_row + visible_lines).min(total.saturating_sub(1));
        self.cursor_row = (self.cursor_row + visible_lines).min(total.saturating_sub(1));
        let ll = self.line_len(self.cursor_row);
        if self.cursor_col > ll {
            self.cursor_col = ll;
        }
    }

    /// Ensure the cursor is visible within the viewport.
    pub fn ensure_visible(&mut self, visible_lines: usize) {
        if self.cursor_row < self.scroll_row {
            self.scroll_row = self.cursor_row;
        }
        if self.cursor_row >= self.scroll_row + visible_lines {
            self.scroll_row = self.cursor_row - visible_lines + 1;
        }
    }
}

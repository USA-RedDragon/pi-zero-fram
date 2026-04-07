/// Screen renderer for the Nano-like text editor.
///
/// Layout:
///   Row 1:           Title/status bar (inverse video)
///   Rows 2..rows-2:  Text content area
///   Row rows-1:      Status info line (inverse video)
///   Row rows:        Help bar (inverse video)

use core::cmp::Ord;

use crate::editor::buffer::TextBuffer;
use crate::editor::terminal::{TermWrite, Terminal};

const SPACES: [u8; 256] = [b' '; 256];

fn write_spaces<W: TermWrite>(w: &mut W, count: usize) {
    let mut remaining = count;
    while remaining > 0 {
        let chunk = remaining.min(SPACES.len());
        w.write_bytes(&SPACES[..chunk]);
        remaining -= chunk;
    }
}

pub struct Renderer {
    /// Track which lines need redrawing (true = dirty).
    /// Index 0 = first text line, etc.
    dirty_lines: [bool; 256],
    /// Force full redraw on next render call.
    force_full: bool,
    /// Previous title bar state for change detection.
    prev_buf_len: usize,
    prev_dirty: bool,
    /// Previous status bar state for change detection.
    prev_cursor_row: usize,
    prev_cursor_col: usize,
    prev_line_count: usize,
    prev_status_msg_len: usize,
}

impl Renderer {
    pub const fn new() -> Self {
        Self {
            dirty_lines: [true; 256],
            force_full: true,
            prev_buf_len: usize::MAX,
            prev_dirty: false,
            prev_cursor_row: usize::MAX,
            prev_cursor_col: usize::MAX,
            prev_line_count: usize::MAX,
            prev_status_msg_len: usize::MAX,
        }
    }

    /// Mark all lines as dirty (full redraw).
    pub fn invalidate_all(&mut self) {
        self.force_full = true;
    }

    /// Mark a single visible line as dirty (index relative to scroll_row).
    pub fn mark_line_dirty(&mut self, visible_idx: usize) {
        if visible_idx < self.dirty_lines.len() {
            self.dirty_lines[visible_idx] = true;
        }
    }

    /// Number of visible text lines (between title bar and bottom bars).
    pub fn visible_lines(term: &Terminal) -> usize {
        if term.rows > 3 { term.rows - 3 } else { 1 }
    }

    /// Render the editor screen.
    pub fn render<W: TermWrite>(
        &mut self,
        w: &mut W,
        term: &Terminal,
        buf: &TextBuffer,
    ) {
        let visible = Self::visible_lines(term);

        // Check what actually changed for incremental rendering
        let title_changed = buf.len != self.prev_buf_len || buf.dirty != self.prev_dirty;
        let line_count = buf.line_count();
        let status_changed = buf.cursor_row != self.prev_cursor_row
            || buf.cursor_col != self.prev_cursor_col
            || line_count != self.prev_line_count
            || buf.status_msg_len != self.prev_status_msg_len;

        if self.force_full {
            term.hide_cursor(w);
            term.clear_screen(w);
            self.render_title_bar(w, term, buf);
            for i in 0..visible {
                self.render_text_line(w, term, buf, i);
            }
            self.render_status_bar(w, term, buf);
            self.render_help_bar(w, term);
            self.force_full = false;
            for d in self.dirty_lines.iter_mut() {
                *d = false;
            }
        } else {
            // Check if any text lines are dirty
            let any_dirty = (0..visible).any(|i| {
                i < self.dirty_lines.len() && self.dirty_lines[i]
            });

            // Fast path: cursor-only move, nothing visual changed except cursor position
            if !any_dirty && !title_changed && !status_changed {
                // Just reposition cursor — no hide/show dance needed for most terminals,
                // but we do it for safety
                let screen_row = buf.cursor_row - buf.scroll_row + 2;
                let screen_col = buf.cursor_col + 1;
                term.goto(w, screen_row, screen_col);
                self.update_prev_state(buf, line_count);
                return;
            }

            term.hide_cursor(w);

            if title_changed {
                self.render_title_bar(w, term, buf);
            }
            for i in 0..visible {
                if i < self.dirty_lines.len() && self.dirty_lines[i] {
                    self.render_text_line(w, term, buf, i);
                    self.dirty_lines[i] = false;
                }
            }
            if status_changed {
                self.render_status_bar(w, term, buf);
            }
        }

        // Position cursor at the correct location
        let screen_row = buf.cursor_row - buf.scroll_row + 2; // +2 for 1-indexed + title bar
        let screen_col = buf.cursor_col + 1; // 1-indexed
        term.goto(w, screen_row, screen_col);
        term.show_cursor(w);

        self.update_prev_state(buf, line_count);
    }

    fn update_prev_state(&mut self, buf: &TextBuffer, line_count: usize) {
        self.prev_buf_len = buf.len;
        self.prev_dirty = buf.dirty;
        self.prev_cursor_row = buf.cursor_row;
        self.prev_cursor_col = buf.cursor_col;
        self.prev_line_count = line_count;
        self.prev_status_msg_len = buf.status_msg_len;
    }

    fn render_title_bar<W: TermWrite>(&self, w: &mut W, term: &Terminal, buf: &TextBuffer) {
        term.goto(w, 1, 1);
        term.inverse_on(w);

        let title = b" FRAM Editor";
        w.write_bytes(title);

        let modified = if buf.dirty { b" [Modified]" as &[u8] } else { b"" };
        w.write_bytes(modified);

        let bytes_label = b" - ";
        w.write_bytes(bytes_label);
        write_usize_bytes(w, buf.len);
        w.write_bytes(b"/");
        write_usize_bytes(w, crate::fram::SIZE);
        w.write_bytes(b" bytes");

        // Pad to fill the row (exact count)
        let used = title.len() + modified.len() + bytes_label.len()
            + usize_digit_count(buf.len) + 1 + usize_digit_count(crate::fram::SIZE) + 6;
        if term.cols > used {
            write_spaces(w, term.cols - used);
        }

        term.attr_reset(w);
    }

    fn render_text_line<W: TermWrite>(
        &self,
        w: &mut W,
        term: &Terminal,
        buf: &TextBuffer,
        visible_idx: usize,
    ) {
        let line_num = buf.scroll_row + visible_idx;
        let screen_row = visible_idx + 2; // 1-indexed, after title bar

        term.goto(w, screen_row, 1);
        term.clear_eol(w);

        if line_num < buf.line_count() {
            let content = buf.line_content(line_num);
            let max_cols = term.cols;
            let display_len = content.len().min(max_cols);

            // Write visible portion of the line
            if display_len > 0 {
                w.write_bytes(&content[..display_len]);
            }
        } else {
            // Past end of file — show tilde like vim
            w.write_bytes(b"~");
        }
    }

    fn render_status_bar<W: TermWrite>(&self, w: &mut W, term: &Terminal, buf: &TextBuffer) {
        let status_row = if term.rows > 2 { term.rows - 1 } else { term.rows };
        term.goto(w, status_row, 1);
        term.inverse_on(w);

        // Left side: cursor position
        w.write_bytes(b" Ln ");
        write_usize_bytes(w, buf.cursor_row + 1);
        w.write_bytes(b", Col ");
        write_usize_bytes(w, buf.cursor_col + 1);

        w.write_bytes(b"  |  ");
        write_usize_bytes(w, buf.line_count());
        w.write_bytes(b" lines");

        // Status message if any
        let mut used = 4 + usize_digit_count(buf.cursor_row + 1)
            + 6 + usize_digit_count(buf.cursor_col + 1)
            + 5 + usize_digit_count(buf.line_count()) + 6;
        if buf.status_msg_len > 0 {
            w.write_bytes(b"  |  ");
            w.write_bytes(&buf.status_msg[..buf.status_msg_len]);
            used += 5 + buf.status_msg_len;
        }

        // Pad to fill the row (exact count)
        if term.cols > used {
            write_spaces(w, term.cols - used);
        }

        term.attr_reset(w);
    }

    fn render_help_bar<W: TermWrite>(&self, w: &mut W, term: &Terminal) {
        term.goto(w, term.rows, 1);
        term.inverse_on(w);

        let help = b" ^O Save  ^R Reload  ^L Redraw  Arrows/Home/End Navigate";
        let len = help.len().min(term.cols);
        w.write_bytes(&help[..len]);

        // Pad to fill
        if term.cols > len {
            write_spaces(w, term.cols - len);
        }

        term.attr_reset(w);
    }
}

fn usize_digit_count(val: usize) -> usize {
    if val == 0 { return 1; }
    let mut n = val;
    let mut count = 0;
    while n > 0 {
        count += 1;
        n /= 10;
    }
    count
}

fn write_usize_bytes<W: TermWrite>(w: &mut W, mut val: usize) {
    if val == 0 {
        w.write_bytes(b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut len = 0;
    while val > 0 {
        buf[len] = b'0' + (val % 10) as u8;
        val /= 10;
        len += 1;
    }
    // Reverse
    buf[..len].reverse();
    w.write_bytes(&buf[..len]);
}

/// VT100/ANSI terminal abstraction.
///
/// Provides cursor control, screen clearing, colors, and terminal size
/// detection over a byte-oriented write interface.

use core::option::Option::{self, Some, None};

/// Output trait — the terminal writes its escape sequences here.
/// Implemented by the USB serial wrapper.
pub trait TermWrite {
    fn write_bytes(&mut self, data: &[u8]);
    fn read_bytes(&mut self, buf: &mut [u8]) -> usize;
}

/// Buffered writer that batches small escape-sequence writes into large USB
/// transfers, eliminating per-call USB poll + serial-drain overhead.
pub struct BufWriter<'a, W: TermWrite> {
    inner: &'a mut W,
    buf: [u8; 4096],
    pos: usize,
}

impl<'a, W: TermWrite> BufWriter<'a, W> {
    pub fn new(inner: &'a mut W) -> Self {
        Self { inner, buf: [0; 4096], pos: 0 }
    }

    /// Flush the buffered data to the underlying writer.
    pub fn flush(&mut self) {
        if self.pos > 0 {
            self.inner.write_bytes(&self.buf[..self.pos]);
            self.pos = 0;
        }
    }
}

impl<W: TermWrite> TermWrite for BufWriter<'_, W> {
    fn write_bytes(&mut self, data: &[u8]) {
        if self.pos + data.len() <= self.buf.len() {
            self.buf[self.pos..self.pos + data.len()].copy_from_slice(data);
            self.pos += data.len();
        } else {
            // Flush existing buffer, then either buffer or write directly
            self.flush();
            if data.len() > self.buf.len() {
                self.inner.write_bytes(data);
            } else {
                self.buf[..data.len()].copy_from_slice(data);
                self.pos = data.len();
            }
        }
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> usize {
        self.flush();
        self.inner.read_bytes(buf)
    }
}

pub struct Terminal {
    /// Detected terminal width.
    pub cols: usize,
    /// Detected terminal height.
    pub rows: usize,
    /// Whether size has been successfully detected.
    pub size_detected: bool,
}

impl Terminal {
    pub const fn new() -> Self {
        Self {
            cols: 80,
            rows: 24,
            size_detected: false,
        }
    }

    /// Wait for the terminal to become responsive using DSR (Device Status Report).
    ///
    /// Sends ESC[5n repeatedly; a live terminal responds with ESC[0n.
    /// Returns true once the terminal proves it is actively reading.
    fn wait_for_terminal<W: TermWrite>(&self, w: &mut W) -> bool {
        // Drain stale data once
        let mut drain = [0u8; 64];
        for _ in 0..10 {
            if w.read_bytes(&mut drain) == 0 { break; }
        }

        let mut resp = [0u8; 32];
        let mut resp_len = 0;
        let mut last_ping_us = 0u64;
        let deadline = crate::timer::get_time_us() + 10_000_000; // 10s overall

        loop {
            let now = crate::timer::get_time_us();

            // Send a DSR ping every 200ms
            if now.wrapping_sub(last_ping_us) >= 200_000 || last_ping_us == 0 {
                w.write_bytes(b"\x1b[5n");
                last_ping_us = now;
            }

            // Continuously read — don't drain between pings
            let n = w.read_bytes(&mut resp[resp_len..]);
            resp_len += n;

            if resp_len > 0 && resp[..resp_len].contains(&b'n') {
                return true;
            }

            // Reset buffer if full (discard garbage, keep trying)
            if resp_len >= resp.len() {
                resp_len = 0;
            }

            if now > deadline {
                break;
            }
            crate::timer::delay_us(100);
        }
        false
    }

    /// Detect terminal size using DSR handshake followed by CPR query.
    ///
    /// First verifies the terminal is alive (DSR ping), then queries size.
    pub fn detect_size<W: TermWrite>(&mut self, w: &mut W) {
        // Phase 1: DSR handshake — wait for terminal to be responsive
        if !self.wait_for_terminal(w) {
            return;
        }

        // Phase 2: CPR query — terminal is proven active
        // Drain any leftover DSR response bytes
        let mut drain = [0u8; 64];
        for _ in 0..10 {
            if w.read_bytes(&mut drain) == 0 { break; }
        }

        for _attempt in 0..3u32 {
            // Save cursor, move to far corner, query position
            w.write_bytes(b"\x1b[s\x1b[999;999H\x1b[6n");

            // Read response — 500ms timeout (terminal is already proven alive)
            let mut resp = [0u8; 32];
            let mut resp_len = 0;
            let deadline = crate::timer::get_time_us() + 500_000;

            loop {
                let n = w.read_bytes(&mut resp[resp_len..]);
                resp_len += n;

                if resp_len > 0 && resp[..resp_len].contains(&b'R') {
                    break;
                }
                if crate::timer::get_time_us() > deadline {
                    break;
                }
                crate::timer::delay_us(100);
            }

            // Restore cursor position
            w.write_bytes(b"\x1b[u");

            if let Some(parsed) = self.parse_cpr(&resp[..resp_len]) {
                self.rows = parsed.0;
                self.cols = parsed.1;
                self.size_detected = true;
                return;
            }
        }
    }

    /// Parse a CPR (Cursor Position Report) response: ESC[rows;colsR
    fn parse_cpr(&self, data: &[u8]) -> Option<(usize, usize)> {
        // Find ESC[
        let mut i = 0;
        while i + 1 < data.len() {
            if data[i] == 0x1b && data[i + 1] == b'[' {
                i += 2;
                break;
            }
            i += 1;
        }

        // Parse rows number
        let mut rows: usize = 0;
        while i < data.len() && data[i].is_ascii_digit() && rows < 10000 {
            rows = rows * 10 + (data[i] - b'0') as usize;
            i += 1;
        }

        // Expect semicolon
        if i >= data.len() || data[i] != b';' {
            return None;
        }
        i += 1;

        // Parse cols number
        let mut cols: usize = 0;
        while i < data.len() && data[i].is_ascii_digit() && cols < 10000 {
            cols = cols * 10 + (data[i] - b'0') as usize;
            i += 1;
        }

        // Expect 'R'
        if i >= data.len() || data[i] != b'R' {
            return None;
        }

        if rows > 0 && cols > 0 {
            Some((rows, cols))
        } else {
            None
        }
    }

    /// Move cursor to (row, col) — both 1-indexed.
    pub fn goto<W: TermWrite>(&self, w: &mut W, row: usize, col: usize) {
        let mut buf = [0u8; 16];
        let len = fmt_csi_2(&mut buf, row, col, b'H');
        w.write_bytes(&buf[..len]);
    }

    /// Clear the entire screen.
    pub fn clear_screen<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[2J");
    }

    /// Clear from cursor to end of line.
    pub fn clear_eol<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[K");
    }

    /// Enable inverse video (for status bars).
    pub fn inverse_on<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[7m");
    }

    /// Reset text attributes.
    pub fn attr_reset<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[0m");
    }

    /// Hide the cursor.
    pub fn hide_cursor<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[?25l");
    }

    /// Show the cursor.
    pub fn show_cursor<W: TermWrite>(&self, w: &mut W) {
        w.write_bytes(b"\x1b[?25h");
    }
}

/// Format a CSI sequence with two numeric parameters: ESC[n;mX
fn fmt_csi_2(buf: &mut [u8; 16], n1: usize, n2: usize, suffix: u8) -> usize {
    let mut i = 0;
    buf[i] = 0x1b;
    i += 1;
    buf[i] = b'[';
    i += 1;
    i += write_usize(&mut buf[i..], n1);
    buf[i] = b';';
    i += 1;
    i += write_usize(&mut buf[i..], n2);
    buf[i] = suffix;
    i += 1;
    i
}

/// Write a usize as decimal digits into a buffer. Returns number of bytes written.
fn write_usize(buf: &mut [u8], mut val: usize) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut len = 0;
    while val > 0 {
        tmp[len] = b'0' + (val % 10) as u8;
        val /= 10;
        len += 1;
    }
    for j in 0..len {
        buf[j] = tmp[len - 1 - j];
    }
    len
}

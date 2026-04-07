/// Keyboard input handler — parses raw bytes into editor actions.
///
/// Handles:
/// - Printable ASCII characters
/// - Control keys (Ctrl+O, Ctrl+R, Ctrl+L)
/// - VT100 escape sequences (arrow keys, Home, End, PgUp, PgDn, Delete)

use core::clone::Clone;
use core::cmp::PartialEq;
use core::marker::Copy;
use core::option::Option::{self, Some, None};

/// Actions the editor can perform in response to input.
#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    /// Insert a printable character.
    Char(u8),
    /// Arrow key / cursor movement.
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    /// Home / End.
    Home,
    End,
    /// Page Up / Page Down.
    PageUp,
    PageDown,
    /// Delete key (delete char at cursor).
    Delete,
    /// Backspace (delete char before cursor).
    Backspace,
    /// Enter (insert newline).
    Enter,
    /// Ctrl+O: Save to FRAM.
    Save,
    /// Ctrl+R: Reload from FRAM (discard changes).
    Reload,
    /// Ctrl+L: Refresh / redraw.
    Refresh,
}

/// State machine for parsing escape sequences.
enum State {
    Normal,
    Escape,     // Received ESC (0x1b)
    CsiDigit1,  // Received ESC[ and optionally collecting first number
    CsiDigit2,  // After semicolon, collecting second number
}

pub struct InputParser {
    state: State,
    num1: u32,
    num2: u32,
}

impl InputParser {
    pub const fn new() -> Self {
        Self {
            state: State::Normal,
            num1: 0,
            num2: 0,
        }
    }

    /// Feed a single byte into the parser. Returns an action if one is complete.
    pub fn feed(&mut self, byte: u8) -> Option<Action> {
        match self.state {
            State::Normal => self.handle_normal(byte),
            State::Escape => self.handle_escape(byte),
            State::CsiDigit1 => self.handle_csi_digit1(byte),
            State::CsiDigit2 => self.handle_csi_digit2(byte),
        }
    }

    fn handle_normal(&mut self, byte: u8) -> Option<Action> {
        match byte {
            0x1b => {
                self.state = State::Escape;
                None
            }
            0x0C => Some(Action::Refresh),     // Ctrl+L
            0x0F => Some(Action::Save),        // Ctrl+O
            0x12 => Some(Action::Reload),      // Ctrl+R
            0x08 | 0x7f => Some(Action::Backspace), // Backspace
            0x0D | 0x0A => Some(Action::Enter),     // Enter (CR or LF)
            0x09 => {
                // Tab — insert a space (single space; multi-char insert not supported)
                Some(Action::Char(b' '))
            }
            b if b >= 0x20 && b < 0x7f => Some(Action::Char(b)),
            _ => None,
        }
    }

    fn handle_escape(&mut self, byte: u8) -> Option<Action> {
        match byte {
            b'[' => {
                self.state = State::CsiDigit1;
                self.num1 = 0;
                self.num2 = 0;
                None
            }
            _ => {
                // Unknown escape sequence, reset
                self.state = State::Normal;
                None
            }
        }
    }

    fn handle_csi_digit1(&mut self, byte: u8) -> Option<Action> {
        match byte {
            b'0'..=b'9' => {
                self.num1 = self.num1 * 10 + (byte - b'0') as u32;
                None
            }
            b';' => {
                self.state = State::CsiDigit2;
                None
            }
            // Arrow keys: ESC[A/B/C/D
            b'A' => {
                self.state = State::Normal;
                Some(Action::ArrowUp)
            }
            b'B' => {
                self.state = State::Normal;
                Some(Action::ArrowDown)
            }
            b'C' => {
                self.state = State::Normal;
                Some(Action::ArrowRight)
            }
            b'D' => {
                self.state = State::Normal;
                Some(Action::ArrowLeft)
            }
            // Home / End
            b'H' => {
                self.state = State::Normal;
                Some(Action::Home)
            }
            b'F' => {
                self.state = State::Normal;
                Some(Action::End)
            }
            // Tilde-terminated sequences: ESC[n~
            b'~' => {
                self.state = State::Normal;
                match self.num1 {
                    1 => Some(Action::Home),     // ESC[1~
                    3 => Some(Action::Delete),   // ESC[3~
                    4 => Some(Action::End),      // ESC[4~
                    5 => Some(Action::PageUp),   // ESC[5~
                    6 => Some(Action::PageDown), // ESC[6~
                    7 => Some(Action::Home),     // ESC[7~
                    8 => Some(Action::End),      // ESC[8~
                    _ => None,
                }
            }
            _ => {
                self.state = State::Normal;
                None
            }
        }
    }

    fn handle_csi_digit2(&mut self, byte: u8) -> Option<Action> {
        match byte {
            b'0'..=b'9' => {
                self.num2 = self.num2 * 10 + (byte - b'0') as u32;
                None
            }
            b'R' => {
                // CPR response (used by terminal size detection) — ignore here
                self.state = State::Normal;
                None
            }
            b'~' => {
                self.state = State::Normal;
                None // Extended sequences we don't handle
            }
            _ => {
                self.state = State::Normal;
                None
            }
        }
    }
}

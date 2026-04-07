/// Mini UART driver for BCM2710 (AUX peripheral).
///
/// The Mini UART is part of the AUX peripheral block at 0x3F215000.
/// Used on GPIO 14 (TX) and GPIO 15 (RX) in ALT5 mode.
/// This is the primary debug output channel before USB is operational.

use crate::{gpio, mmio};

const AUX_BASE: usize = mmio::PERIPHERAL_BASE + 0x0021_5000;

const AUX_ENABLES: usize = AUX_BASE + 0x04;
const AUX_MU_IO: usize = AUX_BASE + 0x40;
const AUX_MU_IER: usize = AUX_BASE + 0x44;
const AUX_MU_IIR: usize = AUX_BASE + 0x48;
const AUX_MU_LCR: usize = AUX_BASE + 0x4C;
const AUX_MU_MCR: usize = AUX_BASE + 0x50;
const AUX_MU_LSR: usize = AUX_BASE + 0x54;
const AUX_MU_CNTL: usize = AUX_BASE + 0x60;
const AUX_MU_BAUD: usize = AUX_BASE + 0x68;

/// Initialize the Mini UART at 115200 baud.
///
/// Assumes core clock = 250 MHz (default for Pi Zero 2 W without core_freq override).
/// Baud rate = system_clock / (8 * (AUX_MU_BAUD + 1))
/// 115200 = 250000000 / (8 * (270 + 1)) ≈ 115313 (~0.1% error)
pub fn init() {
    // Enable Mini UART (AUX peripheral bit 0)
    mmio::write(AUX_ENABLES, mmio::read(AUX_ENABLES) | 1);

    // Disable TX/RX while configuring
    mmio::write(AUX_MU_CNTL, 0);
    // Disable interrupts
    mmio::write(AUX_MU_IER, 0);
    // 8-bit mode
    mmio::write(AUX_MU_LCR, 3);
    // RTS line high (unused)
    mmio::write(AUX_MU_MCR, 0);
    // Clear FIFOs
    mmio::write(AUX_MU_IIR, 0xC6);
    // Set baud rate: 270 → ~115200 baud at 250MHz
    mmio::write(AUX_MU_BAUD, 270);

    // Configure GPIO 14 (TX) and 15 (RX) to Alt5 (Mini UART)
    gpio::set_function(14, gpio::Function::Alt5);
    gpio::set_function(15, gpio::Function::Alt5);
    gpio::set_pull(14, gpio::Pull::Off);
    gpio::set_pull(15, gpio::Pull::Off);

    // Enable TX and RX
    mmio::write(AUX_MU_CNTL, 3);
}

/// Send a single byte over the Mini UART. Blocks until transmitter is ready.
pub fn write_byte(b: u8) {
    // Wait until transmitter FIFO can accept data (bit 5 of LSR = TX empty)
    while (mmio::read(AUX_MU_LSR) & 0x20) == 0 {
        core::hint::spin_loop();
    }
    mmio::write(AUX_MU_IO, b as u32);
}

/// Send a string over the Mini UART.
pub fn write_str(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(b);
    }
}

/// Write a u32 as decimal string.
pub fn write_u32(mut val: u32) {
    if val == 0 {
        write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        write_byte(buf[i]);
    }
}

/// Write a u32 as hexadecimal string (with 0x prefix).
pub fn write_hex(val: u32) {
    write_str("0x");
    for i in (0..8).rev() {
        let nibble = (val >> (i * 4)) & 0xF;
        let c = if nibble < 10 {
            b'0' + nibble as u8
        } else {
            b'A' + (nibble - 10) as u8
        };
        write_byte(c);
    }
}

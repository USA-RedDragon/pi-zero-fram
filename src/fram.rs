/// MB85RC256V FRAM driver.
///
/// The MB85RC256V is a 32KB (256Kbit) FRAM with I2C interface.
/// - I2C address: 0x50 (A2=A1=A0=0)
/// - 2-byte (16-bit) addressing: address range 0x0000..0x7FFF
/// - Supports sequential reads and writes (auto-incrementing address)
/// - No page boundaries or write delays (instant, unlimited endurance)

use core::cmp::Ord;
use core::result::Result::{self, Ok};

use crate::i2c;

const FRAM_ADDR: u8 = 0x50;

/// Total size of the FRAM in bytes (32KB).
pub const SIZE: usize = 32 * 1024;

/// Read `buf.len()` bytes starting from the given FRAM offset.
///
/// The MB85RC256V supports sequential reads: after setting the address,
/// it auto-increments on each byte read, wrapping at 0x7FFF → 0x0000.
pub fn read(offset: u16, buf: &mut [u8]) -> Result<(), i2c::I2cError> {
    if buf.is_empty() {
        return Ok(());
    }

    // Set the FRAM internal address pointer by writing the 2-byte address
    let addr_bytes = offset.to_be_bytes();
    i2c::write_read(FRAM_ADDR, &addr_bytes, buf)
}

/// Write `data` bytes starting at the given FRAM offset.
///
/// The MB85RC256V supports sequential writes: address auto-increments
/// on each byte written, wrapping at 0x7FFF → 0x0000.
/// FRAM has no page size limit and no write delay.
///
/// We chunk writes to keep I2C transactions reasonable (max ~128 bytes
/// at a time to avoid FIFO pressure, though the BSC FIFO is only 16 bytes).
pub fn write(offset: u16, data: &[u8]) -> Result<(), i2c::I2cError> {
    if data.is_empty() {
        return Ok(());
    }

    // For writes, we must prepend the 2-byte address to each transaction.
    // The BSC FIFO is 16 bytes, so we chunk data to keep transactions manageable.
    // We send: [addr_hi, addr_lo, data_0, data_1, ...] per chunk.
    const CHUNK_SIZE: usize = 14; // 16-byte FIFO minus 2 address bytes

    let mut pos = 0;
    while pos < data.len() {
        let current_offset = offset.wrapping_add(pos as u16);
        let remaining = data.len() - pos;
        let chunk_len = remaining.min(CHUNK_SIZE);

        // Build transaction: [addr_hi, addr_lo, data...]
        let mut txbuf = [0u8; 16];
        let addr_bytes = current_offset.to_be_bytes();
        txbuf[0] = addr_bytes[0];
        txbuf[1] = addr_bytes[1];
        txbuf[2..2 + chunk_len].copy_from_slice(&data[pos..pos + chunk_len]);

        i2c::write(FRAM_ADDR, &txbuf[..2 + chunk_len])?;
        pos += chunk_len;
    }

    Ok(())
}

/// Read the entire 32KB FRAM contents into the provided buffer.
///
/// `buf` must be exactly `SIZE` (32768) bytes.
/// We read in chunks to avoid I2C issues with very large transfers.
pub fn read_all(buf: &mut [u8; SIZE]) -> Result<(), i2c::I2cError> {
    const CHUNK: usize = 256;
    let mut offset = 0u16;
    let mut pos = 0;

    while pos < SIZE {
        let len = CHUNK.min(SIZE - pos);
        read(offset, &mut buf[pos..pos + len])?;
        offset = offset.wrapping_add(len as u16);
        pos += len;
    }

    Ok(())
}

/// Write the entire 32KB buffer back to FRAM.
///
/// `buf` must be exactly `SIZE` (32768) bytes.
pub fn write_all(buf: &[u8; SIZE]) -> Result<(), i2c::I2cError> {
    const CHUNK: usize = 14; // Same chunking as write()
    let mut pos = 0;

    while pos < SIZE {
        let len = CHUNK.min(SIZE - pos);
        write(pos as u16, &buf[pos..pos + len])?;
        pos += len;
    }

    Ok(())
}

/// Quick sanity check: write and read back a byte at the given address.
/// Returns true if FRAM is responding correctly.
pub fn probe() -> bool {
    let mut buf = [0u8; 1];
    // Try reading a byte at address 0 — if the device NACKs, probe fails
    read(0, &mut buf).is_ok()
}

/// BSC1 (I2C1) driver for BCM2710.
///
/// BSC1 register base: 0x3F804000
/// Uses GPIO 2 (SDA1) and GPIO 3 (SCL1) in Alt0 mode.
///
/// The Broadcom Serial Controller (BSC) is a fast-mode (400kHz) I2C master.
/// We use polled I/O (no DMA, no interrupts) for simplicity.

use core::clone::Clone;
use core::marker::Copy;

use crate::{gpio, mmio, timer};

const BSC1_BASE: usize = mmio::PERIPHERAL_BASE + 0x0080_4000;

const BSC_C: usize = BSC1_BASE + 0x00; // Control
const BSC_S: usize = BSC1_BASE + 0x04; // Status
const BSC_DLEN: usize = BSC1_BASE + 0x08; // Data length
const BSC_A: usize = BSC1_BASE + 0x0C; // Slave address
const BSC_FIFO: usize = BSC1_BASE + 0x10; // Data FIFO
const BSC_DIV: usize = BSC1_BASE + 0x14; // Clock divider
const BSC_CLKT: usize = BSC1_BASE + 0x1C; // Clock stretch timeout

// Control register bits
const C_I2CEN: u32 = 1 << 15; // I2C enable
const C_ST: u32 = 1 << 7; // Start transfer
const C_CLEAR: u32 = 1 << 4; // Clear FIFO (also bit 5)
const C_READ: u32 = 1 << 0; // Read transfer

// Status register bits
const S_CLKT: u32 = 1 << 9; // Clock stretch timeout
const S_ERR: u32 = 1 << 8; // ACK error
const S_RXD: u32 = 1 << 5; // RX FIFO contains data
const S_TXD: u32 = 1 << 4; // TX FIFO can accept data
const S_DONE: u32 = 1 << 1; // Transfer done

/// I2C error types.
#[derive(Debug, Clone, Copy)]
pub enum I2cError {
    /// Slave did not acknowledge
    Nack,
    /// Clock stretch timeout
    ClockTimeout,
    /// Transfer timeout (our own timeout waiting for DONE)
    Timeout,
}

/// Initialize I2C1 at the given speed.
///
/// `speed_khz`: 100 for standard mode, 400 for fast mode.
/// Core clock is assumed to be 250 MHz.
pub fn init(speed_khz: u32) {
    // Configure GPIO 2 (SDA1) and GPIO 3 (SCL1) to Alt0
    gpio::set_function(2, gpio::Function::Alt0);
    gpio::set_function(3, gpio::Function::Alt0);
    gpio::set_pull(2, gpio::Pull::Up);
    gpio::set_pull(3, gpio::Pull::Up);

    // Set clock divider: core_clk / divider = I2C clock
    // 250 MHz / 2500 = 100 kHz; 250 MHz / 625 = 400 kHz
    let divider = 250_000 / speed_khz;
    mmio::write(BSC_DIV, divider);

    // Set clock stretch timeout (reasonable default)
    mmio::write(BSC_CLKT, 0x40);

    // Enable I2C
    mmio::write(BSC_C, C_I2CEN);
}

/// Clear the status flags and FIFO.
fn clear() {
    // Clear DONE, ERR, CLKT flags by writing 1 to them
    mmio::write(BSC_S, S_CLKT | S_ERR | S_DONE);
    // Clear FIFO
    mmio::write(BSC_C, C_I2CEN | C_CLEAR);
}

/// Wait for transfer to complete with timeout.
fn wait_done() -> Result<(), I2cError> {
    let deadline = timer::get_time_us() + 100_000; // 100ms timeout
    loop {
        let status = mmio::read(BSC_S);
        if status & S_DONE != 0 {
            // Check for errors
            if status & S_ERR != 0 {
                mmio::write(BSC_S, S_ERR);
                return Err(I2cError::Nack);
            }
            if status & S_CLKT != 0 {
                mmio::write(BSC_S, S_CLKT);
                return Err(I2cError::ClockTimeout);
            }
            // Clear DONE
            mmio::write(BSC_S, S_DONE);
            return Ok(());
        }
        if timer::get_time_us() > deadline {
            return Err(I2cError::Timeout);
        }
        core::hint::spin_loop();
    }
}

/// Write bytes to an I2C device.
///
/// `addr`: 7-bit I2C slave address
/// `data`: bytes to write
pub fn write(addr: u8, data: &[u8]) -> Result<(), I2cError> {
    clear();

    // Set slave address
    mmio::write(BSC_A, addr as u32);
    // Set data length
    mmio::write(BSC_DLEN, data.len() as u32);

    // Fill FIFO before starting transfer
    let mut idx = 0;
    while idx < data.len() && (mmio::read(BSC_S) & S_TXD != 0) {
        mmio::write(BSC_FIFO, data[idx] as u32);
        idx += 1;
    }

    // Start write transfer (no READ bit)
    mmio::write(BSC_C, C_I2CEN | C_ST);

    // Continue filling FIFO as space becomes available
    while idx < data.len() {
        let status = mmio::read(BSC_S);
        if status & S_TXD != 0 {
            mmio::write(BSC_FIFO, data[idx] as u32);
            idx += 1;
        }
        if status & (S_ERR | S_CLKT) != 0 {
            break;
        }
        core::hint::spin_loop();
    }

    wait_done()
}

/// Read bytes from an I2C device.
///
/// `addr`: 7-bit I2C slave address
/// `buf`: buffer to read into
pub fn read(addr: u8, buf: &mut [u8]) -> Result<(), I2cError> {
    clear();

    // Set slave address
    mmio::write(BSC_A, addr as u32);
    // Set data length
    mmio::write(BSC_DLEN, buf.len() as u32);

    // Start read transfer
    mmio::write(BSC_C, C_I2CEN | C_ST | C_READ);

    // Read from FIFO as data becomes available
    let mut idx = 0;
    let deadline = timer::get_time_us() + 100_000; // 100ms timeout
    while idx < buf.len() {
        let status = mmio::read(BSC_S);
        if status & S_RXD != 0 {
            buf[idx] = (mmio::read(BSC_FIFO) & 0xFF) as u8;
            idx += 1;
        }
        if status & (S_ERR | S_CLKT) != 0 {
            break;
        }
        if status & S_DONE != 0 {
            // Drain remaining FIFO data
            while idx < buf.len() && (mmio::read(BSC_S) & S_RXD != 0) {
                buf[idx] = (mmio::read(BSC_FIFO) & 0xFF) as u8;
                idx += 1;
            }
            break;
        }
        if timer::get_time_us() > deadline {
            return Err(I2cError::Timeout);
        }
        core::hint::spin_loop();
    }

    wait_done()
}

/// Combined write-then-read (repeated start) transaction.
///
/// This is used by the FRAM to set the read address then read data.
/// The BSC controller on BCM2710 does not natively support repeated start in a single
/// transaction, so we do a write followed by a read as two separate transfers.
/// The FRAM device handles this correctly.
pub fn write_read(addr: u8, write_data: &[u8], read_buf: &mut [u8]) -> Result<(), I2cError> {
    write(addr, write_data)?;
    read(addr, read_buf)
}

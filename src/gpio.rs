/// GPIO driver for BCM2710.
///
/// GPIO register base: 0x3F200000
/// Each GPFSEL register controls 10 pins (3 bits per pin).
/// Supports function selection (input, output, alt0..alt5) and pull-up/down config.

use core::clone::Clone;
use core::marker::Copy;

use crate::mmio;

const GPIO_BASE: usize = mmio::PERIPHERAL_BASE + 0x0020_0000;

// GPFSEL registers (function select), 10 pins per register, 3 bits each
const GPFSEL0: usize = GPIO_BASE + 0x00;
// GPIO pull-up/down enable
const GPPUD: usize = GPIO_BASE + 0x94;
// GPIO pull-up/down enable clocks
const GPPUDCLK0: usize = GPIO_BASE + 0x98;

/// GPIO alternate function selections.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Function {
    Alt0 = 0b100,
    Alt5 = 0b010,
}

/// GPIO pull state.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Pull {
    Off = 0b00,
    Up = 0b10,
}

/// Set the alternate function for a GPIO pin.
pub fn set_function(pin: u32, func: Function) {
    let reg = GPFSEL0 + ((pin / 10) * 4) as usize;
    let shift = (pin % 10) * 3;
    let mut val = mmio::read(reg);
    val &= !(0b111 << shift); // Clear existing function
    val |= (func as u32) << shift; // Set new function
    mmio::write(reg, val);
}

/// Configure pull-up/down for a GPIO pin.
///
/// BCM2835/2837 pull-up/down sequence:
/// 1. Write to GPPUD to set desired pull state
/// 2. Wait 150 cycles
/// 3. Write to GPPUDCLK to clock the control signal into the pin
/// 4. Wait 150 cycles
/// 5. Clear GPPUD and GPPUDCLK
pub fn set_pull(pin: u32, pull: Pull) {
    mmio::write(GPPUD, pull as u32);
    mmio::spin_delay(150);

    let clk_reg = GPPUDCLK0 + ((pin / 32) * 4) as usize;
    mmio::write(clk_reg, 1 << (pin % 32));
    mmio::spin_delay(150);

    mmio::write(GPPUD, 0);
    mmio::write(clk_reg, 0);
}



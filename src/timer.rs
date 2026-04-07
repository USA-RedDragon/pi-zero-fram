/// BCM2710 System Timer driver.
///
/// The system timer is a 1 MHz free-running counter at 0x3F003000.
/// It provides a 64-bit counter (CLO + CHI) and 4 compare registers.
/// We only use the free-running counter for delays.

use crate::mmio;

const TIMER_BASE: usize = mmio::PERIPHERAL_BASE + 0x0000_3000;
const TIMER_CLO: usize = TIMER_BASE + 0x04; // Counter lower 32 bits
const TIMER_CHI: usize = TIMER_BASE + 0x08; // Counter upper 32 bits

/// Read the current system timer value in microseconds (64-bit).
pub fn get_time_us() -> u64 {
    // Read high first, then low, then high again to handle rollover
    loop {
        let hi = mmio::read(TIMER_CHI) as u64;
        let lo = mmio::read(TIMER_CLO) as u64;
        let hi2 = mmio::read(TIMER_CHI) as u64;
        if hi == hi2 {
            return (hi << 32) | lo;
        }
    }
}

/// Delay for the specified number of microseconds.
pub fn delay_us(us: u64) {
    let target = get_time_us() + us;
    while get_time_us() < target {
        core::hint::spin_loop();
    }
}

/// Delay for the specified number of milliseconds.
pub fn delay_ms(ms: u64) {
    delay_us(ms * 1000);
}

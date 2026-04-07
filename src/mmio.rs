/// MMIO volatile read/write helpers for bare-metal register access.
///
/// BCM2710 (BCM2837) peripheral base: 0x3F000000

pub const PERIPHERAL_BASE: usize = 0x3F00_0000;

/// Volatile read of a 32-bit MMIO register.
#[inline(always)]
pub fn read(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Volatile write to a 32-bit MMIO register.
#[inline(always)]
pub fn write(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// Read-modify-write: set bits in a register.
#[inline(always)]
pub fn set_bits(addr: usize, mask: u32) {
    write(addr, read(addr) | mask);
}

/// Read-modify-write: clear bits in a register.
#[inline(always)]
pub fn clear_bits(addr: usize, mask: u32) {
    write(addr, read(addr) & !mask);
}

/// Spin-loop delay (approximate, not calibrated).
#[inline(always)]
pub fn spin_delay(cycles: u32) {
    for _ in 0..cycles {
        core::hint::spin_loop();
    }
}

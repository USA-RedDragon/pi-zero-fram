/// Panic handler — writes panic message to Mini UART if initialized.

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Try to print panic info via UART for debugging
    crate::uart::write_str("\r\n!!! PANIC !!!\r\n");
    if let Some(location) = info.location() {
        crate::uart::write_str(location.file());
        crate::uart::write_str(":");
        crate::uart::write_u32(location.line());
        crate::uart::write_str("\r\n");
    }
    if let Some(msg) = info.message().as_str() {
        crate::uart::write_str(msg);
        crate::uart::write_str("\r\n");
    }
    loop {
        core::hint::spin_loop();
    }
}

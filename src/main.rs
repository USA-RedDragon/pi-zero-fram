#![no_std]
#![no_main]

//! Pi Zero 2 W — Bare Metal FRAM Text Editor
//!
//! Boots on the Cortex-A53, initializes I2C + USB CDC ACM serial,
//! loads 32KB FRAM contents, and presents a Nano-like text editor
//! over the USB serial terminal.

use core::arch::global_asm;

mod mmio;
mod gpio;
mod uart;
mod timer;
mod i2c;
mod fram;
mod mailbox;
mod panic;
mod usb;
mod editor;

use editor::buffer::TextBuffer;
use editor::input::{Action, InputParser};
use editor::render::Renderer;
use editor::terminal::{BufWriter, TermWrite, Terminal};
use usb::dwc2::Dwc2Bus;
use usb_device::bus::UsbBusAllocator;
use usb_device::prelude::*;

// Include the boot assembly
global_asm!(include_str!("boot.s"));

// ============================================================================
// Static storage (no allocator)
// ============================================================================

/// USB bus allocator — must be static for usb-device lifetime requirements.
/// Initialized once in kernel_main before any references are taken.
static mut USB_BUS_ALLOC: Option<UsbBusAllocator<Dwc2Bus>> = None;

/// The text editing buffer — 32KB in-memory copy of FRAM.
static mut TEXT_BUF: TextBuffer = TextBuffer::new();

/// Buffer for serial data received during rendering (write_bytes drains into this
/// to prevent USB deadlock when host can't send because its OUT buffer is full).
static mut PENDING_RX: [u8; 4096] = [0; 4096];
static mut PENDING_RX_LEN: usize = 0;

// ============================================================================
// USB serial TermWrite adapter
// ============================================================================

/// Wraps mutable references to SerialPort + UsbDevice so that the
/// rendering / terminal code can write escape sequences through TermWrite.
struct UsbWriter<'a, 'b> {
    serial: &'a mut usbd_serial::SerialPort<'b, Dwc2Bus>,
    usb_dev: &'a mut UsbDevice<'b, Dwc2Bus>,
}

impl TermWrite for UsbWriter<'_, '_> {
    fn write_bytes(&mut self, data: &[u8]) {
        let mut off = 0;
        let deadline = timer::get_time_us() + 2_000_000; // 2 second timeout
        while off < data.len() {
            self.usb_dev.poll(&mut [self.serial]);

            // Drain incoming serial data to prevent USB OUT buffer from filling
            // up and deadlocking the IN endpoint. Data is buffered for later
            // processing by the main loop.
            {
                let pending = unsafe { &mut *core::ptr::addr_of_mut!(PENDING_RX) };
                let pending_len = unsafe { &mut *core::ptr::addr_of_mut!(PENDING_RX_LEN) };
                if *pending_len < pending.len() {
                    let mut tmp = [0u8; 64];
                    if let Ok(n) = self.serial.read(&mut tmp) {
                        let avail = pending.len() - *pending_len;
                        let copy = n.min(avail);
                        pending[*pending_len..*pending_len + copy]
                            .copy_from_slice(&tmp[..copy]);
                        *pending_len += copy;
                    }
                }
            }

            match self.serial.write(&data[off..]) {
                Ok(n) => {
                    off += n;
                }
                Err(UsbError::WouldBlock) => {
                    self.usb_dev.poll(&mut [self.serial]);
                    core::hint::spin_loop();
                }
                Err(_) => return,
            }
            if timer::get_time_us() > deadline {
                uart::write_str("[WRITE] timeout!\n");
                return;
            }
        }
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> usize {
        self.usb_dev.poll(&mut [self.serial]);
        match self.serial.read(buf) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

}

// ============================================================================
// Entry point
// ============================================================================

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    // Phase 1: Initialize debug output
    uart::init();
    uart::write_str("\n\n=== Pi Zero 2 FRAM Editor ===\n");
    uart::write_str("[MAIN] UART initialized\n");

    // Phase 2: Initialize I2C and probe FRAM
    uart::write_str("[MAIN] Initializing I2C at 400kHz...\n");
    i2c::init(400);

    uart::write_str("[MAIN] Probing FRAM at 0x50...\n");
    if !fram::probe() {
        uart::write_str("[MAIN] ERROR: FRAM not responding! Check wiring.\n");
        uart::write_str("[MAIN] Continuing anyway (editor will show empty buffer)\n");
    } else {
        uart::write_str("[MAIN] FRAM detected OK\n");
    }

    // Load FRAM contents into text buffer
    uart::write_str("[MAIN] Loading FRAM contents...\n");
    // Safety: single-threaded bare metal, TEXT_BUF is only accessed here.
    let text_buf = unsafe { &mut *core::ptr::addr_of_mut!(TEXT_BUF) };
    match text_buf.load_from_fram() {
        Ok(()) => {
            uart::write_str("[MAIN] Loaded ");
            uart::write_u32(text_buf.len as u32);
            uart::write_str(" bytes from FRAM\n");
        }
        Err(_) => {
            uart::write_str("[MAIN] WARN: Failed to load FRAM, starting with empty buffer\n");
        }
    }

    // Phase 3: Initialize USB
    uart::write_str("[MAIN] Initializing USB DWC2...\n");

    // Create the bus, init hardware, then store in static allocator.
    // init() only touches MMIO registers — it doesn't modify Dwc2Bus fields,
    // so the bus state inside the allocator is identical to Dwc2Bus::new().
    {
        let mut bus = Dwc2Bus::new();
        bus.init();
        // Safety: single-threaded bare metal; we write once before taking any references.
        unsafe { *core::ptr::addr_of_mut!(USB_BUS_ALLOC) = Some(UsbBusAllocator::new(bus)) };
    }
    // Safety: set above, never modified again.
    let usb_alloc = unsafe { (*core::ptr::addr_of!(USB_BUS_ALLOC)).as_ref().unwrap() };

    let mut serial = usbd_serial::SerialPort::new(usb_alloc);

    let mut usb_dev = UsbDeviceBuilder::new(usb_alloc, UsbVidPid(0x1209, 0x0001))
        .strings(&[StringDescriptors::default()
            .manufacturer("PiZeroFRAM")
            .product("FRAM Text Editor")
            .serial_number("0001")])
        .unwrap()
        .device_class(usbd_serial::USB_CLASS_CDC)
        .max_packet_size_0(64)
        .unwrap()
        .build();

    uart::write_str("[MAIN] USB device created, waiting for host connection...\n");

    // Wait for enumeration
    loop {
        if usb_dev.poll(&mut [&mut serial]) {
            // Drain any initial data
            let mut tmp = [0u8; 64];
            let _ = serial.read(&mut tmp);
        }
        if usb_dev.state() == UsbDeviceState::Configured {
            break;
        }
        core::hint::spin_loop();
    }

    uart::write_str("[MAIN] USB configured, waiting for terminal (DTR)...\n");

    // Wait for the host terminal (screen/minicom) to open the port.
    // When screen opens /dev/ttyACMx, the Linux cdc_acm driver sends
    // SET_CONTROL_LINE_STATE with DTR=1. We detect that here.
    // This doesn't consume any user keystrokes.
    loop {
        usb_dev.poll(&mut [&mut serial]);
        if serial.dtr() {
            break;
        }
        core::hint::spin_loop();
    }
    // Small delay for the terminal to finish initializing
    for _ in 0..100 {
        usb_dev.poll(&mut [&mut serial]);
        timer::delay_ms(2);
    }

    uart::write_str("[MAIN] Terminal connected (DTR asserted), starting editor\n");

    // Phase 4: Run the editor
    let mut term = Terminal::new();
    let mut renderer = Renderer::new();
    let mut input_parser = InputParser::new();

    // Detect terminal size using DSR handshake + CPR.
    // DSR pings until screen is responsive, then queries size.
    {
        let mut w = UsbWriter { serial: &mut serial, usb_dev: &mut usb_dev };
        term.detect_size(&mut w);
    }

    uart::write_str("[MAIN] Terminal size: ");
    uart::write_u32(term.cols as u32);
    uart::write_str("x");
    uart::write_u32(term.rows as u32);
    uart::write_str("\n");

    // Initial full render at detected (or default 80x24) size
    uart::write_str("[RENDER] initial start\n");
    let mut visible = Renderer::visible_lines(&term);
    text_buf.ensure_visible(visible);
    renderer.invalidate_all();
    {
        let mut w = UsbWriter { serial: &mut serial, usb_dev: &mut usb_dev };
        let mut bw = BufWriter::new(&mut w);
        renderer.render(&mut bw, &term, text_buf);
        bw.flush();
    }
    for _ in 0..10 {
        usb_dev.poll(&mut [&mut serial]);
    }
    uart::write_str("[RENDER] initial done\n");

    // Main editor loop
    let mut read_buf = [0u8; 64];
    let mut pending_discard = false;
    loop {
        // Poll USB
        usb_dev.poll(&mut [&mut serial]);

        // Check if still configured
        if usb_dev.state() != UsbDeviceState::Configured {
            // USB disconnected — wait for reconnection
            loop {
                if usb_dev.poll(&mut [&mut serial])
                    && usb_dev.state() == UsbDeviceState::Configured
                {
                    timer::delay_ms(200);
                    renderer.invalidate_all();
                    let mut w = UsbWriter { serial: &mut serial, usb_dev: &mut usb_dev };
                    let mut bw = BufWriter::new(&mut w);
                    renderer.render(&mut bw, &term, text_buf);
                    bw.flush();
                    break;
                }
                core::hint::spin_loop();
            }
            continue;
        }

        // Read and process all available input before rendering.
        // Batches paste operations so we don't render after every 64-byte USB packet.
        // When data has been received, waits up to 20ms of silence before rendering,
        // so that inter-packet gaps during paste don't trigger premature redraws.
        let mut needs_render = false;
        let mut needs_refresh = false;
        let mut content_changed = false; // text was inserted/deleted
        let mut newline_deleted = false; // a newline was removed (lines shift)
        let scroll_before = text_buf.scroll_row;
        let drain_deadline = timer::get_time_us() + 500_000; // overall 500ms cap
        let mut idle_since: u64 = 0;
        let mut total_bytes_rx: usize = 0;

        loop {
            usb_dev.poll(&mut [&mut serial]);

            let n = match serial.read(&mut read_buf) {
                Ok(n) => n,
                Err(UsbError::WouldBlock) => 0,
                Err(_e) => {
                    uart::write_str("[ERR] serial.read error\n");
                    0
                }
            };

            if n == 0 {
                if !needs_render {
                    break; // no data at all, back to outer idle loop
                }
                // Data was previously received. Wait briefly for more paste data.
                if idle_since == 0 {
                    idle_since = timer::get_time_us();
                }
                // Adaptive gap: 20ms for normal typing, 200ms when paste detected
                let gap_us = if total_bytes_rx > 64 { 200_000 } else { 20_000 };
                if timer::get_time_us().wrapping_sub(idle_since) > gap_us {
                    break; // silence exceeded gap threshold — go render
                }
                if timer::get_time_us() > drain_deadline {
                    break;
                }
                core::hint::spin_loop();
                continue;
            }

            idle_since = 0; // reset gap timer on any data
            total_bytes_rx += n;

            for i in 0..n {
                if let Some(action) = input_parser.feed(read_buf[i]) {
                    // Reset the quit-confirmation flag on any action other than Reload itself
                    if !matches!(action, Action::Reload) {
                        pending_discard = false;
                    }
                    match action {
                        Action::Char(ch) => {
                            text_buf.insert_byte(ch);
                            needs_render = true;
                            content_changed = true;
                        }
                        Action::Enter => {
                            text_buf.insert_byte(b'\n');
                            needs_render = true;
                            content_changed = true;
                            newline_deleted = true; // reuse flag: newline structural change
                        }
                        Action::Backspace => {
                            let was_newline = text_buf.backspace();
                            needs_render = true;
                            content_changed = true;
                            newline_deleted |= was_newline;
                        }
                        Action::Delete => {
                            let was_newline = text_buf.delete_at_cursor();
                            needs_render = true;
                            content_changed = true;
                            newline_deleted |= was_newline;
                        }
                        Action::ArrowUp => {
                            text_buf.move_up();
                            needs_render = true;
                        }
                        Action::ArrowDown => {
                            text_buf.move_down();
                            needs_render = true;
                        }
                        Action::ArrowLeft => {
                            text_buf.move_left();
                            needs_render = true;
                        }
                        Action::ArrowRight => {
                            text_buf.move_right();
                            needs_render = true;
                        }
                        Action::Home => {
                            text_buf.home();
                            needs_render = true;
                        }
                        Action::End => {
                            text_buf.end();
                            needs_render = true;
                        }
                        Action::PageUp => {
                            text_buf.page_up(visible);
                            needs_render = true;
                        }
                        Action::PageDown => {
                            text_buf.page_down(visible);
                            needs_render = true;
                        }
                        Action::Save => {
                            match text_buf.save_to_fram() {
                                Ok(()) => {
                                    uart::write_str("[MAIN] Saved to FRAM\n");
                                }
                                Err(_) => {
                                    text_buf.set_status(b"ERROR: Save failed!");
                                    uart::write_str("[MAIN] ERROR: Save failed\n");
                                }
                            }
                            needs_render = true;
                        }
                        Action::Reload => {
                            if text_buf.dirty && !pending_discard {
                                text_buf.set_status(b"Unsaved changes! ^R again to discard");
                                pending_discard = true;
                                needs_render = true;
                            } else {
                                pending_discard = false;
                                let _ = text_buf.load_from_fram();
                                text_buf.set_status(b"Reloaded from FRAM");
                                needs_render = true;
                                content_changed = true;
                            }
                        }
                        Action::Refresh => {
                            pending_discard = false;
                            needs_refresh = true;
                        }
                    }
                }
            }

            // Stop draining if Refresh requested (needs terminal I/O) or time limit hit
            if needs_refresh || timer::get_time_us() > drain_deadline {
                break;
            }
        }

        if !needs_render && !needs_refresh {
            core::hint::spin_loop();
            continue;
        }

        if needs_refresh {
            // Re-detect terminal size (requires its own read/write cycle)
            {
                let mut w = UsbWriter { serial: &mut serial, usb_dev: &mut usb_dev };
                term.detect_size(&mut w);
            }
            visible = Renderer::visible_lines(&term);
        }

        text_buf.ensure_visible(visible);

        // Decide what to invalidate:
        // - Refresh or Reload: full redraw
        // - Scroll changed: full redraw (all visible lines differ)
        // - Newline inserted/deleted: full redraw (lines below shift)
        // - Character delete/backspace (no newline): only current line dirty
        // - Character insert (no newline): only current line dirty
        // - Cursor-only move (no scroll): incremental (just bars + cursor)
        let scroll_changed = text_buf.scroll_row != scroll_before;
        if needs_refresh || scroll_changed || newline_deleted {
            renderer.invalidate_all();
        } else if content_changed {
            // Only the current line changed (single char insert/delete, no newline)
            let vis_idx = text_buf.cursor_row.saturating_sub(text_buf.scroll_row);
            renderer.mark_line_dirty(vis_idx);
        }
        // Status bar and cursor are always updated by render()

        // Render loop: render, then check if data arrived during the render
        // (buffered by write_bytes into PENDING_RX). If so, process it and
        // render again. Repeat until no more pending data.
        loop {
            {
                // Clear pending buffer before render (write_bytes will fill it)
                unsafe { *core::ptr::addr_of_mut!(PENDING_RX_LEN) = 0; }
                let mut w = UsbWriter { serial: &mut serial, usb_dev: &mut usb_dev };
                let mut bw = BufWriter::new(&mut w);
                renderer.render(&mut bw, &term, text_buf);
                bw.flush();
            }
            // Flush remaining serial data by polling a few times
            for _ in 0..4 {
                usb_dev.poll(&mut [&mut serial]);
            }

            // Process any input data that was drained during the render.
            let pend_len = unsafe { *core::ptr::addr_of!(PENDING_RX_LEN) };
            if pend_len == 0 {
                break; // no data arrived during render — we're done
            }

            let pending = unsafe { &*core::ptr::addr_of!(PENDING_RX) };
            for i in 0..pend_len {
                if let Some(action) = input_parser.feed(pending[i]) {
                    if !matches!(action, Action::Reload) {
                        pending_discard = false;
                    }
                    match action {
                        Action::Char(ch) => { text_buf.insert_byte(ch); }
                        Action::Enter => { text_buf.insert_byte(b'\n'); }
                        Action::Backspace => { text_buf.backspace(); }
                        Action::Delete => { text_buf.delete_at_cursor(); }
                        Action::ArrowUp => { text_buf.move_up(); }
                        Action::ArrowDown => { text_buf.move_down(); }
                        Action::ArrowLeft => { text_buf.move_left(); }
                        Action::ArrowRight => { text_buf.move_right(); }
                        Action::Home => { text_buf.home(); }
                        Action::End => { text_buf.end(); }
                        Action::PageUp => { text_buf.page_up(visible); }
                        Action::PageDown => { text_buf.page_down(visible); }
                        Action::Save => { let _ = text_buf.save_to_fram(); }
                        Action::Reload => {
                            if text_buf.dirty && !pending_discard {
                                text_buf.set_status(b"Unsaved changes! ^R again to discard");
                                pending_discard = true;
                            } else {
                                pending_discard = false;
                                let _ = text_buf.load_from_fram();
                                text_buf.set_status(b"Reloaded from FRAM");
                            }
                        }
                        Action::Refresh => {}
                    }
                }
            }
            unsafe { *core::ptr::addr_of_mut!(PENDING_RX_LEN) = 0; }

            // More data was processed — need to re-render with updated content
            text_buf.ensure_visible(visible);
            renderer.invalidate_all();
        }

        text_buf.status_msg_len = 0;
    }
}

/// Synopsys DesignWare DWC2 OTG USB controller — Device mode driver.
///
/// This implements the `usb_device::bus::UsbBus` trait for the BCM2710's
/// DWC2 controller at 0x3F980000, enabling USB Full Speed device operation.
///
/// Reference implementations:
/// - Circle framework: lib/usb/gadget/dwusbgadget.cpp
/// - Linux kernel: drivers/usb/dwc2/gadget.c
/// - Synopsys DesignWare Cores USB 2.0 Hi-Speed OTG Databook

use core::cell::UnsafeCell;
use core::cmp::Ord;
use core::iter::Iterator;
use core::option::Option::{self, Some, None};
use core::result::Result::{Ok, Err};

use crate::{mmio, timer, uart};
use usb_device::bus::{PollResult, UsbBus};
use usb_device::endpoint::{EndpointAddress, EndpointType};
use usb_device::{Result as UsbResult, UsbDirection, UsbError};

// ============================================================================
// DWC2 Register Map (base address for BCM2710)
// ============================================================================

const USB_BASE: usize = mmio::PERIPHERAL_BASE + 0x0098_0000;

// -- Core Global Registers --
const GAHBCFG: usize = USB_BASE + 0x008;
const GUSBCFG: usize = USB_BASE + 0x00C;
const GRSTCTL: usize = USB_BASE + 0x010;
const GINTSTS: usize = USB_BASE + 0x014;
const GINTMSK: usize = USB_BASE + 0x018;
const GRXSTSP: usize = USB_BASE + 0x020; // RX status pop
const GRXFSIZ: usize = USB_BASE + 0x024;
const GNPTXFSIZ: usize = USB_BASE + 0x028; // EP0 TX FIFO size (in device mode)
const GSNPSID: usize = USB_BASE + 0x040;
const GHWCFG3: usize = USB_BASE + 0x04C; // Hardware config register 3
const GDFIFOCFG: usize = USB_BASE + 0x05C; // Global DFIFO configuration

// Device IN Endpoint TX FIFO Size registers (EP1..EP15)
const fn dieptxf(ep: usize) -> usize {
    USB_BASE + 0x104 + (ep - 1) * 4
}

// -- Device Mode Registers --
const DCFG: usize = USB_BASE + 0x800;
const DCTL: usize = USB_BASE + 0x804;
const DSTS: usize = USB_BASE + 0x808;
const DIEPMSK: usize = USB_BASE + 0x810;
const DOEPMSK: usize = USB_BASE + 0x814;
const DAINT: usize = USB_BASE + 0x818;
const DAINTMSK: usize = USB_BASE + 0x81C;

// -- Device IN Endpoint registers (0x900 + ep * 0x20) --
const fn diepctl(ep: usize) -> usize {
    USB_BASE + 0x900 + ep * 0x20
}
const fn diepint(ep: usize) -> usize {
    USB_BASE + 0x908 + ep * 0x20
}
const fn diepsiz(ep: usize) -> usize {
    USB_BASE + 0x910 + ep * 0x20
}
const fn dtxfsts(ep: usize) -> usize {
    USB_BASE + 0x918 + ep * 0x20
}

// -- Device OUT Endpoint registers (0xB00 + ep * 0x20) --
const fn doepctl(ep: usize) -> usize {
    USB_BASE + 0xB00 + ep * 0x20
}
const fn doepint(ep: usize) -> usize {
    USB_BASE + 0xB08 + ep * 0x20
}
const fn doepsiz(ep: usize) -> usize {
    USB_BASE + 0xB10 + ep * 0x20
}

// -- FIFO access: EP n FIFO at base + 0x1000 + n * 0x1000 --
const fn ep_fifo(ep: usize) -> usize {
    USB_BASE + 0x1000 + ep * 0x1000
}

// PCGCCTL — Power and Clock Gating Control
const PCGCCTL: usize = USB_BASE + 0xE00;

// ============================================================================
// Register bit definitions
// ============================================================================

// GAHBCFG
const GAHBCFG_GLBLINTRMSK: u32 = 1 << 0;
const GAHBCFG_TXFELVL: u32 = 1 << 7; // TX FIFO empty level (1 = completely empty)

// GUSBCFG
const GUSBCFG_PHYIF: u32 = 1 << 3; // PHY interface width (0=8-bit UTMI+)
const GUSBCFG_ULPI_UTMI_SEL: u32 = 1 << 4; // 0=UTMI+, 1=ULPI
const GUSBCFG_SRPCAP: u32 = 1 << 8; // SRP capable
const GUSBCFG_HNPCAP: u32 = 1 << 9; // HNP capable
const GUSBCFG_USBTRDTIM_SHIFT: u32 = 10;
const GUSBCFG_USBTRDTIM_MASK: u32 = 0xF << 10;
const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30;

// GRSTCTL
const GRSTCTL_CSRST: u32 = 1 << 0; // Core soft reset
const GRSTCTL_AHBIDLE: u32 = 1 << 31; // AHB idle
const GRSTCTL_TXFFLSH: u32 = 1 << 5; // TX FIFO flush
const GRSTCTL_RXFFLSH: u32 = 1 << 4; // RX FIFO flush
const GRSTCTL_INTKNQFLSH: u32 = 1 << 3; // IN token sequence learning queue flush
const GRSTCTL_TXFNUM_SHIFT: u32 = 6;

// GINTSTS / GINTMSK
const GINTSTS_RXFLVL: u32 = 1 << 4; // RX FIFO non-empty
const GINTSTS_USBSUSP: u32 = 1 << 11;
const GINTSTS_USBRST: u32 = 1 << 12; // USB reset
const GINTSTS_ENUMDNE: u32 = 1 << 13; // Enumeration done
const GINTSTS_IEPINT: u32 = 1 << 18; // IN endpoint interrupt
const GINTSTS_OEPINT: u32 = 1 << 19; // OUT endpoint interrupt
const GINTSTS_WKUPINT: u32 = 1 << 31; // Resume/wakeup

// GRXSTSP bits (RX status pop)
const GRXSTSP_EPNUM_MASK: u32 = 0xF;
const GRXSTSP_BCNT_SHIFT: u32 = 4;
const GRXSTSP_BCNT_MASK: u32 = 0x7FF << 4;
const GRXSTSP_PKTSTS_SHIFT: u32 = 17;
const GRXSTSP_PKTSTS_MASK: u32 = 0xF << 17;

// GRXSTSP packet status values
const PKTSTS_GLOBAL_OUT_NAK: u32 = 1;
const PKTSTS_OUT_DATA: u32 = 2;
const PKTSTS_OUT_COMPLETE: u32 = 3;
const PKTSTS_SETUP_COMPLETE: u32 = 4;
const PKTSTS_SETUP_DATA: u32 = 6;

// DCFG
const DCFG_DEVSPD_MASK: u32 = 0x3;
const DCFG_DEVSPD_FS: u32 = 0x1; // Full speed using UTMI+ PHY
const DCFG_DEVADDR_SHIFT: u32 = 4;
const DCFG_DEVADDR_MASK: u32 = 0x7F << 4;

// DCTL
const DCTL_SFTDISCON: u32 = 1 << 1;
const DCTL_CGNPINNAK: u32 = 1 << 8;
const DCTL_CGOUTNAK: u32 = 1 << 10;
const DCTL_PWRONPRGDONE: u32 = 1 << 11; // Power-on programming done

// DSTS
const DSTS_ENUMSPD_MASK: u32 = 0x6;
const DSTS_ENUMSPD_FS: u32 = 0x2; // Full speed (30/60 MHz UTMI+ PHY)

// DIEPCTLn / DOEPCTLn
const DEPCTL_MPS_MASK: u32 = 0x7FF; // Max packet size (bits 10:0)
const DEPCTL_USBACTEP: u32 = 1 << 15;
const DEPCTL_SETD0PID: u32 = 1 << 28;
const DEPCTL_STALL: u32 = 1 << 21;
const DEPCTL_EPDIS: u32 = 1 << 30; // Endpoint disable
const DEPCTL_TXFNUM_SHIFT: u32 = 22;
const DEPCTL_CNAK: u32 = 1 << 26;
const DEPCTL_SNAK: u32 = 1 << 27;
const DEPCTL_EPENA: u32 = 1 << 31;

// EP type in DEPCTL (bits 19:18)
const DEPCTL_EPTYP_CTRL: u32 = 0 << 18;
const DEPCTL_EPTYP_ISO: u32 = 1 << 18;
const DEPCTL_EPTYP_BULK: u32 = 2 << 18;
const DEPCTL_EPTYP_INT: u32 = 3 << 18;

// DIEPINTn / DOEPINTn bits
const DEPINT_XFERCOMPL: u32 = 1 << 0;
const DEPINT_SETUP: u32 = 1 << 3; // OUT EP only: SETUP phase done


// DIEPSIZn / DOEPSIZn
const DEPSIZ_PKTCNT_SHIFT: u32 = 19;
const DEPSIZ_SUPCNT_SHIFT: u32 = 29; // DOEPSIZ0 only: SETUP packet count

// ============================================================================
// DWC2 USB Bus Implementation
// ============================================================================

/// Maximum number of endpoints (EP0 + 3 non-zero EPs for CDC ACM).
const MAX_EP: usize = 4;

/// Maximum packet size for Full Speed.
const MAX_PACKET_SIZE: u16 = 64;

/// Per-endpoint state.
struct EpState {
    ep_type: Option<EndpointType>,
    max_packet_size: u16,
    is_stalled: bool,
    // For OUT endpoints: buffer for received data
    out_buf: [u8; 64],
    out_len: usize,
    out_ready: bool,
}

impl EpState {
    const fn new() -> Self {
        Self {
            ep_type: None,
            max_packet_size: 0,
            is_stalled: false,
            out_buf: [0; 64],
            out_len: 0,
            out_ready: false,
        }
    }
}

/// DWC2 USB Bus interior state.
struct Dwc2State {
    ep_in: [EpState; MAX_EP],
    ep_out: [EpState; MAX_EP],
    ep_in_alloc: [bool; MAX_EP],
    ep_out_alloc: [bool; MAX_EP],
    setup_buf: [u8; 8],
    setup_ready: bool,
    bus_reset: bool,
}

impl Dwc2State {
    const fn new() -> Self {
        Self {
            ep_in: [EpState::new(), EpState::new(), EpState::new(), EpState::new()],
            ep_out: [EpState::new(), EpState::new(), EpState::new(), EpState::new()],
            ep_in_alloc: [false; MAX_EP],
            ep_out_alloc: [false; MAX_EP],
            setup_buf: [0; 8],
            setup_ready: false,
            bus_reset: false,
        }
    }

    fn init(&mut self) {
        uart::write_str("[USB] DWC2 init starting\n");

        // 0. Power on the USB controller via the VideoCore mailbox.
        // Without this the PHY and analog circuits are unpowered;
        // register writes succeed but the hardware doesn't function.
        uart::write_str("[USB] Powering on USB via mailbox...\n");
        if !crate::mailbox::power_on_usb() {
            uart::write_str("[USB] WARN: mailbox power-on failed!\n");
        } else {
            uart::write_str("[USB] USB power on OK\n");
        }

        // Verify DWC2 is present by reading SNPS ID
        let snps_id = mmio::read(GSNPSID);
        uart::write_str("[USB] SNPSID: ");
        uart::write_hex(snps_id);
        uart::write_str("\n");

        // 1. Disable power gating
        mmio::write(PCGCCTL, 0);

        // 2. Core soft reset
        self.core_reset();

        // 2a. Immediately force soft-disconnect after reset.
        // core_reset() clears DCTL, leaving SFTDISCON=0 (connected).
        // Without this, the host sees the device for ~90ms during PHY/mode
        // init, starts enumeration, then we disconnect — causing a double
        // enumeration that triggers ~5s reconnect delays in tio/screen.
        mmio::set_bits(DCTL, DCTL_SFTDISCON);

        // 3. Configure USB: select UTMI+ PHY, force device mode
        //
        // BCM2837 DWC2 has a UTMI+ PHY (not an internal FS transceiver).
        // - Clear ULPI_UTMI_SEL (bit 4) → select UTMI+ over ULPI
        // - Clear PHYIF (bit 3) → 8-bit UTMI+ interface width
        // - Do NOT set PHYSEL (bit 6) — that selects a non-existent internal
        //   FS serial transceiver; setting it results in a dead USB port.
        // - USB turnaround time = 9 (correct for 8-bit UTMI+ at full speed;
        //   value 5 is for 16-bit UTMI+ at high speed).
        let mut gusbcfg = mmio::read(GUSBCFG);
        gusbcfg &= !GUSBCFG_HNPCAP;
        gusbcfg &= !GUSBCFG_SRPCAP;
        gusbcfg &= !GUSBCFG_ULPI_UTMI_SEL; // UTMI+
        gusbcfg &= !GUSBCFG_PHYIF;         // 8-bit width
        gusbcfg |= GUSBCFG_FORCEDEVMODE;
        gusbcfg &= !GUSBCFG_USBTRDTIM_MASK;
        gusbcfg |= 9 << GUSBCFG_USBTRDTIM_SHIFT;
        mmio::write(GUSBCFG, gusbcfg);

        // Wait for device mode to settle (>25ms per spec; Circle waits 25ms)
        timer::delay_ms(50);

        // 3a. Restart PHY clock after mode switch (Circle: InitCoreDevice).
        // Core soft reset may put PCGCCTL into a stopped-clock state.
        // Writing 0 ensures the PHY clock and AHB clock are both running.
        // The 40ms delay gives the PHY PLL time to lock onto the correct
        // frequency (48 MHz for FS). Without this, data packets are corrupted.
        mmio::write(PCGCCTL, 0);
        timer::delay_ms(40);

        // 3b. Set soft-disconnect NOW (device mode is active, DCTL writes
        //     take effect).  We hold SFTDISCON=1 until enable() clears it
        //     after all endpoints are allocated.
        mmio::set_bits(DCTL, DCTL_SFTDISCON);

        // 4. Configure device: Full Speed, EP mismatch count = 1
        let mut dcfg = mmio::read(DCFG);
        dcfg &= !DCFG_DEVSPD_MASK;
        dcfg |= DCFG_DEVSPD_FS;
        dcfg &= !DCFG_DEVADDR_MASK; // Address 0
        dcfg &= !(0x1F << 18); // Clear EP mismatch count
        dcfg |= 1 << 18; // EPMISCNT = 1 (Linux: DCFG_EPMISCNT(1))
        mmio::write(DCFG, dcfg);

        // 5. Configure FIFO sizes (in 32-bit words)
        //
        // DWC2 on BCM2710 has 4096 bytes (1024 words) of shared FIFO.
        // Allocation:
        //   RX FIFO:    256 words (1024 bytes) — must hold largest OUT packet + status
        //   EP0 TX:     64 words (256 bytes)
        //   EP1 TX:     128 words (512 bytes) — bulk IN for CDC data
        //   EP2 TX:     64 words (256 bytes) — interrupt IN for CDC notification
        //   Remaining:  reserved
        mmio::write(GRXFSIZ, 256);
        // GNPTXFSIZ: start address | size (EP0 TX)
        mmio::write(GNPTXFSIZ, (64 << 16) | 256);
        // DIEPTXF1: EP1 TX FIFO (start after EP0 TX)
        mmio::write(dieptxf(1), (128 << 16) | (256 + 64));
        // DIEPTXF2: EP2 TX FIFO
        mmio::write(dieptxf(2), (64 << 16) | (256 + 64 + 128));

        // 5a. Write Global DFIFO Configuration.
        // Tells the DWC2 the total FIFO depth and where the EP info base
        // starts. Both Circle and Linux write this register. Without it
        // the controller may use incorrect FIFO RAM boundaries.
        let hwcfg3 = mmio::read(GHWCFG3);
        let total_fifo_depth = (hwcfg3 >> 16) & 0xFFFF;
        let ep_info_base: u32 = 256 + 64 + 128 + 64; // sum of all configured FIFOs
        mmio::write(GDFIFOCFG, total_fifo_depth | (ep_info_base << 16));
        uart::write_str("[USB] GHWCFG3=0x");
        uart::write_hex(hwcfg3);
        uart::write_str(" GDFIFOCFG=0x");
        uart::write_hex(mmio::read(GDFIFOCFG));
        uart::write_str("\n");

        // 5b. Disable any stale endpoints left from a previous session.
        // Circle and Linux both explicitly disable all EPs during init.
        for ep in 0..MAX_EP {
            let depctl_in = mmio::read(diepctl(ep));
            if depctl_in & DEPCTL_EPENA != 0 {
                mmio::write(diepctl(ep), depctl_in | DEPCTL_EPDIS | DEPCTL_SNAK);
            }
            mmio::write(diepsiz(ep), 0);

            let depctl_out = mmio::read(doepctl(ep));
            if depctl_out & DEPCTL_EPENA != 0 {
                mmio::write(doepctl(ep), depctl_out | DEPCTL_EPDIS | DEPCTL_SNAK);
            }
            mmio::write(doepsiz(ep), 0);
        }

        // 6. Flush all FIFOs
        self.flush_tx_fifo(0x10); // 0x10 = flush all TX FIFOs
        self.flush_rx_fifo();

        // 6a. Flush the IN token sequence learning queue.
        // Stale entries can cause the DWC2 to send data from the wrong TX FIFO
        // in response to host IN tokens → EPROTO.
        mmio::write(GRSTCTL, GRSTCTL_INTKNQFLSH);

        // 7. Clear all pending interrupts
        mmio::write(GINTSTS, 0xFFFF_FFFF);
        for ep in 0..MAX_EP {
            mmio::write(diepint(ep), 0xFF);
            mmio::write(doepint(ep), 0xFF);
        }

        // 8. Configure interrupt masks
        // Device endpoint interrupt mask.
        // Do NOT unmask DEPINT_TXFEMP: the TX FIFOs start empty so TXFEMP would
        // fire constantly before any transfer, making IEPINT always asserted and
        // causing poll() to spin forever checking empty interrupts.
        mmio::write(DIEPMSK, DEPINT_XFERCOMPL | DEPINT_SETUP);
        mmio::write(DOEPMSK, DEPINT_XFERCOMPL | DEPINT_SETUP);

        // Global interrupt mask
        mmio::write(
            GINTMSK,
            GINTSTS_USBRST
                | GINTSTS_ENUMDNE
                | GINTSTS_IEPINT
                | GINTSTS_OEPINT
                | GINTSTS_RXFLVL
                | GINTSTS_USBSUSP
                | GINTSTS_WKUPINT,
        );

        // 9. Enable global interrupts in AHB config
        mmio::write(
            GAHBCFG,
            GAHBCFG_GLBLINTRMSK | GAHBCFG_TXFELVL,
        );

        // 9a. Signal power-on programming done.
        // Linux kernel (dwc2_hsotg_core_init_disconnected): pulse this bit to
        // tell the DWC2 core that register programming is complete.
        mmio::set_bits(DCTL, DCTL_PWRONPRGDONE);
        timer::delay_us(10);
        mmio::clear_bits(DCTL, DCTL_PWRONPRGDONE);

        // 9b. Activate EP0 (Linux sets USBACTEP on both DOEPCTL0 and DIEPCTL0).
        mmio::write(
            doepctl(0),
            DEPCTL_USBACTEP | DEPCTL_EPENA | DEPCTL_CNAK,
        );
        mmio::write(
            diepctl(0),
            DEPCTL_USBACTEP, // EP0 IN: active but not enabled yet
        );

        // 10. Leave soft-disconnect set: do NOT connect yet.
        // The device will connect when enable() is called after all endpoints
        // have been allocated by usb-device (UsbDeviceBuilder::build()).
        // Connecting here (before alloc_ep/enable) means the host sees a device
        // with no working control endpoint and aborts enumeration permanently.

        uart::write_str("[USB] DWC2 init complete (not connected yet)\n");
    }

    fn core_reset(&self) {
        // Wait for AHB idle
        let deadline = timer::get_time_us() + 100_000;
        while mmio::read(GRSTCTL) & GRSTCTL_AHBIDLE == 0 {
            if timer::get_time_us() > deadline {
                uart::write_str("[USB] WARN: AHB idle timeout\n");
                break;
            }
            core::hint::spin_loop();
        }

        // Trigger core soft reset
        mmio::set_bits(GRSTCTL, GRSTCTL_CSRST);

        // Wait for reset to complete (CSRST bit self-clears)
        let deadline = timer::get_time_us() + 100_000;
        while mmio::read(GRSTCTL) & GRSTCTL_CSRST != 0 {
            if timer::get_time_us() > deadline {
                uart::write_str("[USB] WARN: core reset timeout\n");
                break;
            }
            core::hint::spin_loop();
        }

        // Wait after reset — DWC2 databook says internal clocks need time
        // to stabilize.  Circle waits 100ms; less may work but we match them.
        timer::delay_ms(100);
    }

    fn flush_tx_fifo(&self, fifo_num: u32) {
        mmio::write(GRSTCTL, GRSTCTL_TXFFLSH | (fifo_num << GRSTCTL_TXFNUM_SHIFT));
        let deadline = timer::get_time_us() + 10_000;
        while mmio::read(GRSTCTL) & GRSTCTL_TXFFLSH != 0 {
            if timer::get_time_us() > deadline {
                break;
            }
            core::hint::spin_loop();
        }
        timer::delay_us(3);
    }

    fn flush_rx_fifo(&self) {
        mmio::write(GRSTCTL, GRSTCTL_RXFFLSH);
        let deadline = timer::get_time_us() + 10_000;
        while mmio::read(GRSTCTL) & GRSTCTL_RXFFLSH != 0 {
            if timer::get_time_us() > deadline {
                break;
            }
            core::hint::spin_loop();
        }
        timer::delay_us(3);
    }

    /// Handle USB bus reset: reconfigure EP0, clear address.
    fn handle_reset(&mut self) {
        uart::write_str("[USB] Bus reset\n");

        // Clear device address
        let dcfg = mmio::read(DCFG);
        mmio::write(DCFG, dcfg & !DCFG_DEVADDR_MASK);

        // Ensure EP0 IN and OUT interrupts are unmasked.
        // Use set_bits (not write) to preserve non-zero EP bits set by enable().
        mmio::set_bits(DAINTMSK, (1 << 0) | (1 << 16)); // EP0 IN + EP0 OUT

        // Set NAK on all OUT endpoints before flushing
        for ep in 0..MAX_EP {
            mmio::set_bits(doepctl(ep), DEPCTL_SNAK);
        }

        // Flush all FIFOs FIRST (Circle order: flush, then re-arm EP0)
        self.flush_tx_fifo(0x10);
        self.flush_rx_fifo();

        // Flush the IN token learning queue (stale entries cause EPROTO)
        mmio::write(GRSTCTL, GRSTCTL_INTKNQFLSH);

        // Clear all endpoint interrupts
        for ep in 0..MAX_EP {
            mmio::write(diepint(ep), 0xFF);
            mmio::write(doepint(ep), 0xFF);
        }

        // NOW configure EP0 OUT to receive SETUP packets
        // DOEPSIZ0: 1 packet, 64 bytes max, expect up to 3 SETUP packets
        mmio::write(
            doepsiz(0),
            (1 << DEPSIZ_PKTCNT_SHIFT) | 64 | (3 << DEPSIZ_SUPCNT_SHIFT),
        );

        // Enable EP0 OUT with USBACTEP
        mmio::write(
            doepctl(0),
            DEPCTL_USBACTEP | DEPCTL_EPENA | DEPCTL_CNAK,
        );

        // Activate EP0 IN (not enabled, just mark active)
        mmio::write(diepctl(0), DEPCTL_USBACTEP);

        // Clear global OUT NAK and IN NAK
        mmio::set_bits(DCTL, DCTL_CGOUTNAK | DCTL_CGNPINNAK);

        self.bus_reset = true;
    }

    /// Handle enumeration done interrupt.
    fn handle_enum_done(&mut self) {
        uart::write_str("[USB] Enumeration done\n");

        // Read enumerated speed from DSTS
        let dsts = mmio::read(DSTS);
        let speed = dsts & DSTS_ENUMSPD_MASK;
        if speed == DSTS_ENUMSPD_FS {
            uart::write_str("[USB] Full Speed detected\n");
        } else {
            uart::write_str("[USB] Speed bits: ");
            uart::write_hex(speed);
            uart::write_str("\n");
        }

        // Update USB turnaround time now that we know enumerated speed.
        // Circle writes 9 here for UTMI+ 8-bit.
        let mut gusbcfg = mmio::read(GUSBCFG);
        gusbcfg &= !GUSBCFG_USBTRDTIM_MASK;
        gusbcfg |= 9 << GUSBCFG_USBTRDTIM_SHIFT;
        mmio::write(GUSBCFG, gusbcfg);

        // Set EP0 max packet size to 64 bytes.
        // DIEPCTL0 bits [1:0] = MPSIZ: 0b00 = 64 bytes (FS default).
        // Preserve USBACTEP.
        let ctl = mmio::read(diepctl(0));
        mmio::write(diepctl(0), (ctl & !0x3) | DEPCTL_USBACTEP);

        // Re-arm EP0 OUT for the first SETUP packet after enumeration
        self.ep0_out_prepare();
    }

    /// Process RX FIFO data (called when RXFLVL interrupt fires).
    fn handle_rxflvl(&mut self) {
        let status = mmio::read(GRXSTSP);
        let ep_num = (status & GRXSTSP_EPNUM_MASK) as usize;
        let byte_count = ((status & GRXSTSP_BCNT_MASK) >> GRXSTSP_BCNT_SHIFT) as usize;
        let pkt_status = (status & GRXSTSP_PKTSTS_MASK) >> GRXSTSP_PKTSTS_SHIFT;

        match pkt_status {
            PKTSTS_SETUP_DATA => {
                // Read 8 bytes of SETUP data from FIFO
                if ep_num == 0 && byte_count == 8 {
                    let w0 = mmio::read(ep_fifo(0));
                    let w1 = mmio::read(ep_fifo(0));
                    self.setup_buf[0..4].copy_from_slice(&w0.to_le_bytes());
                    self.setup_buf[4..8].copy_from_slice(&w1.to_le_bytes());
                    self.setup_ready = true;
                }
            }
            PKTSTS_OUT_DATA => {
                // Read OUT data from FIFO
                if ep_num < MAX_EP {
                    if byte_count > 0 {
                        let words = (byte_count + 3) / 4;
                        let mut pos = 0;
                        for _ in 0..words {
                            let word = mmio::read(ep_fifo(0));
                            let bytes = word.to_le_bytes();
                            for j in 0..4 {
                                if pos < byte_count && pos < self.ep_out[ep_num].out_buf.len() {
                                    self.ep_out[ep_num].out_buf[pos] = bytes[j];
                                    pos += 1;
                                }
                            }
                        }
                    }
                    // Set ready even for zero-length packets (control status phase)
                    self.ep_out[ep_num].out_len = byte_count;
                    self.ep_out[ep_num].out_ready = true;
                }
            }
            PKTSTS_OUT_COMPLETE => {
                // OUT transfer complete — no data to read from FIFO.
            }
            PKTSTS_SETUP_COMPLETE => {
                // Clear the hardware SETUP-done interrupt immediately.
                //
                // DWC2 latches DEPINT_SETUP in DOEPINT when the SETUP
                // phase finishes.  poll() snapshots GINTSTS *before*
                // draining the RX FIFO, so GINTSTS_OEPINT may not yet
                // reflect this new interrupt.  Without clearing here, a
                // stale DEPINT_SETUP survives into the NEXT poll() cycle
                // and sets a phantom ep_setup bit.  usb-device then calls
                // handle_setup() when it should call handle_out(),
                // consuming the DATA OUT phase as a bogus SETUP packet
                // and stalling the control transfer for 5 seconds.
                // This specifically breaks SET_LINE_CODING (the only
                // standard CDC request with an OUT data phase).
                mmio::write(doepint(ep_num), DEPINT_SETUP | DEPINT_XFERCOMPL);
            }
            PKTSTS_GLOBAL_OUT_NAK => {
                // Global OUT NAK effective — no action needed
            }
            _ => {}
        }
    }

    /// Start a write (IN) transfer on the given endpoint.
    fn start_in_transfer(&self, ep_num: usize, data: &[u8]) {
        let len = data.len();

        // Set transfer size and packet count
        let pkt_cnt = if len == 0 { 1 } else { 1 }; // Single packet for now
        mmio::write(diepsiz(ep_num), (pkt_cnt << DEPSIZ_PKTCNT_SHIFT) | (len as u32));

        // Enable endpoint and clear NAK
        let ctl = mmio::read(diepctl(ep_num));
        mmio::write(diepctl(ep_num), ctl | DEPCTL_EPENA | DEPCTL_CNAK);

        // Write data to TX FIFO
        let words = (len + 3) / 4;
        for i in 0..words {
            let mut word = 0u32;
            for j in 0..4 {
                let idx = i * 4 + j;
                if idx < len {
                    word |= (data[idx] as u32) << (j * 8);
                }
            }
            mmio::write(ep_fifo(ep_num), word);
        }
    }

    /// Prepare EP0 OUT to receive the next SETUP/OUT packet.
    fn ep0_out_prepare(&self) {
        mmio::write(
            doepsiz(0),
            (1 << DEPSIZ_PKTCNT_SHIFT) | 64 | (3 << DEPSIZ_SUPCNT_SHIFT),
        );
        mmio::write(doepctl(0), mmio::read(doepctl(0)) | DEPCTL_EPENA | DEPCTL_CNAK);
    }

    /// Prepare a non-zero OUT endpoint to receive data.
    fn epn_out_prepare(&self, ep_num: usize, max_pkt: u16) {
        mmio::write(
            doepsiz(ep_num),
            (1 << DEPSIZ_PKTCNT_SHIFT) | (max_pkt as u32),
        );
        mmio::write(
            doepctl(ep_num),
            mmio::read(doepctl(ep_num)) | DEPCTL_EPENA | DEPCTL_CNAK,
        );
    }
}

impl Dwc2State {
    fn alloc_ep(
        &mut self,
        ep_dir: UsbDirection,
        ep_addr: Option<EndpointAddress>,
        ep_type: EndpointType,
        max_packet_size: u16,
        _interval: u8,
    ) -> UsbResult<EndpointAddress> {
        let mps = if max_packet_size == 0 || max_packet_size > MAX_PACKET_SIZE {
            MAX_PACKET_SIZE
        } else {
            max_packet_size
        };

        match ep_dir {
            UsbDirection::In => {
                // Find a free IN endpoint
                let idx = if let Some(addr) = ep_addr {
                    let i = addr.index();
                    if i < MAX_EP && !self.ep_in_alloc[i] {
                        i
                    } else {
                        return Err(UsbError::InvalidEndpoint);
                    }
                } else {
                    // Auto-allocate: skip EP0 if looking for non-control
                    let start = if ep_type == EndpointType::Control { 0 } else { 1 };
                    match (start..MAX_EP).find(|&i| !self.ep_in_alloc[i]) {
                        Some(i) => i,
                        None => return Err(UsbError::EndpointOverflow),
                    }
                };
                self.ep_in_alloc[idx] = true;
                self.ep_in[idx].ep_type = Some(ep_type);
                self.ep_in[idx].max_packet_size = mps;
                Ok(EndpointAddress::from_parts(idx, UsbDirection::In))
            }
            UsbDirection::Out => {
                let idx = if let Some(addr) = ep_addr {
                    let i = addr.index();
                    if i < MAX_EP && !self.ep_out_alloc[i] {
                        i
                    } else {
                        return Err(UsbError::InvalidEndpoint);
                    }
                } else {
                    let start = if ep_type == EndpointType::Control { 0 } else { 1 };
                    match (start..MAX_EP).find(|&i| !self.ep_out_alloc[i]) {
                        Some(i) => i,
                        None => return Err(UsbError::EndpointOverflow),
                    }
                };
                self.ep_out_alloc[idx] = true;
                self.ep_out[idx].ep_type = Some(ep_type);
                self.ep_out[idx].max_packet_size = mps;
                Ok(EndpointAddress::from_parts(idx, UsbDirection::Out))
            }
        }
    }

    fn enable(&mut self) {
        uart::write_str("[USB] UsbBus::enable()\n");

        // Configure allocated non-zero endpoints in hardware
        for ep_num in 1..MAX_EP {
            if self.ep_in_alloc[ep_num] {
                let ep = &self.ep_in[ep_num];
                let ep_type_bits = match ep.ep_type.unwrap() {
                    EndpointType::Control => DEPCTL_EPTYP_CTRL,
                    EndpointType::Isochronous { .. } => DEPCTL_EPTYP_ISO,
                    EndpointType::Bulk => DEPCTL_EPTYP_BULK,
                    EndpointType::Interrupt => DEPCTL_EPTYP_INT,
                };
                mmio::write(
                    diepctl(ep_num),
                    DEPCTL_USBACTEP
                        | DEPCTL_SNAK
                        | ep_type_bits
                        | (ep.max_packet_size as u32 & DEPCTL_MPS_MASK)
                        | ((ep_num as u32) << DEPCTL_TXFNUM_SHIFT)
                        | DEPCTL_SETD0PID,
                );
                // Unmask this EP's interrupt
                mmio::set_bits(DAINTMSK, 1 << ep_num);
            }

            if self.ep_out_alloc[ep_num] {
                let ep = &self.ep_out[ep_num];
                let ep_type_bits = match ep.ep_type.unwrap() {
                    EndpointType::Control => DEPCTL_EPTYP_CTRL,
                    EndpointType::Isochronous { .. } => DEPCTL_EPTYP_ISO,
                    EndpointType::Bulk => DEPCTL_EPTYP_BULK,
                    EndpointType::Interrupt => DEPCTL_EPTYP_INT,
                };
                mmio::write(
                    doepctl(ep_num),
                    DEPCTL_USBACTEP
                        | DEPCTL_SNAK
                        | ep_type_bits
                        | (ep.max_packet_size as u32 & DEPCTL_MPS_MASK)
                        | DEPCTL_SETD0PID,
                );
                // Unmask this EP's interrupt (OUT EPs are in bits 16..31)
                mmio::set_bits(DAINTMSK, 1 << (16 + ep_num));
                // Prepare to receive
                self.epn_out_prepare(ep_num, ep.max_packet_size);
            }
        }

        // Clear global OUT/IN NAK before connecting. The DWC2 may have
        // these set from init. Without this, per-endpoint CNAK is ignored
        // and all OUT endpoints silently drop incoming data.
        mmio::set_bits(DCTL, DCTL_CGOUTNAK | DCTL_CGNPINNAK);

        // Now connect: clear soft-disconnect so the host sees the device.
        uart::write_str("[USB] Connecting device (clearing SFTDISCON)\n");
        mmio::clear_bits(DCTL, DCTL_SFTDISCON);
    }

    fn reset(&self) {
        // Called by usb-device after it processes a bus reset.
        // Re-prepare EP0 OUT for SETUP packets.
        self.ep0_out_prepare();

        // Fully re-configure all allocated non-zero endpoints.
        // The DWC2 hardware clears ACTIVE_EP (and possibly other fields)
        // in DIEPCTL/DOEPCTL on every USB bus reset.  Circle's code
        // confirms this with an assert(!ACTIVE_EP) after reset.
        // Without restoring the full configuration, endpoints NAK forever.
        for ep_num in 1..MAX_EP {
            if self.ep_in_alloc[ep_num] {
                let ep = &self.ep_in[ep_num];
                let ep_type_bits = match ep.ep_type.unwrap() {
                    EndpointType::Control => DEPCTL_EPTYP_CTRL,
                    EndpointType::Isochronous { .. } => DEPCTL_EPTYP_ISO,
                    EndpointType::Bulk => DEPCTL_EPTYP_BULK,
                    EndpointType::Interrupt => DEPCTL_EPTYP_INT,
                };
                mmio::write(
                    diepctl(ep_num),
                    DEPCTL_USBACTEP
                        | DEPCTL_SNAK
                        | ep_type_bits
                        | (ep.max_packet_size as u32 & DEPCTL_MPS_MASK)
                        | ((ep_num as u32) << DEPCTL_TXFNUM_SHIFT)
                        | DEPCTL_SETD0PID,
                );
                mmio::set_bits(DAINTMSK, 1 << ep_num);
            }

            if self.ep_out_alloc[ep_num] {
                let ep = &self.ep_out[ep_num];
                let ep_type_bits = match ep.ep_type.unwrap() {
                    EndpointType::Control => DEPCTL_EPTYP_CTRL,
                    EndpointType::Isochronous { .. } => DEPCTL_EPTYP_ISO,
                    EndpointType::Bulk => DEPCTL_EPTYP_BULK,
                    EndpointType::Interrupt => DEPCTL_EPTYP_INT,
                };
                mmio::write(
                    doepctl(ep_num),
                    DEPCTL_USBACTEP
                        | DEPCTL_SNAK
                        | ep_type_bits
                        | (ep.max_packet_size as u32 & DEPCTL_MPS_MASK)
                        | DEPCTL_SETD0PID,
                );
                mmio::set_bits(DAINTMSK, 1 << (16 + ep_num));
                self.epn_out_prepare(ep_num, ep.max_packet_size);
            }
        }
    }

    fn set_device_address(&self, addr: u8) {
        uart::write_str("[USB] set_device_address(");
        uart::write_hex(addr as u32);
        uart::write_str(")\n");
        let dcfg = mmio::read(DCFG);
        mmio::write(
            DCFG,
            (dcfg & !DCFG_DEVADDR_MASK) | ((addr as u32) << DCFG_DEVADDR_SHIFT),
        );
    }

    fn write(&self, ep_addr: EndpointAddress, buf: &[u8]) -> UsbResult<usize> {
        let ep_num = ep_addr.index();
        if ep_num >= MAX_EP || ep_addr.direction() != UsbDirection::In {
            return Err(UsbError::InvalidEndpoint);
        }

        // Don't start a new transfer while one is in progress.
        // The DWC2 doesn't support overlapping IN transfers on the same EP.
        let ctl = mmio::read(diepctl(ep_num));
        if ctl & DEPCTL_EPENA != 0 {
            return Err(UsbError::WouldBlock);
        }

        // Check TX FIFO has space
        let space = (mmio::read(dtxfsts(ep_num)) & 0xFFFF) as usize * 4;
        if space < buf.len() {
            return Err(UsbError::WouldBlock);
        }

        self.start_in_transfer(ep_num, buf);
        Ok(buf.len())
    }

    fn read(&mut self, ep_addr: EndpointAddress, buf: &mut [u8]) -> UsbResult<usize> {
        let ep_num = ep_addr.index();
        if ep_num >= MAX_EP || ep_addr.direction() != UsbDirection::Out {
            return Err(UsbError::InvalidEndpoint);
        }

        if ep_num == 0 && self.setup_ready {
            // Return SETUP data
            let len = 8.min(buf.len());
            buf[..len].copy_from_slice(&self.setup_buf[..len]);
            self.setup_ready = false;
            self.ep0_out_prepare();
            return Ok(len);
        }

        if self.ep_out[ep_num].out_ready {
            let len = self.ep_out[ep_num].out_len.min(buf.len());
            buf[..len].copy_from_slice(&self.ep_out[ep_num].out_buf[..len]);
            self.ep_out[ep_num].out_ready = false;
            self.ep_out[ep_num].out_len = 0;

            // Re-arm the OUT endpoint
            if ep_num == 0 {
                self.ep0_out_prepare();
            } else {
                let mps = self.ep_out[ep_num].max_packet_size;
                self.epn_out_prepare(ep_num, mps);
            }
            return Ok(len);
        }

        Err(UsbError::WouldBlock)
    }

    fn set_stalled(&mut self, ep_addr: EndpointAddress, stalled: bool) {
        let ep_num = ep_addr.index();
        if ep_num >= MAX_EP {
            return;
        }

        match ep_addr.direction() {
            UsbDirection::In => {
                self.ep_in[ep_num].is_stalled = stalled;
                if stalled {
                    mmio::set_bits(diepctl(ep_num), DEPCTL_STALL);
                } else {
                    mmio::clear_bits(diepctl(ep_num), DEPCTL_STALL);
                    mmio::set_bits(diepctl(ep_num), DEPCTL_SETD0PID);
                }
            }
            UsbDirection::Out => {
                self.ep_out[ep_num].is_stalled = stalled;
                if stalled {
                    mmio::set_bits(doepctl(ep_num), DEPCTL_STALL);
                } else {
                    mmio::clear_bits(doepctl(ep_num), DEPCTL_STALL);
                    mmio::set_bits(doepctl(ep_num), DEPCTL_SETD0PID);
                }
            }
        }
    }

    fn is_stalled(&self, ep_addr: EndpointAddress) -> bool {
        let ep_num = ep_addr.index();
        if ep_num >= MAX_EP {
            return false;
        }
        match ep_addr.direction() {
            UsbDirection::In => self.ep_in[ep_num].is_stalled,
            UsbDirection::Out => self.ep_out[ep_num].is_stalled,
        }
    }

    fn poll(&mut self) -> PollResult {
        let gintsts = mmio::read(GINTSTS);
        let gintmsk = mmio::read(GINTMSK);
        let active = gintsts & gintmsk;

        if active == 0 {
            return PollResult::None;
        }

        // Handle bus reset
        if active & GINTSTS_USBRST != 0 {
            mmio::write(GINTSTS, GINTSTS_USBRST);
            self.handle_reset();
            return PollResult::Reset;
        }

        // Handle enumeration done
        if active & GINTSTS_ENUMDNE != 0 {
            mmio::write(GINTSTS, GINTSTS_ENUMDNE);
            self.handle_enum_done();
        }

        // Handle suspend
        if active & GINTSTS_USBSUSP != 0 {
            mmio::write(GINTSTS, GINTSTS_USBSUSP);
            return PollResult::Suspend;
        }

        // Handle resume/wakeup
        if active & GINTSTS_WKUPINT != 0 {
            mmio::write(GINTSTS, GINTSTS_WKUPINT);
            return PollResult::Resume;
        }

        // Handle RX FIFO non-empty — drain ALL pending entries.
        // The DWC2 pushes multiple status entries per transaction (e.g.
        // SETUP_DATA then SETUP_COMPLETE). If we only pop one entry per
        // poll() call, stale entries remain in the FIFO and can confuse
        // the DWC2's internal packet state machine.
        while mmio::read(GINTSTS) & GINTSTS_RXFLVL != 0 {
            mmio::clear_bits(GINTMSK, GINTSTS_RXFLVL);
            self.handle_rxflvl();
            mmio::set_bits(GINTMSK, GINTSTS_RXFLVL);
        }

        // Build endpoint bitmasks for usb-device
        let mut ep_out: u16 = 0;
        let mut ep_in_complete: u16 = 0;
        let mut ep_setup: u16 = 0;

        // Check OUT endpoint interrupts
        if active & GINTSTS_OEPINT != 0 {
            let daint = mmio::read(DAINT);
            for ep_num in 0..MAX_EP {
                if daint & (1 << (16 + ep_num)) != 0 {
                    let int_val = mmio::read(doepint(ep_num));

                    if int_val & DEPINT_SETUP != 0 {
                        mmio::write(doepint(ep_num), DEPINT_SETUP);
                        ep_setup |= 1 << ep_num;
                    }

                    if int_val & DEPINT_XFERCOMPL != 0 {
                        mmio::write(doepint(ep_num), DEPINT_XFERCOMPL);
                        if ep_setup & (1 << ep_num) == 0 {
                            ep_out |= 1 << ep_num;
                        }
                    }
                }
            }
        }

        // Check IN endpoint interrupts
        if active & GINTSTS_IEPINT != 0 {
            let daint = mmio::read(DAINT);
            for ep_num in 0..MAX_EP {
                if daint & (1 << ep_num) != 0 {
                    let int_val = mmio::read(diepint(ep_num));

                    if int_val & DEPINT_XFERCOMPL != 0 {
                        mmio::write(diepint(ep_num), DEPINT_XFERCOMPL);
                        ep_in_complete |= 1 << ep_num;
                    }
                }
            }
        }

        // Check if setup_ready was set by RXFLVL handler
        if self.setup_ready {
            ep_setup |= 1 << 0;
        }

        // Check if OUT data was received by RXFLVL handler
        for ep_num in 0..MAX_EP {
            if self.ep_out[ep_num].out_ready {
                ep_out |= 1 << ep_num;
            }
        }

        if ep_setup != 0 || ep_out != 0 || ep_in_complete != 0 {
            PollResult::Data {
                ep_out,
                ep_in_complete,
                ep_setup,
            }
        } else {
            PollResult::None
        }
    }

    fn force_reset(&self) -> UsbResult<()> {
        // Trigger soft disconnect then reconnect
        mmio::set_bits(DCTL, DCTL_SFTDISCON);
        timer::delay_ms(10);
        mmio::clear_bits(DCTL, DCTL_SFTDISCON);
        Ok(())
    }
}

// ============================================================================
// Public wrapper: UnsafeCell provides interior mutability for UsbBus trait
// ============================================================================

/// DWC2 USB Bus for usb-device.
///
/// Wraps the internal state in `UnsafeCell` because the `UsbBus` trait requires
/// `&self` for methods like `read()` and `poll()` that must mutate endpoint state.
pub struct Dwc2Bus {
    inner: UnsafeCell<Dwc2State>,
}

// Safety: bare-metal single-threaded — no concurrent access occurs.
unsafe impl Sync for Dwc2Bus {}

impl Dwc2Bus {
    /// Create a new DWC2 USB bus (uninitialized — call `init()` before use).
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Dwc2State::new()),
        }
    }

    /// Initialize the DWC2 controller in device mode.
    pub fn init(&mut self) {
        self.inner.get_mut().init();
    }
}

impl UsbBus for Dwc2Bus {
    // The DWC2 applies DCFG.DAD immediately — the address must be set
    // BEFORE the status ZLP, not after.  Without this quirk, a race in
    // poll() can cause ep_in_complete (ZLP XFERCOMPL) and ep_setup (next
    // SETUP) to arrive together; usb-device prioritises SETUP and never
    // calls handle_in_complete, so pending_address is never applied.
    const QUIRK_SET_ADDRESS_BEFORE_STATUS: bool = true;

    fn alloc_ep(
        &mut self,
        ep_dir: UsbDirection,
        ep_addr: Option<EndpointAddress>,
        ep_type: EndpointType,
        max_packet_size: u16,
        interval: u8,
    ) -> UsbResult<EndpointAddress> {
        self.inner
            .get_mut()
            .alloc_ep(ep_dir, ep_addr, ep_type, max_packet_size, interval)
    }

    fn enable(&mut self) {
        self.inner.get_mut().enable()
    }

    fn reset(&self) {
        // Safety: bare-metal single-threaded; reset() only writes MMIO registers.
        unsafe { (*self.inner.get()).reset() }
    }

    fn set_device_address(&self, addr: u8) {
        unsafe { (*self.inner.get()).set_device_address(addr) }
    }

    fn write(&self, ep_addr: EndpointAddress, buf: &[u8]) -> UsbResult<usize> {
        unsafe { (*self.inner.get()).write(ep_addr, buf) }
    }

    fn read(&self, ep_addr: EndpointAddress, buf: &mut [u8]) -> UsbResult<usize> {
        // Safety: bare-metal single-threaded; UsbBus requires &self but read() mutates state.
        unsafe { (*self.inner.get()).read(ep_addr, buf) }
    }

    fn set_stalled(&self, ep_addr: EndpointAddress, stalled: bool) {
        unsafe { (*self.inner.get()).set_stalled(ep_addr, stalled) }
    }

    fn is_stalled(&self, ep_addr: EndpointAddress) -> bool {
        unsafe { (*self.inner.get()).is_stalled(ep_addr) }
    }

    fn suspend(&self) {}
    fn resume(&self) {}

    fn poll(&self) -> PollResult {
        // Safety: bare-metal single-threaded; UsbBus requires &self but poll() mutates state.
        unsafe { (*self.inner.get()).poll() }
    }

    fn force_reset(&self) -> UsbResult<()> {
        unsafe { (*self.inner.get()).force_reset() }
    }
}

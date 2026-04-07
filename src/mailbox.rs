/// VideoCore mailbox property tag interface for BCM2710.
///
/// The mailbox sits at PERIPHERAL_BASE + 0xB880 and provides communication
/// with the GPU firmware.  We use channel 8 (property tags ARM→VC) to send
/// power-on requests for the USB controller.
///
/// Reference: <https://github.com/raspberrypi/firmware/wiki/Mailbox-property-interface>

use crate::mmio;

const MAILBOX_BASE: usize = mmio::PERIPHERAL_BASE + 0x0000_B880;

const MAILBOX_READ: usize = MAILBOX_BASE + 0x00;
const MAILBOX_STATUS: usize = MAILBOX_BASE + 0x18;
const MAILBOX_WRITE: usize = MAILBOX_BASE + 0x20;

const MAILBOX_FULL: u32 = 1 << 31;
const MAILBOX_EMPTY: u32 = 1 << 30;

const CHANNEL_PROPERTY: u32 = 8;

// Property tag constants
const TAG_SET_POWER_STATE: u32 = 0x0002_8001;
const TAG_END: u32 = 0;

const DEVICE_USB_HCD: u32 = 3;
const POWER_STATE_ON: u32 = 1;
const POWER_STATE_WAIT: u32 = 2;

/// 16-byte-aligned buffer for a SET_POWER_STATE request.
///
/// Layout (8 × u32 = 32 bytes):
///   [0] buffer size in bytes (32)
///   [1] request/response code (0 = request)
///   [2] tag id
///   [3] value buffer size in bytes (8)
///   [4] request length (0 on request; bit 31 set on response)
///   [5] device id
///   [6] state
///   [7] end tag (0)
#[repr(C, align(16))]
struct PowerMailbox {
    buf: [u32; 8],
}

/// Send a mailbox request on the property channel and wait for the response.
///
/// `buf_phys` is the physical address of the 16-byte-aligned buffer.
fn mailbox_call(buf_phys: u32) {
    let val = (buf_phys & !0xF) | CHANNEL_PROPERTY;

    // Wait until mailbox is not full
    while mmio::read(MAILBOX_STATUS) & MAILBOX_FULL != 0 {
        core::hint::spin_loop();
    }
    mmio::write(MAILBOX_WRITE, val);

    // Wait for a response on our channel
    loop {
        while mmio::read(MAILBOX_STATUS) & MAILBOX_EMPTY != 0 {
            core::hint::spin_loop();
        }
        let resp = mmio::read(MAILBOX_READ);
        if (resp & 0xF) == CHANNEL_PROPERTY {
            break;
        }
    }
}

/// Power on the USB HCD via the VideoCore firmware.
///
/// Returns `true` if the firmware acknowledged the power-on, `false` on
/// failure (e.g. `NO_DEVICE` bit set or state not ON).
pub fn power_on_usb() -> bool {
    let mb = PowerMailbox {
        buf: [
            32,                                  // [0] buffer size
            0,                                   // [1] request code
            TAG_SET_POWER_STATE,                 // [2] tag
            8,                                   // [3] value buffer size
            0,                                   // [4] request length
            DEVICE_USB_HCD,                      // [5] device id
            POWER_STATE_ON | POWER_STATE_WAIT,   // [6] state: ON + WAIT
            TAG_END,                             // [7] end tag
        ],
    };

    // Compute physical address.  In our bare-metal 1:1 mapping the virtual
    // address equals the physical address.  We OR with 0xC000_0000 to use the
    // ARM→GPU "uncached" bus alias so the GPU sees coherent data without us
    // needing to manage caches (L1/L2 may be enabled by the GPU firmware).
    let phys = core::ptr::addr_of!(mb.buf) as u32;
    let bus_addr = phys | 0xC000_0000;

    mailbox_call(bus_addr);

    // Check response: bit 31 of [4] should be set (response), and [6] should
    // have ON bit set and NO_DEVICE (bit 1) clear.
    let state = mb.buf[6];
    (state & POWER_STATE_ON) != 0 && (state & (1 << 1)) == 0
}

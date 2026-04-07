# Pi Zero 2 FRAM Text Editor

Bare-metal Rust program for the Raspberry Pi Zero 2 W that exposes an MB85RC256V 32KB FRAM
as an editable text file over USB CDC serial, presented as a Nano-like terminal text editor.

## Hardware

| Component | Details |
| --------- | ------- |
| Board | Raspberry Pi Zero 2 W (BCM2710 / Cortex-A53) |
| FRAM | MB85RC256V, 32KB, I2C address 0x50 |
| I2C Pins | GPIO 2 (SDA1), GPIO 3 (SCL1) |
| USB | Pi's micro-USB OTG port → host PC |
| Debug | Mini UART on GPIO 14 (TX) / GPIO 15 (RX) at 115200 baud |

### Wiring

```text
Pi Zero 2 W          MB85RC256V
-----------          ----------
Pin 1 (3.3V)  ───── VCC
Pin 3 (GPIO2) ───── SDA
Pin 5 (GPIO3) ───── SCL
Pin 6 (GND)   ───── GND
                     WP ──── GND (write protect disabled)
                     A0 ──── GND  
                     A1 ──── GND  (I2C address = 0x50)
                     A2 ──── GND
```

## Building

### Prerequisites

- Rust nightly toolchain with `aarch64-unknown-none` target
- `llvm-tools` component (for `rust-objcopy`)

```bash
# Install if needed (rust-toolchain.toml handles this automatically)
rustup toolchain install nightly
rustup target add aarch64-unknown-none --toolchain nightly
rustup component add llvm-tools --toolchain nightly
```

### Build

```bash
cargo build --release

# Convert ELF to raw binary
rust-objcopy -O binary \
    target/aarch64-unknown-none/release/pi-zero-fram \
    kernel8.img
```

### Deploy to SD Card

1. Format an SD card as FAT32
2. Download Pi firmware files from [raspberrypi/firmware](https://github.com/raspberrypi/firmware/tree/master/boot):
   - `bootcode.bin`
   - `start.elf`  
   - `fixup.dat`
3. Copy to the SD card root:
   - The firmware files above
   - `boot/config.txt` from this repo
   - `kernel8.img` (the built binary)

## Usage

1. Insert the SD card into the Pi Zero 2 W
2. Connect the Pi's **micro-USB data port** (not power) to your PC
3. Power on the Pi (via the other micro-USB port, or a header)
4. A serial device appears on the host:
   - Linux: `/dev/ttyACM0`
   - macOS: `/dev/tty.usbmodem*`
   - Windows: `COM` port (may need CDC driver)
5. Connect with a terminal emulator:

```bash
# Linux
minicom -D /dev/ttyACM0
# or
picocom /dev/ttyACM0
# or
screen /dev/ttyACM0
```

6. The Nano-like editor appears showing the FRAM contents

### Editor Keys

| Key | Action |
| --- | ------ |
| Arrow keys | Navigate |
| Home / End | Beginning / end of line |
| PgUp / PgDn | Scroll by page |
| ^O | **Save** to FRAM |
| ^R | Reload from FRAM (confirm if unsaved) |
| ^L | Redraw screen |
| Backspace | Delete character before cursor |
| Delete | Delete character at cursor |
| Enter | Insert newline |
| Tab | Insert space |

### Debug Output

During development, connect a USB-UART adapter to GPIO 14 (TX) and GPIO 15 (RX)
at 115200 baud to see boot messages and diagnostic output.

## Architecture

```text
Host PC (terminal emulator)
    │
    │  USB CDC ACM (Full Speed, 12 Mbps)
    │
Pi Zero 2 W (bare metal AArch64)
    ├── DWC2 OTG Device Driver (usb-device UsbBus trait)
    ├── CDC ACM Serial (usbd-serial)
    ├── VT100 Terminal + Nano-like Editor
    ├── FRAM Driver (MB85RC256V @ I2C 0x50)
    └── BSC1 I2C Driver (GPIO 2/3)
```

### Project Structure

```text
src/
├── boot.s              # AArch64 boot: park cores 1-3, zero BSS, set stack, jump to Rust
├── main.rs             # Entry point, hardware init, editor main loop
├── mmio.rs             # Volatile MMIO read/write helpers
├── gpio.rs             # GPIO function select and pull-up/down config
├── uart.rs             # Mini UART debug output (GPIO 14/15)
├── timer.rs            # BCM2710 system timer (delays)
├── i2c.rs              # BSC1 I2C master driver
├── fram.rs             # MB85RC256V FRAM read/write
├── mailbox.rs          # VideoCore mailbox interface (USB power-on)
├── panic.rs            # Panic handler (prints to UART)
├── usb/
│   ├── mod.rs
│   └── dwc2.rs         # DWC2 USB OTG device driver implementing UsbBus
└── editor/
    ├── mod.rs
    ├── buffer.rs        # 32KB text buffer with cursor and edit operations
    ├── terminal.rs      # VT100/ANSI escape sequence abstraction
    ├── input.rs         # Keyboard input parser (escape sequences → actions)
    └── render.rs        # Screen renderer (title bar, text area, status, help)
```

## Technical Details

- **Peripheral base**: 0x3F000000 (BCM2710/BCM2837)
- **Kernel load address**: 0x80000
- **I2C clock**: 400 kHz (BSC1 at 0x3F804000)
- **USB**: DWC2 OTG at 0x3F980000, Full Speed (12 Mbps)
- **No allocator**: All memory is statically allocated
- **No interrupts**: Polled I/O throughout (simplifies bare-metal code)
- **FRAM writes**: Buffered in RAM, flushed to FRAM on Ctrl+S
- **VID/PID**: 0x1209:0x0001 (pid.codes test PID)

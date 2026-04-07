# Boot Files

Copy these files to the root of a FAT32-formatted SD card:

1. **From this directory:**
   - `config.txt`

2. **From the [Raspberry Pi firmware repository](https://github.com/raspberrypi/firmware/tree/master/boot):**
   - `bootcode.bin`
   - `start.elf`
   - `fixup.dat`

3. **Built binary (after `cargo build --release`):**
   - Copy `target/aarch64-unknown-none/release/pi-zero-fram` as `kernel8.img`

   ```bash
   # From the project root:
   rust-objcopy -O binary target/aarch64-unknown-none/release/pi-zero-fram kernel8.img
   ```

## SD Card Layout

```text
/
├── bootcode.bin    (from RPi firmware)
├── start.elf       (from RPi firmware)
├── fixup.dat       (from RPi firmware)
├── config.txt      (from this directory)
└── kernel8.img     (your built binary)
```

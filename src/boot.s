// AArch64 bare-metal boot for Raspberry Pi Zero 2 W (BCM2710 / Cortex-A53)
// Loaded at 0x80000 by GPU firmware. All 4 cores start here;
// we park cores 1-3 and let core 0 proceed to Rust.

.section .text._start
.global _start

_start:
    // Read core ID from MPIDR_EL1
    mrs     x0, mpidr_el1
    and     x0, x0, #0x3           // Mask out Aff0 (core number)
    cbz     x0, core0_boot         // Core 0 continues

    // Cores 1-3: park in a low-power WFE loop
park:
    wfe
    b       park

core0_boot:
    // Zero the BSS section
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
bss_loop:
    cmp     x0, x1
    b.ge    bss_done
    str     xzr, [x0], #8
    b       bss_loop
bss_done:

    // Set up stack pointer: place it well above the kernel image.
    // Kernel starts at 0x80000; code + data + BSS (including 32KB TextBuffer)
    // fit well within 256KB.  0x200000 gives ~1.5MB of stack space.
    ldr     x0, =0x200000
    mov     sp, x0

    // Jump to Rust entry point
    bl      kernel_main

    // If kernel_main returns, halt
halt:
    wfe
    b       halt

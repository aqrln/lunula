//! # Booting Lunula
//!
//! The kernel is linked at the physical address 0x80200000 as a safe default for
//! QEMU with OpenSBI firmware. A custom bootloader for bare metal can load it at a
//! different address without re-linking the kernel (uhh, in theory at least).
//!
//! Requirements:
//!
//! 1. The bootloader should load the kernel as low as possible in RAM. The area
//!    from the start of the RAM until the load address of the kernel is assumed
//!    to contain the SBI firmware and will not be used by the operating system.
//!
//! 2. SBI environment must be available. SBI must transfer control to the kernel
//!    in S-mode with hart id in a0 and DTB addresss in a1.
//!
//! 2. Only single-hart systems are supported right now. Starting Lunula on a multi-hart
//!    system is currently undefined behavior. The boot trampoline does not yet
//!    check the hart id in a0 and has no protection against it.

use core::arch::naked_asm;

/// Lunula boot entrypoint. Stackless, position-independent boot trampoline that initializes
/// the CPU and memory to the expected state and maps the kernel to the upper canonical
/// half using the temporary page tables.
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.init")]
extern "C" fn _start() -> ! {
    naked_asm!(
        // Load the DTB size while we're still in Bare addressing mode.
        "lwu a2, 4(a1)",

        // Store the real physical address of the beginning of the kernel
        // so we can translate the physical addresses later in Rust if we're
        // loaded elsewhere than __link_base_addr. All memory accesses in the
        // assembly boot trampoline are either PC-relative or reference virtual
        // absolute addresses in the upper canonical half, so they need no translation.
        "la a3, __link_base_addr",
        "la a4, __kernel_start_phys",

        // Clear the boot page tables
        "la t0, __init_bss_start",
        "la t1, __init_bss_end",
        "1:",
        "bgeu t0, t1, 2f",
        "sd zero, 0(t0)",
        "addi t0, t0, 8",
        "j 1b",
        "2:",

        // Identity map the boot trampoline page.
        "srli t0, a3, 30",
        "andi t0, t0, 0x1ff", // t0 = vpn2(a3)
        "la t1, __init_page_table_l2",
        "li t3, 8",
        "mul t0, t0, t3",
        "add t0, t0, t1",
        "la t1, __init_page_table_l1_trampoline",
        "mv t4, t1",
        "srli t4, t4, 12", // t4 = ppn(t1)
        "slli t4, t4, 10", // ppn to pte
        "ori t4, t4, 1", // flags: V
        "sd t4, 0(t0)",
        "srli t0, a3, 21",
        "andi t0, t0, 0x1ff", // t0 = vpn1(a3)
        "mul t0, t0, t3",
        "add t0, t0, t1",
        "la t1, __init_page_table_l0_trampoline",
        "mv t4, t1",
        "srli t4, t4, 12", // t4 = ppn(t1)
        "slli t4, t4, 10", // ppn to pte
        "ori t4, t4, 1", // flags: V
        "sd t4, 0(t0)",
        "srli t0, a3, 12",
        "andi t0, t0, 0x1ff", // t0 = vpn0(a3)
        "mul t0, t0, t3",
        "add t0, t0, t1",
        "srli t1, a3, 12", // t1 = ppn(a3)
        "slli t1, t1, 10", // ppn to pte
        "ori t1, t1, 11", // flags: VRX
        "sd t1, 0(t0)",

        // Map the kernel up above.
        // Kernel's virtual base address must be aligned to 2 MB
        // and kernel size must not exceed 2 MB.
        // We map individual 4K pages and not a single 2M page because
        // the physical address might not be aligned to 2M.
        "ld t2, __kernel_start_ptr",
        "srli t0, t2, 30",
        "andi t0, t0, 0x1ff", // t0 = vpn2(t2)
        "mul t0, t0, t3",
        "la t1, __init_page_table_l2",
        "add t0, t0, t1",
        "la t1, __init_page_table_l1_kernel",
        "mv t4, t1",
        "srli t4, t4, 12", // t4 = ppn(t1)
        "slli t4, t4, 10", // ppn to pte
        "ori t4, t4, 1", // flags: V
        "sd t4, 0(t0)",
        "srli t0, t2, 21",
        "andi t0, t0, 0x1ff", // t0 = vpn1(t2)
        "mul t0, t0, t3",
        "add t0, t0, t1",
        "la t1, __init_page_table_l0_kernel",
        "mv t4, t1",
        "srli t4, t4, 12", // t4 = ppn(t1)
        "slli t4, t4, 10", // ppn to pte
        "ori t4, t4, 1", // flags: V
        "sd t4, 0(t0)",
        "la t0, __kernel_start_phys",
        "srli t0, t0, 12",
        "addi t2, t0, 512",
        "3:", // loop over PTEs, t1 = &pte, t0 = ppn, t2 = max_ppn + 1
        "bgeu t0, t2, 4f",
        "slli t3, t0, 10",
        "ori t3, t3, 0xf", // flags: VRWX
        "sd t3, 0(t1)",
        "addi t0, t0, 1",
        "addi t1, t1, 8",
        "j 3b",
        "4:",

        // Enable Sv39 virtual memory
        "sfence.vma zero, zero",
        "la t0, __init_page_table_l2",
        "srli t0, t0, 12",
        "li t1, 8",
        "slli t1, t1, 60",
        "or t0, t0, t1",
        "csrw satp, t0",

        // Initialize gp for relative addressing of small data
        ".option push",
        ".option norelax",
        "ld gp, __global_pointer_ptr",
        ".option pop",

        // Initialize stack pointer
        "ld sp, __stack_top_ptr",

        // Clear .bss
        "ld t0, __bss_start_ptr",
        "ld t1, __bss_end_ptr",
        "5:",
        "bgeu t0, t1, 6f",
        "sd zero, 0(t0)",
        "addi t0, t0, 8",
        "j 5b",
        "6:",

        // Enable floating-point operations
        "li t0, 0x6000",
        "csrc sstatus, t0",
        "li t0, 0x2000",
        "csrs sstatus, t0",
        "csrw fcsr, zero",

        // Call main with a0 and a1 set by SBI and a2-a4 by us
        "ld t0, __main_ptr",
        "jr t0",

        // Pointers to far symbols that can't be addressed relative to PC
        ".balign 8",
        "__kernel_start_ptr: .dword __kernel_start",
        "__global_pointer_ptr: .dword __global_pointer$",
        "__stack_top_ptr: .dword __stack_top",
        "__bss_start_ptr: .dword __bss_start",
        "__bss_end_ptr: .dword __bss_end",
        "__main_ptr: .dword {main}",

        main = sym crate::main,
    )
}

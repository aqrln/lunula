#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![feature(debug_closure_helpers)]
#![feature(ptr_alignment_type)]
#![cfg_attr(test, feature(const_type_name))]
#![test_runner(crate::test::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use device_tree_parser::DeviceTreeParser;
use embedded_alloc::TlsfHeap;

use crate::mmu::{
    MemoryManager, PagePermissions,
    addr::{AddressRange, PageType},
};

mod boot;
mod console;
mod mmu;
mod shutdown;
#[cfg(test)]
mod test;

#[global_allocator]
static HEAP: TlsfHeap = TlsfHeap::empty();

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static __text_start: u8;
    static __rodata_start: u8;
    static __data_start: u8;
    static __stack_protector: u8;
    static __stack_bottom: u8;
    static __stack_top: u8;
}

static LOGO: &str = r#"
 _                   _
| |_   _ _ __  _   _| | __ _
| | | | | '_ \| | | | |/ _` |
| | |_| | | | | |_| | | (_| |
|_|\__,_|_| |_|\__,_|_|\__,_|

"#;

extern "C" fn main(
    hart_id: usize,
    dtb_address: usize,
    dtb_size_be: usize,
    load_address: usize,
    kernel_start_phys: usize,
) -> ! {
    let dtb_size = u32::from_be(dtb_size_be as _) as usize;

    println!("{LOGO}");
    println!(
        "starting lunula on hart {hart_id}, dtb_address={dtb_address:#x}, dtb_size={dtb_size:#x}, load_address={load_address:#x}, kernel_start_phys={kernel_start_phys:#x}"
    );

    unsafe {
        embedded_alloc::init!(HEAP, 1024 * 1024);
    }

    let mut mm = MemoryManager::new_with_global_mappings(
        (&raw const __kernel_start).expose_provenance() - kernel_start_phys,
        (&raw const __kernel_end).into(),
        vec![
            (
                (&raw const __text_start..&raw const __rodata_start).into(),
                PagePermissions::READ | PagePermissions::EXECUTE,
            ),
            (
                (&raw const __rodata_start..&raw const __data_start).into(),
                PagePermissions::READ,
            ),
            (
                (&raw const __data_start..&raw const __stack_protector).into(),
                PagePermissions::READ | PagePermissions::WRITE,
            ),
            (
                (&raw const __stack_bottom..&raw const __stack_top).into(),
                PagePermissions::READ | PagePermissions::WRITE,
            ),
        ],
    )
    .expect("failed to map kernel address space");

    let dtb_address = {
        let dtb_page_addr = dtb_address & PageType::Small.mask();
        let dtb_page_offset = dtb_address & !PageType::Small.mask();
        let (dtb_range, update) = mm
            .map_kernel_private(
                AddressRange::from(dtb_page_addr..dtb_address.strict_add(dtb_size))
                    .with_aligned_end(PageType::Small),
                PageType::Small,
                PagePermissions::READ,
            )
            .expect("failed to map dtb");
        update.do_not_flush();
        dtb_range.start.add(dtb_page_offset)
    };

    unsafe { mm.activate_kernel_address_space() };

    let dtb_data = unsafe { core::slice::from_raw_parts(dtb_address.as_ptr(), dtb_size) };
    let dtp = DeviceTreeParser::new(dtb_data);

    let tree = dtp.parse_tree().expect("device tree must be valid");
    // println!("\nDevice tree:\n{tree}");

    for node in tree.iter_nodes() {
        if node.prop_string("device_type") == Some("memory") {
            let addrs = node
                .translate_reg_addresses(Some(&tree))
                .expect("register property of the memory device must be valid");
            for (addr, size) in addrs {
                let size_unit = match size {
                    x if x >= 1024 * 1024 * 1024 => format_args!("{} GB", x / 1024 / 1024 / 1024),
                    x if x >= 1024 * 1024 => format_args!("{} MB", x / 1024 / 1024),
                    x if x >= 1024 => format_args!("{} KB", x / 1024),
                    x => format_args!("{} B", x.clone()),
                };
                println!(
                    "{size_unit} of memory found at {addr:#016x}..{:#016x}",
                    addr + size
                );
            }
        }
    }

    for res in dtp
        .parse_memory_reservations()
        .expect("memory reservations must be valid")
    {
        println!(
            "reserved: {:#016x}..{:#016x} ({:#016x})",
            res.address,
            res.address + res.size,
            res.size
        );
    }

    shutdown::init(&tree, &mut mm).expect("global shutdown device not initialized");

    #[cfg(test)]
    test_main();

    loop {
        riscv::asm::wfi();
    }
}

#[panic_handler]
fn on_panic(info: &core::panic::PanicInfo) -> ! {
    riscv::interrupt::disable();

    match info.location() {
        Some(location) => println!("kernel panic at {}: {}", location, info.message()),
        None => println!("kernel panic: {}", info.message()),
    }

    shutdown::get().shutdown_failure();
}

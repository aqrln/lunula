use core::mem::Alignment;

use alloc::{boxed::Box, collections::btree_map::BTreeMap, vec::Vec};
use bitflags::bitflags;
use riscv::register::satp;

use crate::{
    mmu::{
        addr::{AddressRange, PageType, PhysicalAddr, VirtualAddr},
        pte::{InvalidPermissions, PteSlot, PteValue},
    },
    println,
};

pub mod addr;
pub mod pte;

#[repr(C, align(4096))]
struct PageTable {
    entries: [PteSlot; 512],
}

impl PageTable {
    fn new() -> Self {
        Self {
            entries: [const { PteSlot::new(PteValue::unmapped()) }; _],
        }
    }

    fn entry(&self, idx: usize) -> &PteSlot {
        &self.entries[idx]
    }

    fn physical_addr(&self, kernel_phys_to_virt_offset: usize) -> PhysicalAddr {
        let virt_addr = VirtualAddr::from(self.entries.as_ptr());
        AddressSpace::global_mapping_virt_to_phys(virt_addr, kernel_phys_to_virt_offset)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AddressSpaceId(u16);

impl AddressSpaceId {
    pub fn kernel() -> Self {
        Self(0)
    }

    fn get(self) -> u16 {
        self.0
    }
}

struct AddressSpace {
    root: Box<PageTable>,
    available_range: AddressRange<VirtualAddr>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MapError {
    #[error("virtual address {0} is not aligned to page size")]
    VirtualUnaligned(VirtualAddr),
    #[error("physical address {0} is not aligned to page size")]
    PhysicalUnaligned(PhysicalAddr),
    #[error(
        "virtual range {0} contains {vsize} bytes but physical range {1} contains {psize} bytes",
        vsize = .0.size(),
        psize = .1.size())
    ]
    MismatchedLength(AddressRange<VirtualAddr>, AddressRange<PhysicalAddr>),
    #[error("range {0} is already mapped in this address space")]
    Conflict(AddressRange<VirtualAddr>),
    #[error("invalid page permissions: {0}")]
    InvalidPermissions(#[from] InvalidPermissions),
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct PagePermissions: u8 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
    }
}

impl AddressSpace {
    fn new(range: AddressRange<VirtualAddr>) -> Self {
        Self {
            available_range: range,
            root: Box::new(PageTable::new()),
        }
    }

    fn new_with_global_mappings(
        range: AddressRange<VirtualAddr>,
        kernel_phys_to_virt_offset: usize,
        mappings: &[(AddressRange<VirtualAddr>, PagePermissions)],
    ) -> Result<Self, MapError> {
        let mut space = Self::new(range);
        for &(range, permissions) in mappings {
            // We don't need to flush these pages because the corresponding address space
            // has not been activated by writing to satp register yet. We will flush the
            // whole address space when switching to it
            space
                .map_global_kernel_range(range, permissions, kernel_phys_to_virt_offset)?
                .do_not_flush();
        }
        Ok(space)
    }

    fn map_global_kernel_range(
        &mut self,
        range: AddressRange<VirtualAddr>,
        permissions: PagePermissions,
        kernel_phys_to_virt_offset: usize,
    ) -> Result<PageTableUpdate, MapError> {
        self.map_range(
            range,
            range.map(|addr| Self::global_mapping_virt_to_phys(addr, kernel_phys_to_virt_offset)),
            permissions,
            kernel_phys_to_virt_offset,
        )
    }

    /// Maps the specified range using an optimal number of pages of automatically chosen size.
    ///
    /// If an error occurs, the partially applied updates are not reverted, and also may not
    /// be visible until TLB is flushed.
    fn map_range(
        &mut self,
        mut virtual_range: AddressRange<VirtualAddr>,
        mut physical_range: AddressRange<PhysicalAddr>,
        permissions: PagePermissions,
        kernel_phys_to_virt_offset: usize,
    ) -> Result<PageTableUpdate, MapError> {
        println!("mapping range {virtual_range} to {physical_range}");

        for addr in [virtual_range.start, virtual_range.end] {
            if !addr.is_aligned(PageType::Small) {
                return Err(MapError::VirtualUnaligned(addr));
            }
        }

        for addr in [physical_range.start, physical_range.end] {
            if !addr.is_aligned(PageType::Small) {
                return Err(MapError::PhysicalUnaligned(addr));
            }
        }

        if virtual_range.size() != physical_range.size() {
            return Err(MapError::MismatchedLength(virtual_range, physical_range));
        }

        let mut update = PageTableUpdate::default();

        for page_type in [PageType::Huge, PageType::Large, PageType::Small] {
            while virtual_range.start.is_aligned(page_type)
                && physical_range.start.is_aligned(page_type)
                && virtual_range.size() >= page_type.size()
                && physical_range.size() >= page_type.size()
            {
                update += self.map_page(
                    page_type,
                    virtual_range.start,
                    physical_range.start,
                    permissions,
                    kernel_phys_to_virt_offset,
                )?;
                let offset = page_type.size() as isize;
                virtual_range.start = virtual_range.start.offset(offset);
                physical_range.start = physical_range.start.offset(offset);
            }
        }

        assert!(virtual_range.size() == 0);
        assert!(physical_range.size() == 0);

        Ok(update)
    }

    /// Maps a single page without alignment checks.
    ///
    /// Although the page table has interior mutability and an exclusive reference
    /// is not required for mutation, it is important for correctness: it statically
    /// proves that this method cannot be used concurrently and nothing can mutate
    /// the page table entries at the same time (other than the CPU setting the
    /// dirty/accessed flags).
    fn map_page(
        &mut self,
        page_type: PageType,
        virtual_addr: VirtualAddr,
        physical_addr: PhysicalAddr,
        permissions: PagePermissions,
        kernel_phys_to_virt_offset: usize,
    ) -> Result<PageTableUpdate, MapError> {
        let mut update = PageTableUpdate::One(virtual_addr);

        let map_leaf_pte = |table: &PageTable, pte_index| {
            let pte = table.entry(pte_index);
            if pte.load().is_valid() {
                Err(MapError::Conflict(AddressRange::page(
                    virtual_addr,
                    page_type,
                )))
            } else {
                pte.store(PteValue::leaf(physical_addr, permissions)?);
                Ok(())
            }
        };

        let get_or_create_page_table =
            |parent_table: &PageTable, parent_pte_index, update: &mut PageTableUpdate| {
                let pte = parent_table.entry(parent_pte_index);
                let pte_val = pte.load();

                if !pte_val.is_valid() {
                    *update = PageTableUpdate::Many;
                    let next_table = Box::leak(Box::new(PageTable::new())) as &_;
                    let addr = VirtualAddr::from(next_table as *const _);
                    let phys_addr =
                        Self::global_mapping_virt_to_phys(addr, kernel_phys_to_virt_offset);
                    pte.store(PteValue::non_leaf(phys_addr));
                    Ok(next_table)
                } else if pte_val.is_leaf() {
                    Err(MapError::Conflict(AddressRange::page(
                        virtual_addr,
                        page_type,
                    )))
                } else {
                    let phys_addr = PhysicalAddr::from_ppn(pte_val.ppn());
                    let addr =
                        Self::global_mapping_phys_to_virt(phys_addr, kernel_phys_to_virt_offset);
                    let ptr = addr.as_ptr().cast();
                    Ok(unsafe { &*ptr })
                }
            };

        let map_indirect_pte = |parent_table, parent_pte_index, leaf_pte_index, update| {
            map_leaf_pte(
                get_or_create_page_table(parent_table, parent_pte_index, update)?,
                leaf_pte_index,
            )
        };

        match page_type {
            PageType::Huge => map_leaf_pte(&self.root, virtual_addr.vpn2())?,
            PageType::Large => map_indirect_pte(
                &self.root,
                virtual_addr.vpn2(),
                virtual_addr.vpn1(),
                &mut update,
            )?,
            PageType::Small => {
                let next_table =
                    get_or_create_page_table(&self.root, virtual_addr.vpn2(), &mut update)?;
                map_indirect_pte(
                    next_table,
                    virtual_addr.vpn1(),
                    virtual_addr.vpn0(),
                    &mut update,
                )?
            }
        }

        Ok(update)
    }

    fn allocate_addresses(
        &mut self,
        size: usize,
        alignment: Alignment,
    ) -> AddressRange<VirtualAddr> {
        let start = VirtualAddr::new(
            self.available_range
                .start
                .get()
                .next_multiple_of(alignment.max(PageType::Small.alignment()).as_usize()),
        );

        let end = start.add(size.next_multiple_of(PageType::Small.alignment().as_usize()));
        if end > self.available_range.end {
            panic!(
                "ran out of virtual addresses (allocation size={size}, alignment={alignment:?})"
            );
        }

        self.available_range.start = end;

        (start..end).into()
    }

    fn global_mapping_virt_to_phys(
        addr: VirtualAddr,
        kernel_phys_to_virt_offset: usize,
    ) -> PhysicalAddr {
        PhysicalAddr::new(addr.get().strict_sub(kernel_phys_to_virt_offset) as u64)
    }

    fn global_mapping_phys_to_virt(
        addr: PhysicalAddr,
        kernel_phys_to_virt_offset: usize,
    ) -> VirtualAddr {
        VirtualAddr::new(
            (addr.get() as usize)
                .checked_add(kernel_phys_to_virt_offset)
                .expect("kernel_virt_to_phys_offset addition to a valid kernel physical address should not overflow"),
        )
    }
}

#[derive(Debug, Default)]
#[must_use = "page table updates should be flushed and not discarded"]
pub enum PageTableUpdate {
    /// No page table updates were performed.
    #[default]
    None,
    /// One page is affected: a single leaf PTE was created or updated.
    One(VirtualAddr),
    /// Many pages are affected: a non-leaf PTE or multiple leaf PTEs were created or updated.
    Many,
}

impl PageTableUpdate {
    pub fn flush(self, asid: AddressSpaceId) {
        match self {
            Self::None => {}
            Self::One(addr) => MemoryManager::sync_page(asid, addr),
            Self::Many => MemoryManager::sync_address_space(asid),
        }
    }

    pub fn do_not_flush(self) {}
}

impl core::ops::Add<PageTableUpdate> for PageTableUpdate {
    type Output = Self;

    fn add(self, other: Self) -> Self::Output {
        match (self, other) {
            (Self::None, u) => u,
            (u, Self::None) => u,
            _ => Self::Many,
        }
    }
}

impl core::ops::AddAssign<PageTableUpdate> for PageTableUpdate {
    fn add_assign(&mut self, rhs: PageTableUpdate) {
        *self = core::mem::take(self) + rhs
    }
}

pub struct MemoryManager {
    address_spaces: BTreeMap<AddressSpaceId, AddressSpace>,
    kernel_phys_to_virt_offset: usize,
    _global_mappings: Vec<(AddressRange<VirtualAddr>, PagePermissions)>,
}

impl MemoryManager {
    pub fn new_with_global_mappings(
        kernel_phys_to_virt_offset: usize,
        kernel_end: VirtualAddr,
        global_mappings: Vec<(AddressRange<VirtualAddr>, PagePermissions)>,
    ) -> Result<Self, MapError> {
        let mut address_spaces = BTreeMap::new();
        address_spaces.insert(
            AddressSpaceId::kernel(),
            AddressSpace::new_with_global_mappings(
                (kernel_end..VirtualAddr::new(usize::MAX)).into(),
                kernel_phys_to_virt_offset,
                &global_mappings,
            )?,
        );
        Ok(Self {
            address_spaces,
            kernel_phys_to_virt_offset,
            _global_mappings: global_mappings,
        })
    }

    pub fn map_kernel_private(
        &mut self,
        range: AddressRange<PhysicalAddr>,
        min_page_type: PageType,
        permissions: PagePermissions,
    ) -> Result<(AddressRange<VirtualAddr>, PageTableUpdate), MapError> {
        let kernel_phys_to_virt_offset = self.kernel_phys_to_virt_offset;
        let addr_space = self.kernel_address_space_mut();
        let virt = addr_space.allocate_addresses(range.size(), min_page_type.alignment());
        let update = addr_space.map_range(virt, range, permissions, kernel_phys_to_virt_offset)?;
        Ok((virt, update))
    }

    fn kernel_address_space(&self) -> &AddressSpace {
        self.address_spaces
            .get(&AddressSpaceId::kernel())
            .expect("kernel address space should exist")
    }

    fn kernel_address_space_mut(&mut self) -> &mut AddressSpace {
        self.address_spaces
            .get_mut(&AddressSpaceId::kernel())
            .expect("kernel address space should exist")
    }

    pub fn sync_page(asid: AddressSpaceId, addr: VirtualAddr) {
        riscv::asm::sfence_vma(asid.get() as _, addr.get());
    }

    pub fn sync_address_space(asid: AddressSpaceId) {
        // riscv crate provides wrappers for the `sfence.vma x0, x0` and `sfence.vma rs1, rs2`
        // variants of the instruction but not for `sfence.vma x0, rs2` or `sfence.vma rs1, x0`.
        unsafe {
            core::arch::asm!(
                "sfence.vma x0, {asid}",
                asid = in(reg) asid.get(),
                options(nostack)
            )
        };
    }

    pub unsafe fn activate_kernel_address_space(&self) {
        unsafe {
            satp::set(
                satp::Mode::Sv39,
                AddressSpaceId::kernel().get() as _,
                self.kernel_address_space()
                    .root
                    .physical_addr(self.kernel_phys_to_virt_offset)
                    .ppn() as _,
            )
        };
        Self::sync_address_space(AddressSpaceId::kernel());
    }
}

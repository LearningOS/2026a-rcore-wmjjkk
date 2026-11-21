//! Memory management implementation
//!
//! SV39 page-based virtual-memory architecture for RV64 systems, and
//! everything about memory management, like frame allocator, page table,
//! map area and memory set, is implemented here.
//!
//! Every task or process has a memory_set to control its virtual memory.
mod address;
mod frame_allocator;
mod heap_allocator;
mod memory_set;
mod page_table;

use crate::config::PAGE_SIZE;
pub use address::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use address::{StepByOne, VPNRange};
pub use frame_allocator::{frame_alloc, FrameTracker};
pub use memory_set::remap_test;
pub use memory_set::{MapPermission, MemorySet, KERNEL_SPACE};
pub use page_table::{translated_byte_buffer, translated_refmut, translated_str, PageTableEntry};
use page_table::{PTEFlags, PageTable};

/// Copy bytes into a user buffer after validating the full destination range.
/// The range may span multiple pages.
pub fn copy_to_user(token: usize, dst: *mut u8, src: &[u8]) -> bool {
    let start = dst as usize;
    let end = match start.checked_add(src.len()) {
        Some(end) => end,
        None => return false,
    };
    if src.is_empty() {
        return true;
    }

    let page_table = PageTable::from_token(token);

    // Validate every page first so a bad later page cannot cause a partial write.
    let mut addr = start;
    while addr < end {
        let va = VirtAddr::from(addr);
        if usize::from(va) != addr {
            return false;
        }
        let pte = match page_table.translate(va.floor()) {
            Some(pte) => pte,
            None => return false,
        };
        if !pte.is_valid() || !pte.flags().contains(PTEFlags::U | PTEFlags::W) {
            return false;
        }
        addr = core::cmp::min(end, addr + PAGE_SIZE - va.page_offset());
    }

    let mut addr = start;
    let mut copied = 0;
    while addr < end {
        let va = VirtAddr::from(addr);
        let pte = page_table.translate(va.floor()).unwrap();
        let count = core::cmp::min(end - addr, PAGE_SIZE - va.page_offset());
        let page_bytes = pte.ppn().get_bytes_array();
        let offset = va.page_offset();
        page_bytes[offset..offset + count].copy_from_slice(&src[copied..copied + count]);
        addr += count;
        copied += count;
    }
    true
}
/// initiate heap allocator, frame allocator and kernel space
pub fn init() {
    heap_allocator::init_heap();
    frame_allocator::init_frame_allocator();
    KERNEL_SPACE.exclusive_access().activate();
}

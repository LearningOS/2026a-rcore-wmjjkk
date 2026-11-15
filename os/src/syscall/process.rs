//! Process management syscalls
use crate::config::PAGE_SIZE;
use crate::mm::{PageTable, PhysPageNum, PTEFlags, VirtAddr, MapPermission};
use crate::task::{current_user_token,change_program_brk, exit_current_and_run_next, suspend_current_and_run_next, get_syscall_count};
#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

fn user_byte_location (
    addr: usize,
    write: bool,
) -> Option<(PhysPageNum,usize)> {
    // 1.check thr original address before using the truncated value
    let va = VirtAddr::from(addr);
    if usize::from(va) != addr {
        return None;
    }
    // 2. look up this address in the current task's page table
    let page_table = PageTable::from_token(current_user_token());
    let pte = page_table.translate(va.floor())?;
    // 3.the mapping must be valid and accessible from user mode
    if !pte.is_valid() || !pte.flags().contains(PTEFlags::U) {
        return None;
    }
    // 4. check the requested access permission
    if write {
        if !pte.writable() {
            return None;
        }
    }else if !pte.readable() {
        return None;
    }
    Some((pte.ppn(),va.page_offset()))
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    // 先把两个字段转换为字节，避免直接解引用指针
    let start = ts as usize;
    let now_us = crate::timer::get_time_us();
    let sec = now_us / 1_000_000;
    let usec = now_us % 1_000_000;
    let word_size = core::mem::size_of::<usize>();
    let mut bytes = [0u8;2 * core::mem::size_of::<usize>()];
    bytes[..word_size].copy_from_slice(&sec.to_ne_bytes());
    bytes[word_size..].copy_from_slice(&usec.to_ne_bytes());
    // 检查地址加法是否溢出
    let end = match start.checked_add(bytes.len()) {
        Some(end) => end,
        None => return -1,
    };
    for addr in start..end {
        if user_byte_location(addr,true).is_none() {
            return -1;
        }
    }
    for(i,byte) in bytes.iter().enumerate(){
        match user_byte_location(start + i, true) {
            Some((ppn,offset)) => {
                ppn.get_bytes_array()[offset] = *byte;
            }
            None => return -1,
        }
    }
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    match _trace_request {
        0 => {
            match user_byte_location(_id,false){
                Some((ppn,offset)) => {
                    ppn.get_bytes_array()[offset] as isize
                }
                None => -1,
            }
        }
        1 => {
            match user_byte_location(_id, true) {
                Some((ppn,offset)) => {
                    ppn.get_bytes_array()[offset] = _data as u8;
                    0
                }
                None => -1,
            }
        }
        2 => get_syscall_count(_id),
        _ => -1,
    }
}

fn mmap_permission(prot: usize) -> Option<MapPermission> {
    if prot == 0 || (prot & !0x7) != 0 {
        return None;
    }
    let mut permission = MapPermission::U;
    if prot & 1 != 0 {
        permission |= MapPermission::R;
    }
    if prot & 2 != 0 {
        permission |= MapPermission::W;
    }
    if prot & 4 != 0 {
        permission |= MapPermission::X;
    }
    Some(permission)
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    const USER_VA_END: usize = 1usize << 38;
    if start % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return -1,
    };
    if start >= USER_VA_END || end > USER_VA_END {
        return -1;
    }
    let permission = match mmap_permission(prot){
        Some(permission) => permission,
        None => return -1,
    };
    if crate::task::mmap_current(VirtAddr::from(start),VirtAddr::from(end),permission){
        0
    }else{
        -1
    }
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    const USER_VA_END: usize = 1usize << 38;
    if start % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    let end = match start.checked_add(len){
        Some(end) => end,
        None => return -1,
    };
    if start >= USER_VA_END || end > USER_VA_END {
        return -1;
    }
    if crate::task::munmap_current(VirtAddr::from(start),VirtAddr::from(end)){
        0
    }else {
        -1
    }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

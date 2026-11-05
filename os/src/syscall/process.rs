//! Process management syscalls
use crate::config::PAGE_SIZE;
use crate::mm::{translated_byte_buffer_checked, MapPermission};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next, get_current_syscall_times,
    mmap_current, munmap_current, suspend_current_and_run_next,
};
use crate::timer::get_time_us;

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

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// Copy `len` bytes from the current task's memory at `ptr` into `dst`.
///
/// Return `false` if the range is not accessible from the current task.
fn read_from_user(ptr: *const u8, len: usize, dst: &mut [u8]) -> bool {
    let token = current_user_token();
    match translated_byte_buffer_checked(token, ptr, len, false) {
        Some(buffers) => {
            let mut copied = 0;
            for buffer in buffers {
                let n = buffer.len().min(dst.len() - copied);
                dst[copied..copied + n].copy_from_slice(&buffer[..n]);
                copied += n;
            }
            true
        }
        None => false,
    }
}

/// Copy `src` into the current task's memory at `ptr`.
///
/// Return `false` if the range is not writable by the current task.
fn write_to_user(ptr: *mut u8, src: &[u8]) -> bool {
    let token = current_user_token();
    match translated_byte_buffer_checked(token, ptr, src.len(), true) {
        Some(buffers) => {
            let mut copied = 0;
            for buffer in buffers {
                let n = buffer.len().min(src.len() - copied);
                buffer[..n].copy_from_slice(&src[copied..copied + n]);
                copied += n;
            }
            true
        }
        None => false,
    }
}

/// get time with second and microsecond
///
/// The `TimeVal` is written through the page table of the current task, so it
/// also works when the structure lies across two pages.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let time_val = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(&time_val as *const TimeVal as *const u8, core::mem::size_of::<TimeVal>())
    };
    if write_to_user(ts as *mut u8, bytes) {
        0
    } else {
        -1
    }
}

/// trace request: read one byte from the memory of the current task
const TRACE_READ: usize = 0;
/// trace request: write one byte into the memory of the current task
const TRACE_WRITE: usize = 1;
/// trace request: get how many times the current task has called a syscall
const TRACE_SYSCALL: usize = 2;

/// Trace the execution of the current task.
///
/// - `trace_request == 0`: `id` is an address, return the byte stored there;
/// - `trace_request == 1`: `id` is an address, write `data` into that byte;
/// - `trace_request == 2`: `id` is a syscall id, return how many times the
///   current task has called that syscall.
///
/// Return `-1` if the request is illegal, e.g. the address is not mapped in the
/// address space of the current task or is not readable / writable.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        TRACE_READ => {
            let mut byte = [0u8; 1];
            if read_from_user(id as *const u8, 1, &mut byte) {
                byte[0] as isize
            } else {
                -1
            }
        }
        TRACE_WRITE => {
            if write_to_user(id as *mut u8, &[data as u8]) {
                0
            } else {
                -1
            }
        }
        TRACE_SYSCALL => get_current_syscall_times(id) as isize,
        _ => -1,
    }
}

/// Convert the `port` given by the user into `MapPermission`.
///
/// The three lowest bits stand for readable, writable and executable, and a
/// page can always be accessed in user mode.
fn port_to_permission(port: usize) -> Option<MapPermission> {
    if port == 0 || port > 0b111 {
        return None;
    }
    let mut permission = MapPermission::U;
    if port & 0b001 != 0 {
        permission |= MapPermission::R;
    }
    if port & 0b010 != 0 {
        permission |= MapPermission::W;
    }
    if port & 0b100 != 0 {
        permission |= MapPermission::X;
    }
    Some(permission)
}

// YOUR JOB: Implement mmap.
/// Map `len` bytes at `start` for the current task.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    trace!("kernel: sys_mmap");
    if start % PAGE_SIZE != 0 || len == 0 {
        return -1;
    }
    match port_to_permission(port) {
        Some(permission) => {
            if mmap_current(start, len, permission) {
                0
            } else {
                -1
            }
        }
        None => -1,
    }
}

// YOUR JOB: Implement munmap.
/// Unmap `len` bytes at `start` of the current task.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if munmap_current(start, len) {
        0
    } else {
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

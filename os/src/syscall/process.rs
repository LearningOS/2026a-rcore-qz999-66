//! Process management syscalls
use crate::{
    config::MEMORY_END,
    task::{
        exit_current_and_run_next, get_current_syscall_times, suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

/// trace request: read one byte from the memory of the current task
const TRACE_READ: usize = 0;
/// trace request: write one byte into the memory of the current task
const TRACE_WRITE: usize = 1;
/// trace request: get how many times the current task has called a syscall
const TRACE_SYSCALL: usize = 2;

/// Whether `addr` is an address the current task is allowed to access.
///
/// In chapter 3 there is no address space yet: the kernel, the apps and the
/// user stacks of the apps all live in the same identity-mapped physical
/// memory starting from `0x8000_0000`, and a user stack is in fact a static
/// array in the kernel's BSS segment. Therefore every address in the physical
/// memory can be accessed by a task, while an address outside it (such as
/// `isize::MAX`) cannot.
fn is_valid_user_addr(addr: usize) -> bool {
    (0x8000_0000..MEMORY_END).contains(&addr)
}

/// Trace the execution of the current task.
///
/// - `trace_request == 0`: `id` is an address, return the byte stored there;
/// - `trace_request == 1`: `id` is an address, write `data` into that byte;
/// - `trace_request == 2`: `id` is a syscall id, return how many times the
///   current task has called that syscall.
///
/// Return `-1` if the request is illegal, e.g. the address does not belong to
/// the current task.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        TRACE_READ => {
            if is_valid_user_addr(id) {
                unsafe { core::ptr::read_volatile(id as *const u8) as isize }
            } else {
                -1
            }
        }
        TRACE_WRITE => {
            if is_valid_user_addr(id) {
                unsafe { core::ptr::write_volatile(id as *mut u8, data as u8) };
                0
            } else {
                -1
            }
        }
        TRACE_SYSCALL => get_current_syscall_times(id) as isize,
        _ => -1,
    }
}

//! Types related to task management

use super::TaskContext;
use crate::config::MAX_SYSCALL_NUM;

/// The task control block (TCB) of a task.
#[derive(Copy, Clone)]
pub struct TaskControlBlock {
    /// The task status in it's lifecycle
    pub task_status: TaskStatus,
    /// The task context
    pub task_cx: TaskContext,
    /// How many times each syscall has been called by this task.
    ///
    /// The syscall id is used as the index, so that `sys_trace` can report
    /// the statistics of the current task in constant time.
    pub syscall_times: [u32; MAX_SYSCALL_NUM],
}

impl TaskControlBlock {
    /// Count one more call of the syscall with id `syscall_id`.
    pub fn increase_syscall_times(&mut self, syscall_id: usize) {
        if syscall_id < MAX_SYSCALL_NUM {
            self.syscall_times[syscall_id] += 1;
        }
    }

    /// How many times the syscall with id `syscall_id` has been called.
    pub fn get_syscall_times(&self, syscall_id: usize) -> usize {
        if syscall_id < MAX_SYSCALL_NUM {
            self.syscall_times[syscall_id] as usize
        } else {
            0
        }
    }
}

/// The status of a task
#[derive(Copy, Clone, PartialEq)]
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Exited,
}

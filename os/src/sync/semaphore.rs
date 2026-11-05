//! Semaphore

use crate::sync::deadlock;
use crate::sync::UPSafeCell;
use crate::task::{
    block_current_and_run_next, current_task, current_tid, wakeup_task, TaskControlBlock,
};
use alloc::{collections::VecDeque, sync::Arc};

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let rid = self as *const _ as usize;
        deadlock::release_holder(rid, current_tid());
        let mut inner = self.inner.exclusive_access();
        inner.count += 1;
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                wakeup_task(task);
            }
        }
    }

    /// down operation of semaphore
    ///
    /// Return 0 if a unit is acquired, or `-0xdead` if waiting would deadlock
    /// (only possible when the deadlock detection is enabled).
    pub fn down(&self) -> isize {
        trace!("kernel: Semaphore::down");
        // the address of the semaphore is its identity in the wait-for graph
        let rid = self as *const _ as usize;
        let tid = current_tid();
        let mut inner = self.inner.exclusive_access();
        if inner.count <= 0 {
            if deadlock::check_deadlock(tid, rid) {
                return deadlock::DEADLOCK_RET;
            }
            inner.count -= 1;
            inner.wait_queue.push_back(current_task().unwrap());
            deadlock::set_waiting(tid, rid);
            drop(inner);
            block_current_and_run_next();
            deadlock::clear_waiting(tid);
        } else {
            inner.count -= 1;
            deadlock::add_holder(rid, tid);
        }
        0
    }
}

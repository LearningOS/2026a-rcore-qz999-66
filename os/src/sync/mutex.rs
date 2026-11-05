//! Mutex (spin-like and blocking(sleep))

use super::deadlock;
use super::UPSafeCell;
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, current_tid, wakeup_task};
use alloc::{collections::VecDeque, sync::Arc};

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    ///
    /// Return 0 if the mutex is acquired, or `-0xdead` if the lock would
    /// deadlock (only possible when the deadlock detection is enabled).
    fn lock(&self) -> isize;
    /// Unlock the mutex
    fn unlock(&self) -> isize;
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    locked: UPSafeCell<bool>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            locked: unsafe { UPSafeCell::new(false) },
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) -> isize {
        trace!("kernel: MutexSpin::lock");
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                continue;
            } else {
                *locked = true;
                return 0;
            }
        }
    }

    fn unlock(&self) -> isize {
        trace!("kernel: MutexSpin::unlock");
        let mut locked = self.locked.exclusive_access();
        *locked = false;
        0
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

pub struct MutexBlockingInner {
    locked: bool,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) -> isize {
        trace!("kernel: MutexBlocking::lock");
        // the address of the mutex is its identity in the wait-for graph
        let rid = self as *const _ as usize;
        let tid = current_tid();
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            if deadlock::check_deadlock(tid, rid) {
                return deadlock::DEADLOCK_RET;
            }
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            deadlock::set_waiting(tid, rid);
            drop(mutex_inner);
            block_current_and_run_next();
            deadlock::clear_waiting(tid);
        } else {
            mutex_inner.locked = true;
            deadlock::add_holder(rid, tid);
        }
        0
    }

    /// unlock the blocking mutex
    fn unlock(&self) -> isize {
        trace!("kernel: MutexBlocking::unlock");
        let rid = self as *const _ as usize;
        deadlock::release_holder(rid, current_tid());
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
        }
        0
    }
}

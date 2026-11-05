//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;

///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: Vec<Arc<TaskControlBlock>>,
}

/// A stride scheduler.
///
/// Every process has a `stride` recording how much it has run, and a `pass`
/// which is `BIG_STRIDE / priority`. Each time we schedule, the process with
/// the smallest stride is chosen, and its stride is increased by its pass.
/// Therefore the CPU time a process gets is proportional to its priority.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: Vec::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push(task);
    }
    /// Take the process with the smallest stride out of the ready queue,
    /// and increase its stride by its pass.
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        // The brute force scan is fast enough here: the number of runnable
        // processes is small.
        let mut chosen: Option<usize> = None;
        let mut min_stride = usize::MAX;
        for (index, task) in self.ready_queue.iter().enumerate() {
            let stride = task.inner_exclusive_access().stride;
            if stride < min_stride {
                min_stride = stride;
                chosen = Some(index);
            }
        }
        let index = chosen?;
        let task = self.ready_queue.remove(index);
        // Update the stride of the chosen process.
        {
            let mut inner = task.inner_exclusive_access();
            inner.stride += inner.pass;
        }
        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}

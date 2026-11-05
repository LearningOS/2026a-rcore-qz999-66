//! Deadlock detection for blocking mutexes and semaphores
//!
//! The detector maintains a "wait-for graph": every task that is blocked on a
//! resource is recorded together with the resource it waits for, and every
//! resource records which tasks hold it. When a task is about to block, we
//! compute the set of tasks that can never make progress, and report a
//! deadlock if every holder of the requested resource is in that set.
//!
//! A task is stuck if it is blocked on a resource whose holders are all stuck.
//! This is a greatest fixed point: at first every blocked task is a candidate,
//! and a candidate is dropped as soon as one of the holders of the resource it
//! waits for is not a candidate. A task that is blocked on a timer (or on
//! anything that is not part of the wait-for graph) is never stuck, because it
//! will be woken up by the kernel eventually.

use crate::sync::UPSafeCell;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::*;

/// The error code returned to the user when a deadlock is detected
pub const DEADLOCK_RET: isize = -0xdead;

/// The state of the detector
struct DeadlockDetector {
    /// whether the detection is enabled by the user
    enabled: bool,
    /// resource id -> ids of the tasks holding it (one entry per held unit)
    holders: BTreeMap<usize, Vec<usize>>,
    /// task id -> the id of the resource it is blocked on
    waiting: BTreeMap<usize, usize>,
}

impl DeadlockDetector {
    fn new() -> Self {
        Self {
            enabled: false,
            holders: BTreeMap::new(),
            waiting: BTreeMap::new(),
        }
    }

    /// The number of units of `rid` that are currently held.
    fn held_units(&self, rid: usize) -> usize {
        self.holders.get(&rid).map_or(0, |holders| holders.len())
    }

    /// Compute the tasks that can never make progress (the greatest fixed
    /// point described above).
    fn stuck_tasks(&self) -> Vec<usize> {
        let mut stuck: Vec<usize> = self.waiting.keys().copied().collect();
        loop {
            let mut changed = false;
            for tid in stuck.clone() {
                let rid = match self.waiting.get(&tid) {
                    Some(rid) => *rid,
                    None => {
                        stuck.retain(|t| *t != tid);
                        changed = true;
                        continue;
                    }
                };
                // nobody holds the resource, so the task would not block on
                // it, which means it is not really waiting for something held
                if self.held_units(rid) == 0 {
                    stuck.retain(|t| *t != tid);
                    changed = true;
                    continue;
                }
                let all_holders_stuck = self
                    .holders
                    .get(&rid)
                    .map(|holders| holders.iter().all(|holder| stuck.contains(holder)))
                    .unwrap_or(false);
                if !all_holders_stuck {
                    stuck.retain(|t| *t != tid);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        stuck
    }
}

lazy_static! {
    static ref DETECTOR: UPSafeCell<DeadlockDetector> =
        unsafe { UPSafeCell::new(DeadlockDetector::new()) };
}

/// Enable or disable the deadlock detection
pub fn enable(enabled: bool) {
    DETECTOR.exclusive_access().enabled = enabled;
}

/// Register that the task `tid` holds one more unit of the resource `rid`.
pub fn add_holder(rid: usize, tid: usize) {
    let mut detector = DETECTOR.exclusive_access();
    if !detector.enabled {
        return;
    }
    detector.holders.entry(rid).or_default().push(tid);
}

/// Register that one unit of the resource `rid` is released by `tid`.
///
/// The unit is given back either by its holder itself, or (for semaphores,
/// which are often released by a different task) by anybody else. In the
/// latter case we prefer to drop a holder that is still runnable, since a
/// blocked holder would only be released by somebody else anyway.
pub fn release_holder(rid: usize, tid: usize) {
    let mut detector = DETECTOR.exclusive_access();
    if !detector.enabled {
        return;
    }
    // compute the index to remove before borrowing the holder list mutably
    let waiting = detector.waiting.clone();
    if let Some(holders) = detector.holders.get_mut(&rid) {
        let index = holders
            .iter()
            .rposition(|holder| *holder == tid)
            .or_else(|| holders.iter().position(|h| !waiting.contains_key(h)))
            .or_else(|| holders.len().checked_sub(1));
        if let Some(index) = index {
            holders.remove(index);
        }
        if holders.is_empty() {
            detector.holders.remove(&rid);
        }
    }
}

/// Record that the task `tid` is blocked on the resource `rid`.
pub fn set_waiting(tid: usize, rid: usize) {
    let mut detector = DETECTOR.exclusive_access();
    if detector.enabled {
        detector.waiting.insert(tid, rid);
    }
}

/// Record that the task `tid` is not blocked any more.
pub fn clear_waiting(tid: usize) {
    let mut detector = DETECTOR.exclusive_access();
    detector.waiting.remove(&tid);
}

/// Check whether the task `tid` would deadlock if it blocks on the resource
/// `rid`. It must be called before the task actually blocks.
pub fn check_deadlock(tid: usize, rid: usize) -> bool {
    let mut detector = DETECTOR.exclusive_access();
    if !detector.enabled {
        return false;
    }
    // a resource already held by the requester can never be acquired again
    if let Some(holders) = detector.holders.get(&rid) {
        if holders.contains(&tid) {
            return true;
        }
    }
    // The requester itself is about to block, so it must be regarded as a
    // blocked holder during the detection; otherwise the very last task that
    // closes the cycle would not see itself in it.
    let previous = detector.waiting.insert(tid, rid);
    let holders = detector.holders.get(&rid).cloned().unwrap_or_default();
    let deadlock = if holders.is_empty() {
        false
    } else {
        let stuck = detector.stuck_tasks();
        holders.iter().all(|holder| stuck.contains(holder))
    };
    // restore the previous state, the caller sets it again if it blocks
    match previous {
        Some(old_rid) => {
            detector.waiting.insert(tid, old_rid);
        }
        None => {
            detector.waiting.remove(&tid);
        }
    }
    deadlock
}

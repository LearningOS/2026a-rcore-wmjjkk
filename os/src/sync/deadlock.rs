//! Deadlock detection based on the Banker's algorithm.

use alloc::vec;
use alloc::vec::Vec;

/// Per-process deadlock detector tracking both mutex and semaphore resources.
pub struct DeadlockDetector {
    enabled: bool,
    mutex_available: Vec<usize>,
    mutex_allocation: Vec<Vec<usize>>,
    mutex_need: Vec<Vec<usize>>,
    sem_available: Vec<usize>,
    sem_allocation: Vec<Vec<usize>>,
    sem_need: Vec<Vec<usize>>,
}

impl DeadlockDetector {
    /// Create an empty deadlock detector (disabled by default).
    pub fn new() -> Self {
        Self {
            enabled: false,
            mutex_available: Vec::new(),
            mutex_allocation: Vec::new(),
            mutex_need: Vec::new(),
            sem_available: Vec::new(),
            sem_allocation: Vec::new(),
            sem_need: Vec::new(),
        }
    }

    /// Enable or disable deadlock detection.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Ensure per-thread rows exist for `tid`.
    fn ensure_tid(&mut self, tid: usize) {
        let m_len = self.mutex_available.len();
        let s_len = self.sem_available.len();
        while self.mutex_allocation.len() <= tid {
            self.mutex_allocation.push(vec![0; m_len]);
            self.mutex_need.push(vec![0; m_len]);
        }
        while self.sem_allocation.len() <= tid {
            self.sem_allocation.push(vec![0; s_len]);
            self.sem_need.push(vec![0; s_len]);
        }
    }

    /// Register a newly created mutex with 1 available unit.
    pub fn add_mutex(&mut self, mutex_id: usize) {
        if self.mutex_available.len() <= mutex_id {
            let new_len = mutex_id + 1;
            self.mutex_available.resize(new_len, 0);
            for row in self.mutex_allocation.iter_mut() {
                row.resize(new_len, 0);
            }
            for row in self.mutex_need.iter_mut() {
                row.resize(new_len, 0);
            }
        }
        self.mutex_available[mutex_id] = 1;
        for row in self.mutex_allocation.iter_mut() {
            row[mutex_id] = 0;
        }
        for row in self.mutex_need.iter_mut() {
            row[mutex_id] = 0;
        }
    }

    /// Register a newly created semaphore with `res_count` available units.
    pub fn add_sem(&mut self, sem_id: usize, res_count: usize) {
        if self.sem_available.len() <= sem_id {
            let new_len = sem_id + 1;
            self.sem_available.resize(new_len, 0);
            for row in self.sem_allocation.iter_mut() {
                row.resize(new_len, 0);
            }
            for row in self.sem_need.iter_mut() {
                row.resize(new_len, 0);
            }
        }
        self.sem_available[sem_id] = res_count;
        for row in self.sem_allocation.iter_mut() {
            row[sem_id] = 0;
        }
        for row in self.sem_need.iter_mut() {
            row[sem_id] = 0;
        }
    }

    /// Run Banker's safety check across all threads, mutexes, and semaphores.
    /// Returns `true` if a deadlock is detected (no safe completion sequence exists).
    pub fn detect(&self) -> bool {
        let thread_num = self.mutex_allocation.len().max(self.sem_allocation.len());
        let mut work_mutex = self.mutex_available.clone();
        let mut work_sem = self.sem_available.clone();
        let mut finish = vec![false; thread_num];

        loop {
            let mut progressed = false;
            for tid in 0..thread_num {
                if finish[tid] {
                    continue;
                }
                let mutex_ok = self.mutex_need[tid]
                    .iter()
                    .enumerate()
                    .all(|(m, &need)| need <= work_mutex[m]);
                let sem_ok = self.sem_need[tid]
                    .iter()
                    .enumerate()
                    .all(|(s, &need)| need <= work_sem[s]);
                if mutex_ok && sem_ok {
                    finish[tid] = true;
                    for (m, &alloc) in self.mutex_allocation[tid].iter().enumerate() {
                        work_mutex[m] += alloc;
                    }
                    for (s, &alloc) in self.sem_allocation[tid].iter().enumerate() {
                        work_sem[s] += alloc;
                    }
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }

        finish.iter().any(|&done| !done)
    }

    /// Record a mutex lock request; if deadlock detection is enabled and triggers, roll back and return `false`.
    pub fn try_request_mutex(&mut self, tid: usize, mutex_id: usize) -> bool {
        self.ensure_tid(tid);
        self.mutex_need[tid][mutex_id] += 1;
        if self.enabled && self.detect() {
            self.mutex_need[tid][mutex_id] -= 1;
            return false;
        }
        true
    }

    /// Commit mutex acquisition once `mutex.lock()` returns.
    pub fn on_mutex_acquired(&mut self, tid: usize, mutex_id: usize) {
        self.ensure_tid(tid);
        self.mutex_need[tid][mutex_id] = self.mutex_need[tid][mutex_id].saturating_sub(1);
        self.mutex_allocation[tid][mutex_id] += 1;
        self.mutex_available[mutex_id] = self.mutex_available[mutex_id].saturating_sub(1);
    }

    /// Record mutex release on `mutex_unlock` or before sleeping in `condvar_wait`.
    pub fn on_mutex_released(&mut self, tid: usize, mutex_id: usize) {
        self.ensure_tid(tid);
        self.mutex_allocation[tid][mutex_id] =
            self.mutex_allocation[tid][mutex_id].saturating_sub(1);
        self.mutex_available[mutex_id] += 1;
    }

    /// Record mutex re-acquisition after `condvar.wait` returns.
    pub fn on_mutex_reacquired(&mut self, tid: usize, mutex_id: usize) {
        self.ensure_tid(tid);
        self.mutex_allocation[tid][mutex_id] += 1;
        self.mutex_available[mutex_id] = self.mutex_available[mutex_id].saturating_sub(1);
    }

    /// Record a semaphore down request; if deadlock detection is enabled and triggers, roll back and return `false`.
    pub fn try_request_sem(&mut self, tid: usize, sem_id: usize) -> bool {
        self.ensure_tid(tid);
        self.sem_need[tid][sem_id] += 1;
        if self.enabled && self.detect() {
            self.sem_need[tid][sem_id] -= 1;
            return false;
        }
        true
    }

    /// Commit semaphore acquisition once `sem.down()` returns.
    pub fn on_sem_acquired(&mut self, tid: usize, sem_id: usize) {
        self.ensure_tid(tid);
        self.sem_need[tid][sem_id] = self.sem_need[tid][sem_id].saturating_sub(1);
        self.sem_allocation[tid][sem_id] += 1;
        self.sem_available[sem_id] = self.sem_available[sem_id].saturating_sub(1);
    }

    /// Record semaphore release on `semaphore_up`.
    pub fn on_sem_released(&mut self, tid: usize, sem_id: usize) {
        self.ensure_tid(tid);
        self.sem_allocation[tid][sem_id] = self.sem_allocation[tid][sem_id].saturating_sub(1);
        self.sem_available[sem_id] += 1;
    }
}

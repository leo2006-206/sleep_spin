#![doc = include_str!("../README.md")]

// src/lib.rs

/// Comprehensive benchmark results across different thread counts and workloads.
#[doc = include_str!("../benches/bench_result.md")]
pub mod bench_result {}

use lock_api::{GuardSend, RawMutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::Duration;

/// Low-level raw spinlock primitive implementing [`lock_api::RawMutex`].
///
/// Direct usage of this type is rarely needed. For safe, general-purpose mutual exclusion,
/// use [`SleepSpinLock`] instead.
pub struct RawSleepSpinLock {
    flag: AtomicU32,
}

// Safety: Implement RawMutex correctly according to its invariants
unsafe impl RawMutex for RawSleepSpinLock {
    const INIT: Self = Self {
        flag: AtomicU32::new(0),
    };

    // GuardSend marks that the MutexGuard can be sent across threads
    type GuardMarker = GuardSend;

    fn lock(&self) {
        let mut counter = 0;
        while self.flag.load(Ordering::Relaxed) != 0 || self.flag.swap(1, Ordering::Acquire) != 0 {
            counter += 1;
            if counter == 8 {
                counter = 0;
                sleep(Duration::from_nanos(1));
            }

            // std::hit::spin_loop here will make the code slower
        }
    }

    fn try_lock(&self) -> bool {
        self.flag.swap(1, Ordering::Acquire) == 0
    }

    /// # Safety
    /// Must only be called if the lock is currently held by the calling thread.
    unsafe fn unlock(&self) {
        self.flag.store(0, Ordering::Release);
    }

    fn is_locked(&self) -> bool {
        self.flag.load(Ordering::Relaxed) != 0
    }
}

/// A mutual exclusion primitive utilizing bounded TTAS spinning and nanosleep fallback.
///
/// This is a type alias for [`lock_api::Mutex<RawSleepSpinLock, T>`].
///
/// # Example
/// ```rust
/// use sleep_spin::SleepSpinLock;
///
/// let lock = SleepSpinLock::new(42);
/// {
///     let mut guard = lock.lock();
///     *guard += 1;
/// }
/// assert_eq!(*lock.lock(), 43);
/// ```
pub type SleepSpinLock<T> = lock_api::Mutex<RawSleepSpinLock, T>;

/// An RAII implementation of a "scoped lock" of a [`SleepSpinLock`].
///
/// When this structure is dropped (falls out of scope), the lock will be unlocked.
pub type SleepSpinLockGuard<'a, T> = lock_api::MutexGuard<'a, RawSleepSpinLock, T>;

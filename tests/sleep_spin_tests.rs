// tests/concurrency_test.rs
use sleep_spin::SleepSpinLock;
use std::sync::Arc;
use std::thread;

#[test]
fn test_basic_lock_and_unlock() {
    let lock = SleepSpinLock::new(42);
    assert_eq!(*lock.lock(), 42);

    {
        let mut guard = lock.lock();
        *guard = 100;
    }

    assert_eq!(*lock.lock(), 100);
}

#[test]
fn test_try_lock() {
    let lock = SleepSpinLock::new(10);
    let guard = lock.try_lock();
    assert!(guard.is_some());

    // Second lock attempt while held should fail non-blockingly
    let second_attempt = lock.try_lock();
    assert!(second_attempt.is_none());

    drop(guard);
    assert!(lock.try_lock().is_some());
}

#[test]
fn test_heavy_concurrent_increment() {
    const THREADS: usize = 16;
    const INCREMENTS_PER_THREAD: usize = 20_000;

    let counter = Arc::new(SleepSpinLock::new(0u64));
    let mut handles = Vec::with_capacity(THREADS);

    for _ in 0..THREADS {
        let counter_clone = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..INCREMENTS_PER_THREAD {
                let mut guard = counter_clone.lock();
                *guard += 1;
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let final_val = *counter.lock();
    let expected = (THREADS * INCREMENTS_PER_THREAD) as u64;
    assert_eq!(
        final_val, expected,
        "Data race detected under high contention!"
    );
}

# SleepSpinLock

A fast spinlock implementation that yields via nanosleep when facing high contention, eliminating kernel futex overhead.

This implementation is compatible with the Rust `lock_api` crate.

## Background

The original design is based on concepts from C++ expert Fedor Pikus.

His talks:
* [Original Source Lightning Talk from CppNow 2022](https://youtu.be/rmGJc9PXpuE)
* [Advanced Talk that dives into the Double Checking Lock Pattern from CppNow 2026](https://youtu.be/UdKqfQ3a_sY)

## Why It Is Fast

1. **Cache-Friendly TTAS (Test-and-Test-and-Set):**
   * Instead of repeatedly issuing atomic writes (`swap`), threads probe the lock with read-only operations (`flag.load(Ordering::Relaxed)`).
   * This prevents invalidating the cache line on other cores until the lock is actually free.

2. **Avoiding Scheduler Lock-Holder Starvation:**
   * Tight spinloops can mislead operating system schedulers into prioritizing the spinning thread over the thread currently holding the lock.
   * Threads spin for a small, empirically tuned window (8 attempts) before yielding via `std::thread::sleep(Duration::from_nanos(1))`.
   * This prevents bus thrashing and OS context-switch storms while allowing the lock holder to finish its work.
   * Note: In benchmarks, adding `std::hint::spin_loop` inside the loop made the code slower.

## Key Features

* No **Fairness** (Unordered)
* No **Starvation Protection**
* No **Priority Inversion Safeguards**
* **Simple** and **Lightweight**
* Best suited for **short critical sections** and **high contention** in normal lock-based code.
* Performance on **long critical sections** or **low contention** can be improved using techniques such as:
  * Double-Checked Locking Pattern (DCLP)
  * Other lock-free patterns to minimize the critical section modification window.

DCLP in Fedor's words:
> Think of how you would write it in lock-free,
> and then do not write it in lock-free.


## Example: Short Critical Section with Normal Lock-Based Code

```rust
use sleep_spin::SleepSpinLock;
use std::sync::Arc;
use std::thread;
use std::hint::black_box;

const NUM_THREADS: usize = 8;
const NUM_WORKS: usize = 100_000;

let lock = Arc::new(SleepSpinLock::new(0));
let mut handles = vec![];

for _ in 0..NUM_THREADS {
    let lock_clone = Arc::clone(&lock);
    handles.push(thread::spawn(move || {
        for _ in 0..NUM_WORKS {
            let mut guard = lock_clone.lock();
            *guard += 1;
            black_box(*guard);
        }    
    }));
}

for handle in handles {
    handle.join().unwrap();
}

assert_eq!(*lock.lock(), NUM_THREADS * NUM_WORKS);

```

## Example: Long Critical Section with Double-Checked Locking Pattern (DCLP)

It is highly recommended to watch the talk [Double Checking Lock Pattern from CppNow 2026](https://youtu.be/UdKqfQ3a_sY).

Using `SleepSpinLock` with DCLP improves performance for longer critical sections:

1. Load the current atomic value into `temp`.
2. Compute the next value `next_val`.
3. Load and compare the current value with `temp`.
4. If equal, acquire the lock and verify against the current value again.
5. If still equal, store `next_val` into the atomic variable and break.

```rust
use sleep_spin::SleepSpinLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::hint::black_box;

const NUM_THREADS: usize = 4;
const NUM_WORKS: usize = 1_000;

let lock = Arc::new(SleepSpinLock::new(()));
let val = Arc::new(AtomicU64::new(0));
let mut handles = Vec::with_capacity(NUM_THREADS);

let task = |x: u64| {
    let mut state = x.wrapping_add(0x9E3779B97F4A7C15);
    for _ in 0..100 {
        state ^= state >> 30;
        state = state.wrapping_mul(0xBF58476D1CE4E5B9);
        state ^= state >> 27;
        state = state.wrapping_mul(0x94D049BB133111EB);
        state ^= state >> 31;
        black_box(state);
    }
    state
};

for _ in 0..NUM_THREADS {
    let lock_clone = Arc::clone(&lock);
    let val_clone = Arc::clone(&val);

    handles.push(thread::spawn(move || {
        for _ in 0..NUM_WORKS {
            loop {
                let temp = val_clone.load(Ordering::Relaxed);
                let next_val = task(temp);

                if temp == val_clone.load(Ordering::Relaxed) {
                    let _guard = lock_clone.lock();
                    if temp == val_clone.load(Ordering::Relaxed) {
                        val_clone.store(next_val, Ordering::Release);
                        black_box(next_val);
                        break;
                    }
                }
            }
        }
    }));
}

for handle in handles {
    handle.join().unwrap();
}

// Calculate deterministic sequential orbit
let mut expected = 0u64;
for _ in 0..(NUM_THREADS * NUM_WORKS) {
    expected = task(expected);
}

assert_eq!(val.load(Ordering::SeqCst), expected);

```

## Benchmarks (`benches/lock_bench.rs`)

Measures wall-clock completion time across different numbers of threads, workloads, and task complexities.

Use `run_bench(bench_mod: i32)` to run all benchmarks:

```rust, ignore
match bench_mod {
    1 => println!("Running Normal lock code bench"),
    2 => println!("Running DCLP bench"),
    3 => println!("Running DCLP bench with 64 aligned SleepSpinLock and payload"),
    _ => panic!("Invalid bench mode: choose 1 for normal, 2 for DCLP, or 3 for aligned DCLP"),
}

```

Or use `single_task()` to isolate a single benchmark for profiling.

Locks compared:

* `std::Mutex`
* `parking_lot::Mutex`
* `spin::Mutex`
* `SleepSpinLock`

Tasks:

```rust, ignore
let tasks: [(&str, fn(u64) -> u64); 3] = [
    // 1. SHORT (~1-2 CPU cycles): Basic increment
    ("Short (Add)", |x| x.wrapping_add(1)),
    // 2. MEDIUM (~40-80 CPU cycles): Float conversions, trig, and integer divisions
    ("Medium (Trig/Div)", |x| {
        let float_calc = ((x as f64).sin().cos().abs() * 10_000.0) as u64;
        float_calc ^ (x / 7).wrapping_add(1)
    }),
    // 3. HEAVY (~500-1000 CPU cycles): Multi-round hash loop
    ("Long (Hash Loop)", |x| {
        let mut state = x.wrapping_add(0x9E3779B97F4A7C15);
        for _ in 0..100 {
            state ^= state >> 30;
            state = state.wrapping_mul(0xBF58476D1CE4E5B9);
            state ^= state >> 27;
            state = state.wrapping_mul(0x94D049BB133111EB);
            state ^= state >> 31;
            black_box(state);
        }
        state
    }),
];

```

### Environment

* **CPU**: 13th Gen Intel(R) Core(TM) i9-13900HX (P-cores: 8, E-cores: 16, Threads: 32)
* **OS**: 7.0.0-30-generic #30~24.04.1-Ubuntu, x86_64 x86_64 x86_64 GNU/Linux
* **Rust**: 1.97.1 (8bab26f4f 2026-07-14)

* Note: `parking_lot::Mutex` provides fairness guarantees that are often crucial in general-purpose workloads.
* For the full benchmark tables on this machine, see [Benchmark Results](crate::bench_result).

**Normal Lock Benchmark — `SleepSpinLock` Speedup (32 Threads, 1M Ops)**

| Task Workload | vs `std::Mutex` | vs `parking_lot` | vs `spin::Mutex` |
| --- | --- | --- | --- |
| **Short (Add)** | **+654%** (7.5x faster) | **+5,535%** (56.4x faster) | **+1,145%** (12.5x faster)|
| **Medium (Trig/Div)** | **+261%** (3.6x faster)| **+1,005%** (11.1x faster)| **+157%** (2.6x faster)|
| **Long (Hash Loop)** | **+10.7%** (1.1x faster)| **+165%** (2.7x faster)| **-9.8%** (1.1x slower)|

---

**DCLP (Optimistic Lock) Benchmark — `SleepSpinLock` Speedup (32 Threads, 1M Ops)**

| Task Workload | vs `std::Mutex` | vs `parking_lot` | vs `spin::Mutex` |
| --- | --- | --- | --- |
| **Short (Add)** | **+1,196%** (13.0x faster)| **+9,374%** (94.7x faster)| **+778%** (8.8x faster)|
| **Medium (Trig/Div)** | **+385%** (4.9x faster)| **+1,565%** (16.7x faster)| **+464%** (5.6x faster)|
| **Long (Hash Loop)** | **+100%** (2.0x faster)| **+392%** (4.9x faster)| **+354%** (4.5x faster)|
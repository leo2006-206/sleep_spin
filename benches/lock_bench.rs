use std::hint::black_box;
use std::ops::DerefMut;
use std::sync::{
    Arc, Barrier, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Mutex as ParkingMutex, MutexGuard as ParkingGuard};
use sleep_spin::{SleepSpinLock, SleepSpinLockGuard};
use spin::{Mutex as SpinCrateMutex, MutexGuard as SpinGuard};

// 1. Common Trait to abstract over different Lock types
pub trait BenchmarkLock<T>: Send + Sync + 'static {
    /// Associated guard type tied to the lifetime of &self
    type Guard<'a>: DerefMut<Target = T>
    where
        Self: 'a,
        T: 'a;

    fn new(val: T) -> Self;

    /// Acquires the lock and returns the RAII guard directly
    fn lock(&self) -> Self::Guard<'_>;

    /// Default implementation calling .lock()
    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut *guard)
    }
}
// 1. std::sync::Mutex
impl<T: Send + 'static> BenchmarkLock<T> for Mutex<T> {
    type Guard<'a>
        = MutexGuard<'a, T>
    where
        T: 'a;

    fn new(val: T) -> Self {
        Mutex::new(val)
    }

    fn lock(&self) -> Self::Guard<'_> {
        self.lock().unwrap()
    }
}

// 2. parking_lot::Mutex
impl<T: Send + 'static> BenchmarkLock<T> for ParkingMutex<T> {
    type Guard<'a>
        = ParkingGuard<'a, T>
    where
        T: 'a;

    fn new(val: T) -> Self {
        ParkingMutex::new(val)
    }

    fn lock(&self) -> Self::Guard<'_> {
        self.lock()
    }
}

// 3. spin::Mutex
impl<T: Send + 'static> BenchmarkLock<T> for SpinCrateMutex<T> {
    type Guard<'a>
        = SpinGuard<'a, T>
    where
        T: 'a;

    fn new(val: T) -> Self {
        SpinCrateMutex::new(val)
    }

    fn lock(&self) -> Self::Guard<'_> {
        self.lock()
    }
}

// 4. Custom SleepSpinLock
// (Assuming your SleepSpinLock has a SleepSpinGuard<'a, T> implementing DerefMut<Target = T>)
impl<T: Send + 'static> BenchmarkLock<T> for SleepSpinLock<T> {
    type Guard<'a>
        = SleepSpinLockGuard<'a, T>
    where
        T: 'a;

    fn new(val: T) -> Self {
        SleepSpinLock::new(val)
    }

    fn lock(&self) -> Self::Guard<'_> {
        self.lock()
    }
}

fn benchmark_base<F>(
    num_threads: usize,
    tasks_per_threads: usize,
    uint_task: fn(u64) -> u64,
    worker_fn: F,
) -> (Duration, u64)
where
    F: Fn(usize) + Send + Sync + 'static,
{
    let barrier = Arc::new(Barrier::new(num_threads + 1));
    let worker_arc = Arc::new(worker_fn);

    let mut handles = Vec::with_capacity(num_threads);

    for _ in 0..num_threads {
        let barrier_clone = barrier.clone();
        let worker_clone = worker_arc.clone();

        handles.push(thread::spawn(move || {
            barrier_clone.wait();
            worker_clone(tasks_per_threads);
        }));
    }

    barrier.wait();
    let start = Instant::now();

    for i in handles {
        i.join().expect("Worker thread panicked");
    }

    let elapsed = start.elapsed();

    let mut expected = 0_u64;
    for _ in 0..(num_threads * tasks_per_threads) {
        expected = uint_task(expected);
    }

    (elapsed, expected)
}

// 2. The Generic Benchmark Function
pub fn benchmark_lock<L: BenchmarkLock<u64>>(
    num_threads: usize,
    tasks_per_threads: usize,
    uint_task: fn(u64) -> u64,
) -> Duration {
    let lock = Arc::new(L::new(0));
    let lock_for_worker = Arc::clone(&lock);

    let task = move |num_task: usize| {
        for _ in 0..num_task {
            lock_for_worker.with_lock(|val| {
                *val = uint_task(*val);
                black_box(*val);
            });
        }
    };

    let (elapsed, expected) = benchmark_base(num_threads, tasks_per_threads, uint_task, task);

    let final_val = lock.with_lock(|val| *val);
    assert_eq!(final_val, expected, "Lock race detected!");
    elapsed
}

pub fn benchmark_dclp<L: BenchmarkLock<()>>(
    num_threads: usize,
    tasks_per_threads: usize,
    uint_task: fn(u64) -> u64,
) -> Duration {
    let lock = Arc::new(L::new(()));
    let val = Arc::new(AtomicU64::new(0));

    let lock_for_worker = Arc::clone(&lock);
    let val_for_worker = Arc::clone(&val);

    let task = move |num_tasks: usize| {
        for _ in 0..num_tasks {
            loop {
                let temp = val_for_worker.load(Ordering::Relaxed);
                let next_val = uint_task(temp);

                if temp == val_for_worker.load(Ordering::Relaxed) {
                    let _guard = lock_for_worker.lock();
                    if temp == val_for_worker.load(Ordering::Relaxed) {
                        val_for_worker.store(next_val, Ordering::Release);
                        black_box(next_val);
                        break;
                    }
                }
            }
        }
    };

    let (elapsed, expected) = benchmark_base(num_threads, tasks_per_threads, uint_task, task);

    let final_val = val.load(Ordering::SeqCst);
    assert_eq!(final_val, expected, "DCLP race detected!");
    elapsed
}

#[repr(align(64))]
struct AlignedAtomicU64(pub AtomicU64);

#[repr(align(64))]
struct AlignedLock<L: BenchmarkLock<()>>(pub L);

pub fn benchmark_dclp_align<L: BenchmarkLock<()>>(
    num_threads: usize,
    tasks_per_threads: usize,
    uint_task: fn(u64) -> u64,
) -> Duration {
    let lock = Arc::new(AlignedLock(L::new(())));
    let val = Arc::new(AlignedAtomicU64(AtomicU64::new(0)));

    let lock_for_worker = Arc::clone(&lock);
    let val_for_worker = Arc::clone(&val);

    let task = move |num_tasks: usize| {
        for _ in 0..num_tasks {
            loop {
                let temp = val_for_worker.0.load(Ordering::Relaxed);
                let next_val = uint_task(temp);

                if temp == val_for_worker.0.load(Ordering::Relaxed) {
                    let _guard = lock_for_worker.0.lock();
                    if temp == val_for_worker.0.load(Ordering::Relaxed) {
                        val_for_worker.0.store(next_val, Ordering::Release);
                        black_box(next_val);
                        break;
                    }
                }
            }
        }
    };

    let (elapsed, expected) = benchmark_base(num_threads, tasks_per_threads, uint_task, task);

    let final_val = val.0.load(Ordering::SeqCst);
    assert_eq!(final_val, expected, "DCLP race detected!");
    elapsed
}

pub fn run_bench(bench_mod: i32) {
    const COL_WIDTH: usize = 18;
    const NUM_COLS: usize = 7;

    /// Prints a single row formatted with centered text in each column
    fn print_row(cols: &[&str]) {
        let row = cols
            .iter()
            .map(|col| format!("{col: ^COL_WIDTH$}"))
            .collect::<Vec<_>>()
            .join("|");
        println!("|{row}|");
    }

    /// Prints a separator line using a repeated character (e.g., '-' or ' ')
    fn print_sep(c: char) {
        let segment = c.to_string().repeat(COL_WIDTH);
        let row = vec![segment; NUM_COLS].join("|");
        println!("|{row}|");
    }

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

    let thread_counts = [1, 2, 4, 8, 16, 24, 32, 64];
    let task_counts = [10_000, 100_000, 1_000_000];

    let bench = match bench_mod {
        1 => |num_threads, num_tasks, task_fn, task_name| {
            // Normal lock code bench
            let std_t = benchmark_lock::<Mutex<u64>>(num_threads, num_tasks, task_fn);
            let pl_t = benchmark_lock::<ParkingMutex<u64>>(num_threads, num_tasks, task_fn);
            let spin_t = benchmark_lock::<SpinCrateMutex<u64>>(num_threads, num_tasks, task_fn);
            let custom_t = benchmark_lock::<SleepSpinLock<u64>>(num_threads, num_tasks, task_fn);
            print_row(&[
                task_name,
                &num_threads.to_string(),
                &num_tasks.to_string(),
                &format!("{std_t:.3?}"),
                &format!("{pl_t:.3?}"),
                &format!("{spin_t:.3?}"),
                &format!("{custom_t:.3?}"),
            ])
        },
        2 => |num_threads, num_tasks, task_fn, task_name| {
            // DCLP bench
            let std_t = benchmark_dclp::<Mutex<()>>(num_threads, num_tasks, task_fn);
            let pl_t = benchmark_dclp::<ParkingMutex<()>>(num_threads, num_tasks, task_fn);
            let spin_t = benchmark_dclp::<SpinCrateMutex<()>>(num_threads, num_tasks, task_fn);
            let custom_t = benchmark_dclp::<SleepSpinLock<()>>(num_threads, num_tasks, task_fn);
            print_row(&[
                task_name,
                &num_threads.to_string(),
                &num_tasks.to_string(),
                &format!("{std_t:.3?}"),
                &format!("{pl_t:.3?}"),
                &format!("{spin_t:.3?}"),
                &format!("{custom_t:.3?}"),
            ])
        },
        3 => |num_threads, num_tasks, task_fn, task_name| {
            // aligned DCLP bench
            let std_t = Duration::from_secs(0);
            let pl_t = Duration::from_secs(0);
            let spin_t = Duration::from_secs(0);
            let custom_t =
                benchmark_dclp_align::<SleepSpinLock<()>>(num_threads, num_tasks, task_fn);
            print_row(&[
                task_name,
                &num_threads.to_string(),
                &num_tasks.to_string(),
                &format!("{std_t:.3?}"),
                &format!("{pl_t:.3?}"),
                &format!("{spin_t:.3?}"),
                &format!("{custom_t:.3?}"),
            ])
        },
        _ => panic!("Non valid bench mod, only 1 for normal, 2 for DCLP, 3 for aligned DCLP"),
    };

    match bench_mod {
        1 => println!("Running Normal lock code bench"),
        2 => println!("Running DCLP bench"),
        3 => println!("Running DCLP bench with 64 aligned SleepSpinLock and payload"),
        _ => panic!("Non valid bench mod, only 1 for normal, 2 for DCLP, 3 for aligned DCLP"),
    }

    // Table Header
    print_sep('-');
    print_row(&[
        "Task",
        "Threads",
        "Loops per Thread",
        "std::Mutex",
        "parking_lot",
        "spin::Mutex",
        "SleepSpinLock",
    ]);
    print_sep('-');

    for (task_name, task_fn) in tasks {
        for num_threads in thread_counts {
            for num_tasks in task_counts {
                bench(num_threads, num_tasks, task_fn, task_name);
            }
            print_sep(' ');
        }
        print_sep('-');
    }
}

fn single_task() {
    let num_threads = 8;
    let num_work = 1_000_000;
    let task = |x: u64| {
        let mut state = x.wrapping_add(0x9E3779B97F4A7C15);
        for _ in 0..100 {
            state ^= state >> 30;
            state = state.wrapping_mul(0xBF58476D1CE4E5B9);
            state ^= state >> 27;
            state = state.wrapping_mul(0x94D049BB133111EB);
            state ^= state >> 31;
        }
        black_box(state);
        state
    };

    // let time = benchmark_dclp::<SleepSpinLock<()>>(num_threads, num_work, task);
    // let time = benchmark_lock::<SleepSpinLock<u64>>(num_threads, num_work, task);
    let time = benchmark_lock::<Mutex<u64>>(num_threads, num_work, task);
    println!("std mutex, normal lock code");
    println!(
        "num_threads = {num_threads}, num_work = {num_work}, task = long hashing, time = {:.3?}",
        time
    );
}

fn main() {
    const BENCH_MOD: i32 = 3;
    // 1 for Normal lock code bench
    // 2 for DCLP bench
    // 3 for DCLP bench with 64 aligned sleep_spinlock and payload
    run_bench(BENCH_MOD);

    // single_task();
}

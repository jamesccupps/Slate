//! Background jobs: work on another thread with progress, cancellation and a result the UI picks up.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Wakes the UI thread (posts a window message); called when a job finishes.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

pub struct Ctx {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<AtomicU64>,
}

impl Ctx {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn set(&self, done: u64) {
        self.progress.store(done, Ordering::Relaxed);
    }
}

pub struct Job<T> {
    pub total: u64,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<T>>>,
    handle: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(total: u64, notify: Notify, f: impl FnOnce(&Ctx) -> T + Send + 'static) -> Job<T> {
        let progress = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let ctx = Ctx { cancel: cancel.clone(), progress: progress.clone() };
        let slot = result.clone();
        let handle = std::thread::Builder::new()
            .name("slate-job".into())
            .spawn(move || {
                let r = f(&ctx);
                *slot.lock().unwrap() = Some(r);
                notify();
            })
            .expect("spawn thread");
        Job { total, progress, cancel, result, handle: Some(handle) }
    }

    pub fn done(&self) -> u64 {
        self.progress.load(Ordering::Relaxed)
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.done() as f64 / self.total as f64).min(1.0) as f32 }
    }

    pub fn is_finished(&self) -> bool {
        self.result.lock().unwrap().is_some()
    }

    /// The result, once the job has finished.
    pub fn take(&mut self) -> Option<T> {
        let r = self.result.lock().unwrap().take();
        if r.is_some() {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
        r
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        // Dropping a job cancels it; the thread finishes on its own.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

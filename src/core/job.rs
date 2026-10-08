//! Background jobs: work on another thread with progress, cancellation and a result the UI picks up.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Wakes the UI thread (posts a window message); called when a job finishes.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// What a job gives back when its work panicked (the panic hook has logged it), so the UI handles it like any
/// other failure instead of waiting forever.
pub trait Failure {
    fn failure(msg: &str) -> Self;
}

/// The message for a job that panicked.
pub const FAILED: &str = "Something went wrong (details in crash.log in the settings folder)";

impl Failure for bool {
    fn failure(_: &str) -> bool {
        false
    }
}
impl<T> Failure for Option<T> {
    fn failure(_: &str) -> Self {
        None
    }
}
impl<T> Failure for Vec<T> {
    fn failure(_: &str) -> Self {
        Vec::new()
    }
}
impl<T> Failure for Result<T, String> {
    fn failure(msg: &str) -> Self {
        Err(msg.to_string())
    }
}
impl<T> Failure for std::io::Result<T> {
    fn failure(msg: &str) -> Self {
        Err(std::io::Error::other(msg.to_string()))
    }
}
impl<T> Failure for Result<T, super::io::SaveError> {
    fn failure(msg: &str) -> Self {
        Err(super::io::SaveError::Io(msg.to_string()))
    }
}
impl Failure for super::search::Found {
    fn failure(_: &str) -> Self {
        super::search::Found { count: 0, positions: Vec::new(), complete: false }
    }
}

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

impl<T: Failure + Send + 'static> Job<T> {
    pub fn spawn(total: u64, notify: Notify, f: impl FnOnce(&Ctx) -> T + Send + 'static) -> Job<T> {
        let progress = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let ctx = Ctx { cancel: cancel.clone(), progress: progress.clone() };
        let slot = result.clone();
        let handle = std::thread::Builder::new()
            .name("slate-job".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ctx))).unwrap_or_else(|_| T::failure(FAILED));
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

    /// Waits for the job to finish and returns its result.
    pub fn wait(&mut self) -> Option<T> {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.result.lock().unwrap().take()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_that_panics_reports_a_failure() {
        let mut j = Job::spawn(0, Arc::new(|| {}), |_| -> Result<u32, String> { panic!("boom") });
        assert_eq!(j.wait(), Some(Err(FAILED.to_string())));
    }
}

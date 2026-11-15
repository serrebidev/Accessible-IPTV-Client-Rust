//! Python `background_tasks`: one daemon worker with a single
//! replaceable pending task and no growing queue. Submitting
//! supersedes any not-yet-started task; a running task observes
//! supersession through its cancel token; results are delivered
//! on the caller's thread via [`LatestTask::poll`], which
//! re-checks the version and the closed flag at delivery time —
//! the Rust shape of Python's `wx.CallAfter` delivery closure.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type Delivery = Box<dyn FnOnce() + Send>;
type Work = Box<dyn FnOnce(&CancelToken) -> Delivery + Send>;

struct State {
    version: u64,
    pending: Option<(u64, Work)>,
    deliveries: Vec<(u64, Delivery)>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

/// The cancellation view handed to running work: cancelled when
/// the task was closed or this item's version is no longer
/// current (a newer submit superseded it).
pub struct CancelToken {
    shared: Arc<Shared>,
    version: u64,
}

impl CancelToken {
    /// Whether this task item has been superseded or closed.
    /// # Panics
    /// Panics if the task's shared state lock is poisoned.
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        let state = self.shared.state.lock().unwrap();
        state.closed || state.version != self.version
    }
}

/// A single-worker, latest-wins background task runner.
pub struct LatestTask {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Default for LatestTask {
    fn default() -> Self {
        Self::new()
    }
}

impl LatestTask {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    version: 0,
                    pending: None,
                    deliveries: Vec::new(),
                    closed: false,
                }),
                ready: Condvar::new(),
            }),
            thread: Mutex::new(None),
        }
    }

    /// Queue `work` on the worker, superseding any pending item.
    /// `completed` runs on the thread that calls
    /// [`LatestTask::poll`], and only if this item is still the
    /// latest and the task has not been closed by then. Returns
    /// false (doing nothing) when already closed or when the
    /// worker thread could not be started.
    /// # Panics
    /// Panics if the task's shared state lock is poisoned.
    pub fn submit<R, W, C>(&self, work: W, completed: C) -> bool
    where
        R: Send + 'static,
        W: FnOnce(&CancelToken) -> Result<R, String> + Send + 'static,
        C: FnOnce(Result<R, String>) + Send + 'static,
    {
        let wrapped: Work = Box::new(move |token| {
            let result = work(token);
            Box::new(move || completed(result)) as Delivery
        });
        let mut state = self.shared.state.lock().unwrap();
        if state.closed {
            return false;
        }
        state.version += 1;
        let version = state.version;
        state.pending = Some((version, wrapped));
        if self.thread.lock().unwrap().is_none() {
            let shared = Arc::clone(&self.shared);
            if let Ok(handle) = std::thread::Builder::new()
                .name("LibraryWorker".to_string())
                .spawn(move || run_worker(&shared))
            {
                *self.thread.lock().unwrap() = Some(handle);
            } else {
                state.pending = None;
                return false;
            }
        }
        self.shared.ready.notify_one();
        true
    }

    /// Run every due completion on the calling thread. A queued
    /// completion whose version has been superseded, or that
    /// arrives after [`LatestTask::close`], is dropped silently.
    /// # Panics
    /// Panics if the task's shared state lock is poisoned.
    pub fn poll(&self) {
        let due: Vec<Delivery> = {
            let mut state = self.shared.state.lock().unwrap();
            let current = state.version;
            let closed = state.closed;
            let queued = std::mem::take(&mut state.deliveries);
            queued
                .into_iter()
                .filter(|(version, _)| !closed && *version == current)
                .map(|(_, delivery)| delivery)
                .collect()
        };
        for delivery in due {
            delivery();
        }
    }

    /// Close the worker: pending work is dropped, a running item
    /// observes cancellation, and queued completions never run.
    /// Does not interrupt unrelated threads.
    /// # Panics
    /// Panics if the task's shared state lock is poisoned.
    pub fn close(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.closed = true;
        state.version += 1;
        state.pending = None;
        self.shared.ready.notify_all();
    }

    /// Whether [`LatestTask::close`] has been called.
    /// # Panics
    /// Panics if the task's shared state lock is poisoned.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.state.lock().unwrap().closed
    }
}

fn run_worker(shared: &Arc<Shared>) {
    loop {
        let (version, work) = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.closed {
                    return;
                }
                if let Some(pending) = state.pending.take() {
                    break pending;
                }
                state = shared.ready.wait(state).unwrap();
            }
        };
        let token = CancelToken {
            shared: Arc::clone(shared),
            version,
        };
        let delivery = work(&token);
        if !token.is_canceled() {
            let mut state = shared.state.lock().unwrap();
            state.deliveries.push((version, delivery));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Wait until the worker has queued a delivery (in-module
    /// peek at the shared state; bounded so a regression fails
    /// instead of hanging).
    fn wait_for_delivery(task: &LatestTask) {
        for _ in 0..200 {
            if !task.shared.state.lock().unwrap().deliveries.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("delivery never queued");
    }

    #[test]
    fn superseded_work_never_delivers() {
        let task = LatestTask::new();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let (ran_tx, ran_rx) = mpsc::channel::<&'static str>();
        let delivered = Arc::new(Mutex::new(Vec::new()));

        // The first task blocks until released, and reports what
        // its cancel token said once it resumes.
        let canceled_seen = Arc::new(Mutex::new(false));
        let seen = Arc::clone(&canceled_seen);
        let blocked_tx = ran_tx.clone();
        task.submit(
            move |token| {
                blocked_tx.send("blocked").unwrap();
                let _ = release_rx.lock().unwrap().recv();
                *seen.lock().unwrap() = token.is_canceled();
                Ok("blocked")
            },
            {
                let delivered = Arc::clone(&delivered);
                move |result| delivered.lock().unwrap().push(result.unwrap())
            },
        );
        assert_eq!(ran_rx.recv().unwrap(), "blocked");

        // 100 superseding submits while the first is blocked: the
        // pending slot holds only the newest.
        for _ in 0..99 {
            let ran = ran_tx.clone();
            task.submit(
                move |_| {
                    let _ = ran.send("middle");
                    Ok("middle")
                },
                {
                    let delivered = Arc::clone(&delivered);
                    move |result| delivered.lock().unwrap().push(result.unwrap())
                },
            );
        }
        let ran = ran_tx.clone();
        task.submit(
            move |_| {
                ran.send("latest").unwrap();
                Ok("latest")
            },
            {
                let delivered = Arc::clone(&delivered);
                move |result| delivered.lock().unwrap().push(result.unwrap())
            },
        );

        release_tx.send(()).unwrap();
        wait_for_delivery(&task);
        task.poll();
        // Only the blocked task and the final task ran (the
        // "blocked" send was already consumed by the recv above);
        // only the final result was delivered; the blocked task
        // observed its own cancellation.
        let ran: Vec<&str> = ran_rx.try_iter().collect();
        assert_eq!(ran, ["latest"]);
        assert_eq!(*delivered.lock().unwrap(), ["latest"]);
        assert!(*canceled_seen.lock().unwrap());
    }

    #[test]
    fn close_suppresses_an_already_queued_delivery() {
        let task = LatestTask::new();
        let delivered = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&delivered);
        task.submit(|_| Ok("done"), move |_| *flag.lock().unwrap() = true);
        wait_for_delivery(&task);
        task.close();
        task.poll();
        assert!(!*delivered.lock().unwrap());
        // And a closed task accepts no new work.
        assert!(!task.submit(|_| Ok("x"), |_| {}));
    }

    #[test]
    fn errors_are_delivered_as_errors() {
        let task = LatestTask::new();
        let (tx, rx) = mpsc::channel::<Result<String, String>>();
        task.submit(
            |_| Err("boom".to_string()),
            move |result| tx.send(result).unwrap(),
        );
        wait_for_delivery(&task);
        task.poll();
        assert_eq!(rx.recv().unwrap(), Err("boom".to_string()));
    }
}

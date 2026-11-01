//! Cancellable local-file loading and UI-neutral recovery outcomes.
use crate::{parse_cancellable, BrowseModel};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadFailure {
    Missing,
    Unreadable,
}

impl LoadFailure {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Missing => "Playlist file is missing. Previous channels kept. Open playlist to choose a file or retry.",
            Self::Unreadable => "Playlist file could not be read. Previous channels kept. Open playlist to choose a file or retry.",
        }
    }
}

pub struct LoadResult {
    pub generation: u64,
    pub outcome: Result<Arc<BrowseModel>, LoadFailure>,
}

/// Read in bounded chunks, checking cancellation between reads and parser batches.
/// An in-progress OS read cannot be interrupted; it never blocks the UI thread.
pub(crate) fn read_local(
    path: &Path,
    current: impl Fn() -> bool,
) -> io::Result<Option<BrowseModel>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        if !current() {
            return Ok(None);
        }
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    // Match Python's local-file utf-8-sig/errors=ignore, without replacement glyphs.
    let mut text = String::new();
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        if !current() {
            return Ok(None);
        }
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(error) => {
                text.push_str(
                    std::str::from_utf8(&remaining[..error.valid_up_to()]).expect("valid prefix"),
                );
                let skip = error.valid_up_to()
                    + error
                        .error_len()
                        .unwrap_or(remaining.len() - error.valid_up_to());
                remaining = &remaining[skip..];
            }
        }
    }
    Ok(parse_cancellable(&text, current))
}

/// One loader serializes work and discards obsolete requests and completions.
/// Dropping or cancelling never waits on disk I/O or parsing.
pub struct LoadWorker {
    generation: Arc<AtomicU64>,
    requests: mpsc::Sender<(u64, PathBuf)>,
    results: mpsc::Receiver<LoadResult>,
}

impl Default for LoadWorker {
    fn default() -> Self {
        let (requests, pending) = mpsc::channel::<(u64, PathBuf)>();
        let (completed, results) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let active = Arc::clone(&generation);
        std::thread::spawn(move || {
            while let Ok(mut request) = pending.recv() {
                while let Ok(newer) = pending.try_recv() {
                    request = newer;
                }
                let current = || active.load(Ordering::Relaxed) == request.0;
                let outcome = match read_local(&request.1, current) {
                    Ok(Some(model)) => Ok(Arc::new(model)),
                    Ok(None) => continue,
                    Err(error) => Err(if error.kind() == io::ErrorKind::NotFound {
                        LoadFailure::Missing
                    } else {
                        LoadFailure::Unreadable
                    }),
                };
                if current()
                    && completed
                        .send(LoadResult {
                            generation: request.0,
                            outcome,
                        })
                        .is_err()
                {
                    break;
                }
            }
        });
        Self {
            generation,
            requests,
            results,
        }
    }
}

impl LoadWorker {
    pub fn submit(&self, path: PathBuf) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.requests.send((generation, path));
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn try_latest(&self) -> Option<LoadResult> {
        while let Ok(result) = self.results.try_recv() {
            if result.generation == self.generation.load(Ordering::Relaxed) {
                return Some(result);
            }
            std::thread::spawn(move || drop(result));
        }
        None
    }
}

impl Drop for LoadWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    fn wait(worker: &LoadWorker) -> LoadResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = worker.try_latest() {
                return result;
            }
            assert!(Instant::now() < deadline, "load timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn missing_unreadable_and_retry_are_recoverable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let worker = LoadWorker::default();
        worker.submit(root.join("tests/fixtures"));
        assert_eq!(wait(&worker).outcome.unwrap_err(), LoadFailure::Unreadable);
        worker.submit(root.join("tests/fixtures/absent.m3u"));
        assert_eq!(wait(&worker).outcome.unwrap_err(), LoadFailure::Missing);
        worker.submit(root.join("tests/fixtures/local.m3u"));
        assert_eq!(wait(&worker).outcome.unwrap().channels().len(), 4);
    }

    #[test]
    fn cancellation_checks_reads_and_parsing() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local.m3u");
        assert!(read_local(&path, || false).unwrap().is_none());
        let text = "#EXTINF:-1,News\nurl\n".repeat(1024);
        let checks = Cell::new(0);
        assert!(parse_cancellable(&text, || {
            checks.set(checks.get() + 1);
            checks.get() < 2
        })
        .is_none());
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn queued_success_and_failure_cannot_survive_cancel_or_replacement() {
        let (completed, results) = mpsc::channel();
        let (requests, _pending) = mpsc::channel();
        let worker = LoadWorker {
            generation: Arc::new(AtomicU64::new(0)),
            requests,
            results,
        };
        completed
            .send(LoadResult {
                generation: 0,
                outcome: Ok(Arc::new(BrowseModel::empty())),
            })
            .unwrap();
        worker.cancel();
        assert!(worker.try_latest().is_none());
        completed
            .send(LoadResult {
                generation: 1,
                outcome: Err(LoadFailure::Missing),
            })
            .unwrap();
        worker.submit(PathBuf::new());
        assert!(worker.try_latest().is_none());
    }
}

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
    /// A parsed-cache prefill (B11): displayable now, but a final
    /// result for the same generation still follows.
    pub intermediate: bool,
    /// Sources whose previous channels were retained (B12).
    pub stale_sources: Vec<String>,
    /// Playlist-header guides per source read this load (B09).
    pub guide_urls: Vec<(String, Vec<String>)>,
}

impl LoadResult {
    #[cfg(test)]
    fn plain(generation: u64, outcome: Result<Arc<BrowseModel>, LoadFailure>) -> Self {
        Self {
            generation,
            outcome,
            intermediate: false,
            stale_sources: Vec::new(),
            guide_urls: Vec::new(),
        }
    }
}

/// Read in bounded chunks, checking cancellation between reads and parser batches.
/// An in-progress OS read cannot be interrupted; it never blocks the UI thread.
pub(crate) fn read_local(
    path: &Path,
    current: impl Fn() -> bool,
) -> io::Result<Option<BrowseModel>> {
    let Some(text) = read_local_text(path, &current)? else {
        return Ok(None);
    };
    Ok(parse_cancellable(&text, current))
}

/// The text half of [`read_local`]: chunked reads plus the Python
/// utf-8-sig/errors=ignore decoding, without replacement glyphs.
pub(crate) fn read_local_text(
    path: &Path,
    current: &dyn Fn() -> bool,
) -> io::Result<Option<String>> {
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
    Ok(Some(text))
}

enum LoadRequest {
    Local(PathBuf),
    Sources {
        sources: Vec<crate::sources::PlaylistSource>,
        cache_dir: Option<PathBuf>,
        /// The last usable catalogue, for per-source retention (B12).
        previous: Arc<BrowseModel>,
        /// Bypass the URL download cache (B13 refresh).
        force_refresh: bool,
    },
}

/// One loader serializes work and discards obsolete requests and completions.
/// Dropping or cancelling never waits on disk I/O or parsing.
pub struct LoadWorker {
    generation: Arc<AtomicU64>,
    requests: mpsc::Sender<(u64, LoadRequest)>,
    results: mpsc::Receiver<LoadResult>,
}

impl Default for LoadWorker {
    fn default() -> Self {
        let (requests, pending) = mpsc::channel::<(u64, LoadRequest)>();
        let (completed, results) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let active = Arc::clone(&generation);
        std::thread::spawn(move || {
            while let Ok(mut request) = pending.recv() {
                while let Ok(newer) = pending.try_recv() {
                    request = newer;
                }
                let current = || active.load(Ordering::Relaxed) == request.0;
                if let LoadRequest::Sources {
                    sources,
                    cache_dir: Some(dir),
                    ..
                } = &request.1
                {
                    // B11 prefill: parsed-cache channels display
                    // before any network work, then the real load
                    // replaces them.
                    if let Some(prefill) = crate::sources::prefill_model(sources, dir) {
                        if !current()
                            || completed
                                .send(LoadResult {
                                    generation: request.0,
                                    outcome: Ok(Arc::new(prefill)),
                                    intermediate: true,
                                    stale_sources: Vec::new(),
                                    guide_urls: Vec::new(),
                                })
                                .is_err()
                        {
                            break;
                        }
                    }
                }
                let (outcome, stale_sources, guide_urls) = match &request.1 {
                    LoadRequest::Local(path) => match read_local(path, current) {
                        Ok(Some(model)) => (Ok(Arc::new(model)), Vec::new(), Vec::new()),
                        Ok(None) => continue,
                        Err(error) => (
                            Err(if error.kind() == io::ErrorKind::NotFound {
                                LoadFailure::Missing
                            } else {
                                LoadFailure::Unreadable
                            }),
                            Vec::new(),
                            Vec::new(),
                        ),
                    },
                    LoadRequest::Sources {
                        sources,
                        cache_dir,
                        previous,
                        force_refresh,
                    } => {
                        let options = crate::sources::MergeOptions {
                            cache_dir: cache_dir.as_deref(),
                            previous: previous.channels(),
                            force_refresh: *force_refresh,
                        };
                        match crate::sources::load_merged_with(
                            sources,
                            &crate::sources::DefaultFetcher,
                            options,
                            current,
                        ) {
                            Ok(Some(merged)) => (
                                Ok(Arc::new(merged.model)),
                                merged.stale_sources,
                                merged.guide_urls,
                            ),
                            Ok(None) => continue,
                            Err(failure) => (Err(failure), Vec::new(), Vec::new()),
                        }
                    }
                };
                if current()
                    && completed
                        .send(LoadResult {
                            generation: request.0,
                            outcome,
                            intermediate: false,
                            stale_sources,
                            guide_urls,
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
        let _ = self.requests.send((generation, LoadRequest::Local(path)));
    }

    pub fn submit_sources(&self, sources: Vec<crate::sources::PlaylistSource>) {
        self.submit_sources_full(sources, None, Arc::new(BrowseModel::empty()), false);
    }

    /// Sources load with the plain-source caches, the previous
    /// catalogue for retention, and optional cache bypass.
    pub fn submit_sources_full(
        &self,
        sources: Vec<crate::sources::PlaylistSource>,
        cache_dir: Option<PathBuf>,
        previous: Arc<BrowseModel>,
        force_refresh: bool,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.requests.send((
            generation,
            LoadRequest::Sources {
                sources,
                cache_dir,
                previous,
                force_refresh,
            },
        ));
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
    fn sources_load_prefills_from_parsed_cache_then_publishes_final() {
        // B11/B12 worker plumbing: the second load of an unchanged
        // file source emits the parsed-cache prefill first and the
        // final result after; a vanished source retains its channels.
        let dir = std::env::temp_dir().join(format!("iptv-load-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("list.m3u");
        std::fs::write(
            &file,
            "#EXTM3U x-tvg-url=\"https://g.example/a.xml\"\n#EXTINF:-1,Alpha\nhttp://a/1\n",
        )
        .unwrap();
        let cache_dir = dir.join("cache");
        let source = crate::sources::PlaylistSource::new(file.to_string_lossy().into_owned());
        let worker = LoadWorker::default();
        worker.submit_sources_full(
            vec![source.clone()],
            Some(cache_dir.clone()),
            Arc::new(BrowseModel::empty()),
            false,
        );
        let first = wait(&worker);
        assert!(!first.intermediate);
        assert_eq!(first.outcome.as_ref().unwrap().channels().len(), 1);
        assert_eq!(
            first.guide_urls,
            [(
                file.to_string_lossy().into_owned(),
                vec!["https://g.example/a.xml".to_string()]
            )]
        );
        let previous = first.outcome.unwrap();
        worker.submit_sources_full(
            vec![source.clone()],
            Some(cache_dir.clone()),
            Arc::clone(&previous),
            false,
        );
        let prefill = wait(&worker);
        assert!(prefill.intermediate, "parsed-cache prefill comes first");
        assert_eq!(prefill.outcome.as_ref().unwrap().channels().len(), 1);
        let final_result = wait(&worker);
        assert!(!final_result.intermediate);
        // Remove the file: the load now retains the previous channels
        // and reports the source stale instead of failing.
        std::fs::remove_file(&file).unwrap();
        worker.submit_sources_full(vec![source], Some(cache_dir), Arc::clone(&previous), false);
        let retained = wait(&worker);
        if retained.intermediate {
            let retained = wait(&worker);
            assert_eq!(retained.stale_sources.len(), 1);
            assert_eq!(retained.outcome.unwrap().channels().len(), 1);
        } else {
            assert_eq!(retained.stale_sources.len(), 1);
            assert_eq!(retained.outcome.unwrap().channels().len(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
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
            .send(LoadResult::plain(0, Ok(Arc::new(BrowseModel::empty()))))
            .unwrap();
        worker.cancel();
        assert!(worker.try_latest().is_none());
        completed
            .send(LoadResult::plain(1, Err(LoadFailure::Missing)))
            .unwrap();
        worker.submit(PathBuf::new());
        assert!(worker.try_latest().is_none());
    }
}

//! Single-flight, replaceable channel-name searches; no widget dependencies.
use crate::{search_rank, BrowseModel, Channel};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};

pub(crate) fn matching_rows(
    channels: &[Channel],
    query: &str,
    current: impl Fn() -> bool,
) -> Option<Vec<usize>> {
    let query = query.trim().to_lowercase();
    let mut matches = Vec::new();
    for (index, channel) in channels.iter().enumerate() {
        if index % 256 == 0 && !current() {
            return None;
        }
        if query.is_empty() {
            matches.push(((0, 0), index));
            continue;
        }
        let name = channel.name.to_lowercase();
        if name.contains(&query) {
            let rank = search_rank(&name, &query);
            matches.push((rank, index));
        }
    }
    if !current() {
        return None;
    }
    // The source index makes ties deterministic without sorting names again.
    if !query.is_empty() {
        matches.sort_unstable();
    }
    if !current() {
        return None;
    }
    Some(matches.into_iter().map(|(_, index)| index).collect())
}

/// A worker-built virtual-list snapshot. Playlist indexes are stable identities
/// within this local playlist, including duplicate names and stream URLs.
#[derive(Debug)]
pub struct SearchResults {
    pub generation: u64,
    playlist: Arc<BrowseModel>,
    rows: Vec<usize>,
    positions: Vec<usize>,
    query: String,
}

impl Default for SearchResults {
    fn default() -> Self {
        Self {
            generation: 0,
            playlist: Arc::new(BrowseModel::empty()),
            rows: Vec::new(),
            positions: Vec::new(),
            query: String::new(),
        }
    }
}

impl SearchResults {
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn channel(&self, row: usize) -> Option<&Channel> {
        self.rows
            .get(row)
            .and_then(|&index| self.playlist.channels().get(index))
    }

    /// An unchanged row needs no selection/focus cycling or fresh announcement.
    #[must_use]
    pub fn retains_row(&self, previous: &Self, row: usize) -> bool {
        Arc::ptr_eq(&self.playlist, &previous.playlist)
            && self.rows.get(row).is_some()
            && self.rows.get(row) == previous.rows.get(row)
    }

    /// Restore a selected or focused row by identity, falling back to the first
    /// result. This is an item-focus signal, never a request for keyboard focus.
    #[must_use]
    pub fn restore_row(&self, previous: &Self, row: Option<usize>) -> Option<usize> {
        if Arc::ptr_eq(&self.playlist, &previous.playlist) {
            if let Some(position) = row
                .and_then(|row| previous.rows.get(row))
                .and_then(|&index| self.positions.get(index))
                .copied()
                .filter(|&position| position != usize::MAX)
            {
                return Some(position);
            }
        }
        (!self.is_empty()).then_some(0)
    }

    #[must_use]
    pub fn status(&self) -> String {
        let message = if self.playlist.channels().is_empty() {
            "Channel list is empty. Open another playlist.".to_owned()
        } else if self.query.trim().is_empty() {
            format!(
                "{} channels. Search cleared; showing all channels.",
                self.len()
            )
        } else if self.is_empty() {
            "No matching channels. Change or clear Search channels.".to_owned()
        } else {
            format!("{} matching channels.", self.len())
        };
        if self.playlist.malformed_entries() > 0 {
            format!("Playlist contains malformed entries; usable channels kept. {message}")
        } else {
            message
        }
    }
}

struct Request {
    generation: u64,
    playlist: Arc<BrowseModel>,
    query: String,
}

/// One worker serializes searches, skips queued obsolete requests, and checks
/// cancellation every 256 channels. Dropping it cancels without joining the UI.
pub struct SearchWorker {
    generation: Arc<AtomicU64>,
    requests: mpsc::Sender<Request>,
    results: mpsc::Receiver<SearchResults>,
}

impl Default for SearchWorker {
    fn default() -> Self {
        let (requests, pending) = mpsc::channel::<Request>();
        let (completed, results) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let active = Arc::clone(&generation);
        std::thread::spawn(move || {
            while let Ok(mut request) = pending.recv() {
                while let Ok(newer) = pending.try_recv() {
                    request = newer;
                }
                let current = || active.load(Ordering::Relaxed) == request.generation;
                let Some(rows) =
                    matching_rows(request.playlist.channels(), &request.query, current)
                else {
                    continue;
                };
                let mut positions = vec![usize::MAX; request.playlist.channels().len()];
                for (row, &index) in rows.iter().enumerate() {
                    positions[index] = row;
                }
                if current()
                    && completed
                        .send(SearchResults {
                            generation: request.generation,
                            playlist: request.playlist,
                            rows,
                            positions,
                            query: request.query,
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

impl SearchWorker {
    /// Queue a cheap shared playlist snapshot. No channel scanning on the caller.
    pub fn submit(&self, playlist: Arc<BrowseModel>, query: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.requests.send(Request {
            generation,
            playlist,
            query,
        });
    }

    /// Invalidate active and already queued completions without waiting.
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Take only a current result, including when an old completion was already
    /// queued before the next keystroke. Retire stale playlists off the caller.
    #[must_use]
    pub fn try_latest(&self) -> Option<SearchResults> {
        while let Ok(result) = self.results.try_recv() {
            if result.generation == self.generation.load(Ordering::Relaxed) {
                return Some(result);
            }
            std::thread::spawn(move || drop(result));
        }
        None
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_m3u;
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    fn playlist(names: &[&str]) -> Arc<BrowseModel> {
        let mut text = String::new();
        for name in names {
            text.push_str("#EXTINF:-1,");
            text.push_str(name);
            text.push_str("\nsame-url\n");
        }
        Arc::new(BrowseModel {
            channels: parse_m3u(&text),
            malformed: 0,
        })
    }

    fn wait(worker: &SearchWorker) -> SearchResults {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = worker.try_latest() {
                return result;
            }
            assert!(Instant::now() < deadline, "worker timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn matches_python_ranking_including_later_word_starts_and_unicode_lengths() {
        let source = playlist(&["xcatzz", "xcat cat", "xx cat", "\u{732b}cat", "Cat", "CAT"]);
        let rows = matching_rows(source.channels(), "  cAt ", || true).unwrap();
        assert_eq!(rows, [4, 5, 3, 2, 1, 0]);
        let overlapping = playlist(&["a\u{e9}\u{e9}\u{e9}", "aa \u{e9}\u{e9}"]);
        assert_eq!(
            matching_rows(overlapping.channels(), "\u{e9}\u{e9}", || true).unwrap(),
            [0, 1]
        );
    }

    #[test]
    fn incremental_queries_empty_results_and_clear_restore_source_order() {
        let worker = SearchWorker::default();
        let source = playlist(&["News Two", "Sport", "News"]);
        for (query, expected) in [
            ("n", vec![2, 0]),
            ("NEws t", vec![0]),
            ("missing", vec![]),
            ("  ", vec![0, 1, 2]),
        ] {
            worker.submit(Arc::clone(&source), query.to_owned());
            let result = wait(&worker);
            assert_eq!(result.rows, expected);
            if query == "missing" {
                assert!(result.status().contains("No matching channels"));
            }
            if query == "  " {
                assert!(result.status().contains("Search cleared"));
            }
        }
    }

    #[test]
    fn selection_and_item_focus_follow_identity_even_for_duplicate_names_and_urls() {
        let worker = SearchWorker::default();
        let source = playlist(&["News Two", "Sport", "News", "News"]);
        worker.submit(Arc::clone(&source), String::new());
        let all = wait(&worker);
        worker.submit(Arc::clone(&source), "news".to_owned());
        let news = wait(&worker);
        assert!(all.retains_row(&all, 3));
        assert!(!all.retains_row(&all, 4));
        assert!(!news.retains_row(&all, 0));
        assert_eq!(news.restore_row(&all, Some(0)), Some(2));
        assert_eq!(news.restore_row(&all, Some(3)), Some(1));
        assert_eq!(news.restore_row(&all, Some(1)), Some(0));
        worker.submit(Arc::clone(&source), "absent".to_owned());
        let none = wait(&worker);
        assert_eq!(none.restore_row(&news, Some(1)), None);
        worker.submit(Arc::clone(&source), String::new());
        let cleared = wait(&worker);
        assert_eq!(cleared.restore_row(&news, Some(1)), Some(3));
        assert_eq!(cleared.restore_row(&none, None), Some(0));
        worker.submit(playlist(&["Other"]), String::new());
        assert_eq!(wait(&worker).restore_row(&all, Some(3)), Some(0));
    }

    #[test]
    fn scan_cancels_at_a_bounded_checkpoint() {
        let source = playlist(&vec!["News"; 1024]);
        let checks = Cell::new(0);
        let result = matching_rows(source.channels(), "news", || {
            checks.set(checks.get() + 1);
            checks.get() < 2
        });
        assert!(result.is_none());
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn already_queued_results_cannot_overwrite_a_newer_input() {
        let (completed, results) = mpsc::channel();
        let (requests, _pending) = mpsc::channel();
        let worker = SearchWorker {
            generation: Arc::new(AtomicU64::new(0)),
            requests,
            results,
        };
        completed.send(SearchResults::default()).unwrap();
        worker.submit(playlist(&["News"]), "news".to_owned());
        assert!(worker.try_latest().is_none());
    }

    #[test]
    fn newer_search_replaces_large_search_and_playlist_snapshot() {
        let worker = SearchWorker::default();
        let large = playlist(&vec!["News event"; 500_000]);
        worker.submit(Arc::clone(&large), "news".to_owned());
        let full = wait(&worker);
        assert_eq!(full.len(), 500_000);
        assert_eq!(full.restore_row(&full, Some(499_999)), Some(499_999));
        worker.submit(large, "news".to_owned());
        worker.submit(playlist(&["Replacement", "Other"]), "replace".to_owned());
        let result = wait(&worker);
        assert_eq!(result.generation, 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result.channel(0).unwrap().name, "Replacement");
        assert!(result.channel(1).is_none());
    }
}

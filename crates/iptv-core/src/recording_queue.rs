//! Session-only recording playback queue (C7; port of Python's
//! `recording_queue.py`).
//!
//! The queue holds library rows for the session only — nothing
//! is persisted. File validation is deferred until each
//! hand-off: a row is checked against the disk when playback
//! reaches it, and a missing, changed or still-recording file
//! stops the queue at that title with Python's message.

use crate::recording_library::LibraryRow;

/// One queue edit's outcome (Python `RecordingQueue.edit`):
/// the selection index to show after the edit, and whether the
/// head changed while its validation was in flight (the caller
/// must restart the hand-off, keeping focus in the editor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    pub index: usize,
    pub restart: bool,
}

/// The session queue (Python `RecordingQueue` without its
/// worker/dispatch half: the app owns the hand-off thread and
/// the ticket delivery).
#[derive(Clone, Debug, Default)]
pub struct RecordingQueue {
    pending: Vec<LibraryRow>,
    playing: bool,
    loading: bool,
    ticket: u64,
}

impl RecordingQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queued rows, in play order.
    #[must_use]
    pub fn pending(&self) -> &[LibraryRow] {
        &self.pending
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    #[must_use]
    pub fn playing(&self) -> bool {
        self.playing
    }

    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    #[must_use]
    pub fn ticket(&self) -> u64 {
        self.ticket
    }

    /// Add rows, skipping paths already queued (Python `add`:
    /// `os.path.normcase` dedupe). Returns the new count.
    pub fn add(&mut self, rows: impl IntoIterator<Item = LibraryRow>) -> usize {
        let mut known: Vec<String> = self
            .pending
            .iter()
            .map(|row| norm_case(&row.path))
            .collect();
        for row in rows {
            let key = norm_case(&row.path);
            if !known.contains(&key) {
                known.push(key);
                self.pending.push(row);
            }
        }
        self.pending.len()
    }

    /// Reorder or remove one row (Python `edit`). `delta` of
    /// `Some(±1)` swaps with the neighbour; `None` removes.
    /// An out-of-range index or move is a no-op returning the
    /// index unchanged (clamped to the last row); a removal
    /// past the end clamps to the new last row.
    pub fn edit(&mut self, mut index: usize, delta: Option<i32>) -> Edit {
        if index >= self.pending.len() {
            return Edit {
                index,
                restart: false,
            };
        }
        let head_first = self.pending.first().map(|row| row.path.clone());
        match delta {
            None => {
                self.pending.remove(index);
            }
            Some(step) if (-1..=1).contains(&step) && step != 0 => {
                let target = if step > 0 {
                    index.checked_add(1)
                } else {
                    index.checked_sub(1)
                };
                let Some(target) = target.filter(|t| *t < self.pending.len()) else {
                    return Edit {
                        index,
                        restart: false,
                    };
                };
                self.pending.swap(index, target);
                index = target;
            }
            _ => {
                return Edit {
                    index,
                    restart: false,
                };
            }
        }
        let restart = self.loading
            && (self.pending.is_empty()
                || self.pending.first().map(|row| row.path.clone()) != head_first);
        Edit {
            index: index.min(self.pending.len().saturating_sub(1)),
            restart,
        }
    }

    fn bump_ticket(&mut self) {
        self.ticket = self.ticket.wrapping_add(1).max(1);
    }

    /// Stop the queue without clearing it (Python `stop`):
    /// the head stays queued for a later Play Queue.
    pub fn stop(&mut self) {
        self.bump_ticket();
        self.playing = false;
        self.loading = false;
    }

    /// Start playing from the head (Python `start`): any
    /// in-flight validation is discarded. Returns the ticket
    /// the hand-off must carry, or `None` when empty.
    pub fn start(&mut self) -> Option<u64> {
        self.stop();
        if self.pending.is_empty() {
            return None;
        }
        self.playing = true;
        self.bump_ticket();
        Some(self.ticket)
    }

    /// Begin validating the head for hand-off (Python `next`'s
    /// loading half): marks the queue loading and returns the
    /// head row with the ticket. `None` ends the queue.
    pub fn begin_next(&mut self, exit_requested: bool) -> Option<(LibraryRow, u64)> {
        if self.pending.is_empty() || exit_requested {
            self.stop();
            return None;
        }
        self.loading = true;
        let row = self.pending[0].clone();
        Some((row, self.ticket))
    }

    /// A hand-off finished (Python `ready`): on success the
    /// head is consumed and loading clears; on failure the
    /// queue stops. Returns the stopped-at message on failure.
    pub fn finish_next(
        &mut self,
        ticket: u64,
        exit_requested: bool,
        error: Option<String>,
    ) -> Option<String> {
        if ticket != self.ticket || exit_requested {
            return None;
        }
        if let Some(reason) = error {
            let title = self
                .pending
                .first()
                .map_or_else(String::new, |row| row.title.clone());
            self.stop();
            return Some(stopped_at(&title, &reason));
        }
        self.loading = false;
        self.pending.remove(0);
        None
    }

    /// The player ended the queue's item (Python `ended`): the
    /// next item starts only while this ticket is current, the
    /// queue is playing, no hand-off is loading, the generation
    /// matches and the stop was not manual.
    #[must_use]
    pub fn should_advance(
        &self,
        ticket: Option<u64>,
        generation: u64,
        current_generation: u64,
        manual_stop: bool,
        destroyed: bool,
    ) -> bool {
        ticket == Some(self.ticket)
            && self.playing
            && !self.loading
            && generation == current_generation
            && !destroyed
            && !manual_stop
    }
}

/// `os.path.normcase`: lowercased on Windows, unchanged elsewhere.
#[must_use]
pub fn norm_case(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// One editor row (Python `show_editor.refresh`).
#[must_use]
pub fn item_label(position: usize, count: usize, title: &str) -> String {
    format!("{position} of {count}: {title}")
}

/// The library status line after adding (Python
/// `queue_selected`).
#[must_use]
pub fn queued_count_text(count: usize) -> String {
    format!("Recordings in the playback queue: {count}.")
}

/// The stop message (Python `ready`'s failure box).
#[must_use]
pub fn stopped_at(title: &str, reason: &str) -> String {
    format!("Playback queue stopped at {title}. {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, title: &str) -> LibraryRow {
        LibraryRow {
            path: path.to_string(),
            title: title.to_string(),
            channel: String::new(),
            duration: None,
            status: "completed".to_string(),
            size: 0,
            modified_ns: 0,
            date: String::new(),
            directory: String::new(),
            trash_id: None,
            original_name: None,
        }
    }

    #[test]
    fn add_dedupes_by_normalised_path() {
        let mut queue = RecordingQueue::new();
        let count = queue.add(vec![
            row("C:\\a.mkv", "A"),
            row("C:\\b.mkv", "B"),
            row("C:\\a.mkv", "A again"),
        ]);
        assert_eq!(count, 2);
        assert_eq!(queue.pending()[0].title, "A");
        assert_eq!(queue.pending()[1].title, "B");
        // A later add of a known path is skipped.
        assert_eq!(queue.add(vec![row("C:\\b.mkv", "B again")]), 2);
        // os.path.normcase folds case only on Windows.
        #[cfg(windows)]
        {
            assert_eq!(queue.add(vec![row("C:\\B.mkv", "B again")]), 2);
        }
    }

    #[test]
    fn edit_moves_removes_and_clamps() {
        let mut queue = RecordingQueue::new();
        queue.add(vec![
            row("C:\\a.mkv", "A"),
            row("C:\\b.mkv", "B"),
            row("C:\\c.mkv", "C"),
        ]);
        let moved = queue.edit(1, Some(1));
        assert_eq!(moved.index, 2);
        assert_eq!(queue.pending()[2].title, "B");
        // Moving past the end is a no-op.
        let still = queue.edit(2, Some(1));
        assert_eq!(still.index, 2);
        assert_eq!(queue.pending()[2].title, "B");
        // Removing the last row clamps to the new last row.
        let removed = queue.edit(2, None);
        assert_eq!(removed.index, 1);
        assert_eq!(queue.len(), 2);
        // An out-of-range index is a no-op.
        let ignored = queue.edit(9, None);
        assert_eq!(ignored.index, 9);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn edit_restarts_a_loading_hand_off_only_when_the_head_changes() {
        let mut queue = RecordingQueue::new();
        queue.add(vec![
            row("C:\\a.mkv", "A"),
            row("C:\\b.mkv", "B"),
            row("C:\\c.mkv", "C"),
        ]);
        queue.start();
        queue.begin_next(false);
        assert!(queue.loading());
        // Reordering the tail does not restart the head validation.
        let tail = queue.edit(2, Some(-1));
        assert!(!tail.restart);
        assert_eq!(queue.pending()[0].title, "A");
        // Moving the head restarts it ([A,C,B] -> [C,A,B]).
        let head_move = queue.edit(1, Some(-1));
        assert!(head_move.restart);
        assert_eq!(queue.pending()[0].title, "C");
        // Removing the head restarts it.
        let mut fresh = RecordingQueue::new();
        fresh.add(vec![row("C:\\a.mkv", "A"), row("C:\\b.mkv", "B")]);
        fresh.start();
        fresh.begin_next(false);
        let head = fresh.edit(0, None);
        assert!(head.restart);
    }

    #[test]
    fn start_and_stop_keep_the_pending_list() {
        let mut queue = RecordingQueue::new();
        queue.add(vec![row("C:\\a.mkv", "A")]);
        let ticket = queue.start().expect("non-empty queue starts");
        assert!(queue.playing());
        assert_eq!(queue.ticket(), ticket);
        queue.stop();
        assert!(!queue.playing());
        assert_eq!(queue.len(), 1);
        assert!(RecordingQueue::new().start().is_none());
    }

    #[test]
    fn finish_consumes_on_success_and_stops_on_error() {
        let mut queue = RecordingQueue::new();
        queue.add(vec![row("C:\\a.mkv", "A"), row("C:\\b.mkv", "B")]);
        queue.start();
        let (head, ticket) = queue.begin_next(false).expect("head");
        assert_eq!(head.title, "A");
        assert!(queue.finish_next(ticket, false, None).is_none());
        assert_eq!(queue.len(), 1);
        assert!(!queue.loading());
        // A stale ticket (a stop intervened) finishes nothing.
        queue.begin_next(false);
        queue.stop();
        assert!(queue.finish_next(ticket, false, None).is_none());
        assert_eq!(queue.len(), 1);
        queue.start();
        // An error stops at the title.
        let (head, ticket) = queue.begin_next(false).expect("head");
        assert_eq!(head.title, "B");
        let stopped = queue
            .finish_next(
                ticket,
                false,
                Some("The recording file is no longer available.".to_string()),
            )
            .expect("stop message");
        assert_eq!(
            stopped,
            "Playback queue stopped at B. The recording file is no longer available."
        );
        assert!(!queue.playing());
        // Exit requested settles nothing.
        assert!(queue.finish_next(ticket, true, None).is_none());
    }

    #[test]
    fn advance_needs_the_current_ticket_and_generation() {
        let mut queue = RecordingQueue::new();
        queue.add(vec![row("C:\\a.mkv", "A")]);
        let ticket = queue.start().expect("starts");
        assert!(queue.should_advance(Some(ticket), 7, 7, false, false));
        assert!(!queue.should_advance(Some(ticket), 7, 8, false, false));
        assert!(!queue.should_advance(Some(ticket), 7, 7, true, false));
        assert!(!queue.should_advance(Some(ticket), 7, 7, false, true));
        assert!(!queue.should_advance(None, 7, 7, false, false));
        queue.begin_next(false);
        assert!(!queue.should_advance(Some(ticket), 7, 7, false, false));
    }

    #[test]
    fn wording_matches_python() {
        assert_eq!(item_label(1, 3, "News"), "1 of 3: News");
        assert_eq!(queued_count_text(2), "Recordings in the playback queue: 2.");
        assert_eq!(
            stopped_at("News", "gone"),
            "Playback queue stopped at News. gone"
        );
    }
}

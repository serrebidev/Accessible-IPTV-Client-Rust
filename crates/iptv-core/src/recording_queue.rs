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
    /// The head row handed to the player but not yet
    /// confirmed playing (Python PR #46 `opening`): it stays
    /// queued until playback actually starts, so an
    /// unconfirmed entry is retried instead of lost.
    opening: Option<LibraryRow>,
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

    /// The row handed to the player, awaiting its playback
    /// confirmation (Python `opening`).
    #[must_use]
    pub fn opening(&self) -> Option<&LibraryRow> {
        self.opening.as_ref()
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
        let restart = (self.loading || self.opening.is_some())
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
        self.opening = None;
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
    /// head becomes the opening row and stays queued until
    /// the player confirms it started (`started`); on
    /// failure the queue stops. Returns the stopped-at
    /// message on failure.
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
        self.opening = self.pending.first().cloned();
        None
    }

    /// The player confirmed the opening row is playing
    /// (Python PR #46 `started`): only now is the head
    /// consumed. A stale ticket or generation, or a head
    /// that is no longer the opening row, consumes nothing.
    pub fn started(&mut self, ticket: u64, generation: u64, current_generation: u64) -> bool {
        let Some(opening) = &self.opening else {
            return false;
        };
        if ticket != self.ticket || generation != current_generation {
            return false;
        }
        if self.pending.first().map(|row| &row.path) != Some(&opening.path) {
            return false;
        }
        self.pending.remove(0);
        self.opening = None;
        true
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

// ------------------------------------------------------------ saved queues

/// Most named queues kept (Python `MAX_SAVED_QUEUES`).
pub const MAX_SAVED_QUEUES: usize = 30;

/// Most entries in one saved queue (Python
/// `MAX_QUEUE_ENTRIES`).
pub const MAX_QUEUE_ENTRIES: usize = 500;

/// One saved queue's stored form: the five fields a
/// hand-off needs to re-validate the file (Python's entry
/// dicts).
fn entry_json(row: &LibraryRow) -> serde_json::Value {
    serde_json::json!({
        "path": row.path,
        "directory": row.directory,
        "title": row.title,
        "size": row.size,
        "modified_ns": row.modified_ns,
    })
}

/// Validate one stored entry (Python `saved_queues`' row
/// filter): non-empty NUL-free strings, absolute paths, the
/// file directly inside its directory, a non-negative size.
fn entry_row(entry: &serde_json::Value) -> Option<LibraryRow> {
    let path = entry.get("path")?.as_str()?;
    let directory = entry.get("directory")?.as_str()?;
    let title = entry.get("title")?.as_str()?;
    if [path, directory, title]
        .iter()
        .any(|text| text.is_empty() || text.contains('\0'))
    {
        return None;
    }
    let path_ref = std::path::Path::new(path);
    if !path_ref.is_absolute() || !std::path::Path::new(directory).is_absolute() {
        return None;
    }
    if path_ref.parent() != Some(std::path::Path::new(directory)) {
        return None;
    }
    let size = entry.get("size")?.as_i64()?;
    if size < 0 {
        return None;
    }
    let modified_ns = entry.get("modified_ns")?.as_i64()?;
    Some(LibraryRow {
        path: path.to_string(),
        title: title.to_string(),
        channel: String::new(),
        duration: None,
        status: "completed".to_string(),
        size: u64::try_from(size).ok()?,
        modified_ns,
        date: String::new(),
        directory: directory.to_string(),
        trash_id: None,
        original_name: None,
    })
}

/// A queue name as Python accepts it: 1–80 characters once
/// trimmed, nothing unprintable (control characters here).
/// Returns the trimmed name.
fn valid_queue_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let count = trimmed.chars().count();
    if !(1..=80).contains(&count) || name.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.to_string())
}

/// The validated saved queues in a stored value (Python
/// `saved_queues`): bad names, case-folded duplicates and
/// bad entries are dropped; a queue with no valid entry
/// left is dropped; at most [`MAX_SAVED_QUEUES`] survive.
#[must_use]
pub fn saved_queues(value: &serde_json::Value) -> Vec<(String, Vec<LibraryRow>)> {
    let mut result: Vec<(String, Vec<LibraryRow>)> = Vec::new();
    let Some(object) = value.as_object() else {
        return result;
    };
    for (name, entries) in object {
        let Some(name) = valid_queue_name(name) else {
            continue;
        };
        if result
            .iter()
            .any(|(existing, _)| existing.to_lowercase() == name.to_lowercase())
        {
            continue;
        }
        let Some(entries) = entries.as_array() else {
            continue;
        };
        let rows: Vec<LibraryRow> = entries
            .iter()
            .take(MAX_QUEUE_ENTRIES)
            .filter_map(entry_row)
            .collect();
        if rows.is_empty() {
            continue;
        }
        result.push((name, rows));
        if result.len() >= MAX_SAVED_QUEUES {
            break;
        }
    }
    result
}

/// The stored JSON form of validated saved queues.
#[must_use]
pub fn saved_queues_json(queues: &[(String, Vec<LibraryRow>)]) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for (name, rows) in queues {
        object.insert(
            name.clone(),
            serde_json::Value::Array(rows.iter().map(entry_json).collect()),
        );
    }
    serde_json::Value::Object(object)
}

/// Save the current queue under a name (Python `save_named`).
///
/// # Errors
/// Python's messages: an unusable name or empty queue, a
/// case-folded name clash without `replace`, the saved
/// queue limit, or entries that fail validation.
pub fn save_named(
    value: &serde_json::Value,
    name: &str,
    rows: &[LibraryRow],
    replace: bool,
) -> Result<Vec<(String, Vec<LibraryRow>)>, String> {
    let single = saved_queues_json(&vec![(name.to_string(), rows.to_vec())]);
    let entries = saved_queues(&single);
    if entries.is_empty() {
        return Err(
            "Enter a queue name of 1 to 80 characters and include a recording.".to_string(),
        );
    }
    let count = rows.len();
    let (name, rows) = entries.into_iter().next().expect("one validated queue");
    let mut queues = saved_queues(value);
    let existing = queues
        .iter()
        .position(|(key, _)| key.to_lowercase() == name.to_lowercase());
    if let Some(position) = existing {
        if !replace {
            return Err("A saved queue with this name already exists.".to_string());
        }
        queues.remove(position);
    } else if queues.len() >= MAX_SAVED_QUEUES {
        return Err("The saved queue limit has been reached.".to_string());
    }
    if rows.len() != count {
        return Err("The queue could not be saved.".to_string());
    }
    queues.push((name, rows));
    Ok(queues)
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
        // PR #46: success only marks the head as opening; it
        // is consumed when playback is confirmed started.
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.opening().map(|row| row.title.as_str()), Some("A"));
        assert!(!queue.loading());
        assert!(queue.started(ticket, 5, 5));
        assert_eq!(queue.len(), 1);
        assert!(queue.opening().is_none());
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
    fn unconfirmed_opening_stays_queued_for_a_retry() {
        // Python: an entry whose playback never confirms is
        // not lost; a stale generation confirms nothing, and
        // stopping keeps the row queued.
        let mut queue = RecordingQueue::new();
        queue.add(vec![row("C:\\a.mkv", "A"), row("C:\\b.mkv", "B")]);
        let ticket = queue.start().expect("starts");
        queue.begin_next(false);
        assert!(queue.finish_next(ticket, false, None).is_none());
        assert!(!queue.started(ticket, 5, 6), "stale generation");
        assert_eq!(queue.len(), 2);
        queue.stop();
        assert!(queue.opening().is_none());
        assert_eq!(queue.len(), 2, "the unconfirmed entry is kept");
        // A fresh start hands the same head off again.
        let ticket = queue.start().expect("restarts");
        let (head, _) = queue.begin_next(false).expect("head again");
        assert_eq!(head.title, "A");
        assert!(queue.finish_next(ticket, false, None).is_none());
        assert!(queue.started(ticket, 1, 1));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn saved_queue_round_trip_order_and_confirmation() {
        // Python test_saved_queue_round_trip_order_and_confirmation.
        let base = std::env::temp_dir().to_string_lossy().into_owned();
        let mut first = row(&format!("{base}/b.mkv"), "b.mkv");
        first.directory = base.clone();
        first.size = 12;
        first.modified_ns = 1;
        let mut second = row(&format!("{base}/a.mkv"), "a.mkv");
        second.directory = base;
        second.size = 12;
        second.modified_ns = 1;
        let rows = vec![first, second];
        let data = save_named(&serde_json::json!({}), "Este", &rows, false).expect("saves");
        let stored = saved_queues_json(&data);
        assert_eq!(saved_queues(&stored), data);
        assert_eq!(saved_queues(&stored)[0].1, rows);
        // A case-folded clash needs replace=True.
        assert_eq!(
            save_named(&stored, "ESTE", &rows, false),
            Err("A saved queue with this name already exists.".to_string())
        );
        let replaced = save_named(&stored, "ESTE", &rows.iter().rev().cloned().collect::<Vec<_>>(), true)
            .expect("replaces");
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].1, rows.iter().rev().cloned().collect::<Vec<_>>());
        // A wholly invalid stored queue validates to nothing.
        assert!(saved_queues(&serde_json::json!({
            "invalid": [{"path": "https://example.invalid"}]
        }))
        .is_empty());
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

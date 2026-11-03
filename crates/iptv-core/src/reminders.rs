//! Programme reminders, ported from Python's `programme_reminders.py`:
//! bounded, durable rows with content-derived ids, no media URLs, timers or
//! GUI objects stored.

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::epg::Programme;
use crate::favorites;
use crate::Channel;

pub const MAX_REMINDERS: usize = 500;
const MAX_TIME: i64 = 253_402_300_799;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderStatus {
    Scheduled,
    Ready,
    Notified,
    Expired,
}

/// One stored reminder row (Python's dict shape).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub channel_key: String,
    pub title: String,
    pub channel: String,
    pub start: i64,
    pub end: i64,
    pub lead: i64,
    pub status: ReminderStatus,
}

/// The reminder-scope key for a channel: sha256 of the Python
/// `json.dumps([source, identity])` pair, where source is the provider or
/// playlist scope id and identity is the favorites channel key.
#[must_use]
pub fn reminder_channel_key(channel: &Channel) -> String {
    let source = channel
        .unknown_attributes
        .get("provider-id")
        .or_else(|| channel.unknown_attributes.get("playlist-id"))
        .map_or("", String::as_str);
    let identity = favorites::channel_key(channel);
    if identity.is_empty() {
        return String::new();
    }
    sha256_hex(py_json_pair(source, &identity).as_bytes())
}

/// Python `json.dumps` of a two-string list: `["a", "b"]` with ASCII
/// escaping, as the reminder hashes were defined over.
fn py_json_pair(first: &str, second: &str) -> String {
    format!("[{}, {}]", py_json_string(first), py_json_string(second))
}

fn py_json_string(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = sha2::Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Python `safe_title`: trimmed, URL-like or control-char text dropped,
/// capped at 240 characters.
#[must_use]
pub fn safe_title(value: &str) -> String {
    let text = value.trim();
    let lowered = text.to_lowercase();
    if lowered.contains("://") || lowered.contains("www.") || text.chars().any(char::is_control) {
        return String::new();
    }
    text.chars().take(240).collect()
}

fn valid_time(value: i64) -> bool {
    (0..=MAX_TIME).contains(&value)
}

/// Build a reminder for a future programme (Python `build`).
///
/// # Errors
///
/// Returns an error for an identity-less channel, an already-started or
/// malformed programme, and leads outside 0..=120 minutes.
pub fn build_reminder(
    channel: &Channel,
    programme: &Programme,
    lead: i64,
    now: i64,
) -> Result<Reminder, &'static str> {
    let source = reminder_channel_key(channel);
    if source.is_empty()
        || !valid_time(programme.start)
        || !valid_time(programme.stop)
        || programme.start <= now
        || programme.stop <= programme.start
        || !(0..=120).contains(&lead)
    {
        return Err("invalid or already-started programme");
    }
    // Python hashes json.dumps([source, start]) with start a float.
    let id_input = format!("[{}, {}.0]", py_json_string(&source), programme.start);
    Ok(Reminder {
        id: sha256_hex(id_input.as_bytes()),
        channel_key: source,
        title: safe_title(&programme.title),
        channel: safe_title(favorites::display_name(channel)),
        start: programme.start,
        end: programme.stop,
        lead,
        status: ReminderStatus::Scheduled,
    })
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Validate and bound stored rows (Python `normalize`): ids must recompute
/// from their channel key and start; active reminders sort first.
#[must_use]
pub fn normalize_reminders(rows: Vec<Reminder>) -> Vec<Reminder> {
    let mut by_id: std::collections::HashMap<String, Reminder> = std::collections::HashMap::new();
    for row in rows {
        let recomputed = format!("[{}, {}.0]", py_json_string(&row.channel_key), row.start);
        if !is_hex64(&row.id)
            || !is_hex64(&row.channel_key)
            || sha256_hex(recomputed.as_bytes()) != row.id
            || !valid_time(row.start)
            || !valid_time(row.end)
            || row.end <= row.start
            || !(0..=120).contains(&row.lead)
        {
            continue;
        }
        by_id.insert(
            row.id.clone(),
            Reminder {
                title: safe_title(&row.title),
                channel: safe_title(&row.channel),
                ..row
            },
        );
    }
    let mut result: Vec<Reminder> = by_id.into_values().collect();
    result.sort_by(|a, b| {
        let a_active = matches!(a.status, ReminderStatus::Scheduled | ReminderStatus::Ready);
        let b_active = matches!(b.status, ReminderStatus::Scheduled | ReminderStatus::Ready);
        b_active
            .cmp(&a_active)
            .then_with(|| {
                if a_active {
                    a.start.cmp(&b.start)
                } else {
                    b.start.cmp(&a.start)
                }
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    result.truncate(MAX_REMINDERS);
    result
}

impl Reminder {
    /// True when the lead window has opened and the reminder still waits.
    #[must_use]
    pub fn is_due(&self, now: i64) -> bool {
        self.status == ReminderStatus::Scheduled && now >= self.start - self.lead * 60
    }

    /// True when the programme has ended without firing.
    #[must_use]
    pub fn is_expired(&self, now: i64) -> bool {
        matches!(
            self.status,
            ReminderStatus::Scheduled | ReminderStatus::Ready
        ) && now > self.end
    }
}

/// The next reminder to deliver, one at a time (Python's reminder
/// poll in `planning_ui.py`): a clone of the first row in stored
/// order whose lead window has opened, whose programme has not yet
/// ended, and whose id is not in `seen` (already shown but not yet
/// acknowledged). Ready rows count as due — Python's `due()` promotes
/// scheduled rows to ready inside the window — while
/// [`Reminder::is_due`] only names the scheduled state. The caller
/// adds the returned id to `seen`, speaks the reminder, then calls
/// [`acknowledge`]; each reminder is therefore delivered exactly
/// once even when several fall due together.
#[must_use]
pub fn next_pending_delivery<S: std::hash::BuildHasher>(
    rows: &[Reminder],
    seen: &std::collections::HashSet<String, S>,
    now: i64,
) -> Option<Reminder> {
    rows.iter()
        .find(|row| {
            !seen.contains(&row.id)
                && matches!(
                    row.status,
                    ReminderStatus::Scheduled | ReminderStatus::Ready
                )
                && now >= row.start - row.lead * 60
                && now < row.end
        })
        .cloned()
}

/// Mark one delivered reminder as notified (Python's
/// `acknowledge`): only a Scheduled or Ready row changes, so a
/// reminder is never acknowledged twice. Returns whether a row
/// changed.
pub fn acknowledge(rows: &mut [Reminder], id: &str) -> bool {
    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
        return false;
    };
    if !matches!(
        row.status,
        ReminderStatus::Scheduled | ReminderStatus::Ready
    ) {
        return false;
    }
    row.status = ReminderStatus::Notified;
    true
}

/// Edit a reminder's lead time (Python's `ReminderStore.set_lead`):
/// the lead must be within 0..=120 minutes and the programme must
/// not have started yet. Editing re-arms the reminder — its status
/// resets to Scheduled, like Python.
///
/// # Errors
///
/// Returns an error when the lead is out of range, the row is
/// missing, or the programme has already started.
pub fn set_lead(rows: &mut [Reminder], id: &str, lead: i64, now: i64) -> Result<(), &'static str> {
    if !(0..=120).contains(&lead) {
        return Err("invalid reminder lead time");
    }
    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
        return Err("invalid reminder lead time");
    };
    if row.start <= now {
        return Err("invalid reminder lead time");
    }
    row.lead = lead;
    row.status = ReminderStatus::Scheduled;
    Ok(())
}

/// The status label Python's Reminders dialog shows for a row
/// (`planning_ui.py`): a scheduled or ready row past its end reads
/// Expired; a ready row, or a scheduled row inside its lead window,
/// reads Pending; notified and expired rows keep their names;
/// anything else reads Scheduled.
#[must_use]
pub fn display_status(reminder: &Reminder, now: i64) -> &'static str {
    if reminder.end <= now
        && matches!(
            reminder.status,
            ReminderStatus::Scheduled | ReminderStatus::Ready
        )
    {
        return "Expired";
    }
    match reminder.status {
        ReminderStatus::Ready => "Pending",
        ReminderStatus::Scheduled if now >= reminder.start - reminder.lead * 60 => "Pending",
        ReminderStatus::Notified => "Notified",
        ReminderStatus::Expired => "Expired",
        ReminderStatus::Scheduled => "Scheduled",
    }
}

/// Resolve a reminder's channel for Open Channel (port of Python's
/// `programme_reminders.resolve_channel`): the channel whose
/// reminder key matches the row's, returned only when exactly one
/// distinct match exists. Python keys matches by (url, headers);
/// url distinctness is the practical equivalent here — two entries
/// sharing a url are the same channel for opening purposes, while
/// two different urls under one key stay ambiguous and resolve to
/// nothing.
#[must_use]
pub fn find_channel_for_reminder<'a>(
    row: &Reminder,
    channels: &'a [Channel],
) -> Option<&'a Channel> {
    let mut found: Option<&Channel> = None;
    let mut seen_urls: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for channel in channels {
        if channel.url.is_empty() || reminder_channel_key(channel) != row.channel_key {
            continue;
        }
        if !seen_urls.insert(channel.url.as_str()) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(channel);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn channel() -> Channel {
        let mut attrs = BTreeMap::new();
        attrs.insert("playlist-id".to_string(), "m3u:abc".to_string());
        Channel {
            name: "News Channel".to_string(),
            group: String::new(),
            url: "https://example.com/news".to_string(),
            tvg_id: "news.example".to_string(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs,
        }
    }

    fn programme(start: i64) -> Programme {
        Programme {
            channel_id: "news.example".to_string(),
            start,
            stop: start + 3600,
            title: "Evening News".to_string(),
            description: String::new(),
        }
    }

    #[test]
    fn build_and_normalize_round_trip() {
        let now = 1_000_000;
        let reminder = build_reminder(&channel(), &programme(now + 7200), 5, now).expect("build");
        assert_eq!(reminder.status, ReminderStatus::Scheduled);
        assert_eq!(reminder.id.len(), 64);
        let kept = normalize_reminders(vec![reminder.clone()]);
        assert_eq!(kept, vec![reminder]);
    }

    #[test]
    fn build_rejects_started_and_bad_lead() {
        let now = 1_000_000;
        assert!(build_reminder(&channel(), &programme(now - 10), 5, now).is_err());
        assert!(build_reminder(&channel(), &programme(now + 7200), 121, now).is_err());
    }

    #[test]
    fn tampered_rows_drop() {
        let now = 1_000_000;
        let mut reminder = build_reminder(&channel(), &programme(now + 7200), 5, now).expect("b");
        reminder.start += 60; // id no longer recomputes
        assert!(normalize_reminders(vec![reminder]).is_empty());
    }

    #[test]
    fn hashes_match_python_goldens() {
        // Computed with the pinned Python programme_reminders hashing on
        // this fixture channel (identity "news.example|News Channel").
        assert_eq!(
            reminder_channel_key(&channel()),
            "ec78d18fa17bb0c0c1b1cf4704434f71be84abbcdd5b69969dced18eaf708695"
        );
        let reminder =
            build_reminder(&channel(), &programme(1_762_000_000), 5, 1_000_000).expect("build");
        assert_eq!(
            reminder.id,
            "cb3b5edb27f2c991e0f57963ea2273244631113327cfbdd332595ccdfcd2e3d9"
        );
    }

    #[test]
    fn due_window_uses_lead() {
        let now = 1_000_000;
        let reminder = build_reminder(&channel(), &programme(now + 7200), 5, now).expect("b");
        assert!(!reminder.is_due(now));
        assert!(reminder.is_due(now + 7200 - 300));
    }

    #[test]
    fn simultaneous_reminders_deliver_one_at_a_time_exactly_once() {
        let now = 1_000_000;
        let chan = channel();
        // Distinct starts give distinct ids; at the third row's
        // window opening all three are due together.
        let mut rows: Vec<Reminder> = [7200, 7260, 7320]
            .iter()
            .map(|offset| build_reminder(&chan, &programme(now + offset), 5, now).expect("b"))
            .collect();
        let due_now = rows[2].start - 300;
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut delivered: Vec<String> = Vec::new();
        // First delivery: the first row in stored order.
        let first = next_pending_delivery(&rows, &seen, due_now).expect("first");
        assert_eq!(first.id, ids[0]);
        delivered.push(first.id.clone());
        seen.insert(first.id);
        // Shown but unacknowledged: the seen set, not the row
        // status, moves delivery on to the second row.
        assert_eq!(rows[0].status, ReminderStatus::Scheduled);
        let second = next_pending_delivery(&rows, &seen, due_now).expect("second");
        assert_eq!(second.id, ids[1]);
        delivered.push(second.id.clone());
        seen.insert(second.id);
        let third = next_pending_delivery(&rows, &seen, due_now).expect("third");
        assert_eq!(third.id, ids[2]);
        delivered.push(third.id.clone());
        seen.insert(third.id);
        assert!(next_pending_delivery(&rows, &seen, due_now).is_none());
        // Each id was delivered exactly once.
        let unique: std::collections::HashSet<&String> = delivered.iter().collect();
        assert_eq!(unique.len(), 3);
        // After acknowledging all three, nothing delivers again,
        // even with an empty seen set.
        for id in &ids {
            assert!(acknowledge(&mut rows, id));
        }
        assert!(rows
            .iter()
            .all(|row| row.status == ReminderStatus::Notified));
        assert!(!acknowledge(&mut rows, &ids[0]));
        let empty: std::collections::HashSet<String> = std::collections::HashSet::new();
        assert!(next_pending_delivery(&rows, &empty, due_now).is_none());
    }

    #[test]
    fn set_lead_edits_future_rows_and_rearms() {
        let now = 1_000_000;
        let mut rows = vec![build_reminder(&channel(), &programme(now + 7200), 5, now).expect("b")];
        let id = rows[0].id.clone();
        for lead in [0, 30, 120] {
            rows[0].status = ReminderStatus::Ready;
            assert!(set_lead(&mut rows, &id, lead, now).is_ok());
            assert_eq!(rows[0].lead, lead);
            // Editing re-arms the reminder.
            assert_eq!(rows[0].status, ReminderStatus::Scheduled);
        }
        assert!(set_lead(&mut rows, &id, 121, now).is_err());
        assert!(set_lead(&mut rows, &id, -1, now).is_err());
        assert!(set_lead(&mut rows, "no-such-id", 30, now).is_err());
        // A programme that has already started cannot be edited.
        let start = rows[0].start;
        assert!(set_lead(&mut rows, &id, 30, start).is_err());
        assert!(set_lead(&mut rows, &id, 30, start + 1).is_err());
    }

    #[test]
    fn display_status_follows_python_labels() {
        let now = 1_000_000;
        let reminder = build_reminder(&channel(), &programme(now + 7200), 5, now).expect("b");
        // Scheduled, outside the lead window.
        assert_eq!(display_status(&reminder, now), "Scheduled");
        // Inside the lead window (start - lead*60).
        assert_eq!(display_status(&reminder, reminder.start - 300), "Pending");
        // Past the end while still scheduled.
        assert_eq!(display_status(&reminder, reminder.end), "Expired");
        // A ready row reads Pending even before its window, and
        // Expired once past its end.
        let mut ready = reminder.clone();
        ready.status = ReminderStatus::Ready;
        assert_eq!(display_status(&ready, now), "Pending");
        assert_eq!(display_status(&ready, reminder.end), "Expired");
        // Notified and Expired keep their names.
        let mut notified = reminder.clone();
        notified.status = ReminderStatus::Notified;
        assert_eq!(display_status(&notified, now), "Notified");
        let mut expired = reminder;
        expired.status = ReminderStatus::Expired;
        assert_eq!(display_status(&expired, now), "Expired");
    }

    #[test]
    fn find_channel_for_reminder_needs_a_unique_url() {
        let now = 1_000_000;
        let chan = channel();
        let row = build_reminder(&chan, &programme(now + 7200), 5, now).expect("b");
        // A unique match resolves.
        let one = [chan.clone()];
        let found = find_channel_for_reminder(&row, &one).expect("found");
        assert_eq!(found.url, "https://example.com/news");
        // The same url twice is still one distinct match.
        let two = [chan.clone(), chan.clone()];
        let found = find_channel_for_reminder(&row, &two).expect("found");
        assert_eq!(found.url, "https://example.com/news");
        // Two channels sharing the key with different urls are
        // ambiguous: the url is not part of the reminder key, so
        // only the address changes.
        let mut other_url = chan.clone();
        other_url.url = "https://example.com/news-mirror".to_string();
        assert_eq!(reminder_channel_key(&other_url), row.channel_key);
        assert!(find_channel_for_reminder(&row, &[chan.clone(), other_url]).is_none());
        // No matching channel resolves to nothing.
        let mut unrelated = chan.clone();
        unrelated.name = "Other Channel".to_string();
        unrelated.tvg_id = "other.example".to_string();
        assert!(find_channel_for_reminder(&row, &[unrelated]).is_none());
        assert!(find_channel_for_reminder(&row, &[]).is_none());
    }
}

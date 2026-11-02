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
}

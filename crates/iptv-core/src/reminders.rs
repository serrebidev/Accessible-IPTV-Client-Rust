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

/// Whole epoch seconds written as Python writes them: a JSON float
/// (`1795000000.0`), since Python's ids hash `json.dumps([key, start])`
/// over that float; read from an integer or a float.
mod float_secs {
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::cast_precision_loss, clippy::trivially_copy_pass_by_ref)] // epoch seconds fit an f64; serde's `with` signature
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(*value as f64)
    }

    #[allow(clippy::cast_possible_truncation)] // whole seconds; Python's are too
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if value.is_finite() {
            Ok(value.floor() as i64)
        } else {
            Err(serde::de::Error::custom("time is not finite"))
        }
    }

    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};

        #[allow(clippy::ref_option, clippy::cast_precision_loss)] // serde's `with` signature
        pub fn serialize<S: Serializer>(
            value: &Option<i64>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(value) => serializer.serialize_f64(*value as f64),
                None => serializer.serialize_none(),
            }
        }

        #[allow(clippy::cast_possible_truncation)] // whole seconds
        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<i64>, D::Error> {
            let value = Option::<f64>::deserialize(deserializer)?;
            Ok(value.filter(|v| v.is_finite()).map(|v| v.floor() as i64))
        }
    }
}

/// One stored reminder row (Python's dict shape).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub channel_key: String,
    pub title: String,
    pub channel: String,
    #[serde(with = "float_secs")]
    pub start: i64,
    #[serde(with = "float_secs")]
    pub end: i64,
    pub lead: i64,
    pub status: ReminderStatus,
    /// A postponed reminder's next alert time (Python `notify_at`,
    /// Remind Again / Remind at Programme Start).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "float_secs::option"
    )]
    pub notify_at: Option<i64>,
    /// sha256 of the guide channel id the reminder was booked
    /// against (Python `guide_fingerprint`, PR #44); kept only
    /// when it is a 64-char hex digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guide_fingerprint: Option<String>,
    /// sha256 of the booked programme's episode key (Python
    /// `episode_fingerprint`, PR #44).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_fingerprint: Option<String>,
}

/// Python's reminder file beside the schedule.
#[must_use]
pub fn reminders_path(schedule_path: &std::path::Path) -> std::path::PathBuf {
    schedule_path.with_file_name("programme-reminders.json")
}

/// What reading the reminder file gave.
#[derive(Debug, Default)]
pub struct ReminderLoad {
    pub rows: Vec<Reminder>,
    /// Python's warning when rows were skipped or the file could not
    /// be read; empty otherwise.
    pub warning: String,
    /// The original file could not be preserved: writes are refused.
    pub blocked: bool,
    /// The file did not exist.
    pub missing: bool,
}

/// Python `ReminderStore.__init__`: read `{"version": 1, "reminders":
/// [...]}`; when any row is invalid (or the file cannot be read), the
/// original bytes are copied to `<path>.corrupt-<hex>` first, and if
/// that fails every later write is refused.
#[must_use]
pub fn load_reminder_file(path: &std::path::Path) -> ReminderLoad {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ReminderLoad {
                missing: true,
                ..ReminderLoad::default()
            };
        }
        Err(_) => return preserve_reminder_file(path, Vec::new()),
    };
    let parsed = (bytes.len() <= 2 * 1024 * 1024)
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .flatten()
        .filter(|data| data.get("version") == Some(&serde_json::json!(1)));
    let Some(serde_json::Value::Array(raw)) =
        parsed.and_then(|mut data| data.get_mut("reminders").map(serde_json::Value::take))
    else {
        return preserve_reminder_file(path, Vec::new());
    };
    let total = raw.len();
    let rows: Vec<Reminder> = raw
        .into_iter()
        .filter_map(|row| serde_json::from_value(row).ok())
        .collect();
    let rows = normalize_reminders(rows);
    if rows.len() == total {
        return ReminderLoad {
            rows,
            ..ReminderLoad::default()
        };
    }
    preserve_reminder_file(path, rows)
}

fn preserve_reminder_file(path: &std::path::Path, rows: Vec<Reminder>) -> ReminderLoad {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let suffix = &sha256_hex(format!("{}:{nanos}", std::process::id()).as_bytes())[..32];
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".corrupt-{suffix}"));
    let backup = std::path::PathBuf::from(name);
    let copied = (|| -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()
    })();
    if copied.is_ok() {
        ReminderLoad {
            rows,
            warning: format!(
                "Invalid reminders were skipped. The original file was preserved at {}.",
                backup.display()
            ),
            ..ReminderLoad::default()
        }
    } else {
        let _ = std::fs::remove_file(&backup);
        ReminderLoad {
            rows,
            warning: "Invalid reminders could not be backed up. Changes are disabled to protect the original file.".to_string(),
            blocked: true,
            missing: false,
        }
    }
}

/// Python `ReminderStore._commit`: the normalized rows, written to a
/// synced temp file renamed over the old one.
///
/// # Errors
/// Returns the I/O error; the previous file is then untouched.
pub fn save_reminder_file(path: &std::path::Path, rows: &[Reminder]) -> Result<(), String> {
    let normalized = normalize_reminders(rows.to_vec());
    let text = serde_json::to_string(&serde_json::json!({"version": 1, "reminders": normalized}))
        .map_err(|e| e.to_string())?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temporary = directory.join(format!("reminders-{}-{nanos}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
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

/// Python `identity_digest`: the sha256 hex of a value's text,
/// empty for an empty value.
#[must_use]
pub fn identity_digest(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        sha256_hex(value.as_bytes())
    }
}

/// Python `portal_identity`: a Stalker channel's stable identity
/// as a four-part tuple — the portal scope, then whether the row
/// id or the play command names it — or None for other channels.
/// Rust channels carry the portal row's fields as flat
/// attributes.
#[must_use]
pub fn portal_identity(channel: &Channel) -> Option<[String; 4]> {
    let attr = |key: &str| channel.unknown_attributes.get(key).map_or("", |v| v.trim());
    if attr("provider-type") != "stalker" || attr("provider-id").is_empty() {
        return None;
    }
    let cmd = attr("cmd");
    if cmd.is_empty() {
        return None;
    }
    let provider = attr("provider-id").to_string();
    let id = attr("id");
    if id.is_empty() {
        Some([
            "stalker".to_string(),
            provider,
            "cmd".to_string(),
            cmd.to_string(),
        ])
    } else {
        Some([
            "stalker".to_string(),
            provider,
            "id".to_string(),
            id.to_string(),
        ])
    }
}

/// Python `station_key`: the portal identity's hash for a
/// Stalker channel, the reminder channel key otherwise. Rows
/// stored before PR #44 carry the channel key, so resolvers
/// accept both.
#[must_use]
pub fn station_key(channel: &Channel) -> String {
    match portal_identity(channel) {
        Some(portal) => sha256_hex(py_json_list(&portal).as_bytes()),
        None => reminder_channel_key(channel),
    }
}

/// Python `playable`: a channel the app can open — a stream URL,
/// or a portal identity resolved on demand.
#[must_use]
pub fn playable(channel: &Channel) -> bool {
    !channel.url.is_empty() || portal_identity(channel).is_some()
}

/// Python `media_identity`: what distinguishes two listings of
/// one channel — the portal identity for a Stalker channel,
/// else the stream URL with its request headers as sorted JSON.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MediaIdentity {
    /// A Stalker portal identity tuple.
    Portal([String; 4]),
    /// Stream URL and its headers JSON.
    Stream(String, String),
}

/// Python `media_identity` for a channel.
#[must_use]
pub fn media_identity(channel: &Channel) -> MediaIdentity {
    if let Some(portal) = portal_identity(channel) {
        return MediaIdentity::Portal(portal);
    }
    let mut headers = crate::downloads::channel_headers(channel);
    headers.sort();
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, value) in headers {
        map.insert(name, value);
    }
    let inner = map
        .iter()
        .map(|(k, v)| format!("{}: {}", py_json_string(k), py_json_string(v)))
        .collect::<Vec<_>>()
        .join(", ");
    MediaIdentity::Stream(channel.url.clone(), format!("{{{inner}}}"))
}

/// Python `json.dumps` of a string list, ASCII-escaped.
fn py_json_list(values: &[String]) -> String {
    let parts: Vec<String> = values.iter().map(|v| py_json_string(v)).collect();
    format!("[{}]", parts.join(", "))
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
    let source = station_key(channel);
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
    let digest = |value: &str| {
        let digest = identity_digest(value);
        (!digest.is_empty()).then_some(digest)
    };
    Ok(Reminder {
        id: sha256_hex(id_input.as_bytes()),
        channel_key: source,
        title: safe_title(&programme.title),
        channel: safe_title(favorites::display_name(channel)),
        start: programme.start,
        end: programme.stop,
        lead,
        status: ReminderStatus::Scheduled,
        notify_at: None,
        guide_fingerprint: digest(&programme.channel_id),
        episode_fingerprint: crate::dvr::program_episode_key(&programme_value(programme))
            .as_deref()
            .and_then(digest),
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
        // Python keeps a postponement only inside the alert window.
        let notify_at = row
            .notify_at
            .filter(|at| valid_time(*at) && row.start - row.lead * 60 <= *at && *at < row.end);
        let keep_digest = |value: &Option<String>| value.as_ref().filter(|v| is_hex64(v)).cloned();
        by_id.insert(
            row.id.clone(),
            Reminder {
                title: safe_title(&row.title),
                channel: safe_title(&row.channel),
                notify_at,
                guide_fingerprint: keep_digest(&row.guide_fingerprint),
                episode_fingerprint: keep_digest(&row.episode_fingerprint),
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
    /// When the alert falls due: a postponement's time, else the lead
    /// before the start (Python `row.get('notify_at', start - lead*60)`).
    #[must_use]
    pub fn alert_at(&self) -> i64 {
        self.notify_at.unwrap_or(self.start - self.lead * 60)
    }

    /// True when the lead window has opened and the reminder still waits.
    #[must_use]
    pub fn is_due(&self, now: i64) -> bool {
        self.status == ReminderStatus::Scheduled && now >= self.alert_at()
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
                && now >= row.alert_at()
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
    row.notify_at = None;
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
        ReminderStatus::Scheduled if now >= reminder.alert_at() => "Pending",
        ReminderStatus::Notified => "Notified",
        ReminderStatus::Expired => "Expired",
        ReminderStatus::Scheduled => "Scheduled",
    }
}

/// Postpone a reminder (Python `ReminderStore.snooze`): five
/// minutes by default, or to the programme start; the new
/// alert must land strictly between now and the programme's
/// end, and an expired reminder cannot be postponed.
///
/// # Errors
/// Python's messages for an invalid postponement and for a
/// target at or beyond the programme's end.
pub fn snooze(
    rows: &mut [Reminder],
    id: &str,
    minutes: i64,
    at_start: bool,
    now: i64,
) -> Result<(), String> {
    if !valid_time(now) || !(1..=120).contains(&minutes) {
        return Err("Invalid reminder postponement.".to_string());
    }
    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
        return Err("The reminder cannot be postponed beyond the programme's end.".to_string());
    };
    let target = if at_start {
        row.start
    } else {
        now + minutes * 60
    };
    if row.status == ReminderStatus::Expired || !(now < target && target < row.end) {
        return Err("The reminder cannot be postponed beyond the programme's end.".to_string());
    }
    row.status = ReminderStatus::Scheduled;
    row.notify_at = Some(target);
    Ok(())
}

/// Record the alert dialog's choice (Python
/// `ReminderStore.save_alert_choice`): a retry keeps the
/// originally requested time and respects subsequent edits.
/// Only a due row changes: past its end it expires; a target
/// already past, or at/after the end, leaves it due so the
/// next poll can show it again; otherwise it re-arms as
/// scheduled at the target. Python's store only ever holds
/// due rows as `ready` (its poll promotes them); this port's
/// poll treats scheduled rows in their window as due without
/// promoting, so both states count as due here. Returns
/// whether a row changed.
pub fn save_alert_choice(rows: &mut [Reminder], id: &str, target: i64, now: i64) -> bool {
    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
        return false;
    };
    if !matches!(
        row.status,
        ReminderStatus::Scheduled | ReminderStatus::Ready
    ) {
        return false;
    }
    if row.end <= now {
        row.status = ReminderStatus::Expired;
        return true;
    }
    if target <= now || target >= row.end {
        return false;
    }
    row.status = ReminderStatus::Scheduled;
    row.notify_at = Some(target);
    true
}

/// Playlist source display names (Python
/// `programme_reminders.source_names`): each playlist's id
/// (or provider id) mapped to its cleaned name, plus the
/// `playlist_names` aliases keyed `m3u:<sha256>`. Used by the
/// favourite transfer, the guide-by-time scope and recording
/// edits; ported with M4 as the spec's scope sweep directs.
#[must_use]
pub fn source_names(config: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let mut names: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Some(sources) = config
        .get("playlists")
        .and_then(serde_json::Value::as_array)
    {
        for source in sources {
            let key = source
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    source
                        .get("provider_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                });
            if let Some(key) = key {
                let name = source
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                names.insert(key.to_string(), safe_title(name));
            }
        }
    }
    if let Some(aliases) = config
        .get("playlist_names")
        .and_then(serde_json::Value::as_object)
    {
        for (key, name) in aliases {
            if key.len() == 64
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                let name = name.as_str().unwrap_or("");
                names.insert(format!("m3u:{key}"), safe_title(name));
            }
        }
    }
    names
}

/// The programme as the JSON shape `program_episode_key` reads.
fn programme_value(programme: &Programme) -> serde_json::Value {
    serde_json::json!({
        "title": programme.title,
        "episode": programme.episode,
        "subtitle": programme.subtitle,
        "description": programme.description,
        "channel_id": programme.channel_id,
    })
}

/// Move a reminder to the guide's new time for its programme
/// (Python `ReminderStore.retime`): the stored row must still
/// equal `expected`, still be scheduled, and neither the old nor
/// the new start may have passed. A kept postponement shifts by
/// the same offset and must land inside the new window. The
/// caller saves the rows afterwards. Returns the new row id.
///
/// # Errors
///
/// Python's messages for a changed or started reminder, a
/// duplicate at the new time, and a postponement that falls
/// outside the new window.
pub fn retime_reminder(
    rows: &mut [Reminder],
    expected: &Reminder,
    programme: &Programme,
    keep_snooze: bool,
    now: i64,
) -> Result<String, String> {
    let start = programme.start;
    let end = programme.stop;
    let changed = "The reminder changed or the programme has started. Review the times again.";
    let Some(index) = rows.iter().position(|row| row.id == expected.id) else {
        return Err(changed.to_string());
    };
    if rows[index] != *expected
        || rows[index].status != ReminderStatus::Scheduled
        || start.min(rows[index].start) <= now
        || end <= start
    {
        return Err(changed.to_string());
    }
    let id_input = format!(
        "[{}, {}.0]",
        py_json_string(&rows[index].channel_key),
        start
    );
    let key = sha256_hex(id_input.as_bytes());
    if rows
        .iter()
        .any(|other| other.id == key && other.id != rows[index].id)
    {
        return Err("There is already a reminder for the new time.".to_string());
    }
    let postponed = rows[index].notify_at.take();
    if keep_snooze {
        if let Some(postponed) = postponed {
            let target = postponed + start - rows[index].start;
            if !(now < target && target < end) {
                rows[index].notify_at = Some(postponed);
                return Err("The postponed alert falls outside the new time window.".to_string());
            }
            rows[index].notify_at = Some(target);
        }
    }
    let row = &mut rows[index];
    row.id = key.clone();
    row.start = start;
    row.end = end;
    if !programme.channel_id.is_empty() {
        row.guide_fingerprint = Some(identity_digest(&programme.channel_id));
    }
    if let Some(episode) = crate::dvr::program_episode_key(&programme_value(programme)) {
        if !episode.is_empty() {
            row.episode_fingerprint = Some(identity_digest(&episode));
        }
    }
    Ok(key)
}

/// Reminders whose programme moved in the guide (Python
/// `guide_changes`): each pending reminder is offered to
/// `changed_guide_times` as a booking on its resolved channel,
/// its fingerprints riding in the job's extra fields.
///
/// # Errors
/// Returns the guide store's error text.
pub fn guide_changes(
    store: &crate::epgstore::EpgStore,
    rows: &[Reminder],
    channels: &[Channel],
    now: i64,
    guide_for: &crate::recording_edits::GuideFor<'_>,
    canceled: &dyn Fn() -> bool,
) -> Result<Vec<(Reminder, Programme, bool)>, String> {
    let pending: Vec<&Reminder> = rows
        .iter()
        .filter(|row| row.status == ReminderStatus::Scheduled && row.start > now)
        .collect();
    let wanted: std::collections::HashSet<&str> =
        pending.iter().map(|row| row.channel_key.as_str()).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut stations: std::collections::HashMap<String, Vec<Channel>> =
        std::collections::HashMap::new();
    for channel in channels {
        if canceled() {
            return Ok(Vec::new());
        }
        let key = reminder_channel_key(channel);
        let mut keys = vec![key];
        if channel
            .unknown_attributes
            .get("provider-type")
            .is_some_and(|kind| kind == "stalker")
        {
            keys.push(station_key(channel));
        }
        for key in keys {
            if wanted.contains(key.as_str()) {
                stations.entry(key).or_default().push(channel.clone());
            }
        }
    }
    let stamp = |secs: i64| {
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|dt| dt.format("%Y%m%d%H%M%S").to_string())
            .unwrap_or_default()
    };
    let mut jobs = Vec::new();
    for row in &pending {
        let empty: Vec<Channel> = Vec::new();
        let candidates = stations.get(row.channel_key.as_str()).unwrap_or(&empty);
        let Some(channel) = find_channel_for_reminder(row, candidates) else {
            continue;
        };
        let mut job = crate::dvr::DvrJob::default();
        job.id = row.id.clone();
        job.title = row.title.clone();
        job.status = crate::dvr::STATUS_SCHEDULED.to_string();
        #[allow(clippy::cast_precision_loss)] // whole seconds; Python's are floats too
        let start_ts = row.start as f64;
        job.start_ts = start_ts;
        job.channel = crate::dvr::channel_snapshot(&channel);
        job.program = serde_json::json!({
            "title": row.title,
            "start": stamp(row.start),
            "end": stamp(row.end),
        });
        if let Some(value) = &row.guide_fingerprint {
            job.extra
                .insert("guide_fingerprint".to_string(), value.clone().into());
        }
        if let Some(value) = &row.episode_fingerprint {
            job.extra
                .insert("episode_fingerprint".to_string(), value.clone().into());
        }
        jobs.push(job);
    }
    #[allow(clippy::cast_precision_loss)] // whole seconds; Python's are floats too
    let now_f = now as f64;
    let changes =
        crate::recording_edits::changed_guide_times(store, &jobs, now_f, guide_for, canceled)?;
    let lookup: std::collections::HashMap<&str, &Reminder> =
        pending.iter().map(|row| (row.id.as_str(), *row)).collect();
    Ok(changes
        .into_iter()
        .filter_map(|(job, programme, exact)| {
            lookup
                .get(job.id.as_str())
                .map(|row| ((*row).clone(), programme, exact))
        })
        .collect())
}

/// Resolve a reminder's channel for Open Channel (port of Python's
/// `programme_reminders.resolve_channel`): a playable channel
/// whose channel key — or, for a Stalker channel, station key —
/// matches the row's, returned only when exactly one distinct
/// media identity matches.
#[must_use]
pub fn find_channel_for_reminder<'a>(
    row: &Reminder,
    channels: &'a [Channel],
) -> Option<&'a Channel> {
    let mut found: Option<&Channel> = None;
    let mut seen: Vec<MediaIdentity> = Vec::new();
    for channel in channels {
        if !playable(channel) {
            continue;
        }
        let is_stalker = channel
            .unknown_attributes
            .get("provider-type")
            .is_some_and(|kind| kind == "stalker");
        if reminder_channel_key(channel) != row.channel_key
            && !(is_stalker && station_key(channel) == row.channel_key)
        {
            continue;
        }
        let identity = media_identity(channel);
        if seen.contains(&identity) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        seen.push(identity);
        found = Some(channel);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reminder file as Python v1.151.6 writes it (float times, a
    /// postponement) loads and saves back unchanged.
    #[test]
    fn python_reminder_file_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "iptv-reminders-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("programme-reminders.json");
        let built = build_reminder(&channel(), &programme(1_795_000_000), 5, 0).unwrap();
        let mut row = serde_json::to_value(&built).unwrap();
        row["notify_at"] = serde_json::json!(1_794_999_900.0);
        assert_eq!(row["start"], serde_json::json!(1_795_000_000.0));
        let file = serde_json::json!({"version": 1, "reminders": [row.clone()]});
        std::fs::write(&path, file.to_string()).unwrap();
        let loaded = load_reminder_file(&path);
        assert!(loaded.warning.is_empty(), "{}", loaded.warning);
        assert_eq!(loaded.rows.len(), 1);
        assert_eq!(loaded.rows[0].notify_at, Some(1_794_999_900));
        assert_eq!(loaded.rows[0].alert_at(), 1_794_999_900);
        save_reminder_file(&path, &loaded.rows).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, file);

        // An invalid row is skipped and the original kept aside.
        let mut bad = row;
        bad["id"] = serde_json::json!("0".repeat(64));
        let text = serde_json::json!({"version": 1, "reminders": [bad]}).to_string();
        std::fs::write(&path, &text).unwrap();
        let loaded = load_reminder_file(&path);
        assert!(loaded.rows.is_empty() && !loaded.blocked);
        assert!(loaded
            .warning
            .starts_with("Invalid reminders were skipped."));
        let copies = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(copies, 1);
        assert!(load_reminder_file(&dir.join("absent.json")).missing);
        let _ = std::fs::remove_dir_all(dir);
    }
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

    fn stalker_channel() -> Channel {
        let mut attrs = BTreeMap::new();
        attrs.insert("provider-type".to_string(), "stalker".to_string());
        attrs.insert("provider-id".to_string(), "portal-a".to_string());
        attrs.insert(
            "cmd".to_string(),
            "ffmpeg http://portal/live/17".to_string(),
        );
        attrs.insert("id".to_string(), "17".to_string());
        Channel {
            name: "Portal One".to_string(),
            group: String::new(),
            url: String::new(),
            tvg_id: "portal.example".to_string(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs,
        }
    }

    #[test]
    fn stalker_channels_key_by_portal_identity() {
        let channel = stalker_channel();
        assert!(playable(&channel));
        assert_ne!(station_key(&channel), reminder_channel_key(&channel));
        assert_eq!(
            media_identity(&channel),
            MediaIdentity::Portal([
                "stalker".to_string(),
                "portal-a".to_string(),
                "id".to_string(),
                "17".to_string(),
            ])
        );
        // A reminder built on the portal resolves back to it even
        // though the channel has no stream URL.
        let row = build_reminder(&channel, &programme(1_795_000_000), 5, 0).unwrap();
        assert_eq!(row.channel_key, station_key(&channel));
        assert!(row.guide_fingerprint.is_some());
        let channels = vec![channel];
        assert!(find_channel_for_reminder(&row, &channels).is_some());
    }

    #[test]
    fn retime_moves_the_row_and_keeps_a_shifted_snooze() {
        let now = 1_795_000_000;
        let mut rows = vec![build_reminder(&channel(), &programme(now + 7200), 5, 0).unwrap()];
        rows[0].notify_at = Some(now + 7000);
        let expected = rows[0].clone();
        let mut moved = programme(now + 10_800);
        moved.stop = now + 14_400;
        let key = retime_reminder(&mut rows, &expected, &moved, true, now).unwrap();
        assert_eq!(rows[0].id, key);
        assert_eq!(rows[0].start, now + 10_800);
        assert_eq!(rows[0].notify_at, Some(now + 7000 + 3600));
        // A stale snapshot is refused.
        let stale = retime_reminder(&mut rows, &expected, &moved, true, now);
        assert!(stale.is_err());
    }

    fn programme(start: i64) -> Programme {
        Programme {
            channel_id: "news.example".to_string(),
            start,
            stop: start + 3600,
            title: "Evening News".to_string(),
            description: String::new(),
            episode: String::new(),
            subtitle: String::new(),
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

    fn due_row(now: i64) -> Reminder {
        Reminder {
            id: "abc".to_string(),
            channel_key: "def".to_string(),
            title: "Show".to_string(),
            channel: "Chan".to_string(),
            start: now + 3600,
            end: now + 7200,
            lead: 5,
            status: ReminderStatus::Ready,
            notify_at: None,
            guide_fingerprint: None,
            episode_fingerprint: None,
        }
    }

    #[test]
    fn snooze_moves_the_alert_and_validates() {
        let now = 1_000_000;
        let mut rows = vec![due_row(now)];
        snooze(&mut rows, "abc", 5, false, now).expect("snooze");
        assert_eq!(rows[0].status, ReminderStatus::Scheduled);
        assert_eq!(rows[0].notify_at, Some(now + 300));
        // At start targets the programme start.
        let mut rows = vec![due_row(now)];
        snooze(&mut rows, "abc", 5, true, now).expect("at start");
        assert_eq!(rows[0].notify_at, Some(now + 3600));
        // Past the end and bad minutes are refused with Python's lines.
        let mut rows = vec![due_row(now)];
        rows[0].end = now + 120;
        assert_eq!(
            snooze(&mut rows, "abc", 5, false, now),
            Err("The reminder cannot be postponed beyond the programme's end.".to_string())
        );
        let mut rows = vec![due_row(now)];
        assert_eq!(
            snooze(&mut rows, "abc", 0, false, now),
            Err("Invalid reminder postponement.".to_string())
        );
        assert_eq!(
            snooze(&mut rows, "missing", 5, false, now),
            Err("The reminder cannot be postponed beyond the programme's end.".to_string())
        );
    }

    #[test]
    fn save_alert_choice_rearms_expires_or_leaves_due() {
        let now = 1_000_000;
        // A valid target re-arms as scheduled at the target.
        let mut rows = vec![due_row(now)];
        assert!(save_alert_choice(&mut rows, "abc", now + 300, now));
        assert_eq!(rows[0].status, ReminderStatus::Scheduled);
        assert_eq!(rows[0].notify_at, Some(now + 300));
        // A target already past leaves the row due and unchanged.
        let mut rows = vec![due_row(now)];
        assert!(!save_alert_choice(&mut rows, "abc", now - 5, now));
        assert_eq!(rows[0].status, ReminderStatus::Ready);
        assert_eq!(rows[0].notify_at, None);
        // Past the end, the row expires.
        let mut rows = vec![due_row(now)];
        assert!(save_alert_choice(&mut rows, "abc", now + 300, now + 8000));
        assert_eq!(rows[0].status, ReminderStatus::Expired);
        // A notified row never changes.
        let mut rows = vec![due_row(now)];
        rows[0].status = ReminderStatus::Notified;
        assert!(!save_alert_choice(&mut rows, "abc", now + 300, now));
    }

    #[test]
    fn source_names_maps_ids_and_aliases() {
        let key = "a".repeat(64);
        let config = serde_json::json!({
            "playlists": [
                {"id": "src1", "name": "  My List  "},
                {"provider_id": "src2", "name": "https://bad.example/x"},
                {"name": "No id"},
            ],
            "playlist_names": { key.clone(): "Alias Name", "short": "Skip" },
        });
        let names = source_names(&config);
        assert_eq!(names.get("src1").map(String::as_str), Some("My List"));
        assert_eq!(names.get("src2").map(String::as_str), Some(""));
        assert_eq!(
            names.get(&format!("m3u:{key}")).map(String::as_str),
            Some("Alias Name")
        );
        assert_eq!(names.len(), 3);
    }
}

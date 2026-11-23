//! The DVR schedule, ported from Python's `dvr.py`: persisted jobs in
//! `scheduled_recordings.json` (the same version-1 file shape), padding,
//! the tick state machine, restart re-arming, and daily/weekly repeats.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::TimeZone as _;
use serde::{Deserialize, Serialize};

use crate::Channel;

pub const STATUS_SCHEDULED: &str = "scheduled";
/// Due, but a provider connection conflict keeps it from
/// starting (issue #134). Persisted so the reason survives; on
/// load it re-arms to scheduled and capacity is reassessed.
pub const STATUS_WAITING: &str = "waiting";
pub const STATUS_RECORDING: &str = "recording";
pub const STATUS_STOPPING: &str = "stopping";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_PARTIAL: &str = "partial";
pub const STATUS_STOPPED: &str = "stopped";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_MISSED: &str = "missed";
pub const STATUS_CANCELED: &str = "canceled";
pub const STATUS_SKIPPED: &str = "skipped";

pub const DONE_STATUSES: &[&str] = &[
    STATUS_COMPLETED,
    STATUS_PARTIAL,
    STATUS_STOPPED,
    STATUS_FAILED,
    STATUS_MISSED,
    STATUS_CANCELED,
    STATUS_SKIPPED,
];
const ACTIVE_STATUSES: &[&str] = &[STATUS_SCHEDULED, STATUS_RECORDING, STATUS_STOPPING];
/// Clock-based repeats (Python `TIME_REPEATS`); `series` follows the guide.
pub const TIME_REPEATS: &[&str] = &["daily", "weekly", "weekdays"];

pub const UNREADABLE_WARNING: &str = "The recording schedule could not be read. The original file was preserved at {path}. Check your scheduled recordings before adding new ones.";
pub const BLOCKED_WARNING: &str = "The recording schedule could not be read or backed up. Changes are disabled to protect the original file. Check the file permissions and available disk space, then restart the application.";
const STOP_TIMEOUT_SECONDS: f64 = 1800.0;

pub const DEFAULT_PRE_PADDING_MINUTES: i64 = 2;
pub const DEFAULT_POST_PADDING_MINUTES: i64 = 5;

/// One scheduled recording, serialized with the reference's field
/// names. Every field defaults, and fields this port does not model
/// ride in `extra`, so a schedule Python wrote survives a Rust save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DvrJob {
    pub id: String,
    pub status: String,
    pub title: String,
    pub channel_name: String,
    pub display_title: String,
    /// The channel snapshot (the reference stores the full channel dict;
    /// ours carries the same keys the loader produces).
    pub channel: serde_json::Value,
    /// The programme snapshot: title plus XMLTV start/end strings.
    pub program: serde_json::Value,
    pub format: String,
    pub start_at: String,
    pub end_at: String,
    pub start_ts: f64,
    pub stop_ts: f64,
    pub pre_padding_minutes: i64,
    pub post_padding_minutes: i64,
    pub created_at: String,
    /// The recording actually began after the programme's own
    /// start (padding already consumed): a successful finish
    /// carries the missed-beginning warning (issue #130).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub started_late: bool,
    /// Why a waiting job is waiting (the connection conflict's
    /// user-visible reason); kept after the wait ends so the
    /// outcome stays explainable (issues #134/#135).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wait_reason: String,
    pub recording_id: Option<String>,
    pub output_path: String,
    pub message: String,
    pub repeat: String,
    /// Monday = 0 … Sunday = 6, for `repeat == "weekdays"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekdays: Option<Vec<u8>>,
    /// Padding (before, after) for later occurrences of a recurring
    /// recording whose own padding was changed for this one only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub future_padding: Option<[i64; 2]>,
    /// A series rule's padding changes: (XMLTV start from which it
    /// applies, before, after), ascending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_padding_changes: Option<Vec<(String, i64, i64)>>,
    /// The series rule that booked this episode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_source: Option<String>,
    /// Scheduled by time, not from the guide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
fn iso_from_ts(ts: f64) -> String {
    let secs = ts as i64;
    chrono::Utc
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S+00:00").to_string())
        .unwrap_or_default()
}

/// Python `manual_schedule.validate_window`: the window a
/// by-time recording may occupy.
///
/// # Errors
/// Returns Python's messages for non-finite times, an end not
/// in the future or not after the start or beyond seven days,
/// and a start already passed when the job has not started.
pub fn validate_window(start: f64, end: f64, now: f64, allow_started: bool) -> Result<(), String> {
    if !start.is_finite() || !end.is_finite() || !now.is_finite() {
        return Err("Invalid recording time.".to_string());
    }
    if end <= start || end <= now || end - start > 7.0 * 86_400.0 {
        return Err(
            "The end must be in the future and after the start, within seven days.".to_string(),
        );
    }
    if !allow_started && start < now {
        return Err("The recording start time has already passed.".to_string());
    }
    Ok(())
}

/// The programme's own start: the padded record start plus the
/// pre-padding that leads it.
#[allow(clippy::cast_precision_loss)] // schedule times are whole seconds
fn programme_start_ts(job: &DvrJob) -> f64 {
    job.start_ts + f64::from(i32::try_from(job.pre_padding_minutes).unwrap_or(0)) * 60.0
}

/// Flag a job whose recording begins after the programme itself
/// started: the beginning is gone, and the finish must say so.
fn note_started(job: &mut DvrJob, now: f64) {
    if now > programme_start_ts(job) {
        job.started_late = true;
    }
}

fn hex_id(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(seed.as_bytes());
    let mut out = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn xmltv_from_ts(ts: i64) -> String {
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y%m%d%H%M%S").to_string())
        .unwrap_or_default()
}

/// The channel snapshot stored in a job: the keys a recording needs to
/// start again later (name/url/group, tvg fields, and every extra
/// attribute flattened the way provider fields ride a Python channel).
#[must_use]
pub fn channel_snapshot(channel: &Channel) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), channel.name.clone().into());
    map.insert("group".into(), channel.group.clone().into());
    map.insert("url".into(), channel.url.clone().into());
    map.insert("tvg-id".into(), channel.tvg_id.clone().into());
    map.insert("tvg-name".into(), channel.tvg_name.clone().into());
    map.insert("tvg-logo".into(), channel.tvg_logo.clone().into());
    for (key, value) in &channel.unknown_attributes {
        map.insert(key.clone(), value.clone().into());
    }
    serde_json::Value::Object(map)
}

/// Rebuild a channel from its job snapshot.
#[must_use]
pub fn channel_from_snapshot(value: &serde_json::Value) -> Channel {
    let get = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut channel = Channel {
        name: get("name"),
        group: get("group"),
        url: get("url"),
        tvg_id: get("tvg-id"),
        tvg_name: get("tvg-name"),
        tvg_logo: get("tvg-logo"),
        unknown_attributes: std::collections::BTreeMap::new(),
    };
    if channel.name.is_empty() {
        channel.name = get("channel_name");
    }
    if let Some(obj) = value.as_object() {
        for (key, val) in obj {
            if matches!(
                key.as_str(),
                "name" | "group" | "url" | "tvg-id" | "tvg-name" | "tvg-logo"
            ) {
                continue;
            }
            if let Some(text) = val.as_str() {
                channel
                    .unknown_attributes
                    .insert(key.clone(), text.to_string());
            }
        }
    }
    channel
}

/// The inputs for [`build_job`].
#[derive(Clone, Debug)]
pub struct NewJob<'a> {
    pub channel: &'a Channel,
    pub programme_title: &'a str,
    pub programme_start: i64,
    pub programme_stop: i64,
    pub format: &'a str,
    pub pre_padding_minutes: i64,
    pub post_padding_minutes: i64,
    pub repeat: &'a str,
}

/// Build a job from a channel and a programme window (port of
/// `build_job`). Ids are content/time hashes standing in for uuids.
///
/// # Errors
///
/// Returns the reference's message when the end is not after the start.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // the store format itself is float seconds, exactly like Python's
pub fn build_job(spec: &NewJob<'_>, now: f64) -> Result<DvrJob, String> {
    let channel = spec.channel;
    let (programme_start, programme_stop) = (spec.programme_start, spec.programme_stop);
    if programme_stop <= programme_start {
        return Err("Program end time must be after the start time.".to_string());
    }
    let pre = spec.pre_padding_minutes.max(0);
    let post = spec.post_padding_minutes.max(0);
    let start_ts = programme_start as f64 - f64::from(i32::try_from(pre).unwrap_or(0)) * 60.0;
    let stop_ts = programme_stop as f64 + f64::from(i32::try_from(post).unwrap_or(0)) * 60.0;
    let title = if spec.programme_title.trim().is_empty() {
        "Untitled program".to_string()
    } else {
        spec.programme_title.trim().to_string()
    };
    let chan_name = if channel.name.trim().is_empty() {
        "Unknown channel".to_string()
    } else {
        channel.name.trim().to_string()
    };
    let id = hex_id(&format!("{now}:{title}:{chan_name}:{programme_start}"));
    Ok(DvrJob {
        id,
        status: STATUS_SCHEDULED.to_string(),
        title: title.clone(),
        channel_name: chan_name.clone(),
        display_title: format!("{title} - {chan_name}"),
        channel: channel_snapshot(channel),
        program: serde_json::json!({
            "title": spec.programme_title,
            "start": xmltv_from_ts(programme_start),
            "end": xmltv_from_ts(programme_stop),
        }),
        format: spec.format.to_string(),
        start_at: iso_from_ts(programme_start as f64),
        end_at: iso_from_ts(programme_stop as f64),
        start_ts,
        stop_ts,
        pre_padding_minutes: pre,
        post_padding_minutes: post,
        created_at: iso_from_ts(now),
        started_late: false,
        wait_reason: String::new(),
        recording_id: None,
        output_path: String::new(),
        message: String::new(),
        repeat: if matches!(spec.repeat, "daily" | "weekly" | "series") {
            spec.repeat.to_string()
        } else {
            String::new()
        },
        ..DvrJob::default()
    })
}

/// What the scheduler wants the caller to do after a tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DvrAction {
    /// Resolve a temporary provider stream before starting this job.
    Resolve {
        job_id: String,
        provider_id: String,
        data: std::collections::BTreeMap<String, String>,
    },
    /// Start recording this job now.
    Start(String),
    /// Stop the recording for this job.
    Stop(String),
    /// The job just entered the waiting state over a connection
    /// conflict (emitted once, on the transition, so the caller
    /// announces it once).
    Conflict(String),
}

/// The persisted schedule.
#[derive(Default)]
pub struct DvrSchedule {
    jobs: Vec<DvrJob>,
    stopping_since: std::collections::BTreeMap<String, f64>,
    resolving: BTreeSet<String>,
    /// Jobs the caller is checking capacity for: the tick leaves
    /// them entirely alone (no start, no missed) until released.
    /// Transient, like `resolving`.
    holds: BTreeSet<String>,
    path: PathBuf,
    /// Python's `load_warning`: what went wrong reading the file,
    /// for the app to show once. Empty when the load was clean.
    pub load_warning: String,
    /// Where the unreadable file was preserved (empty when it could
    /// not be), so the app can translate [`UNREADABLE_WARNING`]
    /// before filling in the path.
    pub load_warning_backup: String,
    /// The unreadable file could not be backed up: every write is
    /// refused so the original bytes survive (Python `_load_error`).
    load_error: bool,
}

/// Python `checked_weekdays`: Monday 0 to Sunday 6, sorted and
/// deduplicated.
///
/// # Errors
/// Returns Python's message for anything else.
pub fn checked_weekdays(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let message = || "Choose weekdays from Monday to Sunday.".to_string();
    let items = value.as_array().ok_or_else(message)?;
    let mut days = BTreeSet::new();
    for item in items {
        let day = item
            .as_u64()
            .filter(|d| *d <= 6 && !item.is_f64())
            .ok_or_else(message)?;
        days.insert(u8::try_from(day).map_err(|_| message())?);
    }
    Ok(days.into_iter().collect())
}

fn is_int_in(value: &serde_json::Value, max: i64) -> bool {
    value.as_i64().is_some_and(|n| (0..=max).contains(&n)) && !value.is_f64()
}

fn valid_padding_pair(value: &serde_json::Value) -> bool {
    value
        .as_array()
        .is_some_and(|pair| pair.len() == 2 && pair.iter().all(|n| is_int_in(n, 180)))
}

fn valid_series_padding(value: &serde_json::Value) -> bool {
    let Some(changes) = value.as_array() else {
        return false;
    };
    let mut previous = String::new();
    for change in changes {
        let Some(row) = change.as_array() else {
            return false;
        };
        let Some(start) = row.first().and_then(serde_json::Value::as_str) else {
            return false;
        };
        if row.len() != 3
            || start.len() != 14
            || !start.bytes().all(|b| b.is_ascii_digit())
            || start <= previous.as_str()
            || !valid_padding_pair(&serde_json::Value::Array(row[1..].to_vec()))
        {
            return false;
        }
        previous = start.to_string();
    }
    true
}

fn number_or(value: Option<&serde_json::Value>, default: f64) -> f64 {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|n| n.is_finite())
        .unwrap_or(default)
}

/// Python `_valid_stored_job`: a stored entry the scheduler can run.
/// Rust's own `waiting` status is accepted too.
fn valid_stored_job(job: &serde_json::Value) -> bool {
    let Some(map) = job.as_object() else {
        return false;
    };
    let status_ok = map
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| {
            ACTIVE_STATUSES.contains(&s) || DONE_STATUSES.contains(&s) || s == STATUS_WAITING
        });
    let id_ok = match map.get("id") {
        Some(serde_json::Value::String(id)) => !id.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_i64().is_some_and(|n| n != 0),
        _ => false,
    };
    if !status_ok || !id_ok {
        return false;
    }
    if map
        .get("future_padding")
        .is_some_and(|v| !valid_padding_pair(v))
    {
        return false;
    }
    if map
        .get("series_padding_changes")
        .is_some_and(|v| !valid_series_padding(v))
    {
        return false;
    }
    let repeat = match map.get("repeat") {
        None | Some(serde_json::Value::Null) => "",
        Some(serde_json::Value::String(r)) => r.as_str(),
        Some(_) => return false,
    };
    if !matches!(repeat, "" | "daily" | "weekly" | "series" | "weekdays") {
        return false;
    }
    if repeat == "weekdays" {
        match map.get("weekdays").map(checked_weekdays) {
            Some(Ok(days)) if !days.is_empty() => {}
            _ => return false,
        }
    }
    if TIME_REPEATS.contains(&repeat) {
        let start = number_or(map.get("start_ts"), -1.0);
        let stop = number_or(map.get("stop_ts"), 0.0);
        return 0.0 <= start && start < stop && stop <= 253_402_300_799.0;
    }
    true
}

fn unique_suffix(seed: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hex_id(&format!("{seed}:{}:{nanos}", std::process::id()))
}

impl DvrSchedule {
    /// Load (or start empty) from the store path, re-arming jobs that
    /// were mid-recording when the app last closed. An unreadable file,
    /// or one with invalid entries, is copied to
    /// `<path>.corrupt-<hex>` first (Python `load`); valid entries
    /// still load.
    #[must_use]
    pub fn load(path: PathBuf, now: f64) -> Self {
        let mut schedule = Self {
            path,
            ..Self::default()
        };
        let bytes = match std::fs::read(&schedule.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return schedule,
            Err(_) => {
                schedule.preserve_unreadable();
                return schedule;
            }
        };
        let list = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| match value {
                serde_json::Value::Object(mut map) => map.remove("jobs"),
                other => Some(other),
            });
        let Some(serde_json::Value::Array(list)) = list else {
            schedule.preserve_unreadable();
            return schedule;
        };
        let mut seen = BTreeSet::new();
        let mut invalid = false;
        let mut jobs = Vec::new();
        for mut value in list {
            if !valid_stored_job(&value) {
                invalid = true;
                continue;
            }
            for key in ["id", "recording_id"] {
                if let Some(id) = value.get(key).and_then(serde_json::Value::as_i64) {
                    value[key] = id.to_string().into();
                }
            }
            match serde_json::from_value::<DvrJob>(value) {
                Ok(job) if seen.insert(job.id.clone()) => jobs.push(job),
                _ => invalid = true,
            }
        }
        if invalid {
            schedule.preserve_unreadable();
        }
        let mut interrupted = Vec::new();
        for job in &mut jobs {
            if job.status == STATUS_WAITING {
                // A wait is not replayed after a restart: the job
                // re-arms and capacity is reassessed fresh.
                job.status = STATUS_SCHEDULED.to_string();
            }
            if job.status == STATUS_RECORDING || job.status == STATUS_STOPPING {
                job.recording_id = None;
                if job.stop_ts > 0.0 && now >= job.stop_ts {
                    job.status = STATUS_FAILED.to_string();
                    if TIME_REPEATS.contains(&job.repeat.as_str()) {
                        interrupted.push(job.clone());
                    }
                    if job.message.is_empty() {
                        job.message = "Interrupted by application restart.".to_string();
                    }
                } else {
                    job.status = STATUS_SCHEDULED.to_string();
                    job.message = String::new();
                }
            }
        }
        schedule.jobs = jobs;
        for job in &interrupted {
            schedule.schedule_next_occurrence(job, now);
        }
        if !interrupted.is_empty() {
            schedule.save_logged();
        }
        schedule
    }

    /// Keep the exact bytes before a new schedule may replace them
    /// (Python `_preserve_corrupt_schedule`); when that fails, block
    /// every write.
    fn preserve_unreadable(&mut self) {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".corrupt-{}", unique_suffix("corrupt")));
        let backup = PathBuf::from(name);
        let copied = (|| -> std::io::Result<()> {
            let bytes = std::fs::read(&self.path)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)?;
            std::io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()
        })();
        if copied.is_ok() {
            self.load_warning_backup = backup.display().to_string();
            self.load_warning = UNREADABLE_WARNING.replace("{path}", &self.load_warning_backup);
        } else {
            let _ = std::fs::remove_file(&backup);
            self.load_error = true;
            self.load_warning = BLOCKED_WARNING.to_string();
        }
        crate::applog::error("dvr", &self.load_warning);
    }

    /// Persist the store: a complete, synced temp file renamed over
    /// the old one, sorted by start (Python `_write_proposal`).
    ///
    /// # Errors
    /// Returns the load warning while writes are blocked, or the
    /// I/O error; the previous file is then untouched.
    pub fn save(&self) -> Result<(), String> {
        self.write(&self.jobs)
    }

    fn write(&self, jobs: &[DvrJob]) -> Result<(), String> {
        if self.load_error {
            return Err(self.load_warning.clone());
        }
        let mut sorted: Vec<&DvrJob> = jobs.iter().collect();
        sorted.sort_by(|a, b| a.start_ts.total_cmp(&b.start_ts));
        let text = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "jobs": sorted,
        }))
        .map_err(|e| e.to_string())?;
        let directory = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let temporary = directory.join(format!("schedule-{}.tmp", &unique_suffix("save")[..16]));
        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        Ok(())
    }

    /// A clock-driven save: a failure is logged, never turned into a
    /// job state (Python `_save_schedule`).
    pub fn save_logged(&self) {
        if let Err(error) = self.save() {
            crate::applog::error("dvr", &format!("Could not save the DVR schedule: {error}"));
        }
    }

    /// Apply a user edit to a copy, write it, and only then make it
    /// current: a failed write leaves the schedule as it was.
    fn transaction<T>(
        &mut self,
        change: impl FnOnce(&mut Vec<DvrJob>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut proposal = self.jobs.clone();
        let result = change(&mut proposal)?;
        self.write(&proposal)?;
        self.jobs = proposal;
        Ok(result)
    }

    #[must_use]
    pub fn jobs(&self) -> &[DvrJob] {
        &self.jobs
    }

    #[must_use]
    pub fn get(&self, job_id: &str) -> Option<&DvrJob> {
        self.jobs.iter().find(|j| j.id == job_id)
    }

    /// Jobs still scheduled, recording or stopping (the power gate's
    /// pending count).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.status.as_str(),
                    STATUS_SCHEDULED | STATUS_WAITING | STATUS_RECORDING | STATUS_STOPPING
                )
            })
            .count()
    }

    /// Add one job durably.
    ///
    /// # Errors
    /// Returns the write error; the job is then not added.
    pub fn add_job(&mut self, job: DvrJob) -> Result<String, String> {
        let id = job.id.clone();
        self.transaction(|jobs| {
            jobs.push(job);
            Ok(())
        })?;
        Ok(id)
    }

    /// Python `edit_timing`: retime one scheduled or recording
    /// job, refusing a stale snapshot (the job's times or status
    /// moved since the dialog opened) and any other status. The
    /// edited window replaces the padded one, so padding zeroes.
    ///
    /// # Errors
    /// Returns Python's messages for a stale or changed job, a
    /// running job's start move, an invalid window, and a failed
    /// write (the schedule is then unchanged).
    #[allow(clippy::float_cmp)] // the expected snapshot compares exactly, as Python's does
    #[allow(clippy::cast_possible_truncation)] // programme times are whole seconds
    pub fn edit_timing(
        &mut self,
        job_id: &str,
        start: f64,
        end: f64,
        expected: (f64, f64, &str),
        now: f64,
        weekdays: Option<&[u8]>,
    ) -> Result<DvrJob, String> {
        let weekdays = weekdays
            .map(|days| checked_weekdays(&serde_json::json!(days)))
            .transpose()?;
        self.transaction(|jobs| {
            let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) else {
                return Err("The recording changed. Reopen its timing dialog.".to_string());
            };
            if (job.status != STATUS_SCHEDULED && job.status != STATUS_RECORDING)
                || job.start_ts != expected.0
                || job.stop_ts != expected.1
                || job.status != expected.2
            {
                return Err("The recording changed. Reopen its timing dialog.".to_string());
            }
            let running = job.status == STATUS_RECORDING;
            if running && start != job.start_ts {
                return Err("A running recording's start time cannot be changed.".to_string());
            }
            validate_window(start, end, now, running)?;
            job.start_ts = start;
            job.stop_ts = end;
            job.start_at = iso_from_ts(start);
            job.end_at = iso_from_ts(end);
            job.pre_padding_minutes = 0;
            job.post_padding_minutes = 0;
            if let Some(program) = job.program.as_object_mut() {
                program.insert(
                    "start".to_string(),
                    serde_json::json!(xmltv_from_ts(start as i64)),
                );
                program.insert(
                    "end".to_string(),
                    serde_json::json!(xmltv_from_ts(end as i64)),
                );
            }
            if let Some(days) = weekdays {
                if running || job.repeat == "series" {
                    return Err(
                        "Select one pending recording that is not a series rule.".to_string()
                    );
                }
                job.repeat = if days.is_empty() {
                    String::new()
                } else {
                    "weekdays".to_string()
                };
                job.weekdays = Some(days);
            }
            Ok(job.clone())
        })
    }

    /// Delete a job outright (finished jobs only, like Python's
    /// Scheduled Recordings dialog Delete).
    ///
    /// # Errors
    /// Returns the write error; the job then stays.
    pub fn remove_job(&mut self, job_id: &str) -> Result<bool, String> {
        let removable = self
            .jobs
            .iter()
            .any(|j| j.id == job_id && DONE_STATUSES.contains(&j.status.as_str()));
        if !removable {
            return Ok(false);
        }
        self.transaction(|jobs| {
            jobs.retain(|j| j.id != job_id);
            Ok(())
        })?;
        self.stopping_since.remove(job_id);
        Ok(true)
    }

    /// Cancel a job that has not finished.
    ///
    /// # Errors
    /// Returns the write error; the job then keeps its status.
    pub fn cancel_job(&mut self, job_id: &str) -> Result<bool, String> {
        let cancelled = self.transaction(|jobs| {
            let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) else {
                return Ok(false);
            };
            if DONE_STATUSES.contains(&job.status.as_str()) {
                return Ok(false);
            }
            job.status = STATUS_CANCELED.to_string();
            job.message = "Canceled by user.".to_string();
            Ok(true)
        })?;
        if cancelled {
            self.resolving.remove(job_id);
            self.holds.remove(job_id);
        }
        Ok(cancelled)
    }

    /// Change a job's repeat rule (port of Python `dvr.set_repeat`):
    /// only `daily`, `weekly` and `series` are accepted, and the job
    /// must still be scheduled, recording or stopping.
    ///
    /// # Errors
    /// Returns the write error; the rule is then unchanged.
    pub fn set_repeat(&mut self, job_id: &str, repeat: &str) -> Result<bool, String> {
        if !matches!(repeat, "daily" | "weekly" | "series") {
            return Ok(false);
        }
        self.transaction(|jobs| {
            let Some(job) = jobs
                .iter_mut()
                .find(|j| j.id == job_id && ACTIVE_STATUSES.contains(&j.status.as_str()))
            else {
                return Ok(false);
            };
            job.repeat = repeat.to_string();
            Ok(true)
        })
    }

    /// The id of the first non-done job that duplicates `candidate`
    /// (port of Python `_find_duplicate_scheduled_job`): same channel
    /// identity (see [`job_channel_key`]) and the same programme
    /// `start_at`/`end_at` strings.
    #[must_use]
    pub fn find_duplicate(&self, candidate: &DvrJob) -> Option<String> {
        let candidate_key = job_channel_key(candidate);
        if candidate_key.is_empty() {
            return None;
        }
        self.jobs
            .iter()
            .filter(|job| !DONE_STATUSES.contains(&job.status.as_str()))
            .find(|job| {
                job_channel_key(job) == candidate_key
                    && job.start_at == candidate.start_at
                    && job.end_at == candidate.end_at
            })
            .map(|job| job.id.clone())
    }

    /// Ids of non-done jobs whose padded record windows overlap
    /// `candidate`'s (the creation-time overlap check in Python's
    /// `_schedule_program_recording`). Touching windows
    /// (`end == start`) do not overlap.
    #[must_use]
    pub fn overlapping(&self, candidate: &DvrJob) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|job| !DONE_STATUSES.contains(&job.status.as_str()))
            .filter(|job| job.start_ts < candidate.stop_ts && candidate.start_ts < job.stop_ts)
            .map(|job| job.id.clone())
            .collect()
    }

    /// Hold a due job while the caller checks provider capacity
    /// for it: ticks skip held jobs entirely until released.
    pub fn hold_job(&mut self, job_id: &str) {
        self.holds.insert(job_id.to_string());
    }

    /// Release a capacity hold; the next tick reassesses the job.
    pub fn release_hold(&mut self, job_id: &str) {
        self.holds.remove(job_id);
    }

    /// Whether the caller is currently holding this job.
    #[must_use]
    pub fn is_held(&self, job_id: &str) -> bool {
        self.holds.contains(job_id)
    }

    fn set_status(&mut self, job_id: &str, status: &str, message: &str) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.status = status.to_string();
            if !message.is_empty() {
                job.message = message.to_string();
            }
        }
    }

    /// Record that a job's recording has started.
    pub fn mark_recording(&mut self, job_id: &str, now: f64) {
        self.resolving.remove(job_id);
        self.set_status(job_id, STATUS_RECORDING, "");
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            note_started(job, now);
        }
        self.save_logged();
    }

    /// Apply a successful asynchronous provider resolution to a still-pending
    /// job. Returns false for canceled, expired, or otherwise stale results.
    pub fn resolution_succeeded(&mut self, job_id: &str, now: f64) -> bool {
        let active = self.resolving.contains(job_id)
            && self
                .jobs
                .iter()
                .any(|job| job.id == job_id && job.status == STATUS_SCHEDULED);
        if !active {
            return false;
        }
        self.resolving.remove(job_id);
        self.set_status(job_id, STATUS_RECORDING, "");
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            note_started(job, now);
        }
        self.save_logged();
        true
    }

    /// Keep a due job scheduled while an asynchronous provider link is being
    /// resolved. The transient ownership never enters the persisted store.
    pub fn mark_resolving(&mut self, job_id: &str) {
        self.resolving.insert(job_id.to_string());
        self.set_status(job_id, STATUS_SCHEDULED, "");
        self.save_logged();
    }

    /// Return a failed or unavailable resolution to the normal due-job path.
    /// The next scheduler tick may request another link while the window is
    /// open; after the window closes the job becomes durably failed.
    pub fn defer_resolution(&mut self, job_id: &str, now: f64) {
        let active = self.resolving.contains(job_id)
            && self
                .jobs
                .iter()
                .any(|job| job.id == job_id && job.status == STATUS_SCHEDULED);
        if !active {
            return;
        }
        self.resolving.remove(job_id);
        let expired = self
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .is_some_and(|job| job.stop_ts > 0.0 && now >= job.stop_ts);
        if expired {
            self.set_status(
                job_id,
                STATUS_FAILED,
                "Could not resolve the scheduled recording before its window ended.",
            );
            if let Some(job) = self.get(job_id).cloned() {
                self.schedule_next_occurrence(&job, now);
            }
        } else {
            self.set_status(job_id, STATUS_SCHEDULED, "");
        }
        self.save_logged();
    }

    /// Advance due jobs (port of `tick`): start what's due, stop what's
    /// over, mark what was missed, time out stuck stops.
    pub fn tick(&mut self, now: f64) -> Vec<DvrAction> {
        self.tick_gated(now, &mut |_| None)
    }

    /// `tick` with a provider-capacity gate (issue #134): before a
    /// due job starts, the gate is asked about it; `Some(reason)`
    /// means a new connection is not available and the job waits
    /// with that user-visible reason, `None` lets it start. The
    /// gate is only consulted for jobs that are due. A waiting job
    /// whose window closes is missed with its conflict reason; a
    /// waiting job the gate later clears starts (late, per #130).
    /// Job ids in fair processing order (issue #135):
    /// earliest scheduled start first, ties by saved order.
    fn fair_order_ids(&self) -> Vec<String> {
        let mut keyed: Vec<(f64, usize, String)> = self
            .jobs
            .iter()
            .enumerate()
            .map(|(i, j)| (j.start_ts, i, j.id.clone()))
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        keyed.into_iter().map(|k| k.2).collect()
    }

    pub fn tick_gated(
        &mut self,
        now: f64,
        gate: &mut dyn FnMut(&DvrJob) -> Option<String>,
    ) -> Vec<DvrAction> {
        let mut actions = Vec::new();
        let mut changed = false;
        let ids = self.fair_order_ids();
        for id in ids {
            let Some(job) = self.jobs.iter().find(|j| j.id == id).cloned() else {
                continue;
            };
            match job.status.as_str() {
                STATUS_SCHEDULED | STATUS_WAITING => {
                    if self.resolving.contains(&id) {
                        if job.stop_ts > 0.0 && now >= job.stop_ts {
                            self.resolving.remove(&id);
                            self.set_status(
                                &id,
                                STATUS_FAILED,
                                "Could not resolve the scheduled recording before its window ended.",
                            );
                            self.schedule_next_occurrence(&job, now);
                            changed = true;
                        }
                        continue;
                    }
                    if self.holds.contains(&id) {
                        continue;
                    }
                    if job.stop_ts > 0.0 && now >= job.stop_ts {
                        if job.status == STATUS_WAITING && !job.wait_reason.is_empty() {
                            let message = format!(
                                "Missed while waiting for a connection: {}",
                                job.wait_reason
                            );
                            self.set_status(&id, STATUS_MISSED, &message);
                        } else {
                            self.set_status(&id, STATUS_MISSED, "Scheduled recording was missed.");
                        }
                        self.schedule_next_occurrence(&job, now);
                        changed = true;
                    } else if now >= job.start_ts {
                        if let Some(reason) = gate(&job) {
                            if job.status != STATUS_WAITING {
                                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                                    job.status = STATUS_WAITING.to_string();
                                    job.wait_reason.clone_from(&reason);
                                    job.message.clone_from(&reason);
                                }
                                actions.push(DvrAction::Conflict(id));
                            } else if job.wait_reason != reason {
                                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                                    job.wait_reason.clone_from(&reason);
                                    job.message.clone_from(&reason);
                                }
                            }
                            changed = true;
                        } else if let Some((provider_id, data)) = stalker_resolution_request(&job) {
                            actions.push(DvrAction::Resolve {
                                job_id: id,
                                provider_id,
                                data,
                            });
                        } else {
                            let was_waiting = job.status == STATUS_WAITING;
                            self.set_status(&id, STATUS_RECORDING, "");
                            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                                if was_waiting {
                                    // The wait is over; its reason
                                    // stays on the job, the live
                                    // message clears.
                                    job.message = String::new();
                                }
                                note_started(job, now);
                            }
                            changed = true;
                            actions.push(DvrAction::Start(id));
                        }
                    }
                }
                STATUS_RECORDING => {
                    if job.stop_ts > 0.0 && now >= job.stop_ts {
                        self.set_status(&id, STATUS_STOPPING, "");
                        self.stopping_since.insert(id.clone(), now);
                        changed = true;
                        actions.push(DvrAction::Stop(id));
                    }
                }
                STATUS_STOPPING => {
                    if let Some(since) = self.stopping_since.get(&id) {
                        if now - since > STOP_TIMEOUT_SECONDS {
                            self.mark_finished_inner(&id, false, "", "Stop timed out.", now);
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if changed {
            self.save_logged();
        }
        actions
    }

    fn mark_finished_inner(
        &mut self,
        job_id: &str,
        success: bool,
        output_path: &str,
        message: &str,
        now: f64,
    ) {
        self.stopping_since.remove(job_id);
        self.resolving.remove(job_id);
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) else {
            return;
        };
        job.recording_id = None;
        if !output_path.is_empty() {
            job.output_path = output_path.to_string();
        }
        if DONE_STATUSES.contains(&job.status.as_str()) && job.status != STATUS_CANCELED {
            return;
        }
        if job.status != STATUS_CANCELED {
            job.status = if success {
                STATUS_COMPLETED.to_string()
            } else {
                STATUS_FAILED.to_string()
            };
            job.message = if success && job.started_late {
                let warning = "Started late; the beginning of the programme was missed.";
                if message.is_empty() {
                    warning.to_string()
                } else {
                    format!("{message} {warning}")
                }
            } else {
                message.to_string()
            };
        }
        let snapshot = self.jobs.iter().find(|j| j.id == job_id).cloned();
        if let Some(job) = snapshot {
            self.schedule_next_occurrence(&job, now);
        }
    }

    /// Record a recording's end and persist (port of `mark_finished`).
    pub fn mark_finished(
        &mut self,
        job_id: &str,
        success: bool,
        output_path: &str,
        message: &str,
        now: f64,
    ) {
        self.mark_finished_inner(job_id, success, output_path, message, now);
        self.save_logged();
    }

    /// Queue a time repeat's next occurrence once (Python
    /// `_schedule_next_occurrence`: `setdefault` on the stable
    /// successor id, so a second call adds nothing).
    fn schedule_next_occurrence(&mut self, job: &DvrJob, now: f64) {
        if !TIME_REPEATS.contains(&job.repeat.as_str()) {
            return;
        }
        match next_time_occurrence(job, now, &chrono::Local) {
            Ok(next) => {
                if !self.jobs.iter().any(|j| j.id == next.id) {
                    self.jobs.push(next);
                }
            }
            Err(error) => crate::applog::error(
                "dvr",
                &format!("Invalid recurring recording {}: {error}", job.id),
            ),
        }
    }

    /// Python `skip_occurrence`: mark one pending time-based
    /// occurrence skipped and queue the next, in one durable edit.
    /// Returns the next occurrence.
    ///
    /// # Errors
    /// Returns Python's message for anything but a pending time
    /// repeat still starting at `expected_start`, and write errors.
    #[allow(clippy::float_cmp)] // the expected start compares exactly, as Python's does
    pub fn skip_occurrence(
        &mut self,
        job_id: &str,
        expected_start: f64,
        now: f64,
    ) -> Result<DvrJob, String> {
        self.transaction(|jobs| {
            let Some(index) = jobs.iter().position(|j| {
                j.id == job_id
                    && j.status == STATUS_SCHEDULED
                    && TIME_REPEATS.contains(&j.repeat.as_str())
                    && j.start_ts == expected_start
            }) else {
                return Err("Select one pending recurring recording.".to_string());
            };
            let successor = next_time_occurrence(&jobs[index], now, &chrono::Local)?;
            jobs[index].status = STATUS_SKIPPED.to_string();
            jobs[index].message = String::new();
            if let Some(existing) = jobs.iter().find(|j| j.id == successor.id) {
                return Ok(existing.clone());
            }
            jobs.push(successor.clone());
            Ok(successor)
        })
    }

    /// Python `set_weekdays`: repeat one pending recording that is
    /// not a series rule on the given weekdays; no days makes it a
    /// single recording again.
    ///
    /// # Errors
    /// Returns Python's messages for invalid days or the wrong job,
    /// and write errors.
    pub fn set_weekdays(&mut self, job_id: &str, weekdays: &[u8]) -> Result<(), String> {
        let days = checked_weekdays(&serde_json::json!(weekdays))?;
        self.transaction(|jobs| {
            let Some(job) = jobs
                .iter_mut()
                .find(|j| j.id == job_id && j.status == STATUS_SCHEDULED && j.repeat != "series")
            else {
                return Err("Select one pending recording that is not a series rule.".to_string());
            };
            job.repeat = if days.is_empty() {
                String::new()
            } else {
                "weekdays".to_string()
            };
            job.weekdays = Some(days);
            Ok(())
        })
    }
}

/// Python `manual_schedule.local_candidates` for one wall-clock
/// minute: the UTC seconds it maps to in `tz` (two during the
/// autumn repeat, none in the spring gap), ascending.
fn local_candidates<Tz: chrono::TimeZone>(tz: &Tz, wall: chrono::NaiveDateTime) -> Vec<i64> {
    let mut stamps: Vec<i64> = match tz.from_local_datetime(&wall) {
        chrono::LocalResult::Single(dt) => vec![dt.timestamp()],
        chrono::LocalResult::Ambiguous(a, b) => vec![a.timestamp(), b.timestamp()],
        chrono::LocalResult::None => Vec::new(),
    };
    stamps.retain(|s| *s >= 0);
    stamps.sort_unstable();
    stamps.dedup();
    stamps
}

/// The local wall time of a UTC instant, with Python's `fold`
/// (1 when it is the second of two identical wall times).
#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
fn local_wall<Tz: chrono::TimeZone>(tz: &Tz, ts: f64) -> Option<(chrono::NaiveDateTime, usize)> {
    let secs = ts.floor() as i64;
    let wall = tz.timestamp_opt(secs, 0).single()?.naive_local();
    let fold = match tz.from_local_datetime(&wall) {
        chrono::LocalResult::Ambiguous(_, later) if later.timestamp() == secs => 1,
        _ => 0,
    };
    Some((wall, fold))
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // whole seconds
fn ts_f64(secs: i64) -> f64 {
    secs as f64
}

/// Python `planning_tools.next_weekday_job`: the next occurrence of
/// a recurring job on one of `weekdays` (Monday 0), keeping the
/// programme's wall-clock times and padding, skipping a window the
/// clock change makes nonexistent, taking `future_padding` when set.
/// `None` when no weekday is chosen.
///
/// # Errors
/// Returns Python's messages for invalid days, an unreadable job
/// time, and no valid window within 370 days.
#[allow(clippy::cast_possible_truncation)] // padding minutes and schedule times are whole numbers
pub fn next_weekday_job<Tz: chrono::TimeZone>(
    job: &DvrJob,
    now: f64,
    weekdays: &[u8],
    future_only: bool,
    tz: &Tz,
) -> Result<Option<DvrJob>, String> {
    use chrono::{Datelike as _, Timelike as _};
    let days = checked_weekdays(&serde_json::json!(weekdays))?;
    if days.is_empty() {
        return Ok(None);
    }
    let invalid = || "The recording times are invalid. Check the recording schedule.".to_string();
    let pre = job.pre_padding_minutes * 60;
    let post = job.post_padding_minutes * 60;
    let previous_start = job.start_ts;
    let (original, original_fold) =
        local_wall(tz, job.start_ts + ts_f64(pre)).ok_or_else(invalid)?;
    let (end, end_fold) = local_wall(tz, job.stop_ts - ts_f64(post)).ok_or_else(invalid)?;
    let [pre_minutes, post_minutes] = job.future_padding.unwrap_or([pre / 60, post / 60]);
    let (pre, post) = (pre_minutes * 60, post_minutes * 60);
    let offset = (end.date() - original.date()).num_days();
    let (today, _) = local_wall(tz, now).ok_or_else(invalid)?;
    let first = std::cmp::max(
        original.date() + chrono::TimeDelta::days(1),
        today.date() - chrono::TimeDelta::days(offset.max(1)),
    );
    let minute = |date: chrono::NaiveDate, time: chrono::NaiveDateTime| {
        date.and_hms_opt(time.hour(), time.minute(), 0)
    };
    for step in 0..370 {
        let date = first + chrono::TimeDelta::days(step);
        let weekday = u8::try_from(date.weekday().num_days_from_monday()).unwrap_or(7);
        if !days.contains(&weekday) {
            continue;
        }
        let (Some(starts), Some(ends)) = (
            minute(date, original),
            minute(date + chrono::TimeDelta::days(offset), end),
        ) else {
            continue;
        };
        let (a, b) = (local_candidates(tz, starts), local_candidates(tz, ends));
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let start = a[original_fold.min(a.len() - 1)] + i64::from(original.second()) - pre;
        let stop = b[end_fold.min(b.len() - 1)] + i64::from(end.second()) + post;
        let (start_f, stop_f) = (ts_f64(start), ts_f64(stop));
        if start_f <= previous_start
            || stop_f <= now
            || stop <= start
            || (future_only && start_f <= now)
        {
            continue;
        }
        let mut result = job.clone();
        result.future_padding = None;
        result.id = unique_suffix(&job.id);
        result.start_ts = start_f;
        result.stop_ts = stop_f;
        result.pre_padding_minutes = pre / 60;
        result.post_padding_minutes = post / 60;
        result.start_at = iso_from_ts(ts_f64(start + pre));
        result.end_at = iso_from_ts(ts_f64(stop - post));
        result.status = STATUS_SCHEDULED.to_string();
        result.recording_id = None;
        result.output_path = String::new();
        result.message = String::new();
        result.started_late = false;
        result.wait_reason = String::new();
        result.created_at = iso_from_ts(now);
        if !result.program.is_object() {
            result.program = serde_json::json!({});
        }
        if let Some(program) = result.program.as_object_mut() {
            program.insert("start".into(), xmltv_from_ts(start + pre).into());
            program.insert("end".into(), xmltv_from_ts(stop - post).into());
        }
        return Ok(Some(result));
    }
    Err("No valid recording window was found for the selected weekdays.".to_string())
}

/// Python `dvr.next_time_occurrence`: the next daily, weekly or
/// weekday occurrence, with the id every caller derives the same
/// way (`uuid5(NAMESPACE_URL, "iptv-dvr:" + id)`), so a restart or a
/// second caller cannot queue a second copy.
///
/// # Errors
/// As [`next_weekday_job`]; also when the job is not a time repeat.
#[allow(clippy::cast_possible_truncation)] // padding minutes are whole numbers
pub fn next_time_occurrence<Tz: chrono::TimeZone>(
    job: &DvrJob,
    now: f64,
    tz: &Tz,
) -> Result<DvrJob, String> {
    use chrono::Datelike as _;
    let invalid = || "The recording times are invalid. Check the recording schedule.".to_string();
    let days: Vec<u8> = match job.repeat.as_str() {
        "daily" => (0..7).collect(),
        "weekly" => {
            let (start, _) = local_wall(tz, job.start_ts + ts_f64(job.pre_padding_minutes * 60))
                .ok_or_else(invalid)?;
            vec![u8::try_from(start.weekday().num_days_from_monday()).unwrap_or(0)]
        }
        "weekdays" => job.weekdays.clone().unwrap_or_default(),
        _ => return Err(invalid()),
    };
    let mut successor = next_weekday_job(job, now, &days, false, tz)?.ok_or_else(invalid)?;
    successor.id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("iptv-dvr:{}", job.id).as_bytes(),
    )
    .simple()
    .to_string();
    Ok(successor)
}

fn stalker_resolution_request(
    job: &DvrJob,
) -> Option<(String, std::collections::BTreeMap<String, String>)> {
    let channel = channel_from_snapshot(&job.channel);
    if !channel.url.is_empty()
        || channel
            .unknown_attributes
            .get("provider-type")
            .map(String::as_str)
            != Some("stalker")
    {
        return None;
    }
    let provider_id = channel
        .unknown_attributes
        .get("provider-id")
        .cloned()
        .unwrap_or_default();
    Some((provider_id, channel.unknown_attributes))
}

/// The schedule store path: beside the loaded config file, which is
/// the reference's user config dir (`scheduled_recordings.json`).
#[must_use]
pub fn schedule_path(config_file: &Path) -> PathBuf {
    config_file.parent().map_or_else(
        || PathBuf::from("scheduled_recordings.json"),
        |p| p.join("scheduled_recordings.json"),
    )
}

/// A channel snapshot value as text: strings as themselves, numbers
/// and booleans in their JSON spelling, anything else empty.
fn snapshot_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        Some(serde_json::Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// The first non-empty snapshot value among `keys`, in order.
fn snapshot_first(channel: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        let text = snapshot_text(channel.get(key));
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// A copy of `value` with every object's keys in sorted order, so
/// its compact JSON serialization is the `sort_keys` spelling
/// regardless of the map's insertion order.
fn sorted_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sorted_json_value(&map[key]));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sorted_json_value).collect())
        }
        other => other.clone(),
    }
}

fn sorted_json_compact(value: &serde_json::Value) -> String {
    serde_json::to_string(&sorted_json_value(value)).unwrap_or_default()
}

/// Stable identity for a job's channel (port of Python
/// `_channel_record_key` over the job's channel snapshot): the
/// non-empty parts joined with `|`, in order — provider type,
/// provider id, stream id, URL, provider data, name. The resolved
/// stream URL can change per resolve; the provider parts are what
/// make the identity stable.
#[must_use]
pub fn job_channel_key(job: &DvrJob) -> String {
    let channel = &job.channel;
    let provider_data = match channel.get("provider-data") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(value) => sorted_json_compact(value),
    };
    let parts = [
        snapshot_first(channel, &["provider-type"]),
        snapshot_first(channel, &["provider-id"]),
        snapshot_first(channel, &["stream-id", "stream_id"]),
        snapshot_first(channel, &["url"]),
        provider_data,
        snapshot_first(channel, &["name", "tvg-name", "tvg_name"]),
    ];
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("|")
}

/// Whether a series rule's title matches a programme title (the
/// series scan's comparison in Python's `_refresh_series_recordings`):
/// trimmed, case-insensitive equality; an empty side never matches.
#[must_use]
pub fn series_title_matches(rule_title: &str, programme_title: &str) -> bool {
    let rule = rule_title.trim();
    let programme = programme_title.trim();
    !rule.is_empty() && !programme.is_empty() && rule.to_lowercase() == programme.to_lowercase()
}

/// Format a padded-window timestamp in local time for a row label.
#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
fn local_row_time(ts: f64) -> String {
    chrono::Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// One-line label for a Scheduled Recordings row: the full padded
/// record window, title, channel, status and format label — every
/// column Python's Scheduled Recordings dialog shows, on one line
/// for the list control.
#[must_use]
pub fn job_row_label(job: &DvrJob) -> String {
    let format_label = crate::recording::RECORDING_FORMATS
        .iter()
        .find(|entry| entry.0 == job.format)
        .map_or(job.format.as_str(), |entry| entry.1);
    format!(
        "{} - {} | {} | {} | {} | {}",
        local_row_time(job.start_ts),
        local_row_time(job.stop_ts),
        job.title,
        job.channel_name,
        job.status,
        format_label
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel {
        Channel {
            name: "News".to_string(),
            group: String::new(),
            url: "http://example.com/s".to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        }
    }

    fn stalker_channel() -> Channel {
        let mut channel = channel();
        channel.url.clear();
        channel
            .unknown_attributes
            .insert("provider-type".to_string(), "stalker".to_string());
        channel
            .unknown_attributes
            .insert("provider-id".to_string(), "portal-1".to_string());
        channel
            .unknown_attributes
            .insert("cmd".to_string(), "ffmpeg http://portal/live".to_string());
        channel
    }

    fn spec<'a>(
        channel: &'a Channel,
        title: &'a str,
        start: i64,
        stop: i64,
        pre: i64,
        post: i64,
    ) -> NewJob<'a> {
        NewJob {
            channel,
            programme_title: title,
            programme_start: start,
            programme_stop: stop,
            format: "provider_mkv",
            pre_padding_minutes: pre,
            post_padding_minutes: post,
            repeat: "",
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iptv-dvr-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn corrupt_copies(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("scheduled_recordings.json.corrupt-"))
                    .is_some_and(|hex| {
                        hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit())
                    })
            })
            .collect()
    }

    /// A job as Python v1.151.6 writes it, with every field this
    /// port must carry through untouched.
    fn python_job() -> serde_json::Value {
        serde_json::json!({
            "id": "0123456789abcdef0123456789abcdef",
            "status": "scheduled",
            "title": "Evening News",
            "channel_name": "News",
            "display_title": "Evening News - News",
            "channel": {"name": "News", "url": "http://example.com/s", "provider-id": "p1"},
            "program": {"title": "Evening News", "start": "20261124180000", "end": "20261124190000",
                        "episode": "0.4.", "subtitle": "Pilot", "description": "Text.", "channel_id": "news.hu"},
            "format": "provider_mkv",
            "start_at": "2026-11-24T18:00:00+00:00",
            "end_at": "2026-11-24T19:00:00+00:00",
            "start_ts": 1_795_543_200.0,
            "stop_ts": 1_795_546_920.0,
            "pre_padding_minutes": 0,
            "post_padding_minutes": 2,
            "created_at": "2026-11-23T10:00:00+00:00",
            "recording_id": null,
            "output_path": "",
            "message": "",
            "repeat": "weekdays",
            "weekdays": [0, 2, 4],
            "future_padding": [1, 3],
            "series_padding_changes": [["20261201000000", 2, 4]],
            "series_source": "fedcba9876543210fedcba9876543210",
            "manual": true,
            "a_future_python_field": {"kept": [1, 2]}
        })
    }

    fn utc(text: &str) -> f64 {
        #[allow(clippy::cast_precision_loss)] // whole-second test times
        let secs = chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp() as f64;
        secs
    }

    /// A recurring job whose padded window is `start`..`stop` (UTC
    /// RFC 3339) with the given padding minutes.
    fn recurring(repeat: &str, start: &str, stop: &str, pre: i64, post: i64) -> DvrJob {
        let chan = channel();
        let mut job = build_job(&spec(&chan, "Show", 0, 1, 0, 0), 0.0).unwrap();
        job.id = "abc".to_string();
        job.repeat = repeat.to_string();
        job.start_ts = utc(start) - 60.0 * f64::from(i32::try_from(pre).unwrap());
        job.stop_ts = utc(stop) + 60.0 * f64::from(i32::try_from(post).unwrap());
        job.pre_padding_minutes = pre;
        job.post_padding_minutes = post;
        job
    }

    const BUDAPEST: chrono_tz::Tz = chrono_tz::Europe::Budapest;

    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn weekday_recurrence_keeps_wall_clock_times() {
        // Monday 21:00 to 22:00 Budapest (UTC+1), Mondays and Wednesdays.
        let mut job = recurring(
            "weekdays",
            "2026-11-23T20:00:00Z",
            "2026-11-23T21:00:00Z",
            0,
            0,
        );
        job.weekdays = Some(vec![0, 2]);
        let next = next_weekday_job(&job, job.stop_ts, &[0, 2], false, &BUDAPEST)
            .unwrap()
            .unwrap();
        assert_eq!(next.start_ts, utc("2026-11-25T20:00:00Z"));
        assert_eq!(next.stop_ts, utc("2026-11-25T21:00:00Z"));
        assert_eq!(next.program["start"], "20261125200000");
        assert_eq!(next.status, STATUS_SCHEDULED);

        // Overnight 23:30 to 00:30 keeps its one-day offset.
        let job = recurring(
            "weekdays",
            "2026-11-23T22:30:00Z",
            "2026-11-23T23:30:00Z",
            0,
            0,
        );
        let next = next_weekday_job(&job, job.stop_ts, &[0], false, &BUDAPEST)
            .unwrap()
            .unwrap();
        assert_eq!(next.start_ts, utc("2026-11-30T22:30:00Z"));
        assert_eq!(next.stop_ts, utc("2026-11-30T23:30:00Z"));

        assert!(next_weekday_job(&job, 0.0, &[], false, &BUDAPEST)
            .unwrap()
            .is_none());
    }

    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn recurrence_skips_spring_gap_and_keeps_autumn_fold() {
        // Daily 02:30 to 03:30 local: 2026-03-29 02:30 does not exist
        // in Budapest, so the next recording is on 2026-03-30 (CEST).
        let job = recurring(
            "daily",
            "2026-03-28T01:30:00Z",
            "2026-03-28T02:30:00Z",
            0,
            0,
        );
        let next = next_time_occurrence(&job, job.stop_ts, &BUDAPEST).unwrap();
        assert_eq!(next.start_ts, utc("2026-03-30T00:30:00Z"));
        assert_eq!(next.stop_ts, utc("2026-03-30T01:30:00Z"));

        // Daily 02:30 to 03:00 CEST: on 2026-10-25 02:30 happens
        // twice; the first occurrence (fold 0) is kept, and 03:00
        // is the single CET time after the change.
        let job = recurring(
            "daily",
            "2026-10-24T00:30:00Z",
            "2026-10-24T01:00:00Z",
            0,
            0,
        );
        let next = next_time_occurrence(&job, job.stop_ts, &BUDAPEST).unwrap();
        assert_eq!(next.start_ts, utc("2026-10-25T00:30:00Z"));
        assert_eq!(next.stop_ts, utc("2026-10-25T02:00:00Z"));
    }

    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn successor_takes_future_padding_and_a_stable_id() {
        let mut job = recurring(
            "weekly",
            "2026-11-23T20:00:00Z",
            "2026-11-23T21:00:00Z",
            5,
            10,
        );
        job.future_padding = Some([1, 3]);
        let next = next_time_occurrence(&job, job.stop_ts, &BUDAPEST).unwrap();
        // Python: uuid.uuid5(uuid.NAMESPACE_URL, "iptv-dvr:abc").hex
        assert_eq!(next.id, "00caabfe6acd508e88d744d59ffb82ff");
        assert_eq!(next.start_ts, utc("2026-11-30T20:00:00Z") - 60.0);
        assert_eq!(next.stop_ts, utc("2026-11-30T21:00:00Z") + 180.0);
        assert_eq!(
            (next.pre_padding_minutes, next.post_padding_minutes),
            (1, 3)
        );
        assert_eq!(next.future_padding, None);
        assert_eq!(next.start_at, "2026-11-30T20:00:00+00:00");
    }

    #[test]
    fn skip_occurrence_and_failed_repeats_queue_one_successor() {
        let dir = temp_dir("skip");
        let mut schedule = DvrSchedule::load(dir.join("scheduled_recordings.json"), 0.0);
        let job = recurring(
            "daily",
            "2026-11-23T20:00:00Z",
            "2026-11-23T21:00:00Z",
            0,
            0,
        );
        let start = job.start_ts;
        schedule.add_job(job).unwrap();
        assert_eq!(
            schedule.skip_occurrence("abc", start + 1.0, start - 60.0),
            Err("Select one pending recurring recording.".to_string())
        );
        let next = schedule
            .skip_occurrence("abc", start, start - 60.0)
            .unwrap();
        assert_eq!(schedule.get("abc").unwrap().status, STATUS_SKIPPED);
        assert_eq!(schedule.jobs().len(), 2);
        assert_eq!(schedule.get(&next.id).unwrap().status, STATUS_SCHEDULED);

        // A missed time repeat queues its successor too (Python
        // `_set_status`), and only once.
        let missed_at = next.stop_ts + 1.0;
        schedule.tick(missed_at);
        assert_eq!(schedule.get(&next.id).unwrap().status, STATUS_MISSED);
        assert_eq!(schedule.jobs().len(), 3);
        schedule.tick(missed_at);
        assert_eq!(schedule.jobs().len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn set_weekdays_refuses_series_rules() {
        let dir = temp_dir("weekdays");
        let mut schedule = DvrSchedule::load(dir.join("scheduled_recordings.json"), 0.0);
        schedule
            .add_job(recurring(
                "series",
                "2026-11-23T20:00:00Z",
                "2026-11-23T21:00:00Z",
                0,
                0,
            ))
            .unwrap();
        assert_eq!(
            schedule.set_weekdays("abc", &[1]),
            Err("Select one pending recording that is not a series rule.".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
        let mut schedule = DvrSchedule::load(dir.join("scheduled_recordings.json"), 0.0);
        schedule
            .add_job(recurring(
                "",
                "2026-11-23T20:00:00Z",
                "2026-11-23T21:00:00Z",
                0,
                0,
            ))
            .unwrap();
        schedule.set_weekdays("abc", &[4, 1, 4]).unwrap();
        let job = schedule.get("abc").unwrap();
        assert_eq!(
            (job.repeat.as_str(), job.weekdays.clone()),
            ("weekdays", Some(vec![1, 4]))
        );
        schedule.set_weekdays("abc", &[]).unwrap();
        let job = schedule.get("abc").unwrap();
        assert_eq!(
            (job.repeat.as_str(), job.weekdays.clone()),
            ("", Some(vec![]))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn python_schedule_round_trips() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("scheduled_recordings.json");
        let job = python_job();
        std::fs::write(
            &path,
            serde_json::json!({"version": 1, "jobs": [job.clone()]}).to_string(),
        )
        .unwrap();
        let schedule = DvrSchedule::load(path.clone(), 1_795_000_000.0);
        assert!(
            schedule.load_warning.is_empty(),
            "{}",
            schedule.load_warning
        );
        assert_eq!(schedule.jobs().len(), 1);
        schedule.save().unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["jobs"][0], job);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_schedule_is_preserved() {
        let dir = temp_dir("corrupt");
        let path = dir.join("scheduled_recordings.json");
        std::fs::write(&path, b"{not json").unwrap();
        let schedule = DvrSchedule::load(path.clone(), 0.0);
        assert!(schedule.jobs().is_empty());
        let copies = corrupt_copies(&dir);
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read(&copies[0]).unwrap(), b"{not json");
        assert!(schedule.load_warning.starts_with(
            "The recording schedule could not be read. The original file was preserved at "
        ));
        schedule.save().unwrap();
        let _ = std::fs::remove_dir_all(dir);

        let dir = temp_dir("invalid-job");
        let path = dir.join("scheduled_recordings.json");
        let mut bad = python_job();
        bad["status"] = "bogus".into();
        let mut dup = python_job();
        dup["title"] = "Duplicate id".into();
        let text = serde_json::json!({"version": 1, "jobs": [python_job(), bad, dup]}).to_string();
        std::fs::write(&path, &text).unwrap();
        let schedule = DvrSchedule::load(path, 0.0);
        assert_eq!(schedule.jobs().len(), 1);
        assert_eq!(schedule.jobs()[0].title, "Evening News");
        let copies = corrupt_copies(&dir);
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read_to_string(&copies[0]).unwrap(), text);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A schedule that can be neither read nor backed up (here a
    /// directory where the file should be) blocks every write.
    #[test]
    fn blocked_when_backup_fails() {
        let dir = temp_dir("blocked");
        let path = dir.join("scheduled_recordings.json");
        std::fs::create_dir_all(&path).unwrap();
        let mut schedule = DvrSchedule::load(path, 0.0);
        assert!(schedule.load_warning.starts_with(
            "The recording schedule could not be read or backed up. Changes are disabled"
        ));
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 1000, 4600, 0, 0), 900.0).unwrap();
        assert_eq!(schedule.add_job(job), Err(schedule.load_warning.clone()));
        assert!(schedule.jobs().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[allow(clippy::float_cmp)] // whole-second schedule values are exact in f64
    fn build_job_applies_padding_and_titles() {
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 1000, 4600, 2, 5), 900.0).expect("job");
        assert_eq!(job.start_ts, 880.0);
        assert_eq!(job.stop_ts, 4900.0);
        assert_eq!(job.display_title, "Show - News");
        assert_eq!(job.status, STATUS_SCHEDULED);
        assert!(build_job(&spec(&chan, "X", 100, 100, 0, 0), 0.0).is_err());
    }

    #[test]
    fn tick_starts_due_jobs_and_marks_missed() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-test-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let due = build_job(&spec(&chan, "Due", 100, 200, 0, 0), 0.0).expect("j");
        let missed = build_job(&spec(&chan, "Old", 10, 20, 0, 0), 0.0).expect("j");
        let missed_id = missed.id.clone();
        let due_id = due.id.clone();
        schedule.add_job(due).unwrap();
        schedule.add_job(missed).unwrap();
        let actions = schedule.tick(150.0);
        assert_eq!(actions, vec![DvrAction::Start(due_id.clone())]);
        assert_eq!(schedule.get(&due_id).expect("due").status, STATUS_RECORDING);
        assert_eq!(schedule.get(&missed_id).expect("m").status, STATUS_MISSED);
        let stop_actions = schedule.tick(250.0);
        assert_eq!(stop_actions, vec![DvrAction::Stop(due_id.clone())]);
        schedule.mark_finished(&due_id, true, "/tmp/x.mkv", "", 260.0);
        assert_eq!(schedule.get(&due_id).expect("due").status, STATUS_COMPLETED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn due_stalker_job_emits_resolution_before_recording() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-stalker-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let channel = stalker_channel();
        let job = build_job(&spec(&channel, "Due", 100, 200, 0, 0), 0.0).expect("job");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();

        let actions = schedule.tick(150.0);
        assert_eq!(
            actions,
            vec![DvrAction::Resolve {
                job_id: id.clone(),
                provider_id: "portal-1".to_string(),
                data: channel.unknown_attributes.clone(),
            }]
        );
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_SCHEDULED);
        schedule.mark_resolving(&id);
        assert!(schedule.resolution_succeeded(&id, 150.0));
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_RECORDING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolving_due_job_is_not_started_again_until_resolution_finishes() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-resolve-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let due = build_job(&spec(&channel(), "Due", 100, 200, 0, 0), 0.0).expect("job");
        let id = due.id.clone();
        schedule.add_job(due).unwrap();

        assert_eq!(schedule.tick(150.0), vec![DvrAction::Start(id.clone())]);
        schedule.mark_resolving(&id);
        assert!(schedule.tick(160.0).is_empty());
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_SCHEDULED);

        schedule.defer_resolution(&id, 160.0);
        assert_eq!(schedule.tick(170.0), vec![DvrAction::Start(id)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_resolution_retries_before_window_and_fails_after_it() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-retry-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let due = build_job(&spec(&channel(), "Due", 100, 200, 0, 0), 0.0).expect("job");
        let id = due.id.clone();
        schedule.add_job(due).unwrap();

        assert_eq!(schedule.tick(150.0), vec![DvrAction::Start(id.clone())]);
        schedule.mark_resolving(&id);
        schedule.defer_resolution(&id, 160.0);
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_SCHEDULED);
        assert_eq!(schedule.tick(170.0), vec![DvrAction::Start(id.clone())]);
        schedule.mark_resolving(&id);
        schedule.defer_resolution(&id, 200.0);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_FAILED);
        assert!(job.message.contains("window ended"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn late_resolution_failure_cannot_resurrect_a_canceled_job() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-cancel-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let due = build_job(&spec(&channel(), "Due", 100, 200, 0, 0), 0.0).expect("job");
        let id = due.id.clone();
        schedule.add_job(due).unwrap();
        assert_eq!(schedule.tick(150.0), vec![DvrAction::Start(id.clone())]);
        schedule.mark_resolving(&id);
        assert!(schedule.cancel_job(&id).unwrap());
        schedule.defer_resolution(&id, 160.0);
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_CANCELED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn late_resolution_success_cannot_resurrect_a_canceled_job() {
        let dir =
            std::env::temp_dir().join(format!("iptv-dvr-cancel-success-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let due = build_job(&spec(&stalker_channel(), "Due", 100, 200, 0, 0), 0.0).expect("job");
        let id = due.id.clone();
        schedule.add_job(due).unwrap();
        assert_eq!(schedule.tick(150.0).len(), 1);
        schedule.mark_resolving(&id);
        assert!(schedule.cancel_job(&id).unwrap());
        assert!(!schedule.resolution_succeeded(&id, 150.0));
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_CANCELED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn late_start_completes_with_a_persistent_missed_beginning_warning() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-late-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        // Programme 1000-2000 with two minutes of pre-padding:
        // the padded start is 880.
        let late = build_job(&spec(&chan, "Late", 1000, 2000, 2, 5), 0.0).expect("j");
        let late_id = late.id.clone();
        schedule.add_job(late).unwrap();
        // First tick lands after the programme itself began.
        assert_eq!(schedule.tick(1001.0).len(), 1);
        let job = schedule.get(&late_id).expect("job");
        assert_eq!(job.status, STATUS_RECORDING);
        assert!(job.started_late);
        // It still stops at the saved stop time (2300, padding in).
        assert_eq!(
            schedule.tick(2300.0),
            vec![DvrAction::Stop(late_id.clone())]
        );
        schedule.mark_finished(&late_id, true, "/tmp/out.mkv", "", 2301.0);
        let job = schedule.get(&late_id).expect("job");
        assert_eq!(job.status, STATUS_COMPLETED);
        assert_eq!(
            job.message,
            "Started late; the beginning of the programme was missed."
        );
        // The warning is persisted, not just in memory.
        let reloaded = DvrSchedule::load(dir.join("scheduled_recordings.json"), 0.0);
        assert!(reloaded
            .get(&late_id)
            .expect("job")
            .message
            .contains("beginning of the programme was missed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_inside_padding_is_not_late() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-pad-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Padded", 1000, 2000, 2, 5), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        assert_eq!(schedule.tick(900.0).len(), 1); // padded start 880, programme 1000
        assert!(!schedule.get(&id).expect("job").started_late);
        schedule.mark_finished(&id, true, "/tmp/out.mkv", "", 2301.0);
        assert_eq!(schedule.get(&id).expect("job").message, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gated_tick_waits_once_then_starts_when_cleared() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-gate-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        let mut blocked = |_: &DvrJob| Some("All 1 provider connection(s) are in use.".to_string());
        let actions = schedule.tick_gated(150.0, &mut blocked);
        assert_eq!(actions, vec![DvrAction::Conflict(id.clone())]);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_WAITING);
        assert_eq!(job.wait_reason, "All 1 provider connection(s) are in use.");
        // Still blocked: no second Conflict (announced once).
        let actions = schedule.tick_gated(160.0, &mut blocked);
        assert!(actions.is_empty());
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_WAITING);
        // Cleared: the job starts, late (programme began at 100).
        let actions = schedule.tick_gated(170.0, &mut |_| None);
        assert_eq!(actions, vec![DvrAction::Start(id.clone())]);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_RECORDING);
        assert!(job.started_late);
        assert_eq!(job.wait_reason, "All 1 provider connection(s) are in use.");
        assert_eq!(job.message, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gated_tick_expires_a_waiting_job_with_its_reason() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-wexp-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        let mut blocked = |_: &DvrJob| Some("busy".to_string());
        let _ = schedule.tick_gated(150.0, &mut blocked);
        let _ = schedule.tick_gated(250.0, &mut blocked);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_MISSED);
        assert_eq!(job.message, "Missed while waiting for a connection: busy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn held_jobs_neither_start_nor_miss() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-hold-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        schedule.hold_job(&id);
        // Past the whole window while held: nothing happens.
        assert!(schedule.tick(500.0).is_empty());
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_SCHEDULED);
        schedule.release_hold(&id);
        // Released after the window: the ordinary missed path.
        let _ = schedule.tick(500.0);
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_MISSED);
        // A waiting job can be canceled like any other.
        let job2 = build_job(&spec(&chan, "Two", 100, 200, 0, 0), 1.0).expect("j");
        let id2 = job2.id.clone();
        schedule.add_job(job2).unwrap();
        let _ = schedule.tick_gated(150.0, &mut |_| Some("busy".to_string()));
        assert_eq!(schedule.get(&id2).expect("job").status, STATUS_WAITING);
        assert!(schedule.cancel_job(&id2).unwrap());
        assert_eq!(schedule.get(&id2).expect("job").status, STATUS_CANCELED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waiting_jobs_rearm_to_scheduled_on_load() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-wload-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path.clone(), 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        let _ = schedule.tick_gated(150.0, &mut |_| Some("busy".to_string()));
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_WAITING);
        // The persisted waiting job comes back scheduled: the
        // wait is reassessed, never replayed.
        let reloaded = DvrSchedule::load(path, 0.0);
        assert_eq!(reloaded.get(&id).expect("job").status, STATUS_SCHEDULED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gated_tick_grants_a_freed_slot_to_the_earliest_start() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-fair-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        // Saved second, due first.
        let later = build_job(&spec(&chan, "Later", 300, 900, 0, 0), 0.0).expect("j");
        let earlier = build_job(&spec(&chan, "Earlier", 100, 900, 0, 0), 0.0).expect("j");
        let (later_id, earlier_id) = (later.id.clone(), earlier.id.clone());
        schedule.add_job(later).unwrap();
        schedule.add_job(earlier).unwrap();
        // One slot only: the gate frees exactly its first caller.
        let mut calls = 0;
        let mut one_slot = |_: &DvrJob| {
            calls += 1;
            if calls == 1 {
                None
            } else {
                Some("busy".to_string())
            }
        };
        let actions = schedule.tick_gated(400.0, &mut one_slot);
        assert!(actions.contains(&DvrAction::Start(earlier_id.clone())));
        assert!(actions.contains(&DvrAction::Conflict(later_id.clone())));
        assert_eq!(
            schedule.get(&earlier_id).expect("j").status,
            STATUS_RECORDING
        );
        assert_eq!(schedule.get(&later_id).expect("j").status, STATUS_WAITING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gated_tick_breaks_equal_starts_by_saved_order() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-tie-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let first = build_job(&spec(&chan, "First", 100, 900, 0, 0), 0.0).expect("j");
        let second = build_job(&spec(&chan, "Second", 100, 900, 0, 0), 1.0).expect("j");
        let (first_id, second_id) = (first.id.clone(), second.id.clone());
        schedule.add_job(first).unwrap();
        schedule.add_job(second).unwrap();
        let mut calls = 0;
        let mut one_slot = |_: &DvrJob| {
            calls += 1;
            if calls == 1 {
                None
            } else {
                Some("busy".to_string())
            }
        };
        let actions = schedule.tick_gated(400.0, &mut one_slot);
        assert!(actions.contains(&DvrAction::Start(first_id.clone())));
        assert_eq!(schedule.get(&first_id).expect("j").status, STATUS_RECORDING);
        assert_eq!(schedule.get(&second_id).expect("j").status, STATUS_WAITING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gated_tick_leaves_other_accounts_independent() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-ind-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let blocked = build_job(&spec(&chan, "Blocked", 100, 900, 0, 0), 0.0).expect("j");
        let free = build_job(&spec(&chan, "Free", 100, 900, 0, 0), 1.0).expect("j");
        let (blocked_id, free_id) = (blocked.id.clone(), free.id.clone());
        schedule.add_job(blocked).unwrap();
        schedule.add_job(free).unwrap();
        let mut gate = |job: &DvrJob| {
            if job.title == "Blocked" {
                Some("busy".to_string())
            } else {
                None
            }
        };
        let actions = schedule.tick_gated(400.0, &mut gate);
        assert!(actions.contains(&DvrAction::Start(free_id.clone())));
        assert_eq!(schedule.get(&blocked_id).expect("j").status, STATUS_WAITING);
        assert_eq!(schedule.get(&free_id).expect("j").status, STATUS_RECORDING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waiting_job_at_its_stop_boundary_is_missed_not_started() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-race-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        let _ = schedule.tick_gated(150.0, &mut |_| Some("busy".to_string()));
        // Capacity frees exactly as the window closes: expiry
        // wins the race, the job is missed with its reason.
        let actions = schedule.tick_gated(200.0, &mut |_| None);
        assert!(actions.is_empty());
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_MISSED);
        assert_eq!(job.message, "Missed while waiting for a connection: busy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waiting_job_expired_during_downtime_is_missed_after_restart() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-wdown-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path.clone(), 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        let _ = schedule.tick_gated(150.0, &mut |_| Some("busy".to_string()));
        assert_eq!(schedule.get(&id).expect("job").status, STATUS_WAITING);
        drop(schedule);
        // The app restarts after the window closed: the job is
        // reassessed, not replayed — plain missed, reason kept.
        let mut reloaded = DvrSchedule::load(path, 300.0);
        assert_eq!(reloaded.get(&id).expect("job").status, STATUS_SCHEDULED);
        let _ = reloaded.tick(300.0);
        let job = reloaded.get(&id).expect("job");
        assert_eq!(job.status, STATUS_MISSED);
        assert_eq!(job.wait_reason, "busy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn job_with_channel(snapshot: serde_json::Value) -> DvrJob {
        let chan = channel();
        let mut job = build_job(&spec(&chan, "Show", 1000, 4600, 0, 0), 0.0).expect("j");
        job.channel = snapshot;
        job
    }

    #[test]
    fn set_repeat_accepts_known_rules_and_persists() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-repeat-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path.clone(), 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        for repeat in ["daily", "weekly", "series"] {
            assert!(schedule.set_repeat(&id, repeat).unwrap());
            assert_eq!(schedule.get(&id).expect("job").repeat, repeat);
        }
        // The last setting is persisted, not just in memory.
        let reloaded = DvrSchedule::load(path, 0.0);
        assert_eq!(reloaded.get(&id).expect("job").repeat, "series");
        // Anything else, and unknown ids, are refused.
        assert!(!schedule.set_repeat(&id, "none").unwrap());
        assert!(!schedule.set_repeat(&id, "bogus").unwrap());
        assert!(!schedule.set_repeat(&id, "").unwrap());
        assert!(!schedule.set_repeat("no-such-job", "daily").unwrap());
        assert_eq!(schedule.get(&id).expect("job").repeat, "series");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn job_channel_key_orders_parts_and_applies_fallbacks() {
        // Full snapshot: parts in order, provider-data object as
        // compact JSON with sorted keys (recursively).
        let job = job_with_channel(serde_json::json!({
            "provider-type": "stalker",
            "provider-id": "portal-1",
            "stream-id": "42",
            "url": "http://example.com/live",
            "provider-data": {"b": 1, "a": {"d": 4, "c": 3}},
            "name": "News",
        }));
        assert_eq!(
            job_channel_key(&job),
            "stalker|portal-1|42|http://example.com/live|{\"a\":{\"c\":3,\"d\":4},\"b\":1}|News"
        );
        // stream_id fallback, tvg-name name fallback, string
        // provider-data as itself.
        let job = job_with_channel(serde_json::json!({
            "provider-type": "xtream",
            "stream_id": "7",
            "url": "http://example.com/s",
            "provider-data": "raw-data",
            "tvg-name": "TVG News",
        }));
        assert_eq!(
            job_channel_key(&job),
            "xtream|7|http://example.com/s|raw-data|TVG News"
        );
        // tvg_name is the last name fallback; empty parts are skipped.
        let job = job_with_channel(serde_json::json!({
            "url": "http://example.com/only",
            "name": "",
            "tvg_name": "Last Resort",
        }));
        assert_eq!(job_channel_key(&job), "http://example.com/only|Last Resort");
        // An empty snapshot has no identity.
        let job = job_with_channel(serde_json::json!({}));
        assert_eq!(job_channel_key(&job), "");
    }

    #[test]
    fn find_duplicate_needs_same_channel_and_window() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-dup-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let existing = build_job(&spec(&chan, "Show", 1000, 4600, 0, 0), 0.0).expect("j");
        let existing_id = existing.id.clone();
        schedule.add_job(existing).unwrap();
        // Same channel and programme window (built later, so a
        // different id) duplicates the stored job.
        let candidate = build_job(&spec(&chan, "Show", 1000, 4600, 0, 0), 1.0).expect("j");
        assert_ne!(candidate.id, existing_id);
        assert_eq!(
            schedule.find_duplicate(&candidate),
            Some(existing_id.clone())
        );
        // A different end time is not a duplicate.
        let other_end = build_job(&spec(&chan, "Show", 1000, 5000, 0, 0), 2.0).expect("j");
        assert_eq!(schedule.find_duplicate(&other_end), None);
        // A done job no longer counts as a duplicate.
        assert!(schedule.cancel_job(&existing_id).unwrap());
        assert_eq!(schedule.find_duplicate(&candidate), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_uses_padded_windows_and_skips_done() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-overlap-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let existing = build_job(&spec(&chan, "Show", 1000, 2000, 0, 0), 0.0).expect("j");
        let existing_id = existing.id.clone();
        schedule.add_job(existing).unwrap();
        // Touching windows (candidate starts exactly at the stored
        // stop) do not overlap.
        let touching = build_job(&spec(&chan, "Next", 2000, 3000, 0, 0), 0.0).expect("j");
        assert!(schedule.overlapping(&touching).is_empty());
        // A partial overlap does.
        let partial = build_job(&spec(&chan, "Partial", 1500, 2500, 0, 0), 0.0).expect("j");
        assert_eq!(schedule.overlapping(&partial), vec![existing_id.clone()]);
        // Done jobs are excluded.
        assert!(schedule.cancel_job(&existing_id).unwrap());
        assert!(schedule.overlapping(&partial).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn series_title_matches_is_trimmed_and_case_insensitive() {
        assert!(series_title_matches("News", "news"));
        assert!(series_title_matches("  Evening News ", "evening news"));
        assert!(series_title_matches("Café Report", "CAFÉ REPORT"));
        assert!(!series_title_matches("News", "News Extra"));
        assert!(!series_title_matches("", "News"));
        assert!(!series_title_matches("News", "  "));
        assert!(!series_title_matches("", ""));
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn job_row_label_shows_padded_window_and_format_label() {
        let chan = channel();
        // Programme 1000-4600 with 2/5 padding: window 880-4900.
        let job = build_job(&spec(&chan, "Show", 1000, 4600, 2, 5), 0.0).expect("j");
        let fmt_local = |ts: f64| {
            chrono::Local
                .timestamp_opt(ts as i64, 0)
                .single()
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        };
        let expected = format!(
            "{} - {} | Show | News | scheduled | Provider quality (copy, MKV)",
            fmt_local(880.0),
            fmt_local(4900.0)
        );
        assert_eq!(job_row_label(&job), expected);
        // An unknown format key falls back to the raw key.
        let mut job = job;
        job.format = "bogus_fmt".to_string();
        assert!(job_row_label(&job).ends_with("| bogus_fmt"));
    }

    #[test]
    fn finish_after_cancel_keeps_canceled_and_the_users_reason() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-cfin-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job).unwrap();
        assert_eq!(schedule.tick(150.0).len(), 1);
        assert!(schedule.cancel_job(&id).unwrap());
        // The capture that was starting when the cancel landed
        // ends now: the job stays Canceled with the user's reason.
        schedule.mark_finished(&id, true, "/tmp/out.mkv", "", 160.0);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_CANCELED);
        assert_eq!(job.message, "Canceled by user.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

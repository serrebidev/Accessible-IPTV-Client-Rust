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
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_MISSED: &str = "missed";
pub const STATUS_CANCELED: &str = "canceled";

pub const DONE_STATUSES: &[&str] = &[
    STATUS_COMPLETED,
    STATUS_FAILED,
    STATUS_MISSED,
    STATUS_CANCELED,
];
const STOP_TIMEOUT_SECONDS: f64 = 1800.0;

pub const DEFAULT_PRE_PADDING_MINUTES: i64 = 2;
pub const DEFAULT_POST_PADDING_MINUTES: i64 = 5;

/// One scheduled recording, serialized with the reference's field names.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    #[serde(default)]
    pub started_late: bool,
    /// Why a waiting job is waiting (the connection conflict's
    /// user-visible reason); kept after the wait ends so the
    /// outcome stays explainable (issues #134/#135).
    #[serde(default)]
    pub wait_reason: String,
    pub recording_id: Option<String>,
    pub output_path: String,
    pub message: String,
    pub repeat: String,
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
}

impl DvrSchedule {
    /// Load (or start empty) from the store path, re-arming jobs that
    /// were mid-recording when the app last closed.
    #[must_use]
    pub fn load(path: PathBuf, now: f64) -> Self {
        let mut schedule = Self {
            jobs: Vec::new(),
            stopping_since: std::collections::BTreeMap::new(),
            resolving: BTreeSet::new(),
            holds: BTreeSet::new(),
            path,
        };
        let Ok(text) = std::fs::read_to_string(&schedule.path) else {
            return schedule;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return schedule;
        };
        let list = value.get("jobs").cloned().unwrap_or_else(|| value.clone());
        let Ok(mut jobs) = serde_json::from_value::<Vec<DvrJob>>(list) else {
            return schedule;
        };
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
        schedule
    }

    /// Persist the store (atomic temp + rename, sorted by start).
    pub fn save(&self) {
        let mut jobs: Vec<&DvrJob> = self.jobs.iter().collect();
        jobs.sort_by(|a, b| a.start_ts.total_cmp(&b.start_ts));
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "version": 1,
            "jobs": jobs,
        }))
        .unwrap_or_default();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
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

    pub fn add_job(&mut self, job: DvrJob) -> String {
        let id = job.id.clone();
        self.jobs.push(job);
        self.save();
        id
    }

    /// Delete a job outright (finished jobs only, like Python's
    /// Scheduled Recordings dialog Delete).
    pub fn remove_job(&mut self, job_id: &str) -> bool {
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
            return false;
        };
        if !DONE_STATUSES.contains(&job.status.as_str()) {
            return false;
        }
        self.jobs.retain(|j| j.id != job_id);
        self.stopping_since.remove(job_id);
        self.save();
        true
    }

    pub fn cancel_job(&mut self, job_id: &str) -> bool {
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) else {
            return false;
        };
        if DONE_STATUSES.contains(&job.status.as_str()) {
            return false;
        }
        self.resolving.remove(job_id);
        self.holds.remove(job_id);
        job.status = STATUS_CANCELED.to_string();
        job.message = "Canceled by user.".to_string();
        self.save();
        true
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
        self.save();
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
        self.save();
        true
    }

    /// Keep a due job scheduled while an asynchronous provider link is being
    /// resolved. The transient ownership never enters the persisted store.
    pub fn mark_resolving(&mut self, job_id: &str) {
        self.resolving.insert(job_id.to_string());
        self.set_status(job_id, STATUS_SCHEDULED, "");
        self.save();
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
        } else {
            self.set_status(job_id, STATUS_SCHEDULED, "");
        }
        self.save();
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
            self.save();
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
        self.save();
    }

    fn schedule_next_occurrence(&mut self, job: &DvrJob, now: f64) {
        let days: i64 = match job.repeat.as_str() {
            "daily" => 1,
            "weekly" => 7,
            _ => return,
        };
        let mut next = job.clone();
        next.id = hex_id(&format!("{}:{}:{}", now, job.id, job.start_ts));
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let shift_local_days = |ts: f64, days: i64| -> f64 {
            let local = chrono::Utc
                .timestamp_opt(ts as i64, 0)
                .single()
                .map(|dt| dt.with_timezone(&chrono::Local));
            match local {
                Some(dt) => (dt + chrono::TimeDelta::days(days)).timestamp() as f64,
                None => ts + f64::from(i32::try_from(days * 86_400).unwrap_or(0)),
            }
        };
        while next.stop_ts <= now {
            next.start_ts = shift_local_days(next.start_ts, days);
            next.stop_ts = shift_local_days(next.stop_ts, days);
        }
        if next.start_ts <= job.start_ts {
            next.start_ts = shift_local_days(next.start_ts, days);
            next.stop_ts = shift_local_days(next.stop_ts, days);
        }
        next.start_at = iso_from_ts(
            next.start_ts + f64::from(i32::try_from(next.pre_padding_minutes).unwrap_or(0)) * 60.0,
        );
        next.end_at = iso_from_ts(
            next.stop_ts - f64::from(i32::try_from(next.post_padding_minutes).unwrap_or(0)) * 60.0,
        );
        next.status = STATUS_SCHEDULED.to_string();
        next.started_late = false;
        next.wait_reason = String::new();
        next.recording_id = None;
        next.output_path = String::new();
        next.message = String::new();
        next.created_at = iso_from_ts(now);
        self.jobs.push(next);
    }
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
        schedule.add_job(due);
        schedule.add_job(missed);
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
        schedule.add_job(job);

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
        schedule.add_job(due);

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
        schedule.add_job(due);

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
        schedule.add_job(due);
        assert_eq!(schedule.tick(150.0), vec![DvrAction::Start(id.clone())]);
        schedule.mark_resolving(&id);
        assert!(schedule.cancel_job(&id));
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
        schedule.add_job(due);
        assert_eq!(schedule.tick(150.0).len(), 1);
        schedule.mark_resolving(&id);
        assert!(schedule.cancel_job(&id));
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
        schedule.add_job(late);
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
        schedule.add_job(job);
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
        schedule.add_job(job);
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
        schedule.add_job(job);
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
        schedule.add_job(job);
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
        schedule.add_job(job2);
        let _ = schedule.tick_gated(150.0, &mut |_| Some("busy".to_string()));
        assert_eq!(schedule.get(&id2).expect("job").status, STATUS_WAITING);
        assert!(schedule.cancel_job(&id2));
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
        schedule.add_job(job);
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
        schedule.add_job(later);
        schedule.add_job(earlier);
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
        schedule.add_job(first);
        schedule.add_job(second);
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
        schedule.add_job(blocked);
        schedule.add_job(free);
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
        schedule.add_job(job);
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
        schedule.add_job(job);
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

    #[test]
    fn finish_after_cancel_keeps_canceled_and_the_users_reason() {
        let dir = std::env::temp_dir().join(format!("iptv-dvr-cfin-{}", std::process::id()));
        let path = dir.join("scheduled_recordings.json");
        let mut schedule = DvrSchedule::load(path, 0.0);
        let chan = channel();
        let job = build_job(&spec(&chan, "Show", 100, 200, 0, 0), 0.0).expect("j");
        let id = job.id.clone();
        schedule.add_job(job);
        assert_eq!(schedule.tick(150.0).len(), 1);
        assert!(schedule.cancel_job(&id));
        // The capture that was starting when the cancel landed
        // ends now: the job stays Canceled with the user's reason.
        schedule.mark_finished(&id, true, "/tmp/out.mkv", "", 160.0);
        let job = schedule.get(&id).expect("job");
        assert_eq!(job.status, STATUS_CANCELED);
        assert_eq!(job.message, "Canceled by user.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

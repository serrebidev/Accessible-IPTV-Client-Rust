//! The DVR schedule, ported from Python's `dvr.py`: persisted jobs in
//! `scheduled_recordings.json` (the same version-1 file shape), padding,
//! the tick state machine, restart re-arming, and daily/weekly repeats.

use std::path::{Path, PathBuf};

use chrono::TimeZone as _;
use serde::{Deserialize, Serialize};

use crate::Channel;

pub const STATUS_SCHEDULED: &str = "scheduled";
pub const STATUS_RECORDING: &str = "recording";
pub const STATUS_STOPPING: &str = "stopping";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_MISSED: &str = "missed";
pub const STATUS_CANCELED: &str = "canceled";

const DONE_STATUSES: &[&str] = &[
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
    /// Start recording this job now.
    Start(String),
    /// Stop the recording for this job.
    Stop(String),
}

/// The persisted schedule.
#[derive(Default)]
pub struct DvrSchedule {
    jobs: Vec<DvrJob>,
    stopping_since: std::collections::BTreeMap<String, f64>,
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
                    STATUS_SCHEDULED | STATUS_RECORDING | STATUS_STOPPING
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

    pub fn cancel_job(&mut self, job_id: &str) -> bool {
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) else {
            return false;
        };
        if DONE_STATUSES.contains(&job.status.as_str()) {
            return false;
        }
        job.status = STATUS_CANCELED.to_string();
        job.message = "Canceled by user.".to_string();
        self.save();
        true
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
    pub fn mark_recording(&mut self, job_id: &str) {
        self.set_status(job_id, STATUS_RECORDING, "");
        self.save();
    }

    /// Advance due jobs (port of `tick`): start what's due, stop what's
    /// over, mark what was missed, time out stuck stops.
    pub fn tick(&mut self, now: f64) -> Vec<DvrAction> {
        let mut actions = Vec::new();
        let mut changed = false;
        let ids: Vec<String> = self.jobs.iter().map(|j| j.id.clone()).collect();
        for id in ids {
            let Some(job) = self.jobs.iter().find(|j| j.id == id).cloned() else {
                continue;
            };
            match job.status.as_str() {
                STATUS_SCHEDULED => {
                    if job.stop_ts > 0.0 && now >= job.stop_ts {
                        self.set_status(&id, STATUS_MISSED, "Scheduled recording was missed.");
                        changed = true;
                    } else if now >= job.start_ts {
                        self.set_status(&id, STATUS_RECORDING, "");
                        changed = true;
                        actions.push(DvrAction::Start(id));
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
            job.message = message.to_string();
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
        next.recording_id = None;
        next.output_path = String::new();
        next.message = String::new();
        next.created_at = iso_from_ts(now);
        self.jobs.push(next);
    }
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
}

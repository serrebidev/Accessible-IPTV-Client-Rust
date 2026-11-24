//! Recover from Channel Archive for missed, failed or partial
//! scheduled recordings (Python `main.py`
//! `ScheduledRecordingsDialog._recover_archive`, with
//! `programme_reminders.resolve_channel` and the
//! `recording_limits.check_start` budget check it runs before and
//! after resolving the channel).
//!
//! Recovery downloads a separate copy of the programme from the
//! provider's archive when it is still available. The recording's
//! own padding decides the download window (Python's
//! `schedule_padding`), so the copy covers exactly the span the
//! recording was booked for.

use serde_json::Value;

use crate::catchup::{channel_has_catchup, within_catchup_window};
use crate::dvr::{channel_from_snapshot, DvrJob};
use crate::epg::Programme;
use crate::playback::StreamHeaders;
use crate::recording_limits;
use crate::reminders::reminder_channel_key;
use crate::Channel;

/// Recording statuses recoverable from the channel archive:
/// missed, failed and partial.
pub const RECOVERABLE_STATUSES: [&str; 3] = [
    crate::dvr::STATUS_MISSED,
    crate::dvr::STATUS_FAILED,
    crate::dvr::STATUS_PARTIAL,
];

/// Python's recovery menu gate: only a missed, failed or partial
/// recording offers Recover from Channel Archive.
#[must_use]
pub fn recoverable_status(status: &str) -> bool {
    RECOVERABLE_STATUSES.contains(&status)
}

/// One channel's media identity: its stream URL plus its headers,
/// the second half of `programme_reminders.media_identity`.
/// Python hashes the headers as sorted JSON; here the canonical
/// header list is sorted the same way, which is all the equality
/// needs.
#[must_use]
pub fn media_identity(channel: &Channel) -> (String, String) {
    let headers = StreamHeaders::for_channel(channel);
    let mut pairs: Vec<(String, String)> = headers
        .entries
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .chain(headers.extra.iter().map(|line| {
            let Some((name, value)) = line.split_once(": ") else {
                return (line.clone(), String::new());
            };
            (name.to_string(), value.to_string())
        }))
        .collect();
    pairs.sort();
    (channel.url.clone(), format!("{pairs:?}"))
}

/// Python `resolve_channel`: the channel a scheduled recording
/// was booked on, found through the reminder scope key
/// (`programme_reminders.channel_key` of the stored channel
/// snapshot). Exactly one distinct media identity must match;
/// an ambiguous or absent match is None.
#[must_use]
pub fn resolve_recovery_channel(job: &DvrJob, channels: &[Channel]) -> Option<Channel> {
    let snapshot = channel_from_snapshot(&job.channel);
    let key = reminder_channel_key(&snapshot);
    if key.is_empty() {
        return None;
    }
    let mut matches = channels
        .iter()
        .filter(|channel| !channel.url.is_empty() && reminder_channel_key(channel) == key);
    let first = matches.next()?;
    let first_identity = media_identity(first);
    if matches.any(|channel| media_identity(channel) != first_identity) {
        return None;
    }
    Some(first.clone())
}

/// The programme window a recovered recording downloads, plus the
/// recording's own padding for the download window (Python's
/// `schedule_padding`): the programme starts `pre` minutes after
/// the padded record start and ends `post` minutes before the
/// padded record end, and the archive download asks the provider
/// for the whole padded span again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryShow {
    /// The programme with its true start and end (epoch seconds).
    pub programme: Programme,
    /// The recording's pre-padding minutes (the download window
    /// widens by the same amount, as Python does).
    pub pre_minutes: i64,
    /// The recording's post-padding minutes.
    pub post_minutes: i64,
}

/// Python `_recover_archive`'s `prepare`: the job must be missed,
/// failed or partial, the programme must be over, in the channel
/// archive and inside the catch-up window.
///
/// # Errors
/// Returns Python's messages verbatim.
pub fn recovery_show(job: &DvrJob, channel: &Channel, now: i64) -> Result<RecoveryShow, String> {
    if !recoverable_status(&job.status) {
        return Err("Select a missed, failed or partial recording.".to_string());
    }
    let pre = job.pre_padding_minutes.max(0);
    let post = job.post_padding_minutes.max(0);
    if !job.start_ts.is_finite() || !job.stop_ts.is_finite() {
        return Err("This programme is unavailable in the channel archive.".to_string());
    }
    let pre_seconds = f64::from(i32::try_from(pre).unwrap_or(0)) * 60.0;
    let post_seconds = f64::from(i32::try_from(post).unwrap_or(0)) * 60.0;
    #[allow(clippy::cast_possible_truncation)] // whole-second schedule times
    let start = (job.start_ts + pre_seconds) as i64;
    #[allow(clippy::cast_possible_truncation)] // whole-second schedule times
    let end = (job.stop_ts - post_seconds) as i64;
    if end <= start || end >= now {
        return Err("This programme is unavailable in the channel archive.".to_string());
    }
    if !channel_has_catchup(channel) || !within_catchup_window(channel, start, now) {
        return Err("This programme is unavailable in the channel archive.".to_string());
    }
    let snapshot = channel_from_snapshot(&job.channel);
    let title = if !job.title.is_empty() {
        job.title.clone()
    } else if let Some(title) = job.program.get("title").and_then(Value::as_str) {
        title.to_string()
    } else {
        job.channel_name.clone()
    };
    let channel_id = if snapshot.tvg_id.is_empty() {
        channel.tvg_id.clone()
    } else {
        snapshot.tvg_id.clone()
    };
    Ok(RecoveryShow {
        programme: Programme {
            channel_id,
            start,
            stop: end,
            title,
            ..Programme::default()
        },
        pre_minutes: pre,
        post_minutes: post,
    })
}

/// Python `recording_limits.check_start`: the recording
/// connection-limit refusal for one account. `active` carries the
/// account keys of the recordings and downloads currently
/// running (the app maps its active recordings to account keys);
/// buffer-lease holders never count there, and they never reach
/// this list.
///
/// # Errors
/// Returns Python's message when the account's limit is already
/// reached.
pub fn check_start(config: &Value, channel: &Channel, active: &[String]) -> Result<String, String> {
    let key = recording_limits::channel_key(channel, config);
    let limit = recording_limits::normalize(config.get("recording_connection_limits"))
        .get(&key)
        .copied()
        .unwrap_or(0);
    if limit == 0 {
        return Ok(key);
    }
    let used = active
        .iter()
        .filter(|account| account.as_str() == key)
        .count();
    if i64::try_from(used).unwrap_or(i64::MAX) >= limit {
        return Err(format!(
            "The recording connection limit for this account has been reached ({limit}). Existing recordings will continue."
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvr::channel_snapshot;

    const NOW: i64 = 1_700_100_000;

    fn channel_with(attrs: &[(&str, &str)]) -> Channel {
        Channel {
            name: "Chan".to_string(),
            url: "http://example.com/live/stream.m3u8".to_string(),
            tvg_id: "chan-id".to_string(),
            unknown_attributes: attrs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            ..Channel::default()
        }
    }

    fn catchup_channel() -> Channel {
        channel_with(&[("catchup", "default"), ("provider-id", "prov-1")])
    }

    fn job_for(channel: &Channel, status: &str) -> DvrJob {
        DvrJob {
            id: "job-1".to_string(),
            status: status.to_string(),
            title: "Show".to_string(),
            channel_name: "Chan".to_string(),
            channel: channel_snapshot(channel),
            start_ts: 1_700_000_000.0,
            stop_ts: 1_700_003_600.0,
            pre_padding_minutes: 2,
            post_padding_minutes: 5,
            ..DvrJob::default()
        }
    }

    #[test]
    fn only_missed_failed_partial_offer_recovery() {
        assert!(recoverable_status(crate::dvr::STATUS_MISSED));
        assert!(recoverable_status(crate::dvr::STATUS_FAILED));
        assert!(recoverable_status(crate::dvr::STATUS_PARTIAL));
        assert!(!recoverable_status(crate::dvr::STATUS_SCHEDULED));
        assert!(!recoverable_status(crate::dvr::STATUS_COMPLETED));
        assert!(!recoverable_status(crate::dvr::STATUS_STOPPED));
        assert!(!recoverable_status(crate::dvr::STATUS_RECORDING));
        assert!(!recoverable_status(""));
    }

    #[test]
    fn resolve_finds_the_booked_channel() {
        let channel = catchup_channel();
        let job = job_for(&channel, crate::dvr::STATUS_FAILED);
        let other = channel_with(&[("catchup", "default"), ("provider-id", "prov-2")]);
        assert_eq!(
            resolve_recovery_channel(&job, &[other, channel.clone()]),
            Some(channel)
        );
    }

    #[test]
    fn resolve_refuses_ambiguous_or_unknown_channels() {
        let channel = catchup_channel();
        let job = job_for(&channel, crate::dvr::STATUS_FAILED);
        assert_eq!(resolve_recovery_channel(&job, &[]), None);
        let mut twin = channel.clone();
        twin.url = "http://example.com/other.m3u8".to_string();
        assert_eq!(resolve_recovery_channel(&job, &[channel, twin]), None);
        // A snapshot with no identity cannot resolve at all.
        let bare = DvrJob {
            channel: serde_json::json!({}),
            ..DvrJob::default()
        };
        assert_eq!(resolve_recovery_channel(&bare, &[catchup_channel()]), None);
    }

    #[test]
    fn recovery_show_uses_programme_times_and_keeps_padding() {
        let channel = catchup_channel();
        let job = job_for(&channel, crate::dvr::STATUS_PARTIAL);
        let show = recovery_show(&job, &channel, NOW).expect("recoverable");
        assert_eq!(show.programme.start, 1_700_000_120);
        assert_eq!(show.programme.stop, 1_700_003_300);
        assert_eq!(show.programme.title, "Show");
        assert_eq!(show.pre_minutes, 2);
        assert_eq!(show.post_minutes, 5);
        // The download window (programme widened by the schedule
        // padding) is exactly the booked record window.
        assert_eq!(show.programme.start - show.pre_minutes * 60, 1_700_000_000);
        assert_eq!(show.programme.stop + show.post_minutes * 60, 1_700_003_600);
    }

    #[test]
    fn recovery_refuses_non_recoverable_or_unavailable() {
        let channel = catchup_channel();
        // A missed recording inside the archive window recovers.
        let job = job_for(&channel, crate::dvr::STATUS_MISSED);
        assert!(recovery_show(&job, &channel, NOW).is_ok());
        let pending = job_for(&channel, crate::dvr::STATUS_SCHEDULED);
        assert_eq!(
            recovery_show(&pending, &channel, NOW).unwrap_err(),
            "Select a missed, failed or partial recording."
        );
        // Not over yet: the programme end is still in the future.
        let running = job_for(&channel, crate::dvr::STATUS_FAILED);
        assert_eq!(
            recovery_show(&running, &channel, 1_700_003_000).unwrap_err(),
            "This programme is unavailable in the channel archive."
        );
        // A channel with no archive cannot recover.
        let plain = channel_with(&[("provider-id", "prov-1")]);
        assert_eq!(
            recovery_show(&job, &plain, NOW).unwrap_err(),
            "This programme is unavailable in the channel archive."
        );
        // Older than the stated catch-up window.
        let dated = channel_with(&[
            ("catchup", "default"),
            ("catchup-days", "1"),
            ("provider-id", "prov-1"),
        ]);
        assert_eq!(
            recovery_show(&job, &dated, NOW).unwrap_err(),
            "This programme is unavailable in the channel archive."
        );
    }

    #[test]
    fn check_start_honours_the_account_limit() {
        // One configured Xtream source: its account key is the
        // budget key (the fixture style of recording_limits).
        let source = serde_json::json!({
            "id": "prov-1", "name": "My provider",
            "url": "http://a.example:8080/get.php?username=usera&password=passa&type=m3u",
            "type": "xtream",
        });
        let probe = serde_json::json!({"playlists": [source.clone()]});
        let channel = catchup_channel();
        let key = recording_limits::channel_key(&channel, &probe);
        assert!(!key.is_empty(), "account must resolve");
        let config = serde_json::json!({
            "playlists": [source],
            "recording_connection_limits": { key.clone(): 1 },
        });
        assert!(check_start(&config, &channel, &[]).is_ok());
        assert_eq!(
            check_start(&config, &channel, std::slice::from_ref(&key)).unwrap_err(),
            "The recording connection limit for this account has been reached (1). Existing recordings will continue."
        );
        // Another account's recording does not count.
        assert!(check_start(&config, &channel, &["other".to_string()]).is_ok());
        // No configured limit: no refusal.
        assert!(check_start(&probe, &channel, &[key]).is_ok());
    }
}

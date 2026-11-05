//! The exit-time work check (second-round audit N07): closing
//! the app can silently lose pending reminders, scheduled
//! recordings that have not started, and a download in flight,
//! so Python asks before exiting in each of those states
//! (`main.py` `on_close`, `planning_ui.py`
//! `_confirm_reminder_exit`). The decision and the message
//! text live here, wx-free; the adapter only gathers the live
//! state and shows the boxes in the returned order. A declined
//! box vetoes the exit, and later boxes are not shown.

use crate::dvr::{DvrJob, STATUS_SCHEDULED, STATUS_WAITING};
use crate::reminders::{Reminder, ReminderStatus};

/// One confirmation to show, in Python's order: reminders,
/// then recordings, then the download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExitPrompt {
    pub title: &'static str,
    pub message: String,
    /// Python's reminder box defaults to No (`wx.NO_DEFAULT`):
    /// an accidental Enter must not drop the reminders.
    pub no_default: bool,
}

/// Reminders that still need the app alive: scheduled or ready
/// and not yet past their end.
#[must_use]
pub fn pending_reminder_count(reminders: &[Reminder], now: i64) -> usize {
    reminders
        .iter()
        .filter(|r| {
            matches!(r.status, ReminderStatus::Scheduled | ReminderStatus::Ready) && r.end > now
        })
        .count()
}

/// DVR jobs that would never start if the app closed now,
/// earliest first. Python filters to `scheduled`; the Rust-only
/// `waiting` state (a job held by a connection conflict, issue
/// #134) is in the same position — not started, gone on exit —
/// so it is included.
#[must_use]
pub fn upcoming_recording_jobs(jobs: &[DvrJob]) -> Vec<&DvrJob> {
    let mut upcoming: Vec<&DvrJob> = jobs
        .iter()
        .filter(|job| job.status == STATUS_SCHEDULED || job.status == STATUS_WAITING)
        .collect();
    upcoming.sort_by(|a, b| a.start_ts.total_cmp(&b.start_ts));
    upcoming
}

/// Python's `_schedule_window_label`: the job's local window,
/// `YYYY-MM-DD HH:MM - YYYY-MM-DD HH:MM`.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // schedule times are whole seconds
pub fn schedule_window_label(job: &DvrJob) -> String {
    let fmt = |ts: f64| {
        chrono::DateTime::from_timestamp(ts as i64, 0).map_or_else(
            || "Unknown time".to_string(),
            |utc| {
                utc.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
    };
    format!("{} - {}", fmt(job.start_ts), fmt(job.stop_ts))
}

/// The prompts for one exit attempt, in the order Python asks
/// them. Empty means nothing would be lost: exit silently.
#[must_use]
pub fn exit_prompts(
    reminders: &[Reminder],
    jobs: &[DvrJob],
    recording_active: bool,
    download_active: bool,
    now: i64,
) -> Vec<ExitPrompt> {
    let mut prompts = Vec::new();
    let pending = pending_reminder_count(reminders, now);
    if pending > 0 {
        prompts.push(ExitPrompt {
            title: "Programme Reminders",
            message: format!(
                "Pending programme reminders: {pending}.\n\nNotifications require the application to remain running. Exit anyway?"
            ),
            no_default: true,
        });
    }
    let upcoming = upcoming_recording_jobs(jobs);
    if !upcoming.is_empty() {
        let names = upcoming
            .iter()
            .map(|job| {
                let title = if !job.display_title.is_empty() {
                    job.display_title.as_str()
                } else if !job.title.is_empty() {
                    job.title.as_str()
                } else {
                    "?"
                };
                format!("{title}\n{}", schedule_window_label(job))
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let count = upcoming.len();
        let message = if recording_active {
            format!(
                "A recording is in progress and {count} recording is still scheduled.\n\n{names}\n\nThe app must stay open to start them. Exit anyway?"
            )
        } else {
            format!(
                "{count} recording is scheduled but has not started yet.\n\n{names}\n\nThe app must stay open to start it. Exit anyway?"
            )
        };
        prompts.push(ExitPrompt {
            title: "Scheduled recordings",
            message,
            no_default: false,
        });
    }
    if download_active {
        prompts.push(ExitPrompt {
            title: "Download in progress",
            message: "A download is still in progress.\n\nIf you exit now it will stop, and only the part captured so far is kept.\n\nExit anyway?"
                .to_string(),
            no_default: false,
        });
    }
    prompts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminders::ReminderStatus;

    fn reminder(status: ReminderStatus, end: i64) -> Reminder {
        Reminder {
            id: "r".to_string(),
            channel_key: "k".to_string(),
            title: "Show".to_string(),
            channel: "Chan".to_string(),
            start: end - 3600,
            end,
            lead: 300,
            status,
        }
    }

    fn job(status: &str, title: &str, start_ts: f64) -> DvrJob {
        DvrJob {
            id: format!("j{start_ts}"),
            status: status.to_string(),
            title: title.to_string(),
            channel_name: "Chan".to_string(),
            display_title: title.to_string(),
            channel: serde_json::json!({}),
            program: serde_json::json!({}),
            format: "ts".to_string(),
            start_at: String::new(),
            end_at: String::new(),
            start_ts,
            stop_ts: start_ts + 3600.0,
            pre_padding_minutes: 0,
            post_padding_minutes: 0,
            created_at: String::new(),
            started_late: false,
            wait_reason: String::new(),
            recording_id: None,
            output_path: String::new(),
            message: String::new(),
            repeat: String::new(),
        }
    }

    #[test]
    fn pending_reminders_count_only_live_states() {
        let now = 1_000_000;
        let reminders = vec![
            reminder(ReminderStatus::Scheduled, now + 60),
            reminder(ReminderStatus::Ready, now + 60),
            reminder(ReminderStatus::Notified, now + 60),
            reminder(ReminderStatus::Expired, now + 60),
            reminder(ReminderStatus::Scheduled, now - 1),
        ];
        assert_eq!(pending_reminder_count(&reminders, now), 2);
    }

    #[test]
    fn prompts_follow_pythons_order_and_wording() {
        let now = 1_700_000_000;
        let reminders = vec![reminder(ReminderStatus::Scheduled, now + 600)];
        let jobs = vec![
            job(STATUS_SCHEDULED, "Later Show", 1_700_007_200.0),
            job("completed", "Done Show", 1_700_000_060.0),
            job(STATUS_WAITING, "First Show", 1_700_003_600.0),
        ];
        let prompts = exit_prompts(&reminders, &jobs, false, true, now);
        assert_eq!(prompts.len(), 3);
        assert_eq!(prompts[0].title, "Programme Reminders");
        assert!(prompts[0].no_default);
        assert!(prompts[0]
            .message
            .contains("Pending programme reminders: 1."));
        assert_eq!(prompts[1].title, "Scheduled recordings");
        assert!(prompts[1]
            .message
            .starts_with("2 recording is scheduled but has not started yet."));
        // Earliest first in the names block, window after title.
        let first_pos = prompts[1].message.find("First Show").unwrap();
        let later_pos = prompts[1].message.find("Later Show").unwrap();
        assert!(first_pos < later_pos);
        assert!(prompts[1].message.contains(" - "));
        assert_eq!(prompts[2].title, "Download in progress");
    }

    #[test]
    fn active_recording_changes_the_recordings_wording() {
        let now = 1_700_000_000;
        let jobs = vec![job(STATUS_SCHEDULED, "Show", 1_700_000_060.0)];
        let prompts = exit_prompts(&[], &jobs, true, false, now);
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0]
            .message
            .starts_with("A recording is in progress and 1 recording is still scheduled."));
    }

    #[test]
    fn nothing_at_risk_means_no_prompts() {
        let now = 1_700_000_000;
        let jobs = vec![job("completed", "Done", 1_700_000_060.0)];
        assert!(exit_prompts(&[], &jobs, false, false, now).is_empty());
        // A running recording alone does not prompt: Python only
        // warns about recordings through the scheduled-jobs box.
        assert!(exit_prompts(&[], &[], true, false, now).is_empty());
    }
}

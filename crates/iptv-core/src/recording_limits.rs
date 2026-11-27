//! Python `recording_limits`: account-scoped recording budgets
//! and exact interval capacity checks, plus the conflict-row
//! computation for the recording-conflicts dialog (Python
//! `recording_conflicts_ui.conflict_rows`). These are
//! user-assigned budgets for recordings, not a claim about
//! connections on other devices.

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::Digest;

use crate::account_info::{discover_accounts, Account};
use crate::Channel;

/// Python `account_key`: the sha256 of the account's dedupe key
/// serialised the way Python's `json.dumps` serialises the tuple
/// (`, ` separators, ASCII-escaped strings), so budget keys in a
/// shared config match the Python client's byte for byte.
#[must_use]
pub fn account_key(account: &Account) -> String {
    let (kind, host, identity) = account.dedupe_key();
    let text = format!(
        "[{}, {}, {}]",
        py_json_string(&kind),
        py_json_string(&host),
        py_json_string(&identity)
    );
    crate::hex_lower(&sha2::Sha256::digest(text.as_bytes()))
}

/// Python `json.dumps` string escaping (default
/// `ensure_ascii=True`).
fn py_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                use std::fmt::Write;
                let mut buffer = [0_u16; 2];
                for unit in c.encode_utf16(&mut buffer) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// Python `normalize`: keep only well-formed budget entries —
/// 64-hex account keys mapped to integer limits in 1..=64.
#[must_use]
pub fn normalize(value: Option<&Value>) -> BTreeMap<String, i64> {
    let mut result = BTreeMap::new();
    let Some(map) = value.and_then(Value::as_object) else {
        return result;
    };
    for (key, limit) in map {
        let key_ok = key.len() == 64
            && key
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase());
        let limit = limit.as_i64();
        if key_ok {
            if let Some(limit) = limit.filter(|limit| (1..=64).contains(limit)) {
                // `type(limit) is int`: a JSON float like 2.0 has
                // no as_i64 in serde_json only when it carries a
                // fraction; 2.0 parses as f64 and is rejected,
                // matching Python.
                result.insert(key.clone(), limit);
            }
        }
    }
    result
}

fn attr<'a>(channel: &'a Channel, key: &str) -> &'a str {
    channel
        .unknown_attributes
        .get(key)
        .map_or("", String::as_str)
}

fn playlists(config: &Value) -> Vec<Value> {
    config
        .get("playlists")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Python `channel_key`: the budget account key for a channel —
/// via its provider id against each configured source in turn
/// (aliases of one account keep their provider id but hash to
/// the same key), else via account discovery on the channel
/// itself when that yields exactly one account.
#[must_use]
pub fn channel_key(channel: &Channel, config: &Value) -> String {
    let provider = attr(channel, "provider-id");
    if !provider.is_empty() {
        for source in playlists(config) {
            for account in discover_accounts(&[source], &[]) {
                if account.provider_id == provider {
                    return account_key(&account);
                }
            }
        }
    }
    let accounts = discover_accounts(&[], std::slice::from_ref(channel));
    if accounts.len() == 1 {
        return account_key(&accounts[0]);
    }
    String::new()
}

/// A DVR job projected for budget/conflict math (Python's job
/// dict, whitelisted to the fields these functions read).
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub id: String,
    pub title: String,
    pub status: String,
    pub start_ts: f64,
    pub stop_ts: f64,
    pub pre_padding_minutes: i64,
    pub post_padding_minutes: i64,
    /// The job's pause window, when it carries one (PR #44).
    pub pause_dates: Option<[String; 2]>,
    pub channel: Channel,
}

/// Whether this view's occurrence is paused (Python
/// `dvr.paused_occurrence` on the job dict).
#[must_use]
pub fn view_paused(job: &JobView) -> bool {
    job.status == "scheduled"
        && job.pause_dates.as_ref().is_some_and(|period| {
            crate::dvr::paused_in(period, job.start_ts, job.pre_padding_minutes)
        })
}

impl JobView {
    /// A schedule job as the limits see it: Rust's `waiting` counts
    /// as scheduled.
    #[must_use]
    pub fn from_job(job: &crate::dvr::DvrJob) -> Self {
        Self {
            id: job.id.clone(),
            title: job.title.clone(),
            status: if job.status == crate::dvr::STATUS_WAITING {
                crate::dvr::STATUS_SCHEDULED.to_string()
            } else {
                job.status.clone()
            },
            start_ts: job.start_ts,
            stop_ts: job.stop_ts,
            pre_padding_minutes: job.pre_padding_minutes,
            post_padding_minutes: job.post_padding_minutes,
            pause_dates: job.pause_dates.clone(),
            channel: crate::dvr::channel_from_snapshot(&job.channel),
        }
    }
}

fn job_active(job: &JobView) -> bool {
    matches!(job.status.as_str(), "scheduled" | "recording" | "stopping")
        && job.start_ts.is_finite()
        && job.stop_ts.is_finite()
}

/// Python `peak_demand` at its post-PR-#36 state: no configured
/// limits means no demand answer at all (returned before any
/// channel-key work), and each job is attributed to its account
/// through the provider-id map built from the configured
/// sources, falling back to the job channel's own key.
///
/// Returns `(peak, limit)` for the target channel's account.
#[must_use]
pub fn peak_demand(
    config: &Value,
    channel: &Channel,
    start: f64,
    end: f64,
    jobs: &[JobView],
    exclude: Option<&str>,
) -> Option<(i64, i64)> {
    let limits = normalize(config.get("recording_connection_limits"));
    if limits.is_empty() {
        return None;
    }
    let key = channel_key(channel, config);
    let limit = limits.get(&key).copied().unwrap_or(0);
    if limit == 0 {
        return None;
    }
    let mut by_provider: BTreeMap<String, String> = BTreeMap::new();
    for source in playlists(config) {
        for account in discover_accounts(&[source], &[]) {
            if !account.provider_id.is_empty() {
                by_provider.insert(account.provider_id.clone(), account_key(&account));
            }
        }
    }
    let mut events: Vec<(f64, i64)> = vec![(start, 1), (end, -1)];
    for job in jobs {
        if view_paused(job) {
            continue;
        }
        if Some(job.id.as_str()) == exclude || !job_active(job) {
            continue;
        }
        let (a, b) = (job.start_ts, job.stop_ts);
        if a < end && b > start {
            let identity = by_provider
                .get(attr(&job.channel, "provider-id"))
                .cloned()
                .unwrap_or_else(|| channel_key(&job.channel, config));
            if identity != key {
                continue;
            }
            events.push((a.max(start), 1));
            events.push((b.min(end), -1));
        }
    }
    // Ends sort before starts at a shared boundary (Python sorts
    // (time, delta) tuples, and -1 < 1).
    events.sort_by(|x, y| {
        x.0.partial_cmp(&y.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.1.cmp(&y.1))
    });
    let mut current = 0_i64;
    let mut peak = 0_i64;
    for (_, delta) in events {
        current += delta;
        peak = peak.max(current);
    }
    Some((peak, limit))
}

/// Python `warning`: the schedule-time question for a new or
/// edited job — the over-budget wording when budgets apply, the
/// plain overlap count when they do not, empty when there is
/// nothing to ask.
#[must_use]
pub fn warning(
    config: &Value,
    channel: &Channel,
    start: f64,
    end: f64,
    jobs: &[JobView],
    exclude: Option<&str>,
) -> String {
    match peak_demand(config, channel, start, end, jobs, exclude) {
        None => {
            let overlaps = jobs
                .iter()
                .filter(|job| {
                    !view_paused(job)
                        && Some(job.id.as_str()) != exclude
                        && job.start_ts < end
                        && job.stop_ts > start
                })
                .count();
            if overlaps > 0 {
                format!("This overlaps {overlaps} recording(s). Continue?")
            } else {
                String::new()
            }
        }
        Some((peak, limit)) => {
            if peak > limit {
                format!(
                    "This schedule needs {peak} simultaneous recording connections for the same account, but its recording limit is {limit}. Schedule it anyway?"
                )
            } else {
                String::new()
            }
        }
    }
}

/// One row of the conflicts dialog (Python `conflict_rows`'
/// row dict): the job snapshot plus its display account alias.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictRow {
    pub job: JobView,
    /// The configured playlist alias — never a URL or credential.
    pub account: String,
    pub start: f64,
    pub end: f64,
}

/// The configured playlist name for a channel's account (never a URL
/// or credential), empty when none is configured.
#[must_use]
pub fn account_alias(config: &Value, channel: &Channel) -> String {
    let provider = attr(channel, "provider-id");
    let playlist = attr(channel, "playlist-id");
    for entry in playlists(config) {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        if id == provider || (!playlist.is_empty() && id == playlist) {
            return entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }
    String::new()
}

/// Python `conflict_rows`: the jobs strictly overlapping a
/// window (touching windows do not overlap), restricted to the
/// target channel's account when budgets are configured.
#[must_use]
pub fn conflict_rows(
    config: &Value,
    channel: &Channel,
    start: f64,
    end: f64,
    jobs: &[JobView],
    exclude: Option<&str>,
    canceled: &dyn Fn() -> bool,
) -> Vec<ConflictRow> {
    let limits = normalize(config.get("recording_connection_limits"));
    let target_key = if limits.is_empty() {
        String::new()
    } else {
        channel_key(channel, config)
    };
    let mut keys: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let mut rows = Vec::new();
    for job in jobs {
        if canceled() {
            return Vec::new();
        }
        if Some(job.id.as_str()) == exclude || !job_active(job) {
            continue;
        }
        if view_paused(job) {
            continue;
        }
        if !(job.start_ts < end && job.stop_ts > start) {
            continue;
        }
        if !limits.is_empty() {
            let identity = (
                attr(&job.channel, "provider-id").to_string(),
                attr(&job.channel, "playlist-id").to_string(),
                job.channel.url.clone(),
            );
            let job_key = keys
                .entry(identity)
                .or_insert_with(|| channel_key(&job.channel, config))
                .clone();
            if job_key != target_key {
                continue;
            }
        }
        rows.push(ConflictRow {
            account: account_alias(config, &job.channel),
            start: job.start_ts,
            end: job.stop_ts,
            job: job.clone(),
        });
    }
    rows.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.job.id.cmp(&b.job.id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn channel_with(provider: &str, playlist: &str, url: &str) -> Channel {
        let mut attrs = BTreeMap::new();
        if !provider.is_empty() {
            attrs.insert("provider-id".to_string(), provider.to_string());
        }
        if !playlist.is_empty() {
            attrs.insert("playlist-id".to_string(), playlist.to_string());
        }
        Channel {
            name: "Chan".to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs,
        }
    }

    fn job(id: &str, status: &str, start: f64, stop: f64, channel: Channel, pre: i64) -> JobView {
        JobView {
            id: id.to_string(),
            title: format!("Job {id}"),
            status: status.to_string(),
            start_ts: start,
            stop_ts: stop,
            pre_padding_minutes: pre,
            post_padding_minutes: 0,
            pause_dates: None,
            channel,
        }
    }

    /// Two configured Xtream sources on different hosts =
    /// different accounts; provider ids pA / pB.
    fn config_with_limits(limit_a: i64) -> (Value, String) {
        let source_a = serde_json::json!({
            "id": "pA", "name": "My provider",
            "url": "http://a.example:8080/get.php?username=usera&password=passa&type=m3u",
            "type": "xtream",
        });
        let source_b = serde_json::json!({
            "id": "pB", "name": "Other",
            "url": "http://b.example:8080/get.php?username=userb&password=passb&type=m3u",
            "type": "xtream",
        });
        // Discover the account key for A the same way the code
        // under test does, then pin the limit to it.
        let probe = serde_json::json!({"playlists": [source_a.clone(), source_b.clone()]});
        let key_a = channel_key(&channel_with("pA", "", "http://a.example/x"), &probe);
        assert!(!key_a.is_empty(), "account A must resolve");
        let mut limits = serde_json::Map::new();
        limits.insert(key_a.clone(), serde_json::json!(limit_a));
        let config = serde_json::json!({
            "playlists": [source_a, source_b],
            "recording_connection_limits": Value::Object(limits),
        });
        (config, key_a)
    }

    #[test]
    fn normalize_drops_malformed_entries() {
        let good = "a".repeat(64);
        let value = serde_json::json!({
            good.clone(): 3,
            "not-hex": 2,
            "B".repeat(64): 2,
            good.clone() + "x": 1,
        });
        let mut by_key = normalize(Some(&value));
        assert_eq!(by_key.remove(&good), Some(3));
        assert!(by_key.is_empty());
        let zero = serde_json::json!({good.clone(): 0});
        assert!(normalize(Some(&zero)).is_empty());
        let float = serde_json::json!({good: 2.5});
        assert!(normalize(Some(&float)).is_empty());
    }

    #[test]
    fn conflict_rows_filter_by_account_and_strict_overlap() {
        let (config, _key) = config_with_limits(1);
        let target = channel_with("pA", "", "http://a.example/live/1");
        let jobs = vec![
            job(
                "a-overlap",
                "scheduled",
                100.0,
                200.0,
                channel_with("pA", "", "http://a.example/live/2"),
                2,
            ),
            job(
                "b-overlap",
                "scheduled",
                100.0,
                200.0,
                channel_with("pB", "", "http://b.example/live/2"),
                0,
            ),
            job(
                "a-touching",
                "scheduled",
                300.0,
                400.0,
                channel_with("pA", "", "http://a.example/live/3"),
                0,
            ),
            job(
                "a-done",
                "completed",
                100.0,
                200.0,
                channel_with("pA", "", "http://a.example/live/4"),
                0,
            ),
        ];
        let rows = conflict_rows(&config, &target, 150.0, 300.0, &jobs, None, &|| false);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job.id, "a-overlap");
        assert_eq!(rows[0].job.pre_padding_minutes, 2);
        assert_eq!(rows[0].account, "My provider");
        // exclude removes it.
        let rows = conflict_rows(
            &config,
            &target,
            150.0,
            300.0,
            &jobs,
            Some("a-overlap"),
            &|| false,
        );
        assert!(rows.is_empty());
        // Without limits, both overlapping accounts are shown.
        let bare = serde_json::json!({"playlists": []});
        let rows = conflict_rows(&bare, &target, 150.0, 300.0, &jobs, None, &|| false);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn warning_and_peak_demand_follow_budgets() {
        let (config, _key) = config_with_limits(1);
        let target = channel_with("pA", "", "http://a.example/live/1");
        let jobs = vec![job(
            "a-overlap",
            "scheduled",
            100.0,
            200.0,
            channel_with("pA", "", "http://a.example/live/2"),
            0,
        )];
        // Target window overlapping the existing job: peak 2 > 1.
        let text = warning(&config, &target, 150.0, 250.0, &jobs, None);
        assert!(text.contains("needs 2 simultaneous"), "{text}");
        assert!(text.contains("limit is 1"), "{text}");
        // No limits configured: peak_demand is None and the
        // warning falls back to the overlap count.
        let bare = serde_json::json!({"playlists": []});
        assert_eq!(peak_demand(&bare, &target, 150.0, 250.0, &jobs, None), None);
        assert_eq!(
            warning(&bare, &target, 150.0, 250.0, &jobs, None),
            "This overlaps 1 recording(s). Continue?"
        );
        assert_eq!(warning(&bare, &target, 500.0, 600.0, &jobs, None), "");
    }

    #[test]
    fn paused_jobs_do_not_count_toward_peak_or_conflicts() {
        let (config, _key) = config_with_limits(1);
        let target = channel_with("pA", "", "http://a.example/live/1");
        let mut paused = job(
            "a-paused",
            "scheduled",
            100.0,
            200.0,
            channel_with("pA", "", "http://a.example/live/2"),
            0,
        );
        // The job's programme date in local time, paused for that day.
        let date = crate::dvr::programme_local_start(paused.start_ts, 0)
            .expect("start")
            .date()
            .format("%Y-%m-%d")
            .to_string();
        paused.pause_dates = Some([date.clone(), date]);
        let jobs = vec![paused];
        // Only the target window itself counts: peak stays at the limit.
        assert_eq!(
            peak_demand(&config, &target, 150.0, 250.0, &jobs, None),
            Some((1, 1))
        );
        assert_eq!(warning(&config, &target, 150.0, 250.0, &jobs, None), "");
        assert!(conflict_rows(&config, &target, 150.0, 250.0, &jobs, None, &|| false).is_empty());
    }

    #[test]
    fn account_key_matches_python_json_shape() {
        // Python: sha256(json.dumps(("xtream", "h.example", "user")))
        // == sha256('["xtream", "h.example", "user"]').
        let account = Account {
            kind: "xtream".to_string(),
            base_url: "http://h.example".to_string(),
            username: "user".to_string(),
            password: String::new(),
            mac: String::new(),
            name: String::new(),
            provider_id: String::new(),
            user_agent: String::new(),
            detected: false,
            detected_from: String::new(),
        };
        let expected = crate::hex_lower(&sha2::Sha256::digest(
            b"[\"xtream\", \"h.example\", \"user\"]".as_slice(),
        ));
        assert_eq!(account_key(&account), expected);
    }
}

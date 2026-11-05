//! Per-recording ffmpeg logs: parsing, problem summaries and
//! problem times (N12; port of the log half of Python's
//! `recorder.py`).
//!
//! Every recording runs ffmpeg with `-loglevel level+datetime+info`
//! when the binary supports it (probed once per path), stderr
//! redirected into `<recordings>/logs/<name>.log`. When ffmpeg
//! exits, the engine reads the problems back out of the log,
//! appends a readable summary to it, and the app reports the
//! outcome in Python's words; the Recording Problem Times dialog
//! places each kind of problem in time from the same parse.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

/// Subfolder of the recordings folder holding the ffmpeg logs
/// (Python `RECORDING_LOG_DIRNAME`).
pub const RECORDING_LOG_DIRNAME: &str = "logs";

/// Problem lines kept for a finish report (Python
/// `STDERR_TAIL_LINES`).
pub const STDERR_TAIL_LINES: usize = 12;

/// Bytes of log tail the finish-path readers scan (Python
/// `_LOG_TAIL_WINDOW_BYTES`).
pub const LOG_TAIL_WINDOW_BYTES: u64 = 262_144;

/// Occurrences of one problem kind the problem-times list names
/// (Python `_PROBLEM_TIMES_LIMIT`).
pub const PROBLEM_TIMES_LIMIT: usize = 12;

/// Kinds of problem the closing summary lists per phase before
/// it stops (Python `_SUMMARY_MAX_KINDS`).
const SUMMARY_MAX_KINDS: usize = 20;

/// Occurrences listed one by one in a summary line before only
/// the first and last are named (Python `_SUMMARY_MAX_TIMES`).
const SUMMARY_MAX_TIMES: usize = 5;

const SUMMARY_MARKER: &str = "# ===== Recording summary =====";

const LEVELS: [&str; 8] = [
    "panic", "fatal", "error", "warning", "info", "verbose", "debug", "trace",
];

const PROBLEM_LEVELS: [&str; 4] = ["panic", "fatal", "error", "warning"];

/// Where the full ffmpeg stderr for `out_path` is written
/// (Python `recording_log_path`).
#[must_use]
pub fn recording_log_path(out_dir: &Path, out_path: &Path) -> PathBuf {
    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    out_dir
        .join(RECORDING_LOG_DIRNAME)
        .join(format!("{stem}.log"))
}

/// One parsed ffmpeg log line (Python `parse_log_line`'s dict).
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedLogLine {
    /// When ffmpeg time-stamped the line, if it did.
    pub when: Option<NaiveDateTime>,
    /// The level, lowercased (`error`, `warning`, `info`, ...).
    pub level: String,
    /// The first `[name @ address]` prefix's name, if any.
    pub component: String,
    pub message: String,
}

/// The `YYYY-MM-DD HH:MM:SS` shape at a fixed position, with an
/// optional `.fraction` — the stamp `level+datetime` writes.
fn stamp_at(text: &str) -> Option<(NaiveDateTime, usize)> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    let shape = digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && bytes[4] == b'-'
        && digit(5)
        && digit(6)
        && bytes[7] == b'-'
        && digit(8)
        && digit(9)
        && bytes[10] == b' '
        && digit(11)
        && digit(12)
        && bytes[13] == b':'
        && digit(14)
        && digit(15)
        && bytes[16] == b':'
        && digit(17)
        && digit(18);
    if !shape {
        return None;
    }
    let num = |from: usize, to: usize| text[from..to].parse::<i32>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, min, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut consumed = 19;
    let mut nanos: u32 = 0;
    if bytes.get(19) == Some(&b'.') {
        let mut end = 20;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end > 20 {
            let frac = &text[20..end];
            let mut scaled = frac.to_string();
            while scaled.len() < 9 {
                scaled.push('0');
            }
            scaled.truncate(9);
            nanos = scaled.parse().unwrap_or(0);
            consumed = end;
        }
    }
    let date = NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)?;
    let time = NaiveTime::from_hms_nano_opt(
        u32::try_from(hour).ok()?,
        u32::try_from(min).ok()?,
        u32::try_from(sec).ok()?,
        nanos,
    )?;
    Some((date.and_time(time), consumed))
}

/// Split one ffmpeg log line into its time, level, component and
/// message (Python `parse_log_line`). `None` for lines without a
/// level (the header, stats banners, `[q] command received`).
#[must_use]
pub fn parse_log_line(line: &str) -> Option<ParsedLogLine> {
    let line = line.trim();
    let (when, rest) = match stamp_at(line) {
        Some((stamp, consumed)) => {
            // The stamp is followed by exactly one space in the
            // regex; the remainder is matched as-is.
            let rest = line[consumed..]
                .strip_prefix(' ')
                .unwrap_or(&line[consumed..]);
            (Some(stamp), rest)
        }
        None => (None, line),
    };
    // Leading bracket groups: component prefixes, then the one
    // level bracket. Python's regex is greedy, so the level is
    // the LAST leading group naming a level.
    let bytes = rest.as_bytes();
    let mut groups: Vec<(String, usize)> = Vec::new();
    let mut pos = 0;
    while bytes.get(pos) == Some(&b'[') {
        let close = rest[pos..].find(']')? + pos;
        groups.push((rest[pos + 1..close].to_string(), close + 1));
        // Prefix groups are followed by a space; the level
        // bracket may end the bracket run without one.
        if bytes.get(close + 1) == Some(&b' ') && bytes.get(close + 2) == Some(&b'[') {
            pos = close + 2;
        } else {
            break;
        }
    }
    let level_index = groups
        .iter()
        .rposition(|(content, _)| LEVELS.contains(&content.to_lowercase().as_str()))?;
    let (level_raw, level_end) = &groups[level_index];
    let component = if level_index > 0 {
        groups[0]
            .0
            .split(" @ ")
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        String::new()
    };
    let message = rest[*level_end..]
        .strip_prefix(' ')
        .unwrap_or(&rest[*level_end..])
        .trim()
        .to_string();
    Some(ParsedLogLine {
        when,
        level: level_raw.to_lowercase(),
        component,
        message,
    })
}

/// A log line minus ffmpeg's date prefix, for dialogs that quote
/// it (Python `strip_log_timestamp`).
#[must_use]
pub fn strip_log_timestamp(line: &str) -> String {
    let line = line.trim();
    match stamp_at(line) {
        Some((_, consumed)) => line[consumed..].trim_start().to_string(),
        None => line.to_string(),
    }
}

/// Whether a line reports a problem (Python `_is_problem_line`).
#[must_use]
pub fn is_problem_line(line: &str) -> bool {
    parse_log_line(line).is_some_and(|parsed| PROBLEM_LEVELS.contains(&parsed.level.as_str()))
}

/// Every line of a recording log. ffmpeg ends stats lines with a
/// bare CR, so both CR and LF separate lines (Python
/// `_read_log_lines`).
fn read_log_lines(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&data)
        .split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// The log's tail window as text (the finish-path readers never
/// scan a whole long log).
fn read_log_tail(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map_or(0, |m| m.len());
    if size > LOG_TAIL_WINDOW_BYTES {
        use std::io::Seek;
        let _ = file.seek(std::io::SeekFrom::Start(size - LOG_TAIL_WINDOW_BYTES));
    }
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&data).into_owned()
}

/// The last few warning/error lines of a recording log, for the
/// finish dialog (Python `read_log_problems`).
#[must_use]
pub fn read_log_problems(path: &Path, limit: usize) -> Vec<String> {
    let text = read_log_tail(path);
    let problems: Vec<String> = text
        .split(['\n', '\r'])
        .filter(|line| is_problem_line(line))
        .map(strip_log_timestamp)
        .collect();
    problems[problems.len().saturating_sub(limit)..].to_vec()
}

/// The newest `time=HH:MM:SS.xx` value in a text, as media
/// seconds (ffmpeg's stats clock).
fn find_time_value(text: &str) -> Option<f64> {
    let mut found = None;
    let mut rest = text;
    while let Some(index) = rest.find("time=") {
        let after = &rest[index + 5..];
        rest = after;
        if let Some(value) = parse_time_value(after) {
            found = Some(value);
        }
    }
    found
}

/// Parse `H+:MM:SS(.frac)` at the start of `text`.
fn parse_time_value(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let take_digits = |pos: &mut usize| -> Option<String> {
        let start = *pos;
        while bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
            *pos += 1;
        }
        if *pos == start {
            None
        } else {
            Some(text[start..*pos].to_string())
        }
    };
    let hours: f64 = take_digits(&mut pos)?.parse().ok()?;
    if bytes.get(pos) != Some(&b':') {
        return None;
    }
    pos += 1;
    let minutes: f64 = take_digits(&mut pos)?.parse().ok()?;
    if bytes.get(pos) != Some(&b':') {
        return None;
    }
    pos += 1;
    let seconds_text = take_digits(&mut pos)?;
    let mut seconds: f64 = seconds_text.parse().ok()?;
    if bytes.get(pos) == Some(&b'.') {
        let mut frac_end = pos + 1;
        while bytes.get(frac_end).is_some_and(u8::is_ascii_digit) {
            frac_end += 1;
        }
        if frac_end > pos + 1 {
            let frac: f64 = format!("0.{}", &text[pos + 1..frac_end]).parse().ok()?;
            seconds += frac;
        }
    }
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// Media seconds ffmpeg has written so far, from the newest
/// `time=` line in the log (Python `parse_ffmpeg_progress`).
/// `None` when the log carries none.
#[must_use]
pub fn parse_ffmpeg_progress(path: &Path) -> Option<f64> {
    find_time_value(&read_log_tail(path))
}

/// One kind of problem in a log summary: an ffmpeg message with
/// its numbers folded out, counted and placed in time.
#[derive(Clone, Debug, PartialEq)]
pub struct ProblemKind {
    pub level: String,
    pub component: String,
    /// The first occurrence's message, verbatim.
    pub message: String,
    pub count: u64,
    /// Seconds since the output opened, one per occurrence;
    /// empty when ffmpeg did not time-stamp its lines.
    pub offsets: Vec<f64>,
}

/// A recording log's problems grouped by kind, split at the
/// start of writing (Python `summarize_log`'s result).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogSummary {
    pub opening: Vec<ProblemKind>,
    pub recording: Vec<ProblemKind>,
    /// Whether the recording got as far as writing at all.
    pub output_opened: bool,
    /// Whether problems can be placed in time at all.
    pub timestamps: bool,
}

/// Fold the digits out of a message so otherwise-identical
/// messages group together (timestamps, sizes vary).
fn fold_numbers(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut in_digits = false;
    for ch in message.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
        } else {
            out.push(ch);
            in_digits = false;
        }
    }
    out
}

/// The count in a `Last message repeated N times` line: without
/// `-loglevel repeat` ffmpeg folds identical lines into this one.
fn repeated_count(line: &str) -> Option<u64> {
    let marker = "Last message repeated ";
    let start = line.find(marker)? + marker.len();
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Group a recording log's problems by kind, split at the start
/// of writing (Python `summarize_log`).
#[must_use]
pub fn summarize_log(path: &Path) -> LogSummary {
    let mut summary = LogSummary::default();
    let mut index: HashMap<(bool, String, String, String), usize> = HashMap::new();
    let mut opened_at: Option<NaiveDateTime> = None;
    let mut stamped = false;
    // The kind the previous line belonged to, for folded repeats:
    // (recording phase, position in that phase's vec).
    let mut last_kind: Option<(bool, usize)> = None;
    for line in read_log_lines(path) {
        if let Some(extra) = repeated_count(&line) {
            if let Some((recording, position)) = last_kind {
                let kinds = if recording {
                    &mut summary.recording
                } else {
                    &mut summary.opening
                };
                if let Some(kind) = kinds.get_mut(position) {
                    kind.count += extra;
                    if let Some(last) = kind.offsets.last().copied() {
                        kind.offsets.extend(std::iter::repeat_n(
                            last,
                            usize::try_from(extra).unwrap_or(0),
                        ));
                    }
                }
            }
            continue;
        }
        let Some(parsed) = parse_log_line(&line) else {
            continue;
        };
        if parsed.when.is_some() {
            stamped = true;
        }
        if !summary.output_opened
            && parsed.level == "info"
            && parsed.message.starts_with("Output #")
            && parsed
                .message
                .as_bytes()
                .get(8)
                .is_some_and(u8::is_ascii_digit)
        {
            summary.output_opened = true;
            opened_at = parsed.when;
            continue;
        }
        if !PROBLEM_LEVELS.contains(&parsed.level.as_str()) {
            last_kind = None;
            continue;
        }
        let recording = summary.output_opened;
        let level = if parsed.level == "panic" {
            "fatal".to_string()
        } else {
            parsed.level.clone()
        };
        let key = (
            recording,
            level.clone(),
            parsed.component.clone(),
            fold_numbers(&parsed.message),
        );
        let position = if let Some(position) = index.get(&key) {
            *position
        } else {
            let kinds = if recording {
                &mut summary.recording
            } else {
                &mut summary.opening
            };
            kinds.push(ProblemKind {
                level: level.clone(),
                component: parsed.component.clone(),
                message: parsed.message.clone(),
                count: 0,
                offsets: Vec::new(),
            });
            let position = kinds.len() - 1;
            index.insert(key, position);
            position
        };
        let kinds = if recording {
            &mut summary.recording
        } else {
            &mut summary.opening
        };
        let kind = &mut kinds[position];
        kind.count += 1;
        if recording {
            if let (Some(opened), Some(when)) = (opened_at, parsed.when) {
                let offset = when
                    .signed_duration_since(opened)
                    .to_std()
                    .map_or(0.0, |d| d.as_secs_f64());
                kind.offsets.push(offset.max(0.0));
            }
        }
        last_kind = Some((recording, position));
    }
    summary.timestamps = stamped && opened_at.is_some();
    summary
}

/// Warning/error/fatal totals over the whole log (Python
/// `count_log_problems`): (warnings, errors, fatals).
#[must_use]
pub fn count_log_problems(path: &Path) -> (u64, u64, u64) {
    let summary = summarize_log(path);
    let mut counts = (0u64, 0u64, 0u64);
    for kind in summary.opening.iter().chain(summary.recording.iter()) {
        match kind.level.as_str() {
            "warning" => counts.0 += kind.count,
            "error" => counts.1 += kind.count,
            "fatal" => counts.2 += kind.count,
            _ => {}
        }
    }
    counts
}

/// `h:mm:ss`, always with hours, so times in a list line up
/// (Python `_clock`).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a duration's whole seconds
pub fn clock_hms(seconds: f64) -> String {
    // Python's round() is half-to-even; match it so a problem
    // time reads the same in both clients.
    let total = seconds.max(0.0).round_ties_even() as u64;
    format!(
        "{}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

fn plural(count: u64, word: &str) -> String {
    if count == 1 {
        format!("{count} {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn problem_totals(kinds: &[ProblemKind]) -> String {
    let mut totals: HashMap<&str, u64> = HashMap::new();
    for kind in kinds {
        *totals.entry(kind.level.as_str()).or_default() += kind.count;
    }
    format!(
        "{}, {}, {}",
        plural(totals.get("warning").copied().unwrap_or(0), "warning"),
        plural(totals.get("error").copied().unwrap_or(0), "error"),
        plural(totals.get("fatal").copied().unwrap_or(0), "fatal error"),
    )
}

fn describe_kind(kind: &ProblemKind) -> String {
    let source = if kind.component.is_empty() {
        String::new()
    } else {
        format!(" from {}", kind.component)
    };
    let times = if kind.count == 1 {
        "once".to_string()
    } else {
        format!("{} times", kind.count)
    };
    let level = &kind.level;
    let message = &kind.message;
    let mut text = format!("{times}: {level}{source}: {message}");
    // A burst within one second is one time, not the same time
    // repeated.
    let mut clocks: Vec<String> = Vec::new();
    for value in &kind.offsets {
        let clock = clock_hms(*value);
        if !clocks.contains(&clock) {
            clocks.push(clock);
        }
    }
    if !clocks.is_empty() {
        if clocks.len() <= SUMMARY_MAX_TIMES {
            let _ = write!(text, ". At {}.", clocks.join(", "));
        } else {
            let _ = write!(
                text,
                ". First at {}, last at {}.",
                clocks.first().map_or("", String::as_str),
                clocks.last().map_or("", String::as_str)
            );
        }
    }
    text
}

/// Human-readable byte size, or a placeholder when unknown
/// (Python `format_size`).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole bytes in the B line
pub fn format_size(num_bytes: Option<f64>) -> String {
    let Some(mut value) = num_bytes.filter(|v| *v > 0.0) else {
        return "--".to_string();
    };
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if value < 1024.0 || unit == "TB" {
            if unit == "B" {
                return format!("{} {unit}", value as u64);
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    "--".to_string()
}

/// One phase's grouped problem lines in the closing summary:
/// busiest first, capped at `SUMMARY_MAX_KINDS` kinds.
fn append_kind_lines(lines: &mut Vec<String>, title: &str, kinds: &[ProblemKind]) {
    if kinds.is_empty() {
        return;
    }
    lines.push("#".to_string());
    lines.push(format!("# {title}:"));
    let mut ordered: Vec<&ProblemKind> = kinds.iter().collect();
    ordered.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.message.cmp(&b.message))
    });
    for kind in ordered.iter().take(SUMMARY_MAX_KINDS) {
        lines.push(format!("#   {}", describe_kind(kind)));
    }
    if ordered.len() > SUMMARY_MAX_KINDS {
        lines.push(format!(
            "#   ...and {} other kinds; see above.",
            ordered.len() - SUMMARY_MAX_KINDS
        ));
    }
}

#[allow(clippy::cast_possible_truncation)] // epoch seconds for a display stamp
fn stamp_of(epoch_seconds: f64) -> String {
    Local
        .timestamp_opt(epoch_seconds as i64, 0)
        .single()
        .map(|when| when.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// The lines appended to a recording log once ffmpeg has exited
/// (Python `format_recording_summary`): how long the file should
/// be, how long it is, and which problems were logged, grouped by
/// kind and placed in time.
#[must_use]
#[allow(clippy::too_many_arguments)] // the summary's inputs, mirroring the Python signature
#[allow(clippy::cast_precision_loss)] // a file size in a display line, as Python's float
pub fn format_recording_summary(
    out_path: &str,
    started_at: f64,
    ended_at: f64,
    planned_seconds: Option<f64>,
    written_seconds: Option<f64>,
    file_size: Option<u64>,
    ending: &str,
    summary: &LogSummary,
) -> Vec<String> {
    let mut lines = vec![
        SUMMARY_MARKER.to_string(),
        format!("# File: {out_path}"),
        format!("# Started: {}", stamp_of(started_at)),
        format!(
            "# Ended: {} (ran for {})",
            stamp_of(ended_at),
            clock_hms(ended_at - started_at)
        ),
        format!("# How it ended: {ending}"),
    ];
    match planned_seconds.filter(|v| *v > 0.0) {
        Some(planned) => lines.push(format!("# Planned length: {}", clock_hms(planned))),
        None => lines.push("# Planned length: none, recorded until stopped".to_string()),
    }
    match written_seconds.filter(|v| *v > 0.0) {
        Some(written) => {
            let mut recorded = format!("# Recorded length: {}", clock_hms(written));
            if let Some(planned) = planned_seconds.filter(|v| *v > 0.0) {
                let gap = planned - written;
                let _ = write!(recorded, " ({:.1}% of planned", 100.0 * written / planned);
                if gap.abs() >= 1.0 {
                    let _ = write!(
                        recorded,
                        ", {} {}",
                        clock_hms(gap.abs()),
                        if gap > 0.0 { "short" } else { "over" }
                    );
                }
                recorded += ")";
            }
            lines.push(recorded);
        }
        None => lines.push("# Recorded length: unknown (ffmpeg did not report it)".to_string()),
    }
    lines.push(format!(
        "# File size: {}",
        if file_size.is_some() {
            format_size(file_size.map(|v| v as f64))
        } else {
            "no file was written".to_string()
        }
    ));
    lines.push(format!(
        "# Problems while opening the stream: {}",
        if summary.opening.is_empty() {
            "none".to_string()
        } else {
            problem_totals(&summary.opening)
        }
    ));
    if summary.output_opened {
        lines.push(format!(
            "# Problems during the recording: {}",
            if summary.recording.is_empty() {
                "none".to_string()
            } else {
                problem_totals(&summary.recording)
            }
        ));
    } else {
        lines.push(
            "# The recording never started writing: ffmpeg did not open the output file."
                .to_string(),
        );
    }
    append_kind_lines(&mut lines, "While opening the stream", &summary.opening);
    append_kind_lines(&mut lines, "During the recording", &summary.recording);
    if !summary.opening.is_empty() && summary.output_opened {
        lines.push("#".to_string());
        lines.push(
            "# Problems while opening the stream happen before anything is written:".to_string(),
        );
        lines.push(
            "# ffmpeg joins a live stream part-way through and reads its start to".to_string(),
        );
        lines.push("# identify it. They are usually harmless.".to_string());
    }
    if !summary.recording.is_empty() {
        lines.push("#".to_string());
        if summary.timestamps {
            lines.push("# Times are clock time since the recording started writing.".to_string());
        } else {
            lines.push("# No times: this ffmpeg cannot time-stamp its log.".to_string());
        }
    }
    lines.push("# ===== End of summary =====".to_string());
    lines
}

/// A log's text and the wall clock at its `Output #` line
/// (Python `_read_log_and_opened_at`).
fn read_log_and_opened_at(path: &Path) -> (String, Option<NaiveDateTime>) {
    let Ok(data) = std::fs::read(path) else {
        return (String::new(), None);
    };
    let text = String::from_utf8_lossy(&data).into_owned();
    for line in text.split(['\n', '\r']) {
        if let Some(parsed) = parse_log_line(line) {
            if parsed.level == "info" && parsed.message.starts_with("Output #") {
                if parsed.when.is_some() {
                    return (text.clone(), parsed.when);
                }
                break;
            }
        }
    }
    (text, None)
}

/// `media_at_offset` over an already-read log (Python
/// `media_at_offset_from`): the newest `time=` stats value logged
/// before `when_seconds` into the recording, as h:mm:ss — the
/// position a player counts the file from.
fn media_at_offset_from(
    data: &str,
    opened_at: Option<NaiveDateTime>,
    when_seconds: f64,
    window_seconds: f64,
) -> String {
    let Some(opened) = opened_at else {
        return String::new();
    };
    let mut best = String::new();
    for line in data.split(['\n', '\r']) {
        let Some(parsed) = parse_log_line(line) else {
            continue;
        };
        let Some(when) = parsed.when else {
            continue;
        };
        let offset = when
            .signed_duration_since(opened)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64())
            .max(0.0);
        if offset > when_seconds + window_seconds {
            break;
        }
        if offset <= when_seconds {
            if let Some(value) = find_time_value(&parsed.message) {
                best = clock_hms(value);
            }
        }
    }
    best
}

/// One kind of problem placed in time, for the Recording Problem
/// Times dialog (Python `recording_problem_times`' entries).
#[derive(Clone, Debug, PartialEq)]
pub struct ProblemTimes {
    pub message: String,
    pub level: String,
    pub count: u64,
    /// h:mm:ss offsets from the moment ffmpeg opened the output.
    pub clock_times: Vec<String>,
    /// The newest `time=` before each occurrence, where known.
    pub media_times: Vec<String>,
    /// Media times where known, clock times otherwise, deduped:
    /// the times the dialog lists.
    pub jump_times: Vec<String>,
}

/// When problems were logged, in seconds into the recording
/// (Python `recording_problem_times`). Empty when the log holds
/// nothing timed: without ffmpeg's time stamps nothing can be
/// placed.
#[must_use]
pub fn recording_problem_times(path: &Path, max_times: usize) -> Vec<ProblemTimes> {
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let summary = summarize_log(path);
    if !summary.timestamps {
        return Vec::new();
    }
    let (data, opened_at) = read_log_and_opened_at(path);
    if opened_at.is_none() {
        return Vec::new();
    }
    let mut results: Vec<ProblemTimes> = Vec::new();
    for kind in &summary.recording {
        if kind.offsets.is_empty() {
            continue;
        }
        let placed = &kind.offsets[..kind.offsets.len().min(max_times)];
        let stamps: Vec<String> = placed.iter().map(|v| clock_hms(*v)).collect();
        let media: Vec<String> = placed
            .iter()
            .map(|v| media_at_offset_from(&data, opened_at, *v, 30.0))
            .collect();
        let mut jump_times: Vec<String> = Vec::new();
        for (media_time, clock_time) in media.iter().zip(stamps.iter()) {
            let chosen = if media_time.is_empty() {
                clock_time.clone()
            } else {
                media_time.clone()
            };
            if !jump_times.contains(&chosen) {
                jump_times.push(chosen);
            }
        }
        results.push(ProblemTimes {
            message: kind.message.clone(),
            level: kind.level.clone(),
            count: kind.count,
            clock_times: stamps,
            media_times: media.into_iter().filter(|s| !s.is_empty()).collect(),
            jump_times,
        });
    }
    results.sort_by(|a, b| a.clock_times.first().cmp(&b.clock_times.first()));
    results
}

fn datetime_support_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether this ffmpeg accepts `-loglevel level+datetime+info`
/// (Python `ffmpeg_supports_log_datetime`). The flag arrived in
/// ffmpeg 7; an older system ffmpeg refuses to start at all when
/// given it, so ask once and remember the answer. A check that
/// could not run at all is not remembered.
#[must_use]
pub fn ffmpeg_supports_log_datetime(ffmpeg_path: &str) -> bool {
    if ffmpeg_path.is_empty() {
        return false;
    }
    if let Some(cached) = datetime_support_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(ffmpeg_path)
    {
        return *cached;
    }
    let spawned = Command::new(ffmpeg_path)
        .args([
            "-hide_banner",
            "-loglevel",
            "level+datetime+info",
            "-version",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    let supported = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    };
    datetime_support_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(ffmpeg_path.to_string(), supported);
    supported
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_log(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iptv-recording-log-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recording.log");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    const SAMPLE: &str = "\
# 2026-11-04 10:00:00
# ffmpeg -i http://example.com/stream

2026-11-04 10:00:01.000 [mpegts @ 0x55a0] [warning] first frame is no keyframe
2026-11-04 10:00:02.000 [info] Output #0, matroska, to 'out.mkv':
2026-11-04 10:00:03.500 [h264 @ 0x55b0] [error] decode_slice_header error
2026-11-04 10:00:04.500 [h264 @ 0x55b0] [error] decode_slice_header error
Last message repeated 2 times
2026-11-04 10:00:05.000 [info] frame=  100 fps= 25 time=00:00:04.00 bitrate=1000.0kbits/s
2026-11-04 10:01:03.500 [h264 @ 0x55b0] [error] decode_slice_header error
";

    #[test]
    fn parses_stamped_component_lines() {
        let parsed =
            parse_log_line("2026-11-04 10:00:03.500 [h264 @ 0x55b0] [error] decode error 12")
                .unwrap();
        assert_eq!(parsed.level, "error");
        assert_eq!(parsed.component, "h264");
        assert_eq!(parsed.message, "decode error 12");
        assert!(parsed.when.is_some());
        let plain = parse_log_line("[warning] no stamp here").unwrap();
        assert_eq!(plain.level, "warning");
        assert_eq!(plain.component, "");
        assert!(plain.when.is_none());
        assert!(parse_log_line("frame=  100 fps= 25").is_none());
        assert!(parse_log_line("[q] command received. Exiting.").is_none());
    }

    #[test]
    fn strips_timestamps() {
        assert_eq!(
            strip_log_timestamp("2026-11-04 10:00:03.500 [error] boom"),
            "[error] boom"
        );
        assert_eq!(strip_log_timestamp("[error] boom"), "[error] boom");
    }

    #[test]
    fn log_path_uses_logs_subfolder_and_stem() {
        let path = recording_log_path(Path::new("/rec"), Path::new("/rec/Show - 2026.mkv"));
        assert_eq!(path, PathBuf::from("/rec/logs/Show - 2026.log"));
    }

    #[test]
    fn summarizes_phases_counts_and_repeats() {
        let path = temp_log("summary", SAMPLE);
        let summary = summarize_log(&path);
        assert!(summary.output_opened);
        assert!(summary.timestamps);
        assert_eq!(summary.opening.len(), 1);
        assert_eq!(summary.opening[0].level, "warning");
        assert_eq!(summary.recording.len(), 1);
        let kind = &summary.recording[0];
        // 3 direct occurrences + 2 folded repeats.
        assert_eq!(kind.count, 5);
        assert_eq!(kind.offsets.len(), 5);
        assert!((kind.offsets[0] - 1.5).abs() < 1e-9);
        assert_eq!(count_log_problems(&path), (1, 5, 0));
    }

    #[test]
    fn reads_problem_tail_and_progress() {
        let path = temp_log("tail", SAMPLE);
        let problems = read_log_problems(&path, 12);
        assert_eq!(problems.len(), 4);
        assert!(problems[0].contains("first frame is no keyframe"));
        assert_eq!(parse_ffmpeg_progress(&path), Some(4.0));
    }

    #[test]
    fn problem_times_place_kinds() {
        let path = temp_log("times", SAMPLE);
        let times = recording_problem_times(&path, 12);
        assert_eq!(times.len(), 1);
        assert_eq!(times[0].count, 5);
        // Offsets 1.5 (x3 via repeats), 2.5, 61.5 seconds in.
        assert_eq!(times[0].clock_times[0], "0:00:02");
        assert_eq!(times[0].clock_times.last().unwrap(), "0:01:02");
        // The one stats line (time=4s at 3s in) is the media
        // position for everything after it, so jump times end
        // there, exactly as Python's media-first choice reads.
        assert_eq!(times[0].jump_times, vec!["0:00:02", "0:00:04"]);
    }

    #[test]
    fn problem_times_empty_without_timestamps() {
        let path = temp_log(
            "nostamps",
            "[error] one\n[info] Output #0, matroska, to 'x':\n[error] two\n",
        );
        assert!(recording_problem_times(&path, 12).is_empty());
    }

    #[test]
    fn summary_lines_cover_the_outcome() {
        let path = temp_log("format", SAMPLE);
        let summary = summarize_log(&path);
        let lines = format_recording_summary(
            "/rec/out.mkv",
            1_000.0,
            1_060.0,
            Some(120.0),
            Some(4.0),
            Some(2048),
            "finished normally.",
            &summary,
        );
        let text = lines.join("\n");
        assert!(text.contains("# ===== Recording summary ====="));
        assert!(text.contains("# Planned length: 0:02:00"));
        assert!(text.contains("# Recorded length: 0:00:04 (3.3% of planned, 0:01:56 short)"));
        assert!(text.contains("# File size: 2.0 KB"));
        assert!(text
            .contains("# Problems while opening the stream: 1 warning, 0 errors, 0 fatal errors"));
        assert!(text.contains("5 times: error from h264: decode_slice_header error"));
    }

    #[test]
    fn clock_and_size_formatting() {
        assert_eq!(clock_hms(0.0), "0:00:00");
        assert_eq!(clock_hms(3661.4), "1:01:01");
        assert_eq!(format_size(None), "--");
        assert_eq!(format_size(Some(512.0)), "512 B");
        assert_eq!(format_size(Some(2048.0)), "2.0 KB");
    }
}

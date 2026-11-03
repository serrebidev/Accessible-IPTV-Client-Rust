//! Subtitle cue parsing, ported from Python's `subtitle_cues.py`:
//! SRT/WebVTT blocks and ASS/SSA dialogue events, spoken-text cleanup,
//! the file-size cap, and playback-time cue lookup.

use std::path::Path;

pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const SPEAKABLE_SUFFIXES: &[&str] = &[".srt", ".vtt", ".ass", ".ssa"];

/// One subtitle cue, times in milliseconds.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Cue {
    pub start: i64,
    pub end: i64,
    pub text: String,
}

fn ms(hours: &str, minutes: &str, seconds: &str, frac: &str) -> Option<i64> {
    if hours.len() > 19 {
        return None;
    }
    let m: i64 = minutes.parse().ok()?;
    let s: i64 = seconds.parse().ok()?;
    if m >= 60 || s >= 60 {
        return None;
    }
    let h: i64 = if hours.is_empty() {
        0
    } else {
        hours.parse().ok()?
    };
    let mut frac_text = frac.to_string();
    while frac_text.len() < 3 {
        frac_text.push('0');
    }
    let f: i64 = frac_text.parse().ok()?;
    let value = ((h * 60 + m) * 60 + s) * 1000 + f;
    Some(value)
}

/// Parse one `H:MM:SS.mmm` / `MM:SS.mmm` timestamp (comma or dot).
fn parse_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    let (main, frac) = {
        let dot = text.rfind([',', '.'])?;
        (&text[..dot], &text[dot + 1..])
    };
    if frac.is_empty() || frac.len() > 3 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = main.split(':').collect();
    match parts.as_slice() {
        [h, m, s] => {
            if m.len() > 2 || s.len() != 2 {
                return None;
            }
            ms(h, m, s, frac)
        }
        [m, s] => {
            if m.len() > 2 || s.len() != 2 {
                return None;
            }
            ms("", m, s, frac)
        }
        _ => None,
    }
}

/// Parse a full timing line `start --> end [settings]`.
fn parse_timing_line(line: &str) -> Option<(i64, i64)> {
    let (left, right) = line.trim().split_once("-->")?;
    let start = parse_timestamp(left)?;
    let end_token = right.trim().split([' ', '\t']).next().unwrap_or("");
    let end = parse_timestamp(end_token)?;
    Some((start, end))
}

/// Strip closed inline tags, preserving unclosed fragments as literal
/// text (port of `_strip_markup`).
fn strip_markup(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut offset = 0;
    let mut scan = 0;
    let mut exhausted: Vec<char> = Vec::new();
    while scan < bytes.len() {
        // Find the next '<' or '{\'.
        let mut found: Option<(usize, char)> = None;
        let mut i = scan;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                found = Some((i, '>'));
                break;
            }
            if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                found = Some((i, '}'));
                break;
            }
            i += 1;
        }
        let Some((start, closer)) = found else {
            break;
        };
        scan = if closer == '>' { start + 1 } else { start + 2 };
        if exhausted.contains(&closer) {
            continue;
        }
        let rest = &text[scan..];
        match rest.find(closer) {
            Some(end) => {
                parts.push(text[offset..start].to_string());
                offset = scan + end + 1;
                scan = offset;
            }
            None => {
                exhausted.push(closer);
            }
        }
    }
    parts.push(text[offset..].to_string());
    parts.concat()
}

/// The common HTML entities plus numeric references (the spoken-text
/// subset of `html.unescape`).
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let replacement: Option<String> = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            "nbsp" => Some("\u{a0}".to_string()),
            _ => {
                if let Some(num) = entity.strip_prefix('#') {
                    let code = if let Some(hex) =
                        num.strip_prefix('x').or_else(|| num.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32).map(|c| c.to_string())
                } else {
                    None
                }
            }
        };
        if let Some(text) = replacement {
            out.push_str(&text);
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn ass_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    let (main, frac) = text.rsplit_once('.')?;
    if frac.is_empty() || frac.len() > 3 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() != 3 || parts[1].len() != 2 || parts[2].len() != 2 {
        return None;
    }
    ms(parts[0], parts[1], parts[2], frac)
}

/// Whether an ASS override body switches drawing mode on.
fn drawing_mode_in(body: &str) -> Option<bool> {
    let bytes = body.as_bytes();
    let mut mode: Option<bool> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' && bytes[i + 1] == b'p' {
            let mut j = i + 2;
            let digits_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start
                && (j >= bytes.len() || bytes[j] == b'\\' || bytes[j].is_ascii_whitespace())
            {
                let digits = &body[digits_start..j];
                mode = Some(!digits.trim_start_matches('0').is_empty());
                i = j;
                continue;
            }
        }
        i += 1;
    }
    mode
}

/// ASS spoken text (port of `_ass_plain_text`): overrides and drawing
/// dropped, escapes become spaces.
fn ass_plain_text(text: &str) -> String {
    let mut drawing = false;
    let mut parts: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let opening = text[offset..].find('{').map(|i| offset + i);
        let body_end = opening.unwrap_or(text.len());
        if !drawing {
            parts.push(
                text[offset..body_end]
                    .replace('}', "")
                    .replace("\\N", " ")
                    .replace("\\n", " ")
                    .replace("\\h", " "),
            );
        }
        let Some(opening) = opening else {
            break;
        };
        let Some(closing) = text[opening + 1..].find('}').map(|i| opening + 1 + i) else {
            break;
        };
        if let Some(mode) = drawing_mode_in(&text[opening + 1..closing]) {
            drawing = mode;
        }
        offset = closing + 1;
    }
    parts
        .concat()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read ASS/SSA Dialogue events using the declared event format (port
/// of `parse_ass`).
#[must_use]
pub fn parse_ass(text: &str) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut in_events = false;
    let mut fields: Option<Vec<String>> = None;
    for raw_line in text.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_events = line.to_lowercase() == "[events]";
            fields = None;
            continue;
        }
        if !in_events {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_lowercase();
        if name == "format" {
            let declared: Vec<String> = value.split(',').map(|f| f.trim().to_lowercase()).collect();
            let unique = {
                let mut seen: Vec<&String> = Vec::new();
                let mut ok = true;
                for f in &declared {
                    if seen.contains(&f) {
                        ok = false;
                        break;
                    }
                    seen.push(f);
                }
                ok
            };
            fields = if !declared.is_empty()
                && declared.last().map(String::as_str) == Some("text")
                && unique
                && declared.iter().any(|f| f == "start")
                && declared.iter().any(|f| f == "end")
            {
                Some(declared)
            } else {
                None
            };
        } else if name == "dialogue" {
            let Some(fields) = &fields else {
                continue;
            };
            let parts: Vec<&str> = value.trim_start().splitn(fields.len(), ',').collect();
            if parts.len() != fields.len() {
                continue;
            }
            let get = |key: &str| -> &str {
                fields
                    .iter()
                    .position(|f| f == key)
                    .map_or("", |i| parts[i])
            };
            let start = ass_timestamp(get("start"));
            let end = ass_timestamp(get("end"));
            let body = ass_plain_text(get("text"));
            if let (Some(start), Some(end)) = (start, end) {
                if end > start && !body.is_empty() {
                    cues.push(Cue {
                        start,
                        end,
                        text: body,
                    });
                }
            }
        }
    }
    cues.sort();
    cues
}

/// Parse subtitle text into sorted cues (port of `parse`): ASS when the
/// hint or the content says so, otherwise SRT/WebVTT blocks.
#[must_use]
pub fn parse_cues(text: &str, format_hint: &str) -> Vec<Cue> {
    let hint = format_hint.to_lowercase();
    let hint = hint.trim_start_matches('.');
    if matches!(hint, "ass" | "ssa") {
        return parse_ass(text);
    }
    if format_hint.is_empty() {
        let head = text.trim_start_matches(['\u{feff}', ' ', '\t', '\n']);
        let lower = head.to_lowercase();
        if lower.starts_with("[script info]") || lower.starts_with("[events]") {
            return parse_ass(text);
        }
    }
    let normalized = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let is_vtt = hint == "vtt" || normalized.starts_with("WEBVTT");
    let mut cues: Vec<Cue> = Vec::new();
    for block in normalized.split("\n\n") {
        let block = block.trim_matches('\n');
        if block.is_empty() {
            continue;
        }
        let lines: Vec<&str> = block.split('\n').collect();
        if is_vtt {
            let first = lines[0];
            let metadata = first == "WEBVTT"
                || first.starts_with("WEBVTT ")
                || first.starts_with("WEBVTT\t")
                || first == "NOTE"
                || first.starts_with("NOTE ")
                || first.starts_with("NOTE\t")
                || first.trim_end_matches([' ', '\t']) == "STYLE"
                || first.trim_end_matches([' ', '\t']) == "REGION";
            if metadata {
                continue;
            }
        }
        for i in 0..lines.len().min(2) {
            let Some((start, end)) = parse_timing_line(lines[i]) else {
                continue;
            };
            let body = lines[i + 1..]
                .iter()
                .map(|part| unescape(&strip_markup(part)).trim().to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
            if !body.is_empty() && end > start {
                cues.push(Cue {
                    start,
                    end,
                    text: body,
                });
            }
            break;
        }
    }
    cues.sort();
    cues
}

/// Decode raw subtitle bytes honoring BOMs (UTF-32/16 before UTF-8).
fn decode_bytes(raw: &[u8]) -> String {
    if raw.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || raw.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        let little = raw[0] == 0xFF;
        let units: Vec<u32> = raw[4..]
            .chunks_exact(4)
            .map(|c| {
                if little {
                    u32::from_le_bytes([c[0], c[1], c[2], c[3]])
                } else {
                    u32::from_be_bytes([c[0], c[1], c[2], c[3]])
                }
            })
            .collect();
        return units
            .iter()
            .map(|u| char::from_u32(*u).unwrap_or('\u{fffd}'))
            .collect();
    }
    if raw.starts_with(&[0xFF, 0xFE]) || raw.starts_with(&[0xFE, 0xFF]) {
        let little = raw[0] == 0xFF;
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| {
                if little {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{fffd}'))
            .collect();
    }
    String::from_utf8_lossy(raw).to_string()
}

/// Parse a subtitle file (port of `load`): the size cap is enforced
/// while reading, and decoding follows the BOM.
///
/// # Errors
///
/// Returns the OS error for unreadable files and the reference's
/// message for oversized ones.
pub fn load_cues(path: &Path) -> Result<Vec<Cue>, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    if raw.len() > MAX_FILE_BYTES {
        return Err("subtitle file too large".to_string());
    }
    let text = decode_bytes(&raw);
    let hint = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    Ok(parse_cues(&text, &hint))
}

/// Index of the latest cue that has started by `ms` (port of
/// `started`).
#[must_use]
pub fn started(cues: &[Cue], ms: i64) -> Option<usize> {
    let count = cues.partition_point(|cue| cue.start <= ms);
    count.checked_sub(1)
}

/// Index of the cue showing at `ms`; the latest-starting one wins on
/// overlap (port of `active`).
#[must_use]
pub fn active(cues: &[Cue], ms: i64) -> Option<usize> {
    let mut i = started(cues, ms)?;
    loop {
        if cues[i].start <= ms && ms < cues[i].end {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// The subtitle review cursor (P11, Python
/// `_subtitle_review_text`): review traverses only cues that
/// have actually been shown — never the file's unplayed tail —
/// and the last shown cue stays readable through gaps.
#[derive(Clone, Debug, Default)]
pub struct ReviewCursor {
    cursor: Option<usize>,
}

impl ReviewCursor {
    /// New source, stop or track change: review starts empty.
    pub fn reset(&mut self) {
        self.cursor = None;
    }

    /// Per-sample maintenance: a newly active cue becomes the
    /// cursor; the cursor never passes the latest shown cue
    /// (a rewind pulls it back), and nothing shown means no
    /// cursor.
    pub fn on_sample(
        &mut self,
        active: Option<usize>,
        last_shown: Option<usize>,
        active_changed: bool,
    ) {
        if active_changed {
            if let Some(index) = active {
                self.cursor = Some(index);
            }
        }
        match last_shown {
            None => self.cursor = None,
            Some(last) => {
                if self.cursor.is_some_and(|cursor| cursor > last) {
                    self.cursor = Some(last);
                }
            }
        }
    }

    /// Speakable review text for one step: 0 = current (or the
    /// last shown cue, named as such, during a gap), -1 =
    /// previous, +1 = next. The cursor moves only when a cue
    /// is actually read.
    pub fn review(
        &mut self,
        cues: &[Cue],
        active: Option<usize>,
        last_shown: Option<usize>,
        step: i32,
    ) -> String {
        if cues.is_empty() {
            return "No subtitle text to read. Load a supported subtitle file.".to_string();
        }
        let Some(last) = last_shown else {
            return "No subtitle has been shown yet.".to_string();
        };
        if step == 0 {
            if let Some(index) = active {
                self.cursor = Some(index);
                return cues[index].text.clone();
            }
            self.cursor = Some(last);
            return format!("Last subtitle: {}", cues[last].text);
        }
        let base = self.cursor.unwrap_or(last);
        let target = i64::try_from(base).unwrap_or(0) + i64::from(step);
        if target < 0 {
            return "No earlier subtitle.".to_string();
        }
        let target = usize::try_from(target).unwrap_or(usize::MAX);
        if target > last {
            return "No later subtitle.".to_string();
        }
        self.cursor = Some(target);
        cues[target].text.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_cues() -> Vec<Cue> {
        vec![
            Cue {
                start: 0,
                end: 1_000,
                text: "one".to_string(),
            },
            Cue {
                start: 2_000,
                end: 3_000,
                text: "two".to_string(),
            },
            Cue {
                start: 4_000,
                end: 5_000,
                text: "three".to_string(),
            },
        ]
    }

    #[test]
    fn review_cursor_walks_shown_history_only() {
        let cues = three_cues();
        let mut review = ReviewCursor::default();
        // Nothing shown yet, whatever the step.
        assert_eq!(
            review.review(&cues, None, None, 0),
            "No subtitle has been shown yet."
        );
        assert_eq!(
            review.review(&cues, None, None, -1),
            "No subtitle has been shown yet."
        );
        // No cues at all.
        assert_eq!(
            review.review(&[], None, None, 0),
            "No subtitle text to read. Load a supported subtitle file."
        );
        // Cues 0 and 1 have been shown; cue 1 is active.
        review.on_sample(Some(1), Some(1), true);
        // Next must stop at the latest shown cue, never read
        // the file's unplayed tail.
        assert_eq!(
            review.review(&cues, Some(1), Some(1), 1),
            "No later subtitle."
        );
        // Previous walks back one cue per press, to cue 0.
        assert_eq!(review.review(&cues, Some(1), Some(1), -1), "one");
        assert_eq!(
            review.review(&cues, Some(1), Some(1), -1),
            "No earlier subtitle."
        );
        // And forward again over shown history.
        assert_eq!(review.review(&cues, Some(1), Some(1), 1), "two");
    }

    #[test]
    fn review_cursor_retains_the_last_shown_cue_through_gaps() {
        let cues = three_cues();
        let mut review = ReviewCursor::default();
        review.on_sample(Some(0), Some(0), true);
        // A gap: no active cue, cue 0 remains the last shown.
        review.on_sample(None, Some(0), true);
        assert_eq!(review.review(&cues, None, Some(0), 0), "Last subtitle: one");
        // Current with an active cue reads it bare and moves
        // the cursor to it.
        assert_eq!(review.review(&cues, Some(0), Some(0), 0), "one");
        // A rewind pulls the cursor back to the rewound-to
        // latest shown cue.
        review.on_sample(Some(1), Some(1), true);
        review.on_sample(Some(0), Some(0), false);
        assert_eq!(
            review.review(&cues, Some(0), Some(0), 1),
            "No later subtitle."
        );
        review.reset();
        assert_eq!(
            review.review(&cues, Some(0), Some(0), -1),
            "No earlier subtitle."
        );
    }

    #[test]
    fn srt_and_vtt_blocks_parse() {
        let srt = "1\n00:00:01,000 --> 00:00:03,500\nHello <i>world</i> &amp; friends\n\n2\n00:00:04.000 --> 00:00:05.000\nSecond line\n";
        let cues = parse_cues(srt, "srt");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1000);
        assert_eq!(cues[0].end, 3500);
        assert_eq!(cues[0].text, "Hello world & friends");
        let vtt = "WEBVTT\n\nNOTE a comment\n00:00:01.000 --> 00:00:02.000\n\n00:00:06.000 --> 00:00:07.000\nSpoken\n";
        let cues = parse_cues(vtt, "");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Spoken");
    }

    #[test]
    fn ass_dialogue_uses_declared_format() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Text\nDialogue: 0,0:00:01.00,0:00:03.50,{\\pos(1,2)}Hello, world\\Nagain\nComment: 0,0:00:01.00,0:00:02.00,not spoken\n";
        let cues = parse_cues(ass, "");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, 1000);
        assert_eq!(cues[0].text, "Hello, world again");
    }

    #[test]
    fn active_lookup_prefers_latest_start() {
        let cues = parse_cues(
            "00:00:01.000 --> 00:00:09.000\nLong\n\n00:00:03.000 --> 00:00:04.000\nShort\n",
            "srt",
        );
        assert_eq!(
            active(&cues, 3500).map(|i| cues[i].text.as_str()),
            Some("Short")
        );
        assert_eq!(
            active(&cues, 5000).map(|i| cues[i].text.as_str()),
            Some("Long")
        );
        assert_eq!(active(&cues, 500), None);
    }
}

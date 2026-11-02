//! Electronic programme guide: XMLTV import, guide indexes, the Python
//! channel-matching policy, now/next lookup and programme search.
//!
//! Ports `playlist.py`'s EPG import, `channel_identity.py` and the matching
//! vocabulary of `channel_names.py` (pinned Python revision in the parity
//! tool). Storage is in-memory; Python's `SQLite` guide database is the
//! recorded scale endgame for multi-million-programme guides.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;

use crate::Channel;

/// One guide channel: its XMLTV id and first display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpgChannel {
    pub id: String,
    pub display_name: String,
}

/// One programme; `start`/`stop` are Unix epoch seconds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Programme {
    pub channel_id: String,
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub description: String,
}

/// A parsed guide: channels in document order, programmes grouped by
/// channel id and sorted by start.
#[derive(Clone, Debug, Default)]
pub struct EpgGuide {
    pub channels: Vec<EpgChannel>,
    pub programmes: HashMap<String, Vec<Programme>>,
}

impl EpgGuide {
    #[must_use]
    pub fn programme_count(&self) -> usize {
        self.programmes.values().map(Vec::len).sum()
    }

    /// Merge another parsed guide in: first channel wins on id, programmes
    /// append and re-sort (Python imports every source into one database).
    pub fn merge(&mut self, other: EpgGuide) {
        let known: HashSet<String> = self.channels.iter().map(|c| c.id.clone()).collect();
        for channel in other.channels {
            if !known.contains(&channel.id) {
                self.channels.push(channel);
            }
        }
        for (id, mut programmes) in other.programmes {
            let entry = self.programmes.entry(id).or_default();
            entry.append(&mut programmes);
            entry.sort_by_key(|p| p.start);
        }
    }

    /// The programme on at `at` and the one after it, for a guide channel.
    #[must_use]
    pub fn now_next(&self, channel_id: &str, at: i64) -> (Option<Programme>, Option<Programme>) {
        let Some(list) = self.programmes.get(channel_id) else {
            return (None, None);
        };
        let idx = list.partition_point(|p| p.start <= at);
        let current = if idx > 0 {
            let candidate = &list[idx - 1];
            (candidate.stop > at).then(|| candidate.clone())
        } else {
            None
        };
        // The partition point is exactly the first programme that starts
        // after `at`, whether `current` is a live programme or a gap.
        (current, list.get(idx).cloned())
    }

    /// Case-insensitive substring search over titles and descriptions,
    /// chronological, capped at `limit` results.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<(Programme, String)> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let names: HashMap<&str, &str> = self
            .channels
            .iter()
            .map(|c| (c.id.as_str(), c.display_name.as_str()))
            .collect();
        let mut hits: Vec<(Programme, String)> = self
            .programmes
            .values()
            .flatten()
            .filter(|p| {
                p.title.to_lowercase().contains(&needle)
                    || p.description.to_lowercase().contains(&needle)
            })
            .map(|p| {
                (
                    p.clone(),
                    names
                        .get(p.channel_id.as_str())
                        .copied()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        hits.sort_by_key(|(p, _)| p.start);
        hits.truncate(limit);
        hits
    }
}

/// Parse an XMLTV timestamp (`20261101190000 +0000`; the offset and the
/// seconds are optional) into epoch seconds.
#[must_use]
pub fn parse_xmltv_time(value: &str) -> Option<i64> {
    let value = value.trim();
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let field = |from: usize, to: usize| -> i64 {
        digits
            .get(from..to)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    let naive = chrono::NaiveDate::from_ymd_opt(
        i32::try_from(field(0, 4)).ok()?,
        u32::try_from(field(4, 6)).ok()?,
        u32::try_from(field(6, 8)).ok()?,
    )?
    .and_hms_opt(
        u32::try_from(field(8, 10)).ok()?,
        u32::try_from(field(10, 12)).ok()?,
        u32::try_from(field(12, 14)).ok()?,
    )?;
    let mut epoch = naive.and_utc().timestamp();
    let rest = &value[digits.len()..];
    if let Some(off) = rest.trim().strip_prefix(['+', '-']) {
        let sign: i64 = if rest.trim().starts_with('-') { -1 } else { 1 };
        let off_digits: String = off.chars().take(4).collect();
        if off_digits.len() == 4 {
            let hours: i64 = off_digits[..2].parse().ok()?;
            let minutes: i64 = off_digits[2..].parse().ok()?;
            epoch -= sign * (hours * 3600 + minutes * 60);
        }
    }
    Some(epoch)
}

/// Local wall-clock `HH:MM` for an epoch second.
#[must_use]
pub fn local_hhmm(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0).map_or_else(
        || "??:??".to_string(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        },
    )
}

/// Decode guide bytes: gzip when the magic bytes say so, then UTF-8
/// (lossy, as Python's import tolerates damaged feeds).
#[must_use]
pub fn decode_epg_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut text = String::new();
        if decoder.read_to_string(&mut text).is_ok() {
            return text;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Stream-parse an XMLTV document into a guide.
#[must_use]
pub fn parse_xmltv(xml: &str) -> EpgGuide {
    use quick_xml::events::Event;
    let mut guide = EpgGuide::default();
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut channel: Option<(String, String)> = None;
    let mut programme: Option<Programme> = None;
    let mut capture: Option<&'static str> = None;
    let mut text = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "channel" => {
                        let id = attr(&e, b"id").unwrap_or_default();
                        channel = Some((id, String::new()));
                    }
                    "programme" => {
                        programme = Some(Programme {
                            channel_id: attr(&e, b"channel").unwrap_or_default(),
                            start: attr(&e, b"start")
                                .and_then(|v| parse_xmltv_time(&v))
                                .unwrap_or(0),
                            stop: attr(&e, b"stop")
                                .and_then(|v| parse_xmltv_time(&v))
                                .unwrap_or(0),
                            title: String::new(),
                            description: String::new(),
                        });
                    }
                    "display-name" if channel.is_some() => {
                        capture = Some("display-name");
                        text.clear();
                    }
                    "title" if programme.is_some() => {
                        capture = Some("title");
                        text.clear();
                    }
                    "desc" if programme.is_some() => {
                        capture = Some("desc");
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if capture.is_some() {
                    if let Ok(decoded) = t.unescape() {
                        text.push_str(&decoded);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                end_element(
                    &name,
                    &mut guide,
                    &mut channel,
                    &mut programme,
                    &mut capture,
                    &text,
                );
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    for list in guide.programmes.values_mut() {
        list.sort_by_key(|p| p.start);
    }
    guide
}

/// Apply one closing tag to the parse state (first display name / title /
/// description wins, as in the reference import).
fn end_element(
    name: &str,
    guide: &mut EpgGuide,
    channel: &mut Option<(String, String)>,
    programme: &mut Option<Programme>,
    capture: &mut Option<&'static str>,
    text: &str,
) {
    match name {
        "channel" => {
            if let Some((id, display)) = channel.take() {
                if !id.is_empty() {
                    guide.channels.push(EpgChannel {
                        id,
                        display_name: display,
                    });
                }
            }
        }
        "programme" => {
            if let Some(p) = programme.take() {
                if !p.channel_id.is_empty() && p.stop > p.start {
                    guide
                        .programmes
                        .entry(p.channel_id.clone())
                        .or_default()
                        .push(p);
                }
            }
        }
        "display-name" | "title" | "desc" => {
            if let Some(kind) = capture.take() {
                let value = text.trim().to_string();
                match kind {
                    "display-name" => {
                        if let Some((_, display)) = channel.as_mut() {
                            if display.is_empty() {
                                *display = value;
                            }
                        }
                    }
                    "title" => {
                        if let Some(p) = programme.as_mut() {
                            if p.title.is_empty() {
                                p.title = value;
                            }
                        }
                    }
                    _ => {
                        if let Some(p) = programme.as_mut() {
                            if p.description.is_empty() {
                                p.description = value;
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn attr(event: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| String::from_utf8(a.value.into_owned()).ok())
}

// ---------------------------------------------------------------------------
// Channel-name vocabulary (port of channel_names.py matching subset)
// ---------------------------------------------------------------------------

const STRIP_TAGS: &[&str] = &[
    "hd", "sd", "hevc", "fhd", "uhd", "4k", "8k", "hdr", "dash", "hq", "st", "us", "usa", "ca",
    "canada", "uk", "u.k.", "u.k", "uk.", "u.s.", "u.s", "us.", "au", "aus", "nz", "ukhd", "uksd",
    "h.265", "h265", "h.264", "h264", "50fps", "60fps", "eu",
];

const NOISE_WORDS: &[&str] = &[
    "backup", "alt", "feed", "main", "extra", "mirror", "test", "temp", "1", "2", "3", "4", "5",
    "6", "7", "8", "9", "0", "sd", "hd", "fhd", "uhd", "4k", "8k", "plus", "live", "network",
];

const WORD_NUMBERS: &[(&str, &str)] = &[
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
];

const IGNORED_NUMBERS: &[&str] = &[
    "240", "360", "480", "540", "576", "720", "1080", "1440", "2160", "4320", "50", "60",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Replace whole-word occurrences of `word` (case-insensitive, `\b`
/// semantics like the Python regexes) with `replacement`.
fn replace_word(text: &str, word: &str, replacement: &str) -> String {
    let lower = text.to_lowercase();
    let needle = word.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut rest = lower.as_str();
    let mut original = text;
    while let Some(pos) = rest.find(&needle) {
        let before_ok = rest[..pos].chars().last().is_none_or(|c| !is_word_char(c));
        let after_index = pos + needle.len();
        let after_ok = rest[after_index..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            out.push_str(&original[..pos]);
            out.push_str(replacement);
            rest = &rest[after_index..];
            original = &original[after_index..];
        } else {
            out.push_str(&original[..=pos]);
            rest = &rest[pos + 1..];
            original = &original[pos + 1..];
        }
    }
    out.push_str(original);
    out
}

/// Port of `canonicalize_name`: strip quality/country tags at the edges
/// until stable, remove them as words everywhere, drop empty brackets,
/// collapse whitespace.
#[must_use]
pub fn canonicalize_name(name: &str) -> String {
    let mut current = name.trim().to_lowercase();
    loop {
        let stripped = strip_edge_tag(&current);
        if stripped == current {
            break;
        }
        current = stripped;
    }
    for tag in STRIP_TAGS {
        current = replace_word(&current, tag, "");
    }
    current = current.replace("()", "").replace("[]", "");
    current.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_edge_tag(name: &str) -> String {
    let separators = [' ', '-', ':', '(', ')', '[', ']'];
    let mut s = name.to_string();
    // Leading tag: tag at start, then a separator run.
    for tag in STRIP_TAGS {
        if let Some(rest) = s.to_lowercase().strip_prefix(&tag.to_lowercase()) {
            let boundary = rest.chars().next().is_none_or(|c| !is_word_char(c));
            if boundary {
                let trimmed = rest.trim_start_matches(separators);
                s = trimmed.to_string();
                break;
            }
        }
    }
    // Trailing tag: separator run, then tag at end.
    for tag in STRIP_TAGS {
        if let Some(head) = s.to_lowercase().strip_suffix(&tag.to_lowercase()) {
            let boundary = head.chars().last().is_none_or(|c| !is_word_char(c));
            if boundary {
                let trimmed = head.trim_end_matches(separators);
                return trimmed.to_string();
            }
        }
    }
    s
}

/// Port of `strip_noise_words`.
#[must_use]
pub fn strip_noise_words(text: &str) -> String {
    let mut current = text.to_lowercase();
    for word in NOISE_WORDS {
        current = replace_word(&current, word, "");
    }
    current
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port of `replace_word_numbers`: "BBC Two" reads as "BBC 2".
#[must_use]
pub fn replace_word_numbers(text: &str) -> String {
    let mut current = text.to_lowercase();
    for (word, value) in WORD_NUMBERS {
        current = replace_word(&current, word, value);
    }
    current
}

/// Port of `significant_channel_numbers`.
#[must_use]
pub fn significant_channel_numbers(texts: &[&str]) -> BTreeSet<String> {
    let mut numbers = BTreeSet::new();
    for text in texts {
        let s = text.to_lowercase();
        if s.is_empty() {
            continue;
        }
        // Region+digits tokens (au1, uk2, ...) say nothing about the number.
        let mut cleaned = String::with_capacity(s.len());
        let words: Vec<&str> = s.split_whitespace().collect();
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                cleaned.push(' ');
            }
            let is_region_number = ["au", "ca", "ie", "uk", "gb", "us"].iter().any(|r| {
                word.strip_prefix(r).is_some_and(|rest| {
                    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
                })
            });
            if !is_region_number {
                cleaned.push_str(word);
            }
        }
        let mut run = String::new();
        for c in cleaned.chars().chain(std::iter::once(' ')) {
            if c.is_ascii_digit() {
                run.push(c);
            } else if !run.is_empty() {
                let normalized = run.trim_start_matches('0');
                let normalized = if normalized.is_empty() {
                    "0"
                } else {
                    normalized
                };
                if !IGNORED_NUMBERS.contains(&normalized) && normalized.len() <= 3 {
                    numbers.insert(normalized.to_string());
                }
                run.clear();
            }
        }
        for (word, value) in WORD_NUMBERS {
            if contains_word(&s, word) {
                numbers.insert((*value).to_string());
            }
        }
    }
    numbers
}

fn contains_word(text: &str, word: &str) -> bool {
    replace_word(text, word, "\u{1}") != text
}

/// Port of `numbers_disagree`: only two names that both carry a number can
/// disagree about it.
#[must_use]
pub fn numbers_disagree(first: &BTreeSet<String>, second: &BTreeSet<String>) -> bool {
    !first.is_empty() && !second.is_empty() && first.is_disjoint(second)
}

// ---------------------------------------------------------------------------
// Guide indexes and matching (port of channel_identity.py)
// ---------------------------------------------------------------------------

/// Index the guide's channels by id, by name and by noise-stripped name.
#[derive(Debug, Default)]
pub struct GuideIndexes {
    pub display_names: HashMap<String, String>,
    by_id_lower: HashMap<String, String>,
    name_index: HashMap<String, Vec<String>>,
    stripped_index: HashMap<String, Vec<String>>,
}

/// Add one id to a name index bucket, keeping guide order, no duplicates.
fn push_index(index: &mut HashMap<String, Vec<String>>, key: String, id: &str) {
    if !key.is_empty() {
        let bucket = index.entry(key).or_default();
        if !bucket.iter().any(|b| b == id) {
            bucket.push(id.to_string());
        }
    }
}

impl GuideIndexes {
    #[must_use]
    pub fn build(channels: &[EpgChannel]) -> Self {
        let mut indexes = Self {
            display_names: channels
                .iter()
                .map(|c| (c.id.clone(), c.display_name.clone()))
                .collect(),
            by_id_lower: channels
                .iter()
                .map(|c| (c.id.to_lowercase(), c.id.clone()))
                .collect(),
            name_index: HashMap::new(),
            stripped_index: HashMap::new(),
        };
        for channel in channels {
            let display = channel.display_name.as_str();
            let spelled = replace_word_numbers(display);
            push_index(
                &mut indexes.name_index,
                canonicalize_name(display),
                &channel.id,
            );
            push_index(
                &mut indexes.stripped_index,
                canonicalize_name(&strip_noise_words(display)),
                &channel.id,
            );
            push_index(
                &mut indexes.name_index,
                canonicalize_name(&spelled),
                &channel.id,
            );
            push_index(
                &mut indexes.stripped_index,
                canonicalize_name(&strip_noise_words(&spelled)),
                &channel.id,
            );
        }
        indexes
    }
}

/// Expand common XMLTV id variants (port of `expand_tvg_id_candidates`).
#[must_use]
pub fn expand_tvg_id_candidates(tvg_id: &str) -> Vec<String> {
    let raw = tvg_id.trim();
    let mut candidates: Vec<String> = Vec::new();
    let add = |value: &str, candidates: &mut Vec<String>| {
        let value = value.trim();
        if !value.is_empty() && !candidates.iter().any(|c| c.eq_ignore_ascii_case(value)) {
            candidates.push(value.to_string());
        }
    };
    if raw.is_empty() {
        return candidates;
    }
    add(raw, &mut candidates);
    let (base, suffix) = match raw.split_once('@') {
        Some((b, s)) => (b, Some(s)),
        None => (raw, None),
    };
    add_base_variants(base, &mut candidates);
    if let Some(suffix) = suffix {
        if !base.is_empty() {
            add(base, &mut candidates);
            add_base_variants(base, &mut candidates);
            let suffix_clean: String = suffix.chars().filter(|c| c.is_alphanumeric()).collect();
            if !suffix_clean.is_empty()
                && !["sd", "hd", "fhd", "uhd", "4k", "8k"]
                    .contains(&suffix_clean.to_lowercase().as_str())
            {
                if let Some((name, region)) = split_name_region(base) {
                    add(&format!("{name}{suffix_clean}.{region}"), &mut candidates);
                }
            }
        }
    }
    candidates
}

/// Split `Name.Region` the way Python's `^(.+)\.([A-Za-z]{2,3}\d*)$` does.
fn split_name_region(value: &str) -> Option<(&str, &str)> {
    let (name, region) = value.rsplit_once('.')?;
    if name.is_empty() || !(2..=6).contains(&region.len()) {
        return None;
    }
    let letters = region.chars().take_while(|c| c.is_alphabetic()).count();
    if !(2..=3).contains(&letters) {
        return None;
    }
    let tail: String = region.chars().skip(letters).collect();
    if !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if letters + tail.len() != region.len() {
        return None;
    }
    Some((name, region))
}

fn add_base_variants(value: &str, candidates: &mut Vec<String>) {
    let Some((name, region)) = split_name_region(value) else {
        return;
    };
    let lowered = name.to_lowercase();
    for quality in ["fhd", "uhd", "hd", "sd", "4k", "8k"] {
        if lowered.ends_with(quality) && name.len() > quality.len() {
            let trimmed = &name[..name.len() - quality.len()];
            let variant = format!("{trimmed}.{region}");
            if !candidates.iter().any(|c| c.eq_ignore_ascii_case(&variant)) {
                candidates.push(variant);
            }
            break;
        }
    }
    if lowered.ends_with("sport") {
        let variant = format!("{name}s.{region}");
        if !candidates.iter().any(|c| c.eq_ignore_ascii_case(&variant)) {
            candidates.push(variant);
        }
    }
}

/// The guide channel id for a playlist channel, or None (port of
/// `match_guide_channel`). `known_ids` holds every guide channel id in
/// lower case, including ones with nothing on air at the moment.
#[must_use]
pub fn match_guide_channel<S: std::hash::BuildHasher>(
    channel: &Channel,
    indexes: &GuideIndexes,
    known_ids: Option<&HashSet<String, S>>,
) -> Option<String> {
    let raw_id = channel.tvg_id.trim();
    let mut candidates = expand_tvg_id_candidates(raw_id);
    candidates.push(raw_id.to_string());
    if let Some((head, _)) = raw_id.rsplit_once('.') {
        candidates.push(head.to_string());
    }
    for candidate in &candidates {
        let key = candidate.trim().to_lowercase();
        if let Some(id) = indexes.by_id_lower.get(&key) {
            return Some(id.clone());
        }
    }
    if let Some(known) = known_ids {
        if candidates
            .iter()
            .any(|c| known.contains(&c.trim().to_lowercase()))
        {
            return None;
        }
    }
    let playlist_numbers =
        significant_channel_numbers(&[channel.tvg_name.as_str(), channel.name.as_str()]);
    for source in [&channel.name, &channel.tvg_name] {
        let text = source.trim();
        if text.is_empty() {
            continue;
        }
        let spelled = replace_word_numbers(text);
        for (key, index) in [
            (canonicalize_name(text), &indexes.name_index),
            (canonicalize_name(&spelled), &indexes.name_index),
            (
                canonicalize_name(&strip_noise_words(text)),
                &indexes.stripped_index,
            ),
            (
                canonicalize_name(&strip_noise_words(&spelled)),
                &indexes.stripped_index,
            ),
        ] {
            if let Some(bucket) = index.get(&key) {
                for id in bucket {
                    let display = indexes.display_names.get(id).map_or("", String::as_str);
                    let guide_numbers = significant_channel_numbers(&[display]);
                    if !numbers_disagree(&playlist_numbers, &guide_numbers) {
                        return Some(id.clone());
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Background guide loading
// ---------------------------------------------------------------------------

enum EpgJob {
    Fetch(Vec<String>),
}

enum EpgEvent {
    Loaded(Box<EpgGuide>),
    Failed(String),
}

/// Off-UI-thread guide loader mirroring the browse/search worker shape.
pub struct EpgWorker {
    jobs: std::sync::mpsc::Sender<EpgJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<EpgEvent>>,
}

impl Default for EpgWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl EpgWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<EpgJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<EpgEvent>();
        std::thread::spawn(move || {
            while let Ok(EpgJob::Fetch(sources)) = job_receiver.recv() {
                let mut merged = EpgGuide::default();
                let mut loaded = 0_usize;
                let mut first_error = String::new();
                for source in &sources {
                    match fetch_epg_source(source) {
                        Ok(guide) => {
                            merged.merge(guide);
                            loaded += 1;
                        }
                        Err(error) => {
                            if first_error.is_empty() {
                                first_error = error;
                            }
                        }
                    }
                }
                let event = if loaded > 0 {
                    EpgEvent::Loaded(Box::new(merged))
                } else {
                    EpgEvent::Failed(first_error)
                };
                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            events: std::sync::Mutex::new(event_receiver),
        }
    }

    /// Fetch and parse the given guide sources (URLs or file paths).
    pub fn load(&self, sources: Vec<String>) {
        let _ = self.jobs.send(EpgJob::Fetch(sources));
    }

    /// The next finished load, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<EpgGuide, String>> {
        let events = self.events.lock().ok()?;
        match events.try_recv().ok()? {
            EpgEvent::Loaded(guide) => Some(Ok(*guide)),
            EpgEvent::Failed(error) => Some(Err(error)),
        }
    }
}

fn fetch_epg_source(source: &str) -> Result<EpgGuide, String> {
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        let mut response = ureq::get(source)
            .call()
            .map_err(|e| format!("{source}: {e}"))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|e| format!("{source}: {e}"))?
    } else {
        std::fs::read(source).map_err(|e| format!("{source}: {e}"))?
    };
    Ok(parse_xmltv(&decode_epg_bytes(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Channel;
    use std::collections::BTreeMap;

    fn channel(name: &str, tvg_id: &str, tvg_name: &str) -> Channel {
        Channel {
            name: name.to_string(),
            group: String::new(),
            url: "https://example.com/s".to_string(),
            tvg_id: tvg_id.to_string(),
            tvg_name: tvg_name.to_string(),
            tvg_logo: String::new(),
            unknown_attributes: BTreeMap::new(),
        }
    }

    const GUIDE: &str = r#"<?xml version="1.0"?>
<tv>
  <channel id="bbc2.uk"><display-name>BBC Two</display-name></channel>
  <channel id="bbc3.uk"><display-name>BBC Three</display-name></channel>
  <programme channel="bbc2.uk" start="20261101180000 +0000" stop="20261101190000 +0000">
    <title>News Hour</title><desc>Evening news</desc>
  </programme>
  <programme channel="bbc2.uk" start="20261101190000 +0000" stop="20261101200000 +0000">
    <title>Quiz</title><desc></desc>
  </programme>
</tv>"#;

    #[test]
    fn xmltv_parses_channels_programmes_and_times() {
        let guide = parse_xmltv(GUIDE);
        assert_eq!(guide.channels.len(), 2);
        assert_eq!(guide.programme_count(), 2);
        let first = &guide.programmes["bbc2.uk"][0];
        assert_eq!(first.title, "News Hour");
        assert_eq!(
            first.start,
            parse_xmltv_time("20261101180000 +0000").expect("time")
        );
        let (now, next) = guide.now_next("bbc2.uk", first.start + 60);
        assert_eq!(now.expect("now").title, "News Hour");
        assert_eq!(next.expect("next").title, "Quiz");
        let hits = guide.search("quiz", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "BBC Two");
    }

    #[test]
    fn xmltv_time_offsets() {
        let utc = parse_xmltv_time("20261101180000 +0000").expect("utc");
        let plus2 = parse_xmltv_time("20261101200000 +0200").expect("plus2");
        assert_eq!(utc, plus2);
        let no_offset = parse_xmltv_time("20261101180000").expect("naive");
        assert_eq!(utc, no_offset);
    }

    #[test]
    fn number_guard_keeps_bbc2_off_bbc3() {
        let guide = parse_xmltv(GUIDE);
        let indexes = GuideIndexes::build(&guide.channels);
        // Playlist says "BBC 2 HD": the name alone (number stripped by
        // noise rules) must not land on BBC Three.
        let hit = match_guide_channel(
            &channel("BBC 2 HD", "", ""),
            &indexes,
            None::<&HashSet<String>>,
        );
        assert_eq!(hit.as_deref(), Some("bbc2.uk"));
        let hit3 = match_guide_channel(
            &channel("BBC Three", "", ""),
            &indexes,
            None::<&HashSet<String>>,
        );
        assert_eq!(hit3.as_deref(), Some("bbc3.uk"));
    }

    #[test]
    fn tvg_id_variants_match() {
        let guide = parse_xmltv(GUIDE);
        let indexes = GuideIndexes::build(&guide.channels);
        let hit = match_guide_channel(
            &channel("Whatever", "bbc2.uk@City", ""),
            &indexes,
            None::<&HashSet<String>>,
        );
        assert_eq!(hit.as_deref(), Some("bbc2.uk"));
        let cands = expand_tvg_id_candidates("ESPNHD.us");
        assert!(cands.iter().any(|c| c == "ESPN.us"), "{cands:?}");
    }

    #[test]
    fn names_normalize_like_python() {
        assert_eq!(canonicalize_name("BBC Two HD"), "bbc two");
        assert_eq!(replace_word_numbers("BBC Two"), "bbc 2");
        assert_eq!(strip_noise_words("News Live HD"), "news");
        let nums = significant_channel_numbers(&["BBC 2", ""]);
        assert!(nums.contains("2"));
        assert!(!numbers_disagree(&nums, &nums));
    }
}

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

/// The programme on at one instant and the one after it, for one
/// guide channel (a bulk snapshot entry).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NowNext {
    pub current: Option<Programme>,
    pub next: Option<Programme>,
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

    /// Merge another parsed guide in, with Python's identity-based
    /// upsert (`playlist.py` `insert_channel` / `insert_programme`):
    /// a channel whose normalized name changed was renumbered, so its
    /// old programmes go; an incoming programme deletes the stored
    /// rows it overlaps, and an exact-slot match keeps the newer
    /// title and the longer description. Two sources describing the
    /// same slot therefore yield one row, never duplicates.
    pub fn merge(&mut self, other: EpgGuide) {
        for channel in other.channels {
            match self.channels.iter_mut().find(|c| c.id == channel.id) {
                Some(existing) => {
                    let old_norm = canonicalize_name(&strip_noise_words(&existing.display_name));
                    let new_norm = canonicalize_name(&strip_noise_words(&channel.display_name));
                    if !old_norm.is_empty() && !new_norm.is_empty() && old_norm != new_norm {
                        self.programmes.remove(&channel.id);
                    }
                    existing.display_name = channel.display_name;
                }
                None => self.channels.push(channel),
            }
        }
        for (id, programmes) in other.programmes {
            let entry = self.programmes.entry(id).or_default();
            for programme in programmes {
                upsert_programme(entry, programme);
            }
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

    /// Now/next for every guide channel in one pass (port of
    /// `get_all_now_next`'s single-scan design): each channel's
    /// sorted rows are walked once, so building the snapshot costs
    /// O(programmes) total instead of a lookup — and a guide-ID set
    /// rebuild — per playlist channel (audit E12).
    #[must_use]
    pub fn bulk_now_next(&self, at: i64) -> HashMap<String, NowNext> {
        // Python's `get_all_now_next` window (audit 0068): a
        // programme airing now cannot have started more than ~a
        // day ago, and "next" reaches six hours out — the store
        // twin applies the same bounds so both guide surfaces
        // answer identically.
        let floor = at - 24 * 3_600;
        let horizon = at + 6 * 3_600;
        let mut out = HashMap::with_capacity(self.programmes.len());
        for (id, list) in &self.programmes {
            let idx = list.partition_point(|p| p.start <= at);
            let current = if idx > 0 {
                let candidate = &list[idx - 1];
                (candidate.stop > at && candidate.start >= floor).then(|| candidate.clone())
            } else {
                None
            };
            let next = list
                .get(idx)
                .filter(|programme| programme.start <= horizon)
                .cloned();
            out.insert(id.clone(), NowNext { current, next });
        }
        out
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

/// Python's programme upsert against one channel's sorted rows:
/// stored rows overlapping the incoming slot (but not equal to it)
/// are removed; an exact-slot row takes the incoming title, and the
/// incoming description when the title changed or it is longer.
fn upsert_programme(rows: &mut Vec<Programme>, incoming: Programme) {
    rows.retain(|p| {
        !(p.start < incoming.stop && p.stop > incoming.start)
            || (p.start == incoming.start && p.stop == incoming.stop)
    });
    if let Some(existing) = rows
        .iter_mut()
        .find(|p| p.start == incoming.start && p.stop == incoming.stop)
    {
        if existing.title != incoming.title {
            existing.title = incoming.title;
            existing.description = incoming.description;
        } else if incoming.description.len() > existing.description.len() {
            existing.description = incoming.description;
        }
    } else {
        rows.push(incoming);
    }
}

/// Parse an XMLTV timestamp (`20261101190000 +0000`; the offset and the
/// seconds are optional) into epoch seconds.
#[must_use]
pub fn parse_xmltv_time(value: &str) -> Option<i64> {
    let value = value.trim();
    // ISO 8601 form ("2026-11-01T19:00:00Z", Python's 'T' branch).
    if value.contains('T') {
        let mut iso = value.replace(' ', "");
        if let Some(stripped) = iso.strip_suffix('Z') {
            iso = format!("{stripped}+00:00");
        } else if iso.len() > 5 {
            let tail = &iso[iso.len() - 5..];
            if (tail.starts_with('+') || tail.starts_with('-'))
                && tail[1..].bytes().all(|b| b.is_ascii_digit())
            {
                iso = format!("{}:{}", &iso[..iso.len() - 2], &iso[iso.len() - 2..]);
            }
        }
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&iso) {
            return Some(parsed.timestamp());
        }
        return None;
    }
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

/// SHA-1 hex digest, byte-for-byte Python's `hashlib.sha1`
/// (the guide source keys and the sources hash are shared with
/// the Python client's config and database, so the algorithm is
/// part of the format, not an implementation choice).
#[must_use]
pub fn sha1_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[index * 4],
                block[index * 4 + 1],
                block[index * 4 + 2],
                block[index * 4 + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut v0, mut v1, mut v2, mut v3, mut v4) =
            (state[0], state[1], state[2], state[3], state[4]);
        for (index, &word) in words.iter().enumerate() {
            let (mix, constant) = match index {
                0..=19 => ((v1 & v2) | (!v1 & v3), 0x5A82_7999),
                20..=39 => (v1 ^ v2 ^ v3, 0x6ED9_EBA1),
                40..=59 => ((v1 & v2) | (v1 & v3) | (v2 & v3), 0x8F1B_BCDC),
                _ => (v1 ^ v2 ^ v3, 0xCA62_C1D6),
            };
            let temp = v0
                .rotate_left(5)
                .wrapping_add(mix)
                .wrapping_add(v4)
                .wrapping_add(constant)
                .wrapping_add(word);
            v4 = v3;
            v3 = v2;
            v2 = v1.rotate_left(30);
            v1 = v0;
            v0 = temp;
        }
        state[0] = state[0].wrapping_add(v0);
        state[1] = state[1].wrapping_add(v1);
        state[2] = state[2].wrapping_add(v2);
        state[3] = state[3].wrapping_add(v3);
        state[4] = state[4].wrapping_add(v4);
    }
    state.iter().fold(String::new(), |mut out, word| {
        let _ = write!(out, "{word:08x}");
        out
    })
}

/// The store key of one guide source (Python keys every imported
/// channel by the SHA-1 of its source string).
#[must_use]
pub fn source_key(source: &str) -> String {
    sha1_hex(source.as_bytes())
}

/// Port of `reload_epg_sources`: the user's own `epgs` first, then
/// provider-discovered guides, then playlist-discovered guides
/// when `use_playlist_epg` allows; duplicates compare by stripped
/// text so a playlist naming the user's own guide never imports
/// it twice.
#[must_use]
pub fn merged_epg_sources(
    own: &[String],
    provider: &[String],
    playlist: &[String],
    use_playlist_epg: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |source: &String, out: &mut Vec<String>| {
        if seen.insert(source.trim().to_string()) {
            out.push(source.clone());
        }
    };
    for source in own {
        push(source, &mut out);
    }
    for source in provider {
        push(source, &mut out);
    }
    if use_playlist_epg {
        for source in playlist {
            push(source, &mut out);
        }
    }
    out
}

/// Port of `_hash_epg_sources`: SHA-1 over the sorted, stripped
/// source strings joined with `|`.
#[must_use]
pub fn hash_epg_sources(sources: &[String]) -> String {
    let mut normalized: Vec<&str> = sources.iter().map(|s| s.trim()).collect();
    normalized.sort_unstable();
    sha1_hex(normalized.join("|").as_bytes())
}

/// Port of `_get_epg_auto_interval_seconds` + `_should_auto_import_epg`:
/// whether an automatic (not user-forced) guide load should run.
/// A non-positive interval means always; a changed source list, an
/// unusable stored guide, or a missing/future last-import stamp
/// each force a load; otherwise the saved interval must elapse.
#[must_use]
pub fn should_auto_import_epg(
    sources: &[String],
    last_sources_hash: &str,
    last_import_epoch: i64,
    now: i64,
    interval_hours: f64,
    guide_usable: bool,
) -> bool {
    if interval_hours <= 0.0 {
        return true;
    }
    let current_hash = hash_epg_sources(sources);
    if !current_hash.is_empty() && current_hash != last_sources_hash {
        return true;
    }
    if !guide_usable {
        return true;
    }
    if last_import_epoch <= 0 || last_import_epoch > now + 300 {
        return true;
    }
    // The interval comparison is in float seconds, as in Python.
    #[allow(clippy::cast_precision_loss)]
    let elapsed = (now - last_import_epoch) as f64;
    elapsed >= interval_hours * 3600.0
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

/// Local wall-clock `YYYY-MM-DD HH:MM` for an epoch second
/// (Python's Last Watched column format).
#[must_use]
pub fn local_datetime(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0).map_or_else(
        || "Unknown time".to_string(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

/// Decode guide bytes: gzip when the magic bytes say so, then UTF-8
/// (lossy, as Python's import tolerates damaged text). A damaged
/// gzip is an error, never a fallback to reading the compressed
/// bytes as text: Python's importer fails such a source and keeps
/// the previously stored guide (audit E05).
///
/// # Errors
/// Returns a message when the gzip stream cannot be decoded.
pub fn decode_epg_bytes(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut text = String::new();
        decoder
            .read_to_string(&mut text)
            .map_err(|e| format!("damaged gzip guide: {e}"))?;
        return Ok(text);
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// The element's local name: namespace prefixes (`ns:tag`, or a
/// `{uri}tag` expansion) are stripped, as Python's importer does
/// with `tag.rsplit('}', 1)[-1]` and `{*}` lookups (audit E06).
fn local_name(raw: &str) -> String {
    match raw.rfind([':', '}']) {
        Some(index) => raw[index + 1..].to_string(),
        None => raw.to_string(),
    }
}

/// Port of `_parse_duration_to_seconds`: bare digits are minutes
/// (the XMLTV `<length>` default); otherwise an ISO 8601 duration
/// (`P1D`, `PT1H30M`, ...).
#[must_use]
pub fn parse_duration_seconds(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse::<i64>().ok().and_then(|m| m.checked_mul(60));
    }
    let upper = text.to_uppercase();
    let rest = upper.strip_prefix('P')?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (rest, None),
    };
    let mut total: i64 = 0;
    let mut number = String::new();
    for c in date_part.chars() {
        if c.is_ascii_digit() {
            number.push(c);
        } else if c == 'D' && !number.is_empty() {
            total = total.checked_add(number.parse::<i64>().ok()?.checked_mul(86_400)?)?;
            number.clear();
        } else {
            return None;
        }
    }
    if !number.is_empty() {
        return None;
    }
    if let Some(time) = time_part {
        for c in time.chars() {
            if c.is_ascii_digit() {
                number.push(c);
                continue;
            }
            let multiplier = match c {
                'H' => 3_600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
            if number.is_empty() {
                return None;
            }
            total = total.checked_add(number.parse::<i64>().ok()?.checked_mul(multiplier)?)?;
            number.clear();
        }
        if !number.is_empty() {
            return None;
        }
    }
    Some(total)
}

/// Port of the `<length>` half of `_calc_end_from_length_or_duration`:
/// a float count in the element's units (minutes by default).
#[must_use]
pub fn length_seconds(text: &str, units: &str) -> Option<i64> {
    let value: f64 = text.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let multiplier = match units.trim().to_lowercase().as_str() {
        "" | "minute" | "minutes" | "mins" | "min" => 60.0,
        "second" | "seconds" | "sec" | "secs" => 1.0,
        "hour" | "hours" | "hr" | "hrs" => 3_600.0,
        _ => return None,
    };
    // Python int() truncates toward zero.
    #[allow(clippy::cast_possible_truncation)]
    Some((value * multiplier) as i64)
}

/// A programme under construction: the row plus the raw material
/// (`end` attribute, `<length>`, `<duration>`) Python uses to
/// calculate a missing stop time.
struct PendingProgramme {
    row: Programme,
    end_attr: Option<i64>,
    length: Option<(String, String)>,
    duration: Option<String>,
}

impl PendingProgramme {
    /// Resolve the stop time: `stop`, else `end`, else start plus
    /// the `<length>`/`<duration>` span (Python's order).
    fn finish(mut self) -> Option<Programme> {
        if self.row.stop <= 0 {
            self.row.stop = self.end_attr.unwrap_or(0);
        }
        if self.row.stop <= 0 && self.row.start > 0 {
            let span = self
                .length
                .as_ref()
                .and_then(|(text, units)| length_seconds(text, units))
                .or_else(|| self.duration.as_deref().and_then(parse_duration_seconds));
            if let Some(span) = span {
                self.row.stop = self.row.start + span;
            }
        }
        (self.row.stop > self.row.start && !self.row.channel_id.is_empty()).then_some(self.row)
    }
}

/// Stream-parse an XMLTV document into a guide. Lenient: returns
/// whatever parsed before any error. Importers use
/// [`parse_xmltv_checked`] so a broken feed fails its source
/// instead of replacing a good guide with a partial one.
#[must_use]
pub fn parse_xmltv(xml: &str) -> EpgGuide {
    parse_xmltv_inner(xml).0
}

/// Parse an XMLTV document, failing on malformed XML, truncation,
/// or a document whose root is not `<tv>` (audit E05: an HTML
/// error page served with HTTP 200 is not a guide).
///
/// # Errors
/// Returns the parse problem when the document is not usable XMLTV.
pub fn parse_xmltv_checked(xml: &str) -> Result<EpgGuide, String> {
    let (guide, error) = parse_xmltv_inner(xml);
    match error {
        Some(error) => Err(error),
        None => Ok(guide),
    }
}

fn parse_xmltv_inner(xml: &str) -> (EpgGuide, Option<String>) {
    use quick_xml::events::Event;
    let mut guide = EpgGuide::default();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut buf = Vec::new();
    let mut channel: Option<(String, String)> = None;
    let mut programme: Option<PendingProgramme> = None;
    let mut capture: Option<&'static str> = None;
    let mut text = String::new();
    let mut depth: i64 = 0;
    let mut saw_tv_root = false;
    let mut error: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = local_name(e.name().as_ref());
                if depth == 1 && name == "tv" {
                    saw_tv_root = true;
                }
                start_element(
                    &name,
                    &e,
                    &mut channel,
                    &mut programme,
                    &mut capture,
                    &mut text,
                );
            }
            Ok(Event::Text(t)) => {
                if capture.is_some() {
                    text.push_str(&t);
                }
            }
            Ok(Event::CData(t)) => {
                if capture.is_some() {
                    text.push_str(&t);
                }
            }
            Ok(Event::End(e)) => {
                depth -= 1;
                let name = local_name(e.name().as_ref());
                end_element(
                    &name,
                    &mut guide,
                    &mut channel,
                    &mut programme,
                    &mut capture,
                    &text,
                );
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                error = Some(format!("invalid XMLTV: {e}"));
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    if error.is_none() {
        if !saw_tv_root {
            error = Some("not an XMLTV document (no <tv> root)".to_string());
        } else if depth != 0 {
            error = Some("truncated XMLTV document".to_string());
        }
    }
    for list in guide.programmes.values_mut() {
        list.sort_by_key(|p| p.start);
    }
    (guide, error)
}

/// Apply one opening tag to the parse state: begin a channel or
/// programme, or start capturing the text of a known child.
fn start_element(
    name: &str,
    element: &quick_xml::events::BytesStart<'_>,
    channel: &mut Option<(String, String)>,
    programme: &mut Option<PendingProgramme>,
    capture: &mut Option<&'static str>,
    text: &mut String,
) {
    match name {
        "channel" => {
            let id = attr(element, "id").unwrap_or_default();
            *channel = Some((id, String::new()));
        }
        "programme" => {
            *programme = Some(PendingProgramme {
                row: Programme {
                    channel_id: attr(element, "channel").unwrap_or_default(),
                    start: attr(element, "start")
                        .and_then(|v| parse_xmltv_time(&v))
                        .unwrap_or(0),
                    stop: attr(element, "stop")
                        .and_then(|v| parse_xmltv_time(&v))
                        .unwrap_or(0),
                    title: String::new(),
                    description: String::new(),
                },
                end_attr: attr(element, "end").and_then(|v| parse_xmltv_time(&v)),
                length: None,
                duration: None,
            });
        }
        "display-name" if channel.is_some() => {
            *capture = Some("display-name");
            text.clear();
        }
        "title" if programme.is_some() => {
            *capture = Some("title");
            text.clear();
        }
        "desc" if programme.is_some() => {
            *capture = Some("desc");
            text.clear();
        }
        "length" if programme.is_some() => {
            let units = attr(element, "units").unwrap_or_default();
            if let Some(p) = programme.as_mut() {
                p.length = Some((String::new(), units));
            }
            *capture = Some("length");
            text.clear();
        }
        "duration" if programme.is_some() => {
            *capture = Some("duration");
            text.clear();
        }
        _ => {}
    }
}

/// Apply one closing tag to the parse state (first display name / title /
/// description wins, as in the reference import).
fn end_element(
    name: &str,
    guide: &mut EpgGuide,
    channel: &mut Option<(String, String)>,
    programme: &mut Option<PendingProgramme>,
    capture: &mut Option<&'static str>,
    text: &str,
) {
    match name {
        "channel" => {
            if let Some((id, display)) = channel.take() {
                // First definition in a source wins; Python skips
                // repeats so a renumbered id never wipes the rows
                // its first definition is about to fill.
                if !id.is_empty() && !guide.channels.iter().any(|c| c.id == id) {
                    guide.channels.push(EpgChannel {
                        id,
                        display_name: display,
                    });
                }
            }
        }
        "programme" => {
            if let Some(pending) = programme.take() {
                if let Some(p) = pending.finish() {
                    guide
                        .programmes
                        .entry(p.channel_id.clone())
                        .or_default()
                        .push(p);
                }
            }
        }
        "display-name" | "title" | "desc" | "length" | "duration" => {
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
                            if p.row.title.is_empty() {
                                p.row.title = value;
                            }
                        }
                    }
                    "desc" => {
                        if let Some(p) = programme.as_mut() {
                            if p.row.description.is_empty() {
                                p.row.description = value;
                            }
                        }
                    }
                    "length" => {
                        if let Some(p) = programme.as_mut() {
                            if let Some((slot, _)) = p.length.as_mut() {
                                *slot = value;
                            }
                        }
                    }
                    _ => {
                        if let Some(p) = programme.as_mut() {
                            if p.duration.is_none() {
                                p.duration = Some(value);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn attr(event: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| a.value.into_owned())
}

/// Local wall-clock `YYYY-MM-DD` for an epoch second.
#[must_use]
pub fn local_ymd(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0).map_or_else(
        || "????-??-??".to_string(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        },
    )
}

/// The row suffix Python builds from a channel's now/next
/// (`_programme_label` shapes): " — Title (HH:MM–HH:MM)" for the
/// current programme, " — Next: Title (HH:MM)" for the next one.
#[must_use]
pub fn programme_suffix(current: Option<&Programme>, next: Option<&Programme>) -> String {
    use std::fmt::Write as _;
    let mut suffix = String::new();
    if let Some(p) = current {
        if !p.title.trim().is_empty() {
            let _ = write!(
                suffix,
                " — {} ({}–{})",
                p.title,
                local_hhmm(p.start),
                local_hhmm(p.stop)
            );
        }
    }
    if let Some(p) = next {
        if !p.title.trim().is_empty() {
            let _ = write!(suffix, " — Next: {} ({})", p.title, local_hhmm(p.start));
        }
    }
    suffix
}

/// The highlighted channel's information-panel text (port of
/// `_epg_msg_from_tuple`): the current title, times and
/// description, then the next programme and its description; in a
/// gap the panel leads with the next start.
#[must_use]
pub fn epg_info_message(current: Option<&Programme>, next: Option<&Programme>) -> String {
    use std::fmt::Write as _;
    let mut msg = String::new();
    if let Some(now) = current {
        let _ = write!(
            msg,
            "Now: {} ({} – {})",
            now.title,
            local_hhmm(now.start),
            local_hhmm(now.stop)
        );
        if !now.description.trim().is_empty() {
            msg.push('\n');
            msg.push_str(now.description.trim());
        }
    } else if let Some(nxt) = next {
        let _ = write!(msg, "Starts at {}: {}", local_hhmm(nxt.start), nxt.title);
        if !nxt.description.trim().is_empty() {
            msg.push('\n');
            msg.push_str(nxt.description.trim());
        }
    } else {
        msg.push_str("No program currently airing.");
    }
    if let Some(nxt) = next {
        let _ = write!(
            msg,
            "\nNext: {} ({} – {})",
            nxt.title,
            local_hhmm(nxt.start),
            local_hhmm(nxt.stop)
        );
        if current.is_some() && !nxt.description.trim().is_empty() {
            msg.push('\n');
            msg.push_str(nxt.description.trim());
        }
    }
    msg
}

/// Port of `_now_playing_key`: the canonicalized channel name (or
/// the raw lowercased name when it canonicalizes to nothing),
/// disambiguated by the lowercased tvg-id when both exist.
#[must_use]
pub fn now_playing_key(channel: &Channel) -> String {
    let canonical = canonicalize_name(&channel.name);
    let name_key = if canonical.is_empty() {
        channel.name.trim().to_lowercase()
    } else {
        canonical
    };
    let tvg_id = channel.tvg_id.trim().to_lowercase();
    if !name_key.is_empty() && !tvg_id.is_empty() {
        format!("{name_key}\u{1f}{tvg_id}")
    } else {
        name_key
    }
}

/// Playlist-position → guide-id matches, computed once for a
/// playlist+guide pair (port of Python's program-match cache,
/// invalidated when either side is replaced). Matching a channel
/// is the expensive part of a row-guide build — tvg candidates,
/// word-number expansion, canonical name buckets — so a refresh
/// must never redo it per channel per tick (audit E12).
#[derive(Debug)]
pub struct GuideMatchCache {
    matches: Vec<Option<String>>,
}

impl GuideMatchCache {
    /// Match every channel once against the guide indexes.
    #[must_use]
    pub fn build(channels: &[Channel], indexes: &GuideIndexes) -> Self {
        let matches = channels
            .iter()
            .map(|channel| match_guide_channel(channel, indexes, Some(&indexes.known_ids)))
            .collect();
        Self { matches }
    }

    /// The guide id matched to a playlist position, if any.
    #[must_use]
    pub fn get(&self, position: usize) -> Option<&str> {
        self.matches.get(position).and_then(|id| id.as_deref())
    }

    /// Every match by playlist position.
    #[must_use]
    pub fn matches(&self) -> &[Option<String>] {
        &self.matches
    }
}

/// Suffix and on-air description maps from precomputed matches
/// and one bulk now/next snapshot: the steady-state refresh —
/// one hash lookup per matched channel, no matching work at all.
#[must_use]
pub fn row_guide_from_matches<S: std::hash::BuildHasher>(
    matches: &[Option<String>],
    bulk: &HashMap<String, NowNext, S>,
) -> (HashMap<usize, String>, HashMap<usize, String>) {
    let mut suffixes = HashMap::new();
    let mut descriptions = HashMap::new();
    for (index, guide_id) in matches.iter().enumerate() {
        let Some(entry) = guide_id.as_ref().and_then(|id| bulk.get(id)) else {
            continue;
        };
        let suffix = programme_suffix(entry.current.as_ref(), entry.next.as_ref());
        if !suffix.is_empty() {
            suffixes.insert(index, suffix);
        }
        if let Some(current) = &entry.current {
            let text = current.description.trim();
            if !text.is_empty() {
                descriptions.insert(index, text.to_string());
            }
        }
    }
    (suffixes, descriptions)
}

/// Suffix and on-air description maps for a whole catalogue,
/// built from one bulk now/next snapshot (audit E12): the guide
/// indexes are consulted once per channel against the snapshot,
/// never rebuilt per channel. Keys are playlist indexes. This is
/// the uncached form; recurring refreshes should go through
/// [`GuideMatchCache`] + [`row_guide_from_matches`].
#[must_use]
pub fn build_row_guide<S: std::hash::BuildHasher>(
    channels: &[Channel],
    indexes: &GuideIndexes,
    bulk: &HashMap<String, NowNext, S>,
) -> (HashMap<usize, String>, HashMap<usize, String>) {
    let cache = GuideMatchCache::build(channels, indexes);
    row_guide_from_matches(cache.matches(), bulk)
}

/// The index of the programme airing at `now` in a start-sorted
/// schedule, for the Channel EPG window's initial selection
/// (Python selects the airing programme instead of row zero).
#[must_use]
pub fn airing_index(programmes: &[Programme], now: i64) -> Option<usize> {
    programmes
        .iter()
        .position(|p| p.start <= now && now < p.stop)
}

/// One Channel EPG row's time text (Python `ChannelEPGDialog`):
/// clock times, prefixed with the date for rows not starting
/// today (local).
#[must_use]
pub fn epg_time_range(programme: &Programme, now: i64) -> String {
    let range = format!(
        "{} - {}",
        local_hhmm(programme.start),
        local_hhmm(programme.stop)
    );
    if local_ymd(programme.start) == local_ymd(now) {
        range
    } else {
        format!("{} {}", local_ymd(programme.start), range)
    }
}

/// Python's main-window EPG search constants
/// (`_SEARCH_EPG_MIN_CHARS`, `_SEARCH_EPG_BROAD_CHANNEL_LIMIT`,
/// `_SEARCH_EPG_RESULT_LIMIT`).
pub const SEARCH_EPG_MIN_CHARS: usize = 3;
pub const SEARCH_EPG_BROAD_CHANNEL_LIMIT: usize = 75;
pub const SEARCH_EPG_RESULT_LIMIT: usize = 100;

/// Port of `_should_run_epg_search`'s decision: programme rows
/// join a main-window search only for a sufficiently specific
/// channel-name query in All Channels while no import runs and a
/// guide store exists. Broad title search stays deliberately off
/// this route — a leading-wildcard title scan over a large guide
/// is the first-run CPU spike Python avoids.
#[must_use]
#[allow(clippy::fn_params_excessive_bools)] // flat predicate port of Python's guard
pub fn should_run_epg_search(
    epg_enabled: bool,
    in_all_channels: bool,
    import_running: bool,
    query: &str,
    channel_match_count: usize,
    store_available: bool,
) -> bool {
    epg_enabled
        && in_all_channels
        && !import_running
        && query.trim().chars().count() >= SEARCH_EPG_MIN_CHARS
        && channel_match_count <= SEARCH_EPG_BROAD_CHANNEL_LIMIT
        && store_available
}

/// One EPG search row's label (Python's search append):
/// "{channel} - {title} ({start}–{end})" in local clock times.
#[must_use]
pub fn epg_row_label(channel_name: &str, programme: &Programme) -> String {
    format!(
        "{} - {} ({}–{})",
        channel_name,
        programme.title,
        local_hhmm(programme.start),
        local_hhmm(programme.stop)
    )
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
#[derive(Clone, Debug, Default)]
pub struct GuideIndexes {
    pub display_names: HashMap<String, String>,
    /// Every guide channel id in lower case, built once with the
    /// indexes: callers pass it to `match_guide_channel` instead of
    /// rebuilding the set per channel (audit E12).
    pub known_ids: HashSet<String>,
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
            known_ids: channels.iter().map(|c| c.id.to_lowercase()).collect(),
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
    Fetch {
        sources: Vec<String>,
        store: Option<std::path::PathBuf>,
    },
}

enum EpgEvent {
    Loaded(Box<EpgGuide>, String),
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
            while let Ok(EpgJob::Fetch { sources, store }) = job_receiver.recv() {
                let event = run_epg_load(&sources, store.as_deref());
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

    /// Fetch and parse the given guide sources (URLs or file paths),
    /// then mirror the merged guide into the `SQLite` store at `store`
    /// (when given) on this worker's thread.
    pub fn load(&self, sources: Vec<String>, store: Option<std::path::PathBuf>) {
        let _ = self.jobs.send(EpgJob::Fetch { sources, store });
    }

    /// The next finished load, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<(EpgGuide, String), String>> {
        let events = self.events.lock().ok()?;
        match events.try_recv().ok()? {
            EpgEvent::Loaded(guide, note) => Some(Ok((*guide, note))),
            EpgEvent::Failed(error) => Some(Err(error)),
        }
    }
}

/// One load run, on the worker thread: fetch and check each
/// source, merge the good ones, and mirror each into the store
/// under its own source key. A source counts as loaded only when
/// it parsed AND defined channels (Python's `import_epg_xml`
/// return): a run where every source failed is a failure, and a
/// source that fails or is omitted never erases another source's
/// stored guide (audit E04, E05).
fn run_epg_load(sources: &[String], store_path: Option<&std::path::Path>) -> EpgEvent {
    let mut merged = EpgGuide::default();
    let mut loaded = 0_usize;
    let mut failures: Vec<String> = Vec::new();
    // Mirror to the SQLite store HERE, on the worker thread: at
    // real-world scale (800k+ programmes) the import takes
    // seconds, and doing it on the UI tick froze the whole window
    // (validation 0049).
    let mut store = store_path.and_then(|path| crate::epgstore::EpgStore::open(path).ok());
    let mut store_note = String::new();
    if store_path.is_some() && store.is_none() {
        store_note = " Guide database unavailable.".to_string();
    }
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(0);
    for source in sources {
        match fetch_epg_source(source) {
            Ok(guide) if !guide.channels.is_empty() => {
                if let Some(store) = store.as_mut() {
                    if let Err(error) = store.import_source(&source_key(source), &guide, now) {
                        store_note = format!(" Guide database import failed: {error}");
                    }
                }
                merged.merge(guide);
                loaded += 1;
            }
            Ok(_) => failures.push(format!("{source}: guide defined no channels")),
            Err(error) => failures.push(error),
        }
    }
    if loaded == 0 {
        let first = failures
            .first()
            .cloned()
            .unwrap_or_else(|| "no guide sources were given".to_string());
        return EpgEvent::Failed(first);
    }
    let mut note = store_note;
    if !failures.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(
            note,
            " {} of {} guide sources failed: {}",
            failures.len(),
            sources.len(),
            failures[0]
        );
    }
    EpgEvent::Loaded(Box::new(merged), note)
}

/// Whether re-opening the source is likely to succeed (port of
/// `_is_transient_epg_error`'s message hints: truncated transfers,
/// gzip damage, resets and timeouts are transient; a 404 or a
/// malformed document is not).
#[must_use]
pub fn is_transient_epg_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "end-of-stream marker",
        "compressed file ended",
        "unexpected end of data",
        "crc check failed",
        "incomplete read",
        "connection reset",
        "timed out",
        "timeout",
        "forcibly closed",
        "connection aborted",
        "broken pipe",
        "error decoding response body",
        "failed to read response body",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

/// Fetch one guide source with Python's bounded retries: up to
/// three attempts for transient transfer failures. The body must
/// decode and parse as XMLTV — an HTML error page or a truncated
/// document fails the source (audit E05).
fn fetch_epg_source(source: &str) -> Result<EpgGuide, String> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match fetch_epg_source_once(source) {
            Ok(guide) => return Ok(guide),
            Err(error) => {
                if attempt >= 3 || !is_transient_epg_error(&error) {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(400 * attempt));
            }
        }
    }
}

fn fetch_epg_source_once(source: &str) -> Result<EpgGuide, String> {
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        let mut response = ureq::get(source)
            .call()
            .map_err(|e| format!("{source}: {e}"))?;
        crate::sources::read_body_unbounded(response.body_mut())
            .map_err(|e| format!("{source}: {e}"))?
    } else {
        std::fs::read(source).map_err(|e| format!("{source}: {e}"))?
    };
    let text = decode_epg_bytes(&bytes).map_err(|e| format!("{source}: {e}"))?;
    parse_xmltv_checked(&text).map_err(|e| format!("{source}: {e}"))
}

#[cfg(test)]
mod tests {

    #[test]
    fn worker_mirrors_the_guide_to_the_store_on_its_own_thread() {
        let dir = std::env::temp_dir().join(format!("iptv-epg-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let xml = dir.join("guide.xml");
        std::fs::write(
            &xml,
            "<?xml version=\"1.0\"?><tv><channel id=\"c1\"><display-name>One</display-name></channel><programme channel=\"c1\" start=\"20261101180000 +0000\" stop=\"20261101190000 +0000\"><title>Show</title></programme></tv>",
        )
        .unwrap();
        let store_path = dir.join("epg.db");
        let worker = EpgWorker::new();
        worker.load(
            vec![xml.to_string_lossy().into_owned()],
            Some(store_path.clone()),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let outcome = loop {
            if let Some(result) = worker.try_recv() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        let (guide, note) = outcome.unwrap();
        assert_eq!(guide.programme_count(), 1);
        assert!(note.is_empty(), "{note}");
        let store = crate::epgstore::EpgStore::open(&store_path).unwrap();
        assert_eq!(store.counts().unwrap(), (1, 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn http_guide_over_ureq_default_cap_loads() {
        // Regression pair to the playlist cap: a full XMLTV for a
        // large provider runs to hundreds of MB, far over ureq's
        // 10 MiB read_to_vec cap.
        use std::io::{Read as _, Write as _};
        let mut body = String::from(
            "<?xml version=\"1.0\"?><tv><channel id=\"c1\"><display-name>One</display-name></channel>",
        );
        body.push_str("<!--");
        while body.len() <= 11 * 1024 * 1024 {
            body.push_str("0123456789abcdef");
        }
        body.push_str("-->");
        body.push_str(
            "<programme channel=\"c1\" start=\"20261101180000 +0000\" stop=\"20261101190000 +0000\"><title>Show</title></programme></tv>",
        );
        let bytes = body.into_bytes();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap() == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let guide = fetch_epg_source(&format!("http://127.0.0.1:{port}/guide.xml")).unwrap();
        assert_eq!(guide.programme_count(), 1);
    }
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
    #[test]
    fn sha1_matches_python_hashlib() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
        assert_eq!(source_key("abc"), sha1_hex(b"abc"));
    }

    #[test]
    fn merged_sources_dedupe_and_respect_playlist_opt_out() {
        // Audit E01: own first, provider second, playlist last and
        // only when allowed; stripped-text duplicates collapse.
        let own = vec![" https://a/guide.xml ".to_string()];
        let provider = vec![
            "https://a/guide.xml".to_string(),
            "https://b/g.xml".to_string(),
        ];
        let playlist = vec!["https://c/g.xml".to_string()];
        let merged = merged_epg_sources(&own, &provider, &playlist, true);
        assert_eq!(
            merged,
            [
                " https://a/guide.xml ",
                "https://b/g.xml",
                "https://c/g.xml"
            ]
        );
        let merged = merged_epg_sources(&own, &provider, &playlist, false);
        assert_eq!(merged.len(), 2);
        // The hash ignores order and surrounding whitespace.
        assert_eq!(
            hash_epg_sources(&[" b ".to_string(), "a".to_string()]),
            hash_epg_sources(&["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn auto_import_decision_matches_python() {
        // Audit E03: interval, hash, usability and stamp rules.
        let sources = vec!["https://a/guide.xml".to_string()];
        let hash = hash_epg_sources(&sources);
        let now = 1_000_000;
        // Fresh usable guide, same sources, inside the interval: no load.
        assert!(!should_auto_import_epg(
            &sources,
            &hash,
            now - 60,
            now,
            6.0,
            true
        ));
        // Interval elapsed: load.
        assert!(should_auto_import_epg(
            &sources,
            &hash,
            now - 7 * 3600,
            now,
            6.0,
            true
        ));
        // Sources changed: load.
        assert!(should_auto_import_epg(
            &sources,
            "other",
            now - 60,
            now,
            6.0,
            true
        ));
        // Guide not usable: load.
        assert!(should_auto_import_epg(
            &sources,
            &hash,
            now - 60,
            now,
            6.0,
            false
        ));
        // Never imported / future stamp: load.
        assert!(should_auto_import_epg(&sources, &hash, 0, now, 6.0, true));
        assert!(should_auto_import_epg(
            &sources,
            &hash,
            now + 10_000,
            now,
            6.0,
            true
        ));
        // Non-positive interval means always.
        assert!(should_auto_import_epg(
            &sources,
            &hash,
            now - 60,
            now,
            0.0,
            true
        ));
    }

    #[test]
    fn checked_parse_rejects_non_guides_and_truncation() {
        // Audit E05: HTML with HTTP 200, truncated XML and damaged
        // gzip fail the source instead of replacing a good guide.
        assert!(parse_xmltv_checked("<html><body>busy</body></html>").is_err());
        assert!(parse_xmltv_checked("<tv><channel id=\"c1\">").is_err());
        assert!(parse_xmltv_checked("<tv><channel id=\"c1\"></tv>").is_err());
        assert!(parse_xmltv_checked(GUIDE).is_ok());
        let damaged = [0x1f, 0x8b, 0x08, 0x00, 0x55];
        assert!(decode_epg_bytes(&damaged).is_err());
        let plain = decode_epg_bytes(GUIDE.as_bytes()).expect("plain guide decodes");
        assert!(plain.contains("BBC Two"));
    }

    #[test]
    fn merge_upserts_slots_and_enriches_descriptions() {
        // Audit E07: duplicate slots collapse, a corrected
        // overlapping slot replaces the old row, and an exact slot
        // keeps the longer description.
        let base = parse_xmltv(GUIDE);
        let mut merged = base.clone();
        merged.merge(parse_xmltv(GUIDE));
        assert_eq!(merged.programme_count(), 2, "re-import must not duplicate");
        // An exact slot with a longer description enriches in place.
        let richer = parse_xmltv(
            r#"<tv><channel id="bbc2.uk"><display-name>BBC Two</display-name></channel>
            <programme channel="bbc2.uk" start="20261101180000 +0000" stop="20261101190000 +0000">
            <title>News Hour</title><desc>Evening news, expanded with more detail.</desc></programme></tv>"#,
        );
        merged.merge(richer);
        let rows = &merged.programmes["bbc2.uk"];
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].description,
            "Evening news, expanded with more detail."
        );
        // A corrected slot overlapping stored rows replaces them:
        // both News Hour (18:00-19:00) and Quiz (19:00-20:00)
        // overlap 18:30-19:30, so only the correction remains.
        let corrected = parse_xmltv(
            r#"<tv><channel id="bbc2.uk"><display-name>BBC Two</display-name></channel>
            <programme channel="bbc2.uk" start="20261101183000 +0000" stop="20261101193000 +0000">
            <title>Corrected</title></programme></tv>"#,
        );
        merged.merge(corrected);
        let rows = &merged.programmes["bbc2.uk"];
        let titles: Vec<&str> = rows.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Corrected"], "{rows:?}");
    }

    #[test]
    fn bulk_snapshot_and_row_guide_match_single_lookups() {
        // Audit E12: the bulk build agrees with per-channel
        // now/next and needs no per-channel index rebuilds.
        let guide = parse_xmltv(GUIDE);
        let indexes = GuideIndexes::build(&guide.channels);
        let at = parse_xmltv_time("20261101183000 +0000").expect("time");
        let bulk = guide.bulk_now_next(at);
        let entry = &bulk["bbc2.uk"];
        assert_eq!(entry.current.as_ref().expect("now").title, "News Hour");
        assert_eq!(entry.next.as_ref().expect("next").title, "Quiz");
        let channels = vec![channel("BBC 2 HD", "", ""), channel("Unknown", "", "")];
        let (suffixes, descriptions) = build_row_guide(&channels, &indexes, &bulk);
        assert!(suffixes[&0].contains("News Hour"), "{suffixes:?}");
        assert!(suffixes[&0].contains("Next: Quiz"), "{suffixes:?}");
        assert_eq!(descriptions[&0], "Evening news");
        assert!(!suffixes.contains_key(&1));
        // The match-cache path (the recurring refresh) must agree
        // exactly with the uncached build.
        let cache = GuideMatchCache::build(&channels, &indexes);
        assert_eq!(cache.get(0), Some("bbc2.uk"));
        assert_eq!(cache.get(1), None);
        let cached = row_guide_from_matches(cache.matches(), &bulk);
        assert_eq!(cached, (suffixes, descriptions));
    }

    #[test]
    fn info_message_carries_now_next_and_gap_context() {
        // Audit E10: current title/times/description, then next;
        // in a gap the next start leads.
        let guide = parse_xmltv(GUIDE);
        let rows = &guide.programmes["bbc2.uk"];
        let message = epg_info_message(Some(&rows[0]), Some(&rows[1]));
        assert!(message.starts_with("Now: News Hour ("), "{message}");
        assert!(message.contains("Evening news"), "{message}");
        assert!(message.contains("\nNext: Quiz ("), "{message}");
        let gap = epg_info_message(None, Some(&rows[1]));
        assert!(gap.starts_with("Starts at "), "{gap}");
        assert!(gap.contains("Quiz"), "{gap}");
        assert_eq!(epg_info_message(None, None), "No program currently airing.");
    }

    #[test]
    fn epg_search_guard_keeps_broad_searches_out() {
        // Audit E08: min chars, broad-match cap, All Channels only,
        // no import running, store present, guide enabled.
        assert!(should_run_epg_search(true, true, false, "news", 3, true));
        assert!(!should_run_epg_search(true, true, false, "ne", 0, true));
        assert!(!should_run_epg_search(true, true, false, "news", 76, true));
        assert!(!should_run_epg_search(true, false, false, "news", 3, true));
        assert!(!should_run_epg_search(true, true, true, "news", 3, true));
        assert!(!should_run_epg_search(true, true, false, "news", 3, false));
        assert!(!should_run_epg_search(false, true, false, "news", 3, true));
        assert_eq!(
            epg_row_label("BBC Two", &parse_xmltv(GUIDE).programmes["bbc2.uk"][0]),
            "BBC Two - News Hour (18:00–19:00)"
                .replace(
                    "18:00",
                    &local_hhmm(parse_xmltv_time("20261101180000 +0000").unwrap())
                )
                .replace(
                    "19:00",
                    &local_hhmm(parse_xmltv_time("20261101190000 +0000").unwrap())
                )
        );
    }

    #[test]
    fn channel_epg_selection_and_date_labels() {
        // Audit E09: the airing programme is the initial selection;
        // non-today rows carry a date, today's do not.
        let guide = parse_xmltv(GUIDE);
        let rows = &guide.programmes["bbc2.uk"];
        let at = rows[1].start + 60;
        assert_eq!(airing_index(rows, at), Some(1));
        assert_eq!(airing_index(rows, rows[0].start - 10), None);
        let today = epg_time_range(&rows[0], rows[0].start);
        assert!(
            !today.contains('-') || today.matches('-').count() == 1,
            "{today}"
        );
        let later = epg_time_range(&rows[0], rows[0].start + 3 * 86_400);
        assert!(later.starts_with(&local_ymd(rows[0].start)), "{later}");
    }

    #[test]
    fn worker_partial_failure_keeps_loaded_sources_and_reports() {
        // Audits E04/E05 at the worker seam: one good + one broken
        // source loads the good one and says so; a later run with
        // only the good source leaves the other's stored rows.
        let dir = std::env::temp_dir().join(format!("iptv-epg-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.xml");
        std::fs::write(&good, GUIDE).unwrap();
        let other = dir.join("other.xml");
        std::fs::write(
            &other,
            "<?xml version=\"1.0\"?><tv><channel id=\"x1\"><display-name>X One</display-name></channel><programme channel=\"x1\" start=\"20261101180000 +0000\" stop=\"20261101190000 +0000\"><title>X Show</title></programme></tv>",
        )
        .unwrap();
        let broken = dir.join("broken.xml");
        std::fs::write(&broken, "<html>provider busy</html>").unwrap();
        let store_path = dir.join("epg.db");
        let worker = EpgWorker::new();
        worker.load(
            vec![
                good.to_string_lossy().into_owned(),
                broken.to_string_lossy().into_owned(),
            ],
            Some(store_path.clone()),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let (guide, note) = loop {
            if let Some(result) = worker.try_recv() {
                break result.expect("one good source loads");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        assert_eq!(guide.programme_count(), 2);
        assert!(note.contains("1 of 2 guide sources failed"), "{note}");
        // Second run imports the other source too; third run with
        // only the first must not erase the second from the store.
        worker.load(
            vec![other.to_string_lossy().into_owned()],
            Some(store_path.clone()),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if worker.try_recv().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        worker.load(
            vec![good.to_string_lossy().into_owned()],
            Some(store_path.clone()),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if worker.try_recv().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let store = crate::epgstore::EpgStore::open(&store_path).unwrap();
        assert_eq!(store.counts().unwrap(), (3, 3));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_datetime_formats_like_python() {
        // 2026-11-02 00:00 UTC renders as a date + HH:MM in local
        // time; only the shape is asserted (timezone-dependent).
        let text = local_datetime(1_793_952_000);
        assert_eq!(text.len(), 16, "{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[13..14], ":");
        assert_eq!(local_datetime(i64::MAX), "Unknown time");
    }
}

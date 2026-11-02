//! Local live buffer (timeshift) bookkeeping, ported from Python's
//! `live_buffer.py` and the `LiveBufferMixin` in `client_timeshift.py`.
//!
//! Pure model only: segment payloads are tracked as byte counts, the
//! ffmpeg ingest process, the loopback HTTP server and the on-disk
//! spool are adapter seams. What lives here is everything Python
//! decides without touching IO: the bounded segment window (time and
//! size caps with oldest-first eviction), the retained-timeline math
//! behind Rewind Live TV, view/lease accounting, and the rule that
//! the buffer cannot be disabled while a recording feeds from it.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde_json::Value;

use crate::config::Config;

/// Python `buffer_settings` defaults (`live_buffer.py`:33-35).
pub const DEFAULT_BUFFER_SECONDS: i64 = 600;
/// Python `buffer_settings` default size cap (`live_buffer.py`:35).
pub const DEFAULT_BUFFER_MIB: i64 = 512;
/// Smallest size the settings dialog accepts (`client_timeshift.py`:67).
pub const MIN_BUFFER_MIB: i64 = 64;
/// Largest size the settings dialog accepts (`client_timeshift.py`:67).
pub const MAX_BUFFER_MIB: i64 = 4096;
/// Dialog range for the retention prompt (`client_timeshift.py`:50).
pub const MAX_DIALOG_MINUTES: i64 = 30;
/// `buffer_settings` clamps retention to 60..=1800 (`live_buffer.py`:34).
pub const MIN_BUFFER_SECONDS: i64 = 60;
/// See [`MIN_BUFFER_SECONDS`].
pub const MAX_BUFFER_SECONDS: i64 = 1800;
/// One spool write chunk; larger writes are rejected (`live_buffer.py`:24).
pub const CHUNK_BYTES: u64 = 65_536;
/// Manifest and window segment ceiling (`live_buffer.py`, `DiskWindow`).
pub const MAX_SEGMENTS: usize = 1024;
/// Manifest header/footer budget (`live_buffer.py`:25).
pub const MANIFEST_LIMIT: usize = 256 * 1024;
/// Live view ceiling per session (`live_buffer.py`, `cursor()`).
pub const MAX_VIEWS: usize = 32;
/// Recording (protected) view ceiling (`live_buffer.py`, `cursor()`).
pub const MAX_PROTECTED_VIEWS: usize = 16;

/// A timeshift failure, carrying Python's message text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeshiftError {
    /// `ValueError('invalid quota')` — window size below one chunk.
    InvalidQuota,
    /// `ValueError('invalid duration')` / `BufferError('invalid duration')`.
    InvalidDuration,
    /// `BufferError('invalid segment upload')`.
    InvalidSegmentUpload,
    /// `ValueError('oversized write')` — one chunk above [`CHUNK_BYTES`].
    OversizedWrite,
    /// `BufferError('no active segment')`.
    NoActiveSegment,
    /// `BufferError('one segment exceeds quota')`.
    SegmentExceedsQuota,
    /// `BufferError('oversized manifest')`.
    OversizedManifest,
    /// `BufferError('invalid manifest')` or an unparseable manifest field.
    InvalidManifest,
    /// `BufferError('invalid sequence')` — media sequence out of range.
    InvalidSequence,
    /// `BufferError('invalid segment reference')`.
    InvalidSegmentReference,
    /// `BufferError('too many segments')`.
    TooManySegments,
    /// `ValueError('invalid position')` — non-finite rewind offset.
    InvalidPosition,
    /// `BufferError('no completed history')`.
    NoHistory,
    /// `BufferError('too many recordings')`.
    TooManyRecordings,
    /// `BufferError('source is closing')`.
    SourceClosing,
    /// The mixin's refusal (`client_timeshift.py`:57-59).
    StillFeedingRecording,
}

impl fmt::Display for TimeshiftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::InvalidQuota => "invalid quota",
            Self::InvalidDuration => "invalid duration",
            Self::InvalidSegmentUpload => "invalid segment upload",
            Self::OversizedWrite => "oversized write",
            Self::NoActiveSegment => "no active segment",
            Self::SegmentExceedsQuota => "one segment exceeds quota",
            Self::OversizedManifest => "oversized manifest",
            Self::InvalidManifest => "invalid manifest",
            Self::InvalidSequence => "invalid sequence",
            Self::InvalidSegmentReference => "invalid segment reference",
            Self::TooManySegments => "too many segments",
            Self::InvalidPosition => "invalid position",
            Self::NoHistory => "no completed history",
            Self::TooManyRecordings => "too many recordings",
            Self::SourceClosing => "source is closing",
            Self::StillFeedingRecording => {
                "The local buffer is still feeding a recording. \
                 Stop the recording before disabling it."
            }
        };
        f.write_str(text)
    }
}

impl std::error::Error for TimeshiftError {}

/// Resolved live-buffer settings (Python `buffer_settings`,
/// `live_buffer.py`:30-36).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferSettings {
    /// Only an explicit JSON `true` enables the buffer.
    pub enabled: bool,
    /// Retention cap in seconds, clamped to 60..=1800.
    pub seconds: i64,
    /// Size cap in MiB, clamped to 64..=4096.
    pub mib: i64,
}

impl Default for BufferSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            seconds: DEFAULT_BUFFER_SECONDS,
            mib: DEFAULT_BUFFER_MIB,
        }
    }
}

/// Python's `integer()` helper: only a true JSON integer counts
/// (floats and other types fall back), then clamp.
fn config_int(config: &Config, key: &str, default: i64, low: i64, high: i64) -> i64 {
    match config.value(key) {
        Some(Value::Number(number)) => number.as_i64().map_or(default, |v| v.clamp(low, high)),
        _ => default,
    }
}

impl BufferSettings {
    /// Read the three `live_buffer_*` keys from the app config.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            enabled: matches!(config.value("live_buffer_enabled"), Some(Value::Bool(true))),
            seconds: config_int(
                config,
                "live_buffer_seconds",
                DEFAULT_BUFFER_SECONDS,
                MIN_BUFFER_SECONDS,
                MAX_BUFFER_SECONDS,
            ),
            mib: config_int(
                config,
                "live_buffer_mib",
                DEFAULT_BUFFER_MIB,
                MIN_BUFFER_MIB,
                MAX_BUFFER_MIB,
            ),
        }
    }

    /// The size cap in bytes (`mib * 1024**2`, `live_buffer.py`:123).
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        u64::try_from(self.mib).unwrap_or(0) * 1024 * 1024
    }

    /// The value the settings dialog opens with: minutes when
    /// enabled, 0 when disabled (`client_timeshift.py`:51).
    #[must_use]
    pub fn dialog_minutes(&self) -> i64 {
        if self.enabled {
            self.seconds / 60
        } else {
            0
        }
    }

    /// Apply the settings dialog outcome (`client_timeshift.py`:55-72):
    /// 0 minutes disables (retention/size kept), otherwise the buffer
    /// is enabled with `minutes * 60` seconds and the chosen size.
    pub fn apply_dialog_choice(&mut self, minutes: i64, mib: i64) {
        if minutes <= 0 {
            self.enabled = false;
        } else {
            self.enabled = true;
            self.seconds = minutes.clamp(0, MAX_DIALOG_MINUTES) * 60;
            self.mib = mib.clamp(MIN_BUFFER_MIB, MAX_BUFFER_MIB);
        }
    }
}

/// The playback situation Python's `_intercept_live_buffer`
/// weighs before engaging the buffer (client_timeshift.py:84-90).
/// The four conditions are independent inputs in Python, so they
/// stay independent fields here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngageContext {
    /// The built-in player is the selected player.
    pub builtin_player: bool,
    /// A cast device is connected.
    pub casting: bool,
    /// The stream kind is live.
    pub live_stream: bool,
    /// The channel is a movie (VOD), not a live channel.
    pub channel_is_movie: bool,
}

/// Python `_intercept_live_buffer`'s want test: the buffer engages
/// only when enabled, for the built-in player, while not casting,
/// for live streams that are not movies.
#[must_use]
pub fn wants_buffer(settings: &BufferSettings, context: EngageContext) -> bool {
    settings.enabled
        && context.builtin_player
        && !context.casting
        && context.live_stream
        && !context.channel_is_movie
}

/// One retained segment (Python `Segment`, `live_buffer.py`:71-82).
/// Times are seconds on the buffer timeline, which starts at 0 for
/// the first segment the source ever published.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// File name, `seg-<digits>.ts`.
    pub name: String,
    /// HLS media sequence number.
    pub sequence: u64,
    /// Timeline start in seconds.
    pub start: f64,
    /// Segment duration in seconds.
    pub duration: f64,
    /// Byte size on disk.
    pub size: u64,
    /// Whether a discontinuity precedes this segment.
    pub discontinuity: bool,
}

impl Segment {
    /// Timeline end (`start + duration`).
    #[must_use]
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
}

/// The numeric index inside a `seg-<digits>.ts` name, if the name
/// matches Python's `_SEGMENT` pattern (`live_buffer.py`:27).
fn segment_index(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("seg-")?.strip_suffix(".ts")?;
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One parsed manifest row: (sequence, name, duration, discontinuity).
type ManifestRow = (u64, String, f64, bool);

/// Parse and validate a manifest (the front half of Python's
/// `publish`). `Ok(None)` is Python's early return for a manifest
/// with no segment rows.
///
/// # Errors
/// The manifest validation errors from `live_buffer.py`.
fn parse_manifest(payload: &str) -> Result<Option<Vec<ManifestRow>>, TimeshiftError> {
    if payload.len() > MANIFEST_LIMIT {
        return Err(TimeshiftError::OversizedManifest);
    }
    if !payload.starts_with("#EXTM3U") {
        return Err(TimeshiftError::InvalidManifest);
    }
    let mut sequence: u64 = 0;
    let mut duration: Option<f64> = None;
    let mut discontinuity = false;
    let mut rows: Vec<ManifestRow> = Vec::new();
    for raw_line in payload.lines() {
        let line = raw_line.trim();
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = value
                .trim()
                .parse()
                .map_err(|_| TimeshiftError::InvalidManifest)?;
            if sequence > (1 << 53) {
                return Err(TimeshiftError::InvalidSequence);
            }
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let parsed: f64 = value
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .parse()
                .map_err(|_| TimeshiftError::InvalidManifest)?;
            if !parsed.is_finite() || parsed <= 0.0 || parsed > 3600.0 {
                return Err(TimeshiftError::InvalidDuration);
            }
            duration = Some(parsed);
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if !line.is_empty() && !line.starts_with('#') {
            let path = line.split(['?', '#']).next().unwrap_or_default();
            let name = path.rsplit('/').next().unwrap_or_default();
            let Some(pending) = duration else {
                return Err(TimeshiftError::InvalidSegmentReference);
            };
            if segment_index(name).is_none() {
                return Err(TimeshiftError::InvalidSegmentReference);
            }
            rows.push((sequence, name.to_string(), pending, discontinuity));
            sequence += 1;
            duration = None;
            discontinuity = false;
        }
    }
    if rows.len() > MAX_SEGMENTS {
        return Err(TimeshiftError::TooManySegments);
    }
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(rows))
}

/// Python `math.ceil` for a segment duration, as an integer.
// Python durations are positive and bounded by the manifest
// validation above, so the conversion cannot lose meaning.
#[allow(clippy::cast_possible_truncation)]
fn target_ceil(duration: f64) -> i64 {
    duration.ceil() as i64
}

/// Bookkeeping for Python's `DiskWindow` (`live_buffer.py`:85-217):
/// completed segment files, the in-progress segment, byte usage and
/// the retained timeline. Payload bytes never enter the model; the
/// spool layer reports chunk lengths and finished sizes.
#[derive(Clone, Debug)]
pub struct BufferWindow {
    max_bytes: u64,
    seconds: f64,
    used: u64,
    /// (name, size) pairs, kept sorted by segment index on finish.
    files: Vec<(String, u64)>,
    timeline: Vec<Segment>,
    known: BTreeMap<u64, f64>,
    last_end: f64,
    active: Option<(String, u64)>,
    manifest: Option<String>,
    target_duration: Option<i64>,
    closed: bool,
    ended: bool,
    error: Option<String>,
}

impl BufferWindow {
    /// Python's constructor validation: the quota must hold at
    /// least one chunk and the retention must be finite and >= 1.
    ///
    /// # Errors
    /// [`TimeshiftError::InvalidQuota`] or
    /// [`TimeshiftError::InvalidDuration`].
    pub fn new(max_bytes: u64, seconds: f64) -> Result<Self, TimeshiftError> {
        if max_bytes < CHUNK_BYTES {
            return Err(TimeshiftError::InvalidQuota);
        }
        if !seconds.is_finite() || seconds < 1.0 {
            return Err(TimeshiftError::InvalidDuration);
        }
        Ok(Self {
            max_bytes,
            seconds,
            used: 0,
            files: Vec::new(),
            timeline: Vec::new(),
            known: BTreeMap::new(),
            last_end: 0.0,
            active: None,
            manifest: None,
            target_duration: None,
            closed: false,
            ended: false,
            error: None,
        })
    }

    /// Bytes currently held, including the in-progress segment.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    /// Completed segment count.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Whether input marked the stream ended (ENDLIST seen and the
    /// final segment is on disk).
    #[must_use]
    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// Whether the window has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The latched window error, if any (Python `window.error`).
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Latch an error reported by the ingest layer (Python sets
    /// `window.error` from its upload and stderr paths).
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    /// The retained timeline (Python `snapshot()`).
    #[must_use]
    pub fn snapshot(&self) -> &[Segment] {
        &self.timeline
    }

    /// Oldest and newest retained timeline starts — the rewind
    /// dialog's range is `last.start - first.start`
    /// (`client_timeshift.py`:227-229).
    #[must_use]
    pub fn retained_span(&self) -> Option<(f64, f64)> {
        let first = self.timeline.first()?;
        let last = self.timeline.last()?;
        Some((first.start, last.start))
    }

    /// The rewind dialog's maximum offset in whole seconds, or
    /// `None` when nothing is retained yet.
    #[must_use]
    // Python truncates with int(); the span is non-negative.
    #[allow(clippy::cast_possible_truncation)]
    pub fn rewind_range(&self) -> Option<i64> {
        let (first, last) = self.retained_span()?;
        Some(i64::max(0, (last - first) as i64))
    }

    /// Whether a paused playback position has fallen out of the
    /// retained window (`client_timeshift.py`, `_poll_live_buffer`).
    #[must_use]
    pub fn position_expired(&self, position: f64) -> bool {
        self.timeline
            .first()
            .is_some_and(|first| position < first.start)
    }

    fn remove_file(&mut self, name: &str) {
        if let Some(index) = self.files.iter().position(|(n, _)| n == name) {
            let (_, size) = self.files.remove(index);
            self.used -= size;
        }
        self.timeline.retain(|row| row.name != name);
    }

    /// Begin receiving one segment (Python `begin`). While the
    /// window already holds [`MAX_SEGMENTS`] files, the oldest are
    /// evicted first.
    ///
    /// # Errors
    /// [`TimeshiftError::InvalidSegmentUpload`] when closed, busy,
    /// misnamed or a duplicate.
    pub fn begin_segment(&mut self, name: &str) -> Result<(), TimeshiftError> {
        if self.closed
            || self.active.is_some()
            || segment_index(name).is_none()
            || self.files.iter().any(|(n, _)| n == name)
        {
            return Err(TimeshiftError::InvalidSegmentUpload);
        }
        while self.files.len() >= MAX_SEGMENTS {
            let Some(oldest) = self.files.first().map(|(name, _)| name.clone()) else {
                break;
            };
            self.remove_file(&oldest);
        }
        self.active = Some((name.to_string(), 0));
        Ok(())
    }

    /// Record one received chunk of `len` bytes (Python `append`).
    /// Oldest completed segments are evicted while the chunk would
    /// exceed the size cap.
    ///
    /// # Errors
    /// [`TimeshiftError::OversizedWrite`],
    /// [`TimeshiftError::NoActiveSegment`] or
    /// [`TimeshiftError::SegmentExceedsQuota`].
    pub fn append_chunk(&mut self, len: u64) -> Result<(), TimeshiftError> {
        if len > CHUNK_BYTES {
            return Err(TimeshiftError::OversizedWrite);
        }
        if self.closed || self.active.is_none() {
            return Err(TimeshiftError::NoActiveSegment);
        }
        while self.used + len > self.max_bytes {
            let Some(oldest) = self.files.first().map(|(name, _)| name.clone()) else {
                break;
            };
            self.remove_file(&oldest);
        }
        if self.used + len > self.max_bytes {
            return Err(TimeshiftError::SegmentExceedsQuota);
        }
        if let Some((_, size)) = self.active.as_mut() {
            *size += len;
            self.used += len;
        }
        Ok(())
    }

    /// Complete the in-progress segment (Python `finish`): it joins
    /// the file list, re-sorted by segment index, and a previously
    /// published manifest is re-applied so the timeline catches up.
    ///
    /// # Errors
    /// [`TimeshiftError::NoActiveSegment`], or a manifest error from
    /// the re-applied publish.
    pub fn finish_segment(&mut self) -> Result<(), TimeshiftError> {
        let Some((name, size)) = self.active.take() else {
            return Err(TimeshiftError::NoActiveSegment);
        };
        self.files.push((name, size));
        self.files
            .sort_by_key(|(name, _)| segment_index(name).unwrap_or(0));
        if let Some(manifest) = self.manifest.clone() {
            self.publish(&manifest)?;
        }
        Ok(())
    }

    /// Discard the in-progress segment (Python `abort`).
    pub fn abort_segment(&mut self) {
        if let Some((_, size)) = self.active.take() {
            self.used -= size;
        }
    }

    /// Apply one HLS manifest from the ingest (Python `publish`):
    /// rebuild the retained timeline from the segments on disk,
    /// then evict by the retention cap. Manifest text parsing is
    /// part of the model, as in Python.
    ///
    /// # Errors
    /// The manifest validation errors from `live_buffer.py`.
    pub fn publish(&mut self, payload: &str) -> Result<(), TimeshiftError> {
        let Some(rows) = parse_manifest(payload)? else {
            return Ok(());
        };
        if self.closed {
            return Ok(());
        }
        self.manifest = Some(payload.to_string());
        // Python keeps one stable target duration; a larger GOP
        // later is an error and the timeline is left untouched.
        let largest = rows.iter().map(|r| r.2).fold(0.0_f64, f64::max);
        match self.target_duration {
            None => self.target_duration = Some(target_ceil(largest) + 1),
            Some(target) if target_ceil(largest) > target => {
                self.error = Some(
                    "source GOP duration changed beyond the established playlist target"
                        .to_string(),
                );
                return Ok(());
            }
            _ => {}
        }
        let first = rows[0].0;
        let mut start = self.known.get(&first).copied().unwrap_or(self.last_end);
        let mut known = BTreeMap::new();
        let mut timeline: Vec<Segment> = Vec::new();
        for (seq, name, dur, disc) in &rows {
            known.insert(*seq, start);
            if let Some((_, size)) = self.files.iter().find(|(n, _)| n == name) {
                if timeline
                    .last()
                    .is_some_and(|last: &Segment| *seq != last.sequence + 1)
                {
                    timeline.clear();
                }
                timeline.push(Segment {
                    name: name.clone(),
                    sequence: *seq,
                    start,
                    duration: *dur,
                    size: *size,
                    discontinuity: *disc,
                });
            }
            start += dur;
        }
        self.known = known;
        self.last_end = start;
        self.timeline = timeline;
        self.ended = payload.contains("#EXT-X-ENDLIST")
            && rows
                .last()
                .is_some_and(|(_, name, _, _)| self.files.iter().any(|(n, _)| n == name));
        self.evict_expired(first);
        Ok(())
    }

    /// Retention-cap eviction after a publish (`live_buffer.py`:
    /// 209-216): drop files at or before the cutoff, plus files
    /// the manifest has moved past.
    fn evict_expired(&mut self, first_sequence: u64) {
        let Some(last) = self.timeline.last().cloned() else {
            return;
        };
        let cutoff = last.end() - self.seconds;
        let by_name: BTreeMap<&str, &Segment> =
            self.timeline.iter().map(|r| (r.name.as_str(), r)).collect();
        let doomed: Vec<String> = self
            .files
            .iter()
            .filter(|(name, _)| {
                let index = segment_index(name).unwrap_or(0);
                match by_name.get(name.as_str()) {
                    None => index < first_sequence,
                    Some(row) => row.end() <= cutoff,
                }
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in doomed {
            self.remove_file(&name);
        }
    }

    /// Close the window (Python `close`): the in-progress segment
    /// is discarded and further uploads are refused.
    pub fn close(&mut self) {
        self.closed = true;
        self.abort_segment();
    }
}

/// One live view onto the buffer (Python `cursor()` result): the
/// segment playback or a recording starts from.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    /// Opaque view id; pass back to
    /// [`LiveBuffer::release_view`] / [`LiveBuffer::take_initial`].
    pub view_id: u64,
    /// Media sequence the view starts at.
    pub first_sequence: u64,
    /// The segment the view starts on.
    pub segment: Segment,
}

#[derive(Clone, Debug)]
struct View {
    first_sequence: u64,
    protected: bool,
    initial: bool,
}

/// Session-level bookkeeping (Python `LiveBufferSession` minus its
/// process and server): the window, lease counting and view
/// accounting.
#[derive(Debug)]
pub struct LiveBuffer {
    window: BufferWindow,
    leases: HashSet<u64>,
    next_lease: u64,
    views: Vec<(u64, View)>,
    next_view: u64,
    closed: bool,
}

impl LiveBuffer {
    /// A session starts holding one lease: its owner's
    /// (`live_buffer.py`: `_leases = 1`).
    ///
    /// # Errors
    /// The window constructor's validation errors.
    pub fn new(settings: &BufferSettings) -> Result<Self, TimeshiftError> {
        let mut leases = HashSet::new();
        leases.insert(0);
        Ok(Self {
            window: BufferWindow::new(
                settings.max_bytes(),
                f64::from(u32::try_from(settings.seconds).unwrap_or(0)),
            )?,
            leases,
            next_lease: 1,
            views: Vec::new(),
            next_view: 1,
            closed: false,
        })
    }

    /// The segment window.
    #[must_use]
    pub fn window(&self) -> &BufferWindow {
        &self.window
    }

    /// The segment window, mutably (ingest seam).
    pub fn window_mut(&mut self) -> &mut BufferWindow {
        &mut self.window
    }

    /// Outstanding leases (Python `lease_count`).
    #[must_use]
    pub fn lease_count(&self) -> usize {
        self.leases.len()
    }

    /// Whether the session has fully closed (last lease released
    /// or [`LiveBuffer::close`] called).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Take another lease (Python `acquire`); returns its id.
    ///
    /// # Errors
    /// [`TimeshiftError::SourceClosing`] once closed.
    pub fn acquire(&mut self) -> Result<u64, TimeshiftError> {
        if self.closed {
            return Err(TimeshiftError::SourceClosing);
        }
        let id = self.next_lease;
        self.next_lease += 1;
        self.leases.insert(id);
        Ok(id)
    }

    /// Release a lease (Python `BufferLease.release`: idempotent).
    /// Releasing the last lease closes the session.
    pub fn release_lease(&mut self, id: u64) {
        self.leases.remove(&id);
        if self.leases.is_empty() {
            self.closed = true;
            self.window.close();
        }
    }

    /// The mixin's disable rule: with more than the owner's lease
    /// outstanding, something (a recording) is feeding from the
    /// buffer and disabling is refused.
    ///
    /// # Errors
    /// [`TimeshiftError::StillFeedingRecording`].
    pub fn check_disable_allowed(&self) -> Result<(), TimeshiftError> {
        if self.lease_count() > 1 {
            return Err(TimeshiftError::StillFeedingRecording);
        }
        Ok(())
    }

    /// Open a view `behind` seconds behind the live edge (Python
    /// `cursor`): the offset is clamped to the retained span and 0
    /// means live. Protected views are the recording path and are
    /// capped separately; unprotected views evict oldest-first past
    /// [`MAX_VIEWS`].
    ///
    /// # Errors
    /// [`TimeshiftError::InvalidPosition`],
    /// [`TimeshiftError::NoHistory`] or
    /// [`TimeshiftError::TooManyRecordings`].
    pub fn cursor(&mut self, behind: f64, protected: bool) -> Result<Cursor, TimeshiftError> {
        if !behind.is_finite() {
            return Err(TimeshiftError::InvalidPosition);
        }
        let rows = self.window.snapshot();
        if rows.is_empty() {
            return Err(TimeshiftError::NoHistory);
        }
        let target = f64::max(
            rows[0].start,
            rows[rows.len() - 1].start - f64::max(0.0, behind),
        );
        let row = rows
            .iter()
            .rev()
            .find(|r| r.start <= target)
            .unwrap_or(&rows[0])
            .clone();
        if protected
            && self.views.iter().filter(|(_, v)| v.protected).count() >= MAX_PROTECTED_VIEWS
        {
            return Err(TimeshiftError::TooManyRecordings);
        }
        let view_id = self.next_view;
        self.next_view += 1;
        self.views.push((
            view_id,
            View {
                first_sequence: row.sequence,
                protected,
                initial: true,
            },
        ));
        while self.views.len() > MAX_VIEWS {
            let Some(position) = self.views.iter().position(|(_, v)| !v.protected) else {
                break;
            };
            self.views.remove(position);
        }
        Ok(Cursor {
            view_id,
            first_sequence: row.sequence,
            segment: row,
        })
    }

    /// The media sequence a view starts at (the playlist seam
    /// serves only segments at or after it).
    #[must_use]
    pub fn view_first_sequence(&self, view_id: u64) -> Option<u64> {
        self.views
            .iter()
            .find(|(id, _)| *id == view_id)
            .map(|(_, view)| view.first_sequence)
    }

    /// Read and clear a view's initial flag (Python's playlist
    /// fetch consumes it: the first playlist for a fresh view is
    /// trimmed until the input ends).
    #[must_use]
    pub fn take_initial(&mut self, view_id: u64) -> bool {
        if let Some((_, view)) = self.views.iter_mut().find(|(id, _)| *id == view_id) {
            let was = view.initial;
            view.initial = false;
            return was;
        }
        false
    }

    /// Drop a view (Python `BufferLease.release` for its cursor).
    pub fn release_view(&mut self, view_id: u64) {
        self.views.retain(|(id, _)| *id != view_id);
    }

    /// Number of live views.
    #[must_use]
    pub fn view_count(&self) -> usize {
        self.views.len()
    }

    /// Close the session and its window (Python `close`).
    pub fn close(&mut self) {
        self.closed = true;
        self.window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn settings_json(value: serde_json::Value) -> BufferSettings {
        let mut config = Config::default();
        if let serde_json::Value::Object(map) = value {
            for (key, val) in map {
                config.set_value(&key, val);
            }
        }
        BufferSettings::from_config(&config)
    }

    fn manifest_for(segments: &[(u64, f64)], endlist: bool) -> String {
        use std::fmt::Write as _;
        let mut text = format!("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:{}\n", segments[0].0);
        for (seq, dur) in segments {
            let _ = write!(text, "#EXTINF:{dur},\nseg-{seq:09}.ts\n");
        }
        if endlist {
            text += "#EXT-X-ENDLIST\n";
        }
        text
    }

    /// Feed one complete segment of `chunks` 1-KiB pieces.
    fn feed(window: &mut BufferWindow, seq: u64, chunks: u64) {
        let name = format!("seg-{seq:09}.ts");
        window.begin_segment(&name).unwrap();
        for _ in 0..chunks {
            window.append_chunk(1024).unwrap();
        }
        window.finish_segment().unwrap();
    }

    #[test]
    fn settings_defaults_and_clamps_match_python() {
        let settings = settings_json(json!({}));
        assert_eq!(
            settings,
            BufferSettings {
                enabled: false,
                seconds: 600,
                mib: 512
            }
        );
        let settings = settings_json(json!({
            "live_buffer_enabled": true,
            "live_buffer_seconds": 99999,
            "live_buffer_mib": 1,
        }));
        assert!(settings.enabled);
        assert_eq!(settings.seconds, 1800);
        assert_eq!(settings.mib, 64);
        // Python: only a real int counts, and only `True` enables.
        let settings = settings_json(json!({
            "live_buffer_enabled": 1,
            "live_buffer_seconds": 900.0,
            "live_buffer_mib": "512",
        }));
        assert_eq!(settings, BufferSettings::default());
    }

    #[test]
    fn dialog_choice_enables_and_disables() {
        let mut settings = BufferSettings::default();
        assert_eq!(settings.dialog_minutes(), 0);
        settings.apply_dialog_choice(10, 256);
        assert!(settings.enabled);
        assert_eq!(settings.seconds, 600);
        assert_eq!(settings.mib, 256);
        assert_eq!(settings.dialog_minutes(), 10);
        settings.apply_dialog_choice(0, 4096);
        assert!(!settings.enabled);
        assert_eq!(settings.seconds, 600, "disabling keeps retention");
        let live = EngageContext {
            builtin_player: true,
            casting: false,
            live_stream: true,
            channel_is_movie: false,
        };
        assert!(!wants_buffer(&settings, live));
        settings.apply_dialog_choice(5, 512);
        assert!(wants_buffer(&settings, live));
        assert!(
            !wants_buffer(
                &settings,
                EngageContext {
                    casting: true,
                    ..live
                }
            ),
            "casting"
        );
        assert!(
            !wants_buffer(
                &settings,
                EngageContext {
                    live_stream: false,
                    ..live
                }
            ),
            "not live"
        );
        assert!(
            !wants_buffer(
                &settings,
                EngageContext {
                    channel_is_movie: true,
                    ..live
                }
            ),
            "movie"
        );
        assert!(
            !wants_buffer(
                &settings,
                EngageContext {
                    builtin_player: false,
                    ..live
                }
            ),
            "external"
        );
    }

    #[test]
    fn size_cap_evicts_oldest_completed_segments() {
        // 128 KiB quota; each segment is 32 KiB of 1 KiB chunks.
        let mut window = BufferWindow::new(128 * 1024, 3600.0).unwrap();
        for seq in 0..4 {
            feed(&mut window, seq, 32);
        }
        assert_eq!(window.used_bytes(), 128 * 1024);
        // The next chunk must evict segment 0 before it fits.
        window.begin_segment("seg-000000004.ts").unwrap();
        window.append_chunk(1024).unwrap();
        assert_eq!(window.file_count(), 3);
        assert_eq!(window.used_bytes(), 96 * 1024 + 1024);
        window.finish_segment().unwrap();
        assert!(window
            .publish(&manifest_for(
                &[(1, 2.0), (2, 2.0), (3, 2.0), (4, 2.0)],
                false
            ))
            .is_ok());
        let names: Vec<&str> = window.snapshot().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "seg-000000001.ts",
                "seg-000000002.ts",
                "seg-000000003.ts",
                "seg-000000004.ts"
            ]
        );
    }

    #[test]
    fn one_segment_over_quota_is_refused() {
        let mut window = BufferWindow::new(CHUNK_BYTES, 600.0).unwrap();
        window.begin_segment("seg-000000000.ts").unwrap();
        window.append_chunk(CHUNK_BYTES).unwrap();
        assert_eq!(
            window.append_chunk(1),
            Err(TimeshiftError::SegmentExceedsQuota)
        );
        assert_eq!(
            window.append_chunk(CHUNK_BYTES + 1),
            Err(TimeshiftError::OversizedWrite)
        );
    }

    #[test]
    fn time_cap_evicts_segments_at_or_before_cutoff() {
        // Retention 10 s; six 2-second segments span 0..12.
        let mut window = BufferWindow::new(512 * 1024 * 1024, 10.0).unwrap();
        for seq in 0..6 {
            feed(&mut window, seq, 1);
        }
        let rows: Vec<(u64, f64)> = (0..6).map(|s| (s, 2.0)).collect();
        window.publish(&manifest_for(&rows, false)).unwrap();
        // Cutoff = 12 - 10 = 2: only the segment ending at 2 goes.
        let starts: Vec<f64> = window.snapshot().iter().map(|s| s.start).collect();
        assert_eq!(starts.len(), 5);
        for (actual, expected) in starts.iter().zip([2.0, 4.0, 6.0, 8.0, 10.0]) {
            assert!((actual - expected).abs() < f64::EPSILON);
        }
        assert_eq!(window.file_count(), 5);
        assert!(!window.position_expired(2.0));
        assert!(window.position_expired(1.5));
    }

    #[test]
    fn publish_builds_timeline_and_republish_after_finish_extends_it() {
        let mut window = BufferWindow::new(512 * 1024 * 1024, 600.0).unwrap();
        feed(&mut window, 0, 1);
        let text = manifest_for(&[(0, 2.0), (1, 2.0)], false);
        window.publish(&text).unwrap();
        // Segment 1 is in the manifest but not on disk yet.
        assert_eq!(window.snapshot().len(), 1);
        feed(&mut window, 1, 1);
        // finish() re-applies the stored manifest.
        assert_eq!(window.snapshot().len(), 2);
        assert!((window.snapshot()[1].start - 2.0).abs() < f64::EPSILON);
        assert!(!window.is_ended());
    }

    #[test]
    fn sequence_gap_restarts_the_retained_timeline() {
        // Segment 2 is named in the manifest but never arrived on
        // disk: Python's contiguity check drops the earlier run and
        // the retained timeline restarts at segment 3 (start 6.0,
        // after three published durations).
        let mut window = BufferWindow::new(512 * 1024 * 1024, 600.0).unwrap();
        for seq in [0, 1, 3] {
            feed(&mut window, seq, 1);
        }
        window
            .publish(&manifest_for(
                &[(0, 2.0), (1, 2.0), (2, 2.0), (3, 2.0)],
                false,
            ))
            .unwrap();
        let seqs: Vec<u64> = window.snapshot().iter().map(|s| s.sequence).collect();
        assert_eq!(seqs, [3]);
        assert!((window.snapshot()[0].start - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gop_growth_latches_error_and_keeps_timeline() {
        let mut window = BufferWindow::new(512 * 1024 * 1024, 600.0).unwrap();
        feed(&mut window, 0, 1);
        window.publish(&manifest_for(&[(0, 2.0)], false)).unwrap();
        assert!(window.error().is_none());
        feed(&mut window, 1, 1);
        // ceil(9) = 9 > target (2 + 1): error, no timeline change.
        let result = window.publish(&manifest_for(&[(0, 2.0), (1, 9.0)], false));
        assert!(result.is_ok());
        assert!(window.error().is_some_and(|e| e.contains("GOP")));
        assert_eq!(window.snapshot().len(), 1);
    }

    #[test]
    fn rewind_offsets_clamp_to_the_retained_span() {
        let settings = BufferSettings {
            enabled: true,
            seconds: 600,
            mib: 512,
        };
        let mut buffer = LiveBuffer::new(&settings).unwrap();
        for seq in 0..5 {
            feed(buffer.window_mut(), seq, 1);
        }
        let rows: Vec<(u64, f64)> = (0..5).map(|s| (s, 4.0)).collect();
        buffer
            .window_mut()
            .publish(&manifest_for(&rows, false))
            .unwrap();
        assert_eq!(buffer.window().rewind_range(), Some(16));
        // Live: last segment.
        let live = buffer.cursor(0.0, false).unwrap();
        assert_eq!(live.segment.sequence, 4);
        // 10 s behind the live edge (16) targets t=6, which lives
        // in the segment starting at 4 (Python cursor semantics).
        let back = buffer.cursor(10.0, false).unwrap();
        assert_eq!(back.segment.sequence, 1);
        assert!((back.segment.start - 4.0).abs() < f64::EPSILON);
        // Beyond the span clamps to the oldest retained segment.
        let clamped = buffer.cursor(9999.0, false).unwrap();
        assert_eq!(clamped.segment.sequence, 0);
        // Negative behaves as live, like Python's max(0, behind).
        let negative = buffer.cursor(-30.0, false).unwrap();
        assert_eq!(negative.segment.sequence, 4);
        assert_eq!(
            buffer.cursor(f64::NAN, false),
            Err(TimeshiftError::InvalidPosition)
        );
    }

    #[test]
    fn cursor_without_history_and_view_caps() {
        let settings = BufferSettings {
            enabled: true,
            seconds: 600,
            mib: 512,
        };
        let mut buffer = LiveBuffer::new(&settings).unwrap();
        assert_eq!(buffer.cursor(0.0, false), Err(TimeshiftError::NoHistory));
        feed(buffer.window_mut(), 0, 1);
        buffer
            .window_mut()
            .publish(&manifest_for(&[(0, 2.0)], false))
            .unwrap();
        for _ in 0..MAX_PROTECTED_VIEWS {
            buffer.cursor(0.0, true).unwrap();
        }
        assert_eq!(
            buffer.cursor(0.0, true),
            Err(TimeshiftError::TooManyRecordings)
        );
        // Unprotected views still open and evict oldest past the cap.
        let first = buffer.cursor(0.0, false).unwrap();
        assert!(buffer.take_initial(first.view_id));
        assert!(!buffer.take_initial(first.view_id));
        for _ in 0..MAX_VIEWS {
            buffer.cursor(0.0, false).unwrap();
        }
        assert!(buffer.view_count() <= MAX_VIEWS + MAX_PROTECTED_VIEWS);
        buffer.release_view(first.view_id);
        assert!(!buffer.take_initial(first.view_id));
    }

    #[test]
    fn disable_is_refused_while_a_recording_lease_feeds() {
        let settings = BufferSettings {
            enabled: true,
            seconds: 600,
            mib: 512,
        };
        let mut buffer = LiveBuffer::new(&settings).unwrap();
        assert!(buffer.check_disable_allowed().is_ok());
        let recording = buffer.acquire().unwrap();
        assert_eq!(buffer.lease_count(), 2);
        assert_eq!(
            buffer.check_disable_allowed(),
            Err(TimeshiftError::StillFeedingRecording)
        );
        buffer.release_lease(recording);
        assert!(buffer.check_disable_allowed().is_ok());
        // Releasing the owner's lease closes the session.
        buffer.release_lease(0);
        assert!(buffer.is_closed());
        assert_eq!(buffer.acquire(), Err(TimeshiftError::SourceClosing));
        // Lease release is idempotent, as in Python.
        buffer.release_lease(0);
        assert!(buffer.is_closed());
    }
}

//! Media-type detection: audio-only (radio) vs video (port of
//! Python `media_type.py`, audit N16).
//!
//! Detection order, as in the reference: authoritative
//! playlist metadata first (handled by the caller through
//! `playback::audio_only_hint` and its tri-state app twin),
//! then a lightweight ffmpeg probe of the actual stream
//! tracks, cached per channel for the session. Group and
//! channel names are never consulted. An attached picture or
//! static cover-art track never counts as video.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::recording::RECORDING_FORMATS;
use crate::Channel;

pub const MEDIA_AUDIO: &str = "audio";
pub const MEDIA_VIDEO: &str = "video";
pub const MEDIA_UNKNOWN: &str = "unknown";

pub const SOURCE_METADATA: &str = "metadata";
pub const SOURCE_PROBE: &str = "probe";

/// One stream line of an `ffmpeg -i` report: its kind
/// ("audio", "video", ...) and the disposition flags named in
/// parentheses in the codec details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaStream {
    pub kind: String,
    pub dispositions: Vec<String>,
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every stream in an `ffmpeg -i` report (Python
/// `parse_media_streams`): lines shaped
/// `Stream #0:1[..](lang): Kind: details`.
#[must_use]
pub fn parse_media_streams(report: &str) -> Vec<MediaStream> {
    let mut streams = Vec::new();
    for line in report.lines() {
        let Some(stream) = parse_stream_line(line) else {
            continue;
        };
        streams.push(stream);
    }
    streams
}

fn parse_stream_line(line: &str) -> Option<MediaStream> {
    let s = line.trim_start();
    let mut rest = s.strip_prefix("Stream #")?;
    // \d+ : \d+
    let digits_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    rest = &rest[digits_end..];
    rest = rest.strip_prefix(':')?;
    let digits_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    rest = &rest[digits_end..];
    // Optional [..] program/stream id bracket.
    if let Some(stripped) = rest.strip_prefix('[') {
        let close = stripped.find(']')?;
        rest = &stripped[close + 1..];
    }
    // Optional (language) group.
    if let Some(stripped) = rest.strip_prefix('(') {
        let close = stripped.find(')')?;
        rest = &stripped[close + 1..];
    }
    rest = rest.strip_prefix(": ")?;
    // Kind word.
    let kind_end = rest.find(|c: char| !is_word_char(c))?;
    if kind_end == 0 {
        return None;
    }
    let kind = rest[..kind_end].to_lowercase();
    let details = rest[kind_end..].strip_prefix(": ")?;
    Some(MediaStream {
        kind,
        dispositions: disposition_flags(details),
    })
}

/// The `(flag)` groups in a stream's detail text, Python's
/// `\(([a-z][a-z ]*)\)` findall over the lowercased text.
fn disposition_flags(details: &str) -> Vec<String> {
    let lower = details.to_lowercase();
    let bytes = lower.as_bytes();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j] == b' ') {
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b')' && bytes[start].is_ascii_lowercase()
            {
                flags.push(lower[start..j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    flags
}

/// "audio" / "video" / "unknown" from probed stream kinds
/// (Python `classify_media_streams`): a video track flagged
/// `attached pic` (cover art on a radio stream) does not make
/// the stream video-capable.
#[must_use]
pub fn classify_media_streams(streams: &[MediaStream]) -> &'static str {
    let has_video = streams.iter().any(|stream| {
        stream.kind == "video"
            && !stream
                .dispositions
                .iter()
                .any(|flag| flag == "attached pic")
    });
    let has_audio = streams.iter().any(|stream| stream.kind == "audio");
    if has_video {
        MEDIA_VIDEO
    } else if has_audio {
        MEDIA_AUDIO
    } else {
        MEDIA_UNKNOWN
    }
}

/// Classify an `ffmpeg -i` stderr report (Python
/// `classify_probe_report`).
#[must_use]
pub fn classify_probe_report(report: &str) -> &'static str {
    classify_media_streams(&parse_media_streams(report))
}

/// Session media-type cache (Python `MediaTypeCache`): keyed
/// by stable channel identity, valid only while the entry's
/// URL is unchanged — a changed URL invalidates it, which
/// also covers playlist refreshes that rewrite entries.
#[derive(Default)]
pub struct MediaTypeCache {
    entries: Mutex<HashMap<String, (String, String, String)>>,
}

impl MediaTypeCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `(media, source)` or None on a miss or a changed URL.
    #[must_use]
    pub fn lookup(&self, identity: &str, url: &str) -> Option<(String, String)> {
        if identity.is_empty() {
            return None;
        }
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (stored_url, media, source) = entries.get(identity)?;
        if stored_url != url {
            return None;
        }
        Some((media.clone(), source.clone()))
    }

    pub fn store(&self, identity: &str, url: &str, media: &str, source: &str) {
        if identity.is_empty() || (media != MEDIA_AUDIO && media != MEDIA_VIDEO) {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.insert(
            identity.to_string(),
            (url.to_string(), media.to_string(), source.to_string()),
        );
    }

    pub fn invalidate(&self, identity: &str) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.remove(identity);
    }

    pub fn clear(&self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.clear();
    }
}

/// Preset keys the Recording Format menu offers for a media
/// kind (Python `formats_for_media`): audio-only streams get
/// the audio presets only; video and unclassified streams
/// keep the full list, so audio presets stay available as a
/// deliberate soundtrack-only choice.
#[must_use]
pub fn formats_for_media(media: &str) -> Vec<&'static str> {
    if media == MEDIA_AUDIO {
        RECORDING_FORMATS
            .iter()
            .filter(|entry| entry.3 == "audio")
            .map(|entry| entry.0)
            .collect()
    } else {
        RECORDING_FORMATS.iter().map(|entry| entry.0).collect()
    }
}

/// Stable channel identity (Python `_channel_record_key`):
/// the non-empty parts joined with `|`, in order — provider
/// type, provider id, stream id, URL, provider data, name.
/// The resolved stream URL can change per resolve; the
/// provider parts are what make the identity stable.
#[must_use]
pub fn channel_record_key(channel: &Channel) -> String {
    let attr = |keys: &[&str]| -> String {
        for key in keys {
            if let Some(value) = channel.unknown_attributes.get(*key) {
                if !value.is_empty() {
                    return value.clone();
                }
            }
        }
        String::new()
    };
    let name = if channel.name.is_empty() {
        channel.tvg_name.clone()
    } else {
        channel.name.clone()
    };
    let parts = [
        attr(&["provider-type"]),
        attr(&["provider-id"]),
        attr(&["stream-id", "stream_id"]),
        channel.url.clone(),
        attr(&["provider-data"]),
        name,
    ];
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TV_REPORT: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x':\n  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p, 1920x1080, 2000 kb/s, 25 fps\n  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 128 kb/s\n";

    const RADIO_REPORT: &str = "Input #0, mp3, from 'x':\n  Stream #0:0: Audio: mp3, 44100 Hz, stereo, fltp, 192 kb/s\n  Stream #0:1: Video: mjpeg, yuvj420p(pc), 90x90 [SAR 1:1 DAR 1:1], 90k tbr, 90k tbn (attached pic)\n";

    #[test]
    fn classifies_tv_and_radio_reports() {
        assert_eq!(classify_probe_report(TV_REPORT), MEDIA_VIDEO);
        // Cover art does not make a radio stream video.
        assert_eq!(classify_probe_report(RADIO_REPORT), MEDIA_AUDIO);
        assert_eq!(classify_probe_report(""), MEDIA_UNKNOWN);
        assert_eq!(
            classify_probe_report("  Stream #0:0: Subtitle: subrip\n"),
            MEDIA_UNKNOWN
        );
    }

    #[test]
    fn parses_stream_lines_like_python() {
        let streams = parse_media_streams(TV_REPORT);
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].kind, "video");
        assert_eq!(streams[1].kind, "audio");
        let radio = parse_media_streams(RADIO_REPORT);
        assert!(radio[1]
            .dispositions
            .iter()
            .any(|flag| flag == "attached pic"));
        // Not stream lines at all.
        assert!(parse_media_streams("Duration: 00:01:00.00, start: 0.0\n").is_empty());
    }

    #[test]
    fn cache_is_url_scoped_and_stores_only_classified() {
        let cache = MediaTypeCache::new();
        cache.store("id1", "http://a/1", MEDIA_AUDIO, SOURCE_PROBE);
        assert_eq!(
            cache.lookup("id1", "http://a/1"),
            Some((MEDIA_AUDIO.to_string(), SOURCE_PROBE.to_string()))
        );
        // A changed URL invalidates the entry.
        assert_eq!(cache.lookup("id1", "http://a/2"), None);
        // Unknown results are never stored.
        cache.store("id2", "http://a/1", MEDIA_UNKNOWN, SOURCE_PROBE);
        assert_eq!(cache.lookup("id2", "http://a/1"), None);
        cache.invalidate("id1");
        assert_eq!(cache.lookup("id1", "http://a/1"), None);
        cache.store("id1", "http://a/1", MEDIA_VIDEO, SOURCE_PROBE);
        cache.clear();
        assert_eq!(cache.lookup("id1", "http://a/1"), None);
        assert_eq!(cache.lookup("", "http://a/1"), None);
    }

    #[test]
    fn formats_follow_the_media_kind() {
        let audio = formats_for_media(MEDIA_AUDIO);
        assert!(audio.contains(&"provider_mka"));
        assert!(audio.contains(&"audio_mp3_v0"));
        assert!(!audio.contains(&"provider_mkv"));
        assert_eq!(
            formats_for_media(MEDIA_VIDEO).len(),
            RECORDING_FORMATS.len()
        );
        assert_eq!(
            formats_for_media(MEDIA_UNKNOWN).len(),
            RECORDING_FORMATS.len()
        );
    }

    #[test]
    fn record_key_joins_non_empty_parts_in_order() {
        let mut channel = Channel {
            name: "One".to_string(),
            group: String::new(),
            url: "http://h/1".to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::default(),
        };
        assert_eq!(channel_record_key(&channel), "http://h/1|One");
        channel
            .unknown_attributes
            .insert("provider-type".to_string(), "xtream".to_string());
        channel
            .unknown_attributes
            .insert("stream-id".to_string(), "42".to_string());
        assert_eq!(channel_record_key(&channel), "xtream|42|http://h/1|One");
    }
}

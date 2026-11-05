//! wx-free domain types and playlist behavior for the Rust port.

/// Lowercase hex for digest bytes (sha2 0.11's output array has no
/// `LowerHex`, and clippy rejects `format!` in an iterator collect).
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

pub mod applog;
pub mod audiotracks;
pub mod bookmarks;
pub mod browser;
pub mod cache;
pub mod cast;
pub mod catchup;
pub mod catchup_direct;
pub mod categories;
pub mod config;
pub mod downloads;
pub mod dvr;
pub mod epg;
pub mod epgstore;
pub mod exit_guard;
pub mod favorites;
pub mod guide;
pub mod guide_match;
mod guide_match_data;
pub mod i18n;
pub mod instance;
pub mod loading;
pub mod navindex;
pub mod playback;
pub mod providers;
pub mod proxy;
pub mod recording;
pub mod recording_log;
pub mod reminders;
pub mod search;
pub mod settings_backup;
pub mod shortcuts;
pub mod sources;
pub mod speech;
pub mod subtitles;
pub mod timeshift;
pub mod vod;

use std::collections::BTreeMap;
use std::path::Path;

/// A channel as it appears in a playlist.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Channel {
    pub name: String,
    pub group: String,
    pub url: String,
    pub tvg_id: String,
    pub tvg_name: String,
    pub tvg_logo: String,
    pub unknown_attributes: BTreeMap<String, String>,
}

/// Widget-independent local playlist browse state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowseModel {
    channels: Vec<Channel>,
    malformed: usize,
}

impl BrowseModel {
    /// Read a local UTF-8 playlist. Call from a worker, never the UI thread.
    ///
    /// # Errors
    /// Returns a file read error without replacing existing state. Invalid UTF-8 bytes
    /// are ignored, matching the Python local-file loader.
    pub fn open_local(path: &Path) -> std::io::Result<Self> {
        loading::read_local(path, || true)?.ok_or_else(|| std::io::Error::other("Load cancelled"))
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self {
            channels: Vec::new(),
            malformed: 0,
        }
    }

    #[must_use]
    pub const fn malformed_entries(&self) -> usize {
        self.malformed
    }

    pub(crate) fn from_parts(channels: Vec<Channel>, malformed: usize) -> Self {
        Self {
            channels,
            malformed,
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<Channel>, usize) {
        (self.channels, self.malformed)
    }

    #[must_use]
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }
}

/// Parse the common M3U and M3U Plus channel fields used by the Python client.
/// Unknown attributes are retained so future parity work does not require
/// reparsing the original source.
#[must_use]
pub fn parse_m3u(text: &str) -> Vec<Channel> {
    parse_cancellable(text, || true)
        .unwrap_or_default()
        .channels
}

fn parse_cancellable(text: &str, current: impl Fn() -> bool) -> Option<BrowseModel> {
    let mut pending = PendingChannel::default();
    let mut channels = Vec::new();
    let mut malformed = 0;
    let mut has_pending = false;
    let mut pending_malformed = false;
    // Playlist-wide catch-up clock from the #EXTM3U header (B08):
    // Dispatcharr sends catchup-timezone="UTC" and its archive expects
    // UTC start times.
    let mut catchup_timezone = String::new();
    // Python splitlines accepts CR-only and Unicode line boundaries too.
    for (index, raw_line) in text
        .trim_start_matches('\u{feff}')
        .split([
            '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
        ])
        .enumerate()
    {
        if index % 256 == 0 && !current() {
            return None;
        }
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTM3U"))
        {
            let header = parse_attributes(&line[7.min(line.len())..]);
            if let Some(zone) = header.get("catchup-timezone") {
                zone.trim().clone_into(&mut catchup_timezone);
            }
        } else if line
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTINF"))
        {
            if has_pending && !pending_malformed {
                malformed += 1;
            }
            pending_malformed = malformed_extinf(line);
            if pending_malformed {
                malformed += 1;
            }
            pending = PendingChannel::from_extinf(line);
            has_pending = true;
        } else if line
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTVLCOPT"))
        {
            // B07: VLC option directives carry stream headers and
            // catch-up fields the EXTINF line may not have.
            if let Some((key, value)) = directive_pair(line) {
                apply_vlcopt(&mut pending, &key, value);
            }
        } else if line
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#KODIPROP"))
        {
            // B07: Kodi properties carry the same class of metadata.
            if let Some((key, value)) = directive_pair(line) {
                apply_kodiprop(&mut pending, &key, value);
            }
        } else if !line.starts_with('#') {
            channels.push(pending.finish(line, &catchup_timezone));
            pending = PendingChannel::default();
            has_pending = false;
        }
    }

    current().then_some(BrowseModel {
        channels,
        malformed: malformed + usize::from(has_pending && !pending_malformed),
    })
}

/// Search channels using the Python reference's exact-name, word-start, then
/// general-match ranking. Source indexes preserve playlist order for ties.
#[must_use]
pub fn search_channels<'a>(channels: &'a [Channel], query: &str) -> Vec<&'a Channel> {
    search::matching_rows(channels, query, || true)
        .unwrap_or_default()
        .into_iter()
        .map(|index| &channels[index])
        .collect()
}

fn search_rank(name: &str, query: &str) -> (u8, usize) {
    let name = name.trim();
    if name == query {
        return (0, 0);
    }
    let word_start = name.char_indices().any(|(index, _)| {
        (index == 0 || !name.as_bytes()[index - 1].is_ascii_alphanumeric())
            && name[index..].starts_with(query)
    });
    (if word_start { 1 } else { 2 }, name.chars().count())
}

#[derive(Default)]
struct PendingChannel {
    name: String,
    attributes: BTreeMap<String, String>,
    /// Header/catch-up fields supplied by `#EXTVLCOPT` / `#KODIPROP`
    /// directives (B07); they overlay the EXTINF attributes, as in
    /// Python where the directive assigns the same per-channel fields.
    directive: BTreeMap<String, String>,
    /// Extra raw headers from `http-header*` VLC options, in order.
    extra_headers: Vec<String>,
}

impl PendingChannel {
    fn from_extinf(line: &str) -> Self {
        let mut pending = Self::default();
        let comma = name_separator(line);
        let metadata = comma.map_or(line, |index| &line[..index]);
        pending.name = comma.map_or_else(String::new, |index| line[index + 1..].trim().to_owned());
        let attributes = metadata
            .split_once(':')
            .map_or("", |(_, attributes)| attributes);
        pending.attributes = parse_attributes(attributes);
        pending
    }

    fn finish(self, url: &str, catchup_timezone: &str) -> Channel {
        let mut unknown_attributes = self.attributes;
        for (key, value) in self.directive {
            unknown_attributes.insert(key, value);
        }
        if !self.extra_headers.is_empty() {
            // Preserve order, drop duplicates by header name
            // (case-insensitive), like the Python parser.
            let mut seen = std::collections::HashSet::new();
            let mut unique = Vec::new();
            for header in &self.extra_headers {
                let name = header
                    .split_once(':')
                    .map_or(header.as_str(), |(name, _)| name)
                    .trim()
                    .to_lowercase();
                if seen.insert(name) {
                    unique.push(header.clone());
                }
            }
            unknown_attributes.insert("http-headers".to_string(), unique.join("\n"));
        }
        let group = unknown_attributes.remove("group-title").unwrap_or_default();
        let tvg_id = unknown_attributes.remove("tvg-id").unwrap_or_default();
        let tvg_name = unknown_attributes.remove("tvg-name").unwrap_or_default();
        let tvg_logo = unknown_attributes.remove("tvg-logo").unwrap_or_default();
        // B08: a playlist advertising a catch-up clock is a Dispatcharr
        // restream; mark every channel, and give archive-capable ones
        // the playlist's clock.
        if !catchup_timezone.is_empty() {
            unknown_attributes.insert("dispatcharr".to_string(), "true".to_string());
            if unknown_attributes.contains_key("catchup")
                || unknown_attributes.contains_key("catchup-source")
            {
                unknown_attributes
                    .insert("catchup-timezone".to_string(), catchup_timezone.to_owned());
            }
        }
        // B10: Xtream-shaped URLs (and any with a catch-up source)
        // carry their stream id in the path; favorite keys include it.
        let provider_is_xtream = unknown_attributes
            .get("provider-type")
            .is_some_and(|kind| kind == "xtream");
        if provider_is_xtream || unknown_attributes.contains_key("catchup-source") {
            if let Some(stream_id) = extract_stream_id(url) {
                unknown_attributes
                    .entry("stream-id".to_string())
                    .or_insert(stream_id);
            }
        }
        Channel {
            name: self.name,
            group,
            url: url.to_owned(),
            tvg_id,
            tvg_name,
            tvg_logo,
            unknown_attributes,
        }
    }
}

/// The `key=value` payload of a `#EXTVLCOPT:` / `#KODIPROP:` line:
/// text after the first colon, split at the first equals sign.
fn directive_pair(line: &str) -> Option<(String, String)> {
    let (_, data) = line.split_once(':')?;
    let (key, value) = data.split_once('=')?;
    Some((key.trim().to_lowercase(), value.trim().to_owned()))
}

/// Apply one `#EXTVLCOPT` key to the pending channel (Python's mapping).
fn apply_vlcopt(pending: &mut PendingChannel, key: &str, value: String) {
    let canonical = match key {
        "catchup-source" | "catchup_url" => Some("catchup-source"),
        "catchup-days" => Some("catchup-days"),
        "catchup-type" => Some("catchup-type"),
        "http-user-agent" => Some("http-user-agent"),
        "http-referrer" | "http-referer" | "referer" | "referrer" => Some("http-referrer"),
        "http-origin" | "origin" => Some("http-origin"),
        "http-cookie" | "cookie" => Some("http-cookie"),
        "http-authorization" | "authorization" | "auth" => Some("http-authorization"),
        "http-accept" | "accept" => Some("http-accept"),
        _ => None,
    };
    if let Some(canonical) = canonical {
        pending.directive.insert(canonical.to_string(), value);
    } else if key.starts_with("http-header") && !value.is_empty() {
        pending.extra_headers.push(value);
    }
}

/// Apply one `#KODIPROP` key to the pending channel (Python's mapping).
fn apply_kodiprop(pending: &mut PendingChannel, key: &str, value: String) {
    let canonical = if key.ends_with("catchup_days") {
        Some("catchup-days")
    } else if key.ends_with("catchup_source") {
        Some("catchup-source")
    } else {
        match key {
            "http-referrer" | "http-referer" | "referer" | "referrer" => Some("http-referrer"),
            "http-origin" | "origin" => Some("http-origin"),
            "http-cookie" | "cookie" => Some("http-cookie"),
            "http-authorization" | "authorization" | "auth" => Some("http-authorization"),
            "http-accept" | "accept" => Some("http-accept"),
            _ => None,
        }
    };
    if let Some(canonical) = canonical {
        pending.directive.insert(canonical.to_string(), value);
    }
}

/// Python `_extract_stream_id`: the leading digits of the last path
/// segment, else of the second-to-last (Xtream `/live/user/pass/123.ts`
/// and `/movie/user/pass/123.mp4` shapes).
#[must_use]
pub fn extract_stream_id(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = path
        .split_once("://")
        .map_or(path, |(_, rest)| match rest.find('/') {
            Some(index) => &rest[index..],
            None => "",
        });
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let leading_digits = |segment: &str| -> Option<String> {
        let digits: String = segment.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            None
        } else {
            Some(digits)
        }
    };
    parts
        .last()
        .and_then(|last| leading_digits(last))
        .or_else(|| parts.iter().rev().nth(1).and_then(|s| leading_digits(s)))
}

/// Guide URLs a playlist advertises in its `#EXTM3U` header (B09,
/// Python `playlist_headers._playlist_header_epg_urls`): `x-tvg-url`
/// / `url-tvg`, comma-separated, http(s) only, deduplicated, scheme
/// lowercased. Only the first line is read.
#[must_use]
pub fn playlist_header_epg_urls(text: &str) -> Vec<String> {
    let head = text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let first = head.lines().next().unwrap_or("").trim();
    if !first
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTM3U"))
    {
        return Vec::new();
    }
    let mut urls: Vec<String> = Vec::new();
    // The attribute scanner ends unquoted values at a comma, but an
    // unquoted guide list runs to the next whitespace; handle the two
    // header attributes directly instead.
    for (key, value) in header_attributes(&first[7..]) {
        if key != "x-tvg-url" && key != "url-tvg" {
            continue;
        }
        for part in value.split(',') {
            let part = part.trim();
            let Some((scheme, rest)) = part.split_once("://") else {
                continue;
            };
            let scheme = scheme.to_lowercase();
            if scheme != "http" && scheme != "https" {
                continue;
            }
            let url = format!("{scheme}://{rest}");
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

/// Header attributes with values that may run to whitespace when
/// unquoted (for `#EXTM3U` guide lists).
fn header_attributes(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !attribute_key(bytes[cursor]) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && attribute_key(bytes[cursor]) {
            cursor += 1;
        }
        let key = input[start..cursor].to_ascii_lowercase();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value = if let Some(&quote) = bytes.get(cursor).filter(|b| matches!(b, b'"' | b'\'')) {
            let value_start = cursor + 1;
            let end = bytes[value_start..]
                .iter()
                .position(|&b| b == quote)
                .map_or(bytes.len(), |pos| value_start + pos);
            cursor = (end + 1).min(bytes.len());
            input[value_start..end].to_owned()
        } else {
            let value_start = cursor;
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            input[value_start..cursor].to_owned()
        };
        out.push((key, value));
    }
    out
}

/// Merge per-source playlist-header guides after a load (B09, Python
/// `_merge_playlist_header_guides`): a source read in this load
/// replaces its own entry (even with none); a source not read keeps
/// its previous entry; a source no longer configured loses it.
/// Returns (guides per source, flat deduplicated guide list).
#[must_use]
pub fn merge_playlist_header_guides(
    previous: &std::collections::BTreeMap<String, Vec<String>>,
    fetched: &std::collections::BTreeMap<String, Vec<String>>,
    playlist_sources: &[String],
) -> (std::collections::BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut merged: std::collections::BTreeMap<String, Vec<String>> = previous.clone();
    for (source, guides) in fetched {
        merged.insert(source.clone(), guides.clone());
    }
    let mut by_source: std::collections::BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in playlist_sources {
        if let Some(guides) = merged.get(source) {
            by_source.insert(source.clone(), guides.clone());
        }
    }
    let mut flat: Vec<String> = Vec::new();
    for guides in by_source.values() {
        for guide in guides {
            if !flat.contains(guide) {
                flat.push(guide.clone());
            }
        }
    }
    (by_source, flat)
}

fn malformed_extinf(line: &str) -> bool {
    let Some(comma) = name_separator(line) else {
        return true;
    };
    let mut quote = None;
    for character in line[..comma].chars() {
        if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        }
    }
    !line
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTINF:"))
        || quote.is_some()
}

fn name_separator(line: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        if quote == Some(character) {
            quote = None;
        } else if quote.is_none() {
            match character {
                '\'' | '"' => quote = Some(character),
                ',' => return Some(index),
                _ => {}
            }
        }
    }
    // Match Python's recovery for an unterminated quoted attribute.
    line.find(',')
}

fn parse_attributes(input: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let mut cursor = 0;
    let bytes = input.as_bytes();
    while cursor < bytes.len() {
        if !attribute_key(bytes[cursor]) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && attribute_key(bytes[cursor]) {
            cursor += 1;
        }
        let key = input[start..cursor].to_ascii_lowercase();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        let quoted = bytes
            .get(cursor)
            .copied()
            .filter(|b| matches!(b, b'"' | b'\''))
            .and_then(|quote| bytes[cursor + 1..].iter().position(|&b| b == quote));
        let value = if let Some(end) = quoted {
            cursor += end + 2;
            &input[start + 1..cursor - 1]
        } else {
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !matches!(bytes[cursor], b'"' | b',')
            {
                cursor += 1;
            }
            if cursor == start {
                continue;
            }
            &input[start..cursor]
        };
        attributes
            .entry(key)
            .or_insert_with(|| value.trim().to_owned());
    }

    attributes
}

fn attribute_key(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::{parse_m3u, search_channels, BrowseModel};

    #[test]
    fn starts_with_an_empty_browse_model() {
        assert!(BrowseModel::empty().channels().is_empty());
    }

    #[test]
    fn parses_common_plus_fields_and_keeps_unknown_attributes() {
        let channels = parse_m3u(
            "#EXTM3U\n#EXTINF:10 tvg-id=\"news\" tvg-name=\"News\" tvg-logo=\"logo.png\" group-title=\"World\" custom=\"kept\",News HD\nhttps://example.invalid/news\n",
        );
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "News HD");
        assert_eq!(channels[0].group, "World");
        assert_eq!(channels[0].tvg_id, "news");
        assert_eq!(channels[0].tvg_name, "News");
        assert_eq!(channels[0].tvg_logo, "logo.png");
        assert_eq!(channels[0].unknown_attributes["custom"], "kept");
    }

    #[test]
    fn ranks_exact_name_before_word_start_and_general_matches() {
        let channels = parse_m3u(
            "#EXTINF:-1,The Challenge\na\n#EXTINF:-1,Challenge\nb\n#EXTINF:-1,PEACOCK Challenge event\nc\n",
        );
        let results = search_channels(&channels, "challenge");
        assert_eq!(results[0].name, "Challenge");
        assert_eq!(results[1].name, "The Challenge");
        assert_eq!(results[2].name, "PEACOCK Challenge event");
    }

    #[test]
    fn diagnostics_accept_mixed_case_directives_and_quotes_in_display_names() {
        let model = super::parse_cancellable(
            "#ExTiNf:-1 group-title='News',Bob's \"News\"\nurl\n",
            || true,
        )
        .unwrap();
        assert_eq!(model.malformed_entries(), 0);
        assert_eq!(model.channels()[0].name, "Bob's \"News\"");
    }

    #[test]
    fn vlcopt_and_kodiprop_directives_supply_headers_and_catchup() {
        // B07 regression: metadata supplied only through directives
        // must reach the channel.
        let channels = parse_m3u(
            "#EXTM3U\n\
             #EXTINF:-1 group-title=\"News\",Guarded\n\
             #EXTVLCOPT:http-user-agent=Agent/2.0\n\
             #EXTVLCOPT:http-referrer=http://ref.example/\n\
             #EXTVLCOPT:http-header=X-Token: abc\n\
             #EXTVLCOPT:http-header=X-Token: dup\n\
             #EXTVLCOPT:catchup-days=3\n\
             #KODIPROP:inputstream.adaptive.catchup_source=http://archive.example/{utc}\n\
             http://stream.example/1\n",
        );
        let attrs = &channels[0].unknown_attributes;
        assert_eq!(attrs["http-user-agent"], "Agent/2.0");
        assert_eq!(attrs["http-referrer"], "http://ref.example/");
        assert_eq!(attrs["http-headers"], "X-Token: abc");
        assert_eq!(attrs["catchup-days"], "3");
        assert_eq!(attrs["catchup-source"], "http://archive.example/{utc}");
    }

    #[test]
    fn extm3u_catchup_timezone_marks_dispatcharr_channels() {
        // B08 regression: the playlist-wide clock marks channels as
        // Dispatcharr and reaches archive-capable entries.
        let channels = parse_m3u(
            "#EXTM3U catchup-timezone=\"UTC\"\n\
             #EXTINF:-1 catchup=\"default\" catchup-source=\"http://a/{utc}\",Archive\n\
             http://stream.example/1\n\
             #EXTINF:-1,Plain\n\
             http://stream.example/2\n",
        );
        assert_eq!(channels[0].unknown_attributes["dispatcharr"], "true");
        assert_eq!(channels[0].unknown_attributes["catchup-timezone"], "UTC");
        assert_eq!(channels[1].unknown_attributes["dispatcharr"], "true");
        assert!(!channels[1]
            .unknown_attributes
            .contains_key("catchup-timezone"));
    }

    #[test]
    fn playlist_header_epg_urls_parse_dedupe_and_filter() {
        // B09 regression.
        let text = "#EXTM3U x-tvg-url=\"https://g.example/a.xml, https://g.example/a.xml\" url-tvg='http://g.example/b.xml' x-tvg-url-2=\"file:///etc/passwd\"\n#EXTINF:-1,X\nu\n";
        assert_eq!(
            super::playlist_header_epg_urls(text),
            ["https://g.example/a.xml", "http://g.example/b.xml"]
        );
        let unquoted = "#EXTM3U x-tvg-url=https://g.example/a.xml,https://g.example/b.xml\n";
        assert_eq!(
            super::playlist_header_epg_urls(unquoted),
            ["https://g.example/a.xml", "https://g.example/b.xml"]
        );
        assert!(super::playlist_header_epg_urls("#EXTINF:-1,X\nu\n").is_empty());
        // Merge: fetched replaces, unread keeps, removed drops.
        let previous = std::collections::BTreeMap::from([
            ("a".to_string(), vec!["old".to_string()]),
            ("b".to_string(), vec!["keep".to_string()]),
            ("gone".to_string(), vec!["lost".to_string()]),
        ]);
        let fetched =
            std::collections::BTreeMap::from([("a".to_string(), vec!["new".to_string()])]);
        let (by_source, flat) = super::merge_playlist_header_guides(
            &previous,
            &fetched,
            &["a".to_string(), "b".to_string()],
        );
        assert_eq!(by_source.len(), 2);
        assert_eq!(flat, ["new", "keep"]);
    }

    #[test]
    fn stream_id_comes_from_xtream_url_shapes() {
        // B10 regression (parser half): a catch-up source marks the
        // entry as carrying a stream id even before provider tagging.
        assert_eq!(
            super::extract_stream_id("http://h/live/user/pass/123.ts").as_deref(),
            Some("123")
        );
        assert_eq!(
            super::extract_stream_id("http://h/movie/user/pass/456.mp4").as_deref(),
            Some("456")
        );
        assert_eq!(
            super::extract_stream_id("http://h/series/user/pass/789.mkv?x=1").as_deref(),
            Some("789")
        );
        assert_eq!(super::extract_stream_id("http://h/plain/name"), None);
        let channels = parse_m3u(
            "#EXTINF:-1 catchup-source=\"http://a/{utc}\",Movie\nhttp://h/movie/u/p/42.mp4\n",
        );
        assert_eq!(channels[0].unknown_attributes["stream-id"], "42");
    }

    #[test]
    fn parses_plain_urls_without_metadata() {
        let channels = parse_m3u("\u{feff}https://example.invalid/plain\n");
        assert_eq!(channels[0].url, "https://example.invalid/plain");
        assert!(channels[0].name.is_empty());
    }
}

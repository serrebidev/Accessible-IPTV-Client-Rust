//! wx-free domain types and playlist behavior for the Rust port.

use std::collections::BTreeMap;
use std::path::Path;

/// A channel as it appears in a playlist.
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

impl BrowseModel {
    /// Read a local UTF-8 playlist. Call from a worker, never the UI thread.
    ///
    /// # Errors
    /// Returns the file read or UTF-8 decoding error without replacing existing state.
    pub fn open_local(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self {
            channels: parse_m3u(&text),
        })
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self {
            channels: Vec::new(),
        }
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
    let mut pending = PendingChannel::default();
    let mut channels = Vec::new();

    for raw_line in text.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXTINF"))
        {
            pending = PendingChannel::from_extinf(line);
        } else if !line.starts_with('#') {
            channels.push(pending.finish(line));
            pending = PendingChannel::default();
        }
    }

    channels
}

/// Search channels using the Python reference's exact-name, word-start, then
/// general-match ranking. Rust's stable sort preserves source order for ties.
#[must_use]
pub fn search_channels<'a>(channels: &'a [Channel], query: &str) -> Vec<&'a Channel> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return channels.iter().collect();
    }

    let mut matches: Vec<(usize, &Channel)> = channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| channel.name.to_lowercase().contains(&query))
        .collect();
    matches.sort_by(|(left_index, left), (right_index, right)| {
        search_rank(&left.name, &query)
            .cmp(&search_rank(&right.name, &query))
            .then(left_index.cmp(right_index))
    });
    matches.into_iter().map(|(_, channel)| channel).collect()
}

fn search_rank(name: &str, query: &str) -> (u8, usize) {
    let name = name.trim().to_lowercase();
    if name == query {
        return (0, 0);
    }
    let word_start = name
        .find(query)
        .is_some_and(|index| index == 0 || !name.as_bytes()[index - 1].is_ascii_alphanumeric());
    (if word_start { 1 } else { 2 }, name.len())
}

#[derive(Default)]
struct PendingChannel {
    name: String,
    attributes: BTreeMap<String, String>,
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

    fn finish(self, url: &str) -> Channel {
        let mut unknown_attributes = self.attributes;
        let group = unknown_attributes.remove("group-title").unwrap_or_default();
        let tvg_id = unknown_attributes.remove("tvg-id").unwrap_or_default();
        let tvg_name = unknown_attributes.remove("tvg-name").unwrap_or_default();
        let tvg_logo = unknown_attributes.remove("tvg-logo").unwrap_or_default();
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
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b',')
        {
            cursor += 1;
        }
        let key_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'=' && !bytes[cursor].is_ascii_whitespace()
        {
            cursor += 1;
        }
        if key_start == cursor {
            cursor += 1;
            continue;
        }
        let key = input[key_start..cursor].to_lowercase();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value = if cursor < bytes.len() && matches!(bytes[cursor], b'"' | b'\'') {
            let quote = bytes[cursor];
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let value = input[value_start..cursor].to_owned();
            cursor = cursor.saturating_add(1);
            value
        } else {
            let value_start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b','
            {
                cursor += 1;
            }
            input[value_start..cursor].to_owned()
        };
        attributes
            .entry(key)
            .or_insert_with(|| value.trim().to_owned());
    }

    attributes
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
    fn parses_plain_urls_without_metadata() {
        let channels = parse_m3u("\u{feff}https://example.invalid/plain\n");
        assert_eq!(channels[0].url, "https://example.invalid/plain");
        assert!(channels[0].name.is_empty());
    }
}

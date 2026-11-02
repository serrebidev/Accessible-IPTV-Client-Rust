//! wx-free domain types and playlist behavior for the Rust port.

pub mod browser;
pub mod category;
pub mod favorites;
pub mod loading;
pub mod search;

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowseModel {
    channels: Vec<Channel>,
    malformed: usize,
    categories: Vec<category::Category>,
}

impl Default for BrowseModel {
    fn default() -> Self {
        Self::empty()
    }
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
    pub fn empty() -> Self {
        Self {
            channels: Vec::new(),
            malformed: 0,
            categories: vec![category::Category::All, category::Category::Favorites],
        }
    }

    #[must_use]
    pub fn categories(&self) -> &[category::Category] {
        &self.categories
    }

    #[must_use]
    pub const fn malformed_entries(&self) -> usize {
        self.malformed
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
        } else if !line.starts_with('#') {
            channels.push(pending.finish(line));
            pending = PendingChannel::default();
            has_pending = false;
        }
    }

    let mut groups = std::collections::BTreeSet::new();
    for (index, channel) in channels.iter().enumerate() {
        if index % 256 == 0 && !current() {
            return None;
        }
        groups.insert(category::group_name(channel).to_owned());
    }
    let categories = [category::Category::All, category::Category::Favorites]
        .into_iter()
        .chain(groups.into_iter().map(category::Category::Group))
        .collect();
    current().then_some(BrowseModel {
        categories,
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
    fn parses_plain_urls_without_metadata() {
        let channels = parse_m3u("\u{feff}https://example.invalid/plain\n");
        assert_eq!(channels[0].url, "https://example.invalid/plain");
        assert!(channels[0].name.is_empty());
    }
}

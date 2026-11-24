//! Background navigation index (port of Python's `playlist_index.py`,
//! audit B19).
//!
//! Python warms this index in a background thread after a playlist is
//! published and navigation falls back to scanning while it is not
//! ready; the same law holds here. The index stores positions into
//! the catalogue it was built from — never channel copies — plus the
//! lowercased search names, per-scope channel positions and
//! per-scope group positions, so scope/category/favorite projections
//! and repeat searches stop rescanning and re-lowercasing the whole
//! catalogue on the calling thread. It is discarded whenever the
//! catalogue model is replaced.
use crate::Channel;
use std::collections::HashMap;

/// Python `playlist_index.channel_number`: the canonical
/// form of a channel number — decimal digits only (Unicode
/// decimal digits fold to ASCII), leading zeros stripped,
/// anything else empty. Even malformed very long input is
/// handled as text, never parsed to an integer.
/// The Unicode decimal-digit blocks (general category Nd):
/// each is ten contiguous codepoints, zero first. Rust's
/// `char::to_digit` covers ASCII only, while Python's
/// `str.isdecimal` accepts every Nd block.
const DECIMAL_BLOCKS: &[u32] = &[
    0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80,
    0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA60, 0xABF0,
    0xFF10, 0x104A0, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0,
    0x16B50, 0x1D7CE, 0x1E140, 0x1E2F0,
];

fn decimal_value(c: char) -> Option<u32> {
    let code = c as u32;
    DECIMAL_BLOCKS
        .iter()
        .find_map(|&start| (start..start + 10).contains(&code).then(|| code - start))
}

#[must_use]
pub fn channel_number(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| decimal_value(c).is_some()) {
        return String::new();
    }
    let digits: String = value
        .chars()
        .filter_map(|c| decimal_value(c).and_then(|d| char::from_digit(d, 10)))
        .collect();
    digits.trim_start_matches('0').to_string()
}

/// Positions and normalized names for one published catalogue.
#[derive(Debug)]
pub struct NavIndex {
    /// Lowercased channel names by catalogue position (the search
    /// scan's per-query input, computed once).
    normalized_names: Vec<String>,
    /// Channel positions per non-empty `playlist-id` scope, in
    /// catalogue order. Unscoped channels are never stored: the
    /// no-scope accessors read the catalogue directly (Python's
    /// rule — storing them would double the build for nothing).
    scope_channels: HashMap<String, Vec<u32>>,
    /// Group positions per scope: scope → groups in first-appearance
    /// order → positions. Group keys are the raw group strings; the
    /// blank-group rule is applied by callers exactly as
    /// `categories::group_name` does.
    scope_groups: HashMap<String, Vec<(String, Vec<u32>)>>,
}

impl NavIndex {
    /// Build the index over a published catalogue. Runs on a worker
    /// thread; cost is linear in the catalogue size.
    #[must_use]
    pub fn build(channels: &[Channel]) -> Self {
        let mut normalized_names = Vec::with_capacity(channels.len());
        let mut scope_channels: HashMap<String, Vec<u32>> = HashMap::new();
        let mut scope_groups: HashMap<String, Vec<(String, Vec<u32>)>> = HashMap::new();
        for (position, channel) in channels.iter().enumerate() {
            normalized_names.push(channel.name.to_lowercase());
            let Some(scope) = channel.unknown_attributes.get("playlist-id") else {
                continue;
            };
            if scope.is_empty() {
                continue;
            }
            let Ok(pos) = u32::try_from(position) else {
                continue;
            };
            scope_channels.entry(scope.clone()).or_default().push(pos);
            let groups = scope_groups.entry(scope.clone()).or_default();
            match groups.iter_mut().find(|(name, _)| name == &channel.group) {
                Some((_, positions)) => positions.push(pos),
                None => groups.push((channel.group.clone(), vec![pos])),
            }
        }
        Self {
            normalized_names,
            scope_channels,
            scope_groups,
        }
    }

    /// The precomputed lowercase search name at a catalogue position.
    #[must_use]
    pub fn normalized_name(&self, position: usize) -> Option<&str> {
        self.normalized_names.get(position).map(String::as_str)
    }

    /// Every normalized name, by catalogue position (search scans).
    #[must_use]
    pub fn normalized_names(&self) -> &[String] {
        &self.normalized_names
    }

    /// Channel positions belonging to `scope`, in catalogue order.
    #[must_use]
    pub fn scope_positions(&self, scope: &str) -> Option<&[u32]> {
        self.scope_channels.get(scope).map(Vec::as_slice)
    }

    /// Whether `position` belongs to `scope` (positions are stored
    /// in ascending order, so this is a binary search).
    #[must_use]
    pub fn in_scope(&self, scope: &str, position: usize) -> bool {
        self.scope_positions(scope).is_some_and(|positions| {
            u32::try_from(position).is_ok_and(|pos| positions.binary_search(&pos).is_ok())
        })
    }

    /// Group names present in `scope`, in first-appearance order,
    /// as raw group strings (blank means Uncategorized upstream).
    #[must_use]
    pub fn scope_group_names(&self, scope: &str) -> Vec<&str> {
        self.scope_groups
            .get(scope)
            .map_or_else(Vec::new, |groups| {
                groups.iter().map(|(name, _)| name.as_str()).collect()
            })
    }

    /// Positions of one group inside `scope`, in catalogue order.
    #[must_use]
    pub fn scope_group_positions(&self, scope: &str, group: &str) -> Option<&[u32]> {
        self.scope_groups
            .get(scope)?
            .iter()
            .find(|(name, _)| name == group)
            .map(|(_, positions)| positions.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn channel_number_canonicalizes_like_python() {
        assert_eq!(channel_number("02"), "2");
        assert_eq!(channel_number(" 7 "), "7");
        assert_eq!(channel_number("0"), "");
        assert_eq!(channel_number(""), "");
        assert_eq!(channel_number("12a"), "");
        assert_eq!(channel_number("-3"), "");
        // Arabic-Indic decimal digits fold to ASCII.
        assert_eq!(channel_number("٠٢"), "2");
        // Very long input stays text, never an integer.
        assert_eq!(channel_number(&"9".repeat(4900)).len(), 4900);
    }

    fn channel(name: &str, group: &str, scope: Option<&str>) -> Channel {
        let mut attributes = BTreeMap::new();
        if let Some(scope) = scope {
            attributes.insert("playlist-id".to_string(), scope.to_string());
        }
        Channel {
            name: name.to_string(),
            group: group.to_string(),
            url: format!("http://example.test/{name}"),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attributes,
        }
    }

    fn fixture() -> Vec<Channel> {
        vec![
            channel("Alpha News", "News", Some("a")),
            channel("beta sports", "Sports", Some("a")),
            channel("Gamma News", "News", Some("b")),
            channel("Delta", "", Some("b")),
            channel("Epsilon", "News", None),
            channel("Zeta", "News", Some("")),
        ]
    }

    #[test]
    fn scope_positions_match_a_linear_scan() {
        let channels = fixture();
        let index = NavIndex::build(&channels);
        for scope in ["a", "b"] {
            let scanned: Vec<u32> = channels
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.unknown_attributes.get("playlist-id").map(String::as_str) == Some(scope)
                })
                .map(|(i, _)| u32::try_from(i).expect("small"))
                .collect();
            assert_eq!(index.scope_positions(scope), Some(scanned.as_slice()));
            for position in 0..channels.len() {
                assert_eq!(
                    index.in_scope(scope, position),
                    scanned.contains(&u32::try_from(position).expect("small"))
                );
            }
        }
        // Unscoped and empty-scope channels are not stored (Python's rule).
        assert_eq!(index.scope_positions(""), None);
    }

    #[test]
    fn group_positions_and_names_follow_first_appearance() {
        let channels = fixture();
        let index = NavIndex::build(&channels);
        assert_eq!(index.scope_group_names("a"), vec!["News", "Sports"]);
        assert_eq!(index.scope_group_positions("a", "News"), Some(&[0][..]));
        assert_eq!(index.scope_group_positions("b", ""), Some(&[3][..]));
        assert_eq!(index.scope_group_positions("b", "Sports"), None);
    }

    #[test]
    fn normalized_names_are_lowercased_channel_names() {
        let channels = fixture();
        let index = NavIndex::build(&channels);
        assert_eq!(index.normalized_name(0), Some("alpha news"));
        assert_eq!(index.normalized_name(1), Some("beta sports"));
        assert_eq!(index.normalized_name(99), None);
    }
}

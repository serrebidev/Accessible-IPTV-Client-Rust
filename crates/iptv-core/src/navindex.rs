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

//! Python `episode_continuation`'s ordering core: conservative
//! adjacent-episode resolution. An adjacent item is only
//! offered when the current item appears exactly once in the
//! episode list, the candidate appears exactly once, and both
//! share provider, playlist and series identity — ambiguity
//! always means no navigation.

use crate::bookmarks;
use crate::Channel;

fn attr<'a>(channel: &'a Channel, key: &str) -> &'a str {
    channel
        .unknown_attributes
        .get(key)
        .map_or("", String::as_str)
}

/// Python `adjacent_item`: the episode `direction` steps from
/// `current` in `episodes` (-1 = previous, +1 = next), or None
/// when the step is not unambiguous.
#[must_use]
#[allow(
    clippy::cast_possible_wrap,   // episode counts are small
    clippy::cast_possible_truncation, // and the target is bounds-checked below
    clippy::cast_sign_loss
)]
pub fn adjacent_item(current: &Channel, episodes: &[Channel], direction: i32) -> Option<Channel> {
    if direction != -1 && direction != 1 {
        return None;
    }
    let key = bookmarks::item_key(current);
    if key.is_empty() {
        return None;
    }
    let positions: Vec<usize> = episodes
        .iter()
        .enumerate()
        .filter(|(_, item)| bookmarks::item_key(item) == key)
        .map(|(index, _)| index)
        .collect();
    if positions.len() != 1 {
        return None;
    }
    let target = positions[0] as i64 + i64::from(direction);
    if target < 0 || target as usize >= episodes.len() {
        return None;
    }
    let following = &episodes[target as usize];
    let following_key = bookmarks::item_key(following);
    if following_key.is_empty()
        || following_key == key
        || attr(following, "provider-id") != attr(current, "provider-id")
        || attr(following, "playlist-id") != attr(current, "playlist-id")
        || attr(following, "series_id") != attr(current, "series_id")
    {
        return None;
    }
    // Duplicate episode identities are ambiguous even when
    // adjacent rows differ.
    if episodes
        .iter()
        .filter(|item| bookmarks::item_key(item) == following_key)
        .count()
        != 1
    {
        return None;
    }
    Some(following.clone())
}

/// Python `following_item`: the automatic next-episode choice.
#[must_use]
pub fn following_item(current: &Channel, episodes: &[Channel]) -> Option<Channel> {
    adjacent_item(current, episodes, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An episode channel in the shape `vod::playable_item`
    /// builds: Xtream locator attrs plus the series id.
    fn episode(name: &str, series: &str, provider: &str) -> Channel {
        let mut attrs = BTreeMap::new();
        attrs.insert("provider-type".to_string(), "xtream".to_string());
        attrs.insert("provider-id".to_string(), provider.to_string());
        attrs.insert("resume-type".to_string(), "episode".to_string());
        attrs.insert("resume-id".to_string(), name.to_string());
        attrs.insert("series_id".to_string(), series.to_string());
        Channel {
            name: name.to_string(),
            group: String::new(),
            url: format!("http://x.example/series/u/p/{name}.mp4"),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs,
        }
    }

    fn series() -> Vec<Channel> {
        vec![
            episode("e1", "s1", "p1"),
            episode("e2", "s1", "p1"),
            episode("e3", "s1", "p1"),
        ]
    }

    #[test]
    fn middle_item_steps_both_ways() {
        let episodes = series();
        let current = &episodes[1];
        assert_eq!(
            adjacent_item(current, &episodes, 1).map(|c| c.name),
            Some("e3".to_string())
        );
        assert_eq!(
            adjacent_item(current, &episodes, -1).map(|c| c.name),
            Some("e1".to_string())
        );
        assert_eq!(
            following_item(current, &episodes).map(|c| c.name),
            Some("e3".to_string())
        );
    }

    #[test]
    fn boundaries_and_bad_directions_yield_none() {
        let episodes = series();
        assert!(adjacent_item(&episodes[0], &episodes, -1).is_none());
        assert!(adjacent_item(&episodes[2], &episodes, 1).is_none());
        assert!(adjacent_item(&episodes[1], &episodes, 0).is_none());
        assert!(adjacent_item(&episodes[1], &episodes, 2).is_none());
    }

    #[test]
    fn duplicated_current_item_is_ambiguous() {
        let mut episodes = series();
        let duplicate = episodes[1].clone();
        episodes.push(duplicate);
        assert!(adjacent_item(&episodes[1], &episodes, 1).is_none());
    }

    #[test]
    fn candidate_from_another_series_is_rejected() {
        let mut episodes = series();
        episodes[2] = episode("e3", "other-series", "p1");
        assert!(adjacent_item(&episodes[1], &episodes, 1).is_none());
        // And another provider is rejected too.
        let mut episodes = series();
        episodes[2] = episode("e3", "s1", "other-provider");
        assert!(adjacent_item(&episodes[1], &episodes, 1).is_none());
    }
}

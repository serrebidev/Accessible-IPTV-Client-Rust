//! Stable favorites for loaded channels, independent of widgets and storage.
use crate::Channel;
use std::collections::HashSet;

/// UI-neutral result; an unkeyable channel leaves favorites unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToggleOutcome {
    Added,
    Removed,
    Unkeyable,
}

/// Ordered storage keys with constant-time membership for playlist scans.
#[derive(Clone, Debug, Default)]
pub struct FavoriteSet {
    keys: Vec<String>,
    membership: HashSet<String>,
}

impl FavoriteSet {
    /// Normalize stored values, retaining the first occurrence of each trimmed key.
    ///
    /// A storage adapter supplies strings as `Some` and invalid values as `None`;
    /// missing or non-list configuration (including a bare string) supplies an
    /// empty iterator. No configuration format or parser is required by core.
    /// Nonblank strings are opaque keys, including unknown/old keys and escapes.
    #[must_use]
    pub fn from_stored<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Self {
        let mut favorites = Self::default();
        for key in values.into_iter().flatten().map(str::trim) {
            if !key.is_empty() && favorites.membership.insert(key.to_owned()) {
                favorites.keys.push(key.to_owned());
            }
        }
        favorites
    }

    #[must_use]
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    #[must_use]
    pub fn contains(&self, channel: &Channel) -> bool {
        channel_key(channel).is_some_and(|key| self.membership.contains(&key))
    }

    pub fn toggle(&mut self, channel: &Channel) -> ToggleOutcome {
        let Some(key) = channel_key(channel) else {
            return ToggleOutcome::Unkeyable;
        };
        if self.membership.remove(&key) {
            self.keys.retain(|existing| existing != &key);
            ToggleOutcome::Removed
        } else {
            self.membership.insert(key.clone());
            self.keys.push(key);
            ToggleOutcome::Added
        }
    }

    /// Favorited channels in current playlist order, including duplicate rows.
    ///
    /// Scans the supplied snapshot without changing search/category/recovery state.
    /// Call from a worker for large playlists; no UI scheduling is performed here.
    #[must_use]
    pub fn filter_channels<'a>(&self, channels: &'a [Channel]) -> Vec<&'a Channel> {
        if self.membership.is_empty() {
            return Vec::new();
        }
        channels
            .iter()
            .filter(|channel| self.contains(channel))
            .collect()
    }
}

/// Python-compatible identity, never derived from the stream URL or credentials.
///
/// Nonempty provider type, provider ID, stream ID, XMLTV ID and display name are
/// trimmed and joined in that order. Backslashes and pipes are escaped. Plain
/// M3U entries fall back to display name, so namesakes in different groups share
/// a favorite. Display name falls back to TVG name, then XMLTV ID. Names remain
/// case-sensitive; a rename changes identity even when provider IDs are present.
/// Missing fields are omitted, matching the pinned Python reference.
#[must_use]
pub fn channel_key(channel: &Channel) -> Option<String> {
    let attribute = |name: &str| {
        channel
            .unknown_attributes
            .get(name)
            .map_or("", String::as_str)
    };
    // Choose aliases before trimming: a whitespace-only primary field blocks
    // fallback in Python, whereas an empty primary field permits it.
    let stream_id = nonempty(attribute("stream-id"), attribute("stream_id"));
    let tvg_id = nonempty(&channel.tvg_id, attribute("tvg_id"));
    let tvg_name = nonempty(&channel.tvg_name, attribute("tvg_name"));
    let name = nonempty(&channel.name, nonempty(tvg_name, tvg_id));
    let key = [
        attribute("provider-type"),
        attribute("provider-id"),
        stream_id,
        tvg_id,
        name,
    ]
    .into_iter()
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .map(|part| part.replace('\\', "\\\\").replace('|', "\\|"))
    .collect::<Vec<_>>()
    .join("|");
    (!key.is_empty()).then_some(key)
}

fn nonempty<'a>(primary: &'a str, fallback: &'a str) -> &'a str {
    if primary.is_empty() {
        fallback
    } else {
        primary
    }
}

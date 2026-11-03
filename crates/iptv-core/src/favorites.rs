//! Widget-free stable favorite identity and favorite-set behavior.
//!
//! Observable rules ported from the Python reference `favorites.py` (pinned
//! revision recorded in `tests/fixtures/README.md`): a favorite key is built
//! from provider-stable identity fields, never from a resolved stream URL, so
//! favorites survive playlist refreshes and never persist credentials. Plain
//! M3U channels fall back to their display name.
use crate::Channel;

/// Stored sentinel for the Favorites category. Compared and persisted in this
/// exact form; localization happens only where it is displayed.
pub const FAVORITES_SENTINEL: &str = "Favorites";

/// The name a channel is listed under: display name, then tvg-name, then
/// tvg-id, matching the Python reference's precedence.
#[must_use]
pub fn display_name(channel: &Channel) -> &str {
    for value in [&channel.name, &channel.tvg_name, &channel.tvg_id] {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    ""
}

/// Stable identity for a channel, or an empty string when there is nothing
/// to key on. Parts are provider type, provider id, stream id, tvg-id and
/// display name, in that order, with blank parts dropped. The stream URL is
/// never a part, so credential-bearing or refreshed URLs cannot leak into
/// stored favorite state.
#[must_use]
pub fn channel_key(channel: &Channel) -> String {
    let attribute = |key: &str| {
        channel
            .unknown_attributes
            .get(key)
            .map_or("", |value| value.trim())
    };
    let stream_id = match attribute("stream-id") {
        "" => attribute("stream_id"),
        value => value,
    };
    [
        attribute("provider-type"),
        attribute("provider-id"),
        stream_id,
        channel.tvg_id.trim(),
        display_name(channel),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .map(escape_key_part)
    .collect::<Vec<_>>()
    .join("|")
}

/// Keep the separator out of a field: playlists do name channels "A|B".
/// Backslashes are escaped first so an escaped separator stays unambiguous.
fn escape_key_part(part: &str) -> String {
    part.replace('\\', "\\\\").replace('|', "\\|")
}

/// Coerce stored favorite values into an ordered, duplicate-free key list:
/// blank values are dropped, duplicates collapse to their first occurrence,
/// and the surviving order is preserved. Hand-edited or stale configuration
/// data therefore degrades to fewer favorites, never to an error.
#[must_use]
pub fn normalize_keys(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|key| !key.is_empty() && seen.insert(key.clone()))
        .collect()
}

/// The outcome of toggling one channel in a [`FavoriteSet`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Toggle {
    Added,
    Removed,
    /// The channel has no stable key; the set was left unchanged.
    Unchanged,
}

/// An ordered set of stable favorite keys.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FavoriteSet {
    keys: Vec<String>,
    /// Hash index over `keys` (B19): membership tests during category
    /// synchronization were O(keys) per channel, which stalls the UI
    /// thread on large playlists with many favorites. Kept in sync by
    /// every mutation; equality and storage use `keys` alone.
    index: std::collections::HashSet<String>,
}

impl FavoriteSet {
    /// Build a set from stored values, normalized defensively.
    #[must_use]
    pub fn from_stored(values: impl IntoIterator<Item = String>) -> Self {
        let keys = normalize_keys(values);
        let index = keys.iter().cloned().collect();
        Self { keys, index }
    }

    #[must_use]
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    #[must_use]
    pub fn is_favorite(&self, channel: &Channel) -> bool {
        let key = channel_key(channel);
        !key.is_empty() && self.index.contains(&key)
    }

    /// Add the channel when absent, remove it when present. Toggling the
    /// same channel twice restores the original set exactly.
    pub fn toggle(&mut self, channel: &Channel) -> Toggle {
        let key = channel_key(channel);
        if key.is_empty() {
            return Toggle::Unchanged;
        }
        if let Some(position) = self.keys.iter().position(|stored| stored == &key) {
            self.keys.remove(position);
            self.index.remove(&key);
            Toggle::Removed
        } else {
            self.index.insert(key.clone());
            self.keys.push(key);
            Toggle::Added
        }
    }

    /// Playlist indexes of favorited channels, in current playlist order.
    /// Keys with no matching channel are ignored.
    #[must_use]
    pub fn favorite_indexes(&self, channels: &[Channel]) -> Vec<usize> {
        if self.keys.is_empty() {
            return Vec::new();
        }
        channels
            .iter()
            .enumerate()
            .filter(|(_, channel)| self.is_favorite(channel))
            .map(|(index, _)| index)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_m3u;

    fn channel(text: &str) -> Channel {
        parse_m3u(text).into_iter().next().unwrap()
    }

    #[test]
    fn plain_m3u_channels_key_on_display_name_only() {
        let entry = channel("#EXTINF:-1,BBC One\nhttp://a/1\n");
        assert_eq!(channel_key(&entry), "BBC One");
        // Same name in another group shares one favorite, as in Python.
        let other_group = channel("#EXTINF:-1 group-title=\"News\",BBC One\nhttp://b/2\n");
        assert_eq!(channel_key(&other_group), "BBC One");
    }

    #[test]
    fn provider_fields_and_tvg_id_form_the_key_before_the_name() {
        let entry = channel(
            "#EXTINF:-1 provider-type=\"xtream\" provider-id=\"acct\" stream-id=\"42\" tvg-id=\"bbc.uk\",BBC One\nhttp://a/1\n",
        );
        assert_eq!(channel_key(&entry), "xtream|acct|42|bbc.uk|BBC One");
    }

    #[test]
    fn url_changes_never_change_the_key_and_credentials_never_enter_it() {
        let first = channel("#EXTINF:-1 tvg-id=\"bbc.uk\",BBC One\nhttp://user:pass@a/1\n");
        let refreshed = channel("#EXTINF:-1 tvg-id=\"bbc.uk\",BBC One\nhttp://other:secret@b/9\n");
        assert_eq!(channel_key(&first), channel_key(&refreshed));
        assert!(!channel_key(&first).contains("pass"));
        assert!(!channel_key(&first).contains("http"));
        // A changed tvg-id is a changed identity.
        let moved = channel("#EXTINF:-1 tvg-id=\"bbc.us\",BBC One\nhttp://a/1\n");
        assert_ne!(channel_key(&first), channel_key(&moved));
    }

    #[test]
    fn separators_and_backslashes_in_names_are_escaped() {
        let entry = channel("#EXTINF:-1,A|B \\ C\nhttp://a/1\n");
        assert_eq!(channel_key(&entry), "A\\|B \\\\ C");
        let plain = channel("#EXTINF:-1,A\nhttp://a/1\n");
        let piped = channel("#EXTINF:-1,A|B\nhttp://a/1\n");
        assert_ne!(channel_key(&plain), channel_key(&piped));
    }

    #[test]
    fn display_name_falls_back_to_tvg_name_then_tvg_id() {
        let named = channel("#EXTINF:-1 tvg-id=\"id1\" tvg-name=\"Tvg Name\",\nhttp://a\n");
        assert_eq!(display_name(&named), "Tvg Name");
        let id_only = channel("#EXTINF:-1 tvg-id=\"id2\",\nhttp://a\n");
        assert_eq!(display_name(&id_only), "id2");
        let bare = channel("http://a\n");
        assert_eq!(display_name(&bare), "");
        assert_eq!(channel_key(&bare), "");
    }

    #[test]
    fn normalization_drops_blanks_and_duplicates_preserving_order() {
        let stored = vec![
            "  BBC One ".to_owned(),
            String::new(),
            "   ".to_owned(),
            "CNN".to_owned(),
            "BBC One".to_owned(),
        ];
        assert_eq!(normalize_keys(stored), ["BBC One", "CNN"]);
    }

    #[test]
    fn toggle_is_exactly_once_and_unkeyable_channels_do_not_change_the_set() {
        let mut set = FavoriteSet::default();
        let entry = channel("#EXTINF:-1,BBC One\nhttp://a/1\n");
        assert_eq!(set.toggle(&entry), Toggle::Added);
        assert!(set.is_favorite(&entry));
        assert_eq!(set.toggle(&entry), Toggle::Removed);
        assert!(!set.is_favorite(&entry));
        assert!(set.is_empty());
        let bare = channel("http://a\n");
        assert_eq!(set.toggle(&bare), Toggle::Unchanged);
        assert!(set.is_empty());
    }

    #[test]
    fn membership_index_stays_in_sync_with_toggles() {
        // B19 regression: the hash index must agree with the key list
        // after every toggle, or favorites silently vanish.
        let mut set = FavoriteSet::from_stored(["Alpha".to_owned(), "Bravo".to_owned()]);
        let alpha = channel("#EXTINF:-1,Alpha\nu1\n");
        let bravo = channel("#EXTINF:-1,Bravo\nu2\n");
        assert!(set.is_favorite(&alpha));
        assert_eq!(set.toggle(&alpha), Toggle::Removed);
        assert!(!set.is_favorite(&alpha));
        assert!(set.is_favorite(&bravo));
        assert_eq!(set.toggle(&alpha), Toggle::Added);
        assert!(set.is_favorite(&alpha));
        assert_eq!(
            set,
            FavoriteSet::from_stored(["Bravo".to_owned(), "Alpha".to_owned()])
        );
    }

    #[test]
    fn filtering_uses_playlist_order_and_ignores_unknown_keys() {
        let channels =
            parse_m3u("#EXTINF:-1,Alpha\nu1\n#EXTINF:-1,Bravo\nu2\n#EXTINF:-1,Charlie\nu3\n");
        let set = FavoriteSet::from_stored([
            "Charlie".to_owned(),
            "Ghost".to_owned(),
            "Alpha".to_owned(),
        ]);
        assert_eq!(set.favorite_indexes(&channels), [0, 2]);
    }
}

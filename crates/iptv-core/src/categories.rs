//! Widget-free category projection over a loaded playlist.
//!
//! Category rules follow the Python reference: "All Channels" and
//! "Favorites" are stable sentinels (stored and compared in this exact form,
//! localized only at display), group categories come from M3U `group-title`
//! metadata in first-appearance order, and a channel with missing or blank
//! group metadata belongs to "Uncategorized". Channels stay in playlist
//! source order inside every category.
use crate::{favorites::FavoriteSet, Channel};

/// Stored sentinel for the complete playlist.
pub const ALL_CHANNELS_SENTINEL: &str = "All Channels";

/// Category assigned to channels with missing or blank group metadata.
pub const UNCATEGORIZED: &str = "Uncategorized";

/// A browsable category: two sentinels plus one variant per playlist group.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum Category {
    #[default]
    All,
    Favorites,
    Group(String),
    /// Every channel of one playlist inside the All-playlists view
    /// (Python's `("playlist-all", scope)` category key): a leaf under
    /// that playlist's branch of the category tree.
    PlaylistAll(String),
    /// One group of one playlist inside the All-playlists view
    /// (Python's `("playlist-group", scope, group)` category key).
    PlaylistGroup(String, String),
}

impl Category {
    /// The stable stored identity, distinct from any display localization.
    #[must_use]
    pub fn sentinel(&self) -> &str {
        match self {
            Self::All | Self::PlaylistAll(_) => ALL_CHANNELS_SENTINEL,
            Self::Favorites => crate::favorites::FAVORITES_SENTINEL,
            Self::Group(name) | Self::PlaylistGroup(_, name) => name,
        }
    }

    /// Display label before localization. Favorites carries no count here;
    /// adapters may decorate it, but the sentinel never changes.
    #[must_use]
    pub fn label(&self) -> &str {
        self.sentinel()
    }
}

/// The group a channel belongs to, applying the blank-group rule.
#[must_use]
pub fn group_name(channel: &Channel) -> &str {
    if channel.group.is_empty() {
        UNCATEGORIZED
    } else {
        &channel.group
    }
}

/// All Channels first, Favorites second, then each distinct group in
/// first-appearance order. The same playlist always yields the same list.
#[must_use]
pub fn categories(channels: &[Channel]) -> Vec<Category> {
    categories_from(channels.iter())
}

/// [`categories`] over borrowed channels (B19): the scope-filtered
/// variant must not clone every channel just to list group names.
#[must_use]
pub fn categories_from<'a>(channels: impl Iterator<Item = &'a Channel>) -> Vec<Category> {
    let mut list = vec![Category::All, Category::Favorites];
    let mut seen = std::collections::HashSet::new();
    for channel in channels {
        let name = group_name(channel);
        if seen.insert(name.to_owned()) {
            list.push(Category::Group(name.to_owned()));
        }
    }
    list
}

/// The playlist a channel belongs to: its `playlist-id` tag, or ""
/// for channels no configured source produced (Python's
/// `str(channel.get("playlist-id") or "")`).
#[must_use]
pub fn playlist_id(channel: &Channel) -> &str {
    channel
        .unknown_attributes
        .get(crate::sources::PLAYLIST_ID_ATTRIBUTE)
        .map_or("", String::as_str)
}

/// Playlist indexes visible in `category`, in playlist source order.
#[must_use]
pub fn category_rows(
    channels: &[Channel],
    category: &Category,
    favorites: &FavoriteSet,
) -> Vec<usize> {
    match category {
        Category::All => (0..channels.len()).collect(),
        Category::Favorites => favorites.favorite_indexes(channels),
        Category::Group(name) => channels
            .iter()
            .enumerate()
            .filter(|(_, channel)| group_name(channel) == name)
            .map(|(index, _)| index)
            .collect(),
        Category::PlaylistAll(scope) => channels
            .iter()
            .enumerate()
            .filter(|(_, channel)| playlist_id(channel) == scope)
            .map(|(index, _)| index)
            .collect(),
        Category::PlaylistGroup(scope, name) => channels
            .iter()
            .enumerate()
            .filter(|(_, channel)| playlist_id(channel) == scope && group_name(channel) == name)
            .map(|(index, _)| index)
            .collect(),
    }
}

/// The All-playlists category list (Python `_refresh_group_ui` in the
/// All scope): the two sentinels, then one branch per playlist — the
/// configured scopes in picker order, then any scope the channels
/// carry that no configured source owns (cached or legacy channels;
/// the adapter labels that branch "Other playlists"). A branch is a
/// `PlaylistAll` leaf followed by its groups, sorted, as
/// `PlaylistGroup` leaves; a scope with no channels gets no branch.
#[must_use]
pub fn branch_categories(channels: &[Channel], known_scopes: &[String]) -> Vec<Category> {
    let mut groups: std::collections::HashMap<&str, std::collections::BTreeSet<&str>> =
        std::collections::HashMap::new();
    let mut encountered: Vec<&str> = Vec::new();
    for channel in channels {
        let scope = playlist_id(channel);
        if !groups.contains_key(scope) {
            encountered.push(scope);
        }
        groups.entry(scope).or_default().insert(group_name(channel));
    }
    let mut ordered: Vec<&str> = Vec::new();
    for scope in known_scopes {
        if groups.contains_key(scope.as_str()) {
            ordered.push(scope.as_str());
        }
    }
    for scope in encountered {
        if !ordered.contains(&scope) {
            ordered.push(scope);
        }
    }
    let mut list = vec![Category::All, Category::Favorites];
    for scope in ordered {
        list.push(Category::PlaylistAll(scope.to_owned()));
        if let Some(names) = groups.get(scope) {
            for name in names {
                list.push(Category::PlaylistGroup(
                    scope.to_owned(),
                    (*name).to_owned(),
                ));
            }
        }
    }
    list
}

/// The tree path segments of a category label (Python
/// `_AccessibleCategoryTree._path_parts`): a trailing " (count)" is
/// dropped, then the label splits on " > " when present, else on "/",
/// keeping every other punctuation literal.
#[must_use]
pub fn category_path_parts(label: &str) -> Vec<String> {
    let base = strip_count_suffix(label).trim();
    let raw: Vec<&str> = if base.contains(" > ") {
        base.split(" > ").collect()
    } else {
        base.split('/').collect()
    };
    let parts: Vec<String> = raw
        .iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        vec![base.to_owned()]
    } else {
        parts
    }
}

/// A label's trailing " (digits)" count suffix, "" when absent
/// (Python's `_COUNT_SUFFIX`).
#[must_use]
pub fn count_suffix(label: &str) -> &str {
    let Some(open) = label.rfind(" (") else {
        return "";
    };
    let tail = &label[open..];
    if tail.len() < 4 || !tail.ends_with(')') {
        return "";
    }
    let digits = &tail[2..tail.len() - 1];
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        tail
    } else {
        ""
    }
}

fn strip_count_suffix(label: &str) -> &str {
    let suffix = count_suffix(label);
    &label[..label.len() - suffix.len()]
}

/// A tree leaf's text: just the last path segment, retaining its
/// channel count (Python `_leaf_text`).
#[must_use]
pub fn category_leaf_text(label: &str, last_part: &str) -> String {
    format!("{last_part}{}", count_suffix(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_m3u;

    fn sample() -> Vec<Channel> {
        parse_m3u(
            "#EXTINF:-1 group-title=\"News\",Alpha\n\
             u1\n\
             #EXTINF:-1 group-title=\"Sport\",Bravo\n\
             u2\n\
             #EXTINF:-1,Charlie\n\
             u3\n\
             #EXTINF:-1 group-title=\"News\",Delta\n\
             u4\n\
             #EXTINF:-1 group-title=\"   \",Echo\n\
             u5\n",
        )
    }

    #[test]
    fn categories_are_all_favorites_then_groups_in_first_appearance_order() {
        let channels = sample();
        let list = categories(&channels);
        let names: Vec<&str> = list.iter().map(Category::sentinel).collect();
        assert_eq!(
            names,
            [
                "All Channels",
                "Favorites",
                "News",
                "Sport",
                "Uncategorized"
            ]
        );
        // Deterministic: a second pass over the same playlist agrees.
        assert_eq!(categories(&channels), categories(&channels));
    }

    #[test]
    fn blank_and_missing_groups_share_the_uncategorized_rule() {
        let channels = sample();
        assert_eq!(group_name(&channels[2]), UNCATEGORIZED);
        assert_eq!(
            category_rows(
                &channels,
                &Category::Group(UNCATEGORIZED.to_owned()),
                &FavoriteSet::default()
            ),
            [2, 4]
        );
    }

    #[test]
    fn rows_stay_in_playlist_source_order() {
        let channels = sample();
        let favorites = FavoriteSet::default();
        assert_eq!(
            category_rows(&channels, &Category::Group("News".to_owned()), &favorites),
            [0, 3]
        );
        assert_eq!(
            category_rows(&channels, &Category::All, &favorites),
            [0, 1, 2, 3, 4]
        );
        assert!(category_rows(&channels, &Category::Favorites, &favorites).is_empty());
    }

    #[test]
    fn an_empty_playlist_has_only_the_sentinel_categories() {
        assert_eq!(categories(&[]), [Category::All, Category::Favorites]);
        assert!(category_rows(&[], &Category::All, &FavoriteSet::default()).is_empty());
    }

    fn tagged(name: &str, group: &str, scope: &str) -> Channel {
        let mut channel = Channel {
            name: name.to_string(),
            group: group.to_string(),
            url: format!("http://x/{name}"),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        };
        if !scope.is_empty() {
            channel.unknown_attributes.insert(
                crate::sources::PLAYLIST_ID_ATTRIBUTE.to_string(),
                scope.to_string(),
            );
        }
        channel
    }

    #[test]
    fn branch_categories_order_scopes_then_leftovers() {
        let channels = vec![
            tagged("A", "News", "s2"),
            tagged("B", "Sport", "s1"),
            tagged("C", "News", "s1"),
            tagged("D", "Films", ""),
            tagged("E", "", "s2"),
        ];
        let known = vec!["s1".to_string(), "s2".to_string(), "gone".to_string()];
        assert_eq!(
            branch_categories(&channels, &known),
            [
                Category::All,
                Category::Favorites,
                Category::PlaylistAll("s1".to_string()),
                Category::PlaylistGroup("s1".to_string(), "News".to_string()),
                Category::PlaylistGroup("s1".to_string(), "Sport".to_string()),
                Category::PlaylistAll("s2".to_string()),
                Category::PlaylistGroup("s2".to_string(), "News".to_string()),
                Category::PlaylistGroup("s2".to_string(), "Uncategorized".to_string()),
                Category::PlaylistAll(String::new()),
                Category::PlaylistGroup(String::new(), "Films".to_string()),
            ]
        );
        // No channels: only the sentinels, whatever is configured.
        assert_eq!(
            branch_categories(&[], &known),
            [Category::All, Category::Favorites]
        );
    }

    #[test]
    fn branch_rows_filter_by_scope_and_group() {
        let channels = vec![
            tagged("A", "News", "s1"),
            tagged("B", "News", "s2"),
            tagged("C", "Sport", "s1"),
        ];
        let favorites = FavoriteSet::default();
        assert_eq!(
            category_rows(
                &channels,
                &Category::PlaylistAll("s1".to_string()),
                &favorites
            ),
            [0, 2]
        );
        assert_eq!(
            category_rows(
                &channels,
                &Category::PlaylistGroup("s1".to_string(), "News".to_string()),
                &favorites
            ),
            [0]
        );
    }

    #[test]
    fn path_parts_split_and_strip_counts() {
        assert_eq!(category_path_parts("News"), ["News"]);
        assert_eq!(category_path_parts("News / UK"), ["News", "UK"]);
        assert_eq!(category_path_parts("News > UK (12)"), ["News", "UK"]);
        assert_eq!(category_path_parts("All Channels (300)"), ["All Channels"]);
        assert_eq!(category_path_parts("A/B > C"), ["A/B", "C"]);
        assert_eq!(category_leaf_text("News (12)", "News"), "News (12)");
        assert_eq!(category_leaf_text("News", "News"), "News");
        assert_eq!(count_suffix("News (x)"), "");
        assert_eq!(count_suffix("News (12)"), " (12)");
    }
}

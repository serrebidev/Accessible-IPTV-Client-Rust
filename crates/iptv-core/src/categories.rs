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
}

impl Category {
    /// The stable stored identity, distinct from any display localization.
    #[must_use]
    pub fn sentinel(&self) -> &str {
        match self {
            Self::All => ALL_CHANNELS_SENTINEL,
            Self::Favorites => crate::favorites::FAVORITES_SENTINEL,
            Self::Group(name) => name,
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
    }
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
}

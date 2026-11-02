//! Stable category identity is separate from translated display labels.
use crate::Channel;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Category {
    #[default]
    All,
    Favorites,
    Group(String),
}

impl Category {
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::All => "All Channels",
            Self::Favorites => "Favorites",
            Self::Group(name) => name,
        }
    }

    pub(crate) fn includes(
        &self,
        channel: &Channel,
        favorites: &crate::favorites::FavoriteSet,
    ) -> bool {
        match self {
            Self::All => true,
            Self::Favorites => favorites.contains(channel),
            Self::Group(name) => name == group_name(channel),
        }
    }
}

/// Missing, empty and whitespace-only groups join explicit Uncategorized entries.
pub(crate) fn group_name(channel: &Channel) -> &str {
    let group = channel.group.trim();
    if group.is_empty() {
        "Uncategorized"
    } else {
        group
    }
}

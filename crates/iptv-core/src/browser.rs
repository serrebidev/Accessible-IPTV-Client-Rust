//! The local browse application seam, shared by the desktop and domain tests.
use crate::{
    categories::{categories, category_rows, Category},
    favorites::{display_name, FavoriteSet, Toggle},
    loading::{LoadFailure, LoadWorker},
    search::{SearchResults, SearchWorker},
    BrowseModel,
};
use std::{path::PathBuf, sync::Arc};

pub enum BrowseEvent {
    Results(SearchResults),
    Recovery(LoadFailure),
}

/// The UI-neutral outcome of a favorite toggle, ready to announce.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavoriteToggle {
    pub toggle: Toggle,
    /// Status text for the live-region path; empty when nothing changed.
    pub announcement: String,
}

/// Coordinates load/search generations while retaining the last usable playlist.
#[derive(Default)]
pub struct BrowserWorker {
    loader: LoadWorker,
    searcher: SearchWorker,
    playlist: Arc<BrowseModel>,
    replacement: Option<Arc<BrowseModel>>,
    loading: bool,
    searching: bool,
    category: Category,
    scope: Option<String>,
    favorites: FavoriteSet,
    extras: Vec<crate::Channel>,
    raw: Option<Arc<BrowseModel>>,
}

impl BrowserWorker {
    pub fn load(&mut self, path: PathBuf) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
        self.loader.submit(path);
        self.loading = true;
    }

    /// Load several configured sources merged, with scope tags.
    pub fn load_sources(&mut self, sources: Vec<crate::sources::PlaylistSource>) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
        self.loader.submit_sources(sources);
        self.loading = true;
    }

    /// Returns whether a pending load was cancelled. Existing rows stay usable.
    pub fn cancel_load(&mut self) -> bool {
        self.loader.cancel();
        if let Some(replacement) = self.replacement.take() {
            self.searcher.cancel();
            self.searching = false;
            std::thread::spawn(move || drop(replacement));
        }
        std::mem::take(&mut self.loading)
    }

    /// A new query supersedes a pending load and searches the last usable source.
    pub fn search(&mut self, query: String) -> bool {
        let cancelled = self.cancel_load();
        self.submit_current(query);
        cancelled
    }

    /// Switch the active category and re-run the current query inside it.
    /// Like a new search, this supersedes a pending load.
    pub fn select_category(&mut self, category: Category, query: &str) -> bool {
        let cancelled = self.cancel_load();
        self.category = category;
        self.submit_current(query.to_owned());
        cancelled
    }

    #[must_use]
    pub fn active_category(&self) -> &Category {
        &self.category
    }

    /// Categories of the last usable playlist, in deterministic order.
    /// When a playlist scope is set, only that playlist's groups are
    /// listed (Python's Playlist view behavior).
    #[must_use]
    pub fn categories(&self) -> Vec<Category> {
        if self.scope.is_none() {
            return categories(self.playlist.channels());
        }
        let scoped: Vec<crate::Channel> = self
            .playlist
            .channels()
            .iter()
            .filter(|channel| self.in_scope(channel))
            .cloned()
            .collect();
        categories(&scoped)
    }

    /// The active playlist scope id, when scoped to one playlist.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Set the playlist scope (None = all playlists) and re-run the
    /// current query inside it. Returns whether a load was cancelled.
    pub fn set_scope(&mut self, scope: Option<String>, query: &str) -> bool {
        let cancelled = self.cancel_load();
        self.scope = scope;
        self.submit_current(query.to_owned());
        cancelled
    }

    fn in_scope(&self, channel: &crate::Channel) -> bool {
        match &self.scope {
            None => true,
            Some(id) => {
                channel
                    .unknown_attributes
                    .get("playlist-id")
                    .map(String::as_str)
                    == Some(id.as_str())
            }
        }
    }

    /// Drop rows whose channel is outside the playlist scope.
    /// `channels` is the slice the rows index into.
    fn scoped_rows(&self, channels: &[crate::Channel], rows: Vec<usize>) -> Vec<usize> {
        if self.scope.is_none() {
            return rows;
        }
        rows.into_iter()
            .filter(|&index| channels.get(index).is_some_and(|c| self.in_scope(c)))
            .collect()
    }

    #[must_use]
    pub fn favorites(&self) -> &FavoriteSet {
        &self.favorites
    }

    /// The browse model the published search rows index into.
    /// Extras are merged into it by `apply_model` and promoted
    /// into `playlist` when their results land, so the model's
    /// channels ARE the whole catalogue — appending `extras`
    /// again would double-count every provider channel.
    fn catalogue(&self) -> &[crate::Channel] {
        self.playlist.channels()
    }

    /// Every loaded channel (playlist plus provider extras), for
    /// catalogue builders that scan the whole browse state.
    #[must_use]
    pub fn all_channels(&self) -> Vec<crate::Channel> {
        self.catalogue().to_vec()
    }

    /// The channel at a playlist index in the current projection's source.
    #[must_use]
    pub fn channel(&self, playlist_index: usize) -> Option<crate::Channel> {
        self.playlist.channels().get(playlist_index).cloned()
    }

    /// Run `f` over every loaded channel (playlist plus provider
    /// extras, in `all_channels` order) without cloning the
    /// catalogue (audit E12 — suffix building over 500,000
    /// channels must not start by cloning all of them).
    pub fn with_all_channels<R>(&self, f: impl FnOnce(&[crate::Channel]) -> R) -> R {
        f(self.catalogue())
    }

    /// Favorited channels present in the current playlist (display counts).
    #[must_use]
    pub fn favorite_count(&self) -> usize {
        self.favorites
            .favorite_indexes(self.playlist.channels())
            .len()
    }

    #[must_use]
    pub fn is_favorite(&self, playlist_index: usize) -> bool {
        self.playlist
            .channels()
            .get(playlist_index)
            .is_some_and(|channel| self.favorites.is_favorite(channel))
    }

    /// Replace the favorite set (for example from stored configuration),
    /// normalized defensively by [`FavoriteSet::from_stored`].
    pub fn set_favorites(&mut self, favorites: FavoriteSet) {
        self.favorites = favorites;
    }

    /// Toggle the favorite state of one channel by playlist identity and
    /// refresh the active projection. Returns `None` when the index does not
    /// name a channel in the current playlist.
    pub fn toggle_favorite(
        &mut self,
        playlist_index: usize,
        query: &str,
    ) -> Option<FavoriteToggle> {
        let channel = self.playlist.channels().get(playlist_index)?;
        let name = display_name(channel).to_owned();
        let toggle = self.favorites.toggle(channel);
        let announcement = match toggle {
            Toggle::Added => format!("Added {} to Favorites.", announced_name(&name)),
            Toggle::Removed => {
                format!("Removed {} from Favorites.", announced_name(&name))
            }
            Toggle::Unchanged => String::new(),
        };
        if toggle != Toggle::Unchanged {
            self.submit_current(query.to_owned());
        }
        Some(FavoriteToggle {
            toggle,
            announcement,
        })
    }

    /// Submit the current category scope of the last usable playlist.
    fn submit_current(&mut self, query: String) {
        let scope = self.scoped_rows(
            self.playlist.channels(),
            category_rows(self.playlist.channels(), &self.category, &self.favorites),
        );
        self.searcher.submit_scoped(
            Arc::clone(&self.playlist),
            scope,
            self.category.clone(),
            query,
        );
        self.searching = true;
    }

    /// Poll from the UI thread; never reads, parses or scans channels here.
    pub fn poll(&mut self, query: &str) -> Option<BrowseEvent> {
        if let Some(loaded) = self.loader.try_latest() {
            match loaded.outcome {
                Ok(loaded) => self.filter_loaded(&loaded, query),
                Err(error) => {
                    self.loading = false;
                    return Some(BrowseEvent::Recovery(error));
                }
            }
        }
        self.searcher.try_latest().map(|results| {
            if let Some(replacement) = self.replacement.take() {
                let previous = std::mem::replace(&mut self.playlist, replacement);
                std::thread::spawn(move || drop(previous));
                self.loading = false;
            }
            self.searching = false;
            BrowseEvent::Results(results)
        })
    }

    fn filter_loaded(&mut self, loaded: &Arc<BrowseModel>, query: &str) {
        self.raw = Some(Arc::clone(loaded));
        let merged = merged_with_extras(loaded, &self.extras);
        self.apply_model(merged, query);
    }

    /// Channels arriving outside the playlist load (provider accounts)
    /// merge into the browse model without disturbing the active load.
    pub fn add_channels(&mut self, channels: Vec<crate::Channel>, query: &str) {
        self.extras.extend(channels);
        if self.loading {
            return;
        }
        let base = self
            .raw
            .clone()
            .unwrap_or_else(|| Arc::new(BrowseModel::default()));
        let merged = merged_with_extras(&base, &self.extras);
        self.apply_model(merged, query);
    }

    fn apply_model(&mut self, loaded: Arc<BrowseModel>, query: &str) {
        // A group that vanished from the replacement cannot stay active;
        // the sentinels always exist. Favorites survive by stable key.
        if matches!(self.category, Category::Group(_))
            && !categories(loaded.channels()).contains(&self.category)
        {
            self.category = Category::All;
        }
        let scope = self.scoped_rows(
            loaded.channels(),
            category_rows(loaded.channels(), &self.category, &self.favorites),
        );
        self.searcher.submit_scoped(
            Arc::clone(&loaded),
            scope,
            self.category.clone(),
            query.to_owned(),
        );
        self.replacement = Some(loaded);
        self.searching = true;
    }

    #[must_use]
    pub const fn is_busy(&self) -> bool {
        self.loading || self.searching
    }

    pub fn cancel(&mut self) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
    }
}

fn announced_name(name: &str) -> &str {
    if name.is_empty() {
        "Unnamed channel"
    } else {
        name
    }
}

/// A browse model with provider channels appended in arrival order.
fn merged_with_extras(model: &BrowseModel, extras: &[crate::Channel]) -> Arc<BrowseModel> {
    if extras.is_empty() {
        return Arc::new(model.clone());
    }
    let mut channels = model.channels().to_vec();
    channels.extend(extras.iter().cloned());
    Arc::new(BrowseModel::from_parts(channels, model.malformed_entries()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn wait(browser: &mut BrowserWorker, query: &str) -> BrowseEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = browser.poll(query) {
                return event;
            }
            assert!(Instant::now() < deadline, "browse timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn results(event: BrowseEvent) -> SearchResults {
        match event {
            BrowseEvent::Results(results) => results,
            BrowseEvent::Recovery(error) => panic!("{error:?}"),
        }
    }

    #[test]
    fn playlist_scope_limits_rows_and_categories() {
        let dir = std::env::temp_dir().join("iptv-scope-test");
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.m3u");
        let second = dir.join("second.m3u");
        std::fs::write(
            &first,
            "#EXTM3U\n#EXTINF:-1 group-title=\"News\",Alpha One\nhttp://example.com/a1\n#EXTINF:-1 group-title=\"News\",Alpha Two\nhttp://example.com/a2\n",
        )
        .unwrap();
        std::fs::write(
            &second,
            "#EXTM3U\n#EXTINF:-1 group-title=\"Sport\",Beta One\nhttp://example.com/b1\n",
        )
        .unwrap();
        let sources = vec![
            crate::sources::PlaylistSource::new(first.to_string_lossy()),
            crate::sources::PlaylistSource::new(second.to_string_lossy()),
        ];
        let first_id = sources[0].scope_id();
        let mut browser = BrowserWorker::default();
        browser.load_sources(sources);
        assert_eq!(results(wait(&mut browser, "")).len(), 3);
        browser.set_scope(Some(first_id), "");
        assert_eq!(results(wait(&mut browser, "")).len(), 2);
        let groups: Vec<String> = browser
            .categories()
            .iter()
            .map(|c| c.label().to_string())
            .collect();
        assert!(groups.iter().any(|g| g == "News"), "{groups:?}");
        assert!(!groups.iter().any(|g| g == "Sport"), "{groups:?}");
        browser.set_scope(None, "");
        assert_eq!(results(wait(&mut browser, "")).len(), 3);
    }

    #[test]
    fn with_all_channels_covers_playlist_and_extras_in_order() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        assert_eq!(browser.with_all_channels(<[crate::Channel]>::len), 4);
        let extra = crate::Channel {
            name: "Extra".to_owned(),
            group: String::new(),
            url: "http://example.com/extra".to_owned(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        };
        browser.add_channels(vec![extra], "");
        assert_eq!(results(wait(&mut browser, "")).len(), 5);
        let names: Vec<String> =
            browser.with_all_channels(|channels| channels.iter().map(|c| c.name.clone()).collect());
        assert_eq!(names.last().map(String::as_str), Some("Extra"));
        assert_eq!(browser.all_channels().len(), 5);
    }

    #[test]
    fn failures_cancel_and_retry_keep_previous_channels_searchable() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.load(fixture("absent.m3u"));
        assert!(matches!(
            wait(&mut browser, ""),
            BrowseEvent::Recovery(LoadFailure::Missing)
        ));
        assert!(!browser.is_busy());
        browser.search(String::new());
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.load(fixture("empty.m3u"));
        assert!(browser.cancel_load());
        assert!(browser.poll("").is_none());
        browser.search(String::new());
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.load(fixture("empty.m3u"));
        let empty = results(wait(&mut browser, ""));
        assert!(empty.is_empty());
        assert!(empty.status().contains("Open another playlist"));
    }

    #[test]
    fn cancel_after_parsing_also_discards_pending_replacement_search() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.loading = true;
        browser.filter_loaded(
            &Arc::new(BrowseModel::open_local(&fixture("recovery.m3u")).unwrap()),
            "cat",
        );
        assert!(browser.cancel_load());
        assert!(!browser.is_busy());
        assert!(browser.poll("cat").is_none());
        browser.search(String::new());
        let retained = results(wait(&mut browser, ""));
        assert_eq!(retained.len(), 4);
        assert_eq!(retained.channel(0).unwrap().name, "ABC East HD");
    }

    #[test]
    fn new_load_and_search_supersede_each_other() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.search("ABC".to_owned());
        browser.load(fixture("absent.m3u"));
        browser.load(fixture("recovery.m3u"));
        let replacement = results(wait(&mut browser, "cat"));
        assert_eq!(replacement.len(), 4);
        assert!(replacement.status().contains("malformed entries"));
        assert_eq!(replacement.channel(0).unwrap().name, "Cat");
        browser.load(fixture("empty.m3u"));
        assert!(browser.search("Recovered".to_owned()));
        let searched = results(wait(&mut browser, "Recovered"));
        assert_eq!(searched.len(), 1);
        assert_eq!(searched.channel(0).unwrap().name, "Recovered");
        browser.load(fixture("local.m3u"));
        browser.cancel();
        assert!(!browser.is_busy());
        assert!(browser.poll("").is_none());
    }
}

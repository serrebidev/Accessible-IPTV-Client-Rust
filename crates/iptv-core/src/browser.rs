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
    favorites: FavoriteSet,
}

impl BrowserWorker {
    pub fn load(&mut self, path: PathBuf) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
        self.loader.submit(path);
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
    #[must_use]
    pub fn categories(&self) -> Vec<Category> {
        categories(self.playlist.channels())
    }

    #[must_use]
    pub fn favorites(&self) -> &FavoriteSet {
        &self.favorites
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
        let scope = category_rows(self.playlist.channels(), &self.category, &self.favorites);
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
                Ok(loaded) => self.filter_loaded(loaded, query),
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

    fn filter_loaded(&mut self, loaded: Arc<BrowseModel>, query: &str) {
        // A group that vanished from the replacement cannot stay active;
        // the sentinels always exist. Favorites survive by stable key.
        if matches!(self.category, Category::Group(_))
            && !categories(loaded.channels()).contains(&self.category)
        {
            self.category = Category::All;
        }
        let scope = category_rows(loaded.channels(), &self.category, &self.favorites);
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
            Arc::new(BrowseModel::open_local(&fixture("recovery.m3u")).unwrap()),
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

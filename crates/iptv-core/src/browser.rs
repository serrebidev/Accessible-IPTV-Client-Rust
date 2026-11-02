//! The local browse application seam, shared by the desktop and domain tests.
use crate::{
    category::Category,
    favorites::{FavoriteSet, ToggleOutcome},
    loading::{LoadFailure, LoadWorker},
    search::{SearchResults, SearchWorker},
    BrowseModel,
};
use std::{path::PathBuf, sync::Arc};

pub enum BrowseEvent {
    Results(SearchResults),
    Recovery(LoadFailure),
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
    favorites: Arc<FavoriteSet>,
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
        self.searcher.submit_favorites(
            Arc::clone(&self.playlist),
            query,
            self.category.clone(),
            Arc::clone(&self.favorites),
        );
        self.searching = true;
        cancelled
    }

    /// A category change supersedes pending work without changing keyboard focus.
    pub fn select_category(&mut self, category: Category, query: String) -> bool {
        self.category = category;
        self.search(query)
    }

    /// Toggle one real row from the active playlist, never a retired snapshot.
    /// Supersede pending projections before publishing the changed favorite set.
    pub fn toggle_favorite(
        &mut self,
        visible: &SearchResults,
        row: usize,
        query: String,
    ) -> Option<ToggleOutcome> {
        if !visible.is_source(&self.playlist) {
            return None;
        }
        let channel = visible.channel(row)?;
        let outcome = Arc::make_mut(&mut self.favorites).toggle(channel);
        // Other categories have unchanged rows: keep their selection, focus and
        // accessible row cache rather than needlessly publishing the same list.
        if outcome != ToggleOutcome::Unkeyable && self.category == Category::Favorites {
            self.search(query);
        }
        Some(outcome)
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
            self.category = results.category().clone();
            self.searching = false;
            BrowseEvent::Results(results)
        })
    }

    fn filter_loaded(&mut self, loaded: Arc<BrowseModel>, query: &str) {
        let category = if loaded.categories().contains(&self.category) {
            self.category.clone()
        } else {
            Category::All
        };
        self.searcher.submit_favorites(
            Arc::clone(&loaded),
            query.to_owned(),
            category,
            Arc::clone(&self.favorites),
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
    fn categories_survive_failure_cancel_and_replacement_without_stale_rows() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("categories.m3u"));
        let all = results(wait(&mut browser, ""));
        let news = Category::Group("News".into());
        browser.select_category(news.clone(), String::new());
        let selected = results(wait(&mut browser, ""));
        assert_eq!(selected.len(), 2);
        assert_eq!(selected.restore_row(&all, Some(4)), Some(1));
        browser.load(fixture("absent.m3u"));
        assert!(matches!(wait(&mut browser, ""), BrowseEvent::Recovery(_)));
        browser.search(String::new());
        assert_eq!(results(wait(&mut browser, "")).category(), &news);
        browser.loading = true;
        browser.filter_loaded(Arc::new(BrowseModel::empty()), "");
        assert!(browser.cancel_load());
        browser.search(String::new());
        assert_eq!(results(wait(&mut browser, "")).category(), &news);
        browser.load(fixture("categories.m3u"));
        assert_eq!(results(wait(&mut browser, "")).category(), &news);
        browser.load(fixture("empty.m3u"));
        let empty = results(wait(&mut browser, ""));
        assert_eq!(empty.category(), &Category::All);
        assert!(empty.is_empty());
        assert_eq!(empty.restore_row(&selected, Some(1)), None);
        assert_eq!(empty.categories(), &[Category::All, Category::Favorites]);
        browser.load(fixture("categories.m3u"));
        assert!(browser.select_category(news, String::new()));
        assert!(results(wait(&mut browser, "")).is_empty());
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

    #[test]
    fn favorite_toggle_supersedes_a_parsed_replacement_and_queued_projection() {
        let mut browser = BrowserWorker::default();
        browser.load(fixture("categories.m3u"));
        let all = results(wait(&mut browser, ""));
        browser.toggle_favorite(&all, 1, String::new());
        browser.select_category(Category::Favorites, String::new());
        let favorite = results(wait(&mut browser, ""));
        browser.loading = true;
        browser.filter_loaded(Arc::new(BrowseModel::empty()), "");
        browser.toggle_favorite(&favorite, 0, String::new());
        let removed = results(wait(&mut browser, ""));
        assert_eq!(removed.category(), &Category::Favorites);
        assert!(removed.is_empty());
        browser.select_category(Category::All, String::new());
        assert_eq!(results(wait(&mut browser, "")).len(), 7);
        assert!(browser.poll("").is_none());
    }
}

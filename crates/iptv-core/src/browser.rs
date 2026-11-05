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
    /// A sources load finished; carries the locations of sources
    /// whose previous channels were retained after a failure (B12).
    /// The adapter also reacts by refreshing playlist-header guides.
    Loaded {
        stale_sources: Vec<String>,
    },
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
#[allow(clippy::struct_excessive_bools)] // independent load/search state flags
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
    /// How many of the current playlist's tail channels are provider
    /// extras merged into it (keeps [`Self::all_channels`] exact).
    playlist_extras: usize,
    pending_extras: usize,
    /// Directory holding the plain-source caches (B11).
    source_cache_dir: Option<PathBuf>,
    /// A prefill was applied and the final load is still coming.
    awaiting_final: bool,
    /// The last load was a multi-source load (vs a single file).
    last_load_was_sources: bool,
    /// Sources of the last multi-source load, by location.
    last_sources: Vec<crate::sources::PlaylistSource>,
    /// Playlist-header guides by source location (B09).
    playlist_guides: std::collections::BTreeMap<String, Vec<String>>,
    /// The background navigation index (B19, port of Python's
    /// `PlaylistIndex` warm-up): built on a worker thread after a
    /// model is applied, installed only for the exact model it was
    /// built from, and every projection falls back to scanning while
    /// it is absent or stale.
    nav_index: Option<Arc<crate::navindex::NavIndex>>,
    nav_index_model: Option<Arc<BrowseModel>>,
    nav_index_building: Option<Arc<BrowseModel>>,
    nav_index_rx:
        Option<std::sync::mpsc::Receiver<(Arc<BrowseModel>, Arc<crate::navindex::NavIndex>)>>,
}

impl BrowserWorker {
    pub fn load(&mut self, path: PathBuf) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
        self.last_load_was_sources = false;
        self.loader.submit(path);
        self.loading = true;
    }

    /// Where the plain-source download/parsed caches live (B11).
    pub fn set_source_cache_dir(&mut self, dir: PathBuf) {
        self.source_cache_dir = Some(dir);
    }

    /// Load several configured sources merged, with scope tags.
    pub fn load_sources(&mut self, sources: Vec<crate::sources::PlaylistSource>) {
        self.load_sources_mode(sources, false);
    }

    /// Reload sources bypassing the URL download cache (B13 refresh).
    pub fn refresh_sources(&mut self, sources: Vec<crate::sources::PlaylistSource>) {
        self.load_sources_mode(sources, true);
    }

    fn load_sources_mode(&mut self, sources: Vec<crate::sources::PlaylistSource>, force: bool) {
        self.cancel_load();
        self.searcher.cancel();
        self.searching = false;
        self.last_sources.clone_from(&sources);
        self.last_load_was_sources = true;
        self.loader.submit_sources_full(
            sources,
            self.source_cache_dir.clone(),
            Arc::clone(&self.playlist),
            force,
        );
        self.loading = true;
    }

    /// Drop the playlist scope without resubmitting (used after the
    /// source list changes: a scope naming a removed source must not
    /// hide the whole catalogue).
    pub fn reset_scope(&mut self) {
        self.scope = None;
    }

    /// Flat playlist-header guide URLs from the last loads (B09).
    #[must_use]
    pub fn playlist_guide_urls(&self) -> Vec<String> {
        let mut flat: Vec<String> = Vec::new();
        for guides in self.playlist_guides.values() {
            for guide in guides {
                if !flat.contains(guide) {
                    flat.push(guide.clone());
                }
            }
        }
        flat
    }

    /// Returns whether a pending load was cancelled. Existing rows stay usable.
    pub fn cancel_load(&mut self) -> bool {
        self.loader.cancel();
        self.awaiting_final = false;
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
        // B19: with the navigation index installed for this model,
        // the scoped group list comes from its first-appearance
        // group table instead of a full catalogue scan.
        if let (Some(scope), Some(index)) = (&self.scope, self.nav_index_for(&self.playlist)) {
            let mut list = vec![Category::All, Category::Favorites];
            let mut seen = std::collections::HashSet::new();
            for raw in index.scope_group_names(scope) {
                let name = if raw.is_empty() {
                    crate::categories::UNCATEGORIZED
                } else {
                    raw
                };
                if seen.insert(name.to_owned()) {
                    list.push(Category::Group(name.to_owned()));
                }
            }
            return list;
        }
        // B19: borrow, never clone, the scoped channels.
        crate::categories::categories_from(
            self.playlist
                .channels()
                .iter()
                .filter(|channel| self.in_scope(channel)),
        )
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
    /// `model` is the model the rows index into; when the
    /// navigation index was built from it, membership is a binary
    /// search per row instead of a scan of channel attributes (B19).
    fn scoped_rows(&self, model: &Arc<BrowseModel>, rows: Vec<usize>) -> Vec<usize> {
        let Some(scope) = &self.scope else {
            return rows;
        };
        if let Some(nav) = self.nav_index_for(model) {
            return rows
                .into_iter()
                .filter(|&index| nav.in_scope(scope, index))
                .collect();
        }
        let channels = model.channels();
        rows.into_iter()
            .filter(|&index| channels.get(index).is_some_and(|c| self.in_scope(c)))
            .collect()
    }

    /// The navigation index installed for exactly `model`, if any.
    fn nav_index_for(&self, model: &Arc<BrowseModel>) -> Option<Arc<crate::navindex::NavIndex>> {
        match (&self.nav_index, &self.nav_index_model) {
            (Some(index), Some(indexed)) if Arc::ptr_eq(indexed, model) => Some(Arc::clone(index)),
            _ => None,
        }
    }

    /// Warm the navigation index for `model` on a background thread
    /// (Python's `_warm_playlist_index_background`): projections
    /// keep using the scan path until `poll` installs the result,
    /// and a build for a superseded model is discarded on arrival.
    fn request_nav_index(&mut self, model: &Arc<BrowseModel>) {
        if self.nav_index_for(model).is_some() {
            return;
        }
        if self
            .nav_index_building
            .as_ref()
            .is_some_and(|building| Arc::ptr_eq(building, model))
        {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let build_model = Arc::clone(model);
        std::thread::spawn(move || {
            let index = crate::navindex::NavIndex::build(build_model.channels());
            let _ = sender.send((build_model, Arc::new(index)));
        });
        self.nav_index_building = Some(Arc::clone(model));
        self.nav_index_rx = Some(receiver);
    }

    /// Install a finished navigation index whose model is still
    /// current (the published playlist or the pending replacement).
    fn pump_nav_index(&mut self) {
        let Some(receiver) = &self.nav_index_rx else {
            return;
        };
        match receiver.try_recv() {
            Ok((model, index)) => {
                let current = Arc::ptr_eq(&model, &self.playlist)
                    || self
                        .replacement
                        .as_ref()
                        .is_some_and(|pending| Arc::ptr_eq(&model, pending));
                if current {
                    self.nav_index = Some(index);
                    self.nav_index_model = Some(model);
                }
                self.nav_index_building = None;
                self.nav_index_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.nav_index_building = None;
                self.nav_index_rx = None;
            }
        }
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
    /// catalogue builders that scan the whole browse state. The
    /// playlist model already ends with the extras merged into it;
    /// they are counted once (B02).
    #[must_use]
    pub fn all_channels(&self) -> Vec<crate::Channel> {
        let base = self.playlist.channels();
        let merged_tail = self.playlist_extras.min(base.len()).min(self.extras.len());
        let mut out: Vec<crate::Channel> = base[..base.len() - merged_tail].to_vec();
        out.extend(self.extras.iter().cloned());
        out
    }

    /// The browse model's two halves without merging: the plain
    /// playlist model (cheap Arc clone) and the provider extras.
    /// Catalogue builders that filter on a worker thread use this
    /// instead of paying for a full catalogue clone (B19).
    #[must_use]
    pub fn catalogue_parts(&self) -> (Arc<BrowseModel>, Vec<crate::Channel>) {
        (Arc::clone(&self.playlist), self.extras.clone())
    }

    /// Cheap snapshot of the merged catalogue for background builders.
    #[must_use]
    pub fn catalogue_model(&self) -> Arc<BrowseModel> {
        Arc::clone(&self.playlist)
    }

    /// The channel at a playlist index in the current projection's source.
    #[must_use]
    pub fn channel(&self, playlist_index: usize) -> Option<crate::Channel> {
        self.playlist.channels().get(playlist_index).cloned()
    }

    /// Run `f` over every loaded channel (playlist plus provider
    /// extras, in `all_channels` order) without cloning the
    /// catalogue (audit E12 — suffix building over 500,000
    /// channels must not start by cloning all of them). The
    /// playlist model's tail IS the current extras (every extras
    /// change rebuilds the model), so the model slice is the
    /// catalogue in order.
    pub fn with_all_channels<R>(&self, f: impl FnOnce(&[crate::Channel]) -> R) -> R {
        f(self.catalogue())
    }

    /// Favorited channels present in the current playlist (display
    /// counts). Scoped to the active playlist scope (B20): the label
    /// must agree with the visible source.
    #[must_use]
    pub fn favorite_count(&self) -> usize {
        let channels = self.playlist.channels();
        let indexes = self.favorites.favorite_indexes(channels);
        let Some(scope) = &self.scope else {
            return indexes.len();
        };
        if let Some(index) = self.nav_index_for(&self.playlist) {
            return indexes
                .into_iter()
                .filter(|&position| index.in_scope(scope, position))
                .count();
        }
        indexes
            .into_iter()
            .filter(|&index| self.in_scope(&channels[index]))
            .count()
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
        let model = Arc::clone(&self.playlist);
        let scope = self.scoped_rows(
            &model,
            category_rows(model.channels(), &self.category, &self.favorites),
        );
        let nav_index = self.nav_index_for(&model);
        self.searcher
            .submit_scoped(model, scope, self.category.clone(), query, nav_index);
        self.searching = true;
    }

    /// Poll from the UI thread; never reads, parses or scans channels here.
    pub fn poll(&mut self, query: &str) -> Option<BrowseEvent> {
        self.pump_nav_index();
        if let Some(loaded) = self.loader.try_latest() {
            match loaded.outcome {
                Ok(model) => {
                    if loaded.intermediate {
                        // B11 prefill: show the cached channels now;
                        // the final result for this load still comes.
                        self.awaiting_final = true;
                        self.filter_loaded(&model, query);
                    } else {
                        let was_sources = self.last_load_was_sources;
                        self.awaiting_final = false;
                        if was_sources {
                            let fetched: std::collections::BTreeMap<String, Vec<String>> =
                                loaded.guide_urls.iter().cloned().collect();
                            let locations: Vec<String> = self
                                .last_sources
                                .iter()
                                .map(|source| source.location().to_string())
                                .collect();
                            let (by_source, _) = crate::merge_playlist_header_guides(
                                &self.playlist_guides,
                                &fetched,
                                &locations,
                            );
                            self.playlist_guides = by_source;
                        }
                        self.filter_loaded(&model, query);
                        if was_sources {
                            return Some(BrowseEvent::Loaded {
                                stale_sources: loaded.stale_sources,
                            });
                        }
                    }
                }
                Err(error) => {
                    self.loading = false;
                    self.awaiting_final = false;
                    return Some(BrowseEvent::Recovery(error));
                }
            }
        }
        self.searcher.try_latest().map(|results| {
            if let Some(replacement) = self.replacement.take() {
                let previous = std::mem::replace(&mut self.playlist, replacement);
                std::thread::spawn(move || drop(previous));
                self.playlist_extras = self.pending_extras;
                self.loading = self.awaiting_final;
            }
            self.searching = false;
            BrowseEvent::Results(results)
        })
    }

    fn filter_loaded(&mut self, loaded: &Arc<BrowseModel>, query: &str) {
        self.raw = Some(Arc::clone(loaded));
        let merged = merged_with_extras(loaded, &self.extras);
        self.apply_model(&merged, query);
    }

    /// Channels arriving outside the playlist load (provider accounts)
    /// merge into the browse model without disturbing the active load.
    pub fn add_channels(&mut self, channels: Vec<crate::Channel>, query: &str) {
        self.extras.extend(channels);
        self.reapply_merged(query);
    }

    /// A fresh provider load REPLACES the provider channels (B02):
    /// reloading an unchanged account must not append a second copy,
    /// and a removed account's channels must disappear. Python
    /// replaces its catalogue and provider registry on every load.
    pub fn replace_provider_channels(&mut self, channels: Vec<crate::Channel>, query: &str) {
        self.extras = channels;
        self.reapply_merged(query);
    }

    fn reapply_merged(&mut self, query: &str) {
        if self.loading {
            return;
        }
        let base = self
            .raw
            .clone()
            .unwrap_or_else(|| Arc::new(BrowseModel::default()));
        let merged = merged_with_extras(&base, &self.extras);
        self.apply_model(&merged, query);
    }

    fn apply_model(&mut self, loaded: &Arc<BrowseModel>, query: &str) {
        // A group that vanished from the replacement cannot stay active;
        // the sentinels always exist. Favorites survive by stable key.
        if matches!(self.category, Category::Group(_))
            && !categories(loaded.channels()).contains(&self.category)
        {
            self.category = Category::All;
        }
        let scope = self.scoped_rows(
            loaded,
            category_rows(loaded.channels(), &self.category, &self.favorites),
        );
        let nav_index = self.nav_index_for(loaded);
        self.searcher.submit_scoped(
            Arc::clone(loaded),
            scope,
            self.category.clone(),
            query.to_owned(),
            nav_index,
        );
        self.pending_extras = self.extras.len();
        self.replacement = Some(Arc::clone(loaded));
        self.searching = true;
        self.request_nav_index(loaded);
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
            BrowseEvent::Loaded { .. } => panic!("unexpected Loaded event"),
        }
    }

    /// Poll until the Results event, stepping over Loaded notices.
    fn wait_results(browser: &mut BrowserWorker, query: &str) -> SearchResults {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match browser.poll(query) {
                Some(BrowseEvent::Results(results)) => return results,
                Some(BrowseEvent::Recovery(error)) => panic!("{error:?}"),
                Some(BrowseEvent::Loaded { .. }) => {}
                None => {
                    assert!(Instant::now() < deadline, "browse timed out");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
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
        assert_eq!(wait_results(&mut browser, "").len(), 3);
        browser.set_scope(Some(first_id), "");
        assert_eq!(wait_results(&mut browser, "").len(), 2);
        let groups: Vec<String> = browser
            .categories()
            .iter()
            .map(|c| c.label().to_string())
            .collect();
        assert!(groups.iter().any(|g| g == "News"), "{groups:?}");
        assert!(!groups.iter().any(|g| g == "Sport"), "{groups:?}");
        browser.set_scope(None, "");
        assert_eq!(wait_results(&mut browser, "").len(), 3);
    }

    fn provider_channel(name: &str, account: &str) -> crate::Channel {
        let mut channel = crate::parse_m3u(&format!("#EXTINF:-1,{name}\nhttp://h/live/u/p/1.ts\n"))
            .into_iter()
            .next()
            .unwrap();
        channel
            .unknown_attributes
            .insert("provider-type".to_string(), "xtream".to_string());
        channel
            .unknown_attributes
            .insert("provider-id".to_string(), account.to_string());
        channel.unknown_attributes.insert(
            crate::sources::PLAYLIST_ID_ATTRIBUTE.to_string(),
            account.to_string(),
        );
        channel
    }

    #[test]
    fn provider_scope_shows_its_channels_and_replacement_never_duplicates() {
        // B01 + B02 regression: scoping to a provider account lists
        // its channels; a provider reload replaces instead of
        // appending; removing the account removes its channels.
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        let channels = vec![provider_channel("Prov One", "acct-1")];
        browser.replace_provider_channels(channels.clone(), "");
        assert_eq!(wait_results(&mut browser, "").len(), 5);
        assert_eq!(browser.all_channels().len(), 5);
        // Reload the unchanged account: still one copy.
        browser.replace_provider_channels(channels, "");
        assert_eq!(wait_results(&mut browser, "").len(), 5);
        assert_eq!(browser.all_channels().len(), 5);
        // Scope to the account: only its channel remains visible.
        browser.set_scope(Some("acct-1".to_string()), "");
        assert_eq!(wait_results(&mut browser, "").len(), 1);
        browser.set_scope(None, "");
        assert_eq!(wait_results(&mut browser, "").len(), 5);
        // Remove the account: its channels disappear everywhere.
        browser.replace_provider_channels(Vec::new(), "");
        assert_eq!(wait_results(&mut browser, "").len(), 4);
        assert_eq!(browser.all_channels().len(), 4);
    }

    #[test]
    fn nav_index_projections_match_the_scan_path() {
        // B19 regression: once the background navigation index
        // installs, scoped categories, rows and favorite counts must
        // agree exactly with the scan path they replace.
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.replace_provider_channels(
            vec![
                provider_channel("Prov One", "acct-1"),
                provider_channel("Prov Two", "acct-2"),
            ],
            "",
        );
        assert_eq!(wait_results(&mut browser, "").len(), 6);
        browser.set_scope(Some("acct-1".to_string()), "");
        assert_eq!(wait_results(&mut browser, "").len(), 1);
        // Scan-path answers (the index for the published model has
        // not been pumped yet, and each answer below is taken before
        // the install on purpose).
        let scan_categories = browser.categories();
        assert!(browser.toggle_favorite(4, "").is_some());
        let scan_count = browser.favorite_count();
        assert_eq!(scan_count, 1);
        // Wait for the background build to land and install.
        let deadline = Instant::now() + Duration::from_secs(10);
        while browser.nav_index_for(&browser.playlist).is_none() {
            browser.pump_nav_index();
            assert!(Instant::now() < deadline, "nav index never installed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(browser.categories(), scan_categories);
        assert_eq!(browser.favorite_count(), scan_count);
        browser.set_scope(Some("acct-1".to_string()), "");
        assert_eq!(wait_results(&mut browser, "").len(), 1);
        // A scoped search through the index names the same rows.
        browser.search("prov".to_string());
        assert_eq!(wait_results(&mut browser, "prov").len(), 1);
    }

    #[test]
    fn empty_source_list_publishes_an_empty_catalogue() {
        // B03 regression (core half): loading zero sources is a real
        // load, not a skip.
        let mut browser = BrowserWorker::default();
        browser.load(fixture("local.m3u"));
        assert_eq!(results(wait(&mut browser, "")).len(), 4);
        browser.load_sources(Vec::new());
        let empty = wait_results(&mut browser, "");
        assert!(empty.is_empty());
        assert_eq!(browser.all_channels().len(), 0);
    }

    #[test]
    fn favorite_count_follows_the_scope() {
        // B20 regression.
        let dir = std::env::temp_dir().join("iptv-favcount-test");
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.m3u");
        let second = dir.join("second.m3u");
        std::fs::write(&first, "#EXTM3U\n#EXTINF:-1,Alpha\nhttp://a/1\n").unwrap();
        std::fs::write(&second, "#EXTM3U\n#EXTINF:-1,Beta\nhttp://b/1\n").unwrap();
        let sources = vec![
            crate::sources::PlaylistSource::new(first.to_string_lossy()),
            crate::sources::PlaylistSource::new(second.to_string_lossy()),
        ];
        let first_id = sources[0].scope_id();
        let mut browser = BrowserWorker::default();
        browser.set_favorites(FavoriteSet::from_stored([
            "Alpha".to_string(),
            "Beta".to_string(),
        ]));
        browser.load_sources(sources);
        assert_eq!(wait_results(&mut browser, "").len(), 2);
        assert_eq!(browser.favorite_count(), 2);
        browser.set_scope(Some(first_id), "");
        assert_eq!(wait_results(&mut browser, "").len(), 1);
        assert_eq!(browser.favorite_count(), 1);
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

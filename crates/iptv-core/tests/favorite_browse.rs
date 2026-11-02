use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    category::Category,
    favorites::ToggleOutcome,
    search::SearchResults,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn load(browser: &mut BrowserWorker, name: &str) -> SearchResults {
    browser.load(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    );
    wait(browser)
}

fn wait(browser: &mut BrowserWorker) -> SearchResults {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = browser.poll("") {
            return match event {
                BrowseEvent::Results(result) => result,
                BrowseEvent::Recovery(error) => panic!("{error:?}"),
            };
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn favorites_are_a_stable_empty_category_and_toggle_once_in_playlist_order() {
    let mut browser = BrowserWorker::default();
    let all = load(&mut browser, "categories.m3u");
    assert_eq!(
        &all.categories()[..2],
        &[Category::All, Category::Favorites]
    );
    assert_ne!(Category::Favorites, Category::Group("Favorites".into()));
    browser.select_category(Category::Favorites, String::new());
    let empty = wait(&mut browser);
    assert!(empty.is_empty());
    assert!(empty.channel(0).is_none());
    assert!(empty.status().contains("Favorites is empty"));
    assert_eq!(empty.restore_row(&all, Some(4)), None);
    browser.select_category(Category::All, String::new());
    let all = wait(&mut browser);
    assert_eq!(
        browser.toggle_favorite(&all, 4, String::new()),
        Some(ToggleOutcome::Added)
    );
    assert!(!browser.is_busy());
    assert!(browser.poll("").is_none());
    assert_eq!(
        browser.toggle_favorite(&all, 1, String::new()),
        Some(ToggleOutcome::Added)
    );
    browser.select_category(Category::Favorites, String::new());
    let favorites = wait(&mut browser);
    assert_eq!(favorites.len(), 2);
    assert_eq!(favorites.channel(0).unwrap().tvg_id, "n1");
    assert_eq!(favorites.channel(1).unwrap().tvg_id, "n2");
    assert_eq!(favorites.restore_row(&all, Some(4)), Some(1));
    assert_eq!(
        browser.toggle_favorite(&favorites, 1, String::new()),
        Some(ToggleOutcome::Removed)
    );
    let remaining = wait(&mut browser);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.restore_row(&favorites, Some(1)), Some(0));
    assert_eq!(
        browser.toggle_favorite(&remaining, 0, String::new()),
        Some(ToggleOutcome::Removed)
    );
    let empty = wait(&mut browser);
    assert_eq!(empty.len(), 0);
    assert!(empty.channel(0).is_none());
    assert_eq!(browser.toggle_favorite(&empty, 0, String::new()), None);
}

#[test]
fn refresh_preserves_matching_favorites_and_selection_but_rejects_retired_rows() {
    let mut browser = BrowserWorker::default();
    let all = load(&mut browser, "categories.m3u");
    browser.toggle_favorite(&all, 1, String::new());
    browser.toggle_favorite(&all, 4, String::new());
    browser.select_category(Category::Favorites, String::new());
    let favorites = wait(&mut browser);
    let refreshed = load(&mut browser, "favorites-refresh.m3u");
    assert_eq!(refreshed.category(), &Category::Favorites);
    assert_eq!(refreshed.len(), 2);
    assert_eq!(refreshed.channel(0).unwrap().tvg_id, "n2");
    assert_eq!(refreshed.channel(1).unwrap().tvg_id, "n1");
    assert_eq!(refreshed.restore_row(&favorites, Some(0)), Some(1));
    assert_eq!(refreshed.restore_row(&favorites, Some(1)), Some(0));
    assert_eq!(browser.toggle_favorite(&all, 1, String::new()), None);

    browser.load(Path::new(env!("CARGO_MANIFEST_DIR")).join("absent.m3u"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = browser.poll("") {
            assert!(matches!(event, BrowseEvent::Recovery(_)));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    browser.search(String::new());
    let retained = wait(&mut browser);
    assert_eq!(retained.len(), 2);
    assert!(retained.retains_row(&refreshed, 1));

    let empty = load(&mut browser, "empty.m3u");
    assert_eq!(empty.category(), &Category::Favorites);
    assert!(empty.status().contains("Favorites is empty"));
    assert_eq!(empty.restore_row(&retained, Some(1)), None);
    // Missing channels disappear only from the projection, never from the set.
    let returned = load(&mut browser, "categories.m3u");
    assert_eq!(returned.len(), 2);
    browser.select_category(Category::All, String::new());
    browser.select_category(Category::Favorites, String::new());
    browser.toggle_favorite(&returned, 0, String::new());
    browser.toggle_favorite(&returned, 0, String::new());
    let latest = wait(&mut browser);
    assert_eq!(latest.category(), &Category::Favorites);
    assert_eq!(latest.len(), 2);
    assert!(!browser.is_busy());
    assert!(browser.poll("").is_none());
}

#[test]
fn favorites_category_matches_pinned_python_projection_goldens() {
    use iptv_core::BrowseModel;
    let goldens: Vec<_> = include_str!("fixtures/python-favorite-projections.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    let cases: Vec<_> = include_str!("fixtures/favorite-projection-cases.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    assert_eq!(cases.len(), goldens.len());
    for (case, golden) in cases.into_iter().zip(goldens) {
        let fields: Vec<_> = case.split('\t').collect();
        let mut browser = BrowserWorker::default();
        let seeds = load(&mut browser, fields[2]);
        for row in fields[3].split(',').filter(|row| !row.is_empty()) {
            browser.toggle_favorite(&seeds, row.parse().unwrap(), String::new());
        }
        load(&mut browser, fields[1]);
        browser.select_category(Category::Favorites, String::new());
        let projected = wait(&mut browser);
        let source = BrowseModel::open_local(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fields[1]),
        )
        .unwrap();
        let indexes = (0..projected.len())
            .map(|row| {
                source
                    .channels()
                    .iter()
                    .position(|channel| channel == projected.channel(row).unwrap())
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(format!("{}\t{indexes}\t.", fields[0]), golden);
    }
}

//! Category and Favorites seam tests at the wx-free browser boundary,
//! plus favorite-key goldens from the pinned Python reference.
use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    categories::Category,
    favorites::{channel_key, Toggle},
    search::SearchResults,
    BrowseModel,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn wait(browser: &mut BrowserWorker, query: &str) -> SearchResults {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match browser.poll(query) {
            Some(BrowseEvent::Results(results)) => return results,
            Some(BrowseEvent::Recovery(error)) => panic!("unexpected recovery: {error:?}"),
            None => {
                assert!(Instant::now() < deadline, "browse timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn names(results: &SearchResults) -> Vec<&str> {
    (0..results.len())
        .map(|row| results.channel(row).unwrap().name.as_str())
        .collect()
}

#[test]
fn favorite_keys_match_pinned_python_goldens() {
    let model = BrowseModel::open_local(&fixture("categories.m3u")).unwrap();
    let mut channels: Vec<_> = model.channels().to_vec();
    // An escaped-backslash name and an unkeyable entry complete the golden set.
    channels.push(iptv_core::parse_m3u("#EXTINF:-1,A\\B\nu\n").remove(0));
    channels.push(iptv_core::parse_m3u("u\n").remove(0));
    for line in include_str!("fixtures/python-favorites.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let (index, expected) = line.split_once('\t').unwrap();
        let expected = expected.strip_prefix("<empty>").unwrap_or(expected);
        let index: usize = index.parse().unwrap();
        assert_eq!(channel_key(&channels[index]), expected, "golden {index}");
    }
}

#[test]
fn category_browsing_toggle_and_search_compose_in_the_browser_seam() {
    let mut browser = BrowserWorker::default();
    browser.load(fixture("categories.m3u"));
    let all = wait(&mut browser, "");
    assert_eq!(all.len(), 7);
    assert_eq!(
        browser
            .categories()
            .iter()
            .map(Category::sentinel)
            .collect::<Vec<_>>(),
        [
            "All Channels",
            "Favorites",
            "News",
            "Sport",
            "Uncategorized"
        ]
    );

    // Grouped browsing keeps playlist source order.
    browser.select_category(Category::Group("News".to_owned()), "");
    let news = wait(&mut browser, "");
    assert_eq!(names(&news), ["BBC One", "CNN"]);
    assert_eq!(news.status(), "2 channels in News.");

    // Toggle CNN (playlist index 1) from inside its category.
    let outcome = browser.toggle_favorite(1, "").unwrap();
    assert_eq!(outcome.toggle, Toggle::Added);
    assert_eq!(outcome.announcement, "Added CNN to Favorites.");
    let _ = wait(&mut browser, "");

    // Favorites shows it; empty-search status names the category.
    browser.select_category(Category::Favorites, "");
    let favorites = wait(&mut browser, "");
    assert_eq!(names(&favorites), ["CNN"]);
    assert_eq!(favorites.status(), "1 channels in Favorites.");

    // Search stays inside Favorites; clearing restores Favorites, not All.
    browser.search("bbc".to_owned());
    let none = wait(&mut browser, "bbc");
    assert!(none.is_empty());
    assert!(none.status().contains("No matching channels in Favorites"));
    browser.search(String::new());
    let restored = wait(&mut browser, "");
    assert_eq!(names(&restored), ["CNN"]);

    // Toggling off inside Favorites empties it with an explicit status.
    let outcome = browser.toggle_favorite(1, "").unwrap();
    assert_eq!(outcome.toggle, Toggle::Removed);
    let empty = wait(&mut browser, "");
    assert!(empty.is_empty());
    assert!(empty.status().contains("Favorites is empty"));

    // Duplicate plain-M3U names share one favorite key (Python rule).
    browser.select_category(Category::All, "");
    let _ = wait(&mut browser, "");
    let outcome = browser.toggle_favorite(3, "").unwrap();
    assert_eq!(outcome.toggle, Toggle::Added);
    let _ = wait(&mut browser, "");
    browser.select_category(Category::Favorites, "");
    let shared = wait(&mut browser, "");
    assert_eq!(names(&shared), ["BBC One", "BBC One"]);
    assert_eq!(
        shared.playlist_index(0).zip(shared.playlist_index(1)),
        Some((3, 4))
    );
}

#[test]
fn replacement_keeps_matching_favorites_and_drops_vanished_groups() {
    let mut browser = BrowserWorker::default();
    browser.load(fixture("categories.m3u"));
    let _ = wait(&mut browser, "");
    // Favorite the ABC channel of local.m3u before it is loaded: favorite
    // CNN here, then replace with a playlist where Sport is absent.
    let _ = browser.toggle_favorite(1, "").unwrap();
    let _ = wait(&mut browser, "");
    browser.select_category(Category::Group("Sport".to_owned()), "");
    let _ = wait(&mut browser, "");
    browser.load(fixture("local.m3u"));
    let replaced = wait(&mut browser, "");
    // Sport does not exist in local.m3u: the active category fell back.
    assert_eq!(browser.active_category(), &Category::All);
    assert_eq!(replaced.len(), 4);
    // The CNN favorite key matches nothing in the new playlist, so the
    // Favorites projection is empty rather than stale.
    browser.select_category(Category::Favorites, "");
    let favorites = wait(&mut browser, "");
    assert!(favorites.is_empty());
    assert!(favorites.status().contains("Favorites is empty"));
    assert_eq!(
        browser.favorites().len(),
        1,
        "stored keys survive replacement"
    );
}

#[test]
fn failed_load_keeps_the_active_category_projection() {
    let mut browser = BrowserWorker::default();
    browser.load(fixture("categories.m3u"));
    let _ = wait(&mut browser, "");
    browser.select_category(Category::Group("News".to_owned()), "");
    let _ = wait(&mut browser, "");
    browser.load(fixture("absent.m3u"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let recovered = loop {
        match browser.poll("") {
            Some(BrowseEvent::Recovery(_)) => break true,
            Some(BrowseEvent::Results(_)) => break false,
            None => {
                assert!(Instant::now() < deadline, "browse timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    assert!(recovered);
    browser.search(String::new());
    let kept = wait(&mut browser, "");
    assert_eq!(names(&kept), ["BBC One", "CNN"]);
    assert_eq!(
        browser.active_category(),
        &Category::Group("News".to_owned())
    );
}

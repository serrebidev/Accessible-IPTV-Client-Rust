use iptv_core::{
    category::Category,
    search::{SearchResults, SearchWorker},
    BrowseModel,
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
fn run(
    worker: &SearchWorker,
    model: &Arc<BrowseModel>,
    category: Category,
    query: &str,
) -> SearchResults {
    worker.submit_category(Arc::clone(model), query.to_owned(), category);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = worker.try_latest() {
            return result;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
fn fixture() -> Arc<BrowseModel> {
    Arc::new(
        BrowseModel::open_local(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/categories.m3u"),
        )
        .unwrap(),
    )
}
#[test]
fn categories_are_sorted_and_blank_groups_share_uncategorized() {
    let model = fixture();
    assert_eq!(
        model.categories(),
        &[
            Category::All,
            Category::Favorites,
            Category::Group("All Channels".into()),
            Category::Group("News".into()),
            Category::Group("Uncategorized".into()),
            Category::Group("zoo".into())
        ]
    );
    let worker = SearchWorker::default();
    let all = run(&worker, &model, Category::All, "");
    assert_eq!(all.len(), 7);
    let news = run(&worker, &model, Category::Group("News".into()), "");
    assert_eq!(news.channel(0).unwrap().tvg_id, "n1");
    assert_eq!(news.channel(1).unwrap().tvg_id, "n2");
    assert_eq!(news.restore_row(&all, Some(4)), Some(1));
    assert_eq!(news.restore_row(&all, Some(0)), Some(0));
    let ungrouped = run(&worker, &model, Category::Group("Uncategorized".into()), "");
    assert_eq!(
        (0..ungrouped.len())
            .map(|i| ungrouped.channel(i).unwrap().name.as_str())
            .collect::<Vec<_>>(),
        ["Missing", "Blank", "Explicit"]
    );
    let literal = run(&worker, &model, Category::Group("All Channels".into()), "");
    assert_eq!(literal.len(), 1);
}
#[test]
fn category_search_empty_clear_and_supersession_keep_real_rows_only() {
    let model = fixture();
    let worker = SearchWorker::default();
    let category = Category::Group("News".into());
    let news = run(&worker, &model, category.clone(), "");
    let none = run(&worker, &model, category.clone(), "absent");
    assert!(none.is_empty());
    assert_eq!(none.restore_row(&news, Some(1)), None);
    assert!(none.status().contains("News"));
    assert!(!none.status().contains("showing all channels"));
    let cleared = run(&worker, &model, category, "");
    assert_eq!(cleared.len(), 2);
    assert_eq!(cleared.restore_row(&news, Some(1)), Some(1));
    worker.submit_category(Arc::clone(&model), String::new(), Category::All);
    let empty = run(&worker, &model, Category::Group("Removed".into()), "");
    assert_eq!(empty.len(), 0);
    assert!(empty.channel(0).is_none());
    assert!(empty.status().contains("No channels in category Removed"));
    assert_eq!(empty.restore_row(&news, Some(1)), None);
}

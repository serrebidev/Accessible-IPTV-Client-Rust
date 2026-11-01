//! Shared Python-generated goldens: these checks exercise no desktop widgets.
use iptv_core::{search_channels, BrowseModel};
use std::path::Path;

const SOURCES: [&str; 4] = ["local.m3u", "recovery.m3u", "empty.m3u", "invalid-utf8.m3u"];

fn fixture(source: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(source)
}

#[test]
fn pinned_python_channel_fields_and_source_order_match() {
    for source in SOURCES {
        let model = BrowseModel::open_local(&fixture(source)).unwrap();
        let actual: Vec<_> = model
            .channels()
            .iter()
            .map(|channel| {
                [
                    source,
                    &channel.name,
                    &channel.group,
                    &channel.url,
                    &channel.tvg_id,
                    &channel.tvg_name,
                    ".",
                ]
                .join("\t")
            })
            .collect();
        let expected: Vec<_> = include_str!("fixtures/python-channels.tsv")
            .lines()
            .filter(|line| line.split('\t').next() == Some(source))
            .collect();
        assert_eq!(actual, expected, "{source}");
    }
}

#[test]
fn pinned_python_incremental_search_order_and_empty_outcomes_match() {
    for line in include_str!("fixtures/python-search.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        let model = BrowseModel::open_local(&fixture(fields[0])).unwrap();
        let found = search_channels(model.channels(), fields[1]);
        let actual = found
            .iter()
            .map(|channel| {
                model
                    .channels()
                    .iter()
                    .position(|entry| std::ptr::eq(entry, *channel))
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(actual, fields[2], "{} query {:?}", fields[0], fields[1]);
    }
    for line in include_str!("fixtures/python-outcomes.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let (source, expected) = line.split_once('\t').unwrap();
        let model = BrowseModel::open_local(&fixture(source)).unwrap();
        assert_eq!(
            if model.channels().is_empty() {
                "empty"
            } else {
                "loaded"
            },
            expected
        );
    }
}

#[test]
fn malformed_entries_keep_valid_rows_and_report_recovery_without_leaking_metadata() {
    let model = BrowseModel::open_local(&fixture("recovery.m3u")).unwrap();
    assert_eq!(model.channels().len(), 6);
    assert_eq!(model.malformed_entries(), 3); // overwritten metadata, broken quote, dangling entry
    assert_eq!(model.channels()[0].name, "Cat");
    assert_eq!(model.channels()[2].name, "Recovered");
    assert!(model.channels()[5].tvg_id.is_empty());
    assert_eq!(model.channels()[3].name, "\u{732b}cat");
}

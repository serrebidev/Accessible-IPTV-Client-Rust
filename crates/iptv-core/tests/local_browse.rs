use iptv_core::BrowseModel;
use std::path::Path;

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local.m3u")
}

#[test]
fn opens_local_playlist_in_source_order_with_metadata() {
    let model = BrowseModel::open_local(&fixture()).unwrap();
    let channels = model.channels();
    assert_eq!(
        channels.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["ABC East HD", "映画, HD", "Bob's Burgers, Season 1", ""]
    );
    assert_eq!(channels[0].tvg_id, "abc.us");
    assert_eq!(channels[0].tvg_name, "ABC East");
    assert_eq!(channels[0].tvg_logo, "http://logo.com/abc.png");
    assert_eq!(channels[0].group, "USA News");
    assert_eq!(channels[0].url, "http://stream.example.com/abc");
    assert_eq!(channels[1].group, "Movies");
    assert_eq!(
        channels[1].unknown_attributes["http-user-agent"],
        "Browser (KHTML, like Gecko)"
    );
    assert_eq!(channels[1].unknown_attributes["custom"], "kept");
    assert!(channels[3].group.is_empty());
    assert!(channels[3].unknown_attributes.is_empty());
    assert_eq!(channels[3].url, "https://example.invalid/plain");
}

#[test]
fn unsuccessful_open_leaves_current_browse_state_usable() {
    let model = BrowseModel::open_local(&fixture()).unwrap();
    let missing = fixture().join("missing.m3u");
    assert!(BrowseModel::open_local(&missing).is_err());
    assert_eq!(model.channels().len(), 4);
    assert_eq!(model.channels()[0].name, "ABC East HD");
}

#[test]
fn repeated_open_is_deterministic_and_does_not_append_previous_rows() {
    let first = BrowseModel::open_local(&fixture()).unwrap();
    let second = BrowseModel::open_local(&fixture()).unwrap();
    assert_eq!(first, second);
    assert_eq!(second.channels().len(), 4);
}

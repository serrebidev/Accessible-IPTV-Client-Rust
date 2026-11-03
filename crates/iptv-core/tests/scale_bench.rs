//! Scale measurements for audits B19 and E12 at 500,000 channels.
//!
//! These are benches, not gates: they are `#[ignore]`d so the normal
//! suite never pays for them. Run with:
//! `cargo test --release -p iptv-core --test scale_bench -- --ignored --nocapture`
//! Timings print to stdout; validation 0064 records the run made
//! when the navigation index landed.
use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    epg::{build_row_guide, EpgChannel, EpgGuide, GuideIndexes, Programme},
    navindex::NavIndex,
    Channel,
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    time::{Duration, Instant},
};

const CHANNELS: usize = 500_000;

fn synthetic_channels() -> Vec<Channel> {
    (0..CHANNELS)
        .map(|i| {
            let mut attributes = BTreeMap::new();
            attributes.insert("playlist-id".to_string(), format!("scope-{}", i % 10));
            Channel {
                name: format!("Channel {i} News"),
                group: format!("Group {}", i % 200),
                url: format!("http://example.test/{i}"),
                tvg_id: format!("ch{i}"),
                tvg_name: format!("Channel {i} News"),
                tvg_logo: String::new(),
                unknown_attributes: attributes,
            }
        })
        .collect()
}

fn wait_results(browser: &mut BrowserWorker, deadline: Duration) -> usize {
    let start = Instant::now();
    loop {
        if let Some(BrowseEvent::Results(results)) = browser.poll("") {
            return results.len();
        }
        assert!(start.elapsed() < deadline, "timed out waiting for results");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "scale bench: run explicitly with --ignored --nocapture"]
fn bench_b19_navigation_at_500k() {
    let channels = synthetic_channels();

    let start = Instant::now();
    let index = NavIndex::build(&channels);
    println!("B19 NavIndex::build over {CHANNELS}: {:?}", start.elapsed());

    let scope = index.scope_positions("scope-3").expect("scope exists");
    assert_eq!(scope.len(), CHANNELS / 10);
    let start = Instant::now();
    let hits = (0..CHANNELS)
        .filter(|&i| index.in_scope("scope-3", i))
        .count();
    println!(
        "B19 index membership sweep over {CHANNELS}: {:?} ({hits} hits)",
        start.elapsed()
    );
    let start = Instant::now();
    let scanned = channels
        .iter()
        .filter(|c| c.unknown_attributes.get("playlist-id").map(String::as_str) == Some("scope-3"))
        .count();
    println!(
        "B19 scan-path membership sweep: {:?} ({scanned} hits)",
        start.elapsed()
    );
    assert_eq!(hits, scanned);

    // End to end through the browse worker: ten source files of
    // 50k channels each (sources tag their channels with the
    // source's scope id), then real scope/category/search timings.
    let mut sources = Vec::new();
    let mut paths = Vec::new();
    for scope_nr in 0..10 {
        let mut m3u = String::with_capacity(50_000 * 96);
        m3u.push_str("#EXTM3U\n");
        for channel in channels
            .iter()
            .filter(|c| c.unknown_attributes["playlist-id"] == format!("scope-{scope_nr}"))
        {
            write!(
                m3u,
                "#EXTINF:-1 tvg-id=\"{}\" group-title=\"{}\",{}\n{}\n",
                channel.tvg_id, channel.group, channel.name, channel.url
            )
            .expect("write to string");
        }
        let path = std::env::temp_dir().join(format!("iptv-scale-bench-{scope_nr}.m3u"));
        std::fs::write(&path, m3u).expect("write bench playlist");
        sources.push(iptv_core::sources::PlaylistSource::new(
            path.to_string_lossy().into_owned(),
        ));
        paths.push(path);
    }
    let scope_three = sources[3].scope_id();

    let mut browser = BrowserWorker::default();
    let start = Instant::now();
    browser.load_sources(sources);
    let rows = wait_results(&mut browser, Duration::from_mins(5));
    println!(
        "B19 worker load_sources+first results ({rows} rows): {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    browser.set_scope(Some(scope_three), "");
    let rows = wait_results(&mut browser, Duration::from_mins(1));
    println!(
        "B19 set_scope + results ({rows} rows): {:?}",
        start.elapsed()
    );

    browser.set_scope(None, "");
    let _ = wait_results(&mut browser, Duration::from_mins(1));
    let start = Instant::now();
    browser.select_category(
        iptv_core::categories::Category::Group("Group 42".to_string()),
        "",
    );
    let rows = wait_results(&mut browser, Duration::from_mins(1));
    println!(
        "B19 category switch + results ({rows} rows): {:?}",
        start.elapsed()
    );

    browser.select_category(iptv_core::categories::Category::All, "");
    let _ = wait_results(&mut browser, Duration::from_mins(1));
    let start = Instant::now();
    browser.search("channel 499999 news".to_string());
    let rows = wait_results(&mut browser, Duration::from_mins(1));
    println!(
        "B19 rare-match search + results ({rows} rows): {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    browser.search("channel".to_string());
    let rows = wait_results(&mut browser, Duration::from_mins(2));
    println!(
        "B19 all-match search + results ({rows} rows): {:?}",
        start.elapsed()
    );

    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
#[ignore = "scale bench: run explicitly with --ignored --nocapture"]
fn bench_e12_bulk_guide_at_500k() {
    // Playlist names deliberately do NOT name-match the guide's
    // display names: matching runs through the tvg-id fast path,
    // which is the bulk pipeline E12 measures. (Name matching is
    // bucket-based, data-dependent, and a faithful port of
    // Python's matcher; uniform synthetic names would measure the
    // bucket scan, not the bulk snapshot path.)
    let channels: Vec<Channel> = synthetic_channels()
        .into_iter()
        .map(|mut channel| {
            channel.name = channel.name.replace("Channel", "Feed");
            channel.tvg_name.clone_from(&channel.name);
            channel
        })
        .collect();
    // A large guide: 30,000 guide channels, 8 programmes each,
    // matched to the first 30,000 playlist channels by tvg-id.
    let guide_channels: Vec<EpgChannel> = (0..30_000)
        .map(|i| EpgChannel {
            id: format!("ch{i}"),
            display_name: format!("Channel {i} News"),
        })
        .collect();
    let base = 1_700_000_000_i64;
    let mut programmes: HashMap<String, Vec<Programme>> = HashMap::new();
    for channel in &guide_channels {
        let rows = (0..8)
            .map(|slot| Programme {
                channel_id: channel.id.clone(),
                start: base + slot * 3600,
                stop: base + (slot + 1) * 3600,
                title: format!("Show {slot}"),
                description: format!("Description {slot}"),
            })
            .collect();
        programmes.insert(channel.id.clone(), rows);
    }
    let guide = EpgGuide {
        channels: guide_channels,
        programmes,
    };
    println!("E12 guide programmes: {}", guide.programme_count());

    let start = Instant::now();
    let indexes = GuideIndexes::build(&guide.channels);
    println!(
        "E12 GuideIndexes::build (30k guide channels): {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    let bulk = guide.bulk_now_next(base + 1800);
    println!(
        "E12 bulk_now_next ({} entries): {:?}",
        bulk.len(),
        start.elapsed()
    );

    let start = Instant::now();
    let (suffixes, descriptions) = build_row_guide(&channels, &indexes, &bulk);
    println!(
        "E12 build_row_guide over {CHANNELS} channels: {:?} ({} suffixes, {} descriptions)",
        start.elapsed(),
        suffixes.len(),
        descriptions.len()
    );
    assert_eq!(suffixes.len(), 30_000);
    assert_eq!(descriptions.len(), 30_000);
    // The steady-state refresh: matches cached, only the suffix
    // assembly reruns (what the per-minute tick pays).
    let cache_start = Instant::now();
    let cache = iptv_core::epg::GuideMatchCache::build(&channels, &indexes);
    println!(
        "E12 GuideMatchCache::build over {CHANNELS}: {:?}",
        cache_start.elapsed()
    );
    let start = Instant::now();
    let (suffixes2, descriptions2) = iptv_core::epg::row_guide_from_matches(cache.matches(), &bulk);
    println!(
        "E12 cached refresh (row_guide_from_matches): {:?} ({} suffixes)",
        start.elapsed(),
        suffixes2.len()
    );
    assert_eq!(suffixes2.len(), suffixes.len());
    assert_eq!(descriptions2.len(), descriptions.len());
}

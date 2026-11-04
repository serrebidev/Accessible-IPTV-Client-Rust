//! Startup load audit bench (validation 0068): Brandon's real
//! scale — a 115,651-channel playlist (~28 MB) — through the exact
//! worker path (prefill, cold load, parsed-cache hit, changed-text
//! reload) plus the model-side builders. Run with:
//! `cargo test -p iptv-core --test startup_load_bench --release -- --ignored --nocapture`
#![allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]

use std::fmt::Write as _;
use std::time::Instant;

use iptv_core::categories::{categories, category_rows, Category};
use iptv_core::favorites::FavoriteSet;
use iptv_core::loading::LoadFailure;
use iptv_core::navindex::NavIndex;
use iptv_core::sources::{
    load_merged_with, prefill_model, MergeOptions, PlaylistSource, SourceFetcher,
};
use iptv_core::BrowseModel;

const CHANNELS: usize = 115_651;
const GROUPS: usize = 120;

fn generate_playlist() -> String {
    let mut text = String::with_capacity(30_000_000);
    text.push_str("#EXTM3U x-tvg-url=\"http://example.invalid/guide.xml\"\n");
    let kinds = [
        "News", "Movies", "Sports", "Kids", "Music", "Series", "Docs", "Live",
    ];
    for i in 0..CHANNELS {
        let g = i % GROUPS;
        let kind = kinds[i % kinds.len()];
        write!(
            text,
            "#EXTINF:-1 tvg-id=\"ch{i}\" tvg-name=\"Channel {i} {kind}\" tvg-logo=\"http://example.invalid/logos/{i}.png\" group-title=\"Group {g}\",Channel {i} {kind}\nhttp://example.invalid/stream/{i}.m3u8\n"
        )
        .unwrap();
    }
    text
}

struct StubFetcher {
    text: std::sync::Mutex<String>,
}

impl SourceFetcher for StubFetcher {
    fn fetch(
        &self,
        _source: &PlaylistSource,
        _current: &dyn Fn() -> bool,
    ) -> Result<String, LoadFailure> {
        Ok(self.text.lock().unwrap().clone())
    }
}

#[test]
#[ignore = "audit bench: run explicitly in release mode"]
fn startup_load_at_real_scale() {
    let dir = std::env::temp_dir().join(format!("iptv-startup-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let file = dir.join("big.m3u");
    let text = generate_playlist();
    std::fs::write(&file, &text).unwrap();
    println!(
        "generate: {} channels, {:.1} MB",
        CHANNELS,
        text.len() as f64 / 1_000_000.0
    );

    let source = PlaylistSource::new("http://example.invalid/big.m3u");
    let sources = vec![source];
    let fetcher = StubFetcher {
        text: std::sync::Mutex::new(text.clone()),
    };
    let options = MergeOptions {
        cache_dir: Some(&cache_dir),
        previous: &[],
        force_refresh: false,
    };

    // Raw parse alone (local-file path).
    let t = Instant::now();
    let model = BrowseModel::open_local(&file).unwrap();
    println!("open_local (read + parse): {:?}", t.elapsed());
    let channels: &[iptv_core::Channel] = model.channels();

    let t = Instant::now();
    let cats = categories(channels);
    println!(
        "categories(): {:?} ({} categories)",
        t.elapsed(),
        cats.len()
    );

    let t = Instant::now();
    let rows = category_rows(channels, &Category::All, &FavoriteSet::default());
    println!(
        "category_rows(All): {:?} ({} rows)",
        t.elapsed(),
        rows.len()
    );

    let t = Instant::now();
    let index = NavIndex::build(channels);
    println!(
        "NavIndex::build: {:?} ({} names)",
        t.elapsed(),
        index.normalized_names().len()
    );

    // Cold merged load: hash + parse + parsed-cache write + dedupe.
    let t = Instant::now();
    let merged = load_merged_with(&sources, &fetcher, options, || true)
        .unwrap()
        .unwrap();
    println!(
        "load_merged cold (parse + cache write): {:?} ({} channels)",
        t.elapsed(),
        merged.model.channels().len()
    );
    let mut cache_bytes = 0u64;
    for entry in std::fs::read_dir(&cache_dir).unwrap() {
        cache_bytes += entry.unwrap().metadata().unwrap().len();
    }
    println!(
        "cache dir after cold load: {:.1} MB",
        cache_bytes as f64 / 1_000_000.0
    );

    // Warm merged load: download cache + parsed cache both hit.
    let t = Instant::now();
    let merged = load_merged_with(&sources, &fetcher, options, || true)
        .unwrap()
        .unwrap();
    println!(
        "load_merged warm (parsed-cache hit): {:?} ({} channels)",
        t.elapsed(),
        merged.model.channels().len()
    );

    // Prefill alone: what the user sees first at startup.
    let t = Instant::now();
    let prefill = prefill_model(&sources, &cache_dir).unwrap();
    println!(
        "prefill_model: {:?} ({} channels)",
        t.elapsed(),
        prefill.channels().len()
    );

    // Upstream change: one channel renamed in the text. The download
    // cache must be bypassed (fresh fetch in production terms) and
    // the parsed cache misses on the new hash.
    let changed = text.replacen("Channel 42 News", "Channel 42 News RENAMED", 1);
    *fetcher.text.lock().unwrap() = changed;
    let fresh = MergeOptions {
        force_refresh: true,
        ..options
    };
    let t = Instant::now();
    let merged = load_merged_with(&sources, &fetcher, fresh, || true)
        .unwrap()
        .unwrap();
    println!(
        "load_merged changed text (full re-parse + cache rewrite): {:?} ({} channels)",
        t.elapsed(),
        merged.model.channels().len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ----- EPG side at Brandon's real scale: ~813k programmes. -----

const GUIDE_CHANNELS_EPG: usize = 2_400;
const SLOTS_PER_GUIDE_CHANNEL: usize = 339; // 813,600 programmes, ~7 days
const EPOCH_2026_11_02: i64 = 1_793_577_600;
const AT_MID_WINDOW: i64 = 1_793_794_500; // 2026-11-04 12:15 UTC

fn xmltv_time_from(base: i64, slot: usize) -> String {
    let secs = base + (slot as i64) * 1800;
    let day = secs / 86_400;
    let tod = secs % 86_400;
    // Civil date from epoch days (Howard Hinnant's algorithm).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{year:04}{m:02}{d:02}{:02}{:02}00 +0000",
        tod / 3600,
        (tod % 3600) / 60
    )
}

fn generate_big_guide() -> String {
    let mut xml = String::with_capacity(170_000_000);
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><tv>");
    for c in 0..GUIDE_CHANNELS_EPG {
        write!(
            xml,
            "<channel id=\"g{c}\"><display-name>Guide Channel {c}</display-name></channel>"
        )
        .unwrap();
    }
    let shows = [
        "Morning News",
        "The Daily Show",
        "Movie: Night Falls",
        "Cooking Masters",
        "World Football Live",
        "Documentary Earth",
        "Late Night Talk",
        "Kids Cartoons",
    ];
    for c in 0..GUIDE_CHANNELS_EPG {
        for s in 0..SLOTS_PER_GUIDE_CHANNEL {
            let title = shows[(c + s) % shows.len()];
            let start = xmltv_time_from(EPOCH_2026_11_02, s);
            let stop = xmltv_time_from(EPOCH_2026_11_02, s + 1);
            write!(
                xml,
                "<programme channel=\"g{c}\" start=\"{start}\" stop=\"{stop}\"><title>{title} {s}</title><desc>Episode description for slot {s} on channel {c}.</desc></programme>"
            )
            .unwrap();
        }
    }
    xml.push_str("</tv>");
    xml
}

#[test]
#[ignore = "audit bench: run explicitly in release mode"]
#[allow(clippy::too_many_lines)]
fn epg_now_next_at_real_scale() {
    use iptv_core::epg::{parse_xmltv_checked, GuideIndexes};
    use iptv_core::epgstore::EpgStore;

    // The store persists between runs of this bench (import is the
    // slow part and is not what this section measures twice).
    let dir = std::env::temp_dir().join("iptv-startup-bench-store");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("epg.db");
    let mut store = EpgStore::open(&db).unwrap();
    let marker = dir.join("import-complete");
    let existing = store.channels().unwrap().len();
    if !marker.exists() || existing != GUIDE_CHANNELS_EPG {
        let xml = generate_big_guide();
        println!(
            "generate big guide: {} programmes, {:.0} MB",
            GUIDE_CHANNELS_EPG * SLOTS_PER_GUIDE_CHANNEL,
            xml.len() as f64 / 1_000_000.0
        );
        let guide = parse_xmltv_checked(&xml).unwrap();
        let t = Instant::now();
        let count = store
            .import_source("bench", &guide, EPOCH_2026_11_02)
            .unwrap();
        println!("import_source (813k): {:?} ({count})", t.elapsed());
        std::fs::write(&marker, "done").unwrap();
    } else {
        println!("reusing populated store ({existing} channels)");
    }

    let t = Instant::now();
    let channels = store.channels().unwrap();
    println!(
        "store.channels(): {:?} ({} channels)",
        t.elapsed(),
        channels.len()
    );

    let t = Instant::now();
    let indexes = GuideIndexes::build(&channels);
    println!("GuideIndexes::build (store channels): {:?}", t.elapsed());
    drop(indexes);

    let t = Instant::now();
    let snapshot = store.all_now_next(AT_MID_WINDOW).unwrap();
    println!(
        "store.all_now_next(mid-window): {:?} ({} channels)",
        t.elapsed(),
        snapshot.len()
    );

    // The per-minute row-suffix refresh pipeline (the work
    // refresh_epg_suffixes does): bulk now/next for the in-memory
    // guide, the playlist-wide guide match, and the row build.
    // Timed here at the same scale because this pipeline is the
    // audit target for playback-time UI stalls.
    let xml = generate_big_guide();
    let t = Instant::now();
    let guide = parse_xmltv_checked(&xml).unwrap();
    println!(
        "parse big guide (in-memory path): {:?} ({} programmes)",
        t.elapsed(),
        guide.programme_count()
    );
    let t = Instant::now();
    let guide_indexes = GuideIndexes::build(&guide.channels);
    println!("GuideIndexes::build (guide channels): {:?}", t.elapsed());

    let dir2 = std::env::temp_dir().join(format!("iptv-suffix-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir2);
    std::fs::create_dir_all(&dir2).unwrap();
    let file2 = dir2.join("big.m3u");
    std::fs::write(&file2, generate_playlist()).unwrap();
    let model2 = BrowseModel::open_local(&file2).unwrap();
    let playlist_channels: &[iptv_core::Channel] = model2.channels();
    println!("playlist channels: {}", playlist_channels.len());

    let t = Instant::now();
    let bulk = guide.bulk_now_next(AT_MID_WINDOW);
    println!(
        "guide.bulk_now_next(mid-window): {:?} ({} channels)",
        t.elapsed(),
        bulk.len()
    );

    let t = Instant::now();
    let cache = iptv_core::epg::GuideMatchCache::build(playlist_channels, &guide_indexes);
    let matched = cache.matches().iter().filter(|m| m.is_some()).count();
    println!(
        "GuideMatchCache::build (115k, worst case ids/names disjoint): {:?} ({matched} matched)",
        t.elapsed()
    );

    let t = Instant::now();
    let (suffixes, descriptions) = iptv_core::epg::row_guide_from_matches(cache.matches(), &bulk);
    println!(
        "row_guide_from_matches: {:?} ({} suffixes, {} descriptions)",
        t.elapsed(),
        suffixes.len(),
        descriptions.len()
    );

    // Warm repeat (the steady-state per-minute refresh): bulk +
    // row build with the match cache already built.
    let t = Instant::now();
    let bulk2 = guide.bulk_now_next(AT_MID_WINDOW);
    let (suffixes2, _) = iptv_core::epg::row_guide_from_matches(cache.matches(), &bulk2);
    println!(
        "warm refresh (bulk + row build): {:?} ({} suffixes)",
        t.elapsed(),
        suffixes2.len()
    );
    let _ = std::fs::remove_dir_all(&dir2);

    // Guide merge at delivery (poll_epg runs this on the UI
    // thread when an import lands over an in-memory guide):
    // merge a full second copy of the guide into the first.
    let mut merged_guide = guide;
    let t = Instant::now();
    let second = parse_xmltv_checked(&xml).unwrap();
    println!("parse big guide (second copy): {:?}", t.elapsed());
    let t = Instant::now();
    merged_guide.merge(second);
    println!(
        "EpgGuide::merge (full 813k guide into existing): {:?} ({} programmes after)",
        t.elapsed(),
        merged_guide.programme_count()
    );
    let with_current = snapshot
        .iter()
        .filter(|(_, nn)| nn.current.is_some())
        .count();
    let with_next = snapshot.iter().filter(|(_, nn)| nn.next.is_some()).count();
    println!("  current set: {with_current}, next set: {with_next}");
    if let Some((channel, nn)) = snapshot.first() {
        println!(
            "  sample {}: current={:?} next={:?}",
            channel.id,
            nn.current.as_ref().map(|p| p.title.as_str()),
            nn.next.as_ref().map(|p| p.title.as_str())
        );
    }
}

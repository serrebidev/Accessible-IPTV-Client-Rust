//! EPG load-path timings at real-world scale (audit, 2026-11-03):
//! parse, store import (fresh + refresh), store reopen, guide
//! indexes and the playlist match cache, on one generated guide of
//! 2,000 channels x 336 programmes (672k programmes, 7 days of
//! half-hour slots). Run with:
//! `cargo test -p iptv-core --test epg_load_bench -- --ignored --nocapture`
#![allow(clippy::print_stdout)]

use std::time::Instant;

use iptv_core::epg::{parse_xmltv_checked, GuideIndexes, GuideMatchCache};
use iptv_core::epgstore::EpgStore;
use iptv_core::Channel;

use std::fmt::Write as _;

const GUIDE_CHANNELS: usize = 2_000;
const SLOTS_PER_CHANNEL: usize = 336;

fn xmltv_time(day: usize, slot: usize) -> String {
    // 2026-11-02 + day, slot = half hour of day (wraps into next days
    // through the day count; SLOTS_PER_CHANNEL spans 7 days).
    let total_day = day + slot / 48;
    let within = slot % 48;
    format!(
        "202611{:02}{:02}{:02}00 +0000",
        2 + total_day,
        within / 2,
        (within % 2) * 30
    )
}

fn generate_guide() -> String {
    let mut xml = String::with_capacity(180_000_000);
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><tv>");
    for c in 0..GUIDE_CHANNELS {
        write!(
            xml,
            "<channel id=\"ch{c}\"><display-name>Channel {c} Network</display-name></channel>"
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
    for c in 0..GUIDE_CHANNELS {
        for s in 0..SLOTS_PER_CHANNEL {
            let title = shows[(c + s) % shows.len()];
            let start = xmltv_time(0, s);
            let stop = xmltv_time(0, s + 1);
            write!(
                xml,
                "<programme channel=\"ch{c}\" start=\"{start}\" stop=\"{stop}\"><title>{title} {s}</title><desc>Episode description for slot {s} on channel {c}.</desc></programme>"
            )
            .unwrap();
        }
    }
    xml.push_str("</tv>");
    xml
}

fn playlist_channels(n: usize) -> Vec<Channel> {
    (0..n)
        .map(|i| Channel {
            name: format!("Channel {} Network", i % GUIDE_CHANNELS),
            group: String::new(),
            url: format!("http://example.invalid/{i}"),
            tvg_id: format!("ch{}", i % GUIDE_CHANNELS),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        })
        .collect()
}

#[test]
#[ignore = "scale bench: run explicitly with --ignored --nocapture"]
fn bench_epg_load_stages() {
    let dir = std::env::temp_dir().join(format!("iptv-epg-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("epg.db");

    let start = Instant::now();
    let xml = generate_guide();
    println!(
        "generate: {:?} ({} MB of XML)",
        start.elapsed(),
        xml.len() / 1_000_000
    );

    let start = Instant::now();
    let guide = parse_xmltv_checked(&xml).unwrap();
    println!(
        "parse_xmltv_checked: {:?} ({} channels, {} programmes)",
        start.elapsed(),
        guide.channels.len(),
        guide.programme_count()
    );

    let start = Instant::now();
    let mut store = EpgStore::open(&db).unwrap();
    println!("store open (fresh): {:?}", start.elapsed());

    let now = 1_793_500_000_i64;
    let start = Instant::now();
    let count = store.import_source("bench", &guide, now).unwrap();
    println!(
        "import_source (fresh): {:?} ({count} programmes)",
        start.elapsed()
    );

    let start = Instant::now();
    let reopened = EpgStore::open(&db).unwrap();
    println!(
        "store open (populated, migrate path): {:?}",
        start.elapsed()
    );
    drop(reopened);

    let start = Instant::now();
    let count = store.import_source("bench", &guide, now + 3600).unwrap();
    println!(
        "import_source (refresh, same data): {:?} ({count})",
        start.elapsed()
    );

    let start = Instant::now();
    let indexes = GuideIndexes::build(&guide.channels);
    println!("GuideIndexes::build: {:?}", start.elapsed());

    for n in [2_000_usize, 50_000] {
        let channels = playlist_channels(n);
        let start = Instant::now();
        let cache = GuideMatchCache::build(&channels, &indexes);
        let matched = (0..n).filter(|&i| cache.get(i).is_some()).count();
        println!(
            "GuideMatchCache::build over {n} playlist channels: {:?} ({matched} matched)",
            start.elapsed()
        );
    }

    let start = Instant::now();
    let snapshot = store.all_now_next(now + 7200).unwrap();
    println!(
        "store.all_now_next: {:?} ({} channels)",
        start.elapsed(),
        snapshot.len()
    );

    std::fs::remove_dir_all(&dir).ok();
}

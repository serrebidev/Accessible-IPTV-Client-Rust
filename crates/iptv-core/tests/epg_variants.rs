//! Golden XMLTV variant fixtures (audit E06): the shapes Python's
//! importer accepts — `end` instead of `stop`, `<length>` and
//! `<duration>` derived end times, prefixed namespaces and CDATA
//! text — must retain the same times and text in the Rust parse.

use iptv_core::epg::{parse_xmltv_checked, parse_xmltv_time};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("fixture readable")
}

fn at(text: &str) -> i64 {
    parse_xmltv_time(text).expect("timestamp")
}

#[test]
fn xmltv_variants_retain_times_and_text() {
    let guide = parse_xmltv_checked(&fixture("xmltv-variants.xml")).expect("variants parse");
    // Prefixed channel elements still register, with names.
    assert_eq!(guide.channels.len(), 2);
    assert_eq!(guide.channels[0].display_name, "Variant One");
    assert_eq!(guide.programme_count(), 5);

    let var1 = &guide.programmes["var1"];
    // end= is honoured as the stop time.
    assert_eq!(var1[0].title, "End Attr Show");
    assert_eq!(var1[0].stop, at("20261101190000 +0000"));
    // CDATA title/description survive; <length units="minutes">
    // derives the end.
    assert_eq!(var1[1].title, "Length Minutes Show");
    assert_eq!(var1[1].description, "CDATA description survives.");
    assert_eq!(var1[1].stop, at("20261101193000 +0000"));
    // <length units="seconds"> derives the end.
    assert_eq!(var1[2].title, "Length Seconds Show");
    assert_eq!(var1[2].stop, at("20261101200000 +0000"));

    let var2 = &guide.programmes["var2"];
    // ISO 8601 <duration> derives the end.
    assert_eq!(var2[0].title, "ISO Duration Show");
    assert_eq!(var2[0].stop, at("20261101213000 +0000"));
    // A bare-digit <duration> is minutes.
    assert_eq!(var2[1].title, "Bare Duration Show");
    assert_eq!(var2[1].stop, at("20261101221500 +0000"));
}

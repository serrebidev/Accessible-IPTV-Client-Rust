//! Regenerates `po/iptv-app.pot` from every translatable package
//! in the workspace (iptv-app and iptv-core). Run from the
//! workspace root:
//!
//! `cargo run -p iptv-app --example gen_pot`
//!
//! Then refresh each catalog with
//! `msgmerge --update --backup=none --no-fuzzy-matching po/<lang>.po po/iptv-app.pot`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    patois_build::gen_pot(".", "crates/iptv-app/po", "iptv-app")
}

//! Link fix for the `wx` `MediaCtrl` Linux backend.
//!
//! wxdragon-sys 0.9.24 probes `gstreamer-video-1.0` for its Linux media
//! support, but the wxWidgets GTK media backend calls the `GstPlayer` API
//! (`gst_player_*`), which lives in `gstreamer-player-1.0`. Until upstream
//! probes the right pkg-config module, the app links it directly.
fn main() {
    // Compile the gettext catalogs in `po/` to the `locale/` tree
    // `patois::embed_domain!` reads at compile time. Without the
    // gettext tools on PATH the build still succeeds, with a Cargo
    // warning and no translations (patois-build's contract).
    patois_build::compile_translations("po", "locale");
    // The updater's minisign public key: line 2 of the committed
    // public key file (the secret half lives only in the release
    // pipeline's MINISIGN_KEY secret and the owner's vault).
    let pub_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/minisign.pub");
    println!("cargo:rerun-if-changed={}", pub_path.display());
    let text = std::fs::read_to_string(&pub_path).expect("assets/minisign.pub is committed");
    let key = text
        .lines()
        .nth(1)
        .expect("minisign.pub holds the key on line 2")
        .trim();
    println!("cargo:rustc-env=IPTV_MINISIGN_PUBLIC_KEY={key}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
}

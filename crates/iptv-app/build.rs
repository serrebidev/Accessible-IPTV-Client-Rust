//! Link fix for the `wx` `MediaCtrl` Linux backend.
//!
//! wxdragon-sys 0.9.24 probes `gstreamer-video-1.0` for its Linux media
//! support, but the wxWidgets GTK media backend calls the `GstPlayer` API
//! (`gst_player_*`), which lives in `gstreamer-player-1.0`. Until upstream
//! probes the right pkg-config module, the app links it directly.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
}

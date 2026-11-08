//! Link fix for the `wx` `MediaCtrl` Linux backend, same as the
//! app crate's build script: wxdragon-sys probes
//! `gstreamer-video-1.0`, but the wxWidgets GTK media backend
//! calls the `GstPlayer` API in `gstreamer-player-1.0`.
//!
//! This crate needs the directive too: in a workspace build,
//! Cargo unifies the app crate's `patois/wx` feature into this
//! crate's patois dependency, so this crate's test binaries link
//! wxdragon-sys without the app crate's build script applying.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
}

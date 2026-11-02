//! libVLC playback engine, loaded dynamically (port of the Python
//! client's embedded VLC player).
//!
//! This crate is the workspace's one FFI exception: it does not
//! forbid `unsafe_code`, because dlopen'd C calls are its whole
//! job. The surface stays small and every call is wrapped.
//! client's embedded VLC player).
//!
//! libVLC is dlopen'd at first use so its absence never breaks
//! startup: the caller falls back to the wx `MediaCtrl` engine.
//! Headers map to libVLC media options where libVLC has them
//! (`http-user-agent`, `http-referrer`); libVLC 3 has no generic
//! header option, so other headers are the external players' job,
//! exactly as with Python's embedded player.

use std::ffi::{c_char, c_int, c_void, CString};

use libloading::Library;

use iptv_core::playback::StreamHeaders;

type NewFn = unsafe extern "C" fn(c_int, *const *const c_char) -> *mut c_void;
type ReleaseFn = unsafe extern "C" fn(*mut c_void);
type PlayerNewFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type PlayerReleaseFn = unsafe extern "C" fn(*mut c_void);
type MediaNewFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type MediaOptionFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type MediaReleaseFn = unsafe extern "C" fn(*mut c_void);
type SetMediaFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type PlayFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type VoidFn = unsafe extern "C" fn(*mut c_void);
type SetTimeFn = unsafe extern "C" fn(*mut c_void, i64);
type GetTimeFn = unsafe extern "C" fn(*mut c_void) -> i64;
type GetStateFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SetVolumeFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type GetTrackCountFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type GetTrackDescFn = unsafe extern "C" fn(*mut c_void) -> *mut TrackDescription;
type SetTrackFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type TrackDescReleaseFn = unsafe extern "C" fn(*mut TrackDescription);
type SetHwndFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type SetNsObjectFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

#[repr(C)]
struct TrackDescription {
    id: c_int,
    name: *mut c_char,
    next: *mut TrackDescription,
}

/// libVLC player states (`libvlc_state_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VlcState {
    Nothing,
    Opening,
    Buffering,
    Playing,
    Paused,
    Stopped,
    Ended,
    Error,
}

fn map_state(raw: c_int) -> VlcState {
    match raw {
        1 => VlcState::Opening,
        2 => VlcState::Buffering,
        3 => VlcState::Playing,
        4 => VlcState::Paused,
        5 => VlcState::Stopped,
        6 => VlcState::Ended,
        7 => VlcState::Error,
        _ => VlcState::Nothing,
    }
}

/// Candidate library names per platform, most specific first.
fn library_candidates() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["libvlc.dll"]
    } else if cfg!(target_os = "macos") {
        vec![
            "libvlc.dylib",
            "/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib",
            "/opt/homebrew/lib/libvlc.dylib",
            "/usr/local/lib/libvlc.dylib",
        ]
    } else {
        vec!["libvlc.so.5", "libvlc.so"]
    }
}

/// Whether a libVLC library can be loaded on this machine.
#[must_use]
pub fn is_available() -> bool {
    load_library().is_ok()
}

fn load_library() -> Result<Library, String> {
    let mut last = String::new();
    for name in library_candidates() {
        match unsafe { Library::new(name) } {
            Ok(lib) => return Ok(lib),
            Err(error) => last = format!("{name}: {error}"),
        }
    }
    Err(format!("libVLC not found ({last})"))
}

struct Functions {
    release: ReleaseFn,
    player_release: PlayerReleaseFn,
    media_new_location: MediaNewFn,
    media_add_option: MediaOptionFn,
    media_release: MediaReleaseFn,
    set_media: SetMediaFn,
    play: PlayFn,
    pause: VoidFn,
    stop: VoidFn,
    set_time: SetTimeFn,
    get_time: GetTimeFn,
    get_length: GetTimeFn,
    get_state: GetStateFn,
    set_volume: SetVolumeFn,
    get_track_count: GetTrackCountFn,
    get_track_description: GetTrackDescFn,
    set_track: SetTrackFn,
    track_description_release: TrackDescReleaseFn,
    set_hwnd: Option<SetHwndFn>,
    set_nsobject: Option<SetNsObjectFn>,
}

/// A loaded libVLC instance with one media player.
pub struct VlcEngine {
    _lib: Library,
    instance: *mut c_void,
    player: *mut c_void,
    fns: Functions,
}

// The engine is used only from the UI thread; raw handles never
// leave it.
unsafe impl Send for VlcEngine {}

impl VlcEngine {
    /// Load libVLC and create the instance and player.
    ///
    /// # Errors
    /// Returns the load or symbol error when libVLC is missing or
    /// too old, and a message when instance creation fails.
    pub fn new() -> Result<Self, String> {
        let lib = load_library()?;
        macro_rules! sym {
            ($name:expr, $ty:ty) => {
                *unsafe { lib.get::<$ty>($name) }
                    .map_err(|e| format!("libVLC symbol {}: {e}", String::from_utf8_lossy($name)))?
            };
        }
        let new_instance: NewFn = sym!(b"libvlc_new", NewFn);
        let instance = unsafe { new_instance(0, std::ptr::null()) };
        if instance.is_null() {
            return Err("libVLC instance creation failed.".to_string());
        }
        let player_new: PlayerNewFn = sym!(b"libvlc_media_player_new", PlayerNewFn);
        let player = unsafe { player_new(instance) };
        if player.is_null() {
            return Err("libVLC player creation failed.".to_string());
        }
        let fns = Functions {
            release: sym!(b"libvlc_release", ReleaseFn),
            player_release: sym!(b"libvlc_media_player_release", PlayerReleaseFn),
            media_new_location: sym!(b"libvlc_media_new_location", MediaNewFn),
            media_add_option: sym!(b"libvlc_media_add_option", MediaOptionFn),
            media_release: sym!(b"libvlc_media_release", MediaReleaseFn),
            set_media: sym!(b"libvlc_media_player_set_media", SetMediaFn),
            play: sym!(b"libvlc_media_player_play", PlayFn),
            pause: sym!(b"libvlc_media_player_pause", VoidFn),
            stop: sym!(b"libvlc_media_player_stop", VoidFn),
            set_time: sym!(b"libvlc_media_player_set_time", SetTimeFn),
            get_time: sym!(b"libvlc_media_player_get_time", GetTimeFn),
            get_length: sym!(b"libvlc_media_player_get_length", GetTimeFn),
            get_state: sym!(b"libvlc_media_player_get_state", GetStateFn),
            set_volume: sym!(b"libvlc_audio_set_volume", SetVolumeFn),
            get_track_count: sym!(b"libvlc_audio_get_track_count", GetTrackCountFn),
            get_track_description: sym!(b"libvlc_audio_get_track_description", GetTrackDescFn),
            set_track: sym!(b"libvlc_audio_set_track", SetTrackFn),
            track_description_release: sym!(
                b"libvlc_track_description_list_release",
                TrackDescReleaseFn
            ),
            set_hwnd: unsafe { lib.get::<SetHwndFn>(b"libvlc_media_player_set_hwnd") }
                .ok()
                .map(|s| *s),
            set_nsobject: unsafe { lib.get::<SetNsObjectFn>(b"libvlc_media_player_set_nsobject") }
                .ok()
                .map(|s| *s),
        };
        Ok(Self {
            _lib: lib,
            instance,
            player,
            fns,
        })
    }

    /// Point the video output at a native window handle (Windows
    /// HWND, macOS `NSView`), passed as an opaque address. On Linux
    /// libVLC opens its own video window; the handle is ignored
    /// there.
    pub fn set_window_handle(&mut self, handle: usize) {
        if handle == 0 {
            return;
        }
        let pointer = handle as *mut c_void;
        if cfg!(windows) {
            if let Some(set) = self.fns.set_hwnd {
                unsafe { set(self.player, pointer) };
            }
        } else if cfg!(target_os = "macos") {
            if let Some(set) = self.fns.set_nsobject {
                unsafe { set(self.player, pointer) };
            }
        }
    }

    /// Load and play a URL with the stream's headers.
    ///
    /// # Errors
    /// Returns a message when the URL is invalid, libVLC cannot
    /// create the media, or playback is refused.
    pub fn play_url(&mut self, url: &str, headers: &StreamHeaders) -> Result<(), String> {
        let c_url = CString::new(url).map_err(|_| "URL contains a NUL byte.".to_string())?;
        let media = unsafe { (self.fns.media_new_location)(self.instance, c_url.as_ptr()) };
        if media.is_null() {
            return Err("libVLC could not create media.".to_string());
        }
        for (option, header) in [
            (":http-user-agent=", "User-Agent"),
            (":http-referrer=", "Referer"),
        ] {
            if let Some(value) = headers.get(header) {
                if let Ok(option) = CString::new(format!("{option}{value}")) {
                    unsafe { (self.fns.media_add_option)(media, option.as_ptr()) };
                }
            }
        }
        unsafe {
            (self.fns.set_media)(self.player, media);
            (self.fns.media_release)(media);
        }
        if unsafe { (self.fns.play)(self.player) } != 0 {
            return Err("libVLC refused to play.".to_string());
        }
        Ok(())
    }

    /// Resume playback.
    pub fn play(&mut self) {
        unsafe { (self.fns.play)(self.player) };
    }

    /// Pause (libVLC's pause toggles the paused state).
    pub fn pause(&mut self) {
        unsafe { (self.fns.pause)(self.player) };
    }

    /// Stop playback.
    pub fn stop(&mut self) {
        unsafe { (self.fns.stop)(self.player) };
    }

    /// Seek to a position in milliseconds.
    pub fn set_time(&mut self, ms: i64) {
        unsafe { (self.fns.set_time)(self.player, ms) };
    }

    /// Current position in milliseconds (-1 when unknown).
    #[must_use]
    pub fn time(&self) -> i64 {
        unsafe { (self.fns.get_time)(self.player) }
    }

    /// Media length in milliseconds (-1 when unknown).
    #[must_use]
    pub fn length(&self) -> i64 {
        unsafe { (self.fns.get_length)(self.player) }
    }

    /// Current player state.
    #[must_use]
    pub fn state(&self) -> VlcState {
        map_state(unsafe { (self.fns.get_state)(self.player) })
    }

    /// Set the volume (0–100, libVLC's nominal scale).
    pub fn set_volume(&mut self, percent: i32) {
        unsafe { (self.fns.set_volume)(self.player, percent.clamp(0, 100)) };
    }

    /// The audio tracks as (id, name) pairs.
    #[must_use]
    pub fn audio_tracks(&self) -> Vec<(i32, String)> {
        let mut tracks = Vec::new();
        if unsafe { (self.fns.get_track_count)(self.player) } <= 0 {
            return tracks;
        }
        let head = unsafe { (self.fns.get_track_description)(self.player) };
        if head.is_null() {
            return tracks;
        }
        let mut cursor = head;
        while !cursor.is_null() {
            let entry = unsafe { &*cursor };
            let name = if entry.name.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(entry.name) }
                    .to_string_lossy()
                    .into_owned()
            };
            tracks.push((entry.id, name));
            cursor = entry.next;
        }
        unsafe { (self.fns.track_description_release)(head) };
        tracks
    }

    /// Select an audio track by id.
    pub fn set_audio_track(&mut self, id: i32) -> bool {
        (unsafe { (self.fns.set_track)(self.player, id) }) == 0
    }
}

impl Drop for VlcEngine {
    fn drop(&mut self) {
        unsafe {
            (self.fns.player_release)(self.player);
            (self.fns.release)(self.instance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{map_state, VlcState};

    #[test]
    fn state_mapping_matches_libvlc() {
        assert_eq!(map_state(3), VlcState::Playing);
        assert_eq!(map_state(6), VlcState::Ended);
        assert_eq!(map_state(7), VlcState::Error);
        assert_eq!(map_state(0), VlcState::Nothing);
        assert_eq!(map_state(99), VlcState::Nothing);
    }

    #[test]
    fn engine_load_is_graceful_without_libvlc() {
        // With libVLC installed this loads; without, it must error,
        // never panic.
        match super::VlcEngine::new() {
            Ok(mut engine) => {
                assert_eq!(engine.state(), VlcState::Nothing);
                engine.stop();
            }
            Err(error) => assert!(!error.is_empty()),
        }
    }
}

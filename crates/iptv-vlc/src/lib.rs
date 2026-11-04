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

use iptv_core::playback::{AvCounters, StreamHeaders};

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
type AudioOutputEnumFn = unsafe extern "C" fn(*mut c_void) -> *mut AudioOutputDevice;
type AudioOutputReleaseFn = unsafe extern "C" fn(*mut AudioOutputDevice);
type AudioOutputSetFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type SetNsObjectFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type GetMediaFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type MediaGetStatsFn = unsafe extern "C" fn(*mut c_void, *mut MediaStats) -> c_int;
type GetSpuFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SetSpuFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type AddSlaveFn = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, bool) -> c_int;
type GetSpuDelayFn = unsafe extern "C" fn(*mut c_void) -> i64;

/// `libvlc_media_stats_t` (field order per libvlc headers).
#[repr(C)]
#[derive(Default)]
struct MediaStats {
    read_bytes: c_int,
    input_bitrate: f32,
    demux_read_bytes: c_int,
    demux_bitrate: f32,
    decoded_video: c_int,
    decoded_audio: c_int,
    displayed_pictures: c_int,
    lost_pictures: c_int,
    played_abuffers: c_int,
    lost_abuffers: c_int,
    sent_packets: c_int,
    sent_bytes: c_int,
    send_bitrate: f32,
}

/// `libvlc_audio_output_device_t` (linked list node).
#[repr(C)]
struct AudioOutputDevice {
    device: *mut c_char,
    description: *mut c_char,
    next: *mut AudioOutputDevice,
}

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

/// Read a libVLC-owned C string as a lossy Rust string; null
/// reads as empty.
///
/// # Safety
/// `ptr` must be null or point at a NUL-terminated string that
/// stays alive for the call (libVLC list nodes qualify until
/// their list is released).
unsafe fn c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// The plugins directory bundled beside the executable, when the
/// package shipped one.
fn bundled_plugins_dir() -> Option<std::path::PathBuf> {
    plugins_dir_in(&exe_dir()?)
}

fn plugins_dir_in(exe_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = exe_dir.join("plugins");
    dir.is_dir().then_some(dir)
}

/// The directory the running executable lives in, when known.
/// Release packages bundle libVLC there (see PACKAGING.md).
fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
}

/// Candidate library paths, most specific first. A copy bundled
/// beside the executable wins over any system install, so the
/// shipped packages play without VLC being installed.
fn library_candidates() -> Vec<String> {
    candidates_from(exe_dir().as_deref())
}

fn candidates_from(dir: Option<&std::path::Path>) -> Vec<String> {
    let names: &[&str] = if cfg!(windows) {
        &["libvlc.dll"]
    } else if cfg!(target_os = "macos") {
        &["libvlc.dylib"]
    } else {
        &["libvlc.so.5", "libvlc.so"]
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(dir) = dir {
        for name in names {
            out.push(dir.join(name).to_string_lossy().into_owned());
        }
    }
    out.extend(names.iter().map(|name| (*name).to_string()));
    if cfg!(target_os = "macos") {
        out.push("/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib".to_string());
        out.push("/opt/homebrew/lib/libvlc.dylib".to_string());
        out.push("/usr/local/lib/libvlc.dylib".to_string());
    }
    out
}

/// Whether a libVLC library can be loaded on this machine.
#[must_use]
pub fn is_available() -> bool {
    load_library().is_ok()
}

fn load_library() -> Result<Library, String> {
    let mut last = String::new();
    for name in library_candidates() {
        match unsafe { Library::new(&name) } {
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
    audio_output_enum: Option<AudioOutputEnumFn>,
    audio_output_release: Option<AudioOutputReleaseFn>,
    audio_output_set: Option<AudioOutputSetFn>,
    get_media: Option<GetMediaFn>,
    media_get_stats: Option<MediaGetStatsFn>,
    spu_description: Option<GetTrackDescFn>,
    get_spu: Option<GetSpuFn>,
    set_spu: Option<SetSpuFn>,
    add_slave: Option<AddSlaveFn>,
    get_spu_delay: Option<GetSpuDelayFn>,
}

/// The per-load media options the core's cache plan decides
/// (P01): one cache value applied to all four libVLC caches,
/// plus the live/on-demand clock behaviour. Python also always
/// asks for HTTP reconnect and the adaptive access module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaOptions {
    pub cache_ms: u32,
    pub live: bool,
}

impl MediaOptions {
    /// The `:name=value` media-option strings for one load.
    #[must_use]
    pub fn option_strings(&self) -> Vec<String> {
        let mut options = vec![
            format!(":network-caching={}", self.cache_ms),
            format!(":live-caching={}", self.cache_ms),
            format!(":file-caching={}", self.cache_ms),
            format!(":disc-caching={}", self.cache_ms),
            ":http-reconnect=true".to_string(),
            ":adaptive-use-access=1".to_string(),
        ];
        if self.live {
            options.push(":clock-jitter=1500".to_string());
        } else {
            options.extend([
                ":clock-jitter=0".to_string(),
                ":clock-synchro=0".to_string(),
                ":drop-late-frames=0".to_string(),
                ":skip-frames=0".to_string(),
            ]);
        }
        options
    }
}

/// A loaded libVLC instance with one media player. The
/// handles live behind an `Arc` (P18): transport operations
/// (load/stop) run on a serialized worker thread — Python's
/// `VLCControl` model — while state reads happen wherever the
/// caller is. libVLC serializes calls on one player
/// internally; the raw handles never leave this crate.
#[derive(Clone)]
pub struct VlcEngine {
    inner: std::sync::Arc<EngineInner>,
}

struct EngineInner {
    _lib: Library,
    instance: *mut c_void,
    player: *mut c_void,
    fns: Functions,
}

unsafe impl Send for EngineInner {}
unsafe impl Sync for EngineInner {}

// The engine's command methods report success as a bool that
// callers may legitimately ignore (fire-and-forget transport
// control), so `must_use` does not apply to them.
#[allow(clippy::must_use_candidate)]
impl VlcEngine {
    /// Load libVLC and create the instance and player.
    ///
    /// # Errors
    /// Returns the load or symbol error when libVLC is missing or
    /// too old, and a message when instance creation fails.
    // A flat symbol-loading constructor: instance and player
    // creation, then every function pointer in one table.
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Result<Self, String> {
        let lib = load_library()?;
        macro_rules! sym {
            ($name:expr, $ty:ty) => {
                *unsafe { lib.get::<$ty>($name) }
                    .map_err(|e| format!("libVLC symbol {}: {e}", String::from_utf8_lossy($name)))?
            };
        }
        let new_instance: NewFn = sym!(b"libvlc_new", NewFn);
        // Point libVLC at the plugins bundled beside the executable
        // when the package shipped them; a system VLC finds its own.
        // libVLC 3 removed the --plugin-path instance option:
        // passing it only logs "option --plugin-path no longer
        // exists" and it is ignored. VLC_PLUGIN_PATH is the
        // supported mechanism (Brandon's log, 2026-11-03).
        if let Some(dir) = bundled_plugins_dir() {
            std::env::set_var("VLC_PLUGIN_PATH", dir);
        }
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
            audio_output_enum: unsafe {
                lib.get::<AudioOutputEnumFn>(b"libvlc_audio_output_device_enum")
            }
            .ok()
            .map(|s| *s),
            audio_output_release: unsafe {
                lib.get::<AudioOutputReleaseFn>(b"libvlc_audio_output_device_list_release")
            }
            .ok()
            .map(|s| *s),
            audio_output_set: unsafe {
                lib.get::<AudioOutputSetFn>(b"libvlc_audio_output_device_set")
            }
            .ok()
            .map(|s| *s),
            get_media: unsafe { lib.get::<GetMediaFn>(b"libvlc_media_player_get_media") }
                .ok()
                .map(|s| *s),
            media_get_stats: unsafe { lib.get::<MediaGetStatsFn>(b"libvlc_media_get_stats") }
                .ok()
                .map(|s| *s),
            spu_description: unsafe {
                lib.get::<GetTrackDescFn>(b"libvlc_video_get_spu_description")
            }
            .ok()
            .map(|s| *s),
            get_spu: unsafe { lib.get::<GetSpuFn>(b"libvlc_video_get_spu") }
                .ok()
                .map(|s| *s),
            set_spu: unsafe { lib.get::<SetSpuFn>(b"libvlc_video_set_spu") }
                .ok()
                .map(|s| *s),
            add_slave: unsafe { lib.get::<AddSlaveFn>(b"libvlc_media_player_add_slave") }
                .ok()
                .map(|s| *s),
            get_spu_delay: unsafe { lib.get::<GetSpuDelayFn>(b"libvlc_video_get_spu_delay") }
                .ok()
                .map(|s| *s),
        };
        Ok(Self {
            inner: std::sync::Arc::new(EngineInner {
                _lib: lib,
                instance,
                player,
                fns,
            }),
        })
    }

    /// Point the video output at a native window handle (Windows
    /// HWND, macOS `NSView`), passed as an opaque address. On Linux
    /// libVLC opens its own video window; the handle is ignored
    /// there.
    pub fn set_window_handle(&self, handle: usize) {
        if handle == 0 {
            return;
        }
        let pointer = handle as *mut c_void;
        if cfg!(windows) {
            if let Some(set) = self.inner.fns.set_hwnd {
                unsafe { set(self.inner.player, pointer) };
            }
        } else if cfg!(target_os = "macos") {
            if let Some(set) = self.inner.fns.set_nsobject {
                unsafe { set(self.inner.player, pointer) };
            }
        }
    }

    /// Load and play a URL with the stream's headers and the
    /// cache plan's media options (P01).
    ///
    /// # Errors
    /// Returns a message when the URL is invalid, libVLC cannot
    /// create the media, or playback is refused.
    pub fn play_url(
        &self,
        url: &str,
        headers: &StreamHeaders,
        options: MediaOptions,
    ) -> Result<(), String> {
        let c_url = CString::new(url).map_err(|_| "URL contains a NUL byte.".to_string())?;
        let media =
            unsafe { (self.inner.fns.media_new_location)(self.inner.instance, c_url.as_ptr()) };
        if media.is_null() {
            return Err("libVLC could not create media.".to_string());
        }
        for (option, header) in [
            (":http-user-agent=", "User-Agent"),
            (":http-referrer=", "Referer"),
        ] {
            if let Some(value) = headers.get(header) {
                if let Ok(option) = CString::new(format!("{option}{value}")) {
                    unsafe { (self.inner.fns.media_add_option)(media, option.as_ptr()) };
                }
            }
        }
        for option in options.option_strings() {
            if let Ok(option) = CString::new(option) {
                unsafe { (self.inner.fns.media_add_option)(media, option.as_ptr()) };
            }
        }
        unsafe {
            (self.inner.fns.set_media)(self.inner.player, media);
            (self.inner.fns.media_release)(media);
        }
        if unsafe { (self.inner.fns.play)(self.inner.player) } != 0 {
            return Err("libVLC refused to play.".to_string());
        }
        Ok(())
    }

    /// Resume playback.
    pub fn play(&self) {
        unsafe { (self.inner.fns.play)(self.inner.player) };
    }

    /// Pause (libVLC's pause toggles the paused state).
    pub fn pause(&self) {
        unsafe { (self.inner.fns.pause)(self.inner.player) };
    }

    /// Stop playback.
    pub fn stop(&self) {
        unsafe { (self.inner.fns.stop)(self.inner.player) };
    }

    /// Seek to a position in milliseconds.
    pub fn set_time(&self, ms: i64) {
        unsafe { (self.inner.fns.set_time)(self.inner.player, ms) };
    }

    /// Current position in milliseconds (-1 when unknown).
    #[must_use]
    pub fn time(&self) -> i64 {
        unsafe { (self.inner.fns.get_time)(self.inner.player) }
    }

    /// Media length in milliseconds (-1 when unknown).
    #[must_use]
    pub fn length(&self) -> i64 {
        unsafe { (self.inner.fns.get_length)(self.inner.player) }
    }

    /// Current player state.
    #[must_use]
    pub fn state(&self) -> VlcState {
        map_state(unsafe { (self.inner.fns.get_state)(self.inner.player) })
    }

    /// Set the volume (0–100, libVLC's nominal scale).
    pub fn set_volume(&self, percent: i32) {
        unsafe { (self.inner.fns.set_volume)(self.inner.player, percent.clamp(0, 100)) };
    }

    /// The audio tracks as (id, name) pairs.
    #[must_use]
    pub fn audio_tracks(&self) -> Vec<(i32, String)> {
        let mut tracks = Vec::new();
        if unsafe { (self.inner.fns.get_track_count)(self.inner.player) } <= 0 {
            return tracks;
        }
        let head = unsafe { (self.inner.fns.get_track_description)(self.inner.player) };
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
        unsafe { (self.inner.fns.track_description_release)(head) };
        tracks
    }

    /// Every audio output device libVLC reports, as (id,
    /// description) pairs with Python's filtering: the empty id
    /// (libVLC's own system-default entry) is skipped because
    /// the dialog offers its own. The native list is walked via
    /// `next` pointers and released before returning — walking
    /// it any other way reads off the end (Python's crash note).
    #[must_use]
    pub fn audio_output_devices(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let (Some(enumerate), Some(release)) = (
            self.inner.fns.audio_output_enum,
            self.inner.fns.audio_output_release,
        ) else {
            return pairs;
        };
        let head = unsafe { enumerate(self.inner.player) };
        if head.is_null() {
            return pairs;
        }
        let mut node = head;
        while !node.is_null() {
            let (device, description, next) =
                unsafe { ((*node).device, (*node).description, (*node).next) };
            let id = unsafe { c_string(device) };
            if !id.is_empty() {
                let label = unsafe { c_string(description) };
                pairs.push((id.clone(), if label.is_empty() { id } else { label }));
            }
            node = next;
        }
        unsafe { release(head) };
        pairs
    }

    /// Point playback at one audio output device. Returns
    /// libVLC's success flag; an empty id is the caller's
    /// "system default" case and is not sent (Python's apply
    /// returns early on empty, leaving a fresh player's
    /// default in charge).
    pub fn set_audio_output_device(&self, device_id: &str) -> bool {
        if device_id.is_empty() {
            return false;
        }
        let Some(set) = self.inner.fns.audio_output_set else {
            return false;
        };
        let Ok(c_id) = CString::new(device_id) else {
            return false;
        };
        unsafe { set(self.inner.player, c_id.as_ptr()) == 0 }
    }

    /// Select an audio track by id.
    pub fn set_audio_track(&self, id: i32) -> bool {
        (unsafe { (self.inner.fns.set_track)(self.inner.player, id) }) == 0
    }

    /// The current media's decoded/played counters (P03's
    /// freeze evidence), or `None` when libVLC cannot report
    /// them — the caller falls back to the position clock.
    #[must_use]
    pub fn stats(&self) -> Option<AvCounters> {
        let (get_media, get_stats) = (self.inner.fns.get_media?, self.inner.fns.media_get_stats?);
        let media = unsafe { get_media(self.inner.player) };
        if media.is_null() {
            return None;
        }
        let mut stats = MediaStats::default();
        if unsafe { get_stats(media, &raw mut stats) } == 0 {
            return None;
        }
        Some(AvCounters {
            read_bytes: i64::from(stats.read_bytes),
            decoded_video: i64::from(stats.decoded_video),
            displayed_pictures: i64::from(stats.displayed_pictures),
            decoded_audio: i64::from(stats.decoded_audio),
            played_abuffers: i64::from(stats.played_abuffers),
        })
    }

    /// The embedded subtitle (SPU) tracks as (id, name) pairs,
    /// in libVLC's description order.
    #[must_use]
    pub fn subtitle_tracks(&self) -> Vec<(i32, String)> {
        let mut tracks = Vec::new();
        let Some(describe) = self.inner.fns.spu_description else {
            return tracks;
        };
        let head = unsafe { describe(self.inner.player) };
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
        unsafe { (self.inner.fns.track_description_release)(head) };
        tracks
    }

    /// The selected subtitle track id (-1 = Off, -2 when
    /// libVLC cannot say).
    #[must_use]
    pub fn subtitle_track(&self) -> i32 {
        self.inner
            .fns
            .get_spu
            .map_or(-2, |get| unsafe { get(self.inner.player) })
    }

    /// The subtitle delay in milliseconds (libVLC reports
    /// microseconds; Python's sampling clock subtracts
    /// `delay // 1000` from the media position, P10). Zero
    /// when libVLC cannot say.
    #[must_use]
    pub fn subtitle_delay_ms(&self) -> i64 {
        self.inner
            .fns
            .get_spu_delay
            .map_or(0, |get| unsafe { get(self.inner.player) / 1000 })
    }

    /// Select a subtitle track by id (-1 disables rendering).
    pub fn set_subtitle_track(&self, id: i32) -> bool {
        let Some(set) = self.inner.fns.set_spu else {
            return false;
        };
        let code = unsafe { set(self.inner.player, id) };
        code == 0
    }

    /// Attach a sidecar subtitle file for VLC rendering
    /// (Python's `add_slave` with the subtitle slave type and
    /// select = true). `uri` is a `file://` URI.
    pub fn add_subtitle_slave(&self, uri: &str) -> bool {
        let Some(add) = self.inner.fns.add_slave else {
            return false;
        };
        let Ok(c_uri) = CString::new(uri) else {
            return false;
        };
        // libvlc_media_slave_type_t: 0 = subtitle.
        let code = unsafe { add(self.inner.player, 0, c_uri.as_ptr(), true) };
        code == 0
    }
}

impl Drop for EngineInner {
    fn drop(&mut self) {
        unsafe {
            (self.fns.player_release)(self.player);
            (self.fns.release)(self.instance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{candidates_from, map_state, plugins_dir_in, VlcState};

    #[test]
    fn bundled_plugins_dir_found_only_when_shipped() {
        let base =
            std::env::temp_dir().join(format!("iptv-vlc-plugins-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // No plugins folder shipped: nothing to point libVLC at.
        assert_eq!(plugins_dir_in(&base), None);
        std::fs::create_dir(base.join("plugins")).unwrap();
        assert_eq!(
            plugins_dir_in(&base),
            Some(base.join("plugins")),
            "the shipped plugins folder is the one libVLC must use"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bundled_copy_beside_the_exe_wins() {
        let dir = std::path::Path::new("/opt/app");
        let candidates = candidates_from(Some(dir));
        let first = &candidates[0];
        assert!(first.starts_with("/opt/app"), "{first}");
        assert!(
            first.ends_with("libvlc.dll")
                || first.ends_with("libvlc.dylib")
                || first.ends_with("libvlc.so.5"),
            "{first}"
        );
        // And the bare system name stays as a fallback.
        assert!(candidates.iter().any(|c| !c.contains('/')));
    }

    #[test]
    fn media_options_carry_the_cache_plan_to_all_four_caches() {
        let live = super::MediaOptions {
            cache_ms: 500,
            live: true,
        }
        .option_strings();
        for name in ["network", "live", "file", "disc"] {
            assert!(
                live.contains(&format!(":{name}-caching=500")),
                "{name} cache missing: {live:?}"
            );
        }
        assert!(live.contains(&":http-reconnect=true".to_string()));
        assert!(live.contains(&":clock-jitter=1500".to_string()));
        let vod = super::MediaOptions {
            cache_ms: 25_000,
            live: false,
        }
        .option_strings();
        assert!(vod.contains(&":network-caching=25000".to_string()));
        assert!(vod.contains(&":clock-jitter=0".to_string()));
        assert!(vod.contains(&":drop-late-frames=0".to_string()));
        assert!(vod.contains(&":skip-frames=0".to_string()));
    }

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
            Ok(engine) => {
                assert_eq!(engine.state(), VlcState::Nothing);
                engine.stop();
            }
            Err(error) => assert!(!error.is_empty()),
        }
    }
}

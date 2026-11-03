//! The application voice: Windows SAPI subtitle speech, a port
//! of the Python client's `app_tts.py` (issues #131/#132).
//!
//! The application owns this synthesis — its own volume, rate and
//! output device — and it never touches screen-reader settings.
//! Routing decisions (which output, pause vs fallback on device
//! loss, the retained saved selection) live in the wx-free core
//! (`iptv_core::speech::DeviceRouter`); this crate is only the
//! SAPI driver behind them. Off Windows the voice is simply
//! unavailable, exactly as Python's backend is.
//!
//! Threading: like Python's backend, the voice belongs to the
//! thread that constructs it (the UI thread, a COM apartment);
//! every call must come from that thread.

use iptv_core::speech::{AppSpeakResult, DevicePolicy, DeviceRouter, OutputRoute};

#[cfg(not(windows))]
mod stub;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
use stub::Voice;
#[cfg(windows)]
use win::Voice;

/// Whether an application voice could be created on this
/// platform at all (Windows with SAPI). On Windows this probes
/// with a real create-and-release; elsewhere it is false.
#[must_use]
pub fn platform_available() -> bool {
    Voice::probe()
}

/// The application-voice backend.
pub struct AppVoice {
    router: DeviceRouter,
    voice: Voice,
    volume: i32,
    rate: i32,
    speak_failed: bool,
}

impl AppVoice {
    /// Construct the backend and its router from the saved output
    /// selection ("" = system default). SAPI creation is
    /// attempted on Windows; failure just means unavailable.
    #[must_use]
    pub fn new(saved_output: &str) -> Self {
        Self {
            router: DeviceRouter::new(saved_output),
            voice: Voice::new(),
            volume: 100,
            rate: 0,
            speak_failed: false,
        }
    }

    #[must_use]
    pub fn available(&self) -> bool {
        self.voice.available()
    }

    #[must_use]
    pub fn selected_output(&self) -> &str {
        self.router.selected()
    }

    #[must_use]
    pub const fn route(&self) -> OutputRoute {
        self.router.route()
    }

    #[must_use]
    pub const fn volume(&self) -> i32 {
        self.volume
    }

    #[must_use]
    pub const fn rate(&self) -> i32 {
        self.rate
    }

    /// Speak one cue under the device policy. Async with
    /// purge-before-speak, so a fast run of cues never stacks
    /// stale speech (latest cue wins).
    pub fn speak(&mut self, text: &str, policy: DevicePolicy) -> AppSpeakResult {
        if !self.available() {
            return AppSpeakResult::Unavailable;
        }
        if text.trim().is_empty() {
            return AppSpeakResult::Empty;
        }
        if !self.validate_output(policy) {
            return AppSpeakResult::OutputMissing;
        }
        match self.voice.speak_text(text) {
            Ok(()) => {
                self.speak_failed = false;
                AppSpeakResult::Spoken
            }
            Err(()) => {
                self.voice.purge();
                self.speak_failed = true;
                AppSpeakResult::Failed
            }
        }
    }

    /// Stop application-owned speech immediately (purge).
    pub fn stop(&mut self) {
        self.voice.purge();
    }

    /// Set the voice's own volume (0..=100); the stored value
    /// changes only when SAPI accepts it.
    pub fn set_volume(&mut self, level: i32) -> bool {
        let level = level.clamp(0, 100);
        if self.voice.set_volume(level) {
            self.volume = level;
            true
        } else {
            false
        }
    }

    /// Set the voice's own rate (-10..=10, 0 normal); invalid
    /// input falls back to normal, as in Python.
    pub fn set_rate(&mut self, rate: i32) -> bool {
        let rate = rate.clamp(-10, 10);
        if self.voice.set_rate(rate) {
            self.rate = rate;
            true
        } else {
            false
        }
    }

    /// Present output devices as (token ID, display name) pairs.
    pub fn list_outputs(&mut self) -> Vec<(String, String)> {
        self.voice.outputs()
    }

    /// Validate the route against the present devices and apply
    /// it (the device watchdog and the speak path both run this):
    /// true when speech may proceed. Entering the paused route
    /// purges anything queued, so a lost device also silences an
    /// utterance in flight.
    pub fn validate_output(&mut self, policy: DevicePolicy) -> bool {
        if !self.available() {
            return false;
        }
        let devices = self.voice.outputs();
        let ok = self.router.validate(policy, &devices);
        if ok {
            let target = match self.router.route() {
                OutputRoute::Ready => Some(self.router.selected().to_string()),
                _ => None,
            };
            if self.voice.apply_output(target.as_deref()).is_err() && target.is_some() {
                return false;
            }
        } else if self.router.route() == OutputRoute::Paused {
            self.voice.purge();
        }
        ok
    }

    /// Select an output by ID ("" = explicit system default). A
    /// missing selection stays requested and pauses speech under
    /// the pause policy.
    pub fn set_output(&mut self, identity: &str, policy: DevicePolicy) -> bool {
        self.voice.purge();
        let devices = self.voice.outputs();
        let ok = self.router.set_output(identity, policy, &devices);
        if ok {
            let target = match self.router.route() {
                OutputRoute::Ready => Some(self.router.selected().to_string()),
                _ => None,
            };
            let _ = self.voice.apply_output(target.as_deref());
        }
        ok
    }

    /// Release the voice (app shutdown).
    pub fn close(&mut self) {
        self.voice.purge();
        self.voice.release();
    }
}

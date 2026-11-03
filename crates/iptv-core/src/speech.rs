//! Subtitle speech routing (issues #131/#132; Python's
//! `subtitle_speech` routing in `main.py` + `app_tts.py`).
//!
//! Exactly one backend speaks each automatic cue: the screen
//! reader (which owns its voice, volume and endpoint) or the
//! application voice (Windows SAPI, which owns its own). Speak
//! Subtitles is a setting of its own, independent of the
//! announcement level. Application-voice failures are reported
//! through the screen reader exactly once per failure stretch,
//! even when automatic announcements are off — the caller uses an
//! ungated announcement path for them.

use crate::config::Config;

/// Which backend speaks subtitle cues.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpeechBackend {
    #[default]
    ScreenReader,
    AppVoice,
}

/// The configured backend (Python `_subtitle_backend`): the
/// `subtitle_speech_backend` key, anything unrecognized reads as
/// the screen reader.
#[must_use]
pub fn speech_backend(config: &Config) -> SpeechBackend {
    match config
        .string_or("subtitle_speech_backend", "screen_reader")
        .as_str()
    {
        "app_tts" => SpeechBackend::AppVoice,
        _ => SpeechBackend::ScreenReader,
    }
}

/// What one application-voice speak attempt did (Python's
/// `SpeechResult`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppSpeakResult {
    Spoken,
    Failed,
    OutputMissing,
    Unavailable,
    Empty,
}

/// Where one automatic cue goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueRoute {
    ScreenReader(String),
    AppVoice(String),
    Suppressed,
}

/// The once-only failure texts (Python's, verbatim).
pub const FAILURE_TEXT: &str = "Subtitle speech failed.";
pub const PAUSED_TEXT: &str = "Subtitle speech paused: the selected audio output is unavailable.";
pub const DEVICE_LOST_TEXT: &str =
    "The selected audio output is unavailable. Subtitle speech is paused.";

/// Cue routing state: backend choice, application-voice
/// availability, preview suppression, and the once-only failure
/// latch shared by the speak path and the device watchdog.
#[derive(Debug, Default)]
pub struct CueSpeech {
    backend: SpeechBackend,
    app_available: bool,
    failure_announced: bool,
    preview_active: bool,
}

impl CueSpeech {
    #[must_use]
    pub fn new(backend: SpeechBackend, app_available: bool) -> Self {
        Self {
            backend,
            app_available,
            failure_announced: false,
            preview_active: false,
        }
    }

    #[must_use]
    pub const fn backend(&self) -> SpeechBackend {
        self.backend
    }

    /// Switch backends. Switching to the application voice is
    /// refused while it is unavailable (Python shows its "only
    /// available on Windows with SAPI" notice and changes
    /// nothing); the caller stops the application voice on every
    /// accepted switch.
    pub fn set_backend(&mut self, backend: SpeechBackend) -> bool {
        if backend == SpeechBackend::AppVoice && !self.app_available {
            return false;
        }
        self.backend = backend;
        self.failure_announced = false;
        true
    }

    pub fn set_preview_active(&mut self, active: bool) {
        self.preview_active = active;
    }

    /// Route one automatic cue: exactly one backend, and nothing
    /// at all while an output preview is playing (a preview must
    /// not be interrupted by cues).
    #[must_use]
    pub fn route(&self, text: &str) -> CueRoute {
        if self.preview_active {
            return CueRoute::Suppressed;
        }
        match self.backend {
            SpeechBackend::ScreenReader => CueRoute::ScreenReader(text.to_string()),
            SpeechBackend::AppVoice => CueRoute::AppVoice(text.to_string()),
        }
    }

    /// Feed back an application-voice speak attempt. A success
    /// re-arms the failure latch; a failure yields its report
    /// text once per stretch.
    pub fn note_app_result(&mut self, result: AppSpeakResult) -> Option<&'static str> {
        match result {
            AppSpeakResult::Spoken => {
                self.failure_announced = false;
                None
            }
            AppSpeakResult::Empty => None,
            AppSpeakResult::Failed => self.latch(FAILURE_TEXT),
            AppSpeakResult::OutputMissing | AppSpeakResult::Unavailable => self.latch(PAUSED_TEXT),
        }
    }

    /// The device watchdog's report (a selected output vanished):
    /// same latch, Python's watchdog wording.
    pub fn note_device_lost(&mut self) -> Option<&'static str> {
        self.latch(DEVICE_LOST_TEXT)
    }

    /// A healthy route again (device back, or speech flowing):
    /// re-arm the latch without reporting.
    pub fn note_route_healthy(&mut self) {
        self.failure_announced = false;
    }

    fn latch(&mut self, text: &'static str) -> Option<&'static str> {
        if self.failure_announced {
            None
        } else {
            self.failure_announced = true;
            Some(text)
        }
    }
}

/// What the application voice does when its selected output
/// disappears (Python `subtitle_app_device_policy`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DevicePolicy {
    /// Pause speech until the device returns (the default: never
    /// silently reroute private captions to another device).
    #[default]
    Pause,
    /// Fall back to the system default output.
    Default,
}

/// The configured device-loss policy; anything unrecognized is
/// the pause default.
#[must_use]
pub fn device_policy(config: &Config) -> DevicePolicy {
    match config
        .string_or("subtitle_app_device_policy", "pause")
        .as_str()
    {
        "default" => DevicePolicy::Default,
        _ => DevicePolicy::Pause,
    }
}

/// Where the application voice is currently routed (Python's
/// `RoutingState`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputRoute {
    #[default]
    Default,
    Ready,
    Paused,
    Fallback,
}

/// Output-device routing for the application voice (Python's
/// `AppTtsBackend` selection rules): the saved selection is an
/// output token ID, retained across outages and fallbacks; a
/// legacy saved friendly name migrates only on an exact, unique
/// match.
#[derive(Debug, Default)]
pub struct DeviceRouter {
    selected: String,
    route: OutputRoute,
}

impl DeviceRouter {
    #[must_use]
    pub fn new(saved_selection: &str) -> Self {
        Self {
            selected: saved_selection.to_string(),
            route: if saved_selection.is_empty() {
                OutputRoute::Default
            } else {
                OutputRoute::Paused
            },
        }
    }

    #[must_use]
    pub fn selected(&self) -> &str {
        &self.selected
    }

    #[must_use]
    pub const fn route(&self) -> OutputRoute {
        self.route
    }

    /// Resolve the saved selection against the present devices:
    /// an exact ID match, else a unique exact name match (the
    /// legacy migration). Anything else resolves to nothing.
    fn resolve(&self, devices: &[(String, String)]) -> Option<String> {
        if self.selected.is_empty() {
            return None;
        }
        if let Some((id, _)) = devices
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(&self.selected))
        {
            return Some(id.clone());
        }
        let by_name: Vec<&(String, String)> = devices
            .iter()
            .filter(|(_, name)| name.eq_ignore_ascii_case(&self.selected))
            .collect();
        if by_name.len() == 1 {
            return Some(by_name[0].0.clone());
        }
        None
    }

    /// Whether the selected output is among the present devices.
    #[must_use]
    pub fn output_present(&self, devices: &[(String, String)]) -> bool {
        self.selected.is_empty() || self.resolve(devices).is_some()
    }

    /// Validate the route for speaking (Python `validate_output`):
    /// true when speech may proceed. A name-migrated selection is
    /// rewritten to its token ID. Falling back never overwrites
    /// the saved selection.
    pub fn validate(&mut self, policy: DevicePolicy, devices: &[(String, String)]) -> bool {
        if self.selected.is_empty() {
            self.route = OutputRoute::Default;
            return true;
        }
        if let Some(id) = self.resolve(devices) {
            self.selected = id;
            self.route = OutputRoute::Ready;
            return true;
        }
        match policy {
            DevicePolicy::Default => {
                self.route = OutputRoute::Fallback;
                true
            }
            DevicePolicy::Pause => {
                self.route = OutputRoute::Paused;
                false
            }
        }
    }

    /// Choose an output by ID (Python `set_output`): a missing
    /// selection stays requested and pauses speech under the
    /// pause policy. An empty identity is the explicit system
    /// default (Python `reset_output`).
    pub fn set_output(
        &mut self,
        identity: &str,
        policy: DevicePolicy,
        devices: &[(String, String)],
    ) -> bool {
        self.selected = identity.to_string();
        self.route = OutputRoute::Paused;
        self.validate(policy, devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(key: &str, value: &str) -> Config {
        let mut config = Config::default();
        config.set_value(key, serde_json::json!(value));
        config
    }

    #[test]
    fn backend_parsing_defaults_to_screen_reader() {
        assert_eq!(
            speech_backend(&Config::default()),
            SpeechBackend::ScreenReader
        );
        assert_eq!(
            speech_backend(&config_with("subtitle_speech_backend", "app_tts")),
            SpeechBackend::AppVoice
        );
        assert_eq!(
            speech_backend(&config_with("subtitle_speech_backend", "loud")),
            SpeechBackend::ScreenReader
        );
    }

    #[test]
    fn exactly_one_backend_per_cue_and_preview_suppresses() {
        let mut speech = CueSpeech::new(SpeechBackend::ScreenReader, true);
        assert_eq!(
            speech.route("hello"),
            CueRoute::ScreenReader("hello".to_string())
        );
        assert!(speech.set_backend(SpeechBackend::AppVoice));
        assert_eq!(
            speech.route("hello"),
            CueRoute::AppVoice("hello".to_string())
        );
        speech.set_preview_active(true);
        assert_eq!(speech.route("hello"), CueRoute::Suppressed);
    }

    #[test]
    fn app_voice_backend_is_refused_when_unavailable() {
        let mut speech = CueSpeech::new(SpeechBackend::ScreenReader, false);
        assert!(!speech.set_backend(SpeechBackend::AppVoice));
        assert_eq!(speech.backend(), SpeechBackend::ScreenReader);
    }

    #[test]
    fn failures_report_once_per_stretch_and_success_rearms() {
        let mut speech = CueSpeech::new(SpeechBackend::AppVoice, true);
        assert_eq!(
            speech.note_app_result(AppSpeakResult::Failed),
            Some(FAILURE_TEXT)
        );
        assert_eq!(speech.note_app_result(AppSpeakResult::Failed), None);
        assert_eq!(speech.note_app_result(AppSpeakResult::Spoken), None);
        assert_eq!(
            speech.note_app_result(AppSpeakResult::OutputMissing),
            Some(PAUSED_TEXT)
        );
        assert_eq!(speech.note_device_lost(), None, "same latch");
        speech.note_route_healthy();
        assert_eq!(speech.note_device_lost(), Some(DEVICE_LOST_TEXT));
    }

    fn devices() -> Vec<(String, String)> {
        vec![
            ("id-a".to_string(), "USB Headset".to_string()),
            ("id-b".to_string(), "Speakers".to_string()),
        ]
    }

    #[test]
    fn device_router_pause_is_the_default_and_keeps_the_selection() {
        let mut router = DeviceRouter::new("id-a");
        assert!(router.validate(DevicePolicy::Pause, &devices()));
        assert_eq!(router.route(), OutputRoute::Ready);
        // Device disappears: pause, selection retained.
        assert!(!router.validate(DevicePolicy::Pause, &devices()[1..]));
        assert_eq!(router.route(), OutputRoute::Paused);
        assert_eq!(router.selected(), "id-a");
        // Under the default policy the same loss falls back, and
        // the saved selection still survives.
        assert!(router.validate(DevicePolicy::Default, &devices()[1..]));
        assert_eq!(router.route(), OutputRoute::Fallback);
        assert_eq!(router.selected(), "id-a");
        // And it recovers when the device returns.
        assert!(router.validate(DevicePolicy::Pause, &devices()));
        assert_eq!(router.route(), OutputRoute::Ready);
    }

    #[test]
    fn device_router_migrates_a_unique_legacy_name_only() {
        let mut router = DeviceRouter::new("usb headset");
        assert!(router.validate(DevicePolicy::Pause, &devices()));
        assert_eq!(router.selected(), "id-a", "name migrated to its token ID");
        let dup = vec![
            ("id-a".to_string(), "USB Headset".to_string()),
            ("id-c".to_string(), "USB Headset".to_string()),
        ];
        let mut router = DeviceRouter::new("USB Headset");
        assert!(!router.validate(DevicePolicy::Pause, &dup));
        assert_eq!(
            router.selected(),
            "USB Headset",
            "ambiguous name never migrates"
        );
    }

    #[test]
    fn device_router_explicit_default_and_presence() {
        let mut router = DeviceRouter::new("id-a");
        assert!(router.set_output("", DevicePolicy::Pause, &devices()));
        assert_eq!(router.route(), OutputRoute::Default);
        assert!(router.output_present(&[]));
        let router = DeviceRouter::new("id-a");
        assert!(router.output_present(&devices()));
        assert!(!router.output_present(&devices()[1..]));
    }

    #[test]
    fn policy_parsing_defaults_to_pause() {
        assert_eq!(device_policy(&Config::default()), DevicePolicy::Pause);
        assert_eq!(
            device_policy(&config_with("subtitle_app_device_policy", "default")),
            DevicePolicy::Default
        );
        assert_eq!(
            device_policy(&config_with("subtitle_app_device_policy", "loud")),
            DevicePolicy::Pause
        );
    }
}

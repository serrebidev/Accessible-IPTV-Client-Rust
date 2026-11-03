mod buffer;

use iptv_core::guide::Guide;
use iptv_core::i18n::I18n;
use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    cast::{CastDevice, CastEvent, CastWorker},
    categories::Category,
    config::{self, Config},
    dvr::{channel_from_snapshot, DvrAction, DvrSchedule},
    epg::{
        local_datetime, local_hhmm, match_guide_channel, EpgGuide, EpgWorker, GuideIndexes,
        Programme,
    },
    favorites::{channel_key, FavoriteSet, Toggle},
    playback::{
        candidate_executables, detect_from_path, external_argv, player_choice, resume_checkpoint,
        resume_position, EngineAction, EngineEvent, LaunchGate, PlaybackRequest, PlaybackSession,
        PlayerChoice, SessionState,
    },
    providers::{ProviderAccount, ProviderEvent, ProviderWorker},
    recording::{self, RecordingEngine},
    reminders::{normalize_reminders, Reminder, ReminderStatus},
    search::SearchResults,
    sources::PlaylistSource,
    subtitles::{active as active_cue, load_cues, Cue},
    vod::{VodCatalog, VodEvent, VodItem, VodKind, VodWorker},
    Channel,
};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, process::Command, rc::Rc};
use wxdragon::event::EventType;
use wxdragon::prelude::*;
use wxdragon::timer::Timer;
use wxdragon::widgets::media_ctrl::SeekMode;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    SystemOptions::set_option_by_int("msw.no-manifest-check", 1);
    wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("Accessible IPTV Client")
            .with_size(Size::new(800, 600))
            .build();
        frame.set_accessibility_label("Accessible IPTV Client");

        let panel = Panel::builder(&frame).build();
        let (scope_label, scope_choice) = scope_controls(panel);
        let (category_label, category) = category_controls(panel);
        let search_label = StaticText::builder(&panel)
            .with_label("&Search channels")
            .build();
        let search = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        search.set_accessibility_label("Search channels");
        search.set_accessibility_description("Type to filter channel names within the active category. Tab or Enter moves to Channels; Shift+Tab returns to Categories.");
        // Keep the label immediately before the list for native MSW accessible naming.
        let label = StaticText::builder(&panel).with_label("Channels").build();
        let list = ListCtrl::builder(&panel)
            .with_style(
                ListCtrlStyle::Report
                    | ListCtrlStyle::NoHeader
                    | ListCtrlStyle::SingleSel
                    | ListCtrlStyle::Virtual,
            )
            .build();
        list.set_accessibility_label("Channels");
        list.set_accessibility_description("No playlist loaded. Channel list is empty.");
        list.insert_column(0, "Channels", ListColumnFormat::Left, 640);
        // ListCtrl does not implement WindowEvents in wxDragon 0.9.24.
        list.bind_internal(EventType::SIZE, move |event| {
            let width = list.get_client_size().width;
            if width > 0 {
                list.set_column_width(0, width);
            }
            event.skip(true);
        });

        let model = Rc::new(RefCell::new(SearchResults::default()));
        let rows = Rc::clone(&model);
        let suffixes: Rc<EpgSuffixes> =
            Rc::new(RefCell::new((0, 0, std::collections::HashMap::new())));
        let suffixes_for_text = Rc::clone(&suffixes);
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(&rows.borrow(), &suffixes_for_text.borrow(), index, column)
        }));
        list.set_item_count(0);

        let status_label = StaticText::builder(&panel)
            .with_label("Playlist status")
            .build();
        let details = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
            .with_value("No playlist loaded. Channel list is empty.")
            .build();
        details.set_accessibility_label("Playlist status");
        // Python's main window has no visible status field: the
        // status text rides the frame status bar and the live
        // region. Both carriers exist but stay hidden, exactly
        // like Python's hidden announcer control.
        status_label.hide();
        details.hide();
        let status = Status {
            label: status_label,
            details,
            bar: Some(frame),
        };
        let (episode_description, stream_url) = info_panels(panel);
        let controls_all = Controls {
            scope_choice,
            category,
            search,
            list,
            episode_description,
            stream_url,
            status,
        };
        let controls_for_layout = controls_all;
        bind_browser(frame, controls_all, model, suffixes);
        layout_main(
            frame,
            panel,
            controls_for_layout,
            scope_label,
            category_label,
            search_label,
            label,
        );
    })
}

/// The main window layout (Python): two columns — Playlist view
/// and Categories on the left (proportion 1), Search, Channels,
/// Episode description and Stream URL on the right (proportion
/// 2). No button row; the status carriers stay hidden (see main).
fn layout_main(
    frame: Frame,
    panel: Panel,
    controls: Controls,
    scope_label: StaticText,
    category_label: StaticText,
    search_label: StaticText,
    channels_label: StaticText,
) {
    let left = BoxSizer::builder(Orientation::Vertical).build();
    left.add(&scope_label, 0, SizerFlag::All, 8);
    left.add(
        &controls.scope_choice,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
    left.add(&category_label, 0, SizerFlag::All, 8);
    left.add(&controls.category, 1, SizerFlag::Expand | SizerFlag::All, 8);
    let right = BoxSizer::builder(Orientation::Vertical).build();
    right.add(&search_label, 0, SizerFlag::All, 8);
    right.add(&controls.search, 0, SizerFlag::Expand | SizerFlag::All, 8);
    right.add(&channels_label, 0, SizerFlag::All, 8);
    right.add(&controls.list, 1, SizerFlag::Expand | SizerFlag::All, 8);
    right.add(
        &controls.episode_description,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
    right.add(
        &controls.stream_url,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
    let layout = BoxSizer::builder(Orientation::Horizontal).build();
    layout.add_sizer(&left, 1, SizerFlag::Expand, 0);
    layout.add_sizer(&right, 2, SizerFlag::Expand, 0);
    panel.set_sizer(layout, true);
    let frame_layout = BoxSizer::builder(Orientation::Vertical).build();
    frame_layout.add(&panel, 1, SizerFlag::Expand, 0);
    frame.set_sizer(frame_layout, true);
    frame.create_status_bar(1, 0, 0, "Status");
    frame.set_status_text("Ready.", 0);
    frame.show(true);
    frame.centre();
    controls.list.set_focus();
}

/// The Episode description and Stream URL panels that follow the
/// channel list in tab order, as in the Python main window.
fn info_panels(panel: Panel) -> (TextCtrl, TextCtrl) {
    let episode_description = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .build();
    episode_description.set_accessibility_label("Episode description");
    let stream_url = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .build();
    stream_url.set_accessibility_label("Stream URL");
    (episode_description, stream_url)
}

/// The Playlist view scope picker, one Tab stop before Categories,
/// as in the Python main window.
fn scope_controls(panel: Panel) -> (StaticText, Choice) {
    let label = StaticText::builder(&panel)
        .with_label("Playlist &view")
        .build();
    let choice = Choice::builder(&panel).build();
    choice.set_accessibility_label("Playlist view");
    choice.set_accessibility_description(
        "Choose one playlist or All playlists. Categories and Channels show only the chosen playlist.",
    );
    choice.append("All playlists");
    choice.set_selection(0);
    (label, choice)
}

/// A short display name for a playlist source location: the file
/// name for files, the host for URLs.
fn scope_display_name(location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        let rest = location.split("://").nth(1).unwrap_or(location);
        return rest.split('/').next().unwrap_or(rest).to_string();
    }
    std::path::Path::new(location).file_name().map_or_else(
        || location.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The Categories tree, in browse order.
fn category_controls(panel: Panel) -> (StaticText, TreeCtrl) {
    let label = StaticText::builder(&panel).with_label("&Category").build();
    let category = TreeCtrl::builder(&panel)
        .with_style(TreeCtrlStyle::HideRoot)
        .build();
    category.set_accessibility_label("Categories");
    category.set_accessibility_description("Choose All Channels, Favorites, or a playlist group. Tab moves to Search; Shift+Tab returns to Playlist view.");
    if let Some(root) = category.add_root("Categories", None, None) {
        category.append_item(&root, "All Channels", None, None);
        category.append_item(&root, "Favorites", None, None);
        // No expand(&root): the root is hidden (TreeCtrlStyle::HideRoot)
        // and wxWidgets asserts "Can't expand/collapse hidden root
        // node!" when asked to expand it. Its children are always
        // visible, so there is nothing to expand.
    }
    (label, category)
}

/// The tree item at a top-level position (0-based), walking the
/// root's children.
fn tree_item_at(tree: TreeCtrl, index: usize) -> Option<TreeItemId> {
    let root = tree.get_root_item()?;
    let (first, mut cookie) = tree.get_first_child(&root)?;
    if index == 0 {
        return Some(first);
    }
    let mut current = first;
    for _ in 1..=index {
        current = tree.get_next_child(&root, &mut cookie)?;
    }
    Some(current)
}

/// The top-level position of a tree item, by item identity.
fn tree_item_index(tree: TreeCtrl, selected: &TreeItemId) -> Option<usize> {
    let root = tree.get_root_item()?;
    let target = u64::from(selected);
    let (first, mut cookie) = tree.get_first_child(&root)?;
    if u64::from(&first) == target {
        return Some(0);
    }
    let mut index = 0;
    loop {
        index += 1;
        let next = tree.get_next_child(&root, &mut cookie)?;
        if u64::from(&next) == target {
            return Some(index);
        }
    }
}

#[derive(Clone, Copy)]
struct Status {
    label: StaticText,
    details: TextCtrl,
    bar: Option<Frame>,
}

/// The UI translator, installed once the configuration (and its
/// language) is loaded. `tr` is the gettext choke point for menu
/// labels and status announcements; untranslated strings pass
/// through unchanged.
static TRANSLATOR: std::sync::OnceLock<I18n> = std::sync::OnceLock::new();

fn set_translator(i18n: &I18n) {
    let _ = TRANSLATOR.set(i18n.clone());
}

fn tr(text: &str) -> String {
    TRANSLATOR
        .get()
        .map_or_else(|| text.to_string(), |i18n| i18n.gettext(text))
}

impl Status {
    fn set_label(self, message: &str) {
        let message = &tr(message);
        self.details.change_value(message);
        if let Some(frame) = self.bar {
            frame.set_status_text(message, 0);
        }
        live_region::announce_with_priority(self.label, message, live_region::Priority::Medium);
    }

    fn set_silent(self, message: &str) {
        self.details.change_value(message);
        if let Some(frame) = self.bar {
            frame.set_status_text(message, 0);
        }
    }
}

/// Player status announcements, gated by the announcement
/// level (issue #102, Python's `live_announce` law): the visual
/// surfaces (status bar + details) always update; speech fires
/// only when the configured level (0-3, default 2) reaches the
/// message priority — 1 errors, 2 state changes and
/// confirmations, 3 detailed/default. Main-window announcements
/// are never gated (they keep using `Status::set_label`).
fn player_status(status: Status, persist: &Persist, message: &str, priority: i64) {
    let message = &tr(message);
    status.set_silent(message);
    let level = persist
        .config
        .borrow()
        .value("announcement_level")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2)
        .clamp(0, 3);
    if !message.is_empty() && level >= priority {
        live_region::announce_with_priority(status.label, message, live_region::Priority::Medium);
    }
}

#[derive(Clone, Copy)]
struct Controls {
    scope_choice: Choice,
    category: TreeCtrl,
    search: TextCtrl,
    list: ListCtrl,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
    status: Status,
}

/// Now/next suffixes for channel rows, keyed by playlist index:
/// (built-at epoch, playlist identity, suffix per index). Rebuilt
/// when the guide lands, the playlist changes, or the data ages
/// past five minutes — the Python design, where the row itself
/// reads "{name} — {now} — Next: {next}".
type EpgSuffixes = RefCell<(i64, usize, std::collections::HashMap<usize, String>)>;

#[derive(Clone)]
struct Wiring {
    browser: Rc<RefCell<BrowserWorker>>,
    categories: Rc<RefCell<Vec<Category>>>,
    timer: Rc<Timer<Frame>>,
    model: Rc<RefCell<SearchResults>>,
    persist: Persist,
    player: Rc<RefCell<PlayerState>>,
    epg: Rc<RefCell<Option<(EpgGuide, GuideIndexes)>>>,
    epg_suffixes: Rc<EpgSuffixes>,
    epg_store: Rc<RefCell<Option<iptv_core::epgstore::EpgStore>>>,
    epg_worker: Rc<EpgWorker>,
    epg_pending: Rc<std::cell::Cell<bool>>,
    epg_deferred: Rc<std::cell::Cell<bool>>,
    reminders: Rc<RefCell<Vec<Reminder>>>,
    provider_worker: Rc<ProviderWorker>,
    provider_pending: Rc<std::cell::Cell<bool>>,
    resolve_seq: Rc<std::cell::Cell<u64>>,
    pending_resolves: Rc<RefCell<std::collections::HashMap<u64, PendingResolve>>>,
    dvr_resolve: Rc<RefCell<std::collections::HashMap<String, DvrResolve>>>,
    recordings: Rc<RefCell<RecordingEngine>>,
    cast_worker: Rc<CastWorker>,
    cast_pending: Rc<std::cell::Cell<bool>>,
    cast_ui: Rc<RefCell<Option<CastUi>>>,
    proxy: Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    guide_ui: Rc<RefCell<Option<GuideUi>>>,
    tray: Rc<TaskBarIcon>,
    i18n: Rc<I18n>,
    vod_worker: Rc<VodWorker>,
    vod_pending: Rc<std::cell::Cell<bool>>,
    vod_ui: Rc<RefCell<Option<VodUi>>>,
    dvr: Rc<RefCell<DvrSchedule>>,
    recorded: Rc<std::cell::Cell<bool>>,
    shutdown_attempted: Rc<std::cell::Cell<bool>>,
    last_dvr_tick: Rc<std::cell::Cell<f64>>,
    previous_channel: Rc<RefCell<Option<Channel>>>,
    scope_ids: Rc<RefCell<Vec<Option<String>>>>,
    download_worker: Rc<iptv_core::downloads::DownloadWorker>,
    downloads: Rc<RefCell<Vec<DownloadRow>>>,
    downloads_active: Rc<std::cell::Cell<bool>>,
    account_text: Rc<RefCell<Option<TextCtrl>>>,
    buffers: Rc<RefCell<buffer::BufferRegistry>>,
}

/// Built-in player window state, created lazily on first use.
/// Which engine owns the current session's loads.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ActiveEngine {
    #[default]
    Fallback,
    Vlc,
}

struct PlayerState {
    session: PlaybackSession,
    gate: LaunchGate,
    window: Option<PlayerWindow>,
    vlc: Option<iptv_vlc::VlcEngine>,
    vlc_active: bool,
    vlc_tracks_ready: bool,
    vlc_track_ids: Vec<i32>,
    vlc_error: Option<String>,
    shared: Option<PlayerShared>,
    desired_track: Option<String>,
    track_menu_ids: Vec<i32>,
    sleep_deadline: Option<i64>,
    sleep_fade_secs: i64,
    sleep_fading: bool,
    sleep_start_volume: i32,
    last_label_cue: Option<usize>,
    current_channel: Option<Channel>,
    pending_resume_ms: Option<i64>,
    /// A scheduled reconnect: (session generation, due ms,
    /// request). The generation guard keeps a superseded or
    /// stopped retry from ever firing.
    pending_reconnect: Option<(u64, i64, PlaybackRequest)>,
    /// Which engine the current request was loaded into.
    engine: ActiveEngine,
    subtitle_cues: Vec<Cue>,
    last_spoken_cue: Option<usize>,
    history: Vec<Channel>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            session: PlaybackSession::new(),
            gate: LaunchGate::default(),
            window: None,
            vlc: None,
            vlc_active: false,
            vlc_tracks_ready: false,
            vlc_error: None,
            vlc_track_ids: Vec::new(),
            shared: None,
            desired_track: None,
            track_menu_ids: Vec::new(),
            sleep_deadline: None,
            sleep_fade_secs: 5,
            sleep_fading: false,
            sleep_start_volume: 100,
            last_label_cue: None,
            current_channel: None,
            pending_resume_ms: None,
            pending_reconnect: None,
            engine: ActiveEngine::Fallback,
            subtitle_cues: Vec::new(),
            last_spoken_cue: None,
            history: Vec::new(),
        }
    }
}

/// The loaded configuration and the path its saves go to.
#[derive(Clone)]
struct Persist {
    config: Rc<RefCell<Config>>,
    path: PathBuf,
}

impl Persist {
    /// Store the browser's current favorite keys. Best effort, like the
    /// reference's logged save failures: a failed write never breaks use.
    /// Set one config value and persist it (menu preferences).
    fn set_and_save(&self, key: &str, value: serde_json::Value) {
        let mut config = self.config.borrow_mut();
        config.set_value(key, value);
        let _ = config.save(&self.path);
    }

    fn save_favorites(&self, keys: &[String]) {
        let mut config = self.config.borrow_mut();
        config.set_favorites(keys.to_vec());
        let _ = config.save(&self.path);
    }

    fn resume_entries(&self) -> BTreeMap<String, iptv_core::playback::ResumeEntry> {
        let mut entries = BTreeMap::new();
        if let Some(serde_json::Value::Object(map)) = self.config.borrow().value("resume_positions")
        {
            for (key, value) in map {
                let fields = (
                    value.get("position_ms").and_then(serde_json::Value::as_i64),
                    value.get("length_ms").and_then(serde_json::Value::as_i64),
                    value.get("updated").and_then(serde_json::Value::as_i64),
                );
                if let (Some(position_ms), Some(length_ms), Some(updated)) = fields {
                    let title = value
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    entries.insert(
                        key.clone(),
                        iptv_core::playback::ResumeEntry {
                            position_ms,
                            length_ms,
                            updated,
                            title,
                        },
                    );
                }
            }
        }
        entries
    }

    fn reminders(&self) -> Vec<Reminder> {
        let rows: Vec<Reminder> = self
            .config
            .borrow()
            .value("programme_reminders")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        normalize_reminders(rows)
    }

    fn save_reminders(&self, rows: &[Reminder]) {
        let mut config = self.config.borrow_mut();
        config.set_value(
            "programme_reminders",
            serde_json::to_value(rows).unwrap_or(serde_json::Value::Null),
        );
        let _ = config.save(&self.path);
    }

    fn save_resume(&self, entries: &BTreeMap<String, iptv_core::playback::ResumeEntry>) {
        let map: serde_json::Map<String, serde_json::Value> = entries
            .iter()
            .map(|(key, entry)| {
                (
                    key.clone(),
                    serde_json::json!({
                        "position_ms": entry.position_ms,
                        "length_ms": entry.length_ms,
                        "updated": entry.updated,
                        "title": entry.title,
                    }),
                )
            })
            .collect();
        let mut config = self.config.borrow_mut();
        config.set_value("resume_positions", serde_json::Value::Object(map));
        let _ = config.save(&self.path);
    }
}

fn bind_browser(
    frame: Frame,
    controls: Controls,
    model: Rc<RefCell<SearchResults>>,
    suffixes: Rc<EpgSuffixes>,
) {
    let candidates = config::config_candidates();
    let loaded = config::load(&candidates, &candidates[0]);
    if !loaded.config.bool_or("show_channel_url", true) {
        controls.stream_url.hide();
    }
    let dvr_path = iptv_core::dvr::schedule_path(&loaded.path);
    let i18n = {
        let mut i18n = I18n::default();
        i18n.init_from_config(&iptv_core::i18n::locale_dir(), loaded.config.raw());
        Rc::new(i18n)
    };
    let mut browser_worker = BrowserWorker::default();
    browser_worker.set_favorites(FavoriteSet::from_stored(loaded.config.favorites()));
    let wiring = Wiring {
        browser: Rc::new(RefCell::new(browser_worker)),
        categories: Rc::new(RefCell::new(vec![Category::All, Category::Favorites])),
        timer: Rc::new(Timer::new(&frame)),
        model,
        persist: Persist {
            config: Rc::new(RefCell::new(loaded.config)),
            path: loaded.path,
        },
        player: Rc::new(RefCell::new(PlayerState::default())),
        epg: Rc::new(RefCell::new(None)),
        epg_suffixes: suffixes,
        epg_store: Rc::new(RefCell::new(None)),
        epg_worker: Rc::new(EpgWorker::new()),
        epg_pending: Rc::new(std::cell::Cell::new(false)),
        epg_deferred: Rc::new(std::cell::Cell::new(false)),
        reminders: Rc::new(RefCell::new(Vec::new())),
        provider_worker: Rc::new(ProviderWorker::new()),
        provider_pending: Rc::new(std::cell::Cell::new(false)),
        resolve_seq: Rc::new(std::cell::Cell::new(0)),
        pending_resolves: Rc::new(RefCell::new(std::collections::HashMap::new())),
        dvr_resolve: Rc::new(RefCell::new(std::collections::HashMap::new())),
        recordings: Rc::new(RefCell::new(RecordingEngine::new())),
        cast_worker: Rc::new(CastWorker::new()),
        cast_pending: Rc::new(std::cell::Cell::new(false)),
        cast_ui: Rc::new(RefCell::new(None)),
        proxy: Rc::new(RefCell::new(None)),
        guide_ui: Rc::new(RefCell::new(None)),
        tray: Rc::new(build_tray_icon()),
        i18n,
        vod_worker: Rc::new(VodWorker::new()),
        vod_pending: Rc::new(std::cell::Cell::new(false)),
        vod_ui: Rc::new(RefCell::new(None)),
        dvr: Rc::new(RefCell::new(DvrSchedule::load(dvr_path, now_seconds()))),
        recorded: Rc::new(std::cell::Cell::new(false)),
        shutdown_attempted: Rc::new(std::cell::Cell::new(false)),
        last_dvr_tick: Rc::new(std::cell::Cell::new(0.0)),
        previous_channel: Rc::new(RefCell::new(None)),
        scope_ids: Rc::new(RefCell::new(vec![None])),
        download_worker: Rc::new(iptv_core::downloads::DownloadWorker::default()),
        downloads: Rc::new(RefCell::new(Vec::new())),
        downloads_active: Rc::new(std::cell::Cell::new(false)),
        account_text: Rc::new(RefCell::new(None)),
        buffers: Rc::new(RefCell::new(buffer::BufferRegistry::default())),
    };
    wiring.player.borrow_mut().shared = Some(PlayerShared {
        buffers: Rc::clone(&wiring.buffers),
        recordings: Rc::clone(&wiring.recordings),
        cast_ui: Rc::clone(&wiring.cast_ui),
        proxy: Rc::clone(&wiring.proxy),
        cast_worker: Rc::clone(&wiring.cast_worker),
        cast_pending: Rc::clone(&wiring.cast_pending),
        timer: Rc::clone(&wiring.timer),
    });
    *wiring.reminders.borrow_mut() = wiring.persist.reminders();
    if wiring.dvr.borrow().pending_count() > 0 {
        assert!(wiring.timer.start(50, false));
    }
    reload_sources(&wiring);
    reload_epg(&wiring);
    fill_scope_choices(&wiring, controls.scope_choice);
    bind_completion(&wiring, controls);
    build_menu(frame, &wiring, controls);
    bind_tray(frame, &wiring, controls.status);
    maybe_auto_check_updates(&wiring, frame);
    bind_actions(frame, &wiring, controls);
    bind_context_menu(frame, &wiring, controls);
    wiring
        .provider_worker
        .set_cache_dir(iptv_core::config::user_config_dir().join("provider-cache"));
    bind_main_close(frame, &wiring);
}

/// The main window's close behavior: minimize-to-tray hides
/// instead of closing; a real close cancels browsing, closes
/// every live buffer session and stops the tick timer.
fn bind_main_close(frame: Frame, wiring: &Wiring) {
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    let close_persist = wiring.persist.clone();
    let close_buffers = Rc::clone(&wiring.buffers);
    frame.bind_internal(EventType::CLOSE_WINDOW, move |event| {
        if close_persist
            .config
            .borrow()
            .bool_or("minimize_to_tray", false)
        {
            frame.hide();
            event.veto();
            return;
        }
        browser.borrow_mut().cancel();
        close_buffers.borrow_mut().close_all();
        timer.stop();
        event.skip(true);
    });
}

fn bind_completion(wiring: &Wiring, controls: Controls) {
    let search = controls.search;
    let list = controls.list;
    let status = controls.status;
    let category = controls.category;
    let completed_timer = Rc::downgrade(&wiring.timer);
    let completed = wiring.clone();
    let loading_noted = Rc::new(std::cell::Cell::new(false));
    wiring.timer.on_tick(move |_| {
        if completed.browser.borrow().is_busy()
            && completed.model.borrow().is_empty()
            && !loading_noted.get()
        {
            // The list's initial text claims no playlist is loaded;
            // while a load is actually in flight, say so instead.
            list.set_accessibility_description("Loading playlist...");
            loading_noted.set(true);
        }
        let event = completed.browser.borrow_mut().poll(&search.get_value());
        match event {
            Some(BrowseEvent::Results(results)) => {
                loading_noted.set(false);
                publish_results(list, status, &completed.model, results);
                sync_category_control(category, &completed.categories, &completed.browser);
            }
            Some(BrowseEvent::Recovery(error)) => {
                status.set_label(error.message());
                list.set_accessibility_description(error.message());
            }
            None => {}
        }
        if completed.epg_deferred.get() && !completed.browser.borrow().is_busy() {
            completed.epg_deferred.set(false);
            let urls = completed.persist.config.borrow().string_list("epg_urls");
            dispatch_epg_load(&completed, urls);
        }
        poll_epg(&completed, status);
        {
            let stale = {
                let slot = completed.epg_suffixes.borrow();
                slot.1 != completed.model.borrow().playlist_ptr()
                    || (completed.epg.borrow().is_some() && now_epoch() - slot.0 > 300)
            };
            if stale {
                refresh_epg_suffixes(&completed);
                let count = list.get_item_count();
                if count > 0 {
                    list.refresh_items(0, i64::from(count) - 1);
                }
            }
        }
        poll_providers(&completed, controls);
        poll_recordings(&completed, status);
        poll_vod(&completed, status);
        poll_cast(&completed, status);
        poll_downloads(&completed, status);
        if !completed.browser.borrow().is_busy()
            && !completed.epg_pending.get()
            && !completed.provider_pending.get()
            && !completed
                .reminders
                .borrow()
                .iter()
                .any(|r| r.status == ReminderStatus::Scheduled)
            && completed.recordings.borrow().active_count() == 0
            && completed.dvr.borrow().pending_count() == 0
            && !completed.vod_pending.get()
            && !completed.cast_pending.get()
            && !completed.downloads_active.get()
        {
            if let Some(timer) = completed_timer.upgrade() {
                timer.stop();
            }
        }
    });
}

/// The Playlist view picker: switching scope re-runs the current
/// query inside the chosen playlist (or all of them).
fn bind_scope_choice(wiring: &Wiring, scope_choice: Choice, search: TextCtrl, status: Status) {
    let scope_browser = Rc::clone(&wiring.browser);
    let scope_timer = Rc::clone(&wiring.timer);
    let scope_ids = Rc::clone(&wiring.scope_ids);
    scope_choice.bind_internal(EventType::COMMAND_CHOICE_SELECTED, move |event| {
        let Some(selection) = scope_choice.get_selection() else {
            event.skip(true);
            return;
        };
        let id = scope_ids
            .borrow()
            .get(usize::try_from(selection).unwrap_or(usize::MAX))
            .cloned()
            .flatten();
        if scope_browser
            .borrow_mut()
            .set_scope(id, &search.get_value())
        {
            status.set_label("Load cancelled. Browsing previous channels...");
        }
        assert!(scope_timer.start(50, false));
        event.skip(true);
    });
}

fn bind_actions(_frame: Frame, wiring: &Wiring, controls: Controls) {
    let Controls {
        scope_choice,
        category,
        search,
        list,
        episode_description,
        stream_url,
        status,
    } = controls;
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    let categories = Rc::clone(&wiring.categories);
    let search_browser = Rc::clone(&browser);
    let poll_search = Rc::clone(&timer);
    search.on_text_changed(move |_| {
        let cancelled = search_browser.borrow_mut().search(search.get_value());
        if cancelled {
            status.set_label("Load cancelled. Searching previous channels...");
        } else {
            status.details.change_value("Searching channels...");
        }
        assert!(poll_search.start(50, false));
    });
    // Only an explicit user action moves keyboard focus. A late completion can
    // update item state but never interrupt a new editing session in Search.
    search.on_text_enter(move |_| list.set_focus());

    let category_browser = Rc::clone(&browser);
    let category_timer = Rc::clone(&timer);
    let category_choices = Rc::clone(&categories);
    category.bind_internal(EventType::TREE_SEL_CHANGED, move |event| {
        let Some(selection) = category.get_selection() else {
            event.skip(true);
            return;
        };
        let Some(position) = tree_item_index(category, &selection) else {
            event.skip(true);
            return;
        };
        let Some(chosen) = category_choices.borrow().get(position).cloned() else {
            event.skip(true);
            return;
        };
        if category_browser
            .borrow_mut()
            .select_category(chosen, &search.get_value())
        {
            status.set_label("Load cancelled. Browsing previous channels...");
        }
        assert!(category_timer.start(50, false));
        event.skip(true);
    });
    bind_scope_choice(wiring, scope_choice, search, status);
    bind_playback(wiring, list, status, episode_description, stream_url);
    bind_f1_help(&scope_choice, wiring, "playlist-view", status);
    bind_f1_help(&category, wiring, "categories", status);
    bind_f1_help(&search, wiring, "search", status);
    bind_f1_help(&list, wiring, "channel-list", status);
    bind_f1_help(&episode_description, wiring, "episode-description", status);
    bind_f1_help(&stream_url, wiring, "main-window", status);
    bind_main_hotkeys(&scope_choice, wiring, controls, status);
    bind_main_hotkeys(&category, wiring, controls, status);
    bind_main_hotkeys(&search, wiring, controls, status);
    bind_main_hotkeys(&list, wiring, controls, status);
    bind_main_hotkeys(&episode_description, wiring, controls, status);
    bind_main_hotkeys(&stream_url, wiring, controls, status);
}

/// Toggle the favorite state of the channel behind the selected or
/// focused Channels row (View menu, Ctrl+D and the context menu).
#[allow(clippy::too_many_arguments)]
fn toggle_selected_favorite(
    browser: &Rc<RefCell<BrowserWorker>>,
    model: &Rc<RefCell<SearchResults>>,
    persist: &Persist,
    timer: &Rc<Timer<Frame>>,
    list: ListCtrl,
    search: TextCtrl,
    status: Status,
) {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    let Some(row) = pick_row(selected, focused) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    let Some(index) = model.borrow().playlist_index(row) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    match browser
        .borrow_mut()
        .toggle_favorite(index, &search.get_value())
    {
        Some(outcome) if outcome.toggle == Toggle::Unchanged => {
            status.set_label("This channel cannot be marked as a favorite.");
        }
        Some(outcome) => {
            status.set_label(&outcome.announcement);
            let keys = browser.borrow().favorites().keys().to_vec();
            persist.save_favorites(&keys);
            assert!(timer.start(50, false));
        }
        None => status.set_label("No channel selected. Choose a channel in Channels first."),
    }
}

/// Fill the Playlist view picker from the configured sources and
/// accounts; the parallel scope-id list lives on the wiring.
fn fill_scope_choices(wiring: &Wiring, choice: Choice) {
    choice.clear();
    choice.append("All playlists");
    choice.set_selection(0);
    let (sources, accounts) = configured_entries(wiring);
    let mut ids: Vec<Option<String>> = vec![None];
    for source in &sources {
        choice.append(&scope_display_name(source.location()));
        ids.push(Some(source.scope_id()));
    }
    for account in &accounts {
        choice.append(&account.describe());
        ids.push(account.provider_id());
    }
    *wiring.scope_ids.borrow_mut() = ids;
}

/// The configured `playlists` entries split into plain sources (strings)
/// and provider accounts (typed dicts), as the Python loader treats them.
fn configured_entries(wiring: &Wiring) -> (Vec<PlaylistSource>, Vec<ProviderAccount>) {
    let value = wiring.persist.config.borrow().value("playlists").cloned();
    let mut sources: Vec<PlaylistSource> = Vec::new();
    let mut accounts: Vec<ProviderAccount> = Vec::new();
    match value {
        Some(serde_json::Value::Array(entries)) => {
            for entry in &entries {
                match entry {
                    serde_json::Value::String(text) => {
                        sources.push(PlaylistSource::new(text.clone()));
                    }
                    other => {
                        if let Some(account) = ProviderAccount::from_config(other) {
                            accounts.push(account);
                        }
                    }
                }
            }
        }
        Some(serde_json::Value::String(_)) => {
            for text in wiring.persist.config.borrow().string_list("playlists") {
                sources.push(PlaylistSource::new(text));
            }
        }
        _ => {}
    }
    (sources, accounts)
}

/// Drain provider worker events on the poll tick: merged channels, guide
/// sources, and a finished Stalker link resolution that completes a play.
/// What one in-flight provider resolution is for. Results are
/// correlated by request id (issue #68): a result only ever
/// reaches the entry that dispatched it, and unknown or late
/// results are dropped.
enum PendingResolve {
    Play(Channel),
    Dvr(String),
    Download {
        channel: Channel,
        programme: Programme,
    },
}

/// Retry bookkeeping for one Stalker DVR job awaiting a link.
struct DvrResolve {
    request: u64,
    in_flight: bool,
    next_try: f64,
}

/// Dispatch a live-link resolution with a fresh correlation id.
fn dispatch_resolve(
    wiring: &Wiring,
    pending: PendingResolve,
    provider_id: String,
    data: std::collections::BTreeMap<String, String>,
) -> u64 {
    let request = wiring.resolve_seq.get() + 1;
    wiring.resolve_seq.set(request);
    wiring
        .pending_resolves
        .borrow_mut()
        .insert(request, pending);
    wiring.provider_worker.resolve(request, provider_id, data);
    assert!(wiring.timer.start(50, false));
    request
}

/// Dispatch a catch-up resolution with a fresh correlation id.
fn dispatch_resolve_catchup(
    wiring: &Wiring,
    pending: PendingResolve,
    provider_id: String,
    data: std::collections::BTreeMap<String, String>,
    start_text: String,
    duration_minutes: i64,
) -> u64 {
    let request = wiring.resolve_seq.get() + 1;
    wiring.resolve_seq.set(request);
    wiring
        .pending_resolves
        .borrow_mut()
        .insert(request, pending);
    wiring.provider_worker.resolve_catchup(
        request,
        provider_id,
        data,
        start_text,
        duration_minutes,
    );
    assert!(wiring.timer.start(50, false));
    request
}

/// Recordings > Schedule Padding... (Python's dialog fields and
/// 0..=180 range, asked one at a time).
fn prompt_schedule_padding(wiring: &Wiring, frame: Frame, status: Status) {
    let ask = |label: &str, current: i64| -> Option<i64> {
        let dialog = TextEntryDialog::builder(&frame, label, "Schedule Padding")
            .with_default_value(&current.to_string())
            .build();
        if dialog.show_modal() != ID_OK {
            return None;
        }
        dialog
            .get_value()
            .unwrap_or_default()
            .trim()
            .parse::<i64>()
            .ok()
    };
    let config = wiring.persist.config.borrow();
    let pre = config
        .value("recording_pre_padding_minutes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let post = config
        .value("recording_post_padding_minutes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    drop(config);
    let Some(pre) = ask("Minutes before programme:", pre) else {
        return;
    };
    let Some(post) = ask("Minutes after programme:", post) else {
        return;
    };
    if !(0..=180).contains(&pre) || !(0..=180).contains(&post) {
        status.set_label("Padding must be between 0 and 180 minutes.");
        return;
    }
    wiring
        .persist
        .set_and_save("recording_pre_padding_minutes", serde_json::json!(pre));
    wiring
        .persist
        .set_and_save("recording_post_padding_minutes", serde_json::json!(post));
    status.set_label(&format!(
        "Schedule padding: {pre} minutes before, {post} minutes after."
    ));
}

/// Retry due Stalker DVR resolutions on the DVR tick: at most
/// one in-flight request per job, only while the padded window
/// is open; an expired window fails the job durably.
fn retry_dvr_resolves(wiring: &Wiring, now: f64) {
    let due: Vec<String> = wiring
        .dvr_resolve
        .borrow()
        .iter()
        .filter(|(_, state)| !state.in_flight && state.next_try <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for job_id in due {
        let job = wiring.dvr.borrow().get(&job_id).cloned();
        let Some(job) = job else {
            wiring.dvr_resolve.borrow_mut().remove(&job_id);
            continue;
        };
        if iptv_core::dvr::DONE_STATUSES.contains(&job.status.as_str())
            || job.status == iptv_core::dvr::STATUS_CANCELED
        {
            wiring.dvr_resolve.borrow_mut().remove(&job_id);
            continue;
        }
        if job.stop_ts > 0.0 && now >= job.stop_ts {
            wiring.dvr_resolve.borrow_mut().remove(&job_id);
            wiring.dvr.borrow_mut().mark_finished(
                &job_id,
                false,
                "",
                "Could not resolve the stream link in time.",
                now,
            );
            continue;
        }
        let channel = channel_from_snapshot(&job.channel);
        let Some(key) = channel.unknown_attributes.get("provider-id").cloned() else {
            wiring.dvr_resolve.borrow_mut().remove(&job_id);
            continue;
        };
        let request = dispatch_resolve(
            wiring,
            PendingResolve::Dvr(job_id.clone()),
            key,
            channel.unknown_attributes.clone(),
        );
        if let Some(state) = wiring.dvr_resolve.borrow_mut().get_mut(&job_id) {
            state.request = request;
            state.in_flight = true;
        }
    }
}

/// A Stalker resolve for a scheduled recording finished: record
/// with the resolved URL, or fail the job with the error.
fn handle_dvr_resolve(
    wiring: &Wiring,
    status: Status,
    job_id: &str,
    result: Result<String, String>,
) {
    let now = now_seconds();
    let Some(job) = wiring.dvr.borrow().get(job_id).cloned() else {
        wiring.dvr_resolve.borrow_mut().remove(job_id);
        return;
    };
    if iptv_core::dvr::DONE_STATUSES.contains(&job.status.as_str())
        || job.status == iptv_core::dvr::STATUS_CANCELED
    {
        wiring.dvr_resolve.borrow_mut().remove(job_id);
        return;
    }
    let Ok(url) = result else {
        let window_open = job.stop_ts <= 0.0 || now < job.stop_ts;
        if window_open {
            if let Some(state) = wiring.dvr_resolve.borrow_mut().get_mut(job_id) {
                state.in_flight = false;
                state.next_try = now + 10.0;
            }
            wiring.dvr.borrow_mut().defer_resolution(job_id, now);
            status.set_label("The portal did not answer; the scheduled recording will retry.");
        } else {
            wiring.dvr_resolve.borrow_mut().remove(job_id);
            wiring.dvr.borrow_mut().mark_finished(
                job_id,
                false,
                "",
                "Could not resolve the stream link in time.",
                now,
            );
            status.set_label("Scheduled recording failed: the stream link could not be resolved.");
        }
        return;
    };
    wiring.dvr_resolve.borrow_mut().remove(job_id);
    if job.stop_ts > 0.0 && now >= job.stop_ts {
        wiring.dvr.borrow_mut().mark_finished(
            job_id,
            false,
            "",
            "The recording window closed before the stream link was ready.",
            now,
        );
        return;
    }
    let mut channel = channel_from_snapshot(&job.channel);
    channel.url = url;
    if !wiring.dvr.borrow_mut().resolution_succeeded(job_id, now) {
        wiring.dvr_resolve.borrow_mut().remove(job_id);
        return;
    }
    record_dvr_channel(wiring, status, &job, &channel, now);
}
fn poll_providers(wiring: &Wiring, controls: Controls) {
    let status = controls.status;
    while let Some(event) = wiring.provider_worker.try_recv() {
        match event {
            ProviderEvent::Loaded {
                channels,
                epg_urls,
                failures,
            } => {
                wiring.provider_pending.set(false);
                let count = channels.len();
                if count > 0 {
                    wiring
                        .browser
                        .borrow_mut()
                        .add_channels(channels, &controls.search.get_value());
                    status.set_label(&format!("{count} provider channels loaded."));
                    assert!(wiring.timer.start(50, false));
                }
                if !epg_urls.is_empty() {
                    dispatch_epg_load(wiring, epg_urls);
                }
                for failure in failures {
                    status.set_label(&failure);
                }
            }
            ProviderEvent::Resolved { request, result } => {
                let pending = wiring.pending_resolves.borrow_mut().remove(&request);
                match pending {
                    Some(PendingResolve::Play(mut channel)) => match result {
                        Ok(url) => {
                            channel.url = url;
                            play_channel(&wiring.player, &wiring.persist, &channel, status);
                        }
                        Err(error) => status.set_label(&error),
                    },
                    Some(PendingResolve::Dvr(job_id)) => {
                        handle_dvr_resolve(wiring, status, &job_id, result);
                    }
                    Some(PendingResolve::Download { channel, programme }) => match result {
                        Ok(url) => {
                            begin_catchup_download(wiring, &channel, &programme, url, status);
                        }
                        Err(error) => status.set_label(&error),
                    },
                    None => {}
                }
            }
            ProviderEvent::Account(result) => match result {
                Ok(report) => {
                    let first = report.lines().next().unwrap_or_default().to_string();
                    status.set_label(&first);
                    if let Some(text) = wiring.account_text.borrow().as_ref() {
                        text.set_value(&report);
                    }
                }
                Err(error) => status.set_label(&error),
            },
        }
    }
}

/// Drain a finished guide load and fire due reminders; runs on the main
/// poll tick alongside browse completion.
fn poll_epg(wiring: &Wiring, status: Status) {
    if let Some(result) = wiring.epg_worker.try_recv() {
        wiring.epg_pending.set(false);
        match result {
            Ok((guide, store_note)) => {
                let counts = (guide.channels.len(), guide.programme_count());
                let indexes = GuideIndexes::build(&guide.channels);
                *wiring.epg.borrow_mut() = Some((guide, indexes));
                wiring.epg_suffixes.borrow_mut().0 = 0;
                status.set_label(&format!(
                    "Guide loaded: {} channels, {} programmes.{store_note}",
                    counts.0, counts.1
                ));
            }
            Err(error) => status.set_label(&format!("Guide failed to load: {error}")),
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let mut changed = false;
    let mut fired: Vec<String> = Vec::new();
    for reminder in wiring.reminders.borrow_mut().iter_mut() {
        if reminder.is_due(now) {
            reminder.status = ReminderStatus::Notified;
            changed = true;
            fired.push(format!(
                "Reminder: {} on {} starts at {}.",
                reminder.title,
                reminder.channel,
                local_hhmm(reminder.start)
            ));
        } else if reminder.is_expired(now) {
            reminder.status = ReminderStatus::Expired;
            changed = true;
        }
    }
    if changed {
        let rows = wiring.reminders.borrow().clone();
        wiring.persist.save_reminders(&rows);
    }
    if let Some(first) = fired.first() {
        status.set_label(first);
    }
}

/// The channel behind the list's current selection, if any.
fn selected_channel(
    browser: &Rc<RefCell<BrowserWorker>>,
    model: &Rc<RefCell<SearchResults>>,
    list: ListCtrl,
) -> Option<Channel> {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    let row = pick_row(selected, focused)?;
    let index = model.borrow().playlist_index(row)?;
    browser.borrow().channel(index)
}

/// Start or stop a manual recording of the channel.
fn toggle_recording(
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    persist: &Persist,
    channel: &Channel,
    status: Status,
) {
    let key = channel_key(channel);
    if recordings.borrow().is_recording(&key) {
        recordings.borrow_mut().stop(&key);
        release_recording_lease(buffers, &key);
        status.set_label(&format!("Stopping recording of {}.", channel.name));
        return;
    }
    if channel.url.is_empty() {
        status.set_label("This channel's stream link must be resolved before recording.");
        return;
    }
    let format = persist
        .config
        .borrow()
        .string_or("recording_format", "provider_mkv");
    match start_recording(
        recordings,
        buffers,
        persist,
        &key,
        channel,
        &format,
        &channel.name.clone(),
    ) {
        Ok(path) => status.set_label(&format!(
            "Recording {} to {}.",
            channel.name,
            path.display()
        )),
        Err(error) => status.set_label(&error),
    }
}

/// Spawn ffmpeg for one recording; returns the output path.
fn start_recording(
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    persist: &Persist,
    key: &str,
    channel: &Channel,
    format: &str,
    display_name: &str,
) -> Result<PathBuf, String> {
    let dir = recordings_dir(persist);
    let reserve_mib = recording::normalize_reserve_mib(
        persist
            .config
            .borrow()
            .value("recording_min_free_mib")
            .and_then(serde_json::Value::as_i64),
    );
    if let Some(free) = recording::free_space_bytes(&dir) {
        if free < reserve_mib * 1024 * 1024 {
            return Err(format!(
                "Not enough free space to record ({} MB free, {} MiB must stay free).",
                free / (1024 * 1024),
                reserve_mib
            ));
        }
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    let in_use = recordings.borrow().in_use_paths();
    let out = recording::unique_output_path(
        &dir,
        display_name,
        recording::format_extension(format),
        &stamp,
        &in_use,
    );
    let headers = iptv_core::playback::StreamHeaders::for_channel(channel);
    let (clean_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
    // A live buffer session for this source feeds the
    // recording through a protected view instead of a second
    // provider connection (Python's recording_input).
    let mut input_url = clean_url.clone();
    let mut input_headers = headers.clone();
    if let Some(session) = buffers.borrow_mut().find(&channel.url, &headers) {
        if let Ok(lease) = session.acquire() {
            match session.cursor(0.0, true) {
                Ok((view_url, _)) => {
                    input_url = view_url;
                    input_headers = iptv_core::playback::StreamHeaders::default();
                    buffers
                        .borrow_mut()
                        .recording_leases
                        .insert(key.to_string(), (session, lease));
                }
                Err(_) => {
                    session.release(lease);
                }
            }
        }
    }
    let cmd = recording::ffmpeg_command(
        "ffmpeg",
        &input_url,
        &out.to_string_lossy(),
        format,
        &input_headers,
        &recording::FfmpegOptions {
            show_stats: true,
            ..recording::FfmpegOptions::default()
        },
    );
    if let Err(error) = recordings.borrow_mut().start(key, &cmd, out.clone()) {
        release_recording_lease(buffers, key);
        return Err(error);
    }
    Ok(out)
}

/// The recordings folder: the configured path, else
/// ~/Videos/Accessible IPTV Recordings like the reference default.
fn recordings_dir(persist: &Persist) -> PathBuf {
    let configured = persist.config.borrow().string_or("recordings_dir", "");
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join("Videos")
        .join("Accessible IPTV Recordings")
}

/// Start one due DVR job's recording (or fail the job with the reason).
fn start_dvr_job(wiring: &Wiring, status: Status, job_id: &str, now: f64) {
    let Some(job) = wiring.dvr.borrow().get(job_id).cloned() else {
        return;
    };
    let channel = channel_from_snapshot(&job.channel);
    if channel.url.is_empty() {
        let is_stalker = channel
            .unknown_attributes
            .get("provider-type")
            .map(String::as_str)
            == Some("stalker");
        let key = channel.unknown_attributes.get("provider-id").cloned();
        match (is_stalker, key) {
            (true, Some(key)) => {
                if wiring.dvr_resolve.borrow().contains_key(job_id) {
                    return;
                }
                let request = dispatch_resolve(
                    wiring,
                    PendingResolve::Dvr(job_id.to_string()),
                    key,
                    channel.unknown_attributes.clone(),
                );
                wiring.dvr_resolve.borrow_mut().insert(
                    job_id.to_string(),
                    DvrResolve {
                        request,
                        in_flight: true,
                        next_try: now + 10.0,
                    },
                );
                status.set_label(&format!(
                    "Resolving the stream link for the scheduled recording of {}.",
                    channel.name
                ));
            }
            _ => {
                wiring.dvr.borrow_mut().mark_finished(
                    job_id,
                    false,
                    "",
                    "This channel has no stream link to record.",
                    now,
                );
            }
        }
        return;
    }
    record_dvr_channel(wiring, status, &job, &channel, now);
}

/// Start the ffmpeg side of a DVR job once its channel has a
/// playable URL (directly, or after a Stalker resolve).
fn record_dvr_channel(
    wiring: &Wiring,
    status: Status,
    job: &iptv_core::dvr::DvrJob,
    channel: &Channel,
    now: f64,
) {
    let display = if job.title.is_empty() {
        channel.name.clone()
    } else {
        job.title.clone()
    };
    match start_recording(
        &wiring.recordings,
        &wiring.buffers,
        &wiring.persist,
        &format!("dvr:{}", job.id),
        channel,
        &job.format.clone(),
        &display,
    ) {
        Ok(_) => status.set_label(&format!("Recording {display}.")),
        Err(error) => {
            wiring
                .dvr
                .borrow_mut()
                .mark_finished(&job.id, false, "", &error, now);
        }
    }
}

/// Per-tick recording housekeeping: collect finishes, run the DVR
/// state machine (every 10 s, like the reference poll), and apply the
/// shutdown-after-recordings gate.
fn poll_recordings(wiring: &Wiring, status: Status) {
    let now = now_seconds();
    for finished in wiring.recordings.borrow_mut().poll_finished() {
        release_recording_lease(&wiring.buffers, &finished.key);
        wiring.recorded.set(true);
        let name = finished
            .out_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(job_id) = finished.key.strip_prefix("dvr:") {
            wiring.dvr.borrow_mut().mark_finished(
                job_id,
                finished.success,
                &finished.out_path.to_string_lossy(),
                "",
                now,
            );
        }
        if finished.success {
            let size = std::fs::metadata(&finished.out_path).map_or(0, |m| m.len());
            status.set_label(&format!(
                "Recording saved: {name} ({}.{} MB)",
                size / 1_000_000,
                (size % 1_000_000) / 100_000
            ));
        } else {
            status.set_label(&format!("Recording failed: {name}"));
        }
    }
    if now - wiring.last_dvr_tick.get() >= 10.0 {
        wiring.last_dvr_tick.set(now);
        let actions = wiring.dvr.borrow_mut().tick(now);
        for action in actions {
            match action {
                DvrAction::Resolve {
                    job_id,
                    provider_id,
                    data,
                } => {
                    if provider_id.is_empty() {
                        wiring.dvr.borrow_mut().mark_finished(
                            &job_id,
                            false,
                            "",
                            "Scheduled recording has no provider account.",
                            now,
                        );
                        continue;
                    }
                    if wiring.dvr_resolve.borrow().contains_key(&job_id) {
                        continue;
                    }
                    wiring.dvr.borrow_mut().mark_resolving(&job_id);
                    let request = dispatch_resolve(
                        wiring,
                        PendingResolve::Dvr(job_id.clone()),
                        provider_id,
                        data,
                    );
                    wiring.dvr_resolve.borrow_mut().insert(
                        job_id,
                        DvrResolve {
                            request,
                            in_flight: true,
                            next_try: now + 10.0,
                        },
                    );
                    status.set_label("Resolving scheduled recording stream...");
                }
                DvrAction::Stop(job_id) => {
                    wiring
                        .recordings
                        .borrow_mut()
                        .stop(&format!("dvr:{job_id}"));
                }
                DvrAction::Start(job_id) => start_dvr_job(wiring, status, &job_id, now),
            }
        }
        retry_dvr_resolves(wiring, now);
    }
    let armed = wiring
        .persist
        .config
        .borrow()
        .bool_or("shutdown_after_recordings", false);
    if !wiring.shutdown_attempted.get()
        && recording::should_shutdown(
            armed,
            wiring.recorded.get(),
            wiring.recordings.borrow().active_count(),
            wiring.dvr.borrow().pending_count(),
        )
    {
        wiring.shutdown_attempted.set(true);
        open_shutdown_countdown(status);
    }
}

/// The VOD browser window's handles and view state.
#[derive(Clone)]
struct VodUi {
    frame: Frame,
    groups: ListBox,
    items: ListBox,
    status: Status,
    view: Rc<RefCell<VodView>>,
}

#[derive(Default)]
struct VodView {
    catalog: VodCatalog,
    accounts: Vec<ProviderAccount>,
    showing: Vec<VodItem>,
    stack: Vec<Vec<VodItem>>,
}

/// The provider accounts from the configured playlists (the accounts
/// half of the startup split, recomputed when the VOD window opens).
fn vod_accounts(persist: &Persist) -> Vec<ProviderAccount> {
    let value = persist.config.borrow().value("playlists").cloned();
    let mut accounts = Vec::new();
    if let Some(serde_json::Value::Array(entries)) = value {
        for entry in &entries {
            if let Some(account) = ProviderAccount::from_config(entry) {
                accounts.push(account);
            }
        }
    }
    accounts
}

/// Open (or raise) the VOD window and rebuild its catalogue
/// (shared by the button and the View menu).
fn open_vod_window(wiring: &Wiring, status: Status) {
    let ui = ensure_vod_ui(
        &wiring.vod_ui,
        &wiring.player,
        &wiring.persist,
        &wiring.vod_worker,
        &wiring.vod_pending,
        &wiring.timer,
    );
    ui.frame.show(true);
    ui.frame.raise();
    let accounts = vod_accounts(&wiring.persist);
    ui.view.borrow_mut().accounts.clone_from(&accounts);
    wiring
        .vod_worker
        .build(accounts, wiring.browser.borrow().all_channels());
    wiring.vod_pending.set(true);
    assert!(wiring.timer.start(50, false));
    status.set_label("Loading video on demand catalogue...");
}

/// Build the VOD window's widgets: (frame, groups, items, play, back,
/// status).
fn build_vod_window() -> (Frame, ListBox, ListBox, Button, Button, Status) {
    let frame = Frame::builder()
        .with_title("Video on Demand")
        .with_size(Size::new(720, 520))
        .build();
    frame.set_accessibility_label("Video on Demand");
    let panel = Panel::builder(&frame).build();
    let groups = ListBox::builder(&panel).build();
    groups.set_accessibility_label("VOD groups");
    groups.set_accessibility_description("Choose a movie or series group. Tab moves to Items.");
    let items = ListBox::builder(&panel).build();
    items.set_accessibility_label("VOD items");
    items.set_accessibility_description(
        "Choose a movie or series. Play / Open plays a movie or opens a series.",
    );
    let play = Button::builder(&panel).with_label("&Play / Open").build();
    play.set_accessibility_label("Play or open selected item");
    let back = Button::builder(&panel).with_label("&Back").build();
    back.set_accessibility_label("Back");
    let status_label = StaticText::builder(&panel).with_label("VOD status").build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("No catalogue loaded yet.")
        .build();
    details.set_accessibility_label("VOD status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    (frame, groups, items, play, back, status)
}

/// Build the VOD window once and keep it (closing hides it).
fn ensure_vod_ui(
    slot: &Rc<RefCell<Option<VodUi>>>,
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    worker: &Rc<VodWorker>,
    pending: &Rc<std::cell::Cell<bool>>,
    timer: &Rc<Timer<Frame>>,
) -> VodUi {
    if let Some(ui) = slot.borrow().as_ref() {
        return ui.clone();
    }
    let (frame, groups, items, play, back, status) = build_vod_window();
    let view = Rc::new(RefCell::new(VodView::default()));
    let ui = VodUi {
        frame,
        groups,
        items,
        status,
        view,
    };
    // Group selection shows that group's items.
    {
        let view = Rc::clone(&ui.view);
        let groups_box = groups;
        let items_box = items;
        groups.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
            let Some(selection) = groups_box.get_selection() else {
                return;
            };
            let mut view = view.borrow_mut();
            let Some(label) = view.catalog.order.get(selection as usize).cloned() else {
                return;
            };
            view.showing = view.catalog.groups.get(&label).cloned().unwrap_or_default();
            view.stack.clear();
            show_items(items_box, &view.showing);
        });
    }
    // Play / Open: movies play; series drill in (episodes inline for
    // M3U folders, fetched for Xtream series).
    {
        let view = Rc::clone(&ui.view);
        let items_box = items;
        let player = Rc::clone(player);
        let persist = persist.clone();
        let worker = Rc::clone(worker);
        let pending = Rc::clone(pending);
        let timer = Rc::clone(timer);
        play.on_click(move |_| {
            let Some(selection) = items_box.get_selection() else {
                return;
            };
            let item = view.borrow().showing.get(selection as usize).cloned();
            let Some(item) = item else { return };
            match item.kind {
                VodKind::Movie => {
                    if let Some(channel) = &item.channel {
                        play_channel(&player, &persist, channel, status);
                    }
                }
                VodKind::Series => {
                    if !item.episodes.is_empty() {
                        let mut view = view.borrow_mut();
                        let previous = std::mem::take(&mut view.showing);
                        view.stack.push(previous);
                        view.showing.clone_from(&item.episodes);
                        show_items(items_box, &view.showing);
                    } else if let Some(series_id) = &item.series_id {
                        let account = view
                            .borrow()
                            .accounts
                            .iter()
                            .find(|a| a.provider_id() == item.provider_id)
                            .cloned();
                        if let Some(account) = account {
                            worker.episodes(account, series_id.clone());
                            pending.set(true);
                            assert!(timer.start(50, false));
                            status.set_label("Loading episodes...");
                        }
                    }
                }
            }
        });
    }
    // Back: return to the previous item list.
    {
        let view = Rc::clone(&ui.view);
        let items_box = items;
        back.on_click(move |_| {
            let mut view = view.borrow_mut();
            if let Some(previous) = view.stack.pop() {
                view.showing = previous;
                show_items(items_box, &view.showing);
            }
        });
    }
    // Closing the VOD window hides it; the window is reused.
    {
        let frame_hide = frame;
        frame.on_close(move |event| {
            frame_hide.hide();
            event.skip(false);
        });
    }
    *slot.borrow_mut() = Some(ui.clone());
    ui
}

/// Fill the items list from `VodItem`s and select the first row.
fn show_items(items_box: ListBox, showing: &[VodItem]) {
    let names: Vec<String> = showing.iter().map(|i| i.name.clone()).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    items_box.set_items(&refs);
    if !refs.is_empty() {
        items_box.set_selection(0, true);
    }
}

/// Collect VOD worker events into the window.
fn poll_vod(wiring: &Wiring, status: Status) {
    while let Some(event) = wiring.vod_worker.try_recv() {
        wiring.vod_pending.set(false);
        let Some(ui) = wiring.vod_ui.borrow().clone() else {
            continue;
        };
        match event {
            VodEvent::Catalog(catalog) => {
                let groups_count = catalog.order.len();
                let items_count: usize = catalog.groups.values().map(Vec::len).sum();
                let labels = catalog.order.clone();
                ui.view.borrow_mut().catalog = catalog;
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                ui.groups.set_items(&refs);
                if !refs.is_empty() {
                    ui.groups.set_selection(0, true);
                }
                ui.status.set_label(&format!(
                    "VOD catalogue ready: {groups_count} groups, {items_count} items."
                ));
                status.set_label("VOD catalogue loaded.");
            }
            VodEvent::Episodes(Ok(episodes)) => {
                let count = episodes.len();
                let mut view = ui.view.borrow_mut();
                let previous = std::mem::take(&mut view.showing);
                view.stack.push(previous);
                view.showing = episodes;
                show_items(ui.items, &view.showing);
                ui.status.set_label(&format!("{count} episodes loaded."));
            }
            VodEvent::Episodes(Err(error)) => {
                ui.status
                    .set_label(&format!("Could not load episodes: {error}"));
            }
        }
    }
}

/// The cast window's handles and state.
#[derive(Clone)]
struct CastUi {
    frame: Frame,
    devices: ListBox,
    status: Status,
    view: Rc<RefCell<CastView>>,
}

#[derive(Default)]
struct CastView {
    devices: Vec<CastDevice>,
    channel: Option<Channel>,
}

/// Open (or raise) the Cast window and start discovery for the
/// selected channel (shared by the button and the menus).
fn open_cast_window(wiring: &Wiring, list: ListCtrl) {
    let channel = selected_channel(&wiring.browser, &wiring.model, list);
    show_cast_for(
        &wiring.cast_ui,
        &wiring.cast_worker,
        &wiring.cast_pending,
        &wiring.timer,
        &wiring.proxy,
        channel.as_ref(),
    );
}

/// The cast window's Play / Stop / Refresh bindings.
#[allow(clippy::too_many_arguments)]
fn bind_cast_buttons(
    play: Button,
    stop: Button,
    refresh: Button,
    devices: ListBox,
    view: &Rc<RefCell<CastView>>,
    worker: &Rc<CastWorker>,
    pending: &Rc<std::cell::Cell<bool>>,
    timer: &Rc<Timer<Frame>>,
    proxy: &Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    status: Status,
) {
    {
        let view = Rc::clone(view);
        let worker = Rc::clone(worker);
        let pending = Rc::clone(pending);
        let timer = Rc::clone(timer);
        let proxy = Rc::clone(proxy);
        play.on_click(move |_| {
            let Some(selection) = devices.get_selection() else {
                return;
            };
            let view = view.borrow();
            let (Some(device), Some(channel)) =
                (view.devices.get(selection as usize), view.channel.as_ref())
            else {
                return;
            };
            let (clean_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
            let headers = iptv_core::downloads::channel_headers(channel);
            let play_url = if headers.is_empty() {
                clean_url
            } else {
                let mut slot = proxy.borrow_mut();
                if slot.is_none() {
                    *slot = iptv_core::proxy::ProxyServer::start().ok();
                }
                match slot.as_ref() {
                    Some(server) => server.url_for(&clean_url, &headers),
                    None => clean_url,
                }
            };
            worker.play(device.clone(), play_url, channel.name.clone());
            pending.set(true);
            assert!(timer.start(50, false));
            status.set_label(&format!("Casting {} to {}...", channel.name, device.name));
        });
    }
    {
        let view = Rc::clone(view);
        let worker = Rc::clone(worker);
        stop.on_click(move |_| {
            let Some(selection) = devices.get_selection() else {
                return;
            };
            if let Some(device) = view.borrow().devices.get(selection as usize).cloned() {
                worker.stop(device);
                status.set_label("Stopping the cast...");
            }
        });
    }
    {
        let worker = Rc::clone(worker);
        let pending = Rc::clone(pending);
        let timer = Rc::clone(timer);
        refresh.on_click(move |_| {
            worker.discover();
            pending.set(true);
            assert!(timer.start(50, false));
            status.set_label("Looking for cast devices...");
        });
    }
}

fn ensure_cast_ui(
    slot: &Rc<RefCell<Option<CastUi>>>,
    worker: &Rc<CastWorker>,
    pending: &Rc<std::cell::Cell<bool>>,
    timer: &Rc<Timer<Frame>>,
    proxy: &Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
) -> CastUi {
    if let Some(ui) = slot.borrow().as_ref() {
        return ui.clone();
    }
    let frame = Frame::builder()
        .with_title("Cast")
        .with_size(Size::new(560, 420))
        .build();
    frame.set_accessibility_label("Cast");
    let panel = Panel::builder(&frame).build();
    let devices = ListBox::builder(&panel).build();
    devices.set_accessibility_label("Cast devices");
    devices.set_accessibility_description(
        "Choose a device. Play sends the selected channel to it; Stop ends the cast.",
    );
    let play = Button::builder(&panel).with_label("&Play").build();
    play.set_accessibility_label("Play on device");
    let stop = Button::builder(&panel).with_label("&Stop").build();
    stop.set_accessibility_label("Stop casting");
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    refresh.set_accessibility_label("Refresh devices");
    let status_label = StaticText::builder(&panel)
        .with_label("Cast status")
        .build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("No devices yet. Refresh to search your network.")
        .build();
    details.set_accessibility_label("Cast status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    let view = Rc::new(RefCell::new(CastView::default()));
    let ui = CastUi {
        frame,
        devices,
        status,
        view,
    };
    bind_cast_buttons(
        play, stop, refresh, devices, &ui.view, worker, pending, timer, proxy, status,
    );
    {
        let frame_hide = frame;
        frame.on_close(move |event| {
            frame_hide.hide();
            event.skip(false);
        });
    }
    *slot.borrow_mut() = Some(ui.clone());
    ui
}

/// Collect cast worker events into the window.
fn poll_cast(wiring: &Wiring, status: Status) {
    while let Some(event) = wiring.cast_worker.try_recv() {
        wiring.cast_pending.set(false);
        let Some(ui) = wiring.cast_ui.borrow().clone() else {
            continue;
        };
        match event {
            CastEvent::Devices(devices) => {
                let count = devices.len();
                let names: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
                ui.view.borrow_mut().devices = devices;
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                ui.devices.set_items(&refs);
                if !refs.is_empty() {
                    ui.devices.set_selection(0, true);
                }
                if count == 0 {
                    ui.status
                        .set_label("No cast devices found. Check the TV is on and on this network, then Refresh.");
                } else {
                    ui.status.set_label(&format!(
                        "Found {count} cast device(s). Choose one and press Play."
                    ));
                }
                status.set_label("Cast device search finished.");
            }
            CastEvent::Played(Ok(name)) => {
                ui.status.set_label(&format!("Casting to {name}."));
                status.set_label(&format!("Casting to {name}."));
            }
            CastEvent::Played(Err(error)) => {
                ui.status.set_label(&format!("Casting failed: {error}"));
            }
            CastEvent::Stopped(name) => {
                ui.status.set_label(&format!("Stopped casting to {name}."));
            }
        }
    }
}

/// The guide window's handles and state.
#[derive(Clone)]
struct GuideUi {
    frame: Frame,
    toc: ListBox,
    body: TextCtrl,
    status: Status,
    guide: Rc<RefCell<Guide>>,
}

/// Shallow-clone the wiring handles for menu handlers (every field
/// is an Rc handle).
fn clone_wiring(wiring: &Wiring) -> Wiring {
    Wiring {
        persist: Persist {
            config: Rc::clone(&wiring.persist.config),
            path: wiring.persist.path.clone(),
        },
        browser: Rc::clone(&wiring.browser),
        categories: Rc::clone(&wiring.categories),
        timer: Rc::clone(&wiring.timer),
        model: Rc::clone(&wiring.model),
        player: Rc::clone(&wiring.player),
        epg: Rc::clone(&wiring.epg),
        epg_suffixes: Rc::clone(&wiring.epg_suffixes),
        epg_store: Rc::clone(&wiring.epg_store),
        epg_worker: Rc::clone(&wiring.epg_worker),
        epg_pending: Rc::clone(&wiring.epg_pending),
        epg_deferred: Rc::clone(&wiring.epg_deferred),
        reminders: Rc::clone(&wiring.reminders),
        provider_worker: Rc::clone(&wiring.provider_worker),
        provider_pending: Rc::clone(&wiring.provider_pending),
        resolve_seq: Rc::clone(&wiring.resolve_seq),
        pending_resolves: Rc::clone(&wiring.pending_resolves),
        dvr_resolve: Rc::clone(&wiring.dvr_resolve),
        recordings: Rc::clone(&wiring.recordings),
        cast_worker: Rc::clone(&wiring.cast_worker),
        cast_pending: Rc::clone(&wiring.cast_pending),
        cast_ui: Rc::clone(&wiring.cast_ui),
        proxy: Rc::clone(&wiring.proxy),
        guide_ui: Rc::clone(&wiring.guide_ui),
        tray: Rc::clone(&wiring.tray),
        i18n: Rc::clone(&wiring.i18n),
        vod_worker: Rc::clone(&wiring.vod_worker),
        vod_pending: Rc::clone(&wiring.vod_pending),
        vod_ui: Rc::clone(&wiring.vod_ui),
        dvr: Rc::clone(&wiring.dvr),
        recorded: Rc::clone(&wiring.recorded),
        shutdown_attempted: Rc::clone(&wiring.shutdown_attempted),
        last_dvr_tick: Rc::clone(&wiring.last_dvr_tick),
        previous_channel: Rc::clone(&wiring.previous_channel),
        scope_ids: Rc::clone(&wiring.scope_ids),
        download_worker: Rc::clone(&wiring.download_worker),
        downloads: Rc::clone(&wiring.downloads),
        downloads_active: Rc::clone(&wiring.downloads_active),
        account_text: Rc::clone(&wiring.account_text),
        buffers: Rc::clone(&wiring.buffers),
    }
}

/// Open the User Guide window at the start of the manual.
fn open_guide_window(wiring: &Wiring, status: Status) {
    let ui = ensure_guide_ui(&wiring.guide_ui);
    let language = wiring.i18n.resolved_language();
    let (guide, section) = iptv_core::guide::open_topic(Some(&language), None, None);
    show_guide(&ui, guide);
    if let Some(section) = section {
        move_caret(&ui, &section);
    }
    ui.frame.show(true);
    ui.frame.raise();
    status.set_label("User Guide opened.");
}

/// The tray icon (kept alive on the wiring). Its popup menu is
/// built fresh per right-click in `bind_tray` (Python builds its
/// tray menu per popup, because the contents depend on live
/// player and recording state).
fn build_tray_icon() -> TaskBarIcon {
    let icon = TaskBarIcon::builder()
        .with_tooltip("Accessible IPTV Client")
        .build();
    if let Some(bitmap) = Bitmap::new(16, 16) {
        icon.set_icon(&bitmap, "Accessible IPTV Client");
    }
    icon
}

/// Tray popup menu ids.
const TRAY_RESTORE: i32 = 5501;
const TRAY_SHOW_PLAYER: i32 = 5502;
const TRAY_PLAY_PAUSE: i32 = 5503;
const TRAY_STOP: i32 = 5504;
const TRAY_CAST: i32 = 5505;
const TRAY_STOP_RECORDING: i32 = 5506;
const TRAY_EXIT: i32 = 5507;

/// Build the tray popup for the current state (Python's
/// CreatePopupMenu): Restore; a Player Controls submenu whose
/// items are all disabled when nothing is loaded in the
/// built-in player; Stop Recording(s) only while a recording
/// is active; Exit.
fn build_tray_menu(wiring: &Wiring) -> Menu {
    let loaded = wiring.player.borrow().current_channel.is_some();
    let controls = Menu::builder().build();
    for (id, label) in [
        (TRAY_SHOW_PLAYER, "Show Player"),
        (TRAY_PLAY_PAUSE, "Play/Pause"),
        (TRAY_STOP, "Stop"),
    ] {
        controls.append(id, &tr(label), "", ItemKind::Normal);
        controls.enable_item(id, loaded);
    }
    controls.append_separator();
    controls.append(TRAY_CAST, &tr("Cast / Connect..."), "", ItemKind::Normal);
    controls.enable_item(TRAY_CAST, loaded);
    let menu = Menu::builder().build();
    menu.append(TRAY_RESTORE, &tr("Restore"), "", ItemKind::Normal);
    menu.append_submenu(controls, &tr("Player Controls"), "");
    if wiring.recordings.borrow().active_count() > 0 {
        menu.append(
            TRAY_STOP_RECORDING,
            &tr("Stop Recording(s)"),
            "",
            ItemKind::Normal,
        );
    }
    menu.append_separator();
    menu.append(TRAY_EXIT, &tr("Exit"), "", ItemKind::Normal);
    menu
}

/// Bind the tray icon's dynamic popup, menu dispatch and
/// left-click restore (Python: left up and double-click both
/// restore, for NVDA/JAWS tray navigation).
fn bind_tray(frame: Frame, wiring: &Wiring, status: Status) {
    let icon = Rc::clone(&wiring.tray);
    let popup_wiring = clone_wiring(wiring);
    let popup_icon = Rc::clone(&icon);
    icon.on_mouse_right_up(move |_| {
        let mut menu = build_tray_menu(&popup_wiring);
        popup_icon.popup_menu(&mut menu);
    });
    let menu_wiring = clone_wiring(wiring);
    icon.on_menu(move |event| {
        dispatch_tray_menu(&menu_wiring, frame, status, event.get_id());
    });
    icon.on_mouse_left_up(move |_| {
        frame.show(true);
        frame.raise();
    });
    // wxdragon has no double-click event for the macOS tray
    // icon; left-up restore above covers that platform.
    #[cfg(not(target_os = "macos"))]
    {
        let dbl_frame = frame;
        icon.on_left_double_click(move |_| {
            dbl_frame.show(true);
            dbl_frame.raise();
        });
    }
}

/// One tray menu selection.
fn dispatch_tray_menu(wiring: &Wiring, frame: Frame, status: Status, id: i32) {
    match id {
        TRAY_RESTORE => {
            frame.show(true);
            frame.raise();
        }
        TRAY_SHOW_PLAYER => {
            let player = wiring.player.borrow();
            match &player.window {
                Some(win) => {
                    win.frame.show(true);
                    win.frame.raise();
                }
                None => {
                    status.set_label("The built-in player opens when you play a channel.");
                }
            }
        }
        TRAY_PLAY_PAUSE => toggle_pause_state(&wiring.player, &wiring.persist, status),
        TRAY_STOP => menu_player_transport(wiring, false),
        TRAY_CAST => {
            let player = wiring.player.borrow();
            if let Some(shared) = &player.shared {
                show_cast_for(
                    &shared.cast_ui,
                    &shared.cast_worker,
                    &shared.cast_pending,
                    &shared.timer,
                    &shared.proxy,
                    player.current_channel.as_ref(),
                );
            }
        }
        TRAY_STOP_RECORDING => {
            let keys = wiring.recordings.borrow().active_keys();
            let count = keys.len();
            for key in &keys {
                wiring.recordings.borrow_mut().stop(key);
            }
            if count > 0 {
                status.set_label(&format!("Stopping {count} recordings."));
            }
        }
        TRAY_EXIT => {
            frame.close(false);
        }
        _ => {}
    }
}

/// The main window's menu bar. Labels carry the effective shortcuts
/// from the shortcuts engine, so a config override changes the menu
/// Menu command ids for the parity menu tree (G2). Ids 4004/5002/
/// 5003/5004 predate the tree (exit, guide, updates, tray show).
pub const PLAYLIST_MANAGER: i32 = 5101;
pub const EPG_MANAGER: i32 = 5102;
pub const IMPORT_EPG: i32 = 5103;
pub const WHATS_ON_NOW: i32 = 5104;
pub const WHAT_IS_PLAYING: i32 = 5105;
pub const ACCOUNT_INFO: i32 = 5106;
pub const CAST_TO: i32 = 5107;
pub const SHOW_PLAYER: i32 = 5111;
pub const PLAY_PAUSE: i32 = 5112;
pub const STOP: i32 = 5113;
pub const CAST_CONNECT: i32 = 5114;
pub const VIEW_LIVE: i32 = 5121;
pub const VIEW_VOD: i32 = 5122;
pub const CONTINUE_WATCHING: i32 = 5123;
pub const FAVORITE: i32 = 5124;
pub const GO_TO_FAVORITES: i32 = 5125;
pub const PREVIOUS_CHANNEL: i32 = 5126;
pub const RECENT_CHANNELS: i32 = 5127;
pub const CHANNEL_NUMBER: i32 = 5128;
pub const SHOW_DOWNLOADS: i32 = 5129;
pub const PLAYER_RADIO_BASE: i32 = 5131;
pub const CUSTOM_PLAYER: i32 = 5163;
pub const PREFERRED_AUDIO: i32 = 5164;
pub const LANGUAGE_BASE: i32 = 5170;
pub const MINIMIZE_TO_TRAY: i32 = 5195;
pub const SHOW_PLAYER_ON_ENTER: i32 = 5196;
pub const SHOW_STREAM_URL: i32 = 5197;
pub const AUTO_CHECK_UPDATES: i32 = 5198;
pub const EXPORT_SETTINGS: i32 = 5199;
pub const IMPORT_SETTINGS: i32 = 5200;
pub const ANNOUNCE_BASE: i32 = 5165;
pub const SPEAK_SUBTITLES: i32 = 5185;
pub const READ_CURRENT_SUBTITLE: i32 = 5186;
pub const KEYBOARD_SHORTCUTS: i32 = 5190;
pub const WHATS_NEW: i32 = 5191;
pub const OPEN_LOGS: i32 = 5192;
pub const COPY_DEBUG: i32 = 5193;
pub const ABOUT: i32 = 5194;
pub const READ_PREVIOUS_SUBTITLE: i32 = 5187;
pub const REVIEW_SUBTITLES: i32 = 5188;
pub const SUBTITLE_CAPABILITIES: i32 = 5189;
pub const REC_START: i32 = 5201;
pub const REC_STOP: i32 = 5202;
pub const REC_STOP_ALL: i32 = 5203;
pub const REC_REMINDERS: i32 = 5204;
pub const REC_STORAGE: i32 = 5205;
pub const REC_MIN_FREE: i32 = 5206;
pub const REC_SCHEDULED: i32 = 5207;
pub const REC_FORMAT_BASE: i32 = 5210;
pub const REC_PADDING: i32 = 5208;
pub const REC_OPEN_FOLDER: i32 = 5221;
pub const REC_SET_FOLDER: i32 = 5222;
pub const REC_SHUTDOWN_AFTER: i32 = 5223;

/// The "Media Player to Use" radio labels, in Python's order.
const PLAYER_LABELS: [&str; 32] = [
    "Built-in Player",
    "VLC",
    "MPC",
    "MPC-BE",
    "Kodi",
    "Winamp",
    "Foobar2000",
    "MPV",
    "SMPlayer",
    "Totem",
    "QuickTime",
    "iTunes/Apple Music",
    "PotPlayer",
    "KMPlayer",
    "AIMP",
    "QMPlay2",
    "GOM Player",
    "Audacious",
    "Fauxdacious",
    "Clementine",
    "Strawberry",
    "Amarok",
    "Rhythmbox",
    "Pragha",
    "Lollypop",
    "Exaile",
    "Quod Libet",
    "Gmusicbrowser",
    "Xmms",
    "Vocal",
    "Haruna",
    "Celluloid",
];

/// Append one item to a menu under construction.
fn menu_add(menu: &Menu, id: i32, label: &str, help: &str, kind: ItemKind) {
    menu.append(id, &tr(label), &tr(help), kind);
}

/// Build the full Python-parity menu tree (File, Player, View,
/// Options, Help) with initial check states from the config. Items
/// whose dialogs land in later GUI slices are present but disabled,
/// as the audit prescribes.
/// A menu label carrying the command's effective shortcut, when set.
fn menu_label(keys: &std::collections::BTreeMap<String, String>, text: &str, name: &str) -> String {
    match keys.get(name) {
        Some(k) if !k.is_empty() => format!("{text}\t{k}"),
        _ => text.to_string(),
    }
}

fn build_menu(frame: Frame, wiring: &Wiring, controls: Controls) {
    set_translator(&wiring.i18n);
    let config = wiring.persist.config.borrow();
    let raw = serde_json::Value::Object(config.raw().clone());
    let keys = iptv_core::shortcuts::effective(&raw, "main");
    let file_menu = build_file_menu(&keys);
    let player_menu = build_player_menu(&keys);
    let view_menu = build_view_menu(&keys);
    let options_menu = build_options_menu(&config);
    let recordings_menu = build_recordings_menu(&config);
    let help_menu = build_help_menu();
    let menu_bar = MenuBar::builder()
        .append(file_menu, "&File")
        .append(player_menu, "&Player")
        .append(view_menu, "&View")
        .append(options_menu, "&Options")
        .append(recordings_menu, "&Recordings")
        .append(help_menu, "&Help")
        .build();
    frame.set_menu_bar(menu_bar);
    bind_menu_actions(frame, wiring, controls);
}

#[allow(clippy::too_many_lines)]
fn build_file_menu(keys: &std::collections::BTreeMap<String, String>) -> Menu {
    let file_menu = Menu::builder().build();
    menu_add(
        &file_menu,
        PLAYLIST_MANAGER,
        &menu_label(keys, "&Playlist Manager", "playlist_manager"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &file_menu,
        EPG_MANAGER,
        &menu_label(keys, "&EPG Manager", "epg_manager"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &file_menu,
        IMPORT_EPG,
        &menu_label(keys, "Import EPG to &DB", "import_epg"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &file_menu,
        WHATS_ON_NOW,
        &menu_label(keys, "What's on &Now", "whats_on_now"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &file_menu,
        WHAT_IS_PLAYING,
        &menu_label(keys, "What Is &Playing", "what_is_playing"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &file_menu,
        ACCOUNT_INFO,
        &menu_label(keys, "&Account Info", "account"),
        "",
        ItemKind::Normal,
    );
    file_menu.append_separator();
    menu_add(&file_menu, CAST_TO, "Cast &To...", "", ItemKind::Normal);
    file_menu.append_separator();
    menu_add(
        &file_menu,
        4004,
        &menu_label(keys, "E&xit", "exit"),
        "Close the application",
        ItemKind::Normal,
    );

    file_menu
}

fn build_player_menu(keys: &std::collections::BTreeMap<String, String>) -> Menu {
    let player_menu = Menu::builder().build();
    menu_add(
        &player_menu,
        SHOW_PLAYER,
        &menu_label(keys, "Show &Built-in Player", "show_player"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &player_menu,
        PLAY_PAUSE,
        &menu_label(keys, "Play/&Pause", "play_pause"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &player_menu,
        STOP,
        &menu_label(keys, "&Stop", "stop"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &player_menu,
        CAST_CONNECT,
        &menu_label(keys, "Cast / &Connect...", "cast"),
        "",
        ItemKind::Normal,
    );

    player_menu
}

fn build_view_menu(keys: &std::collections::BTreeMap<String, String>) -> Menu {
    let view_menu = Menu::builder().build();
    menu_add(
        &view_menu,
        VIEW_LIVE,
        "Live TV && Catch-up",
        "",
        ItemKind::Radio,
    );
    menu_add(
        &view_menu,
        VIEW_VOD,
        "Video on Demand (Movies && Series)",
        "",
        ItemKind::Radio,
    );
    view_menu.check_item(VIEW_LIVE, true);
    view_menu.append_separator();
    menu_add(
        &view_menu,
        CONTINUE_WATCHING,
        "Continue &Watching...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &view_menu,
        FAVORITE,
        &menu_label(keys, "&Add to Favorites", "favorite"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &view_menu,
        GO_TO_FAVORITES,
        "Go to Favorites",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &view_menu,
        PREVIOUS_CHANNEL,
        &menu_label(keys, "Pre&vious Channel", "previous_channel"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &view_menu,
        RECENT_CHANNELS,
        &menu_label(keys, "&Recently Watched...", "recent_channels"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &view_menu,
        CHANNEL_NUMBER,
        &menu_label(keys, "Go to Channel &Number...", "channel_number"),
        "",
        ItemKind::Normal,
    );
    view_menu.append_separator();
    menu_add(
        &view_menu,
        SHOW_DOWNLOADS,
        &menu_label(keys, "Show &Downloads", "downloads"),
        "",
        ItemKind::Normal,
    );

    view_menu
}

/// The Options > Language radio submenu.
fn build_language_menu(config: &Config) -> Menu {
    let language_sub = Menu::builder().build();
    let current_language = config.string_or("language", "automatic");
    for (offset, (code, name)) in iptv_core::i18n::language_labels().iter().enumerate() {
        let id = LANGUAGE_BASE + i32::try_from(offset).unwrap_or(0);
        menu_add(&language_sub, id, name, "", ItemKind::Radio);
        language_sub.check_item(id, *code == current_language);
    }
    language_sub
}

/// The Options > Automatic Announcements radio submenu.
fn build_announce_menu(config: &Config) -> Menu {
    let announce_sub = Menu::builder().build();
    let level = config
        .value("announcement_level")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    for (offset, name) in ["None", "Errors only", "Important events", "Detailed status"]
        .iter()
        .enumerate()
    {
        let id = ANNOUNCE_BASE + i32::try_from(offset).unwrap_or(0);
        menu_add(&announce_sub, id, name, "", ItemKind::Radio);
        announce_sub.check_item(id, i64::try_from(offset).unwrap_or(0) == level);
    }
    announce_sub
}

/// The Options > Subtitle Speech submenu.
/// The Recordings menu, mirroring Python's `_populate_recordings_menu`.
fn build_recordings_menu(config: &Config) -> Menu {
    let menu = Menu::builder().build();
    menu_add(
        &menu,
        REC_START,
        "Start &Recording",
        "Record the selected channel",
        ItemKind::Normal,
    );
    menu_add(&menu, REC_STOP, "Sto&p Recording", "", ItemKind::Normal);
    menu_add(
        &menu,
        REC_STOP_ALL,
        "Stop &All Recordings",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(
        &menu,
        REC_REMINDERS,
        "Programme &Reminders...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        REC_STORAGE,
        "Check Recording &Storage...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        REC_MIN_FREE,
        "&Minimum Free Space...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        REC_SCHEDULED,
        "&Scheduled Recordings...",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    let format_sub = Menu::builder().build();
    let current = config.string_or("recording_format", "provider_mkv");
    for (offset, (id, label, _, _)) in iptv_core::recording::RECORDING_FORMATS.iter().enumerate() {
        let radio_id = REC_FORMAT_BASE + i32::try_from(offset).unwrap_or(0);
        menu_add(&format_sub, radio_id, label, "", ItemKind::Radio);
        format_sub.check_item(radio_id, *id == current);
    }
    menu.append_submenu(format_sub, "Recording &Format", "");
    menu_add(
        &menu,
        REC_PADDING,
        "Schedule &Padding...",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(
        &menu,
        REC_OPEN_FOLDER,
        "&Open Recordings Folder",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        REC_SET_FOLDER,
        "Set &Download Folder...",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(
        &menu,
        REC_SHUTDOWN_AFTER,
        "Shut Down the Computer &When Recordings Finish",
        "",
        ItemKind::Check,
    );
    menu.check_item(
        REC_SHUTDOWN_AFTER,
        config.bool_or("shutdown_after_recordings", false),
    );
    menu
}

/// Options > Subtitle Speech > Read previous subtitle.
fn menu_read_previous_subtitle(wiring: &Wiring, status: Status) {
    let player = wiring.player.borrow();
    if player.window.is_none() {
        status.set_label("No subtitle is showing.");
        return;
    }
    let position = position_length(&player).0;
    match player
        .subtitle_cues
        .iter()
        .rev()
        .find(|cue| cue.end <= position)
    {
        Some(cue) => status.set_label(&cue.text),
        None => status.set_label("No previous subtitle."),
    }
}

/// Options > Subtitle Speech > Review recent subtitles...
fn open_review_subtitles(wiring: &Wiring, status: Status) {
    let recent: Vec<String> = {
        let player = wiring.player.borrow();
        if player.window.is_none() {
            status.set_label("No subtitles are loaded.");
            return;
        }
        let cues = &player.subtitle_cues;
        let start = cues.len().saturating_sub(20);
        cues[start..].iter().map(|c| c.text.clone()).collect()
    };
    if recent.is_empty() {
        status.set_label("No subtitles are loaded.");
        return;
    }
    let (frame, _panel, list, dstatus) = list_dialog("Recent Subtitles", "Recent subtitles");
    list.set_items(&recent.iter().map(String::as_str).collect::<Vec<_>>());
    if !recent.is_empty() {
        list.set_selection(u32::try_from(recent.len() - 1).unwrap_or(0), true);
    }
    dstatus.set_label(&format!("{} recent subtitles.", recent.len()));
    frame.show(true);
}

/// Options > Subtitle Speech > Subtitle capabilities...
fn show_subtitle_capabilities(frame: Frame, status: Status) {
    let text = "Subtitle support in this build:\n\n- The built-in player shows sidecar subtitle files (SRT, WebVTT, ASS/SSA) loaded with Load Subtitles.\n- Speak subtitles announces each cue through your screen reader when it appears.\n- Read current subtitle and Read previous subtitle speak on demand from the Options > Subtitle Speech menu.\n- External players handle subtitles themselves; their support varies by player.";
    let dialog = MessageDialog::builder(&frame, text, "Subtitle Capabilities")
        .with_style(MessageDialogStyle::OK)
        .build();
    dialog.show_modal();
    status.set_label("Subtitle capabilities shown.");
}

/// Recordings > Check Recording Storage...
fn show_recording_storage(wiring: &Wiring, frame: Frame, status: Status) {
    let dir = recordings_dir(&wiring.persist);
    let reserve = recording::normalize_reserve_mib(
        wiring
            .persist
            .config
            .borrow()
            .value("recording_min_free_mib")
            .and_then(serde_json::Value::as_i64),
    );
    let free_text = match recording::free_space_bytes(&dir) {
        Some(free) => format!("{} MB free", free / (1024 * 1024)),
        None => "free space unavailable".to_string(),
    };
    let text = format!(
        "Recordings folder: {}\nSpace: {free_text}\nMinimum free space before a recording starts: {reserve} MiB",
        dir.display()
    );
    let dialog = MessageDialog::builder(&frame, &text, "Recording Storage")
        .with_style(MessageDialogStyle::OK)
        .build();
    dialog.show_modal();
    status.set_label("Recording storage shown.");
}

/// Recordings > Minimum Free Space...
fn prompt_min_free_space(wiring: &Wiring, frame: Frame, status: Status) {
    let current = recording::normalize_reserve_mib(
        wiring
            .persist
            .config
            .borrow()
            .value("recording_min_free_mib")
            .and_then(serde_json::Value::as_i64),
    );
    let dialog = TextEntryDialog::builder(
        &frame,
        "Minimum free space to keep on the recordings drive, in MiB (0-65536):",
        "Minimum Free Space",
    )
    .with_default_value(&current.to_string())
    .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(value) = text.trim().parse::<i64>() else {
        status.set_label("Enter a whole number of MiB.");
        return;
    };
    let normalized = recording::normalize_reserve_mib(Some(value));
    wiring
        .persist
        .set_and_save("recording_min_free_mib", serde_json::json!(normalized));
    status.set_label(&format!(
        "Minimum free space before recording: {normalized} MiB."
    ));
}

fn build_speech_menu(config: &Config) -> Menu {
    let speech_sub = Menu::builder().build();
    menu_add(
        &speech_sub,
        SPEAK_SUBTITLES,
        "Speak subtitles",
        "",
        ItemKind::Check,
    );
    speech_sub.check_item(SPEAK_SUBTITLES, config.bool_or("speak_subtitles", false));
    menu_add(
        &speech_sub,
        READ_CURRENT_SUBTITLE,
        "Read current subtitle",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &speech_sub,
        READ_PREVIOUS_SUBTITLE,
        "Read previous subtitle",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &speech_sub,
        REVIEW_SUBTITLES,
        "Review recent subtitles...",
        "",
        ItemKind::Normal,
    );
    speech_sub.append_separator();
    menu_add(
        &speech_sub,
        SUBTITLE_CAPABILITIES,
        "Subtitle capabilities...",
        "",
        ItemKind::Normal,
    );
    speech_sub
}

/// The Options menu's four preference check items, with initial
/// states from the config.
fn add_option_checks(options_menu: &Menu, config: &Config) {
    menu_add(
        options_menu,
        MINIMIZE_TO_TRAY,
        "Minimize to System &Tray",
        "",
        ItemKind::Check,
    );
    options_menu.check_item(MINIMIZE_TO_TRAY, config.bool_or("minimize_to_tray", false));
    menu_add(
        options_menu,
        SHOW_PLAYER_ON_ENTER,
        "Show Player on &Enter",
        "",
        ItemKind::Check,
    );
    options_menu.check_item(
        SHOW_PLAYER_ON_ENTER,
        config.bool_or("show_player_on_enter", true),
    );
    menu_add(
        options_menu,
        SHOW_STREAM_URL,
        "Show Stream &URL",
        "",
        ItemKind::Check,
    );
    options_menu.check_item(SHOW_STREAM_URL, config.bool_or("show_channel_url", true));
    menu_add(
        options_menu,
        AUTO_CHECK_UPDATES,
        "Auto-check for &Updates",
        "",
        ItemKind::Check,
    );
    options_menu.check_item(
        AUTO_CHECK_UPDATES,
        config.bool_or("auto_check_updates", false),
    );
}

fn build_options_menu(config: &Config) -> Menu {
    let options_menu = Menu::builder().build();
    let player_sub = Menu::builder().build();
    let current_player = config.string_or("media_player", "Built-in Player");
    for (offset, name) in PLAYER_LABELS.iter().enumerate() {
        let id = PLAYER_RADIO_BASE + i32::try_from(offset).unwrap_or(0);
        menu_add(&player_sub, id, name, "", ItemKind::Radio);
        player_sub.check_item(id, *name == current_player);
    }
    player_sub.append_separator();
    menu_add(
        &player_sub,
        CUSTOM_PLAYER,
        "Custom Player...",
        "",
        ItemKind::Radio,
    );
    player_sub.check_item(CUSTOM_PLAYER, current_player == "Custom");
    options_menu.append_submenu(player_sub, "Media Player to &Use", "");
    menu_add(
        &options_menu,
        PREFERRED_AUDIO,
        "Preferred &Audio Track...",
        "",
        ItemKind::Normal,
    );
    let language_sub = build_language_menu(config);
    options_menu.append_submenu(language_sub, "&Language", "");
    add_option_checks(&options_menu, config);
    menu_add(
        &options_menu,
        EXPORT_SETTINGS,
        "E&xport Settings...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &options_menu,
        IMPORT_SETTINGS,
        "I&mport Settings...",
        "",
        ItemKind::Normal,
    );
    let announce_sub = build_announce_menu(config);
    options_menu.append_submenu(announce_sub, "Automatic &Announcements", "");
    let speech_sub = build_speech_menu(config);
    options_menu.append_submenu(speech_sub, "Subtitle &Speech", "");
    menu_add(
        &options_menu,
        KEYBOARD_SHORTCUTS,
        "&Keyboard Shortcuts...",
        "",
        ItemKind::Normal,
    );
    options_menu
}

fn build_help_menu() -> Menu {
    let help_menu = Menu::builder().build();
    menu_add(
        &help_menu,
        5002,
        "User &Guide\tF1",
        "Open the User Guide",
        ItemKind::Normal,
    );
    menu_add(&help_menu, WHATS_NEW, "What's &New", "", ItemKind::Normal);
    help_menu.append_separator();
    menu_add(
        &help_menu,
        5003,
        "Check for &Updates...",
        "Check GitHub for a newer release",
        ItemKind::Normal,
    );
    menu_add(
        &help_menu,
        OPEN_LOGS,
        "Open &Logs Folder",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &help_menu,
        COPY_DEBUG,
        "Copy Log and &Debug Information",
        "",
        ItemKind::Normal,
    );
    help_menu.append_separator();
    menu_add(&help_menu, ABOUT, "&About...", "", ItemKind::Normal);

    help_menu
}

/// Menu items that simply open a manager or list dialog.
/// Returns true when the id was one of them.
fn menu_dialog_action(id: i32, wiring: &Wiring, status: Status, scope_choice: Choice) -> bool {
    match id {
        PLAYLIST_MANAGER => open_playlist_manager(wiring, status, scope_choice),
        EPG_MANAGER => open_epg_manager(wiring, status),
        WHATS_ON_NOW => open_whats_on_now(wiring, status),
        ACCOUNT_INFO => open_account_info(wiring, status),
        CONTINUE_WATCHING => open_continue_watching(wiring, status),
        RECENT_CHANNELS => open_recent_channels(wiring, status),
        SHOW_DOWNLOADS => open_downloads_window(wiring, status),
        _ => return false,
    }
    true
}

/// Dispatch for the parity menu tree.
fn bind_menu_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    bind_menu_option_actions(frame, wiring, controls);
    let status = controls.status;
    let list = controls.list;
    let search = controls.search;
    let category_box = controls.category;
    let scope_choice = controls.scope_choice;
    let wiring = clone_wiring(wiring);
    frame.on_menu(move |event| {
        let id = event.get_id();
        let persist = &wiring.persist;
        if menu_dialog_action(id, &wiring, status, scope_choice) {
            return;
        }
        match id {
            4004 => frame.close(false),
            5002 => open_guide_window(&wiring, status),
            5003 => check_updates(&frame, ship_shape::ui::CheckTrigger::Manual),
            5004 => {
                frame.show(true);
                frame.raise();
            }
            IMPORT_EPG => menu_import_epg(&wiring, frame, status),
            WHAT_IS_PLAYING => {
                let player = wiring.player.borrow();
                match &player.current_channel {
                    Some(channel) => status.set_label(&format!("Now playing: {}", channel.name)),
                    None => status.set_label("Nothing is playing."),
                }
            }
            CAST_TO | CAST_CONNECT => open_cast_window(&wiring, list),
            SHOW_PLAYER => {
                let player = wiring.player.borrow();
                match &player.window {
                    Some(win) => {
                        win.frame.show(true);
                        win.frame.raise();
                    }
                    None => {
                        status.set_label("The built-in player opens when you play a channel.");
                    }
                }
            }
            PLAY_PAUSE | STOP => menu_player_transport(&wiring, id == PLAY_PAUSE),
            VIEW_LIVE => status.set_label("Live TV and catch-up view."),
            VIEW_VOD => open_vod_window(&wiring, status),
            FAVORITE => toggle_selected_favorite(
                &wiring.browser,
                &wiring.model,
                persist,
                &wiring.timer,
                list,
                search,
                status,
            ),
            GO_TO_FAVORITES => {
                if let Some(item) = tree_item_at(category_box, 1) {
                    category_box.select_item(&item);
                }
                if wiring
                    .browser
                    .borrow_mut()
                    .select_category(Category::Favorites, &search.get_value())
                {
                    status.set_label("Load cancelled. Browsing previous channels...");
                }
                assert!(wiring.timer.start(50, false));
            }
            PREVIOUS_CHANNEL => {
                let previous = wiring.previous_channel.borrow().clone();
                match previous {
                    Some(channel) => {
                        play_channel(&wiring.player, persist, &channel, status);
                    }
                    None => status.set_label("No previous channel yet."),
                }
            }
            CHANNEL_NUMBER => menu_go_to_channel_number(&wiring, frame, list, status),
            _ => {}
        }
    });
}

/// Recordings menu dispatch. Returns true when the id belonged
/// to the Recordings menu.
fn menu_recordings_action(
    id: i32,
    wiring: &Wiring,
    frame: Frame,
    list: ListCtrl,
    status: Status,
) -> bool {
    let persist = &wiring.persist;
    match id {
        REC_START => {
            if let Some(channel) = selected_channel(&wiring.browser, &wiring.model, list) {
                toggle_recording(
                    &wiring.recordings,
                    &wiring.buffers,
                    &wiring.persist,
                    &channel,
                    status,
                );
            } else {
                status.set_label("Select a channel to record first.");
            }
        }
        REC_STOP => {
            if let Some(channel) = selected_channel(&wiring.browser, &wiring.model, list) {
                let key = channel_key(&channel);
                if wiring.recordings.borrow().is_recording(&key) {
                    wiring.recordings.borrow_mut().stop(&key);
                    release_recording_lease(&wiring.buffers, &key);
                    status.set_label(&format!("Stopping recording of {}.", channel.name));
                } else {
                    status.set_label("That channel is not recording.");
                }
            } else {
                status.set_label("Select a channel first.");
            }
        }
        REC_STOP_ALL => {
            let keys = wiring.recordings.borrow().active_keys();
            if keys.is_empty() {
                status.set_label("No recordings are in progress.");
            } else {
                let count = keys.len();
                for key in &keys {
                    wiring.recordings.borrow_mut().stop(key);
                    release_recording_lease(&wiring.buffers, key);
                }
                status.set_label(&format!("Stopping {count} recordings."));
            }
        }
        REC_REMINDERS => open_reminders_dialog(wiring, status),
        REC_SCHEDULED => open_scheduled_dialog(wiring, status),
        REC_STORAGE => show_recording_storage(wiring, frame, status),
        REC_MIN_FREE => prompt_min_free_space(wiring, frame, status),
        REC_PADDING => prompt_schedule_padding(wiring, frame, status),
        REC_OPEN_FOLDER => {
            let dir = recordings_dir(&wiring.persist);
            let _ = std::fs::create_dir_all(&dir);
            open_folder(&dir);
            status.set_label("Opened the recordings folder.");
        }
        REC_SET_FOLDER => {
            let dialog =
                DirDialog::builder(&frame, "Choose the folder for recordings and downloads", "")
                    .build();
            if dialog.show_modal() == ID_OK {
                if let Some(path) = dialog.get_path() {
                    wiring
                        .persist
                        .set_and_save("recordings_dir", serde_json::json!(path));
                    status.set_label("Download folder updated.");
                }
            }
        }
        REC_SHUTDOWN_AFTER => {
            let next = !persist
                .config
                .borrow()
                .bool_or("shutdown_after_recordings", false);
            persist.set_and_save("shutdown_after_recordings", serde_json::json!(next));
            status.set_label(if next {
                "The computer will shut down when recordings finish."
            } else {
                "Shutdown after recordings is off."
            });
        }
        REC_FORMAT_BASE..=5219 => {
            let offset = usize::try_from(id - REC_FORMAT_BASE).unwrap_or(0);
            if let Some((format_id, label, _, _)) =
                iptv_core::recording::RECORDING_FORMATS.get(offset)
            {
                wiring
                    .persist
                    .set_and_save("recording_format", serde_json::json!(format_id));
                status.set_label(&format!("Recording format: {label}."));
            }
        }
        _ => return false,
    }
    true
}

/// Radio-range menu dispatch (player choice, language,
/// announcement level). Returns true when the id was a radio.
fn menu_radio_action(id: i32, wiring: &Wiring, status: Status) -> bool {
    let persist = &wiring.persist;
    if (PLAYER_RADIO_BASE..PLAYER_RADIO_BASE + 32).contains(&id) {
        let index = usize::try_from(id - PLAYER_RADIO_BASE).unwrap_or(0);
        if let Some(name) = PLAYER_LABELS.get(index) {
            persist.set_and_save("media_player", serde_json::json!(name));
            status.set_label(&format!("Media player set to {name}."));
        }
        return true;
    }
    if (LANGUAGE_BASE..LANGUAGE_BASE + 15).contains(&id) {
        let index = usize::try_from(id - LANGUAGE_BASE).unwrap_or(0);
        if let Some((code, name)) = iptv_core::i18n::language_labels().get(index) {
            persist.set_and_save("language", serde_json::json!(code));
            status.set_label(&format!(
                "Language set to {name}. Restart to apply everywhere."
            ));
        }
        return true;
    }
    if (ANNOUNCE_BASE..ANNOUNCE_BASE + 4).contains(&id) {
        persist.set_and_save("announcement_level", serde_json::json!(id - ANNOUNCE_BASE));
        return true;
    }
    false
}

fn bind_menu_option_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    let status = controls.status;
    let stream_url = controls.stream_url;
    let wiring = clone_wiring(wiring);
    frame.on_menu(move |event| {
        let id = event.get_id();
        if id < 5130 {
            return;
        }
        if menu_recordings_action(id, &wiring, frame, controls.list, status)
            || menu_radio_action(id, &wiring, status)
        {
            return;
        }
        let persist = &wiring.persist;
        let flip = |key_name: &str| -> bool {
            let next = !persist.config.borrow().bool_or(key_name, false);
            persist.set_and_save(key_name, serde_json::json!(next));
            next
        };
        match id {
            CUSTOM_PLAYER => menu_custom_player(&wiring, frame, status),
            EXPORT_SETTINGS => menu_export_settings(&wiring, frame, status),
            IMPORT_SETTINGS => menu_import_settings(&wiring, frame, status),
            MINIMIZE_TO_TRAY => {
                flip("minimize_to_tray");
            }
            SHOW_PLAYER_ON_ENTER => {
                let next = !persist.config.borrow().bool_or("show_player_on_enter", true);
                persist.set_and_save("show_player_on_enter", serde_json::json!(next));
            }
            SHOW_STREAM_URL => {
                let next = !persist.config.borrow().bool_or("show_channel_url", true);
                persist.set_and_save("show_channel_url", serde_json::json!(next));
                if next {
                    stream_url.show(true);
                } else {
                    stream_url.hide();
                }
            }
            AUTO_CHECK_UPDATES => {
                flip("auto_check_updates");
            }
            SPEAK_SUBTITLES => {
                flip("speak_subtitles");
            }
            PREFERRED_AUDIO => open_audio_prefs(&wiring, status),
            KEYBOARD_SHORTCUTS => open_shortcuts_editor(&wiring, status),
            WHATS_NEW => open_whats_new(status),
            READ_CURRENT_SUBTITLE => menu_read_current_subtitle(&wiring, status),
            READ_PREVIOUS_SUBTITLE => menu_read_previous_subtitle(&wiring, status),
            REVIEW_SUBTITLES => open_review_subtitles(&wiring, status),
            SUBTITLE_CAPABILITIES => show_subtitle_capabilities(frame, status),
            OPEN_LOGS => {
                let dir = iptv_core::config::user_config_dir();
                open_folder(&dir);
                status.set_label("Opened the settings folder, which holds the logs.");
            }
            COPY_DEBUG => {
                let text = format!(
                    "Accessible IPTV Client {}\nOS: {}\nConfig: {}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    persist.path.display()
                );
                if Clipboard::get().set_text(&text) {
                    status.set_label("Log and debug information copied.");
                }
            }
            ABOUT => {
                let dialog = MessageDialog::builder(
                    &frame,
                    &format!(
                        "Accessible IPTV Client\nVersion {}\nAccessible IPTV player — Rust port of the Python client.",
                        env!("CARGO_PKG_VERSION")
                    ),
                    "About Accessible IPTV Client",
                )
                .build();
                dialog.show_modal();
            }
            _ => {}
        }
    });
}

/// File > Import EPG to DB: pick an XMLTV file, remember it as a
/// guide source, and load it now.
fn menu_import_epg(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Import EPG (XMLTV)")
        .with_wildcard("XMLTV (*.xml;*.xml.gz)|*.xml;*.xml.gz|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    let mut urls = wiring.persist.config.borrow().string_list("epg_urls");
    if !urls.contains(&path) {
        urls.push(path.clone());
        wiring
            .persist
            .set_and_save("epg_urls", serde_json::json!(urls));
    }
    dispatch_epg_load(wiring, vec![path]);
    status.set_label("Importing EPG data...");
}

/// View > Go to Channel Number...: select the row by its 1-based
/// number in the current view.
fn menu_go_to_channel_number(wiring: &Wiring, frame: Frame, list: ListCtrl, status: Status) {
    let dialog =
        TextEntryDialog::builder(&frame, "Go to channel number:", "Go to Channel Number").build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(text) = dialog.get_value() else {
        return;
    };
    let Ok(number) = text.trim().parse::<usize>() else {
        status.set_label("That is not a channel number.");
        return;
    };
    if number == 0 {
        status.set_label("Channel numbers start at 1.");
        return;
    }
    let row = number - 1;
    let Some(index) = wiring.model.borrow().playlist_index(row) else {
        status.set_label("That channel number is not in the current view.");
        return;
    };
    let row_i64 = i64::try_from(row).unwrap_or(0);
    list.set_item_state(
        row_i64,
        ListItemState::Selected | ListItemState::Focused,
        ListItemState::Selected | ListItemState::Focused,
    );
    list.ensure_visible(row_i64);
    list.set_focus();
    if let Some(channel) = wiring.browser.borrow().channel(index) {
        status.set_label(&format!("Channel {}: {}", number, channel.name));
    }
}

/// Options > Custom Player...: pick the player executable and make
/// it the media player.
fn menu_custom_player(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Choose the custom player executable")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    wiring
        .persist
        .set_and_save("custom_player_path", serde_json::json!(path));
    wiring
        .persist
        .set_and_save("media_player", serde_json::json!("Custom"));
    status.set_label("Custom player set.");
}

/// Options > Export Settings...: copy the config file out.
/// Ask for a settings-backup password (Python's flow): a
/// masked entry dialog captioned "Settings Backup"; empty is
/// an error, and export confirms with a second entry.
fn backup_password(frame: Frame, confirm: bool) -> Option<String> {
    let dialog = TextEntryDialog::builder(
        &frame,
        "Enter a password for the settings backup:",
        "Settings Backup",
    )
    .password()
    .build();
    if dialog.show_modal() != ID_OK {
        return None;
    }
    let password = dialog.get_value().unwrap_or_default();
    if password.is_empty() {
        let _ = MessageDialog::builder(&frame, "A backup password is required.", "Settings Backup")
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal();
        return None;
    }
    if confirm {
        let verify =
            TextEntryDialog::builder(&frame, "Enter the password again:", "Settings Backup")
                .password()
                .build();
        if verify.show_modal() != ID_OK {
            return None;
        }
        if verify.get_value().unwrap_or_default() != password {
            let _ =
                MessageDialog::builder(&frame, "The passwords do not match.", "Settings Backup")
                    .with_style(MessageDialogStyle::OK)
                    .build()
                    .show_modal();
            return None;
        }
    }
    Some(password)
}

/// Options > Export Settings...: write the encrypted backup
/// (Python's Fernet format, interchangeable with the Python
/// client) after the two-entry password prompt.
fn menu_export_settings(wiring: &Wiring, frame: Frame, status: Status) {
    let Some(password) = backup_password(frame, true) else {
        return;
    };
    let dialog = FileDialog::builder(&frame)
        .with_message("Export Settings")
        .with_default_file("settings.aiptv")
        .with_wildcard("Accessible IPTV backup (*.aiptv)|*.aiptv|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    let config_json = {
        let config = wiring.persist.config.borrow();
        serde_json::to_vec(&serde_json::Value::Object(config.raw().clone())).unwrap_or_default()
    };
    match iptv_core::settings_backup::export_settings(&config_json, &password) {
        Ok(bytes) => match std::fs::write(&path, bytes) {
            Ok(()) => {
                let _ = MessageDialog::builder(
                    &frame,
                    "Encrypted settings backup saved.",
                    "Settings Backup",
                )
                .with_style(MessageDialogStyle::OK)
                .build()
                .show_modal();
                status.set_label("Encrypted settings backup saved.");
            }
            Err(error) => status.set_label(&format!("Could not export settings: {error}")),
        },
        Err(error) => status.set_label(&format!("Could not export settings: {error}")),
    }
}

/// Options > Import Settings...: choose a backup, enter its
/// password once, confirm the replace, then swap the config
/// file (restart applies, as Python instructs).
fn menu_import_settings(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Import Settings")
        .with_wildcard("Accessible IPTV backup (*.aiptv)|*.aiptv|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    let Some(password) = backup_password(frame, false) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        status.set_label("Could not import settings: cannot read that file.");
        return;
    };
    let decrypted = match iptv_core::settings_backup::import_settings(&bytes, &password) {
        Ok(decrypted) => decrypted,
        Err(error) => {
            status.set_label(&format!("Could not import settings: {error}"));
            return;
        }
    };
    let valid =
        serde_json::from_slice::<serde_json::Value>(&decrypted).is_ok_and(|v| v.is_object());
    if !valid {
        status.set_label("Could not import settings: that backup holds no settings object.");
        return;
    }
    let confirm = MessageDialog::builder(
        &frame,
        "Replace current settings with this backup? Restart the app after importing.",
        "Import Settings",
    )
    .with_style(MessageDialogStyle::YesNo)
    .build();
    if confirm.show_modal() != ID_YES {
        return;
    }
    match std::fs::write(&wiring.persist.path, decrypted) {
        Ok(()) => status.set_label("Settings imported. Restart the app to use them."),
        Err(error) => status.set_label(&format!("Could not import settings: {error}")),
    }
}

/// The Player menu's Play/Pause and Stop items, acting on the
/// active engine (libVLC when it drives playback, else `MediaCtrl`).
fn menu_player_transport(wiring: &Wiring, play_pause: bool) {
    let mut player = wiring.player.borrow_mut();
    if play_pause {
        if player.vlc_active {
            if let Some(engine) = &mut player.vlc {
                engine.pause();
            }
        } else if let Some(win) = &player.window {
            if win.media.get_state() == MediaState::Playing {
                win.media.pause();
            } else {
                win.media.play();
            }
        }
        return;
    }
    if player.vlc_active {
        if let Some(engine) = &mut player.vlc {
            engine.stop();
        }
        player.vlc_active = false;
    }
    if let Some(win) = &player.window {
        win.media.stop();
    }
    player.session.stop();
}

/// Subtitle Speech > Read current subtitle: announce the cue active
/// at the player's current position.
fn menu_read_current_subtitle(wiring: &Wiring, status: Status) {
    let player = wiring.player.borrow();
    if player.window.is_none() {
        status.set_label("No subtitle is showing.");
        return;
    }
    let position = position_length(&player).0;
    match player
        .subtitle_cues
        .iter()
        .find(|cue| cue.start <= position && position < cue.end)
    {
        Some(cue) => status.set_label(&cue.text),
        None => status.set_label("No subtitle is showing."),
    }
}

/// Open a folder in the platform file manager (best effort).
fn open_folder(dir: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(dir).spawn();
}

/// The minisign public key (line 2 of assets/minisign.pub, read
/// in by build.rs). Release assets are signed with the matching
/// secret, which lives only in the release pipeline and the
/// owner's vault — the same keypair `BlindRadio` uses.
const PUBLIC_KEY: &str = env!("IPTV_MINISIGN_PUBLIC_KEY");

/// ship-shape updater configuration: this repo, the portable
/// asset names the release workflow publishes, verified against
/// `PUBLIC_KEY`. Portable on every platform: the packages are
/// archives extracted over the app folder (on macOS ship-shape
/// falls back to opening the signed disk image for a manual
/// install, since the package is not a signed app bundle).
fn updater_config() -> std::sync::Arc<ship_shape::UpdaterConfig> {
    std::sync::Arc::new(
        ship_shape::UpdaterConfig::new(
            "serrebidev/Accessible-IPTV-Client-Rust",
            "accessible-iptv-client",
            "Accessible IPTV Client",
            PUBLIC_KEY,
            env!("CARGO_PKG_VERSION"),
        )
        .with_install_kind(ship_shape::InstallKind::Portable),
    )
}

/// Run a signed update check. ship-shape does the work on its
/// own thread and drives its dialogs from the frame: Manual
/// also reports "up to date" and errors; Automatic stays quiet
/// unless an update is actually available.
fn check_updates(frame: &Frame, trigger: ship_shape::ui::CheckTrigger) {
    ship_shape::ui::run_update_check(
        updater_config(),
        frame,
        ship_shape::UpdateChannel::Stable,
        trigger,
    );
}

/// Start a background update check at launch when the user enabled
/// auto-check in Options.
fn maybe_auto_check_updates(wiring: &Wiring, frame: Frame) {
    if wiring
        .persist
        .config
        .borrow()
        .bool_or("auto_check_updates", false)
    {
        check_updates(&frame, ship_shape::ui::CheckTrigger::Automatic);
    }
}

/// Build the guide window once and keep it (closing hides it).
fn ensure_guide_ui(slot: &Rc<RefCell<Option<GuideUi>>>) -> GuideUi {
    if let Some(ui) = slot.borrow().as_ref() {
        return ui.clone();
    }
    let frame = Frame::builder()
        .with_title("User Guide")
        .with_size(Size::new(760, 560))
        .build();
    frame.set_accessibility_label("User Guide");
    let panel = Panel::builder(&frame).build();
    let toc = ListBox::builder(&panel).build();
    toc.set_accessibility_label("Table of contents");
    toc.set_accessibility_description("Choose a section to read it in the guide text.");
    let body = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .build();
    body.set_accessibility_label("Guide text");
    let find_label = StaticText::builder(&panel).with_label("Find").build();
    let _ = find_label;
    let find_box = TextCtrl::builder(&panel).build();
    find_box.set_accessibility_label("Find in guide");
    let find_button = Button::builder(&panel).with_label("Fin&d").build();
    find_button.set_accessibility_label("Find");
    let status_label = StaticText::builder(&panel)
        .with_label("Guide status")
        .build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("The guide has not been opened yet.")
        .build();
    details.set_accessibility_label("Guide status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    let guide = Rc::new(RefCell::new(iptv_core::guide::load_guide(None, None)));
    let ui = GuideUi {
        frame,
        toc,
        body,
        status,
        guide,
    };
    {
        let guide = Rc::clone(&ui.guide);
        toc.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |event| {
            event.skip(true);
            let Some(selection) = toc.get_selection() else {
                return;
            };
            let guide = guide.borrow();
            if let Some(section) = guide.sections.get(selection as usize) {
                body.set_insertion_point(i64::try_from(section.start).unwrap_or(0));
                body.set_selection(
                    i64::try_from(section.start).unwrap_or(0),
                    i64::try_from(section.start).unwrap_or(0),
                );
                body.set_focus();
            }
        });
    }
    {
        let guide = Rc::clone(&ui.guide);
        find_button.on_click(move |_| {
            let query = find_box.get_value();
            if query.trim().is_empty() {
                status.set_label("Type something to find first.");
                return;
            }
            let guide = guide.borrow();
            let haystack = guide.text.to_lowercase();
            let needle = query.to_lowercase();
            match haystack.find(&needle) {
                Some(byte_index) => {
                    let char_index = haystack[..byte_index].chars().count();
                    body.set_insertion_point(i64::try_from(char_index).unwrap_or(0));
                    body.set_focus();
                    status.set_label(&format!("Found \"{query}\"."));
                }
                None => {
                    status.set_label(&format!("Cannot find \"{query}\"."));
                }
            }
        });
    }
    {
        let frame_hide = frame;
        frame.on_close(move |event| {
            frame_hide.hide();
            event.skip(false);
        });
    }
    *slot.borrow_mut() = Some(ui.clone());
    ui
}

/// Put a guide into the window: text, contents, spoken status.
fn show_guide(ui: &GuideUi, guide: Guide) {
    let titles: Vec<String> = guide
        .sections
        .iter()
        .map(|section| section.title.clone())
        .collect();
    ui.body.change_value(&guide.text);
    let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
    ui.toc.set_items(&refs);
    if !refs.is_empty() {
        ui.toc.set_selection(0, true);
    }
    ui.status.set_label(&format!(
        "User Guide ({}). {} sections. Choose a section, or use Find.",
        guide.language,
        guide.sections.len()
    ));
    *ui.guide.borrow_mut() = guide;
}

/// Move the guide text caret to a section.
fn move_caret(ui: &GuideUi, section: &iptv_core::guide::Section) {
    ui.body
        .set_insertion_point(i64::try_from(section.start).unwrap_or(0));
}

/// Context menu command ids on the Channels list (G3).
const CTX_PLAY: i32 = 6001;
const CTX_FAVORITE: i32 = 6002;
const CTX_RECORD: i32 = 6003;
const CTX_REMINDER: i32 = 6004;
const CTX_SCHEDULE: i32 = 6005;
const CTX_EPG: i32 = 6006;
const CTX_CATCHUP: i32 = 6007;

/// All guide programmes for a channel (empty without a guide match).
fn guide_programmes(wiring: &Wiring, channel: &Channel) -> Vec<Programme> {
    let in_memory = {
        let guard = wiring.epg.borrow();
        match guard.as_ref() {
            Some((guide, indexes)) => {
                let known: std::collections::HashSet<String> =
                    guide.channels.iter().map(|c| c.id.to_lowercase()).collect();
                match match_guide_channel(channel, indexes, Some(&known)) {
                    Some(id) => guide.programmes.get(&id).cloned().unwrap_or_default(),
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        }
    };
    if !in_memory.is_empty() {
        return in_memory;
    }
    store_programmes(wiring, channel)
}

/// Programmes from the `SQLite` store for a channel (tvg-id
/// candidates in match order), from an hour ago, capped at 200.
fn store_programmes(wiring: &Wiring, channel: &Channel) -> Vec<Programme> {
    let guard = wiring.epg_store.borrow();
    let Some(store) = guard.as_ref() else {
        return Vec::new();
    };
    let now = now_epoch();
    let mut candidates = iptv_core::epg::expand_tvg_id_candidates(channel.tvg_id.trim());
    candidates.push(channel.tvg_id.trim().to_string());
    for candidate in candidates {
        if let Ok(programmes) = store.programmes_between(&candidate, now - 3600, now + 14 * 86400) {
            if !programmes.is_empty() {
                return programmes.into_iter().take(200).collect();
            }
        }
    }
    Vec::new()
}

/// The programme airing now on a channel, when the guide has one.
fn current_programme(wiring: &Wiring, channel: &Channel) -> Option<Programme> {
    let now = now_epoch();
    guide_programmes(wiring, channel)
        .into_iter()
        .find(|p| p.start <= now && now < p.stop)
}

/// Add a reminder for a programme (5-minute lead from the context
/// menu; the Reminders dialog in G6 offers lead choices).
fn add_reminder(wiring: &Wiring, channel: &Channel, programme: &Programme, status: Status) {
    let now = now_epoch();
    match iptv_core::reminders::build_reminder(channel, programme, 5, now) {
        Ok(reminder) => {
            wiring.reminders.borrow_mut().push(reminder);
            let rows = wiring.reminders.borrow().clone();
            wiring.persist.save_reminders(&rows);
            status.set_label(&format!(
                "Reminder set for {} at {}.",
                programme.title,
                local_hhmm(programme.start)
            ));
        }
        Err(_) => {
            status.set_label("Reminders can only be set for programmes that have not started.");
        }
    }
}

/// Schedule a DVR recording for a programme window.
fn add_dvr_job(wiring: &Wiring, channel: &Channel, programme: &Programme, status: Status) {
    let format = wiring
        .persist
        .config
        .borrow()
        .string_or("recording_format", "provider_mkv");
    let spec = iptv_core::dvr::NewJob {
        channel,
        programme_title: &programme.title,
        programme_start: programme.start,
        programme_stop: programme.stop,
        format: &format,
        pre_padding_minutes: wiring
            .persist
            .config
            .borrow()
            .value("recording_pre_padding_minutes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        post_padding_minutes: wiring
            .persist
            .config
            .borrow()
            .value("recording_post_padding_minutes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(2),
        repeat: "none",
    };
    match iptv_core::dvr::build_job(&spec, now_seconds()) {
        Ok(job) => {
            let mut dvr = wiring.dvr.borrow_mut();
            dvr.add_job(job);
            dvr.save();
            status.set_label(&format!(
                "Recording scheduled: {} at {}.",
                programme.title,
                local_hhmm(programme.start)
            ));
        }
        Err(error) => status.set_label(&error),
    }
}

/// The Channels right-click menu (Python's channel context menu):
/// Play, Favorite, Record, Set Reminder..., Schedule Recording...,
/// View EPG..., Catch-up...
fn bind_context_menu(frame: Frame, wiring: &Wiring, controls: Controls) {
    let list = controls.list;
    let status = controls.status;
    let search = controls.search;
    let wiring = clone_wiring(wiring);
    let wiring_menu = clone_wiring(&wiring);
    list.bind_internal(EventType::CONTEXT_MENU, move |_| {
        let Some(channel) = selected_channel(&wiring.browser, &wiring.model, list) else {
            return;
        };
        let favorite_label = if wiring.browser.borrow().favorites().is_favorite(&channel) {
            "Remove from Favorites"
        } else {
            "Add to Favorites"
        };
        let record_label = if wiring
            .recordings
            .borrow()
            .is_recording(&channel_key(&channel))
        {
            "Stop Recording"
        } else {
            "Record"
        };
        let mut menu = Menu::builder()
            .append_item(CTX_PLAY, "Play", "")
            .append_item(CTX_FAVORITE, favorite_label, "")
            .append_item(CTX_RECORD, record_label, "")
            .append_item(CTX_REMINDER, "Set Reminder...", "")
            .append_item(CTX_SCHEDULE, "Schedule Recording...", "")
            .append_item(CTX_EPG, "View EPG...", "")
            .append_item(CTX_CATCHUP, "Catch-up...", "")
            .build();
        list.popup_menu(&mut menu, None);
    });
    // Popup-menu selections arrive as menu events on the frame
    // (command events propagate up from the list).
    frame.on_menu(move |event| {
        let Some(channel) = selected_channel(&wiring_menu.browser, &wiring_menu.model, list) else {
            return;
        };
        match event.get_id() {
            CTX_PLAY => activate_channel(&wiring_menu, list, status),
            CTX_FAVORITE => toggle_selected_favorite(
                &wiring_menu.browser,
                &wiring_menu.model,
                &wiring_menu.persist,
                &wiring_menu.timer,
                list,
                search,
                status,
            ),
            CTX_RECORD => {
                toggle_recording(
                    &wiring_menu.recordings,
                    &wiring_menu.buffers,
                    &wiring_menu.persist,
                    &channel,
                    status,
                );
                assert!(wiring_menu.timer.start(50, false));
            }
            CTX_REMINDER => match current_programme(&wiring_menu, &channel) {
                Some(programme) => add_reminder(&wiring_menu, &channel, &programme, status),
                None => status.set_label("No current programme to set a reminder for."),
            },
            CTX_SCHEDULE => match current_programme(&wiring_menu, &channel) {
                Some(programme) => add_dvr_job(&wiring_menu, &channel, &programme, status),
                None => status.set_label("No current programme to schedule a recording for."),
            },
            CTX_EPG => open_channel_epg(&wiring_menu, &channel, status),
            CTX_CATCHUP => play_catchup(&wiring_menu, &channel, status),
            _ => {}
        }
    });
}

/// Resolve and play the catch-up stream for the channel's current
/// programme (Python's Catch-up context action).
fn play_catchup(wiring: &Wiring, channel: &Channel, status: Status) {
    let Some(programme) = current_programme(wiring, channel) else {
        status.set_label("No current programme to catch up.");
        return;
    };
    play_catchup_for(wiring, channel, &programme, status);
}

/// Resolve and play one programme's catch-up stream.
fn play_catchup_for(wiring: &Wiring, channel: &Channel, programme: &Programme, status: Status) {
    let now = now_epoch();
    match iptv_core::catchup::resolve_show_url(channel, programme.start, programme.stop, now) {
        Ok(
            iptv_core::catchup::ShowResolution::Live(_)
            | iptv_core::catchup::ShowResolution::StalkerLive,
        ) => {
            play_channel(&wiring.player, &wiring.persist, channel, status);
        }
        Ok(iptv_core::catchup::ShowResolution::Catchup(url)) => {
            let mut shifted = channel.clone();
            shifted.url = url;
            play_channel(&wiring.player, &wiring.persist, &shifted, status);
        }
        Ok(iptv_core::catchup::ShowResolution::StalkerCatchup {
            start_text,
            duration_minutes,
        }) => {
            let Some(key) = channel.unknown_attributes.get("provider-id").cloned() else {
                status.set_label("This Stalker channel has no provider account configured.");
                return;
            };
            dispatch_resolve_catchup(
                wiring,
                PendingResolve::Play(channel.clone()),
                key,
                channel.unknown_attributes.clone(),
                start_text,
                duration_minutes,
            );
            status.set_label("Resolving catch-up link...");
        }
        Err(error) => status.set_label(&error),
    }
}

/// The Channel EPG window: the channel's upcoming programmes with
/// Set Reminder and Schedule Recording actions on the selection.
/// G6 grows this into the full EPG viewer.
fn open_channel_epg(wiring: &Wiring, channel: &Channel, main_status: Status) {
    let now = now_epoch();
    let programmes: Vec<Programme> = guide_programmes(wiring, channel)
        .into_iter()
        .filter(|p| p.stop > now - 3600)
        .take(200)
        .collect();
    if programmes.is_empty() {
        main_status.set_label("No guide information for this channel.");
        return;
    }
    let frame = Frame::builder()
        .with_title(&format!("EPG — {}", channel.name))
        .with_size(Size::new(640, 480))
        .build();
    frame.set_accessibility_label("Channel EPG");
    let panel = Panel::builder(&frame).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label("Programmes");
    let labels: Vec<String> = programmes
        .iter()
        .map(|p| format!("{}-{} {}", local_hhmm(p.start), local_hhmm(p.stop), p.title))
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    let reminder_button = Button::builder(&panel).with_label("Set &Reminder").build();
    let schedule_button = Button::builder(&panel)
        .with_label("Schedule &Recording")
        .build();
    let close_button = Button::builder(&panel).with_label("&Close").build();
    let status_label = StaticText::builder(&panel).with_label("EPG status").build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("Choose a programme, then Set Reminder or Schedule Recording.")
        .build();
    details.set_accessibility_label("EPG status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    let programmes = Rc::new(RefCell::new(programmes));
    let channel_r = channel.clone();
    let wiring_r = clone_wiring(wiring);
    let programmes_r = Rc::clone(&programmes);
    reminder_button.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        if let Some(programme) = programmes_r
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
        {
            add_reminder(&wiring_r, &channel_r, programme, status);
        }
    });
    let wiring_s = clone_wiring(wiring);
    let programmes_s = Rc::clone(&programmes);
    let channel_s = channel.clone();
    schedule_button.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        if let Some(programme) = programmes_s
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
        {
            add_dvr_job(&wiring_s, &channel_s, programme, status);
        }
    });
    let all_reminders_button = Button::builder(&panel).with_label("Remin&ders...").build();
    let all_scheduled_button = Button::builder(&panel)
        .with_label("S&cheduled Recordings...")
        .build();
    let catchup_button = Button::builder(&panel).with_label("Cat&ch-up...").build();
    let wiring_rem = clone_wiring(wiring);
    all_reminders_button.on_click(move |_| open_reminders_dialog(&wiring_rem, status));
    let wiring_sch = clone_wiring(wiring);
    all_scheduled_button.on_click(move |_| open_scheduled_dialog(&wiring_sch, status));
    let wiring_cat = clone_wiring(wiring);
    let channel_cat = channel.clone();
    catchup_button.on_click(move |_| open_catchup_dialog(&wiring_cat, &channel_cat, status));
    let frame_hide = frame;
    close_button.on_click(move |_| frame_hide.hide());
    frame.show(true);
}

fn bind_playback(
    wiring: &Wiring,
    list: ListCtrl,
    status: Status,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
) {
    bind_epg_highlight(wiring, list, episode_description, stream_url);
    let wiring = clone_wiring(wiring);
    list.on_item_activated(move |_| activate_channel(&wiring, list, status));
}

/// Play the channel behind the selected or focused Channels row
/// (Enter, and the context menu's Play). Stalker channels resolve
/// their link through the provider worker first.
fn activate_channel(wiring: &Wiring, list: ListCtrl, status: Status) {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    let Some(row) = pick_row(selected, focused) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    let Some(index) = wiring.model.borrow().playlist_index(row) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    let Some(channel) = wiring.browser.borrow().channel(index) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    if channel.url.is_empty()
        && channel
            .unknown_attributes
            .get("provider-type")
            .map(String::as_str)
            == Some("stalker")
    {
        let Some(key) = channel.unknown_attributes.get("provider-id").cloned() else {
            status.set_label("This Stalker channel has no provider account configured.");
            return;
        };
        dispatch_resolve(
            wiring,
            PendingResolve::Play(channel.clone()),
            key,
            channel.unknown_attributes.clone(),
        );
        status.set_label("Resolving stream link...");
        return;
    }
    if let Some(current) = wiring.player.borrow().current_channel.clone() {
        if current.url != channel.url {
            *wiring.previous_channel.borrow_mut() = Some(current);
        }
    }
    play_channel(&wiring.player, &wiring.persist, &channel, status);
}

/// Moving through Channels fills the highlighted channel's info
/// panels (stream URL, episode description) silently — Python
/// never speaks the guide on selection; the channel row itself
/// carries the now/next after the channel name.
fn bind_epg_highlight(
    wiring: &Wiring,
    list: ListCtrl,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
) {
    let browser = Rc::clone(&wiring.browser);
    let model = Rc::clone(&wiring.model);
    let epg = Rc::clone(&wiring.epg);
    list.on_item_selected(move |event| {
        let Some(row) = usize::try_from(event.get_item_index()).ok() else {
            return;
        };
        let Some(index) = model.borrow().playlist_index(row) else {
            return;
        };
        let Some(channel) = browser.borrow().channel(index) else {
            return;
        };
        stream_url.set_value(&channel.url);
        let current = {
            let guard = epg.borrow();
            guard.as_ref().and_then(|(guide, indexes)| {
                let known: std::collections::HashSet<String> =
                    guide.channels.iter().map(|c| c.id.to_lowercase()).collect();
                match_guide_channel(&channel, indexes, Some(&known))
                    .and_then(|id| guide.now_next(&id, now_epoch()).0)
            })
        };
        let text = match &current {
            Some(p) if !p.description.is_empty() => p.description.clone(),
            Some(p) => p.title.clone(),
            None => "No description available for this programme.".to_owned(),
        };
        episode_description.set_value(&text);
    });
}

/// Current Unix time in whole seconds (EPG/DVR math).
fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

#[allow(clippy::cast_precision_loss)] // epoch seconds fit comfortably for schedule math
fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs() as f64)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn play_channel(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    channel: &Channel,
    status: Status,
) {
    {
        let mut state = player.borrow_mut();
        if state.history.first().map(|c| &c.url) != Some(&channel.url) {
            state.history.insert(0, channel.clone());
            state.history.truncate(50);
        }
    }
    let key = channel_key(channel);
    let entries = persist.resume_entries();
    let probe = PlaybackRequest::for_channel(channel, None);
    let resume = if probe.live {
        None
    } else {
        resume_position(&entries, &key)
    };
    let request = PlaybackRequest::for_channel(channel, resume);
    let (media_player, custom_path) = {
        let config = persist.config.borrow();
        (
            config.string_or("media_player", "Built-in Player"),
            config.string_or("custom_player_path", ""),
        )
    };
    match player_choice(&media_player, &custom_path) {
        PlayerChoice::BuiltIn => play_builtin(player, persist, channel, request, status),
        PlayerChoice::Named(name) => {
            let candidates = candidate_executables(&name, cfg!(windows), cfg!(target_os = "macos"));
            let exe = if cfg!(windows) || cfg!(target_os = "macos") {
                candidates
                    .iter()
                    .find(|path| std::path::Path::new(path).exists())
                    .cloned()
            } else {
                candidates.into_iter().next()
            };
            match exe {
                Some(exe) => launch_external(player, &name, &exe, &request, status),
                None => status.set_label(&format!("Could not locate {name} executable.")),
            }
        }
        PlayerChoice::Custom(path) => {
            if path.is_empty() {
                status.set_label("No custom player path is configured.");
            } else {
                let name = detect_from_path(&path);
                launch_external(player, &name, &path, &request, status);
            }
        }
    }
}

fn launch_external(
    player: &Rc<RefCell<PlayerState>>,
    name: &str,
    exe: &str,
    request: &PlaybackRequest,
    status: Status,
) {
    let ipc = std::env::temp_dir()
        .join("accessible-iptv-mpv.sock")
        .to_string_lossy()
        .into_owned();
    let accepted = player
        .borrow_mut()
        .gate
        .accept(name, exe, &request.url, now_ms());
    if accepted.is_none() {
        return; // Debounced duplicate, as in Python: accepted silently.
    }
    let argv = external_argv(name, exe, &request.url, &ipc);
    match Command::new(&argv[0]).args(&argv[1..]).spawn() {
        Ok(_) => status.set_label(&format!("Playing {} in {name}.", request.title)),
        Err(error) => status.set_label(&format!("Could not start {name}: {error}")),
    }
}

fn play_builtin(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    channel: &Channel,
    request: PlaybackRequest,
    status: Status,
) {
    if !request.buffer_bypass && buffer_intercept(player, persist, channel, &request, status) {
        return;
    }
    ensure_player_window(player, persist, status);
    let mut state = player.borrow_mut();
    state.current_channel = Some(channel.clone());
    state.pending_resume_ms = request.resume_ms;
    state.pending_reconnect = None;
    state.subtitle_cues.clear();
    state.last_spoken_cue = None;
    let action = state.session.play(request, now_ms());
    let Some(win) = state.window.clone() else {
        return;
    };
    if let EngineAction::Load(load) = action {
        state.vlc_active = false;
        if state.vlc.is_none() {
            match iptv_vlc::VlcEngine::new() {
                Ok(engine) => {
                    state.vlc = Some(engine);
                    state.vlc_error = None;
                }
                Err(error) => state.vlc_error = Some(error),
            }
            if let Some(engine) = &mut state.vlc {
                engine.set_window_handle(win.media.get_handle() as usize);
            }
        }
        let vlc_started = match &mut state.vlc {
            Some(engine) => engine.play_url(&load.url, &load.headers).is_ok(),
            None => false,
        };
        let via_vlc = vlc_started;
        state.engine = if via_vlc {
            ActiveEngine::Vlc
        } else {
            ActiveEngine::Fallback
        };
        if via_vlc {
            state.vlc_active = true;
            state.vlc_tracks_ready = false;
            drop(state);
            apply_saved_audio_device(player, persist);
            state = player.borrow_mut();
        }
        if !via_vlc {
            win.media.load_uri(&load.url);
            win.media.play();
        }
    }
    win.frame
        .set_title(&format!("{} — Player", state.session.announcement()));
    if persist
        .config
        .borrow()
        .bool_or("show_player_on_enter", true)
    {
        win.frame.show(true);
    }
    player_status(status, persist, &state.session.announcement(), 3);
    if !state.vlc_active {
        if let Some(error) = &state.vlc_error {
            status.set_label(&format!(
                "libVLC is not available ({error}); basic player in use, many streams will not play."
            ));
        }
    }
}

/// File a session action away: a reconnect is scheduled under
/// the session's current generation; a stop settles the session.
fn store_session_action(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    action: EngineAction,
) {
    match action {
        EngineAction::Reconnect(request, delay) => {
            let mut state = player.borrow_mut();
            state.pending_reconnect = Some((state.session.generation(), now_ms() + delay, request));
        }
        EngineAction::Stop => settle_session(player, persist, status),
        _ => {}
    }
}

/// Settle a session that asked to stop: silence the engines, and
/// when the stream was lost say so at top priority with the long
/// report in the status details.
fn settle_session(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let lost_report = {
        let mut state = player.borrow_mut();
        state.pending_reconnect = None;
        if state.vlc_active {
            if let Some(engine) = &mut state.vlc {
                engine.stop();
            }
            state.vlc_active = false;
        }
        if let Some(win) = &state.window {
            win.media.stop();
        }
        state.session.lost_report()
    };
    if let Some(report) = lost_report {
        player_status(status, persist, "Stream lost", 1);
        // Python shows the long form in its Stream Lost dialog.
        let frame = player.borrow().window.as_ref().map(|win| win.frame);
        if let Some(frame) = frame {
            let _ = MessageDialog::builder(&frame, &report, "Stream Lost")
                .with_style(MessageDialogStyle::OK)
                .build()
                .show_modal();
        }
    }
}

/// (Re)load a request into the engine that owns the current
/// session. False when no engine accepted the load.
fn engine_load(player: &Rc<RefCell<PlayerState>>, request: &PlaybackRequest) -> bool {
    let mut state = player.borrow_mut();
    if state.engine == ActiveEngine::Vlc {
        match &mut state.vlc {
            Some(engine) => {
                let ok = engine.play_url(&request.url, &request.headers).is_ok();
                if ok {
                    state.vlc_active = true;
                    state.vlc_tracks_ready = false;
                }
                ok
            }
            None => false,
        }
    } else {
        match &state.window {
            Some(win) => {
                win.media.load_uri(&request.url);
                win.media.play();
                true
            }
            None => false,
        }
    }
}

/// One recovery drive from the player tick: fire a due reconnect
/// (generation-checked, so a stopped or superseded retry can
/// never reactivate an old attempt), then let the session's own
/// clock run (startup deadline, no-media timeout, prolonged
/// buffering).
fn drive_session(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let due = {
        let mut state = player.borrow_mut();
        match state.pending_reconnect.take() {
            Some((generation, due_ms, request)) => {
                if generation != state.session.generation() {
                    None // superseded or stopped: drop it
                } else if now_ms() >= due_ms {
                    Some(request)
                } else {
                    state.pending_reconnect = Some((generation, due_ms, request));
                    None
                }
            }
            None => None,
        }
    };
    if let Some(request) = due {
        if !engine_load(player, &request) {
            let action = player.borrow_mut().session.on_event(
                EngineEvent::Error("could not restart playback".to_string()),
                now_ms(),
            );
            store_session_action(player, persist, status, action);
        }
    }
    let action = player.borrow_mut().session.on_tick(now_ms());
    store_session_action(player, persist, status, action);
}

/// The built-in player window's widgets.
#[derive(Clone)]
struct PlayerWindow {
    frame: Frame,
    media: MediaCtrl,
    seek: Slider,
    volume: Slider,
    time_label: StaticText,
    subtitle_label: StaticText,
    tracks: Choice,
}

/// Handles the player window needs from the main wiring (record and
/// cast), installed when the browser wiring is built.
#[derive(Clone)]
struct PlayerShared {
    buffers: Rc<RefCell<buffer::BufferRegistry>>,
    recordings: Rc<RefCell<RecordingEngine>>,
    cast_ui: Rc<RefCell<Option<CastUi>>>,
    proxy: Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    cast_worker: Rc<CastWorker>,
    cast_pending: Rc<std::cell::Cell<bool>>,
    timer: Rc<Timer<Frame>>,
}

/// Build the built-in player window once: video surface, seek and
/// volume sliders, time and subtitle labels, the audio track
/// choice, and the action buttons.
fn ensure_player_window(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    if player.borrow().window.is_some() {
        return;
    }
    let frame = Frame::builder()
        .with_title("Player")
        .with_size(Size::new(560, 460))
        .build();
    frame.set_accessibility_label("Player");
    let panel = Panel::builder(&frame).build();
    let media = MediaCtrl::builder(&panel).build();
    media.set_accessibility_label("Video");
    let seek = Slider::builder(&panel).build();
    seek.set_range(0, 1000);
    seek.set_accessibility_label("Seek");
    let time_label = StaticText::builder(&panel).with_label("Live").build();
    time_label.set_accessibility_label("Playback position");
    let subtitle_label = StaticText::builder(&panel).with_label("").build();
    subtitle_label.set_accessibility_label("Current subtitle");
    let volume_caption = StaticText::builder(&panel).with_label("&Volume").build();
    let _ = volume_caption;
    let volume = Slider::builder(&panel).build();
    volume.set_range(0, 100);
    volume.set_value(100);
    volume.set_accessibility_label("Volume");
    let tracks_caption = StaticText::builder(&panel)
        .with_label("Choose &Audio Track")
        .build();
    let _ = tracks_caption;
    let tracks = Choice::builder(&panel).build();
    tracks.append("Default");
    tracks.set_selection(0);
    tracks.set_accessibility_label("Choose Audio Track");
    let pause = Button::builder(&panel)
        .with_label("&Pause or resume")
        .build();
    pause.set_accessibility_label("Pause or resume");
    let stop = Button::builder(&panel).with_label("&Stop").build();
    stop.set_accessibility_label("Stop");
    let record = Button::builder(&panel).with_label("&Record").build();
    record.set_accessibility_label("Record");
    let cast = Button::builder(&panel).with_label("&Cast").build();
    cast.set_accessibility_label("Cast");
    let fullscreen = Button::builder(&panel).with_label("&Full Screen").build();
    fullscreen.set_accessibility_label("Full screen");
    let subs = Button::builder(&panel)
        .with_label("Load &subtitles…")
        .build();
    subs.set_accessibility_label("Load subtitles");
    subs.set_accessibility_description(
        "Load an SRT, WebVTT or ASS subtitle file for this playback. When Speak subtitles is on in settings, cues are announced as they play.",
    );
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&seek, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&time_label, 0, SizerFlag::All, 4);
    sizer.add(&subtitle_label, 0, SizerFlag::All, 4);
    sizer.add(&volume_caption, 0, SizerFlag::All, 4);
    sizer.add(&volume, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&tracks_caption, 0, SizerFlag::All, 4);
    sizer.add(&tracks, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&pause, 0, SizerFlag::All, 4);
    buttons.add(&stop, 0, SizerFlag::All, 4);
    buttons.add(&record, 0, SizerFlag::All, 4);
    buttons.add(&cast, 0, SizerFlag::All, 4);
    buttons.add(&fullscreen, 0, SizerFlag::All, 4);
    buttons.add(&subs, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    let win = PlayerWindow {
        frame,
        media,
        seek,
        volume,
        time_label,
        subtitle_label,
        tracks,
    };
    wire_player_events(
        player,
        persist,
        status,
        &win,
        &PlayerButtons {
            pause,
            stop,
            record,
            cast,
            fullscreen,
            subs,
        },
    );
    bind_player_controls(player, persist, status, &win);
    build_playback_menu(player, persist, status, &win);
    player.borrow_mut().window = Some(win);
}

/// Player Playback-menu ids.
const PM_PLAY_PAUSE: i32 = 5301;
const PM_STOP: i32 = 5302;
const PM_SLEEP: i32 = 5303;
const PM_RECORD: i32 = 5304;
const PM_NO_TRACKS: i32 = 5305;
const PM_LOAD_SUBS: i32 = 5320;
const PM_SPEAK_SUBS: i32 = 5321;
const PM_READ_SUB: i32 = 5322;
const PM_PREV_SUB: i32 = 5323;
const PM_NEXT_SUB: i32 = 5324;
const PM_AUDIO_DEVICE: i32 = 5325;
const PM_LIVE_BUFFER: i32 = 5326;
const PM_REWIND: i32 = 5327;
const PM_RETURN_LIVE: i32 = 5328;
const PM_CAST: i32 = 5330;
const PM_FULLSCREEN: i32 = 5331;
const PM_WHATS_PLAYING: i32 = 5332;
const PM_HIDE: i32 = 5333;
const PM_EXIT: i32 = 5334;
const PM_TRACK_BASE: i32 = 5340;

/// The Playback menu's transport block: Play/Pause (label
/// carries the effective shortcut), Stop, Sleep Timer, the
/// live-buffer items and Record.
fn playback_transport_items(menu: &Menu, keys: &std::collections::BTreeMap<String, String>) {
    menu_add(
        menu,
        PM_PLAY_PAUSE,
        &menu_label(keys, "&Play/Pause", "play_pause"),
        "",
        ItemKind::Normal,
    );
    menu_add(menu, PM_STOP, "&Stop", "", ItemKind::Normal);
    menu_add(menu, PM_SLEEP, "Sleep &Timer...", "", ItemKind::Normal);
    menu_add(
        menu,
        PM_LIVE_BUFFER,
        "Local Live &Buffer...",
        "",
        ItemKind::Normal,
    );
    menu_add(menu, PM_REWIND, "Re&wind Live TV...", "", ItemKind::Normal);
    menu_add(
        menu,
        PM_RETURN_LIVE,
        "Return to &Live",
        "",
        ItemKind::Normal,
    );
    menu_add(menu, PM_RECORD, "&Record", "", ItemKind::Normal);
}

/// Build the player window's Playback menu (Python's order and
/// items). Dynamic pieces: the Audio Track submenu fills when
/// libVLC publishes tracks (`apply_vlc_tracks`), and the Speak
/// Subtitles check mirrors the setting.
fn build_playback_menu(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    win: &PlayerWindow,
) {
    let config = serde_json::Value::Object(persist.config.borrow().raw().clone());
    let keys = iptv_core::shortcuts::effective(&config, "player")
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let menu = Menu::builder().build();
    playback_transport_items(&menu, &keys);
    menu.append_separator();
    let audio_menu = Menu::builder().build();
    menu_add(
        &audio_menu,
        PM_NO_TRACKS,
        "No audio tracks available",
        "",
        ItemKind::Normal,
    );
    audio_menu.enable_item(PM_NO_TRACKS, false);
    menu.append_submenu(audio_menu, "Audio Trac&k", "");
    menu_add(
        &menu,
        PM_LOAD_SUBS,
        "Load Subtitle &File...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_SPEAK_SUBS,
        "Spea&k Subtitles",
        "",
        ItemKind::Check,
    );
    let speak = persist.config.borrow().bool_or("speak_subtitles", false);
    menu.check_item(PM_SPEAK_SUBS, speak);
    menu_add(
        &menu,
        PM_READ_SUB,
        "Read &Current Subtitle",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_PREV_SUB,
        "&Previous Subtitle",
        "",
        ItemKind::Normal,
    );
    menu_add(&menu, PM_NEXT_SUB, "&Next Subtitle", "", ItemKind::Normal);
    // Python shows a hard-coded D accelerator on this item.
    menu_add(
        &menu,
        PM_AUDIO_DEVICE,
        "Audio Output &Device...\tD",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(&menu, PM_CAST, "&Cast...", "", ItemKind::Normal);
    menu_add(
        &menu,
        PM_FULLSCREEN,
        "Toggle &Full Screen",
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_WHATS_PLAYING,
        "What Is &Playing",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(&menu, PM_HIDE, "&Hide Window", "", ItemKind::Normal);
    menu_add(&menu, PM_EXIT, "E&xit Player", "", ItemKind::Normal);
    let bar = MenuBar::builder().append(menu, "&Playback").build();
    win.frame.set_menu_bar(bar);
    let menu_player = Rc::clone(player);
    let menu_persist = persist.clone();
    win.frame.on_menu(move |event| {
        dispatch_player_menu(&menu_player, &menu_persist, status, event.get_id());
    });
}

/// One Playback-menu selection. The menu mirrors the buttons and
/// the player shortcuts; every arm reuses their helpers.
fn dispatch_player_menu(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    id: i32,
) {
    if (PM_TRACK_BASE..PM_TRACK_BASE + 40).contains(&id) {
        let index = u32::try_from(id - PM_TRACK_BASE).unwrap_or(0);
        if let Some(win) = player.borrow().window.clone() {
            win.tracks.set_selection(index);
        }
        choose_audio_track(player, persist, index, status);
        return;
    }
    let win = player.borrow().window.clone();
    let Some(win) = win else {
        return;
    };
    match id {
        PM_PLAY_PAUSE => toggle_pause_state(player, persist, status),
        PM_STOP | PM_EXIT => player_stop(player, persist, status),
        PM_SLEEP => prompt_sleep_timer(player, win.frame, persist, status),
        PM_LIVE_BUFFER => prompt_live_buffer_config(player, win.frame, persist, status),
        PM_REWIND => prompt_rewind(player, win.frame, persist, status),
        PM_RETURN_LIVE => return_to_live(player, win.frame, persist, status),
        PM_RECORD => {
            let state = player.borrow();
            if let (Some(shared), Some(channel)) = (&state.shared, &state.current_channel) {
                toggle_recording(
                    &shared.recordings,
                    &shared.buffers,
                    persist,
                    channel,
                    status,
                );
                assert!(shared.timer.start(50, false));
            }
        }
        PM_AUDIO_DEVICE => prompt_audio_device(player, win.frame, persist, status),
        PM_LOAD_SUBS => prompt_subtitle_file(player, win.frame, persist, status),
        PM_SPEAK_SUBS => {
            let next = !persist.config.borrow().bool_or("speak_subtitles", false);
            persist.set_and_save("speak_subtitles", serde_json::json!(next));
            if let Some(menu) = playback_menu(player) {
                menu.check_item(PM_SPEAK_SUBS, next);
            }
            player_status(
                status,
                persist,
                if next {
                    "Subtitle speech on."
                } else {
                    "Subtitle speech off."
                },
                2,
            );
        }
        PM_READ_SUB => announce_cue(player, "read_subtitle", status),
        PM_PREV_SUB => announce_cue(player, "previous_subtitle", status),
        PM_NEXT_SUB => announce_cue(player, "next_subtitle", status),
        PM_CAST => {
            let state = player.borrow();
            if let Some(shared) = &state.shared {
                show_cast_for(
                    &shared.cast_ui,
                    &shared.cast_worker,
                    &shared.cast_pending,
                    &shared.timer,
                    &shared.proxy,
                    state.current_channel.as_ref(),
                );
            }
        }
        PM_FULLSCREEN => win.frame.show_full_screen(!win.frame.is_full_screen()),
        PM_WHATS_PLAYING => {
            let state = player.borrow();
            match &state.current_channel {
                Some(channel) => status.set_label(&format!("Now playing: {}", channel.name)),
                None => status.set_label("Nothing is playing."),
            }
        }
        PM_HIDE => win.frame.hide(),
        _ => {}
    }
}

/// The player frame's Playback menu, as a borrowed handle (wx
/// menus are owning wrappers; the frame's menu bar owns the
/// real one after `set_menu_bar`).
fn playback_menu(player: &Rc<RefCell<PlayerState>>) -> Option<Menu> {
    let state = player.borrow();
    let win = state.window.as_ref()?;
    win.frame.get_menu_bar()?.get_menu(0)
}

/// The Audio Track submenu (Playback menu position 5, after
/// Play/Pause, Stop, Sleep Timer, Record and a separator).
fn audio_track_menu(player: &Rc<RefCell<PlayerState>>) -> Option<Menu> {
    playback_menu(player)?
        .find_item_by_position(8)?
        .get_sub_menu()
}

/// Apply the persisted audio output device to the libVLC
/// engine, when one is set (Python applies it at every play
/// start; empty = system default = no call).
fn apply_saved_audio_device(player: &Rc<RefCell<PlayerState>>, persist: &Persist) {
    let device = persist.config.borrow().string_or("audio_output_device", "");
    if device.is_empty() {
        return;
    }
    let mut state = player.borrow_mut();
    if let Some(engine) = &mut state.vlc {
        engine.set_audio_output_device(&device);
    }
}

/// The Audio Output Device dialog (Python): "System default"
/// first, then libVLC's devices (deduped, description or id);
/// OK persists a changed choice and applies it live. With no
/// devices at all, Python shows an information box instead.
fn prompt_audio_device(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let devices = {
        let state = player.borrow();
        state
            .vlc
            .as_ref()
            .map(iptv_vlc::VlcEngine::audio_output_devices)
            .unwrap_or_default()
    };
    if devices.is_empty() {
        let _ = MessageDialog::builder(
            &frame,
            "No audio output devices were found.",
            "Audio Output Device",
        )
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal();
        return;
    }
    let mut entries: Vec<(String, String)> = vec![(String::new(), "System default".to_string())];
    for (id, label) in devices {
        if !entries.iter().any(|(existing, _)| *existing == id) {
            entries.push((id, label));
        }
    }
    let current = persist.config.borrow().string_or("audio_output_device", "");
    let (dialog_frame, panel, list, dstatus) =
        list_dialog("Audio Output Device", "Audio output devices");
    let labels: Vec<String> = entries.iter().map(|(_, label)| label.clone()).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let selected = entries
        .iter()
        .position(|(id, _)| *id == current)
        .unwrap_or(0);
    list.set_selection(u32::try_from(selected).unwrap_or(0), true);
    dstatus.set_silent("Select the audio output device for the built-in player.");
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let entries = Rc::new(entries);
    let choose = {
        let player = Rc::clone(player);
        let persist = persist.clone();
        let entries = Rc::clone(&entries);
        move |list: ListBox, dialog_frame: Frame| {
            let Some(index) = list.get_selection() else {
                return;
            };
            let Some((device_id, _)) = entries.get(usize::try_from(index).unwrap_or(usize::MAX))
            else {
                return;
            };
            let current = persist.config.borrow().string_or("audio_output_device", "");
            if *device_id != current {
                persist.set_and_save("audio_output_device", serde_json::json!(device_id));
                apply_saved_audio_device(&player, &persist);
                status.set_label("Audio output device saved.");
            }
            dialog_frame.hide();
        }
    };
    let choose_ok = choose.clone();
    ok.on_click(move |_| choose_ok(list, dialog_frame));
    list.bind_internal(EventType::COMMAND_LISTBOX_DOUBLECLICKED, move |_| {
        choose(list, dialog_frame);
    });
    cancel.on_click(move |_| dialog_frame.hide());
    dialog_frame.show(true);
}

/// Select an audio track by Choice index (the Choice and the
/// Audio Track menu share this).
fn choose_audio_track(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    index: u32,
    status: Status,
) {
    let mut state = player.borrow_mut();
    let label = state
        .window
        .as_ref()
        .and_then(|win| win.tracks.get_string_selection())
        .unwrap_or_default();
    state.desired_track = Some(label.clone());
    if state.vlc_active {
        if let Some(id) = state.vlc_track_ids.get(index as usize).copied() {
            if let Some(engine) = &mut state.vlc {
                engine.set_audio_track(id);
            }
        }
    }
    player_status(
        status,
        persist,
        &format!("Audio track preference: {label}"),
        2,
    );
}

/// Python's Sleep Timer prompts: minutes (0-1440, 0 cancels),
/// then the fade-out seconds (0-min(300, minutes*60), default
/// from the `sleep_fade_seconds` setting, 5).
fn prompt_sleep_timer(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let current = {
        let state = player.borrow();
        state
            .sleep_deadline
            .map(|deadline| ((deadline - now_epoch()).max(0) + 59) / 60)
    };
    let dialog = TextEntryDialog::builder(
        &frame,
        "Stop built-in playback after 1-1440 minutes; enter 0 to cancel the timer.",
        "Sleep Timer",
    )
    .with_default_value(&current.map(|m| m.to_string()).unwrap_or_default())
    .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(minutes) = text.trim().parse::<i64>() else {
        status.set_label("Enter a whole number of minutes between 0 and 1440.");
        return;
    };
    if !(0..=1440).contains(&minutes) {
        status.set_label("Enter a whole number of minutes between 0 and 1440.");
        return;
    }
    if minutes == 0 {
        cancel_sleep_timer(player);
        status.set_label("Sleep timer cancelled.");
        return;
    }
    let saved_fade = persist
        .config
        .borrow()
        .value("sleep_fade_seconds")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(5);
    let fade_cap = (minutes * 60).min(300);
    let fade_dialog = TextEntryDialog::builder(
        &frame,
        "Fade-out duration before stopping (seconds); 0 disables fading.",
        "Sleep Timer",
    )
    .with_default_value(&saved_fade.clamp(0, fade_cap).to_string())
    .build();
    if fade_dialog.show_modal() != ID_OK {
        return;
    }
    let fade_text = fade_dialog.get_value().unwrap_or_default();
    let Ok(fade) = fade_text.trim().parse::<i64>() else {
        status.set_label("Enter a whole number of fade-out seconds.");
        return;
    };
    if !(0..=fade_cap).contains(&fade) {
        status.set_label(&format!("Enter fade-out seconds between 0 and {fade_cap}."));
        return;
    }
    persist.set_and_save("sleep_fade_seconds", serde_json::json!(fade));
    let mut state = player.borrow_mut();
    state.sleep_deadline = Some(now_epoch() + minutes * 60);
    state.sleep_fade_secs = fade;
    state.sleep_fading = false;
    status.set_label(&format!(
        "Sleep timer set: playback stops in {minutes} minutes."
    ));
}

/// Clear the sleep timer, restoring the pre-fade volume if a
/// fade was in progress.
fn cancel_sleep_timer(player: &Rc<RefCell<PlayerState>>) {
    let restore = {
        let mut state = player.borrow_mut();
        let restore = state.sleep_fading.then_some(state.sleep_start_volume);
        state.sleep_deadline = None;
        state.sleep_fading = false;
        restore
    };
    if let Some(volume) = restore {
        player_set_volume(player, volume);
    }
}

/// The 1-second sleep-timer driver (Python's countdown): the
/// fade announcement fires once when the fade window opens, the
/// volume ramps linearly to zero, and the deadline stops
/// playback (restoring the pre-fade volume for the next play).
fn drive_sleep_timer(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let now = now_epoch();
    let (deadline, fade, fading) = {
        let state = player.borrow();
        (
            state.sleep_deadline,
            state.sleep_fade_secs,
            state.sleep_fading,
        )
    };
    let Some(deadline) = deadline else {
        return;
    };
    if now >= deadline {
        let start_volume = player.borrow().sleep_start_volume;
        {
            let mut state = player.borrow_mut();
            state.sleep_deadline = None;
            state.sleep_fading = false;
        }
        player_stop(player, persist, status);
        player_set_volume(player, start_volume);
        return;
    }
    if fade > 0 && now >= deadline - fade {
        if !fading {
            let start_volume = player
                .borrow()
                .window
                .as_ref()
                .map_or(100, |win| win.volume.get_value());
            let mut state = player.borrow_mut();
            state.sleep_fading = true;
            state.sleep_start_volume = start_volume;
            player_status(
                status,
                persist,
                "Playback volume is fading before the sleep timer stops it.",
                2,
            );
        }
        let start = player.borrow().sleep_start_volume;
        let remaining = deadline - now;
        let level = (i64::from(start) * remaining / fade).clamp(0, i64::from(start));
        player_set_volume(player, i32::try_from(level).unwrap_or(0));
    }
}

/// The player window's buttons.
struct PlayerButtons {
    pause: Button,
    stop: Button,
    record: Button,
    cast: Button,
    fullscreen: Button,
    subs: Button,
}

/// One player-window tick (1 s): buffer delivery and poll,
/// resume checkpointing, subtitle speech, engine state,
/// the sleep timer and the window chrome.
fn player_tick(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    deliver_buffer_pending(player, persist, status);
    poll_buffer(player, persist, status);
    checkpoint_resume(player, persist);
    speak_subtitle_cue(player, persist, status);
    poll_vlc_state(player, persist, status);
    drive_session(player, persist, status);
    drive_sleep_timer(player, persist, status);
    update_player_chrome(player);
}

/// Timer, buttons, media events, keys and close behavior for the
/// player window.
fn wire_player_events(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    win: &PlayerWindow,
    buttons: &PlayerButtons,
) {
    let frame = win.frame;
    let media = win.media;
    let timer = Rc::new(Timer::new(&frame));
    let tick_player = Rc::clone(player);
    let tick_persist = persist.clone();
    timer.on_tick(move |_| {
        player_tick(&tick_player, &tick_persist, status);
    });
    let _ = timer.start(1000, false);

    bind_subtitle_load(buttons.subs, player, frame, persist, status);
    let pause_player = Rc::clone(player);
    let pause_persist = persist.clone();
    buttons
        .pause
        .on_click(move |_| toggle_pause_state(&pause_player, &pause_persist, status));
    let stop_player = Rc::clone(player);
    let stop_persist = persist.clone();
    buttons.stop.on_click(move |_| {
        player_stop(&stop_player, &stop_persist, status);
    });
    let record_player = Rc::clone(player);
    let record_persist = persist.clone();
    buttons.record.on_click(move |_| {
        let state = record_player.borrow();
        let (Some(shared), Some(channel)) = (&state.shared, &state.current_channel) else {
            return;
        };
        toggle_recording(
            &shared.recordings,
            &shared.buffers,
            &record_persist,
            channel,
            status,
        );
        assert!(shared.timer.start(50, false));
    });
    let cast_player = Rc::clone(player);
    buttons.cast.on_click(move |_| {
        let state = cast_player.borrow();
        let Some(shared) = &state.shared else {
            return;
        };
        show_cast_for(
            &shared.cast_ui,
            &shared.cast_worker,
            &shared.cast_pending,
            &shared.timer,
            &shared.proxy,
            state.current_channel.as_ref(),
        );
    });
    buttons.fullscreen.on_click(move |_| {
        frame.show_full_screen(!frame.is_full_screen());
    });
    let loaded_player = Rc::clone(player);
    let loaded_persist = persist.clone();
    media.on_loaded(move |_| {
        let mut state = loaded_player.borrow_mut();
        state.session.on_event(EngineEvent::Started, now_ms());
        let resume_ms = state.pending_resume_ms.take();
        if let (Some(win), Some(ms)) = (state.window.clone(), resume_ms) {
            win.media.seek(ms, SeekMode::FromStart);
        }
        player_status(status, &loaded_persist, &state.session.announcement(), 3);
    });
    let stopped_player = Rc::clone(player);
    let stopped_persist = persist.clone();
    media.on_stop(move |_| {
        let action = {
            let mut state = stopped_player.borrow_mut();
            let near_end = state.window.as_ref().is_some_and(|win| {
                win.media.length() > 0 && win.media.tell() >= win.media.length() - 1500
            });
            let event = if near_end {
                EngineEvent::Ended
            } else {
                EngineEvent::Stalled
            };
            state.session.on_event(event, now_ms())
        };
        store_session_action(&stopped_player, &stopped_persist, status, action);
        let state = stopped_player.borrow();
        player_status(status, &stopped_persist, &state.session.announcement(), 3);
    });
    let close_player = Rc::clone(player);
    let close_persist = persist.clone();
    frame.on_close(move |event| {
        checkpoint_resume(&close_player, &close_persist);
        let mut state = close_player.borrow_mut();
        state.session.stop();
        if let Some(win) = &state.window {
            win.media.stop();
            win.frame.show(false);
        }
        event.skip(false);
    });
}

/// Seek/volume slider, track choice and player shortcut bindings.
fn bind_player_controls(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    win: &PlayerWindow,
) {
    let seek_handle = win.seek;
    let seek_player = Rc::clone(player);
    seek_handle.bind_internal(EventType::SLIDER, move |_| {
        let length = position_length(&seek_player.borrow()).1;
        if length > 0 {
            let target = i64::from(seek_handle.get_value()) * length / 1000;
            player_seek_to(&seek_player, target);
        }
    });
    let volume_handle = win.volume;
    let volume_player = Rc::clone(player);
    volume_handle.bind_internal(EventType::SLIDER, move |_| {
        player_set_volume(&volume_player, volume_handle.get_value());
    });
    let tracks_handle = win.tracks;
    let tracks_player = Rc::clone(player);
    let tracks_persist = persist.clone();
    tracks_handle.bind_internal(EventType::COMMAND_CHOICE_SELECTED, move |_| {
        if let Some(index) = tracks_handle.get_selection() {
            choose_audio_track(&tracks_player, &tracks_persist, index, status);
        }
    });
    let keys_player = Rc::clone(player);
    let keys_persist = persist.clone();
    win.frame.bind_internal(EventType::KEY_DOWN, move |event| {
        let handled = dispatch_player_key(&keys_player, &keys_persist, status, event);
        event.skip(!handled);
    });
}

/// Pause or resume through the session machine (the Pause button
/// and the `play_pause` shortcut share this).
fn toggle_pause_state(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let mut state = player.borrow_mut();
    state.session.toggle_pause();
    let session_state = state.session.state().clone();
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            match session_state {
                SessionState::Paused | SessionState::Playing => engine.pause(),
                _ => {}
            }
        }
    } else if let Some(win) = &state.window {
        match session_state {
            SessionState::Paused => {
                win.media.pause();
            }
            SessionState::Playing => {
                win.media.play();
            }
            _ => {}
        }
    }
    player_status(status, persist, &state.session.announcement(), 3);
    drop(state);
    buffer_resume_check(player, persist, status);
}

/// The live-buffer registry shared with the player, if the
/// shared wiring has been built.
fn buffers_of(player: &Rc<RefCell<PlayerState>>) -> Option<Rc<RefCell<buffer::BufferRegistry>>> {
    player
        .borrow()
        .shared
        .as_ref()
        .map(|shared| Rc::clone(&shared.buffers))
}

/// Stop the engines without hiding the window or announcing
/// (a buffer transition is about to replace the stream).
fn stop_engines_keep_window(player: &Rc<RefCell<PlayerState>>) {
    let mut state = player.borrow_mut();
    state.pending_reconnect = None;
    state.session.stop();
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            engine.stop();
        }
        state.vlc_active = false;
    }
    if let Some(win) = &state.window {
        win.media.stop();
    }
}

/// The live-buffer intercept at the head of `play_builtin`
/// (Python's `_intercept_live_buffer`): returns true when the
/// request was taken over — either replayed through an
/// existing session's view or handed to a background start.
fn buffer_intercept(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    channel: &Channel,
    request: &PlaybackRequest,
    status: Status,
) -> bool {
    let Some(buffers) = buffers_of(player) else {
        return false;
    };
    if buffers.borrow().owns_view(&request.url) {
        return false;
    }
    let settings = iptv_core::timeshift::BufferSettings::from_config(&persist.config.borrow());
    let wants = iptv_core::timeshift::wants_buffer(
        &settings,
        iptv_core::timeshift::EngageContext {
            builtin_player: true,
            // The Rust cast model keeps no persistent
            // connection state to consult (recorded in
            // validation 0045).
            casting: false,
            live_stream: request.live,
            channel_is_movie: false,
        },
    );
    if !wants {
        buffers.borrow_mut().release_current();
        return false;
    }
    if let Some(session) = buffers.borrow_mut().find(&channel.url, &request.headers) {
        if let Ok(lease) = session.acquire() {
            match session.cursor(0.0, false) {
                Ok((url, position)) => {
                    buffers.borrow_mut().current = Some(buffer::BufferCurrent {
                        session,
                        lease,
                        url: url.clone(),
                        channel: channel.clone(),
                        position,
                        last_clock_ms: None,
                    });
                    let mut replay = request.clone();
                    replay.url = url;
                    replay.buffer_bypass = true;
                    replay.resume_ms = None;
                    play_builtin(player, persist, channel, replay, status);
                    return true;
                }
                Err(_) => {
                    session.release(lease);
                }
            }
        }
    }
    // New session: stop the current stream and prepare the
    // buffer in the background; the player tick delivers it.
    buffers.borrow_mut().release_current();
    stop_engines_keep_window(player);
    player_status(status, persist, "Buffering...", 3);
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    {
        let mut registry = buffers.borrow_mut();
        registry.pending = Some(std::sync::Arc::clone(&slot));
        registry.pending_generation = player.borrow().session.generation();
    }
    let url = channel.url.clone();
    let headers = request.headers.clone();
    let channel = channel.clone();
    let cap = persist
        .config
        .borrow()
        .value("internal_player_variant_max_mbps")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| *value > 0.0);
    std::thread::spawn(move || {
        let outcome = start_buffer_session(settings, &url, &headers, cap, channel);
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(outcome);
        }
    });
    true
}

/// The background half of a buffer start (worker thread):
/// create the session, start ingest, wait for history, open
/// the live view.
fn start_buffer_session(
    settings: iptv_core::timeshift::BufferSettings,
    url: &str,
    headers: &iptv_core::playback::StreamHeaders,
    cap: Option<f64>,
    channel: Channel,
) -> buffer::BufferOutcome {
    let key = buffer::source_key(url, headers);
    let Ok(session) = buffer::BufferSession::new(&settings) else {
        return buffer::BufferOutcome::Failed;
    };
    let Ok(lease) = session.acquire() else {
        return buffer::BufferOutcome::Failed;
    };
    if session.start_ingest(url, headers, cap).is_err()
        || session
            .wait_ready(std::time::Duration::from_secs(30))
            .is_err()
    {
        session.release(lease);
        return buffer::BufferOutcome::Failed;
    }
    if let Ok((view_url, position)) = session.cursor(0.0, false) {
        return buffer::BufferOutcome::Ready {
            session,
            key,
            lease,
            url: view_url,
            position,
            channel,
        };
    }
    session.release(lease);
    buffer::BufferOutcome::Failed
}

/// Deliver a finished background buffer start on the player
/// tick: take over playback if the user is still on that
/// channel, otherwise release the lease and let the session
/// close.
fn deliver_buffer_pending(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let Some(buffers) = buffers_of(player) else {
        return;
    };
    let slot = buffers.borrow().pending.clone();
    let Some(slot) = slot else {
        return;
    };
    let outcome = slot.lock().ok().and_then(|mut guard| guard.take());
    let Some(outcome) = outcome else {
        return;
    };
    buffers.borrow_mut().pending = None;
    // A start the user has since stopped or superseded delivers
    // nothing: no playback, no failure dialog.
    if buffers.borrow().pending_generation != player.borrow().session.generation() {
        if let buffer::BufferOutcome::Ready { session, lease, .. } = outcome {
            session.release(lease);
        }
        return;
    }
    match outcome {
        buffer::BufferOutcome::Failed => {
            let frame = player.borrow().window.as_ref().map(|win| win.frame);
            if let Some(frame) = frame {
                let _ = MessageDialog::builder(
                    &frame,
                    "The local live buffer could not start. No extra provider connection was opened. Disable the buffer to retry direct playback.",
                    "Local Live Buffer",
                )
                .with_style(MessageDialogStyle::OK)
                .build()
                .show_modal();
            }
            status.set_label("The local live buffer could not start.");
        }
        buffer::BufferOutcome::Ready {
            session,
            key,
            lease,
            url,
            position,
            channel,
        } => {
            let still_wanted = {
                let state = player.borrow();
                state
                    .current_channel
                    .as_ref()
                    .is_none_or(|current| current.url == channel.url)
            };
            buffers
                .borrow_mut()
                .sessions
                .insert(key, std::sync::Arc::clone(&session));
            if !still_wanted {
                session.release(lease);
                return;
            }
            buffers.borrow_mut().release_current();
            buffers.borrow_mut().current = Some(buffer::BufferCurrent {
                session,
                lease,
                url: url.clone(),
                channel: channel.clone(),
                position,
                last_clock_ms: None,
            });
            let mut replay = PlaybackRequest::for_channel(&channel, None);
            replay.url = url;
            replay.buffer_bypass = true;
            play_builtin(player, persist, &channel, replay, status);
        }
    }
}

/// Playback clock for buffer bookkeeping: (position ms,
/// playing or paused, ended).
fn buffer_clock(state: &PlayerState) -> (i64, bool, bool) {
    if state.vlc_active {
        if let Some(engine) = &state.vlc {
            let state = engine.state();
            return (
                engine.time(),
                matches!(
                    state,
                    iptv_vlc::VlcState::Playing | iptv_vlc::VlcState::Paused
                ),
                state == iptv_vlc::VlcState::Ended,
            );
        }
    }
    match &state.window {
        Some(win) => {
            let media_state = win.media.get_state();
            (
                win.media.tell(),
                matches!(media_state, MediaState::Playing | MediaState::Paused),
                media_state == MediaState::Stopped,
            )
        }
        None => (0, false, false),
    }
}

/// Per-tick buffer housekeeping (Python's `_poll_live_buffer`
/// plus its position tracker): advance the tracked position
/// while the clock runs, jump forward when the tracked
/// position falls out of the retained window, and stop with
/// Python's messages when the buffer fails or input ends.
/// What one buffer poll decided to do.
enum BufferPollAct {
    None,
    Failed,
    Expired,
    Ended,
}

#[allow(clippy::cast_precision_loss)] // ms deltas to f64 seconds, as Python's /1000
fn poll_buffer(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let Some(buffers) = buffers_of(player) else {
        return;
    };
    let act = {
        let mut registry = buffers.borrow_mut();
        let Some(current) = registry.current.as_mut() else {
            return;
        };
        if current.session.window_error().is_some() {
            BufferPollAct::Failed
        } else {
            let (clock_ms, live_clock, ended) = buffer_clock(&player.borrow());
            if live_clock && clock_ms >= 0 {
                if let Some(before) = current.last_clock_ms {
                    if clock_ms >= before {
                        current.position += (clock_ms - before) as f64 / 1000.0;
                    }
                }
                current.last_clock_ms = Some(clock_ms);
            }
            let span = current.session.retained_span();
            let expired = span.is_some_and(|(first, _)| current.position < first);
            let paused = {
                let state = player.borrow();
                state.session.state() == &SessionState::Paused
            };
            if expired && !paused {
                BufferPollAct::Expired
            } else if current.session.process_exited() && ended {
                BufferPollAct::Ended
            } else {
                BufferPollAct::None
            }
        }
    };
    match act {
        BufferPollAct::Failed => {
            player_stop(player, persist, status);
            player_status(
                status,
                persist,
                "The local buffer stopped because its storage or input failed.",
                1,
            );
        }
        BufferPollAct::Expired => {
            if let Some((first, last)) = buffers
                .borrow()
                .current
                .as_ref()
                .and_then(|current| current.session.retained_span())
            {
                jump_live_buffer(player, persist, status, last - first);
                player_status(
                    status,
                    persist,
                    "The old buffered position expired; playback moved to the retained window.",
                    1,
                );
            }
        }
        BufferPollAct::Ended => {
            player_stop(player, persist, status);
            player_status(
                status,
                persist,
                "Live input ended; retained history is still available.",
                1,
            );
        }
        BufferPollAct::None => {}
    }
}

/// Jump the buffered view `behind` seconds behind live
/// (0 = live edge). Returns false when there is no current
/// buffer or the position is not retained.
fn jump_live_buffer(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    behind: f64,
) -> bool {
    let Some(buffers) = buffers_of(player) else {
        return false;
    };
    let jumped = {
        let mut registry = buffers.borrow_mut();
        let Some(current) = registry.current.as_mut() else {
            return false;
        };
        match current.session.cursor(behind, false) {
            Ok((url, position)) => {
                current.url.clone_from(&url);
                current.position = position;
                current.last_clock_ms = None;
                Some((current.channel.clone(), url))
            }
            Err(_) => None,
        }
    };
    let Some((channel, url)) = jumped else {
        return false;
    };
    let mut replay = PlaybackRequest::for_channel(&channel, None);
    replay.url = url;
    replay.buffer_bypass = true;
    play_builtin(player, persist, &channel, replay, status);
    true
}

/// Resume-after-pause expiry (Python's `_resume_live_buffer`
/// position check), run when playback resumes.
fn buffer_resume_check(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let Some(buffers) = buffers_of(player) else {
        return;
    };
    let expired = {
        let registry = buffers.borrow();
        let Some(current) = registry.current.as_ref() else {
            return;
        };
        current
            .session
            .retained_span()
            .is_some_and(|(first, _)| current.position < first)
    };
    if expired {
        if let Some((first, last)) = buffers
            .borrow()
            .current
            .as_ref()
            .and_then(|current| current.session.retained_span())
        {
            jump_live_buffer(player, persist, status, last - first);
            player_status(
                status,
                persist,
                "The paused position expired. Playback resumed at the oldest retained picture.",
                1,
            );
        }
    }
}

/// Playback > Local Live Buffer...: Python's two prompts
/// (minutes, then MiB), the recording-lease refusal when
/// disabling, persistence of the three config keys, and a
/// relaunch of the live channel being watched.
fn prompt_live_buffer_config(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let mut settings = iptv_core::timeshift::BufferSettings::from_config(&persist.config.borrow());
    let dialog = TextEntryDialog::builder(
        &frame,
        "Keep up to this many minutes of live TV (1\u{2013}30); 0 disables the local buffer.",
        "Local Live Buffer",
    )
    .with_default_value(&settings.dialog_minutes().to_string())
    .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(minutes) = text.trim().parse::<i64>() else {
        status.set_label(&format!("\"{text}\" is not a whole number of minutes."));
        return;
    };
    if !(0..=30).contains(&minutes) {
        status.set_label("Minutes must be between 0 and 30.");
        return;
    }
    if minutes == 0 {
        let feeding = buffers_of(player)
            .and_then(|buffers| {
                buffers
                    .borrow()
                    .current
                    .as_ref()
                    .map(|current| current.session.lease_count())
            })
            .is_some_and(|count| count > 1);
        if feeding {
            let _ = MessageDialog::builder(
                &frame,
                "The local buffer is still feeding a recording. Stop the recording before disabling it.",
                "Local Live Buffer",
            )
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal();
            return;
        }
        settings.apply_dialog_choice(0, settings.mib);
    } else {
        let dialog = TextEntryDialog::builder(
            &frame,
            "Maximum local buffer size per session (MiB). Old data is removed when either limit is reached.",
            "Local Live Buffer",
        )
        .with_default_value(&settings.mib.to_string())
        .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        let text = dialog.get_value().unwrap_or_default();
        let Ok(mib) = text.trim().parse::<i64>() else {
            status.set_label(&format!("\"{text}\" is not a whole number of MiB."));
            return;
        };
        if !(64..=4096).contains(&mib) {
            status.set_label("Buffer size must be between 64 and 4096 MiB.");
            return;
        }
        settings.apply_dialog_choice(minutes, mib);
    }
    persist.set_and_save("live_buffer_enabled", serde_json::json!(settings.enabled));
    persist.set_and_save("live_buffer_seconds", serde_json::json!(settings.seconds));
    persist.set_and_save("live_buffer_mib", serde_json::json!(settings.mib));
    status.set_label(if settings.enabled {
        "Local live buffer settings saved."
    } else {
        "Local live buffer disabled."
    });
    // Python relaunches the live channel being watched so the
    // new setting applies immediately.
    let live_channel = {
        let state = player.borrow();
        state
            .current_channel
            .clone()
            .filter(|_| state.session.state() != &SessionState::Idle)
    };
    if let Some(channel) = live_channel {
        if let Some(buffers) = buffers_of(player) {
            buffers.borrow_mut().release_current();
        }
        let request = PlaybackRequest::for_channel(&channel, None);
        if request.live {
            play_builtin(player, persist, &channel, request, status);
        }
    }
}

/// Playback > Rewind Live TV...: Python's seconds prompt over
/// the retained span; without a buffer it falls back to
/// Return to Live, like Python's `_seek_live_buffer`.
#[allow(clippy::cast_precision_loss)] // whole seconds to f64, as Python
fn prompt_rewind(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let range = buffers_of(player).and_then(|buffers| {
        buffers
            .borrow()
            .current
            .as_ref()
            .and_then(|current| current.session.rewind_range())
    });
    let Some(max_seconds) = range else {
        return_to_live(player, frame, persist, status);
        return;
    };
    let dialog = TextEntryDialog::builder(
        &frame,
        "Seconds behind the latest buffered picture; 0 returns to live.",
        "Rewind Live TV",
    )
    .with_default_value("0")
    .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(seconds) = text.trim().parse::<i64>() else {
        status.set_label(&format!("\"{text}\" is not a whole number of seconds."));
        return;
    };
    let seconds = seconds.clamp(0, max_seconds.max(0));
    jump_live_buffer(player, persist, status, seconds as f64);
}

/// Playback > Return to Live: jump to the live edge; with no
/// current buffer, relaunch the last buffered channel (which
/// re-buffers), else Python's information box.
fn return_to_live(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    if jump_live_buffer(player, persist, status, 0.0) {
        return;
    }
    let last = buffers_of(player).and_then(|buffers| buffers.borrow().last.clone());
    if let Some((channel, _view)) = last {
        let request = PlaybackRequest::for_channel(&channel, None);
        play_builtin(player, persist, &channel, request, status);
        return;
    }
    let _ = MessageDialog::builder(
        &frame,
        "No local live history is available.",
        "Local Live Buffer",
    )
    .with_style(MessageDialogStyle::OK)
    .build()
    .show_modal();
}

/// Release a recording's buffer lease, if it held one.
fn release_recording_lease(buffers: &Rc<RefCell<buffer::BufferRegistry>>, key: &str) {
    if let Some((session, lease)) = buffers.borrow_mut().recording_leases.remove(key) {
        session.release(lease);
    }
}

/// Stop playback and hide the player window (Stop button, the stop
/// and exit shortcuts).
fn player_stop(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    if let Some(buffers) = buffers_of(player) {
        buffers.borrow_mut().release_current();
    }
    if let Some(buffers) = buffers_of(player) {
        buffers.borrow_mut().pending = None;
    }
    checkpoint_resume(player, persist);
    let mut state = player.borrow_mut();
    state.pending_reconnect = None;
    state.session.stop();
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            engine.stop();
        }
        state.vlc_active = false;
    }
    if let Some(win) = &state.window {
        win.media.stop();
        win.frame.show(false);
    }
    player_status(status, persist, "Stopped.", 2);
}

/// Seek the active engine (libVLC when it drives playback, else
/// the `MediaCtrl`).
fn player_seek_to(player: &Rc<RefCell<PlayerState>>, target_ms: i64) {
    let mut state = player.borrow_mut();
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            engine.set_time(target_ms);
        }
    } else if let Some(win) = &state.window {
        win.media.seek(target_ms, SeekMode::FromStart);
    }
}

/// Set the active engine's volume and mirror it on the slider.
fn player_set_volume(player: &Rc<RefCell<PlayerState>>, percent: i32) {
    let mut state = player.borrow_mut();
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            engine.set_volume(percent);
        }
    } else if let Some(win) = &state.window {
        win.media.set_volume(f64::from(percent) / 100.0);
    }
    if let Some(win) = &state.window {
        win.volume.set_value(percent);
    }
}

/// Poll the libVLC engine on the 1 s tick: resume seek, one-time
/// track setup, and end/error transitions into the session
/// machine (with its reconnect handling).
fn poll_vlc_state(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let mut setup_tracks = false;
    {
        let mut state = player.borrow_mut();
        if !state.vlc_active {
            return;
        }
        let Some(engine) = &state.vlc else {
            return;
        };
        match engine.state() {
            iptv_vlc::VlcState::Playing => {
                state.session.note_buffering(false, now_ms());
                if !matches!(
                    state.session.state(),
                    SessionState::Playing | SessionState::Paused
                ) {
                    // Playing is reported only on the engine's own
                    // confirmation, never at load time.
                    state.session.on_event(EngineEvent::Started, now_ms());
                    player_status(status, persist, &state.session.announcement(), 3);
                }
                if let Some(ms) = state.pending_resume_ms.take() {
                    if let Some(engine) = &mut state.vlc {
                        engine.set_time(ms);
                    }
                }
                if !state.vlc_tracks_ready {
                    state.vlc_tracks_ready = true;
                    setup_tracks = true;
                }
            }
            iptv_vlc::VlcState::Buffering => {
                state.session.note_buffering(true, now_ms());
            }
            iptv_vlc::VlcState::Ended | iptv_vlc::VlcState::Error => {
                let ended = engine.state() == iptv_vlc::VlcState::Ended;
                let (position, length) = (engine.time(), engine.length());
                let near_end = length > 0 && position >= length - 1500;
                let event = if ended || near_end {
                    EngineEvent::Ended
                } else {
                    EngineEvent::Stalled
                };
                let action = state.session.on_event(event, now_ms());
                state.vlc_active = false;
                drop(state);
                store_session_action(player, persist, status, action);
                let state = player.borrow();
                player_status(status, persist, &state.session.announcement(), 3);
                return;
            }
            _ => {}
        }
    }
    if setup_tracks {
        apply_vlc_tracks(player, persist);
    }
}

/// Fill the player window's audio track choice from libVLC and
/// apply the recorded preference (explicit choice first, then
/// the slice-9 keyword engine).
fn apply_vlc_tracks(player: &Rc<RefCell<PlayerState>>, persist: &Persist) {
    let tracks = {
        let state = player.borrow();
        state
            .vlc
            .as_ref()
            .map(iptv_vlc::VlcEngine::audio_tracks)
            .unwrap_or_default()
    };
    if tracks.is_empty() {
        return;
    }
    let win = {
        let mut state = player.borrow_mut();
        state.vlc_track_ids = tracks.iter().map(|(id, _)| *id).collect();
        state.window.clone()
    };
    let Some(win) = win else {
        return;
    };
    win.tracks.clear();
    for (_, name) in &tracks {
        win.tracks.append(name);
    }
    let (keywords, prefer_ad, desired) = {
        let config = persist.config.borrow();
        (
            iptv_core::audiotracks::preferred_audio_keywords(
                &config.string_list("preferred_audio_tracks"),
                config.bool_or("prefer_audio_description", false),
            ),
            config.bool_or("prefer_audio_description", false),
            player.borrow().desired_track.clone(),
        )
    };
    let pairs: Vec<(i64, String)> = tracks
        .iter()
        .map(|(id, name)| (i64::from(*id), name.clone()))
        .collect();
    let chosen = desired
        .as_ref()
        .and_then(|want| {
            tracks
                .iter()
                .find(|(_, name)| name == want)
                .map(|(id, _)| *id)
        })
        .or_else(|| {
            iptv_core::audiotracks::select_preferred_audio_track(&pairs, &keywords, None, prefer_ad)
                .map(|id| i32::try_from(id).unwrap_or(-1))
        });
    if let Some(id) = chosen {
        let mut state = player.borrow_mut();
        if let Some(engine) = &mut state.vlc {
            engine.set_audio_track(id);
        }
        if let Some(position) = state.vlc_track_ids.iter().position(|track| *track == id) {
            win.tracks
                .set_selection(u32::try_from(position).unwrap_or(0));
        }
    }
    if let Some(menu) = audio_track_menu(player) {
        let mut state = player.borrow_mut();
        for old in state.track_menu_ids.drain(..) {
            menu.delete(old);
        }
        menu.delete(PM_NO_TRACKS);
        for (index, (_, name)) in tracks.iter().enumerate() {
            let id = PM_TRACK_BASE + i32::try_from(index).unwrap_or(0);
            menu_add(&menu, id, name, "", ItemKind::Radio);
            state.track_menu_ids.push(id);
        }
        let selected = win.tracks.get_selection().unwrap_or(0);
        if let Some(id) = state.track_menu_ids.get(selected as usize) {
            menu.check_item(*id, true);
        }
    }
}

/// Refresh the player window's time label, seek position and
/// subtitle label from the media position (1 s tick).
fn update_player_chrome(player: &Rc<RefCell<PlayerState>>) {
    let mut state = player.borrow_mut();
    let Some(win) = state.window.clone() else {
        return;
    };
    let (position, length) = position_length(&state);
    let text = if length > 0 {
        format!(
            "{} / {}",
            format_duration_ms(position),
            format_duration_ms(length)
        )
    } else if position > 0 {
        format!("{} — Live", format_duration_ms(position))
    } else {
        "Live".to_string()
    };
    win.time_label.set_label(&text);
    if length > 0 {
        win.seek
            .set_value(i32::try_from(position * 1000 / length).unwrap_or(0));
    }
    let current = active_cue(&state.subtitle_cues, position);
    if current != state.last_label_cue {
        state.last_label_cue = current;
        let label = current.map_or("", |index| state.subtitle_cues[index].text.as_str());
        win.subtitle_label.set_label(label);
    }
}

/// The canonical shortcut label for a key event, matching the
/// shortcuts engine's spelling ("Ctrl+P", "Shift+S", "F11", "Up").
fn key_event_label(event: wxdragon::event::Event) -> Option<String> {
    let code = event.get_key_code()?;
    let key = match code {
        340..=351 => format!("F{}", code - 339),
        27 => "Esc".to_string(),
        13 => "Enter".to_string(),
        32 => "Space".to_string(),
        315 => "Up".to_string(),
        317 => "Down".to_string(),
        314 => "Left".to_string(),
        316 => "Right".to_string(),
        366 => "PageUp".to_string(),
        367 => "PageDown".to_string(),
        _ => {
            let ch = event
                .get_unicode_key()
                .and_then(|code| u32::try_from(code).ok())
                .and_then(char::from_u32)?;
            if !ch.is_alphanumeric() {
                return None;
            }
            ch.to_uppercase().collect()
        }
    };
    let mut label = String::new();
    if event.control_down() {
        label.push_str("Ctrl+");
    }
    if event.alt_down() {
        label.push_str("Alt+");
    }
    if event.shift_down() {
        label.push_str("Shift+");
    }
    label.push_str(&key);
    Some(label)
}

/// Dispatch one player-window key against the effective player
/// shortcuts. Returns whether the key was a command.
fn dispatch_player_key(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    event: wxdragon::event::Event,
) -> bool {
    let Some(label) = key_event_label(event) else {
        return false;
    };
    let config = serde_json::Value::Object(persist.config.borrow().raw().clone());
    let bindings = iptv_core::shortcuts::effective(&config, "player");
    let Some(action) = bindings
        .iter()
        .find(|(_, binding)| binding.eq_ignore_ascii_case(&label))
        .map(|(action, _)| action.clone())
    else {
        return false;
    };
    let win = player.borrow().window.clone();
    let Some(win) = win else {
        return false;
    };
    match action.as_str() {
        "play_pause" => toggle_pause_state(player, persist, status),
        "stop" | "exit" => player_stop(player, persist, status),
        "record" => {
            let state = player.borrow();
            if let (Some(shared), Some(channel)) = (&state.shared, &state.current_channel) {
                toggle_recording(
                    &shared.recordings,
                    &shared.buffers,
                    persist,
                    channel,
                    status,
                );
                assert!(shared.timer.start(50, false));
            }
        }
        "cast" => {
            let state = player.borrow();
            if let Some(shared) = &state.shared {
                show_cast_for(
                    &shared.cast_ui,
                    &shared.cast_worker,
                    &shared.cast_pending,
                    &shared.timer,
                    &shared.proxy,
                    state.current_channel.as_ref(),
                );
            }
        }
        "volume_up" | "volume_down" => {
            let delta = if action == "volume_up" { 10 } else { -10 };
            let value = (win.volume.get_value() + delta).clamp(0, 100);
            player_set_volume(player, value);
            status.set_silent(&format!("Volume {value} percent."));
        }
        "audio_track" => win.tracks.set_focus(),
        "subtitles" => prompt_subtitle_file(player, win.frame, persist, status),
        "speak_subtitles" => {
            let next = !persist.config.borrow().bool_or("speak_subtitles", false);
            persist.set_and_save("speak_subtitles", serde_json::json!(next));
            player_status(
                status,
                persist,
                if next {
                    "Subtitle speech on."
                } else {
                    "Subtitle speech off."
                },
                2,
            );
        }
        "read_subtitle" | "previous_subtitle" | "next_subtitle" => {
            announce_cue(player, &action, status);
        }
        "fullscreen" => win.frame.show_full_screen(!win.frame.is_full_screen()),
        "hide" => win.frame.hide(),
        "what_is_playing" => {
            let state = player.borrow();
            match &state.current_channel {
                Some(channel) => status.set_label(&format!("Now playing: {}", channel.name)),
                None => status.set_label("Nothing is playing."),
            }
        }
        _ => return false,
    }
    true
}

/// Announce the current, previous, or next subtitle cue's text.
fn announce_cue(player: &Rc<RefCell<PlayerState>>, action: &str, status: Status) {
    let state = player.borrow();
    if state.subtitle_cues.is_empty() {
        status.set_label("No subtitles are loaded.");
        return;
    }
    let position = position_length(&state).0;
    let current = active_cue(&state.subtitle_cues, position);
    let index = match action {
        "previous_subtitle" => current.and_then(|i| i.checked_sub(1)).or(current),
        "next_subtitle" => current
            .map(|i| (i + 1).min(state.subtitle_cues.len() - 1))
            .or(Some(0)),
        _ => current,
    };
    match index.and_then(|i| state.subtitle_cues.get(i)) {
        Some(cue) => {
            live_region::announce_with_priority(
                status.label,
                &cue.text,
                live_region::Priority::Medium,
            );
        }
        None => status.set_label("No subtitle is showing."),
    }
}

/// Open the cast window for an explicit channel (the player
/// window's Cast action; the main window passes its selection).
fn show_cast_for(
    ui_slot: &Rc<RefCell<Option<CastUi>>>,
    worker: &Rc<CastWorker>,
    pending: &Rc<std::cell::Cell<bool>>,
    timer: &Rc<Timer<Frame>>,
    proxy: &Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    channel: Option<&Channel>,
) {
    let ui = ensure_cast_ui(ui_slot, worker, pending, timer, proxy);
    ui.frame.show(true);
    ui.frame.raise();
    ui.view.borrow_mut().channel = channel.cloned();
    match channel {
        Some(channel) => {
            ui.status
                .set_label(&format!("Looking for cast devices for {}...", channel.name));
        }
        None => {
            ui.status
                .set_label("Choose a channel in the main window, then discover.");
        }
    }
    worker.discover();
    pending.set(true);
    assert!(timer.start(50, false));
}

fn bind_subtitle_load(
    subs: Button,
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let subs_player = Rc::clone(player);
    let subs_persist = persist.clone();
    subs.on_click(move |_| prompt_subtitle_file(&subs_player, frame, &subs_persist, status));
}

/// Ask for a subtitle file and load its cues into the player state.
fn prompt_subtitle_file(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    let subs_player = Rc::clone(player);
    {
        let dialog = FileDialog::builder(&frame)
            .with_message("Load subtitles")
            .with_wildcard(
                "Subtitles (*.srt;*.vtt;*.ass;*.ssa)|*.srt;*.vtt;*.ass;*.ssa|All files (*.*)|*.*",
            )
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        let Some(path) = dialog.get_path() else {
            return;
        };
        match load_cues(std::path::Path::new(&path)) {
            Ok(cues) => {
                let count = cues.len();
                let mut state = subs_player.borrow_mut();
                state.subtitle_cues = cues;
                state.last_spoken_cue = None;
                player_status(
                    status,
                    persist,
                    &format!("Loaded {count} subtitle cues."),
                    2,
                );
            }
            Err(error) => {
                player_status(
                    status,
                    persist,
                    &format!("Could not load subtitles: {error}"),
                    1,
                );
            }
        }
    }
}

/// Announce the active subtitle cue when it changes, while the
/// `speak_subtitles` setting is on (the reference's spoken-subtitle
/// path, riding the live region).
/// (position, length) in ms from the active engine: libVLC when
/// it is driving this playback, else the `MediaCtrl`.
fn position_length(state: &PlayerState) -> (i64, i64) {
    if state.vlc_active {
        if let Some(engine) = &state.vlc {
            return (engine.time(), engine.length());
        }
    }
    match &state.window {
        Some(win) => (win.media.tell(), win.media.length()),
        None => (0, 0),
    }
}

fn speak_subtitle_cue(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    if !persist.config.borrow().bool_or("speak_subtitles", false) {
        return;
    }
    let position = position_length(&player.borrow()).0;
    if position <= 0 {
        return;
    }
    let mut state = player.borrow_mut();
    if state.subtitle_cues.is_empty() {
        return;
    }
    let current = active_cue(&state.subtitle_cues, position);
    if current != state.last_spoken_cue {
        state.last_spoken_cue = current;
        if let Some(index) = current {
            let text = state.subtitle_cues[index].text.clone();
            live_region::announce_with_priority(status.label, &text, live_region::Priority::Medium);
        }
    }
}

/// Persist the current position for seekable content (Python checkpoint
/// rules decide whether it is kept).
fn checkpoint_resume(player: &Rc<RefCell<PlayerState>>, persist: &Persist) {
    let state = player.borrow();
    if state.session.current().is_none_or(|r| r.live) {
        return;
    }
    let Some(channel) = &state.current_channel else {
        return;
    };
    let key = channel_key(channel);
    let title = channel.name.clone();
    let (position, length) = position_length(&state);
    drop(state);
    if position <= 0 || length <= 0 {
        return;
    }
    let mut entries = persist.resume_entries();
    resume_checkpoint(
        &mut entries,
        &key,
        &title,
        position,
        length,
        now_ms() / 1000,
    );
    persist.save_resume(&entries);
}

/// The row an action applies to: the selected row, else the focused row.
fn pick_row(selected: Option<usize>, focused: Option<usize>) -> Option<usize> {
    selected.or(focused)
}

/// Reload playlists and provider accounts from the config (startup
/// and after the Playlist Manager saves).
fn reload_sources(wiring: &Wiring) {
    let (sources, accounts) = configured_entries(wiring);
    if !sources.is_empty() {
        wiring.browser.borrow_mut().load_sources(sources);
        assert!(wiring.timer.start(50, false));
    }
    if !accounts.is_empty() {
        wiring.provider_worker.load(accounts);
        wiring.provider_pending.set(true);
        assert!(wiring.timer.start(50, false));
    }
}

/// Reload the guide from the configured EPG URLs.
/// Where the guide's `SQLite` mirror lives.
fn epg_store_path() -> std::path::PathBuf {
    iptv_core::config::user_config_dir().join("epg.db")
}

/// Start a guide load now (the worker also mirrors the guide to
/// the store on its own thread).
fn dispatch_epg_load(wiring: &Wiring, epg_urls: Vec<String>) {
    if epg_urls.is_empty() {
        return;
    }
    wiring.epg_worker.load(epg_urls, Some(epg_store_path()));
    wiring.epg_pending.set(true);
    assert!(wiring.timer.start(50, false));
}

/// Reload the guide from the configured EPG URLs. While a playlist
/// load is running, the guide waits: at real-world scale the guide
/// is hundreds of MB and racing it starved the playlist download
/// and parse, so channels appeared minutes late (validation 0049).
/// The completion tick dispatches the deferred load the moment the
/// browser settles.
fn reload_epg(wiring: &Wiring) {
    let epg_urls = wiring.persist.config.borrow().string_list("epg_urls");
    if epg_urls.is_empty() {
        return;
    }
    if wiring.browser.borrow().is_busy() {
        wiring.epg_deferred.set(true);
        return;
    }
    dispatch_epg_load(wiring, epg_urls);
}

/// Display name for one `playlists` config entry in the manager.
fn entry_display(entry: &serde_json::Value) -> String {
    match entry {
        serde_json::Value::String(text) => {
            if text.starts_with("http://") || text.starts_with("https://") {
                text.clone()
            } else {
                scope_display_name(text)
            }
        }
        serde_json::Value::Object(_) => ProviderAccount::from_config(entry)
            .map_or_else(|| "Account".to_string(), |account| account.describe()),
        _ => "Entry".to_string(),
    }
}

/// Refresh a manager list box from its working entries.
fn manager_refresh(list: ListBox, entries: &[serde_json::Value]) {
    let labels: Vec<String> = entries.iter().map(entry_display).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
}

/// A small deterministic id for a manager-created account, so its
/// channels carry a stable playlist scope id.
fn generated_account_id(kind: &str, base_url: &str, user: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("{base_url}|{user}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{kind}-{hash:012x}")
}

/// The buttons of the Playlist Manager window.
struct PlaylistManagerButtons {
    add_file: Button,
    add_url: Button,
    add_xtream: Button,
    add_stalker: Button,
    edit: Button,
    remove: Button,
}

/// Bind the Playlist Manager's add/edit/remove actions.
#[allow(clippy::too_many_arguments)]
fn bind_playlist_manager_buttons(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    entries: &Rc<RefCell<Vec<serde_json::Value>>>,
    buttons: &PlaylistManagerButtons,
    mstatus: Status,
) {
    let PlaylistManagerButtons {
        add_file,
        add_url,
        add_xtream,
        add_stalker,
        edit,
        remove,
    } = *buttons;
    let entries = Rc::clone(entries);
    let entries_f = Rc::clone(&entries);
    add_file.on_click(move |_| {
        let dialog = FileDialog::builder(&manager_frame)
            .with_message("Add playlist file")
            .with_wildcard("M3U playlists (*.m3u;*.m3u8)|*.m3u;*.m3u8|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        if let Some(path) = dialog.get_path() {
            entries_f.borrow_mut().push(serde_json::Value::String(path));
            manager_refresh(list, &entries_f.borrow());
        }
    });
    let entries_u = Rc::clone(&entries);
    add_url.on_click(move |_| {
        let dialog = TextEntryDialog::builder(&manager_frame, "Playlist URL:", "Add URL").build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        if let Some(url) = dialog.get_value() {
            if !url.trim().is_empty() {
                entries_u
                    .borrow_mut()
                    .push(serde_json::Value::String(url.trim().to_string()));
                manager_refresh(list, &entries_u.borrow());
            }
        }
    });
    let wiring_x = clone_wiring(wiring);
    let entries_x = Rc::clone(&entries);
    add_xtream.on_click(move |_| {
        open_account_form(
            &wiring_x,
            "xtream",
            None,
            Rc::clone(&entries_x),
            list,
            mstatus,
        );
    });
    let wiring_s = clone_wiring(wiring);
    let entries_s = Rc::clone(&entries);
    add_stalker.on_click(move |_| {
        open_account_form(
            &wiring_s,
            "stalker",
            None,
            Rc::clone(&entries_s),
            list,
            mstatus,
        );
    });
    bind_manager_edit_buttons(wiring, manager_frame, list, &entries, edit, remove, mstatus);
}

/// Bind the Playlist Manager's edit/remove actions.
#[allow(clippy::too_many_arguments)]
fn bind_manager_edit_buttons(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    entries: &Rc<RefCell<Vec<serde_json::Value>>>,
    edit: Button,
    remove: Button,
    mstatus: Status,
) {
    let entries = Rc::clone(entries);
    let wiring_e = clone_wiring(wiring);
    let entries_e = Rc::clone(&entries);
    edit.on_click(move |_| {
        let Some(selection) = list.get_selection() else {
            mstatus.set_label("Choose an entry to edit first.");
            return;
        };
        let index = usize::try_from(selection).unwrap_or(usize::MAX);
        let Some(entry) = entries_e.borrow().get(index).cloned() else {
            return;
        };
        match &entry {
            serde_json::Value::String(text) => {
                let dialog =
                    TextEntryDialog::builder(&manager_frame, "Playlist location:", "Edit Playlist")
                        .with_default_value(text)
                        .build();
                if dialog.show_modal() != ID_OK {
                    return;
                }
                if let Some(value) = dialog.get_value() {
                    entries_e.borrow_mut()[index] =
                        serde_json::Value::String(value.trim().to_string());
                    manager_refresh(list, &entries_e.borrow());
                }
            }
            serde_json::Value::Object(map) => {
                let kind = map
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("xtream")
                    .to_string();
                open_account_form(
                    &wiring_e,
                    &kind,
                    Some((index, entry)),
                    Rc::clone(&entries_e),
                    list,
                    mstatus,
                );
            }
            _ => {}
        }
    });
    let entries_r = Rc::clone(&entries);
    remove.on_click(move |_| {
        let Some(selection) = list.get_selection() else {
            mstatus.set_label("Choose an entry to remove first.");
            return;
        };
        let index = usize::try_from(selection).unwrap_or(usize::MAX);
        if index < entries_r.borrow().len() {
            entries_r.borrow_mut().remove(index);
            manager_refresh(list, &entries_r.borrow());
        }
    });
}

/// File > Playlist Manager: edit the `playlists` config list
/// (files, URLs, Xtream and Stalker accounts) with OK/Cancel
/// semantics, as in the Python dialog.
fn open_playlist_manager(wiring: &Wiring, status: Status, scope_choice: Choice) {
    let entries: Rc<RefCell<Vec<serde_json::Value>>> = Rc::new(RefCell::new(
        match wiring.persist.config.borrow().value("playlists") {
            Some(serde_json::Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        },
    ));
    let manager = Frame::builder()
        .with_title("Playlist Manager")
        .with_size(Size::new(680, 520))
        .build();
    manager.set_accessibility_label("Playlist Manager");
    let panel = Panel::builder(&manager).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label("Playlists");
    manager_refresh(list, &entries.borrow());
    let add_file = Button::builder(&panel).with_label("Add &File").build();
    let add_url = Button::builder(&panel).with_label("Add &URL").build();
    let add_xtream = Button::builder(&panel)
        .with_label("Add &Xtream Codes")
        .build();
    let add_stalker = Button::builder(&panel)
        .with_label("Add &Stalker Portal")
        .build();
    let edit = Button::builder(&panel).with_label("&Edit").build();
    let remove = Button::builder(&panel).with_label("&Remove").build();
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let mstatus_label = StaticText::builder(&panel)
        .with_label("Manager status")
        .build();
    let mdetails = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("Add, edit, or remove playlists and accounts. OK saves and reloads.")
        .build();
    mdetails.set_accessibility_label("Manager status");
    let mstatus = Status {
        label: mstatus_label,
        details: mdetails,
        bar: None,
    };

    let manager_frame = manager;
    bind_playlist_manager_buttons(
        wiring,
        manager_frame,
        list,
        &entries,
        &PlaylistManagerButtons {
            add_file,
            add_url,
            add_xtream,
            add_stalker,
            edit,
            remove,
        },
        mstatus,
    );
    let wiring_ok = clone_wiring(wiring);
    let entries_ok = Rc::clone(&entries);
    ok.on_click(move |_| {
        wiring_ok.persist.set_and_save(
            "playlists",
            serde_json::Value::Array(entries_ok.borrow().clone()),
        );
        fill_scope_choices(&wiring_ok, scope_choice);
        reload_sources(&wiring_ok);
        status.set_label("Playlists saved. Reloading...");
        manager_frame.hide();
    });
    cancel.on_click(move |_| manager.hide());
    manager.show(true);
}

/// The Xtream / Stalker account form (add or edit). Fields write the
/// same config dict shape `ProviderAccount::from_config` reads.
fn open_account_form(
    wiring: &Wiring,
    kind: &str,
    existing: Option<(usize, serde_json::Value)>,
    entries: Rc<RefCell<Vec<serde_json::Value>>>,
    manager_list: ListBox,
    mstatus: Status,
) {
    let _ = wiring;
    let title = if kind == "stalker" {
        "Stalker Portal"
    } else {
        "Xtream Codes"
    };
    let form = Frame::builder()
        .with_title(title)
        .with_size(Size::new(520, 420))
        .build();
    form.set_accessibility_label(title);
    let panel = Panel::builder(&form).build();
    let get = |key: &str| -> String {
        existing
            .as_ref()
            .and_then(|(_, value)| value.get(key))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut fields: Vec<(String, TextCtrl)> = Vec::new();
    let specs: Vec<(&str, &str, bool)> = if kind == "stalker" {
        vec![
            ("Name", "name", false),
            ("Portal URL", "base_url", false),
            ("MAC address", "mac", false),
        ]
    } else {
        vec![
            ("Name", "name", false),
            ("Server URL", "base_url", false),
            ("Username", "username", false),
            ("Password", "password", true),
        ]
    };
    for (label, key, secret) in &specs {
        let caption = StaticText::builder(&panel).with_label(label).build();
        let _ = caption;
        let builder = TextCtrl::builder(&panel).with_value(&get(key));
        let field = if *secret {
            builder.with_style(TextCtrlStyle::Password).build()
        } else {
            builder.build()
        };
        field.set_accessibility_label(label);
        fields.push(((*key).to_string(), field));
    }
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let form_hide = form;
    cancel.on_click(move |_| form_hide.hide());
    let kind_owned = kind.to_string();
    ok.on_click(move |_| {
        let mut map = serde_json::Map::new();
        map.insert("type".to_string(), serde_json::json!(kind_owned));
        let mut base_url = String::new();
        let mut user = String::new();
        for (key, field) in &fields {
            let value = field.get_value();
            if key == "base_url" {
                base_url.clone_from(&value);
            }
            if key == "username" || key == "mac" {
                user.clone_from(&value);
            }
            if key == "name" && value.trim().is_empty() {
                continue;
            }
            map.insert(key.clone(), serde_json::json!(value));
        }
        let id = existing
            .as_ref()
            .and_then(|(_, value)| value.get("id"))
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || generated_account_id(&kind_owned, &base_url, &user),
                ToString::to_string,
            );
        map.insert("id".to_string(), serde_json::json!(id));
        let value = serde_json::Value::Object(map);
        match &existing {
            Some((index, _)) => {
                entries.borrow_mut()[*index] = value;
            }
            None => entries.borrow_mut().push(value),
        }
        manager_refresh(manager_list, &entries.borrow());
        mstatus.set_label("Account saved in the list. OK in the Playlist Manager writes it.");
        form.hide();
    });
    form.show(true);
}

/// File > EPG Manager: edit the `epg_urls` config list.
fn open_epg_manager(wiring: &Wiring, status: Status) {
    let urls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(
        wiring.persist.config.borrow().string_list("epg_urls"),
    ));
    let manager = Frame::builder()
        .with_title("EPG Manager")
        .with_size(Size::new(640, 440))
        .build();
    manager.set_accessibility_label("EPG Manager");
    let panel = Panel::builder(&manager).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label("EPG sources");
    list.set_items(&urls.borrow().iter().map(String::as_str).collect::<Vec<_>>());
    let add_url = Button::builder(&panel).with_label("Add &URL").build();
    let add_file = Button::builder(&panel).with_label("Add &File").build();
    let remove = Button::builder(&panel).with_label("&Remove").build();
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let urls_u = Rc::clone(&urls);
    let manager_frame = manager;
    add_url.on_click(move |_| {
        let dialog =
            TextEntryDialog::builder(&manager_frame, "EPG URL (XMLTV):", "Add EPG URL").build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        if let Some(url) = dialog.get_value() {
            if !url.trim().is_empty() {
                urls_u.borrow_mut().push(url.trim().to_string());
                list.set_items(
                    &urls_u
                        .borrow()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                );
            }
        }
    });
    let urls_f = Rc::clone(&urls);
    add_file.on_click(move |_| {
        let dialog = FileDialog::builder(&manager_frame)
            .with_message("Add EPG file")
            .with_wildcard("XMLTV (*.xml;*.xml.gz)|*.xml;*.xml.gz|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        if let Some(path) = dialog.get_path() {
            urls_f.borrow_mut().push(path);
            list.set_items(
                &urls_f
                    .borrow()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            );
        }
    });
    let urls_r = Rc::clone(&urls);
    remove.on_click(move |_| {
        if let Some(selection) = list.get_selection() {
            let index = usize::try_from(selection).unwrap_or(usize::MAX);
            if index < urls_r.borrow().len() {
                urls_r.borrow_mut().remove(index);
                list.set_items(
                    &urls_r
                        .borrow()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                );
            }
        }
    });
    let wiring_ok = clone_wiring(wiring);
    ok.on_click(move |_| {
        wiring_ok
            .persist
            .set_and_save("epg_urls", serde_json::json!(urls.borrow().clone()));
        reload_epg(&wiring_ok);
        status.set_label("EPG sources saved. Reloading guide...");
        manager_frame.hide();
    });
    cancel.on_click(move |_| manager.hide());
    manager.show(true);
}

/// One download tracked by the adapter (worker events arrive in
/// start order; the first unfinished row is the active one).
struct DownloadRow {
    name: String,
    destination: String,
    progress: String,
    done: bool,
}

/// Build a simple list dialog shell: frame, panel, list, status.
fn list_dialog(title: &str, list_label: &str) -> (Frame, Panel, ListBox, Status) {
    let title = tr(title);
    let frame = Frame::builder()
        .with_title(&title)
        .with_size(Size::new(680, 520))
        .build();
    frame.set_accessibility_label(&title);
    let panel = Panel::builder(&frame).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label(&tr(list_label));
    let status_label = StaticText::builder(&panel)
        .with_label("Dialog status")
        .build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("No items yet.")
        .build();
    details.set_accessibility_label("Dialog status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    (frame, panel, list, status)
}

/// The What's on Now dialog's controls and layout: filter
/// field, count label, the programme list, the dialog status
/// field, and the Play / Schedule Recording / Close row.
fn whats_on_layout(
    panel: Panel,
    list: ListBox,
    dstatus: Status,
) -> (TextCtrl, StaticText, Button, Button, Button) {
    let filter_label = StaticText::builder(&panel)
        .with_label("Filter (Tab):")
        .build();
    let filter = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    filter.set_accessibility_label("Filter programs");
    filter.set_accessibility_description(
        "Type to filter programmes by title or channel name. Enter moves to the programme list.",
    );
    let count_label = StaticText::builder(&panel).with_label("0 programs").build();
    let play = Button::builder(&panel).with_label("&Play").build();
    let schedule = Button::builder(&panel)
        .with_label("&Schedule Recording")
        .build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let layout = BoxSizer::builder(Orientation::Vertical).build();
    layout.add(&filter_label, 0, SizerFlag::All, 8);
    layout.add(&filter, 0, SizerFlag::Expand | SizerFlag::All, 8);
    layout.add(&count_label, 0, SizerFlag::All, 8);
    layout.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 8);
    layout.add(&dstatus.details, 0, SizerFlag::Expand | SizerFlag::All, 8);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&play, 0, SizerFlag::All, 4);
    buttons.add(&schedule, 0, SizerFlag::All, 4);
    buttons.add(&close, 0, SizerFlag::All, 4);
    layout.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(layout, true);
    (filter, count_label, play, schedule, close)
}

/// The What's on Now buttons: Play and Schedule Recording act
/// on the filtered row; Close hides the dialog.
#[allow(clippy::too_many_arguments)]
fn wire_whats_on_buttons(
    wiring: &Wiring,
    frame: Frame,
    list: ListBox,
    dstatus: Status,
    status: Status,
    play: Button,
    schedule: Button,
    close: Button,
    rows: &Rc<RefCell<Vec<(Channel, Programme)>>>,
    filtered: &Rc<RefCell<Vec<usize>>>,
) {
    let wiring_p = clone_wiring(wiring);
    let rows_p = Rc::clone(rows);
    let filtered_p = Rc::clone(filtered);
    play.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a programme to play first.");
            return;
        };
        let row_index = filtered_p
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied();
        if let Some((channel, _)) = row_index.and_then(|i| rows_p.borrow().get(i).cloned()) {
            play_channel(&wiring_p.player, &wiring_p.persist, &channel, status);
            frame.hide();
        }
    });
    let wiring_s = clone_wiring(wiring);
    let rows_s = Rc::clone(rows);
    let filtered_s = Rc::clone(filtered);
    schedule.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a programme to schedule first.");
            return;
        };
        let row_index = filtered_s
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied();
        if let Some((channel, programme)) = row_index.and_then(|i| rows_s.borrow().get(i).cloned())
        {
            add_dvr_job(&wiring_s, &channel, &programme, status);
        }
    });
    close.on_click(move |_| frame.hide());
}

/// File > What's on Now: every channel's current programme,
/// with Python's filter field (substring over programme title
/// or channel name), the "{count} programs" label, and Play /
/// Schedule Recording / Close.
fn open_whats_on_now(wiring: &Wiring, status: Status) {
    let mut rows: Vec<(Channel, Programme)> = Vec::new();
    for channel in wiring.browser.borrow().all_channels() {
        if let Some(programme) = current_programme(wiring, &channel) {
            rows.push((channel, programme));
        }
    }
    if rows.is_empty() {
        status.set_label("No guide information is loaded yet.");
        return;
    }
    let (frame, panel, list, dstatus) = list_dialog("What's on Now", "Currently airing programs");
    let (filter, count_label, play, schedule, close) = whats_on_layout(panel, list, dstatus);

    let rows = Rc::new(RefCell::new(rows));
    let filtered = Rc::new(RefCell::new(
        (0..rows.borrow().len()).collect::<Vec<usize>>(),
    ));
    let render = {
        let rows = Rc::clone(&rows);
        let filtered = Rc::clone(&filtered);
        move |list: ListBox, count_label: StaticText| {
            let rows = rows.borrow();
            let filtered = filtered.borrow();
            let labels: Vec<String> = filtered
                .iter()
                .filter_map(|index| rows.get(*index))
                .map(|(channel, p)| {
                    format!(
                        "{} — {} ({}-{})",
                        channel.name,
                        p.title,
                        local_hhmm(p.start),
                        local_hhmm(p.stop)
                    )
                })
                .collect();
            list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
            if !labels.is_empty() {
                list.set_selection(0, true);
            }
            count_label.set_label(&format!("{} programs", filtered.len()));
        }
    };
    render(list, count_label);
    dstatus.set_silent("Select a programme to play or schedule.");
    let apply_filter = {
        let rows = Rc::clone(&rows);
        let filtered = Rc::clone(&filtered);
        let render = render.clone();
        move |list: ListBox, count_label: StaticText, query: &str| {
            let query = query.trim().to_lowercase();
            let mut next: Vec<usize> = Vec::new();
            for (index, (channel, programme)) in rows.borrow().iter().enumerate() {
                if query.is_empty()
                    || programme.title.to_lowercase().contains(&query)
                    || channel.name.to_lowercase().contains(&query)
                {
                    next.push(index);
                }
            }
            if *filtered.borrow() != next {
                *filtered.borrow_mut() = next;
                render(list, count_label);
            }
        }
    };
    let filter_apply = apply_filter.clone();
    filter.bind_internal(EventType::TEXT, move |_| {
        filter_apply(list, count_label, &filter.get_value());
    });
    filter.bind_internal(EventType::TEXT_ENTER, move |_| {
        apply_filter(list, count_label, &filter.get_value());
        list.set_focus();
    });
    wire_whats_on_buttons(
        wiring, frame, list, dstatus, status, play, schedule, close, &rows, &filtered,
    );
    frame.show(true);
}

/// Format a millisecond duration as M:SS or H:MM:SS.
fn format_duration_ms(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// View > Continue Watching...: saved resume positions with Play
/// (the player applies the position) and Remove.
/// One Continue Watching row: a saved position, its fresh
/// catalogue match when one exists, and the availability state
/// (Python's fourth column: Not checked until a check runs).
struct CwRow {
    key: String,
    title: String,
    channel: Option<Channel>,
    position_ms: i64,
    length_ms: i64,
    updated: i64,
    availability: String,
}

fn cw_label(row: &CwRow) -> String {
    let watched = if row.updated > 0 {
        local_datetime(row.updated)
    } else {
        "Unknown time".to_string()
    };
    format!(
        "{} — {} of {} watched — last watched {} — {}",
        row.title,
        format_duration_ms(row.position_ms),
        format_duration_ms(row.length_ms),
        watched,
        row.availability
    )
}

fn render_cw_rows(rows: &[CwRow], list: ListBox) {
    let labels: Vec<String> = rows.iter().map(cw_label).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
}

fn open_continue_watching(wiring: &Wiring, status: Status) {
    let entries = wiring.persist.resume_entries();
    if entries.is_empty() {
        status.set_label("No saved playback positions.");
        return;
    }
    let channels = wiring.browser.borrow().all_channels();
    let mut rows: Vec<CwRow> = entries
        .iter()
        .map(|(key, entry)| {
            let channel = channels.iter().find(|c| channel_key(c) == *key).cloned();
            let title = if !entry.title.is_empty() {
                entry.title.clone()
            } else if let Some(channel) = &channel {
                channel.name.clone()
            } else {
                "Previously saved content (title unavailable)".to_string()
            };
            CwRow {
                key: key.clone(),
                title,
                channel,
                position_ms: entry.position_ms,
                length_ms: entry.length_ms,
                updated: entry.updated,
                availability: "Not checked".to_string(),
            }
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated));
    let (frame, panel, list, dstatus) = list_dialog("Continue Watching", "Programmes to resume");
    render_cw_rows(&rows, list);
    if !rows.is_empty() {
        list.set_selection(0, true);
    }
    dstatus.set_label(&format!("{} programmes to resume.", rows.len()));
    let resume = Button::builder(&panel).with_label("&Resume").build();
    let check = Button::builder(&panel)
        .with_label("C&heck Availability")
        .build();
    let remove = Button::builder(&panel).with_label("&Remove").build();
    let clear = Button::builder(&panel).with_label("Clear &All").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let rows = Rc::new(RefCell::new(rows));
    wire_continue_watching(
        wiring, frame, list, dstatus, status, &rows, resume, check, remove, clear, close,
    );
    frame.show(true);
}

/// Check Availability for one saved position (Python): show
/// "Checking saved content…", re-resolve the bookmark identity
/// against the current catalogue (never probe the stream),
/// then set the row's availability — Available with the fresh
/// channel, or Unavailable or ambiguous — speak it via the
/// dialog status, and keep the selection on the checked row.
fn cw_check_availability(
    wiring: &Wiring,
    rows: &Rc<RefCell<Vec<CwRow>>>,
    index: usize,
    list: ListBox,
    dstatus: Status,
) {
    dstatus.set_label("Checking saved content\u{2026}");
    let fresh = wiring.browser.borrow().all_channels();
    let mut guard = rows.borrow_mut();
    let Some(row) = guard.get_mut(index) else {
        return;
    };
    if let Some(channel) = fresh.iter().find(|c| channel_key(c) == row.key).cloned() {
        if row.title.starts_with("Previously saved content") {
            row.title.clone_from(&channel.name);
        }
        row.channel = Some(channel);
        row.availability = "Available".to_string();
    } else {
        row.channel = None;
        row.availability = "Unavailable or ambiguous".to_string();
    }
    let result = row.availability.clone();
    drop(guard);
    render_cw_rows(&rows.borrow(), list);
    list.set_selection(u32::try_from(index).unwrap_or(0), true);
    dstatus.set_label(&result);
}

/// The Continue Watching buttons: Resume, Check Availability
/// (Python: per selected row, identity re-resolution against
/// the current catalogue — the stream URL is never probed),
/// Remove and Clear All (both confirmed, Python's wording).
#[allow(clippy::too_many_arguments)]
fn wire_continue_watching(
    wiring: &Wiring,
    frame: Frame,
    list: ListBox,
    dstatus: Status,
    status: Status,
    rows: &Rc<RefCell<Vec<CwRow>>>,
    resume: Button,
    check: Button,
    remove: Button,
    clear: Button,
    close: Button,
) {
    let selected = |list: ListBox| -> Option<usize> {
        list.get_selection()
            .map(|index| usize::try_from(index).unwrap_or(usize::MAX))
    };
    let wiring_p = clone_wiring(wiring);
    let rows_p = Rc::clone(rows);
    resume.on_click(move |_| {
        let Some(index) = selected(list) else {
            return;
        };
        let channel = rows_p
            .borrow()
            .get(index)
            .and_then(|row| row.channel.clone());
        match channel {
            Some(channel) => {
                play_channel(&wiring_p.player, &wiring_p.persist, &channel, status);
                frame.hide();
            }
            None => dstatus.set_label("Unavailable or ambiguous"),
        }
    });
    let wiring_c = clone_wiring(wiring);
    let rows_c = Rc::clone(rows);
    check.on_click(move |_| {
        let Some(index) = selected(list) else {
            dstatus.set_label("Choose a saved position to check first.");
            return;
        };
        cw_check_availability(&wiring_c, &rows_c, index, list, dstatus);
    });
    let wiring_r = clone_wiring(wiring);
    let rows_r = Rc::clone(rows);
    remove.on_click(move |_| {
        let Some(index) = selected(list) else {
            return;
        };
        let dialog = MessageDialog::builder(
            &frame,
            "Remove this saved playback position?",
            "Continue Watching",
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        if dialog.show_modal() != ID_YES {
            return;
        }
        let removed = {
            let mut guard = rows_r.borrow_mut();
            (index < guard.len()).then(|| guard.remove(index))
        };
        if let Some(row) = removed {
            let mut entries = wiring_r.persist.resume_entries();
            entries.remove(&row.key);
            wiring_r.persist.save_resume(&entries);
            render_cw_rows(&rows_r.borrow(), list);
            dstatus.set_label("Removed from Continue Watching.");
        }
    });
    let wiring_a = clone_wiring(wiring);
    let rows_a = Rc::clone(rows);
    clear.on_click(move |_| {
        let dialog = MessageDialog::builder(
            &frame,
            "Remove all saved playback positions?",
            "Continue Watching",
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        if dialog.show_modal() != ID_YES {
            return;
        }
        rows_a.borrow_mut().clear();
        wiring_a
            .persist
            .save_resume(&std::collections::BTreeMap::new());
        render_cw_rows(&rows_a.borrow(), list);
        dstatus.set_label("No saved playback positions.");
    });
    close.on_click(move |_| frame.hide());
}

/// View > Recently Watched...: this session's playback history.
fn open_recent_channels(wiring: &Wiring, status: Status) {
    let history = wiring.player.borrow().history.clone();
    if history.is_empty() {
        status.set_label("No channels watched yet this session.");
        return;
    }
    let (frame, panel, list, dstatus) =
        list_dialog("Recently Watched", "Recently watched channels");
    let labels: Vec<String> = history.iter().map(|c| c.name.clone()).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    dstatus.set_label(&format!("{} recently watched channels.", history.len()));
    let play = Button::builder(&panel).with_label("&Play").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let history = Rc::new(RefCell::new(history));
    let wiring_p = clone_wiring(wiring);
    play.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        if let Some(channel) = history
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
        {
            play_channel(&wiring_p.player, &wiring_p.persist, channel, status);
            frame.hide();
        }
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The Reminders dialog: stored reminders with Delete.
fn open_reminders_dialog(wiring: &Wiring, main_status: Status) {
    let (frame, panel, list, dstatus) = list_dialog("Reminders", "Reminders");
    let render = |wiring: &Wiring, list: ListBox, dstatus: Status| {
        let rows = wiring.reminders.borrow();
        let labels: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{} {} — {} ({} min before)",
                    local_hhmm(r.start),
                    r.title,
                    r.channel,
                    r.lead
                )
            })
            .collect();
        list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        dstatus.set_label(&format!("{} reminders.", rows.len()));
    };
    render(wiring, list, dstatus);
    if wiring.reminders.borrow().is_empty() {
        main_status.set_label("No reminders set.");
    }
    let delete = Button::builder(&panel).with_label("&Delete").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_d = clone_wiring(wiring);
    delete.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a reminder to delete first.");
            return;
        };
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let mut rows = wiring_d.reminders.borrow_mut();
        if index < rows.len() {
            rows.remove(index);
            let saved = rows.clone();
            drop(rows);
            wiring_d.persist.save_reminders(&saved);
            render(&wiring_d, list, dstatus);
        }
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The Scheduled Recordings dialog: DVR jobs with Cancel.
fn open_scheduled_dialog(wiring: &Wiring, main_status: Status) {
    let (frame, panel, list, dstatus) = list_dialog("Scheduled Recordings", "Scheduled recordings");
    let render = |wiring: &Wiring, list: ListBox, dstatus: Status| {
        let dvr = wiring.dvr.borrow();
        let labels: Vec<String> = dvr
            .jobs()
            .iter()
            .map(|job| {
                format!(
                    "{} {} — {} [{}]",
                    job.start_at, job.title, job.channel_name, job.status
                )
            })
            .collect();
        list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        dstatus.set_label(&format!("{} scheduled recordings.", dvr.jobs().len()));
    };
    render(wiring, list, dstatus);
    if wiring.dvr.borrow().jobs().is_empty() {
        main_status.set_label("No scheduled recordings.");
    }
    let cancel_job = Button::builder(&panel)
        .with_label("&Cancel Recording")
        .build();
    let delete_job = Button::builder(&panel).with_label("&Delete").build();
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_c = clone_wiring(wiring);
    cancel_job.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a recording to cancel first.");
            return;
        };
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let id = wiring_c
            .dvr
            .borrow()
            .jobs()
            .get(index)
            .map(|job| job.id.clone());
        if let Some(id) = id {
            let mut dvr = wiring_c.dvr.borrow_mut();
            if dvr.cancel_job(&id) {
                dvr.save();
                drop(dvr);
                // Stop the capture too: a canceled job leaves the
                // Recording state, so the scheduler will never
                // emit its Stop action — an in-flight or running
                // capture must be halted here (issue #130).
                wiring_c.recordings.borrow_mut().stop(&format!("dvr:{id}"));
                wiring_c.dvr_resolve.borrow_mut().remove(&id);
                wiring_c.pending_resolves.borrow_mut().retain(
                    |_, pending| !matches!(pending, PendingResolve::Dvr(job) if job == &id),
                );
                render(&wiring_c, list, dstatus);
            }
        }
    });
    let wiring_d = clone_wiring(wiring);
    delete_job.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a finished recording to delete first.");
            return;
        };
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let id = wiring_d
            .dvr
            .borrow()
            .jobs()
            .get(index)
            .map(|job| job.id.clone());
        if let Some(id) = id {
            if wiring_d.dvr.borrow_mut().remove_job(&id) {
                render(&wiring_d, list, dstatus);
            } else {
                dstatus.set_label("Only finished recordings can be deleted.");
            }
        }
    });
    let wiring_r = clone_wiring(wiring);
    refresh.on_click(move |_| {
        render(&wiring_r, list, dstatus);
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The Catch-up dialog for one channel: past programmes with Play
/// and Download.
fn open_catchup_dialog(wiring: &Wiring, channel: &Channel, main_status: Status) {
    if !iptv_core::catchup::channel_has_catchup(channel) {
        main_status.set_label("This channel does not provide catch-up streaming.");
        return;
    }
    let now = now_epoch();
    let programmes: Vec<Programme> = guide_programmes(wiring, channel)
        .into_iter()
        .filter(|p| {
            p.stop <= now && iptv_core::catchup::within_catchup_window(channel, p.start, now)
        })
        .collect();
    if programmes.is_empty() {
        main_status.set_label("No catch-up programmes available for this channel.");
        return;
    }
    let (frame, panel, list, dstatus) = list_dialog(
        &format!("Catch-up — {}", channel.name),
        "Catch-up programmes",
    );
    let labels: Vec<String> = programmes
        .iter()
        .map(|p| format!("{}-{} {}", local_hhmm(p.start), local_hhmm(p.stop), p.title))
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    dstatus.set_label(&format!("{} catch-up programmes.", programmes.len()));
    let play = Button::builder(&panel).with_label("&Play").build();
    let download = Button::builder(&panel).with_label("&Download").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let programmes = Rc::new(RefCell::new(programmes));
    let wiring_p = clone_wiring(wiring);
    let programmes_p = Rc::clone(&programmes);
    let channel_p = channel.clone();
    play.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        if let Some(programme) = programmes_p
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
        {
            play_catchup_for(&wiring_p, &channel_p, programme, main_status);
        }
    });
    let wiring_d = clone_wiring(wiring);
    let programmes_d = Rc::clone(&programmes);
    let channel_d = channel.clone();
    download.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        let Some(programme) = programmes_d
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .cloned()
        else {
            return;
        };
        start_catchup_download(&wiring_d, &channel_d, &programme, dstatus);
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// Resolve a catch-up programme URL and queue its download.
fn start_catchup_download(
    wiring: &Wiring,
    channel: &Channel,
    programme: &Programme,
    status: Status,
) {
    let now = now_epoch();
    match iptv_core::catchup::resolve_show_url(channel, programme.start, programme.stop, now) {
        Ok(
            iptv_core::catchup::ShowResolution::Catchup(url)
            | iptv_core::catchup::ShowResolution::Live(url),
        ) => begin_catchup_download(wiring, channel, programme, url, status),
        Ok(iptv_core::catchup::ShowResolution::StalkerLive) => {
            let Some(key) = channel.unknown_attributes.get("provider-id").cloned() else {
                status.set_label("This Stalker channel has no provider account configured.");
                return;
            };
            dispatch_resolve(
                wiring,
                PendingResolve::Download {
                    channel: channel.clone(),
                    programme: programme.clone(),
                },
                key,
                channel.unknown_attributes.clone(),
            );
            status.set_label("Resolving the download link...");
        }
        Ok(iptv_core::catchup::ShowResolution::StalkerCatchup {
            start_text,
            duration_minutes,
        }) => {
            let Some(key) = channel.unknown_attributes.get("provider-id").cloned() else {
                status.set_label("This Stalker channel has no provider account configured.");
                return;
            };
            dispatch_resolve_catchup(
                wiring,
                PendingResolve::Download {
                    channel: channel.clone(),
                    programme: programme.clone(),
                },
                key,
                channel.unknown_attributes.clone(),
                start_text,
                duration_minutes,
            );
            status.set_label("Resolving the download link...");
        }
        Err(error) => status.set_label(&error),
    }
}

/// Start a catch-up download once its URL is known (directly, or
/// after a portal resolution).
fn begin_catchup_download(
    wiring: &Wiring,
    channel: &Channel,
    programme: &Programme,
    url: String,
    status: Status,
) {
    let dir = recordings_dir(&wiring.persist);
    let _ = std::fs::create_dir_all(&dir);
    let name = iptv_core::recording::sanitize_filename(&programme.title);
    let destination = dir.join(format!("{name}.ts"));
    wiring
        .download_worker
        .start(iptv_core::downloads::DownloadRequest {
            url,
            headers: iptv_core::downloads::channel_headers(channel),
            destination: destination.clone(),
        });
    wiring.downloads.borrow_mut().push(DownloadRow {
        name: programme.title.clone(),
        destination: destination.display().to_string(),
        progress: "Starting...".to_string(),
        done: false,
    });
    wiring.downloads_active.set(true);
    assert!(wiring.timer.start(50, false));
    status.set_label(&format!("Downloading {}...", programme.title));
}

/// View > Show Downloads: tracked downloads with progress.
fn open_downloads_window(wiring: &Wiring, status: Status) {
    let rows = wiring.downloads.borrow();
    if rows.is_empty() {
        status.set_label("No downloads yet. Downloads start from a channel's Catch-up dialog.");
        return;
    }
    let (frame, panel, list, dstatus) = list_dialog("Downloads", "Downloads");
    let labels: Vec<String> = rows
        .iter()
        .map(|row| format!("{} — {} — {}", row.name, row.progress, row.destination))
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    dstatus.set_label(&format!("{} downloads.", rows.len()));
    drop(rows);
    let cancel = Button::builder(&panel)
        .with_label("&Cancel Download")
        .build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_c = clone_wiring(wiring);
    cancel.on_click(move |_| {
        wiring_c.download_worker.cancel();
        dstatus.set_label("Cancelling the active download...");
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// File > Account Info: pick an account and fetch its info report
/// from the provider; the report lands in the dialog's text.
fn open_account_info(wiring: &Wiring, status: Status) {
    let (_, accounts) = configured_entries(wiring);
    if accounts.is_empty() {
        status.set_label("No provider accounts are configured.");
        return;
    }
    let (frame, panel, list, dstatus) = list_dialog("Account Info", "Accounts");
    let labels: Vec<String> = accounts.iter().map(ProviderAccount::describe).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    let fetch = Button::builder(&panel).with_label("&Fetch Info").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let text_slot = Rc::clone(&wiring.account_text);
    let accounts = Rc::new(RefCell::new(accounts));
    let wiring_f = clone_wiring(wiring);
    fetch.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            return;
        };
        let id = accounts
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .and_then(ProviderAccount::provider_id);
        let Some(id) = id else {
            dstatus.set_label("This account has no provider id.");
            return;
        };
        *text_slot.borrow_mut() = Some(dstatus.details);
        wiring_f.provider_worker.account(id);
        wiring_f.provider_pending.set(true);
        assert!(wiring_f.timer.start(50, false));
        dstatus.set_label("Fetching account info...");
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// Drain download events into the tracked rows and announce
/// completions on the main status line.
fn poll_downloads(wiring: &Wiring, status: Status) {
    while let Some(event) = wiring.download_worker.try_recv() {
        let mut rows = wiring.downloads.borrow_mut();
        let Some(row) = rows.iter_mut().find(|row| !row.done) else {
            continue;
        };
        match event {
            iptv_core::downloads::DownloadEvent::Progress { written, total } => {
                row.progress = match total {
                    Some(total) if total > 0 => {
                        format!("{}% of {}", written * 100 / total, format_bytes(total))
                    }
                    _ => format!("{} downloaded", format_bytes(written)),
                };
            }
            iptv_core::downloads::DownloadEvent::Finished(Ok(bytes)) => {
                row.progress = format!("Done ({})", format_bytes(bytes));
                row.done = true;
                let name = row.name.clone();
                drop(rows);
                status.set_label(&format!("Download finished: {name}"));
            }
            iptv_core::downloads::DownloadEvent::Finished(Err(error)) => {
                row.progress = format!("Failed: {error}");
                row.done = true;
            }
        }
    }
    let active = wiring.downloads.borrow().iter().any(|row| !row.done);
    wiring.downloads_active.set(active);
}

/// Human-readable byte counts for download progress.
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

/// Open the User Guide at a specific help topic (per-control F1).
fn open_guide_window_at(wiring: &Wiring, status: Status, topic: &str) {
    let ui = ensure_guide_ui(&wiring.guide_ui);
    let language = wiring.i18n.resolved_language();
    let (guide, section) = iptv_core::guide::open_topic(Some(&language), Some(topic), None);
    show_guide(&ui, guide);
    if let Some(section) = section {
        move_caret(&ui, &section);
    }
    ui.frame.show(true);
    ui.frame.raise();
    status.set_label("User Guide opened.");
}

/// Attach F1 help to one main-window control: F1 opens the guide
/// at the control's topic, as Python's `set_help_topic` does.
fn bind_f1_help<T>(control: &T, wiring: &Wiring, topic: &'static str, status: Status)
where
    T: wxdragon::event::WxEvtHandler,
{
    let wiring = clone_wiring(wiring);
    control.bind_internal(EventType::KEY_DOWN, move |event| {
        if event.get_key_code() == Some(340) {
            open_guide_window_at(&wiring, status, topic);
        }
        event.skip(true);
    });
}

/// Main-window keys that have no menu item (Python dispatches them
/// as accelerators): volume up/down drive the built-in player in
/// steps of 2, and Record toggles recording of the selected channel.
/// wxdragon exposes no accelerator table, so the binding rides the
/// focusable controls' `KEY_DOWN` events, next to the F1 bindings.
fn bind_main_hotkeys<T>(control: &T, wiring: &Wiring, controls: Controls, status: Status)
where
    T: wxdragon::event::WxEvtHandler,
{
    let wiring = clone_wiring(wiring);
    control.bind_internal(EventType::KEY_DOWN, move |event| {
        dispatch_main_hotkey(&wiring, controls, status, event);
        event.skip(true);
    });
}

fn dispatch_main_hotkey(
    wiring: &Wiring,
    controls: Controls,
    status: Status,
    event: wxdragon::event::Event,
) {
    let Some(label) = key_event_label(event) else {
        return;
    };
    let config = serde_json::Value::Object(wiring.persist.config.borrow().raw().clone());
    let keys = iptv_core::shortcuts::effective(&config, "main");
    let action = ["volume_up", "volume_down", "record"]
        .into_iter()
        .find(|name| keys.get(*name) == Some(&label));
    match action {
        Some("volume_up" | "volume_down") => {
            let delta = if action == Some("volume_up") { 2 } else { -2 };
            let player = Rc::clone(&wiring.player);
            let current = player
                .borrow()
                .window
                .as_ref()
                .map(|win| win.volume.get_value());
            if let Some(current) = current {
                let value = (current + delta).clamp(0, 100);
                player_set_volume(&player, value);
                status.set_label(&format!("Volume {value} percent."));
            }
        }
        Some("record") => {
            if let Some(channel) = selected_channel(&wiring.browser, &wiring.model, controls.list) {
                toggle_recording(
                    &wiring.recordings,
                    &wiring.buffers,
                    &wiring.persist,
                    &channel,
                    status,
                );
            } else {
                status.set_label("Select a channel to record first.");
            }
        }
        _ => {}
    }
}

/// Options > Keyboard Shortcuts...: list every command with its
/// effective binding; Edit changes one (validated by the core
/// engine with the Python error strings); Reset restores defaults.
fn open_shortcuts_editor(wiring: &Wiring, status: Status) {
    let (frame, panel, list, dstatus) = list_dialog("Keyboard Shortcuts", "Commands");
    let render = |wiring: &Wiring, list: ListBox| {
        let config = serde_json::Value::Object(wiring.persist.config.borrow().raw().clone());
        let rows = iptv_core::shortcuts::searchable_commands(&config, "");
        let labels: Vec<String> = rows
            .iter()
            .map(|(context, _, label, binding, _display)| {
                let where_ = if context == "player" {
                    "Built-in Player"
                } else {
                    "Main window"
                };
                if binding.is_empty() {
                    format!("{where_} — {label} — (not assigned)")
                } else {
                    format!("{where_} — {label} — {binding}")
                }
            })
            .collect();
        list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        labels.len()
    };
    let count = render(wiring, list);
    dstatus.set_label(&format!(
        "{count} commands. Edit changes a binding; menus pick up changes on restart."
    ));
    let edit = Button::builder(&panel).with_label("&Edit Binding").build();
    let reset = Button::builder(&panel).with_label("&Reset All").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_e = clone_wiring(wiring);
    edit.on_click(move |_| {
        let Some(index) = list.get_selection() else {
            dstatus.set_label("Choose a command to edit first.");
            return;
        };
        let config = serde_json::Value::Object(wiring_e.persist.config.borrow().raw().clone());
        let rows = iptv_core::shortcuts::searchable_commands(&config, "");
        let Some((context, action, label, binding, _display)) =
            rows.get(usize::try_from(index).unwrap_or(usize::MAX))
        else {
            return;
        };
        let dialog = TextEntryDialog::builder(
            &frame,
            &format!("Shortcut for {label} (blank restores the default):"),
            "Edit Shortcut",
        )
        .with_default_value(binding)
        .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        let Some(value) = dialog.get_value() else {
            return;
        };
        let mut raw = wiring_e.persist.config.borrow().raw().clone();
        match iptv_core::shortcuts::set_shortcut(&mut raw, context, action, value.trim()) {
            Ok(()) => {
                let shortcuts = raw
                    .get("shortcuts")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                wiring_e.persist.set_and_save("shortcuts", shortcuts);
                render(&wiring_e, list);
                dstatus.set_label(&format!("Shortcut for {label} updated."));
            }
            Err(error) => dstatus.set_label(&error),
        }
    });
    let wiring_r = clone_wiring(wiring);
    reset.on_click(move |_| {
        wiring_r
            .persist
            .set_and_save("shortcuts", serde_json::json!({}));
        render(&wiring_r, list);
        dstatus.set_label("All shortcuts reset to defaults.");
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
    let _ = status;
}

/// Options > Preferred Audio Track...: user keywords plus the
/// audio-description preference (slice 9's selection engine).
fn open_audio_prefs(wiring: &Wiring, status: Status) {
    let frame = Frame::builder()
        .with_title("Preferred Audio Track")
        .with_size(Size::new(560, 360))
        .build();
    frame.set_accessibility_label("Preferred Audio Track");
    let panel = Panel::builder(&frame).build();
    let caption = StaticText::builder(&panel)
        .with_label("Preferred track keywords (comma separated)")
        .build();
    let _ = caption;
    let keywords = TextCtrl::builder(&panel)
        .with_value(
            &wiring
                .persist
                .config
                .borrow()
                .string_list("preferred_audio_tracks")
                .join(", "),
        )
        .build();
    keywords.set_accessibility_label("Preferred track keywords");
    let ad_check = CheckBox::builder(&panel)
        .with_label("Prefer audio description tracks")
        .build();
    ad_check.set_value(
        wiring
            .persist
            .config
            .borrow()
            .bool_or("prefer_audio_description", false),
    );
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let wiring_ok = clone_wiring(wiring);
    ok.on_click(move |_| {
        let words: Vec<String> = keywords
            .get_value()
            .split(',')
            .map(|word| word.trim().to_string())
            .filter(|word| !word.is_empty())
            .collect();
        wiring_ok
            .persist
            .set_and_save("preferred_audio_tracks", serde_json::json!(words));
        wiring_ok.persist.set_and_save(
            "prefer_audio_description",
            serde_json::json!(ad_check.get_value()),
        );
        status.set_label("Audio track preferences saved.");
        frame.hide();
    });
    cancel.on_click(move |_| frame.hide());
    frame.show(true);
}

/// Help > What's New: the current version's notes.
fn open_whats_new(status: Status) {
    let (frame, _panel, _list, dstatus) = list_dialog("What's New", "Changes");
    dstatus.details.set_value(WHATS_NEW_TEXT);
    dstatus.set_label(&format!(
        "What's new in version {}",
        env!("CARGO_PKG_VERSION")
    ));
    frame.show(true);
    let _ = status;
}

/// The What's New text for the running version (updated per release).
const WHATS_NEW_TEXT: &str = "Version 0.4.1\n\nLess talk. Channel rows now carry the guide the way the Python app does — channel name first, then the current and next programmes after it — and moving through the list no longer speaks the guide out loud on every step; the description panel still fills in silently.\n\nVersion 0.4.0\n\nSigned updates. Check for Updates now uses the ship-shape updater, the same signed updater as BlindRadio: update packages are cryptographically signed and verified before anything installs, then install and relaunch on their own (Windows and Linux; macOS opens the verified disk image for a manual install). Releases before this one can still update forward onto it.\n\nVersion 0.3.4\n\nFix: large playlists load promptly at startup. The playlist now loads first and the programme guide follows once it settles, instead of the two downloads racing; the guide database write no longer freezes the window; and the channel list says Loading playlist while a load is in flight instead of claiming no playlist is loaded.\n\nVersion 0.3.3\n\nFix: streams play again. The packages now bundle libVLC (the built-in player engine) beside the app on Windows, Linux, and macOS, so playback works with no separate VLC install. If libVLC still cannot load, pressing play now says so instead of failing silently.\n\nVersion 0.3.2\n\nFix: large playlists and guides now load. The HTTP library silently capped downloads at 10 MB, so a big provider playlist (for example a 28 MB, 115,000-channel list) failed to add and its EPG could never load. Playlist, EPG, Xtream, and Stalker fetches now take the full body.\n\nVersion 0.3.1\n\nFix: the Windows build no longer shows a wxWidgets debug dialog (\"Can't expand/collapse hidden root node!\") at startup or when the category list rebuilds. Nothing else changes from 0.3.0.\n\nVersion 0.3.0\n\nMain window: the layout now matches the Python client exactly (Playlist view and Categories on the left; Search, Channels, description and Stream URL on the right), and the menu-less shortcuts from the Python client work here: Ctrl+Up/Down volume, Ctrl+Shift+R record, Ctrl+D favorite and the rest of the documented set.\n\nPlayer: a Playback menu in Python's order, a Sleep Timer with an optional volume fade-out, and an Audio Output Device dialog that lists the real libVLC devices (System default first) and applies the saved choice at playback start. Announcement levels now gate player speech exactly as Python's do (None, Errors only, Important events, Detailed status).\n\nLocal Live Buffer: live TV in the built-in player can be buffered locally (off by default; set it up from the player's Playback menu), which enables pausing live TV, Rewind Live TV and Return to Live. A recording of a buffered channel reads the buffer instead of opening a second provider connection.\n\nDialogs: What's on Now has its filter box with a program count; Continue Watching gains Check Availability, which re-resolves each saved position against the current playlists and reports Available or Unavailable per row. Settings backup and restore now use Python's password-protected encrypted format, so backups interchange with the Python client both ways.\n\nMenus and shell: the Media Player menu lists all 32 players the Python client offers, with its exact per-platform program lookup. The tray icon builds Python's popup fresh on every right-click (Restore, Player Controls, Stop Recording(s) while recording, Exit) and left-click restores the window. The Recordings menu's minimum-free-space reserve and schedule padding use Python's values.";

/// Run the platform shutdown (factored out of the recordings poll
/// for the countdown dialog).
fn execute_shutdown(status: Status) {
    let system = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    match recording::shutdown_computer(system, &root) {
        Ok(_) => status.set_label("All recordings finished. Shutting down."),
        Err(error) => status.set_label(&format!("Shutdown failed: {error}")),
    }
}

/// The shutdown countdown window: 30 seconds to cancel before the
/// computer shuts down after recordings (Python's dialog).
fn open_shutdown_countdown(status: Status) {
    let frame = Frame::builder()
        .with_title("Shut Down After Recordings")
        .with_size(Size::new(440, 240))
        .build();
    frame.set_accessibility_label("Shut Down After Recordings");
    let panel = Panel::builder(&frame).build();
    let label = StaticText::builder(&panel)
        .with_label("All recordings have finished.\n\nThe computer will shut down by itself. Seconds remaining: 60")
        .build();
    let cancel = Button::builder(&panel)
        .with_label("&Cancel Shutdown")
        .build();
    let now_button = Button::builder(&panel).with_label("Shut Down &Now").build();
    let remaining = Rc::new(std::cell::Cell::new(60_i64));
    let timer = Rc::new(Timer::new(&frame));
    let timer_tick = Rc::clone(&timer);
    let remaining_tick = Rc::clone(&remaining);
    timer.on_tick(move |_| {
        let next = remaining_tick.get() - 1;
        remaining_tick.set(next);
        if next <= 0 {
            timer_tick.stop();
            execute_shutdown(status);
            frame.hide();
        } else {
            label.set_label(&format!(
                "All recordings have finished.\n\nThe computer will shut down by itself. Seconds remaining: {next}"
            ));
        }
    });
    let timer_cancel = Rc::clone(&timer);
    cancel.on_click(move |_| {
        timer_cancel.stop();
        status.set_label("Shutdown cancelled.");
        frame.hide();
    });
    let timer_now = Rc::clone(&timer);
    now_button.on_click(move |_| {
        timer_now.stop();
        execute_shutdown(status);
        frame.hide();
    });
    assert!(timer.start(1000, false));
    frame.show(true);
    cancel.set_focus();
}

/// Display text for a category row. The sentinel is never decorated; the
/// Favorites count follows the Python reference's group-list label.
fn category_display(category: &Category, favorite_count: usize) -> String {
    match category {
        Category::Favorites if favorite_count > 0 => {
            format!("Favorites ({favorite_count})")
        }
        _ => category.label().to_owned(),
    }
}

/// Refresh the category control from the browser's current playlist without
/// moving keyboard focus: rebuild rows only when the set changed, and keep
/// the selection on the active category.
fn sync_category_control(
    control: TreeCtrl,
    state: &RefCell<Vec<Category>>,
    browser: &RefCell<BrowserWorker>,
) {
    let browser = browser.borrow();
    let current = browser.categories();
    let favorite_count = browser.favorite_count();
    let favorites_active = browser.active_category() == &Category::Favorites;
    if *state.borrow() != current || favorites_active {
        // Rebuild the tree rows (the Favorites count can change the
        // label without a set change, as with the old list).
        if let Some(root) = control.get_root_item() {
            control.delete_children(&root);
            for category in &current {
                control.append_item(
                    &root,
                    &category_display(category, favorite_count),
                    None,
                    None,
                );
            }
            // See category_controls: never expand the hidden root.
        }
        if *state.borrow() != current {
            *state.borrow_mut() = current;
        }
    }
    let active = browser.active_category().clone();
    if let Some(position) = state.borrow().iter().position(|item| item == &active) {
        if let Some(item) = tree_item_at(control, position) {
            let selected = control.get_selection();
            let already = selected
                .as_ref()
                .is_some_and(|sel| u64::from(sel) == u64::from(&item));
            if !already {
                control.select_item(&item);
            }
        }
    }
}

fn publish_results(
    list: ListCtrl,
    status: Status,
    model: &RefCell<SearchResults>,
    results: SearchResults,
) {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    let (restored_selected, restored_focused, keep_selected, keep_focused) = {
        let previous = model.borrow();
        (
            results.restore_row(&previous, selected),
            results.restore_row(&previous, focused),
            selected.is_some_and(|row| results.retains_row(&previous, row)),
            focused.is_some_and(|row| results.retains_row(&previous, row)),
        )
    };
    let count = i64::try_from(results.len()).unwrap_or(i64::MAX);
    // MSAA can synchronously query rows during SetItemCount. On shrink the old
    // model must remain available; on grow the new model must already exist.
    for (row, keep, state) in [
        (selected, keep_selected, ListItemState::Selected),
        (focused, keep_focused, ListItemState::Focused),
    ] {
        if !keep {
            if let Some(row) = row.and_then(|row| i64::try_from(row).ok()) {
                list.set_item_state(row, ListItemState::None, state);
            }
        }
    }
    let old_count = i64::from(list.get_item_count());
    if count < old_count {
        list.set_item_count(count);
    }
    let message = results.status();
    let previous = model.replace(results);
    if count > old_count {
        list.set_item_count(count);
    }
    if let Some(row) = restored_selected
        .filter(|_| !keep_selected)
        .and_then(|row| i64::try_from(row).ok())
    {
        list.set_item_state(row, ListItemState::Selected, ListItemState::Selected);
        list.ensure_visible(row);
    }
    if let Some(row) = restored_focused
        .filter(|_| !keep_focused)
        .and_then(|row| i64::try_from(row).ok())
    {
        list.set_item_state(row, ListItemState::Focused, ListItemState::Focused);
    }
    if count > 0 {
        list.refresh_items(0, count - 1);
    }
    // Search/category completions update visible status without repeating a
    // live announcement already made for the deliberate action.
    status.set_silent(&message);
    list.set_accessibility_description(&message);
    std::thread::spawn(move || drop(previous));
}

fn channel_text(
    model: &SearchResults,
    suffixes: &(i64, usize, std::collections::HashMap<usize, String>),
    index: i64,
    column: i32,
) -> String {
    if column != 0 {
        return String::new();
    }
    let Some(row) = usize::try_from(index).ok() else {
        return String::new();
    };
    let Some(channel) = model.channel(row) else {
        return String::new();
    };
    let name = if channel.name.is_empty() {
        // Stream URLs can contain credentials; never use them as row labels.
        "Unnamed channel".to_owned()
    } else {
        channel.name.clone()
    };
    // Python's row: the channel name first, the guide after it —
    // "Name — Now title (HH:MM–HH:MM) — Next: title (HH:MM)".
    match model
        .playlist_index(row)
        .and_then(|index| suffixes.2.get(&index))
    {
        Some(suffix) => format!("{name}{suffix}"),
        None => name,
    }
}

/// One channel's row suffix from its now/next programmes
/// (Python's `_programme_label` shapes).
fn epg_suffix(current: Option<&Programme>, next: Option<&Programme>) -> String {
    use std::fmt::Write as _;
    let mut suffix = String::new();
    if let Some(p) = current {
        let _ = write!(
            suffix,
            " — {} ({}–{})",
            p.title,
            local_hhmm(p.start),
            local_hhmm(p.stop)
        );
    }
    if let Some(p) = next {
        let _ = write!(suffix, " — Next: {} ({})", p.title, local_hhmm(p.start));
    }
    suffix
}

/// Rebuild the row suffixes for the current playlist + guide.
fn refresh_epg_suffixes(wiring: &Wiring) {
    let now = now_epoch();
    let playlist_ptr = wiring.model.borrow().playlist_ptr();
    let mut map = std::collections::HashMap::new();
    if let Some((guide, indexes)) = wiring.epg.borrow().as_ref() {
        let known: std::collections::HashSet<String> =
            guide.channels.iter().map(|c| c.id.to_lowercase()).collect();
        for (index, channel) in wiring.browser.borrow().all_channels().iter().enumerate() {
            let Some(guide_id) = match_guide_channel(channel, indexes, Some(&known)) else {
                continue;
            };
            let (current, next) = guide.now_next(&guide_id, now);
            let suffix = epg_suffix(current.as_ref(), next.as_ref());
            if !suffix.is_empty() {
                map.insert(index, suffix);
            }
        }
    }
    *wiring.epg_suffixes.borrow_mut() = (now, playlist_ptr, map);
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_core::{search::SearchWorker, BrowseModel};
    use std::sync::Arc;

    fn no_suffixes() -> (i64, usize, std::collections::HashMap<usize, String>) {
        (0, 0, std::collections::HashMap::new())
    }

    #[test]
    fn empty_list_has_no_placeholder_or_stale_accessible_rows() {
        let model = SearchResults::default();
        let suffixes = no_suffixes();
        for index in [-1, 0, 1, i64::MAX] {
            assert_eq!(channel_text(&model, &suffixes, index, 0), "");
            assert_eq!(channel_text(&model, &suffixes, index, 1), "");
        }
    }

    #[test]
    fn row_text_says_the_guide_after_the_channel_name() {
        let mut suffixes = no_suffixes();
        suffixes.2.insert(
            0,
            " — News Hour (18:00–19:00) — Next: Movie (19:00)".to_owned(),
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../iptv-core/tests/fixtures/local.m3u");
        let worker = SearchWorker::default();
        worker.submit(
            Arc::new(BrowseModel::open_local(&path).unwrap()),
            String::new(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let model = loop {
            if let Some(result) = worker.try_latest() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "search timed out");
            std::thread::yield_now();
        };
        assert_eq!(
            channel_text(&model, &suffixes, 0, 0),
            "ABC East HD — News Hour (18:00–19:00) — Next: Movie (19:00)"
        );
        // A channel without guide data keeps the bare name.
        assert_eq!(channel_text(&model, &suffixes, 1, 0), "映画, HD");
    }

    #[test]
    fn suffix_shape_matches_python() {
        let now = Programme {
            channel_id: "c".to_owned(),
            title: "News Hour".to_owned(),
            description: String::new(),
            start: 0,
            stop: 3600,
        };
        let next = Programme {
            title: "Movie".to_owned(),
            start: 3600,
            stop: 7200,
            ..now.clone()
        };
        let suffix = epg_suffix(Some(&now), Some(&next));
        assert!(suffix.starts_with(" — News Hour ("), "{suffix}");
        assert!(suffix.contains(" — Next: Movie ("), "{suffix}");
        assert_eq!(epg_suffix(None, None), "");
    }

    #[test]
    fn populated_rows_expose_names_in_source_order_without_stream_credentials() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../iptv-core/tests/fixtures/local.m3u");
        let worker = SearchWorker::default();
        worker.submit(
            Arc::new(BrowseModel::open_local(&path).unwrap()),
            String::new(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let model = loop {
            if let Some(result) = worker.try_latest() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "search timed out");
            std::thread::yield_now();
        };
        let suffixes = no_suffixes();
        assert_eq!(channel_text(&model, &suffixes, 0, 0), "ABC East HD");
        assert_eq!(channel_text(&model, &suffixes, 1, 0), "映画, HD");
        assert_eq!(
            channel_text(&model, &suffixes, 2, 0),
            "Bob's Burgers, Season 1"
        );
        assert_eq!(channel_text(&model, &suffixes, 3, 0), "Unnamed channel");
        assert_eq!(channel_text(&model, &suffixes, 4, 0), "");
        assert_eq!(channel_text(&model, &suffixes, -1, 0), "");
        assert_eq!(channel_text(&model, &suffixes, 0, 1), "");
    }

    #[test]
    fn category_labels_use_sentinels_and_python_style_favorite_counts() {
        assert_eq!(category_display(&Category::All, 0), "All Channels");
        assert_eq!(category_display(&Category::Favorites, 0), "Favorites");
        assert_eq!(category_display(&Category::Favorites, 3), "Favorites (3)");
        assert_eq!(
            category_display(&Category::Group("News".to_owned()), 3),
            "News"
        );
    }

    #[test]
    fn actions_apply_to_the_selected_row_then_the_focused_row() {
        assert_eq!(pick_row(Some(2), Some(5)), Some(2));
        assert_eq!(pick_row(None, Some(5)), Some(5));
        assert_eq!(pick_row(None, None), None);
    }
}

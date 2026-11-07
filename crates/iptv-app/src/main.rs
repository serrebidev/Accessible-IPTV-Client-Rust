// No console window beside the app on Windows release builds
// (the Python client is a GUI-subsystem program too); debug
// builds keep the console for development output. Same line
// as BlindRadio and Blind Designer (validation 0096).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod buffer;
mod relay;

use iptv_core::guide::Guide;
use iptv_core::i18n::I18n;
use iptv_core::{
    account_info::Account,
    bookmarks::Bookmarks,
    browser::{BrowseEvent, BrowserWorker},
    cast::{CastDevice, CastEvent, CastWorker},
    categories::Category,
    config::{self, Config},
    dvr::{channel_from_snapshot, DvrAction, DvrSchedule},
    epg::{
        airing_index, epg_info_message, epg_row_label, epg_time_range, hash_epg_sources,
        local_datetime, local_hhmm, match_guide_channel, merged_epg_sources,
        should_auto_import_epg, should_run_epg_search, EpgSnapshot, EpgWorker, NowNext, Programme,
        SuffixJob, SuffixWorker, SEARCH_EPG_RESULT_LIMIT,
    },
    favorites::{channel_key, remember_recent, FavoriteSet, Toggle},
    playback::{
        candidate_executables, detect_from_path, external_argv, player_choice, EngineAction,
        EngineEvent, LaunchGate, PlaybackRequest, PlaybackSession, PlayerChoice, SessionState,
    },
    providers::{
        account_form_error, merge_account_fields, random_mag_mac, sanitize_mac, ProviderAccount,
        ProviderEvent, ProviderWorker,
    },
    recording::{self, RecordingEngine},
    reminders::{normalize_reminders, Reminder, ReminderStatus},
    search::SearchResults,
    sources::{source_display_label, PlaylistSource},
    speech::{CueRoute, CueSpeech, DevicePolicy, SpeechBackend},
    subtitles::{active as active_cue, load_cues, started as started_cue, Cue, ReviewCursor},
    vod::{item_details, VodCatalog, VodEvent, VodItem, VodKind, VodSearchHit, VodWorker},
    Channel,
};
use std::{
    cell::RefCell, collections::BTreeMap, path::PathBuf, process::Command, rc::Rc, sync::Arc,
};
use wxdragon::event::EventType;
use wxdragon::prelude::*;
use wxdragon::timer::Timer;
use wxdragon::widgets::media_ctrl::SeekMode;

/// One copy per user (S01, Python's `single_instance`): take the
/// instance lock, or hand over to the running copy — often hidden in
/// the tray — and exit. The returned guard lives until the event
/// loop ends; without it a second scheduler would fight the first
/// for recordings, config saves and provider streams.
fn acquire_instance() -> Option<iptv_core::instance::InstanceGuard> {
    let instance_dir = config::user_config_dir();
    let guard = match iptv_core::instance::acquire(&instance_dir) {
        Some(guard) => Some(guard),
        None => {
            match iptv_core::instance::hand_over(&instance_dir, std::time::Duration::from_secs(15))
            {
                iptv_core::instance::HandOver::Shown => {
                    std::process::exit(iptv_core::instance::HANDED_OVER_EXIT_CODE);
                }
                iptv_core::instance::HandOver::Acquired(guard) => Some(guard),
                iptv_core::instance::HandOver::Unresponsive => {
                    let _ = wxdragon::main(|_| {
                        let frame = Frame::builder()
                            .with_title("Accessible IPTV Client")
                            .build();
                        let dialog = MessageDialog::builder(
                            &frame,
                            "Accessible IPTV Client is already running, but it did not respond. Wait a moment and try again, or close it from the notification area.",
                            "Accessible IPTV Client",
                        )
                        .build();
                        dialog.show_modal_tracked();
                    });
                    std::process::exit(iptv_core::instance::HANDED_OVER_EXIT_CODE);
                }
            }
        }
    };
    iptv_core::instance::clear_show_request(&instance_dir);
    guard
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    SystemOptions::set_option_by_int("msw.no-manifest-check", 1);
    let _instance_guard = acquire_instance();
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
        let epg_rows: Rc<RefCell<Vec<EpgSearchRow>>> = Rc::new(RefCell::new(Vec::new()));
        let epg_rows_for_text = Rc::clone(&epg_rows);
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(
                &rows.borrow(),
                &suffixes_for_text.borrow(),
                &epg_rows_for_text.borrow(),
                index,
                column,
            )
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
        bind_browser(frame, controls_all, model, suffixes, epg_rows);
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
/// language) is loaded, and replaced when the language changes so
/// new dialogs activate the new language at once (Python's
/// `i18n.set_language` in the language selector). `tr` is the
/// gettext choke point for menu labels and status announcements;
/// untranslated strings pass through unchanged.
static TRANSLATOR: std::sync::RwLock<Option<I18n>> = std::sync::RwLock::new(None);

fn set_translator(i18n: &I18n) {
    if let Ok(mut slot) = TRANSLATOR.write() {
        *slot = Some(i18n.clone());
    }
}

fn tr(text: &str) -> String {
    TRANSLATOR.read().map_or_else(
        |_| text.to_string(),
        |slot| {
            slot.as_ref()
                .map_or_else(|| text.to_string(), |i18n| i18n.gettext(text))
        },
    )
}

/// Translate a template, then fill its `{name}` placeholders.
/// Formatting before lookup (the old `tr(&format!(...))` shape) can
/// never match a catalogue template, so dynamic messages always go
/// through here.
fn trf(template: &str, args: &[(&str, &str)]) -> String {
    TRANSLATOR.read().map_or_else(
        |_| iptv_core::i18n::interpolate(template, args),
        |slot| {
            slot.as_ref().map_or_else(
                || iptv_core::i18n::interpolate(template, args),
                |i18n| i18n.gettextf(template, args),
            )
        },
    )
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

/// The player status line (audit N23; Python's
/// `_update_status_label`): rebuild the line from the state's
/// parts and set it silently; when a prefix is given it
/// becomes the line's remembered head and the WHOLE line is
/// spoken while the announcement level allows the priority
/// (that is how Python speaks a track change). A None prefix
/// only refreshes the line, the way a volume change does.
fn update_player_status(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    prefix: Option<&str>,
    priority: i64,
) {
    let line = {
        let mut state = player.borrow_mut();
        if let Some(prefix) = prefix {
            state.status_prefix = prefix.to_string();
        }
        let volume = state
            .window
            .as_ref()
            .map_or(0, |win| win.volume.get_value());
        iptv_core::playback::player_status_line(
            &state.status_prefix,
            state.status_buffer_seconds,
            state.status_bitrate_mbps,
            volume,
            &state.audio_track_label,
        )
    };
    let line = &tr(&line);
    status.set_silent(line);
    if prefix.is_some() && !line.is_empty() {
        let level = persist
            .config
            .borrow()
            .value("announcement_level")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(2)
            .clamp(0, 3);
        if level >= priority {
            live_region::announce_with_priority(status.label, line, live_region::Priority::Medium);
        }
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

/// One programme row appended to the Channels list by the
/// main-window EPG search (Python's `{"type": "epg"}` rows): the
/// row label, the programme, and the playlist channel it routes
/// to, resolved when the row is appended.
struct EpgSearchRow {
    label: String,
    programme: Programme,
    playlist_index: Option<usize>,
}

enum PendingEpgReadUi {
    Highlight {
        channel: Channel,
        list: ListCtrl,
        text: TextCtrl,
        model_ptr: usize,
    },
    ChannelProgrammes {
        list: ListBox,
        description: TextCtrl,
        programmes: Rc<RefCell<Vec<Programme>>>,
        status: Status,
    },
    CatchupProgrammes {
        channel: Channel,
        list: ListBox,
        programmes: Rc<RefCell<Vec<Programme>>>,
        status: Status,
    },
    CatchupCurrent {
        channel: Channel,
        status: Status,
    },
    WhatsPlaying {
        channel: Channel,
        status: Status,
    },
    AutoImportCheck {
        sources: Vec<String>,
    },
    SeriesProgrammes {
        rule: Box<iptv_core::dvr::DvrJob>,
        now: f64,
    },
    SearchRows {
        list: ListCtrl,
        query: String,
        model_ptr: usize,
        channel_matches: usize,
        in_all_channels: bool,
    },
    WhatsOnSnapshot {
        model: Arc<iptv_core::BrowseModel>,
        rows: Rc<RefCell<Vec<(Channel, Programme)>>>,
        filtered: Rc<RefCell<Vec<usize>>>,
        filter: TextCtrl,
        list: ListBox,
        count_label: StaticText,
        status: Status,
    },
}

#[derive(Clone)]
struct Wiring {
    browser: Rc<RefCell<BrowserWorker>>,
    categories: Rc<RefCell<CategoryTreeState>>,
    /// The favorite count the category tree currently displays
    /// (B20: a count change alone must rebuild the Favorites label).
    category_fav_count: Rc<std::cell::Cell<usize>>,
    timer: Rc<Timer<Frame>>,
    model: Rc<RefCell<SearchResults>>,
    persist: Persist,
    player: Rc<RefCell<PlayerState>>,
    epg: Rc<RefCell<Option<Arc<EpgSnapshot>>>>,
    epg_suffixes: Rc<EpgSuffixes>,
    /// The row-suffix rebuild pipeline (validation 0070): the
    /// worker reuses the shared guide indexes and owns the
    /// playlist match cache; the UI only dispatches jobs and
    /// applies results. Guide generations reject stale suffixes.
    suffix_worker: Rc<SuffixWorker>,
    suffix_pending: Rc<std::cell::Cell<bool>>,
    suffix_generation: Rc<std::cell::Cell<u64>>,
    guide_generation: Rc<std::cell::Cell<u64>>,
    epg_worker: Rc<EpgWorker>,
    epg_read_worker: Rc<iptv_core::epg::EpgReadWorker>,
    epg_read_seq: Rc<std::cell::Cell<u64>>,
    pending_epg_reads: Rc<RefCell<std::collections::HashMap<u64, PendingEpgReadUi>>>,
    epg_check_pending: Rc<std::cell::Cell<bool>>,
    epg_pending: Rc<std::cell::Cell<bool>>,
    epg_deferred: Rc<std::cell::Cell<bool>>,
    /// Programme rows appended below the channel rows by the
    /// current search (audit E08); shared with the list's text
    /// callback.
    epg_rows: Rc<RefCell<Vec<EpgSearchRow>>>,
    /// Guide sources discovered from providers (and, later,
    /// playlist headers): merged with the user's own `epgs` by
    /// `effective_epg_sources` (audit E01).
    epg_extra_sources: Rc<RefCell<Vec<String>>>,
    /// The source list of the in-flight guide load, recorded on
    /// completion as the imported-sources hash (audit E03).
    epg_inflight_sources: Rc<RefCell<Vec<String>>>,
    /// A load was requested while one was already running; the
    /// completion re-checks whether the sources changed under it.
    epg_recheck: Rc<std::cell::Cell<bool>>,
    /// The in-flight load was started by hand (Import EPG to DB
    /// or the schema-repair offer) and owes the user a finished
    /// message when it lands (N21; Python's `_epg_import_notify`).
    /// Automatic loads never set it, so they stay quiet.
    epg_import_notify: Rc<std::cell::Cell<bool>>,
    /// The independent 60-second guide-freshness timer (audit
    /// E11): row suffixes track programme boundaries even when
    /// every worker is idle.
    epg_timer: Rc<Timer<Frame>>,
    reminders: Rc<RefCell<Vec<Reminder>>>,
    provider_worker: Rc<ProviderWorker>,
    provider_pending: Rc<std::cell::Cell<bool>>,
    resolve_seq: Rc<std::cell::Cell<u64>>,
    pending_resolves: Rc<RefCell<std::collections::HashMap<u64, PendingResolve>>>,
    dvr_resolve: Rc<RefCell<std::collections::HashMap<String, DvrResolve>>>,
    /// Known provider connection limits by account key (issue
    /// #134's capacity cache); a present key with a `None` value
    /// means the provider declared no limit.
    account_limits: Rc<RefCell<std::collections::BTreeMap<String, Option<i64>>>>,
    /// The provider id of an in-flight background limit fetch,
    /// and the job holds waiting on capacity answers (job id ->
    /// hold expiry, seconds).
    limit_fetch: Rc<RefCell<Option<String>>>,
    limit_holds: Rc<RefCell<std::collections::BTreeMap<String, f64>>>,
    /// Jobs the user ordered to record over a conflict (Stop
    /// Playback and Record): the capacity gate lets them start.
    conflict_override: Rc<RefCell<std::collections::BTreeSet<String>>>,
    recordings: Rc<RefCell<RecordingEngine>>,
    cast_worker: Rc<CastWorker>,
    cast_pending: Rc<std::cell::Cell<bool>>,
    cast_ui: Rc<RefCell<Option<CastUi>>>,
    proxy: Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    guide_ui: Rc<RefCell<Option<GuideUi>>>,
    tray: Rc<TaskBarIcon>,
    i18n: Rc<RefCell<I18n>>,
    /// Always-on 1 s timer: single-instance show requests (S01)
    /// and the hourly automatic update check (S07). The main tick
    /// timer stops when idle, so these live on their own.
    housekeeping: Rc<Timer<Frame>>,
    /// The help topic of the main-window control that last took
    /// focus, so the Help menu opens where the user is (S10).
    help_focus_topic: Rc<RefCell<&'static str>>,
    vod_worker: Rc<VodWorker>,
    vod_pending: Rc<std::cell::Cell<bool>>,
    vod_ui: Rc<RefCell<Option<VodUi>>>,
    dvr: Rc<RefCell<DvrSchedule>>,
    recorded: Rc<std::cell::Cell<bool>>,
    shutdown_attempted: Rc<std::cell::Cell<bool>>,
    /// An explicit Exit (menu, tray) rather than a window close:
    /// Minimize to System Tray must not swallow it, and the
    /// exit warnings run (audit N07, Python's `_exit_requested`).
    exit_requested: Rc<std::cell::Cell<bool>>,
    last_dvr_tick: Rc<std::cell::Cell<f64>>,
    scope_ids: Rc<RefCell<Vec<Option<String>>>>,
    /// N22: category-tree branch labels by scope id, filled with
    /// the scope picker; a scope missing here is a leftover and
    /// its branch reads "Other playlists".
    branch_labels: Rc<RefCell<std::collections::BTreeMap<String, String>>>,
    download_worker: Rc<iptv_core::downloads::DownloadWorker>,
    /// The catch-up direct-file probe worker (N13): probes run
    /// off the UI thread and settle one-stream providers.
    direct_probe_worker: Rc<iptv_core::catchup_direct::DirectProbeWorker>,
    direct_probe_pending: Rc<std::cell::Cell<bool>>,
    downloads: Rc<RefCell<Vec<DownloadRow>>>,
    downloads_active: Rc<std::cell::Cell<bool>>,
    /// The open Downloads window, refreshed live by the
    /// download poll (audit R06).
    downloads_ui: Rc<RefCell<Option<DownloadsUi>>>,
    /// Next download request id (per-request progress/cancel,
    /// audit R06).
    next_download_id: Rc<std::cell::Cell<u64>>,
    /// Reminders shown this session but not yet acknowledged
    /// (audit R12: delivery is one at a time).
    reminder_seen: Rc<RefCell<std::collections::HashSet<String>>>,
    /// A reminder delivery window is currently open; the next
    /// due reminder waits for it (audit R12).
    reminder_window_open: Rc<std::cell::Cell<bool>>,
    /// Catch-up captures staged under a `.part` path, by
    /// recording key, with the final path they earn on a clean
    /// finish (audit R05).
    staged_downloads: Rc<RefCell<std::collections::HashMap<String, StagedDownload>>>,
    /// Recordings the player watches through the relay, and
    /// share starts still settling (N13).
    shared_playback: Rc<RefCell<std::collections::HashMap<String, SharedPlayback>>>,
    pending_shares: Rc<RefCell<std::collections::HashSet<String>>>,
    relay_worker: Rc<RelayWorker>,
    /// Last series-rule scan time, seconds (audit R16).
    series_last_scan: Rc<std::cell::Cell<f64>>,
    /// N17: the open Account Info window's state and the
    /// pending account discovery result.
    account_info_ui: Rc<RefCell<Option<AccountInfoUi>>>,
    account_discovery: Rc<RefCell<Option<AccountDiscovery>>>,
    account_discovery_key: Rc<std::cell::Cell<usize>>,
    buffers: Rc<RefCell<buffer::BufferRegistry>>,
    /// N16: media-type classification state — the session
    /// probe cache, in-flight probes and their ffmpeg handles.
    media_probe: MediaProbe,
    /// The debounced probe schedule: (token, channel, due
    /// time) set by the channel highlight, fired by the
    /// housekeeping tick once the selection settles.
    media_probe_pending: Rc<RefCell<Option<(u64, Channel, f64)>>>,
    media_probe_token: Rc<std::cell::Cell<u64>>,
    /// The Recording Format submenu's last refilled state key
    /// (N16): the menu-open refill is a no-op while nothing it
    /// shows has changed.
    format_menu_state: Rc<RefCell<String>>,
    /// N19: when the delayed startup update check falls due
    /// (unix seconds; 0 = not scheduled). Python runs the
    /// startup check 5 s after it is scheduled so playlist
    /// loading is not disturbed; the housekeeping tick fires
    /// it when the time comes.
    auto_check_due: Rc<std::cell::Cell<f64>>,
}

/// Built-in player window state, created lazily on first use.
/// Which engine owns the current session's loads.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ActiveEngine {
    #[default]
    Fallback,
    Vlc,
}

/// The in-flight VLC worker load's result slot (P18):
/// (session generation, was-a-retry, reply).
type VlcLoadReply = std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>;

/// A subtitle file load's result slot (P09): (generation,
/// parsed cues or the load error).
type PendingSubtitles = std::sync::Arc<std::sync::Mutex<Option<(u64, Result<Vec<Cue>, String>)>>>;

// The player state is a flat bag of session flags by design —
// one struct the poll loop and the menu/key dispatchers share.
// Splitting it into sub-structs would churn every call site.
#[allow(clippy::struct_excessive_bools)]
struct PlayerState {
    session: PlaybackSession,
    gate: LaunchGate,
    window: Option<PlayerWindow>,
    vlc: Option<iptv_vlc::VlcEngine>,
    /// The VLC transport worker's channel (P18): loads and
    /// stops are serialized there, off the UI thread.
    vlc_ops: Option<std::sync::mpsc::Sender<VlcOp>>,
    /// The in-flight worker load's result slot: (session
    /// generation, was-a-retry, reply).
    vlc_load_reply: Option<(u64, bool, VlcLoadReply)>,
    vlc_active: bool,
    vlc_tracks_ready: bool,
    vlc_track_ids: Vec<i32>,
    vlc_error: Option<String>,
    /// libVLC starting on its own thread: the first run after an
    /// install scans every plugin for about 16 s (validation 0071).
    vlc_loader: Option<std::sync::mpsc::Receiver<Result<iptv_vlc::VlcEngine, String>>>,
    /// The play waiting for that start; a later play replaces it.
    pending_play: Option<(Channel, PlaybackRequest)>,
    /// Whether this stream was started with video. Reconnects keep
    /// it, so a hidden channel never comes back decoding video.
    video_visible: bool,
    /// A fullscreen toggle is waiting for its deferred layout +
    /// focus step (see `set_fullscreen`).
    fullscreen_layout_pending: bool,
    /// The player window's 1 s and 150 ms ticks, held for its life.
    player_timers: Vec<Rc<Timer<Frame>>>,
    shared: Option<PlayerShared>,
    desired_track: Option<String>,
    /// N23: the player status line's parts (Python's
    /// `_last_status_prefix`, `_last_buffer_seconds`,
    /// `_last_bitrate_mbps`, `_audio_track_label`).
    status_prefix: String,
    status_buffer_seconds: f64,
    status_bitrate_mbps: Option<f64>,
    audio_track_label: String,
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
    /// The cache plan for this player lifetime (P01): reset on
    /// each new channel choice, grown by recovery restarts.
    cache_plan: iptv_core::playback::CachePlan,
    /// Whether the current channel is a Dispatcharr stream
    /// (its cache never grows, P01).
    dispatcharr: bool,
    /// The frozen-stream watchdog (P03) and the monotonic
    /// instant the current load started (its grace anchor).
    watchdog: iptv_core::playback::FreezeWatchdog,
    play_started_ms: i64,
    /// Preference-application attempts for late-published audio
    /// tracks, in 500 ms units (P07).
    track_attempts: u32,
    /// The pending reconnect is an expected-TS rollover (P04):
    /// no cache growth, quick-start cache on the reload.
    reconnect_rollover: bool,
    /// The Subtitles submenu's dynamic item ids and the SPU ids
    /// they map to (P08), plus the currently selected SPU
    /// (-2 = unknown, -1 = Off) and the sidecar file's track.
    spu_menu_ids: Vec<i32>,
    spu_ids: Vec<i32>,
    selected_spu: i32,
    file_subtitle_track: Option<i32>,
    /// Subtitle file loads run on a worker (P09): the
    /// generation guards delivery, the slot carries the result.
    subtitle_generation: u64,
    pending_subtitles: Option<PendingSubtitles>,
    /// Per-channel audio memory supplied for the current
    /// channel (P06): its key, remembered name and slot.
    audio_key: String,
    supply_audio_name: Option<String>,
    supply_audio_index: Option<i64>,
    subtitle_cues: Vec<Cue>,
    last_spoken_cue: Option<usize>,
    /// Subtitle review state (P11): the cursor bounded by
    /// shown history, the sampled active/latest-shown indexes,
    /// and the cues actually delivered, newest last (the
    /// manager's recent list, capped at 50).
    review: ReviewCursor,
    active_cue_index: Option<usize>,
    last_shown_cue: Option<usize>,
    shown_cues: std::collections::VecDeque<Cue>,
    /// Resume-seek protection (P16) for the adopted saved
    /// position, and the last checkpoint's monotonic time
    /// (non-forced checkpoints are throttled to 30 s).
    resume_guard: Option<iptv_core::playback::ResumeGuard>,
    last_checkpoint_ms: i64,
    /// Whether the current live channel has been recorded in
    /// the persisted recent-channels MRU (P17) — recorded
    /// once, on confirmed start.
    history_recorded: bool,
    /// The application voice (Windows SAPI) and the cue routing
    /// state for subtitle speech (issues #131/#132).
    app_voice: iptv_sapi::AppVoice,
    cue_speech: CueSpeech,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            session: PlaybackSession::new(),
            gate: LaunchGate::default(),
            window: None,
            vlc: None,
            vlc_ops: None,
            vlc_load_reply: None,
            vlc_active: false,
            vlc_tracks_ready: false,
            vlc_error: None,
            vlc_loader: None,
            pending_play: None,
            video_visible: true,
            fullscreen_layout_pending: false,
            player_timers: Vec::new(),
            vlc_track_ids: Vec::new(),
            shared: None,
            desired_track: None,
            status_prefix: String::new(),
            status_buffer_seconds: 0.0,
            status_bitrate_mbps: None,
            audio_track_label: String::new(),
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
            cache_plan: iptv_core::playback::CachePlan::new(
                &iptv_core::config::InternalPlayerSettings {
                    buffer_seconds: 25.0,
                    max_buffer_seconds: 30.0,
                    variant_max_mbps: 0.0,
                },
            ),
            dispatcharr: false,
            watchdog: iptv_core::playback::FreezeWatchdog::new(false, true),
            play_started_ms: 0,
            track_attempts: 0,
            reconnect_rollover: false,
            spu_menu_ids: Vec::new(),
            spu_ids: Vec::new(),
            selected_spu: -2,
            file_subtitle_track: None,
            subtitle_generation: 0,
            pending_subtitles: None,
            audio_key: String::new(),
            supply_audio_name: None,
            supply_audio_index: None,
            subtitle_cues: Vec::new(),
            last_spoken_cue: None,
            review: ReviewCursor::default(),
            active_cue_index: None,
            last_shown_cue: None,
            shown_cues: std::collections::VecDeque::new(),
            resume_guard: None,
            last_checkpoint_ms: 0,
            history_recorded: false,
            app_voice: iptv_sapi::AppVoice::new(""),
            cue_speech: CueSpeech::new(SpeechBackend::ScreenReader, false),
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

    /// The Python `playback_bookmarks` store (P21), normalized
    /// on read exactly as Python's `normalize` does, so records
    /// imported from the Python client are consumed unchanged.
    fn resume_entries(&self) -> Bookmarks {
        let config = self.config.borrow();
        iptv_core::bookmarks::normalize(config.value("playback_bookmarks"))
    }

    /// The persisted recent-channels keys (P17): stable
    /// channel keys only, newest first, capped at 20.
    fn recent_keys(&self) -> Vec<String> {
        let config = self.config.borrow();
        let keys: Vec<String> = match config.value("recent_channels") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        let mut keys = iptv_core::favorites::normalize_keys(keys);
        keys.truncate(iptv_core::favorites::MAX_RECENT_CHANNELS);
        keys
    }

    fn save_recent_keys(&self, keys: &[String]) {
        self.set_and_save(
            "recent_channels",
            serde_json::Value::Array(
                keys.iter()
                    .map(|key| serde_json::Value::String(key.clone()))
                    .collect(),
            ),
        );
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

    /// The per-channel audio-track memory (P06): remembered
    /// track names and slots by audio memory key.
    fn audio_memory(&self) -> (BTreeMap<String, String>, BTreeMap<String, i64>) {
        let read = |key: &str| -> BTreeMap<String, serde_json::Value> {
            match self.config.borrow().value(key) {
                Some(serde_json::Value::Object(map)) => {
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                }
                _ => BTreeMap::new(),
            }
        };
        let names = read("channel_audio_tracks")
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect();
        let indices = read("channel_audio_track_indices")
            .into_iter()
            .filter_map(|(k, v)| v.as_i64().map(|n| (k, n)))
            .collect();
        (names, indices)
    }

    /// Persist one manual audio choice (P06): the channel's
    /// name/slot memory and the global last track, in one save.
    fn save_audio_choice(&self, key: &str, name: &str, index: i64) {
        let (mut names, mut indices) = self.audio_memory();
        iptv_core::audiotracks::remember_channel_track(&mut names, &mut indices, key, name, index);
        let mut config = self.config.borrow_mut();
        config.set_value(
            "channel_audio_tracks",
            serde_json::to_value(names).unwrap_or(serde_json::Value::Null),
        );
        config.set_value(
            "channel_audio_track_indices",
            serde_json::to_value(indices).unwrap_or(serde_json::Value::Null),
        );
        config.set_value("last_audio_track", serde_json::json!(name));
        let _ = config.save(&self.path);
    }

    /// Write the bookmark store in Python's record shape
    /// (P21): `{position_ms, length_ms, updated}` plus a
    /// `metadata` object with the cleaned title and locator
    /// when either survives, newest first.
    fn save_resume(&self, entries: &Bookmarks) {
        let mut rows: Vec<(&String, &iptv_core::bookmarks::Bookmark)> = entries.iter().collect();
        rows.sort_by(|a, b| b.1.updated.cmp(&a.1.updated).then_with(|| a.0.cmp(b.0)));
        let mut map = serde_json::Map::new();
        for (key, entry) in rows {
            let mut record = serde_json::json!({
                "position_ms": entry.position_ms,
                "length_ms": entry.length_ms,
                "updated": entry.updated,
            });
            let mut metadata = serde_json::Map::new();
            if !entry.title.is_empty() {
                metadata.insert("title".to_string(), serde_json::json!(entry.title));
            }
            if let Some(locator) = &entry.locator {
                let mut loc = serde_json::json!({
                    "source": locator.source,
                    "type": locator.kind,
                    "id": locator.id,
                });
                if let Some(series) = &locator.series {
                    loc["series"] = serde_json::json!(series);
                }
                metadata.insert("locator".to_string(), loc);
            }
            if !metadata.is_empty() {
                record["metadata"] = serde_json::Value::Object(metadata);
            }
            map.insert(key.clone(), record);
        }
        let mut config = self.config.borrow_mut();
        config.set_value("playback_bookmarks", serde_json::Value::Object(map));
        let _ = config.save(&self.path);
    }
}

/// Open the real log file (S05): one sink for the process, in the
/// per-user config directory's logs folder, and record how the
/// config load went.
fn init_app_log(loaded: &iptv_core::config::Loaded) {
    iptv_core::applog::init(&iptv_core::applog::log_path(
        &iptv_core::config::user_config_dir(),
    ));
    for kept in &loaded.recovered {
        iptv_core::applog::warn(
            "config",
            &format!("Unreadable config preserved as {}", kept.display()),
        );
    }
    iptv_core::applog::info(
        "app",
        &format!("Started; config loaded from {}", loaded.path.display()),
    );
}

#[allow(clippy::too_many_lines)] // grew with the batch wiring (R01/R06 state)
fn bind_browser(
    frame: Frame,
    controls: Controls,
    model: Rc<RefCell<SearchResults>>,
    suffixes: Rc<EpgSuffixes>,
    epg_rows: Rc<RefCell<Vec<EpgSearchRow>>>,
) {
    let candidates = config::config_candidates();
    let loaded = config::load(&candidates, &config::default_write_target());
    init_app_log(&loaded);
    if !loaded.config.bool_or("show_channel_url", true) {
        controls.stream_url.hide();
    }
    let dvr_path = iptv_core::dvr::schedule_path(&loaded.path);
    let i18n = {
        let mut i18n = I18n::default();
        i18n.init_from_config(&iptv_core::i18n::locale_dir(), loaded.config.raw());
        Rc::new(RefCell::new(i18n))
    };
    let mut browser_worker = BrowserWorker::default();
    browser_worker.set_favorites(FavoriteSet::from_stored(loaded.config.favorites()));
    browser_worker.set_source_cache_dir(config::user_config_dir().join("playlist-cache"));
    let wiring = Wiring {
        browser: Rc::new(RefCell::new(browser_worker)),
        categories: Rc::new(RefCell::new(CategoryTreeState::default())),
        category_fav_count: Rc::new(std::cell::Cell::new(0)),
        timer: Rc::new(Timer::new(&frame)),
        model,
        persist: Persist {
            config: Rc::new(RefCell::new(loaded.config)),
            path: loaded.path,
        },
        player: Rc::new(RefCell::new(PlayerState::default())),
        epg: Rc::new(RefCell::new(None)),
        epg_suffixes: suffixes,
        suffix_worker: Rc::new(SuffixWorker::new()),
        suffix_pending: Rc::new(std::cell::Cell::new(false)),
        suffix_generation: Rc::new(std::cell::Cell::new(0)),
        guide_generation: Rc::new(std::cell::Cell::new(0)),
        epg_worker: Rc::new(EpgWorker::new()),
        epg_read_worker: Rc::new(iptv_core::epg::EpgReadWorker::new()),
        epg_read_seq: Rc::new(std::cell::Cell::new(0)),
        pending_epg_reads: Rc::new(RefCell::new(std::collections::HashMap::new())),
        epg_check_pending: Rc::new(std::cell::Cell::new(false)),
        epg_pending: Rc::new(std::cell::Cell::new(false)),
        epg_deferred: Rc::new(std::cell::Cell::new(false)),
        epg_rows,
        epg_extra_sources: Rc::new(RefCell::new(Vec::new())),
        epg_inflight_sources: Rc::new(RefCell::new(Vec::new())),
        epg_recheck: Rc::new(std::cell::Cell::new(false)),
        epg_import_notify: Rc::new(std::cell::Cell::new(false)),
        epg_timer: Rc::new(Timer::new(&frame)),
        reminders: Rc::new(RefCell::new(Vec::new())),
        provider_worker: Rc::new(ProviderWorker::new()),
        provider_pending: Rc::new(std::cell::Cell::new(false)),
        resolve_seq: Rc::new(std::cell::Cell::new(0)),
        pending_resolves: Rc::new(RefCell::new(std::collections::HashMap::new())),
        dvr_resolve: Rc::new(RefCell::new(std::collections::HashMap::new())),
        account_limits: Rc::new(RefCell::new(std::collections::BTreeMap::new())),
        limit_fetch: Rc::new(RefCell::new(None)),
        limit_holds: Rc::new(RefCell::new(std::collections::BTreeMap::new())),
        conflict_override: Rc::new(RefCell::new(std::collections::BTreeSet::new())),
        recordings: Rc::new(RefCell::new(RecordingEngine::new())),
        cast_worker: Rc::new(CastWorker::new()),
        cast_pending: Rc::new(std::cell::Cell::new(false)),
        cast_ui: Rc::new(RefCell::new(None)),
        proxy: Rc::new(RefCell::new(None)),
        guide_ui: Rc::new(RefCell::new(None)),
        tray: Rc::new(build_tray_icon()),
        i18n,
        housekeeping: Rc::new(Timer::new(&frame)),
        help_focus_topic: Rc::new(RefCell::new("user-guide")),
        vod_worker: Rc::new(VodWorker::new()),
        vod_pending: Rc::new(std::cell::Cell::new(false)),
        vod_ui: Rc::new(RefCell::new(None)),
        dvr: Rc::new(RefCell::new(DvrSchedule::load(dvr_path, now_seconds()))),
        recorded: Rc::new(std::cell::Cell::new(false)),
        shutdown_attempted: Rc::new(std::cell::Cell::new(false)),
        exit_requested: Rc::new(std::cell::Cell::new(false)),
        last_dvr_tick: Rc::new(std::cell::Cell::new(0.0)),
        scope_ids: Rc::new(RefCell::new(vec![None])),
        branch_labels: Rc::new(RefCell::new(std::collections::BTreeMap::new())),
        download_worker: Rc::new(iptv_core::downloads::DownloadWorker::default()),
        direct_probe_worker: Rc::new(iptv_core::catchup_direct::DirectProbeWorker::default()),
        direct_probe_pending: Rc::new(std::cell::Cell::new(false)),
        downloads: Rc::new(RefCell::new(Vec::new())),
        downloads_active: Rc::new(std::cell::Cell::new(false)),
        downloads_ui: Rc::new(RefCell::new(None)),
        next_download_id: Rc::new(std::cell::Cell::new(1)),
        reminder_seen: Rc::new(RefCell::new(std::collections::HashSet::new())),
        reminder_window_open: Rc::new(std::cell::Cell::new(false)),
        staged_downloads: Rc::new(RefCell::new(std::collections::HashMap::new())),
        shared_playback: Rc::new(RefCell::new(std::collections::HashMap::new())),
        pending_shares: Rc::new(RefCell::new(std::collections::HashSet::new())),
        relay_worker: Rc::new(RelayWorker::new()),
        series_last_scan: Rc::new(std::cell::Cell::new(0.0)),
        account_info_ui: Rc::new(RefCell::new(None)),
        account_discovery: Rc::new(RefCell::new(None)),
        account_discovery_key: Rc::new(std::cell::Cell::new(0)),
        buffers: Rc::new(RefCell::new(buffer::BufferRegistry::default())),
        media_probe: MediaProbe::new(),
        media_probe_pending: Rc::new(RefCell::new(None)),
        media_probe_token: Rc::new(std::cell::Cell::new(0)),
        format_menu_state: Rc::new(RefCell::new(String::new())),
        auto_check_due: Rc::new(std::cell::Cell::new(0.0)),
    };
    wiring.player.borrow_mut().shared = Some(PlayerShared {
        buffers: Rc::clone(&wiring.buffers),
        recordings: Rc::clone(&wiring.recordings),
        pending_shares: Rc::clone(&wiring.pending_shares),
        relay_worker: Rc::clone(&wiring.relay_worker),
        cast_ui: Rc::clone(&wiring.cast_ui),
        proxy: Rc::clone(&wiring.proxy),
        cast_worker: Rc::clone(&wiring.cast_worker),
        cast_pending: Rc::clone(&wiring.cast_pending),
        timer: Rc::clone(&wiring.timer),
        guide_ui: Rc::clone(&wiring.guide_ui),
        i18n: Rc::clone(&wiring.i18n),
        media_probe: wiring.media_probe.clone(),
        whats_playing: {
            let hook_wiring = clone_wiring(&wiring);
            Some(Rc::new(move |channel: Option<Channel>, status: Status| {
                announce_what_is_playing(&hook_wiring, channel, None, status);
            }))
        },
    });
    *wiring.reminders.borrow_mut() = wiring.persist.reminders();
    init_speech(&wiring.player, &wiring.persist, controls.status);
    preload_vlc_engine(&wiring.player, &wiring.persist);
    if wiring.dvr.borrow().pending_count() > 0
        || wiring
            .reminders
            .borrow()
            .iter()
            .any(|r| r.status == ReminderStatus::Scheduled)
    {
        assert!(wiring.timer.start(50, false));
    }
    reload_sources(&wiring);
    reload_epg(&wiring);
    // N09: restore the saved playlist scope before the first
    // load lands; loads apply scoped, and the picker shows it.
    let saved_scope = wiring
        .persist
        .config
        .borrow()
        .string_or("playlist_scope", "");
    wiring
        .browser
        .borrow_mut()
        .set_scope_silent(if saved_scope.is_empty() {
            None
        } else {
            Some(saved_scope)
        });
    fill_scope_choices(&wiring, controls.scope_choice);
    bind_completion(frame, &wiring, controls);
    bind_epg_timer(frame, &wiring, controls);
    build_menu(frame, &wiring, controls);
    bind_tray(frame, &wiring, controls.status);
    maybe_auto_check_updates(&wiring);
    bind_actions(frame, &wiring, controls);
    bind_context_menu(frame, &wiring, controls);
    wiring
        .provider_worker
        .set_cache_dir(iptv_core::config::user_config_dir().join("provider-cache"));
    bind_main_close(frame, &wiring);
    bind_housekeeping(frame, &wiring);
}

/// The always-on housekeeping tick (1 s), independent of the main
/// tick timer that stops when idle: answer single-instance show
/// requests by restoring the window (S01, Python's
/// `watch_for_show_requests`), fire the delayed startup update
/// check when it falls due (N19, Python's 5-second `CallLater`),
/// run the automatic update check hourly while the option is
/// enabled (S07, Python's update-check timer, whose handler
/// likewise re-reads the option each time), and reload the
/// playlists every 3 hours (audit N08, Python's refresh timer).
fn bind_housekeeping(frame: Frame, wiring: &Wiring) {
    let tick_wiring = clone_wiring(wiring);
    let last_check = Rc::new(std::cell::Cell::new(now_seconds()));
    let last_playlist_refresh = Rc::new(std::cell::Cell::new(now_seconds()));
    let config_dir = iptv_core::config::user_config_dir();
    wiring.housekeeping.on_tick(move |_| {
        if iptv_core::instance::take_show_request(&config_dir) {
            frame.show(true);
            frame.raise();
        }
        let now = now_seconds();
        // N19: the delayed startup check falls due once,
        // 5 s after it was scheduled at frame build (Python's
        // `_AUTO_UPDATE_DELAY_AFTER_PLAYLIST_MS` CallLater, so
        // playlist loading is not disturbed). Like the hourly
        // handler below, it re-reads the option at fire time.
        let due = tick_wiring.auto_check_due.get();
        if due > 0.0 && now >= due {
            tick_wiring.auto_check_due.set(0.0);
            if tick_wiring
                .persist
                .config
                .borrow()
                .bool_or("auto_check_updates", false)
            {
                run_auto_update_check(&tick_wiring, &frame);
            }
        }
        if now - last_check.get() >= 3600.0 {
            last_check.set(now);
            if tick_wiring
                .persist
                .config
                .borrow()
                .bool_or("auto_check_updates", false)
            {
                run_auto_update_check(&tick_wiring, &frame);
            }
        }
        // Playlists refresh on their own every 3 hours (audit
        // N08, Python's refresh timer): the same full load cycle
        // a manual refresh runs, so a long-running app does not
        // sit on a stale catalogue forever.
        if now - last_playlist_refresh.get() >= 3.0 * 3600.0 {
            last_playlist_refresh.set(now);
            reload_sources(&tick_wiring);
        }
        // N15: due download retries fire and the Downloads
        // window's live figures advance on this 1 s tick, the
        // same cadence as Python's download dialog timer.
        process_download_retries(&tick_wiring);
        refresh_downloads_window(&tick_wiring);
        // N16: a media-type probe whose selection has settled
        // for 1.5 s starts on this tick.
        poll_media_probe_schedule(&tick_wiring);
    });
    let _ = wiring.housekeeping.start(1000, false);
}

/// The main window's close behavior: minimize-to-tray hides
/// instead of closing; a real close cancels browsing, closes
/// every live buffer session, finalizes running recordings
/// (audit R07: graceful `q` first, escalating to a kill, so no
/// orphan ffmpeg keeps a provider connection) and stops the
/// tick timer.
fn bind_main_close(frame: Frame, wiring: &Wiring) {
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    let close_persist = wiring.persist.clone();
    let close_buffers = Rc::clone(&wiring.buffers);
    let close_player = Rc::clone(&wiring.player);
    let close_recordings = Rc::clone(&wiring.recordings);
    let close_dvr = Rc::clone(&wiring.dvr);
    let close_reminders = Rc::clone(&wiring.reminders);
    let close_downloads = Rc::clone(&wiring.downloads);
    let close_exit_requested = Rc::clone(&wiring.exit_requested);
    frame.bind_internal(EventType::CLOSE_WINDOW, move |event| {
        if close_persist
            .config
            .borrow()
            .bool_or("minimize_to_tray", false)
            && !close_exit_requested.get()
        {
            // A plain close with Minimize to System Tray hides
            // the app (and everything it is doing stays alive),
            // so it needs no warning; an explicit Exit falls
            // through to the checks below (audit N07).
            frame.hide();
            event.veto();
            return;
        }
        // Warn before an exit that would lose work (audit N07):
        // pending reminders, scheduled recordings that have not
        // started, and a download in flight each get Python's
        // confirmation, in Python's order; declining any of
        // them vetoes the exit.
        let prompts = {
            let dvr = close_dvr.borrow();
            #[allow(clippy::cast_possible_truncation)] // epoch seconds for reminder expiry
            let now_i64 = now_seconds() as i64;
            iptv_core::exit_guard::exit_prompts(
                &close_reminders.borrow(),
                dvr.jobs(),
                !close_recordings.borrow().active_keys().is_empty(),
                close_downloads.borrow().iter().any(|row| !row.done),
                now_i64,
            )
        };
        for prompt in &prompts {
            let mut style = MessageDialogStyle::YesNo;
            if prompt.no_default {
                style |= MessageDialogStyle::NoDefault;
            }
            let answer = MessageDialog::builder(&frame, &prompt.message, prompt.title)
                .with_style(style)
                .build()
                .show_modal_tracked();
            if answer != ID_YES {
                close_exit_requested.set(false);
                event.veto();
                return;
            }
        }
        browser.borrow_mut().cancel();
        let finished = close_recordings
            .borrow_mut()
            .shutdown_all(std::time::Duration::from_secs(3));
        if !finished.is_empty() {
            let now = now_seconds();
            let mut dvr = close_dvr.borrow_mut();
            for report in &finished {
                if let Some(job_id) = report.key.strip_prefix("dvr:") {
                    // Python's exit path: the job outcome is
                    // persisted as interrupted, never a success.
                    dvr.mark_finished(
                        job_id,
                        false,
                        &report.out_path.to_string_lossy(),
                        "Stopped because the app exited.",
                        now,
                    );
                }
            }
            dvr.save();
        }
        close_buffers.borrow_mut().close_all();
        close_player.borrow_mut().app_voice.close();
        for timer in close_player.borrow_mut().player_timers.drain(..) {
            timer.stop();
        }
        release_engine_off_thread(&close_player);
        timer.stop();
        event.skip(true);
    });
}

/// The independent guide-freshness timer (audit E11): every 60
/// seconds the row suffixes are rebuilt from the current bulk
/// snapshot, so a programme boundary updates the rows even when
/// every loader, recording and reminder is idle and the poll
/// timer has stopped. Python's now-playing timer is exactly this;
/// it skips while the window is hidden in the tray.
fn bind_epg_timer(frame: Frame, wiring: &Wiring, controls: Controls) {
    let list = controls.list;
    let tick = clone_wiring(wiring);
    wiring.epg_timer.on_tick(move |_| {
        if !frame.is_shown() {
            return;
        }
        if !tick.persist.config.borrow().bool_or("epg_enabled", true) {
            return;
        }
        // The rebuild itself runs on the suffix worker
        // (validation 0070); the tick only applies a finished
        // result and queues the next refresh.
        drain_suffix_results(&tick, list);
        request_suffix_refresh(&tick);
    });
    let _ = wiring.epg_timer.start(60_000, false);
}

/// Restart the completion poll when new work appears after the
/// timer stopped itself idle (audit R01): creating a reminder, a
/// DVR job or a menu recording must not depend on some unrelated
/// action to revive polling.
fn ensure_polling(wiring: &Wiring) {
    let _ = wiring.timer.start(50, false);
}

fn bind_completion(frame: Frame, wiring: &Wiring, controls: Controls) {
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
                // New channel rows retire the previous search's
                // programme rows before the new append runs.
                completed.epg_rows.borrow_mut().clear();
                let query = results.query().to_owned();
                let matches = results.len();
                let in_all = matches!(results.category(), Category::All);
                publish_results(list, status, &completed.model, results);
                append_epg_rows(&completed, list, &query, matches, in_all);
                sync_category_control(
                    category,
                    &completed.categories,
                    &completed.category_fav_count,
                    &completed.browser,
                    &completed.branch_labels.borrow(),
                );
            }
            Some(BrowseEvent::Recovery(error)) => {
                status.set_label(error.message());
                list.set_accessibility_description("");
            }
            Some(BrowseEvent::Loaded { stale_sources }) => {
                // B12: name the sources showing retained channels.
                for source in &stale_sources {
                    status.set_label(&format!(
                        "{source} could not be loaded; showing its previous channels."
                    ));
                }
                // B09: playlist-header guides join the guide load.
                reload_epg(&completed);
            }
            None => {}
        }
        if completed.epg_deferred.get() && !completed.browser.borrow().is_busy() {
            completed.epg_deferred.set(false);
            reload_epg(&completed);
        }
        poll_epg(&completed, frame, status);
        drain_epg_reads(&completed);
        drain_suffix_results(&completed, list);
        {
            let stale = {
                let slot = completed.epg_suffixes.borrow();
                slot.1 != completed.model.borrow().playlist_ptr()
                    || (completed.epg.borrow().is_some() && now_epoch() - slot.0 > 300)
            };
            if stale {
                request_suffix_refresh(&completed);
            }
        }
        poll_providers(&completed, controls);
        poll_recordings(&completed, frame, status);
        poll_vod(&completed, status);
        poll_cast(&completed, status);
        poll_downloads(&completed, status);
        poll_account_info(&completed, &frame, status);
        poll_direct_probes(&completed, status);
        poll_relay_jobs(&completed, status);
        if !completed.browser.borrow().is_busy()
            && !completed.epg_pending.get()
            && !completed.suffix_pending.get()
            && completed.pending_epg_reads.borrow().is_empty()
            && !completed.provider_pending.get()
            && !completed.direct_probe_pending.get()
            && !completed.relay_worker.pending()
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
/// Commit the Playlist view picker's current selection
/// (N09; Python's `on_playlist_scope_changed`): the scope is
/// saved, the search clears, and the category resets to All
/// Channels. Choosing the already-committed scope is a no-op.
fn commit_scope_selection(wiring: &Wiring, scope_choice: Choice, search: TextCtrl, status: Status) {
    let Some(selection) = scope_choice.get_selection() else {
        return;
    };
    let id = wiring
        .scope_ids
        .borrow()
        .get(usize::try_from(selection).unwrap_or(usize::MAX))
        .cloned()
        .flatten();
    if wiring.browser.borrow().scope() == id.as_deref() {
        return;
    }
    wiring.persist.set_and_save(
        "playlist_scope",
        serde_json::json!(id.clone().unwrap_or_default()),
    );
    search.set_value("");
    if wiring.browser.borrow_mut().commit_scope(id) {
        status.set_label("Load cancelled. Browsing previous channels...");
    }
    assert!(wiring.timer.start(50, false));
}

/// Put the Playlist view picker's selection back on the
/// committed scope (N09: Escape abandons a preview).
fn revert_scope_selection(wiring: &Wiring, scope_choice: Choice) {
    let committed = wiring.browser.borrow().scope().map(str::to_owned);
    let ids = wiring.scope_ids.borrow();
    let index = ids
        .iter()
        .position(|id| id.as_deref() == committed.as_deref())
        .unwrap_or(0);
    scope_choice.set_selection(u32::try_from(index).unwrap_or(0));
}

fn bind_scope_choice(wiring: &Wiring, scope_choice: Choice, search: TextCtrl, status: Status) {
    // N09 (Python's on_playlist_scope_choice / on_playlist_scope_key):
    // arrow keys only preview a scope — NVDA announces each one
    // without reloading the lists. Enter or Tab commits, a mouse
    // choice commits, and Escape reverts to the committed scope.
    let mouse_armed = Rc::new(std::cell::Cell::new(false));
    {
        let armed = Rc::clone(&mouse_armed);
        scope_choice.bind_internal(EventType::LEFT_DOWN, move |event| {
            armed.set(true);
            event.skip(true);
        });
    }
    {
        let armed = Rc::clone(&mouse_armed);
        let commit_wiring = clone_wiring(wiring);
        scope_choice.bind_internal(EventType::COMMAND_CHOICE_SELECTED, move |event| {
            if armed.replace(false) {
                commit_scope_selection(&commit_wiring, scope_choice, search, status);
            }
            event.skip(true);
        });
    }
    let key_wiring = clone_wiring(wiring);
    scope_choice.bind_internal(EventType::KEY_DOWN, move |event| {
        mouse_armed.set(false);
        match event.get_key_code() {
            // WXK_RETURN / WXK_NUMPAD_ENTER: commit here.
            Some(13 | 370) => {
                commit_scope_selection(&key_wiring, scope_choice, search, status);
                event.skip(false);
            }
            // WXK_ESCAPE: abandon the preview.
            Some(27) => {
                revert_scope_selection(&key_wiring, scope_choice);
                event.skip(false);
            }
            // WXK_TAB: commit, then let focus move on.
            Some(9) => {
                commit_scope_selection(&key_wiring, scope_choice, search, status);
                event.skip(true);
            }
            _ => event.skip(true),
        }
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
    // Entering Search selects the whole query by default, so
    // typing replaces it; Ctrl+A selects all explicitly and is
    // consumed, like a native edit field.
    search.bind_internal(EventType::SET_FOCUS, move |event| {
        search.select_all();
        event.skip(true);
    });
    search.bind_internal(EventType::KEY_DOWN, move |event| {
        if key_event_label(event).as_deref() == Some("Ctrl+A") {
            search.select_all();
            event.skip(false);
        } else {
            event.skip(true);
        }
    });

    let category_browser = Rc::clone(&browser);
    let category_timer = Rc::clone(&timer);
    let category_choices = Rc::clone(&categories);
    category.bind_internal(EventType::TREE_SEL_CHANGED, move |event| {
        let Some(selection) = category.get_selection() else {
            event.skip(true);
            return;
        };
        // N22: after a sync, the selected item maps to the first
        // row whose text path matches its own — a branch header
        // owns no row and changes nothing. Before the first sync
        // the tree still shows its two placeholder rows, which the
        // top-level position maps onto the sentinel categories.
        let chosen = {
            let state = category_choices.borrow();
            if state.rows.is_empty() {
                tree_item_index(category, &selection).and_then(|position| {
                    [Category::All, Category::Favorites].get(position).cloned()
                })
            } else {
                tree_item_text_path(category, u64::from(&selection)).and_then(|path| {
                    state
                        .rows
                        .iter()
                        .find(|row| category_row_text_path(row) == path)
                        .map(|row| row.category.clone())
                })
            }
        };
        let Some(chosen) = chosen else {
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
    bind_playback(
        wiring,
        list,
        search,
        status,
        episode_description,
        stream_url,
    );
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
/// The list row actions apply to: the selected row, else the
/// focused row (the same pick `selected_channel` makes).
fn selected_row(list: ListCtrl) -> Option<usize> {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    pick_row(selected, focused)
}

fn toggle_selected_favorite(wiring: &Wiring, list: ListCtrl, search: TextCtrl, status: Status) {
    let Some(row) = selected_row(list) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    let Some(index) = row_playlist_index(wiring, row) else {
        status.set_label("No channel selected. Choose a channel in Channels first.");
        return;
    };
    // N22: Python's removal box names the channel when a removal
    // inside the Favorites view empties it (main.py:1869), so
    // capture the view and name before the toggle.
    let (in_favorites_view, removed_name) = {
        let browser = wiring.browser.borrow();
        (
            browser.active_category() == &Category::Favorites,
            browser
                .channel(index)
                .map(|channel| iptv_core::favorites::display_name(&channel).to_owned()),
        )
    };
    match wiring
        .browser
        .borrow_mut()
        .toggle_favorite(index, &search.get_value())
    {
        Some(outcome) if outcome.toggle == Toggle::Unchanged => {
            // Python's warning box (main.py:1762) for an entry with
            // no channel key, not a status-line note.
            let dialog = MessageDialog::builder(
                &list,
                "This entry cannot be added to Favorites.",
                "Favorites",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
            dialog.show_modal_tracked();
        }
        Some(outcome) => {
            status.set_label(&outcome.announcement);
            let keys = wiring.browser.borrow().favorites().keys().to_vec();
            wiring.persist.save_favorites(&keys);
            assert!(wiring.timer.start(50, false));
            if outcome.toggle == Toggle::Removed
                && in_favorites_view
                && wiring.browser.borrow().favorite_count() == 0
            {
                if let Some(name) = &removed_name {
                    let dialog = MessageDialog::builder(
                        &list,
                        &format!("{name} was removed. Favorites is now empty."),
                        "Favorites",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                    .build();
                    dialog.show_modal_tracked();
                }
            }
        }
        None => status.set_label("No channel selected. Choose a channel in Channels first."),
    }
}

/// Fill the Playlist view picker from the configured sources and
/// accounts; the parallel scope-id list lives on the wiring.
fn fill_scope_choices(wiring: &Wiring, choice: Choice) {
    choice.clear();
    choice.append("All playlists");
    let (sources, accounts) = configured_entries(wiring);
    let names = playlist_names(wiring);
    let mut ids: Vec<Option<String>> = vec![None];
    let mut branch_labels: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for source in &sources {
        // B13: the friendly name wins over the host/file label.
        let label = iptv_core::sources::source_display_label(source.location(), &names);
        choice.append(&label);
        branch_labels.insert(source.scope_id(), label);
        ids.push(Some(source.scope_id()));
    }
    for account in &accounts {
        // N22: the picker label is Python's `_scope_choice_label`
        // ("Xtream Codes – {name}"), not the provider `describe()`
        // form that keys provider state.
        let label = account.scope_label();
        choice.append(&label);
        if let Some(id) = account.provider_id() {
            branch_labels.insert(id, label);
        }
        ids.push(account.provider_id());
    }
    // N22: the browser branches the All-playlists category list
    // per configured source, in this picker order.
    let branch_ids: Vec<String> = ids.iter().flatten().cloned().collect();
    wiring.browser.borrow_mut().set_branch_sources(branch_ids);
    *wiring.branch_labels.borrow_mut() = branch_labels;
    // N09: the picker shows the committed scope (restored at
    // startup, committed on activation), not always All.
    let committed = wiring.browser.borrow().scope().map(str::to_owned);
    let index = ids
        .iter()
        .position(|id| id.as_deref() == committed.as_deref())
        .unwrap_or(0);
    *wiring.scope_ids.borrow_mut() = ids;
    choice.set_selection(u32::try_from(index).unwrap_or(0));
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
/// 0..=180 range, asked one at a time; Python's introductory
/// explanation heads the first prompt, N24).
fn prompt_schedule_padding(wiring: &Wiring, frame: Frame, status: Status) {
    let ask = |label: &str, current: i64| -> Option<i64> {
        let dialog = TextEntryDialog::builder(&frame, label, "Schedule Padding")
            .with_default_value(&current.to_string())
            .build();
        if dialog.show_modal_tracked() != ID_OK {
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
    let first_label = "Start scheduled recordings early or keep recording after the programme ends. Manual recordings are not changed.\n\nMinutes before programme:";
    let Some(pre) = ask(first_label, pre) else {
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
    status.set_label(&trf(
        "Schedule padding: {pre} minutes before, {post} minutes after.",
        &[("pre", &pre.to_string()), ("post", &post.to_string())],
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
#[allow(clippy::too_many_lines)]
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
                // B02: a provider load replaces the provider channels
                // wholesale — reloads never append duplicates, and a
                // removed account's channels disappear.
                wiring
                    .browser
                    .borrow_mut()
                    .replace_provider_channels(channels, &controls.search.get_value());
                if count > 0 {
                    status.set_label(&trf(
                        "{count} provider channels loaded.",
                        &[("count", &count.to_string())],
                    ));
                    assert!(wiring.timer.start(50, false));
                }
                if !epg_urls.is_empty() {
                    // Provider-discovered guides join the source
                    // list (audit E01); the load itself reads the
                    // merged effective list.
                    let mut extra = wiring.epg_extra_sources.borrow_mut();
                    for url in epg_urls {
                        if !extra.contains(&url) {
                            extra.push(url);
                        }
                    }
                    drop(extra);
                    dispatch_epg_load(wiring, effective_epg_sources(wiring));
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
                            play_channel(
                                &wiring.player,
                                &wiring.persist,
                                &wiring.media_probe,
                                &channel,
                                status,
                            );
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
            ProviderEvent::Account {
                report,
                max_connections,
                provider_id,
            } => {
                // Every account fetch feeds the capacity cache;
                // a background limit fetch (issue #134) also
                // releases the holds waiting on its answer.
                let (_, accounts) = configured_entries(wiring);
                if let Some(account) = accounts
                    .iter()
                    .find(|a| a.provider_id().as_deref() == Some(provider_id.as_str()))
                {
                    wiring
                        .account_limits
                        .borrow_mut()
                        .insert(account.account_key(), max_connections);
                }
                let background =
                    wiring.limit_fetch.borrow().as_deref() == Some(provider_id.as_str());
                if background {
                    *wiring.limit_fetch.borrow_mut() = None;
                    let held: Vec<String> = wiring.limit_holds.borrow().keys().cloned().collect();
                    for id in held {
                        wiring.dvr.borrow_mut().release_hold(&id);
                        wiring.limit_holds.borrow_mut().remove(&id);
                    }
                }
                match report {
                    Ok(report) => {
                        if !background {
                            let first = report.lines().next().unwrap_or_default().to_string();
                            status.set_label(&first);
                        }
                    }
                    Err(error) => {
                        if !background {
                            status.set_label(&error);
                        }
                    }
                }
            }
        }
    }
}

/// Drain a finished guide load and fire due reminders; runs on the main
/// poll tick alongside browse completion.
/// Report how a hand-started import turned out (N21;
/// Python's `_report_epg_import_finished`). Both boxes go
/// through the N18 notification queue, so a modal already
/// open defers them instead of stacking.
fn report_epg_import_finished(frame: Frame, success: bool) {
    if success {
        show_or_queue_notification(
            frame,
            "The EPG import has finished. The programme guide is up to date.",
            "Import Complete",
            MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
            None,
        );
    } else {
        show_or_queue_notification(
            frame,
            "The EPG import did not finish. The programme guide may be incomplete or unchanged.\n\nCheck that the EPG sources in File > EPG Manager are reachable, then try again.",
            "Import Failed",
            MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
            None,
        );
    }
}

fn poll_epg(wiring: &Wiring, frame: Frame, status: Status) {
    if let Some(result) = wiring.epg_worker.try_recv() {
        wiring.epg_pending.set(false);
        // A hand-started import reports how it turned out
        // (N21; Python's `_report_epg_import_finished`, shown
        // once the guide behind it is reloaded). The flag is
        // consumed by whichever load finishes first — a manual
        // start is refused while one is running, so that is
        // always its own load.
        let notify = wiring.epg_import_notify.get();
        wiring.epg_import_notify.set(false);
        match result {
            Ok((snapshot, store_note)) => {
                let counts = (
                    snapshot.guide.channels.len(),
                    snapshot.guide.programme_count(),
                );
                *wiring.epg.borrow_mut() = Some(Arc::clone(&snapshot));
                // The guide changed under the suffix worker's
                // caches (validation 0070): bump its generation
                // and mark the on-screen suffixes stale.
                wiring.guide_generation.set(snapshot.generation);
                wiring.epg_suffixes.borrow_mut().0 = 0;
                // Python records the import stamp and the hash of
                // the sources actually imported, so the next
                // automatic decision can respect the interval.
                let imported = wiring.epg_inflight_sources.borrow().clone();
                wiring
                    .persist
                    .set_and_save("epg_last_import_epoch", serde_json::json!(now_epoch()));
                wiring.persist.set_and_save(
                    "epg_last_sources_hash",
                    serde_json::json!(hash_epg_sources(&imported)),
                );
                status.set_label(&trf(
                    "Guide loaded: {channels} channels, {programmes} programmes.{note}",
                    &[
                        ("channels", &counts.0.to_string()),
                        ("programmes", &counts.1.to_string()),
                        ("note", &store_note),
                    ],
                ));
                if notify {
                    report_epg_import_finished(frame, true);
                }
                // Sources changed while this load ran (a provider
                // finished after it started): run the load the
                // change asked for, as Python does.
                if wiring.epg_recheck.get() {
                    wiring.epg_recheck.set(false);
                    let current = effective_epg_sources(wiring);
                    if hash_epg_sources(&current) != hash_epg_sources(&imported) {
                        dispatch_epg_load(wiring, current);
                    }
                }
            }
            Err(error) => {
                iptv_core::applog::error("epg", &format!("Guide failed to load: {error}"));
                status.set_label(&trf("Guide failed to load: {error}", &[("error", &error)]));
                if notify {
                    report_epg_import_finished(frame, false);
                }
            }
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let mut changed = false;
    for reminder in wiring.reminders.borrow_mut().iter_mut() {
        if reminder.is_expired(now) {
            reminder.status = ReminderStatus::Expired;
            changed = true;
        }
    }
    if changed {
        let rows = wiring.reminders.borrow().clone();
        wiring.persist.save_reminders(&rows);
    }
    // The seen set tracks only live rows (Python prunes it the
    // same way), so a deleted reminder's id cannot suppress a
    // future delivery.
    {
        let ids: std::collections::HashSet<String> = wiring
            .reminders
            .borrow()
            .iter()
            .map(|r| r.id.clone())
            .collect();
        wiring
            .reminder_seen
            .borrow_mut()
            .retain(|id| ids.contains(id));
    }
    // Audit R12: deliver ONE due reminder at a time. It is
    // marked Notified only when its window is answered; the
    // next simultaneous reminder waits for a later tick
    // instead of being silently marked delivered.
    if !wiring.reminder_window_open.get() {
        let due = {
            let rows = wiring.reminders.borrow();
            let seen = wiring.reminder_seen.borrow();
            iptv_core::reminders::next_pending_delivery(&rows, &seen, now)
        };
        if let Some(reminder) = due {
            wiring
                .reminder_seen
                .borrow_mut()
                .insert(reminder.id.clone());
            open_reminder_window(wiring, status, &reminder);
        }
    }
}

fn submit_epg_read(wiring: &Wiring, kind: iptv_core::epg::EpgReadKind, ui: PendingEpgReadUi) {
    let request_id = wiring.epg_read_seq.get().wrapping_add(1);
    wiring.epg_read_seq.set(request_id);
    let guide_generation = wiring
        .epg
        .borrow()
        .as_ref()
        .map_or(wiring.guide_generation.get(), |snapshot| {
            snapshot.generation
        });
    wiring.pending_epg_reads.borrow_mut().insert(request_id, ui);
    wiring.epg_read_worker.submit(iptv_core::epg::EpgReadJob {
        request_id,
        guide_generation,
        store_path: epg_store_path(),
        kind,
    });
    ensure_polling(wiring);
}

#[allow(clippy::too_many_lines)] // One exhaustive dispatcher preserves request/result pairing.
fn drain_epg_reads(wiring: &Wiring) {
    while let Some(result) = wiring.epg_read_worker.try_recv() {
        let Some(ui) = wiring
            .pending_epg_reads
            .borrow_mut()
            .remove(&result.request_id)
        else {
            continue;
        };
        if !epg_read_generation_matches(result.guide_generation, wiring.guide_generation.get()) {
            if let PendingEpgReadUi::AutoImportCheck { sources } = ui {
                wiring.epg_check_pending.set(false);
                if !wiring.epg_pending.get() {
                    dispatch_epg_load_mode(wiring, sources, false);
                }
            }
            continue;
        }
        match (ui, result.result) {
            (
                PendingEpgReadUi::AutoImportCheck { sources },
                Ok(iptv_core::epg::EpgReadValue::UsableData(usable)),
            ) => finish_epg_auto_import(wiring, sources, usable),
            (PendingEpgReadUi::AutoImportCheck { sources }, Err(error)) => {
                wiring.epg_check_pending.set(false);
                iptv_core::applog::error("epg", &format!("Guide freshness check failed: {error}"));
                finish_epg_auto_import(wiring, sources, false);
            }
            (
                PendingEpgReadUi::Highlight {
                    channel,
                    list,
                    text,
                    model_ptr,
                },
                Ok(iptv_core::epg::EpgReadValue::NowNext(entry)),
            ) if model_ptr == wiring.model.borrow().playlist_ptr()
                && selected_channel(wiring, list).as_ref() == Some(&channel) =>
            {
                if let Some(entry) = entry {
                    text.set_value(&epg_info_message(
                        entry.current.as_ref(),
                        entry.next.as_ref(),
                    ));
                } else {
                    text.set_value("");
                }
            }
            (
                PendingEpgReadUi::ChannelProgrammes {
                    list,
                    description,
                    programmes,
                    status,
                },
                Ok(iptv_core::epg::EpgReadValue::Programmes(rows)),
            ) => {
                let now = now_epoch();
                let rows: Vec<Programme> = rows
                    .into_iter()
                    .filter(|p| p.stop > now && p.start < now + 7 * 86_400)
                    .collect();
                if rows.is_empty() {
                    status.set_label("No guide information for this channel.");
                    continue;
                }
                let initial = airing_index(&rows, now).unwrap_or(0);
                let labels: Vec<String> = rows
                    .iter()
                    .map(|p| format!("{} {}", epg_time_range(p, now), p.title))
                    .collect();
                list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
                list.set_selection(u32::try_from(initial).unwrap_or(0), true);
                description.set_value(rows.get(initial).map_or("", |p| p.description.as_str()));
                *programmes.borrow_mut() = rows;
                status.set_silent("Guide information loaded.");
            }
            (
                PendingEpgReadUi::CatchupProgrammes {
                    channel,
                    list,
                    programmes,
                    status,
                },
                Ok(iptv_core::epg::EpgReadValue::Programmes(rows)),
            ) => {
                let now = now_epoch();
                let rows: Vec<Programme> = rows
                    .into_iter()
                    .filter(|p| {
                        p.stop <= now
                            && iptv_core::catchup::within_catchup_window(&channel, p.start, now)
                    })
                    .collect();
                if rows.is_empty() {
                    status.set_label("No catch-up programmes available for this channel.");
                    continue;
                }
                let labels: Vec<String> = rows
                    .iter()
                    .map(|p| {
                        let day = chrono::TimeZone::timestamp_opt(&chrono::Local, p.start, 0)
                            .single()
                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                            .unwrap_or_default();
                        format!(
                            "{} {}-{} {}",
                            day,
                            local_hhmm(p.start),
                            local_hhmm(p.stop),
                            p.title
                        )
                    })
                    .collect();
                list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
                list.set_selection(0, true);
                status.set_label(&trf(
                    "{count} catch-up programmes.",
                    &[("count", &rows.len().to_string())],
                ));
                *programmes.borrow_mut() = rows;
            }
            (
                PendingEpgReadUi::CatchupCurrent { channel, status },
                Ok(iptv_core::epg::EpgReadValue::NowNext(Some(entry))),
            ) => {
                if let Some(programme) = entry.current {
                    play_catchup_for(wiring, &channel, &programme, status);
                } else {
                    status.set_label("No current programme to catch up.");
                }
            }
            (
                PendingEpgReadUi::CatchupCurrent { status, .. },
                Ok(iptv_core::epg::EpgReadValue::NowNext(None)),
            ) => status.set_label("No current programme to catch up."),
            (
                PendingEpgReadUi::WhatsPlaying { channel, status },
                Ok(iptv_core::epg::EpgReadValue::NowNext(entry)),
            ) => deliver_what_is_playing(wiring, &channel, status, entry.as_ref()),
            (PendingEpgReadUi::WhatsPlaying { channel, status }, Err(_)) => {
                // Python logs the failed guide read and still
                // answers with the unavailable wording.
                deliver_what_is_playing(wiring, &channel, status, None);
            }
            (
                PendingEpgReadUi::SeriesProgrammes { rule, now },
                Ok(iptv_core::epg::EpgReadValue::Programmes(rows)),
            ) => apply_series_programmes(wiring, &rule, now, rows),
            (
                PendingEpgReadUi::SearchRows {
                    list,
                    query,
                    model_ptr,
                    channel_matches,
                    in_all_channels,
                },
                Ok(iptv_core::epg::EpgReadValue::Search(hits)),
            ) => {
                let current = wiring.model.borrow();
                let enabled = wiring.persist.config.borrow().bool_or("epg_enabled", true);
                let valid = current.query() == query
                    && current.playlist_ptr() == model_ptr
                    && (current.category() == &Category::All) == in_all_channels
                    && should_run_epg_search(
                        enabled,
                        in_all_channels,
                        wiring.epg_pending.get(),
                        &query,
                        channel_matches,
                        true,
                    );
                if !valid {
                    continue;
                }
                let rows: Vec<EpgSearchRow> = hits
                    .into_iter()
                    .map(|hit| EpgSearchRow {
                        label: epg_row_label(&hit.display_name, &hit.programme),
                        programme: hit.programme,
                        playlist_index: hit.playlist_index,
                    })
                    .collect();
                let base = i64::try_from(current.len()).unwrap_or(0);
                let total = base + i64::try_from(rows.len()).unwrap_or(0);
                drop(current);
                *wiring.epg_rows.borrow_mut() = rows;
                list.set_item_count(total);
                if total > base {
                    list.refresh_items(base, total - 1);
                }
            }
            (
                PendingEpgReadUi::WhatsOnSnapshot {
                    model,
                    rows,
                    filtered,
                    filter,
                    list,
                    count_label,
                    status,
                },
                Ok(iptv_core::epg::EpgReadValue::Snapshot(snapshot_rows)),
            ) => {
                if !std::sync::Arc::ptr_eq(&model, &wiring.browser.borrow().catalogue_model()) {
                    continue;
                }
                let loaded_rows: Vec<(Channel, Programme)> = snapshot_rows
                    .into_iter()
                    .filter_map(|(index, programme)| {
                        model
                            .channels()
                            .get(index)
                            .cloned()
                            .map(|channel| (channel, programme))
                    })
                    .collect();
                *rows.borrow_mut() = loaded_rows;
                filter_whats_on_rows(&rows, &filtered, &filter.get_value());
                render_whats_on_rows(&rows, &filtered, list, count_label);
                if rows.borrow().is_empty() {
                    status.set_label("No guide information is loaded yet.");
                } else {
                    status.set_silent("Select a programme to play or schedule.");
                }
            }
            (
                PendingEpgReadUi::ChannelProgrammes { status, .. }
                | PendingEpgReadUi::CatchupProgrammes { status, .. },
                Err(error),
            ) => {
                iptv_core::applog::error("epg", &format!("Cold guide read failed: {error}"));
                status.set_label("Could not load guide information.");
            }
            (PendingEpgReadUi::CatchupCurrent { status, .. }, Err(error)) => {
                iptv_core::applog::error("epg", &format!("Cold guide read failed: {error}"));
                status.set_label("Could not load the current programme.");
            }
            (PendingEpgReadUi::WhatsOnSnapshot { status, .. }, Err(error)) => {
                iptv_core::applog::error("epg", &format!("Cold guide snapshot failed: {error}"));
                status.set_label("Could not load What's on Now.");
            }
            (_, Err(error)) => {
                iptv_core::applog::error("epg", &format!("Cold guide read failed: {error}"));
            }
            _ => {}
        }
    }
}

fn epg_read_generation_matches(result_generation: u64, current_generation: u64) -> bool {
    result_generation == current_generation
}

fn apply_series_programmes(
    wiring: &Wiring,
    rule: &iptv_core::dvr::DvrJob,
    now: f64,
    programmes: Vec<Programme>,
) {
    let channel = channel_from_snapshot(&rule.channel);
    let mut added = false;
    for programme in programmes {
        if programme.start <= now_epoch()
            || !iptv_core::dvr::series_title_matches(&rule.title, &programme.title)
        {
            continue;
        }
        let spec = iptv_core::dvr::NewJob {
            channel: &channel,
            programme_title: &programme.title,
            programme_start: programme.start,
            programme_stop: programme.stop,
            format: &rule.format,
            pre_padding_minutes: rule.pre_padding_minutes,
            post_padding_minutes: rule.post_padding_minutes,
            repeat: "none",
        };
        let Ok(job) = iptv_core::dvr::build_job(&spec, now) else {
            continue;
        };
        let mut dvr = wiring.dvr.borrow_mut();
        if dvr.find_duplicate(&job).is_none() {
            dvr.add_job(job);
            added = true;
        }
    }
    if added {
        wiring.dvr.borrow_mut().save();
    }
}

/// Mark one reminder delivered and persist (Python's
/// `acknowledge`, called when the delivery window is answered).
fn acknowledge_reminder(wiring: &Wiring, id: &str) {
    let mut rows = wiring.reminders.borrow_mut();
    if iptv_core::reminders::acknowledge(&mut rows, id) {
        let saved = rows.clone();
        drop(rows);
        wiring.persist.save_reminders(&saved);
    }
}

/// The reminder delivery window (audit R12/R13): one reminder,
/// with Open Channel resolving the uniquely matching channel
/// (Python's delivery dialog).
fn open_reminder_window(wiring: &Wiring, status: Status, reminder: &Reminder) {
    wiring.reminder_window_open.set(true);
    let frame = Frame::builder()
        .with_title("Programme Reminder")
        .with_size(Size::new(480, 260))
        .build();
    frame.set_accessibility_label("Programme Reminder");
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    let title = if reminder.title.is_empty() {
        "(No title)"
    } else {
        &reminder.title
    };
    let starts = chrono::TimeZone::timestamp_opt(&chrono::Local, reminder.start, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let text = StaticText::builder(&panel)
        .with_label(&format!("{title}\n{}\nStarts: {starts}", reminder.channel))
        .build();
    text.set_accessibility_label("Reminder details");
    let open_button = Button::builder(&panel).with_label("&Open Channel").build();
    let close_button = Button::builder(&panel).with_label("&Close").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&text, 0, SizerFlag::All, 8);
    sizer.add(&open_button, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&close_button, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let reminder_id = reminder.id.clone();
    let reminder_open = reminder.clone();
    let wiring_o = clone_wiring(wiring);
    open_button.on_click(move |_| {
        acknowledge_reminder(&wiring_o, &reminder_id);
        wiring_o.reminder_window_open.set(false);
        frame.hide();
        let channels = wiring_o.browser.borrow().all_channels();
        match iptv_core::reminders::find_channel_for_reminder(&reminder_open, &channels) {
            Some(channel) => {
                let channel = channel.clone();
                play_channel(
                    &wiring_o.player,
                    &wiring_o.persist,
                    &wiring_o.media_probe,
                    &channel,
                    status,
                );
            }
            None => status.set_label("Channel not found."),
        }
    });
    let wiring_c = clone_wiring(wiring);
    let reminder_id_c = reminder.id.clone();
    close_button.on_click(move |_| {
        acknowledge_reminder(&wiring_c, &reminder_id_c);
        wiring_c.reminder_window_open.set(false);
        frame.hide();
    });
    frame.show(true);
}

/// The (current, next) programmes for a channel from whichever
/// guide surface is live: `None` when the channel has no guide
/// match at all (distinct from a match with nothing on).
fn guide_now_next(wiring: &Wiring, channel: &Channel) -> Option<NowNext> {
    if !wiring.persist.config.borrow().bool_or("epg_enabled", true) {
        return None;
    }
    let now = now_epoch();
    if let Some(snapshot) = wiring.epg.borrow().as_ref() {
        let id = match_guide_channel(
            channel,
            &snapshot.indexes,
            Some(&snapshot.indexes.known_ids),
        )?;
        let (current, next) = snapshot.guide.now_next(&id, now);
        return Some(NowNext { current, next });
    }
    None
}

/// Python's `_channel_display_name`: the first non-empty of
/// the display name, the guide name, the guide id, else the
/// generic stream label.
fn channel_display_name(channel: &Channel) -> String {
    for candidate in [&channel.name, &channel.tvg_name, &channel.tvg_id] {
        if !candidate.is_empty() {
            return candidate.clone();
        }
    }
    "IPTV Stream".to_string()
}

/// Python's `describe` for one What Is Playing programme:
/// title and local start/end times, or the unavailable line.
fn playing_programme_text(programme: Option<&Programme>) -> String {
    match programme {
        None => tr("Guide information unavailable"),
        Some(programme) => {
            let title = if programme.title.is_empty() {
                "Untitled Program"
            } else {
                programme.title.as_str()
            };
            trf(
                "{title}, {start} to {end}",
                &[
                    ("title", title),
                    ("start", &iptv_core::epg::local_hhmm(programme.start)),
                    ("end", &iptv_core::epg::local_hhmm(programme.stop)),
                ],
            )
        }
    }
}

/// The What Is Playing sentence (audit N25, Python's exact
/// format at `main.py:1838`).
fn what_is_playing_text(channel: &Channel, now_next: Option<&NowNext>, recording: bool) -> String {
    let (current, next) = match now_next {
        Some(entry) => (entry.current.as_ref(), entry.next.as_ref()),
        None => (None, None),
    };
    trf(
        "{channel}. Now: {current}. Next: {next}. Recording: {recording}.",
        &[
            ("channel", &channel_display_name(channel)),
            ("current", &playing_programme_text(current)),
            ("next", &playing_programme_text(next)),
            ("recording", &tr(if recording { "Yes" } else { "No" })),
        ],
    )
}

/// Deliver a What Is Playing answer on a status surface —
/// setting the label also speaks it, Python's
/// `_show_playing_info`.
fn deliver_what_is_playing(
    wiring: &Wiring,
    channel: &Channel,
    status: Status,
    now_next: Option<&NowNext>,
) {
    let recording = wiring
        .recordings
        .borrow()
        .is_recording(&channel_key(channel));
    status.set_label(&what_is_playing_text(channel, now_next, recording));
}

/// What Is Playing (audit N25, Python's
/// `_announce_what_is_playing`): the playing channel, else
/// the most recent, else the selected one. The loaded guide
/// answers at once; before it loads, the persisted store is
/// read on the EPG read worker, like Python's executor read
/// of the guide database.
fn announce_what_is_playing(
    wiring: &Wiring,
    preferred: Option<Channel>,
    list: Option<ListCtrl>,
    status: Status,
) {
    let channel = preferred
        .or_else(|| wiring.player.borrow().current_channel.clone())
        .or_else(|| resolved_recent(wiring).into_iter().next())
        .or_else(|| list.and_then(|list| selected_channel(wiring, list)));
    let Some(channel) = channel else {
        status.set_label("No channel is selected.");
        return;
    };
    if wiring.epg.borrow().is_some() || !wiring.persist.config.borrow().bool_or("epg_enabled", true)
    {
        let now_next = guide_now_next(wiring, &channel);
        deliver_what_is_playing(wiring, &channel, status, now_next.as_ref());
        return;
    }
    submit_epg_read(
        wiring,
        iptv_core::epg::EpgReadKind::NowNext {
            channel: channel.clone(),
            at: now_epoch(),
        },
        PendingEpgReadUi::WhatsPlaying { channel, status },
    );
}

/// The EPG search row at a list row, when the row is one of the
/// appended programme rows rather than a channel row.
fn row_epg(wiring: &Wiring, row: usize) -> Option<EpgSearchRow> {
    let base = wiring.model.borrow().len();
    if row < base {
        return None;
    }
    wiring
        .epg_rows
        .borrow()
        .get(row - base)
        .map(|r| EpgSearchRow {
            label: r.label.clone(),
            programme: r.programme.clone(),
            playlist_index: r.playlist_index,
        })
}

/// The playlist index behind a list row: the row's own for
/// channel rows, the routed channel for EPG search rows.
fn row_playlist_index(wiring: &Wiring, row: usize) -> Option<usize> {
    if let Some(epg_row) = row_epg(wiring, row) {
        return epg_row.playlist_index;
    }
    wiring.model.borrow().playlist_index(row)
}

/// Append bounded programme rows to a fresh channel search
/// (audit E08): the store answers the channel-name query, each
/// hit is routed to its playlist channel once, and the rows land
/// below the channel rows. The guard keeps broad queries and
/// non–All Channels views channel-only, as Python does.
fn append_epg_rows(
    wiring: &Wiring,
    list: ListCtrl,
    query: &str,
    channel_matches: usize,
    in_all_channels: bool,
) {
    let enabled = wiring.persist.config.borrow().bool_or("epg_enabled", true);
    if !should_run_epg_search(
        enabled,
        in_all_channels,
        wiring.epg_pending.get(),
        query,
        channel_matches,
        true,
    ) {
        return;
    }
    let model = wiring.browser.borrow().catalogue_model();
    let model_ptr = std::sync::Arc::as_ptr(&model) as usize;
    submit_epg_read(
        wiring,
        iptv_core::epg::EpgReadKind::Search {
            query: query.to_owned(),
            now: now_epoch(),
            limit: SEARCH_EPG_RESULT_LIMIT,
            model,
        },
        PendingEpgReadUi::SearchRows {
            list,
            query: query.to_owned(),
            model_ptr,
            channel_matches,
            in_all_channels,
        },
    );
}

/// The channel behind the list's current selection, if any.
/// EPG search rows resolve to the channel they route to.
fn selected_channel(wiring: &Wiring, list: ListCtrl) -> Option<Channel> {
    let selected = usize::try_from(list.get_first_selected_item()).ok();
    let focused =
        usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused)).ok();
    let row = pick_row(selected, focused)?;
    let index = row_playlist_index(wiring, row)?;
    wiring.browser.borrow().channel(index)
}

/// The identity a channel keeps its remembered audio track
/// under (Python `_channel_audio_key`): its display name,
/// case-folded — deliberately not the stream URL.
fn channel_audio_key(channel: &Channel) -> String {
    let name = if !channel.name.trim().is_empty() {
        channel.name.as_str()
    } else if !channel.tvg_name.trim().is_empty() {
        channel.tvg_name.as_str()
    } else {
        channel.tvg_id.as_str()
    };
    name.trim().to_lowercase()
}

/// N16 media-type probe state (Python's `_media_type_cache`,
/// `_media_probe_inflight` and `_media_probe_procs`): the
/// session cache is shared with the probe worker threads; the
/// in-flight set and the live ffmpeg handles let a user
/// session preempt a probe before it steals a one-stream
/// provider's only connection.
/// One finished account lookup: (request token, account
/// index, report or error) — Python's dialog token guard.
type AccountLookupMsg = (u64, usize, Result<String, String>);

/// A finished account discovery: the accounts found, or the
/// scan's failure text.
type AccountDiscovery =
    std::sync::mpsc::Receiver<Result<Vec<iptv_core::account_info::Account>, String>>;

// Modal tracking (audit N18, Python's `modal_guard.py` +
// `main.py:4071`): opening a second message box from inside
// the first one's message loop desynchronizes wxMSW's
// parent-enable bookkeeping and leaves the main frame
// permanently disabled ("unavailable" to NVDA). So every
// modal in the app counts toward one depth, and
// notifications that arrive while a modal is up queue
// (cap 8, dropped beyond) and show once the depth returns
// to zero.
thread_local! {
    static MODAL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DEFERRED_NOTES: RefCell<std::collections::VecDeque<DeferredNote>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

/// How many notifications may wait behind a modal box
/// (Python's `_DEFERRED_NOTIFICATION_LIMIT`).
const DEFERRED_NOTIFICATION_LIMIT: usize = 8;

/// A notification waiting for the modal depth to clear.
struct DeferredNote {
    parent: Frame,
    message: String,
    caption: String,
    style: MessageDialogStyle,
    on_dismiss: Option<Rc<dyn Fn()>>,
}

/// True while any tracked modal is showing.
fn modal_box_is_open() -> bool {
    MODAL_DEPTH.with(|depth| depth.get() > 0)
}

/// Run a modal with the depth tracked; when the last modal
/// closes, the deferred notifications drain (Python's
/// closed-hook via `wx.CallAfter` — here the chain runs
/// inline, bounded by the queue cap).
fn run_modal<T>(f: impl FnOnce() -> T) -> T {
    MODAL_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let result = f();
    MODAL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    if !modal_box_is_open() {
        drain_deferred_notes();
    }
    result
}

/// `show_modal` with the modal depth tracked (N18). Every
/// modal call site in this file goes through it.
trait ModalRun {
    fn show_modal_tracked(&self) -> i32;
}

impl ModalRun for MessageDialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

impl ModalRun for Dialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

impl ModalRun for FileDialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

impl ModalRun for TextEntryDialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

impl ModalRun for SingleChoiceDialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

impl ModalRun for DirDialog {
    fn show_modal_tracked(&self) -> i32 {
        run_modal(|| self.show_modal())
    }
}

/// Show one deferred notification, if any is queued. The
/// tracked show drains the rest of the queue behind it.
fn drain_deferred_notes() {
    let note = DEFERRED_NOTES.with(|queue| queue.borrow_mut().pop_front());
    if let Some(note) = note {
        let dialog = MessageDialog::builder(&note.parent, &note.message, &note.caption)
            .with_style(note.style)
            .build();
        dialog.show_modal_tracked();
        if let Some(on_dismiss) = note.on_dismiss {
            on_dismiss();
        }
    }
}

/// Show a notification now, or queue it behind an open modal
/// (Python's `_show_or_queue_message_box`). The dismissal
/// callback runs after the box closes, whenever that is.
fn show_or_queue_notification(
    parent: Frame,
    message: &str,
    caption: &str,
    style: MessageDialogStyle,
    on_dismiss: Option<Rc<dyn Fn()>>,
) {
    if modal_box_is_open() {
        DEFERRED_NOTES.with(|queue| {
            let mut queue = queue.borrow_mut();
            if queue.len() < DEFERRED_NOTIFICATION_LIMIT {
                queue.push_back(DeferredNote {
                    parent,
                    message: message.to_string(),
                    caption: caption.to_string(),
                    style,
                    on_dismiss,
                });
            }
        });
        return;
    }
    let dialog = MessageDialog::builder(&parent, message, caption)
        .with_style(style)
        .build();
    dialog.show_modal_tracked();
    if let Some(on_dismiss) = on_dismiss {
        on_dismiss();
    }
}

/// The open Account Info window's state (audit N17, Python's
/// `AccountInfoDialog`): accounts with their cached reports,
/// the request token that retires stale lookups, and the
/// channel lookup workers report on.
struct AccountInfoUi {
    list: ListBox,
    details: TextCtrl,
    status: Status,
    refresh: Button,
    accounts: Vec<iptv_core::account_info::Account>,
    reports: RefCell<std::collections::HashMap<usize, String>>,
    token: std::cell::Cell<u64>,
    sender: std::sync::mpsc::Sender<AccountLookupMsg>,
    receiver: RefCell<std::sync::mpsc::Receiver<AccountLookupMsg>>,
}

/// A probe's ffmpeg child handle, shared between the probe
/// worker (owner) and any session preempting it.
type ProbeChild = Arc<std::sync::Mutex<Option<std::process::Child>>>;
type ProbeProcs = Arc<std::sync::Mutex<std::collections::HashMap<String, ProbeChild>>>;

#[derive(Clone)]
struct MediaProbe {
    cache: Arc<iptv_core::media_type::MediaTypeCache>,
    inflight: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    procs: ProbeProcs,
    urls: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl MediaProbe {
    fn new() -> Self {
        Self {
            cache: Arc::new(iptv_core::media_type::MediaTypeCache::new()),
            inflight: Arc::default(),
            procs: Arc::default(),
            urls: Arc::default(),
        }
    }

    fn in_flight(&self, identity: &str) -> bool {
        self.inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(identity)
    }
}

/// Classification for one channel (Python
/// `_classify_channel_media`): the channel's own metadata
/// statement first, then the session probe cache; returns
/// `(media, source)`. Never probes — probing is scheduled
/// from the selection settle and runs off the UI thread.
fn classify_channel_media(
    cache: &iptv_core::media_type::MediaTypeCache,
    channel: &Channel,
) -> (String, String) {
    use iptv_core::media_type::{MEDIA_AUDIO, MEDIA_UNKNOWN, MEDIA_VIDEO, SOURCE_METADATA};
    match channel_media_is_audio(channel) {
        Some(true) => return (MEDIA_AUDIO.to_string(), SOURCE_METADATA.to_string()),
        Some(false) => return (MEDIA_VIDEO.to_string(), SOURCE_METADATA.to_string()),
        None => {}
    }
    let identity = iptv_core::media_type::channel_record_key(channel);
    if let Some(hit) = cache.lookup(&identity, &channel.url) {
        return hit;
    }
    (MEDIA_UNKNOWN.to_string(), MEDIA_UNKNOWN.to_string())
}

/// Queue a classification probe for when the selection
/// settles (Python `_schedule_media_probe`): each highlight
/// re-arms the 1.5 s debounce, so arrowing through the list
/// opens no streams; the housekeeping tick fires it.
fn schedule_media_probe(wiring: &Wiring, channel: &Channel) {
    let token = wiring.media_probe_token.get() + 1;
    wiring.media_probe_token.set(token);
    *wiring.media_probe_pending.borrow_mut() = Some((token, channel.clone(), now_seconds() + 1.5));
}

/// Fire a due scheduled probe (housekeeping tick).
fn poll_media_probe_schedule(wiring: &Wiring) {
    let due = {
        let pending = wiring.media_probe_pending.borrow();
        match &*pending {
            Some((token, channel, deadline))
                if *token == wiring.media_probe_token.get() && now_seconds() >= *deadline =>
            {
                Some(channel.clone())
            }
            _ => None,
        }
    };
    if let Some(channel) = due {
        *wiring.media_probe_pending.borrow_mut() = None;
        probe_channel_media_now(wiring, &channel);
    }
}

/// Start the worker-thread probe unless the type is known or
/// already in flight (Python `_probe_channel_media_now`). The
/// guards mirror Python's: a buffered channel needs no probe,
/// only http(s) streams are probed, nothing probes while the
/// player has media, and a busy one-stream provider is never
/// stolen from.
fn probe_channel_media_now(wiring: &Wiring, channel: &Channel) {
    let probe = &wiring.media_probe;
    if wiring.buffers.borrow().owns_source(&channel.url) {
        return;
    }
    let (media, _) = classify_channel_media(&probe.cache, channel);
    if media != iptv_core::media_type::MEDIA_UNKNOWN {
        return;
    }
    let (fetch_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
    let lower = fetch_url.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return;
    }
    if wiring.player.borrow().session.current().is_some() {
        return;
    }
    let cast_url = cast_source_url(&wiring.cast_ui);
    if single_stream_busy(
        &wiring.recordings,
        &wiring.buffers,
        Some(&wiring.player),
        &fetch_url,
        true,
        cast_url.as_deref(),
    ) {
        return;
    }
    let identity = iptv_core::media_type::channel_record_key(channel);
    {
        let mut inflight = probe
            .inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inflight.contains(&identity) {
            return;
        }
        inflight.insert(identity.clone());
    }
    let headers = iptv_core::playback::StreamHeaders::for_channel(channel);
    let probe = probe.clone();
    let channel_url = channel.url.clone();
    std::thread::spawn(move || {
        run_media_probe(&probe, &identity, &channel_url, &fetch_url, &headers);
    });
}

/// The probe worker (Python `_probe_channel_media_worker`
/// over recorder's `probe_stream_report`): one `ffmpeg -i`,
/// its stderr classified into the session cache. The child
/// handle is registered so a user session can preempt the
/// probe; a probe preempted before registering kills itself
/// rather than holding a provider slot unowned.
fn run_media_probe(
    probe: &MediaProbe,
    identity: &str,
    channel_url: &str,
    fetch_url: &str,
    headers: &iptv_core::playback::StreamHeaders,
) {
    let mut cmd: Vec<String> = vec![
        "ffmpeg".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-rw_timeout".to_string(),
        "10000000".to_string(),
        "-analyzeduration".to_string(),
        "3000000".to_string(),
    ];
    cmd.extend(recording::header_input_args(headers));
    cmd.push("-i".to_string());
    cmd.push(fetch_url.to_string());
    let mut media = iptv_core::media_type::MEDIA_UNKNOWN;
    if let Ok(mut child) = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        // Drain stderr on a side thread so a chatty stream
        // cannot fill the pipe and stall the probe.
        let mut pipe = child.stderr.take();
        let drain = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            if let Some(pipe) = &mut pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        });
        let handle = Arc::new(std::sync::Mutex::new(Some(child)));
        let registered = register_probe_child(probe, identity, fetch_url, &handle);
        if !registered {
            if let Some(mut child) = handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = child.kill();
            }
        }
        // Python's 20 s budget: after it, the probe counts as
        // unanswered and nothing is cached.
        let timed_out = wait_for_probe_child(&handle);
        if let Some(mut child) = handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            if timed_out {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let report = drain.join().unwrap_or_default();
        if !timed_out {
            media = iptv_core::media_type::classify_probe_report(&report);
        }
    }
    // Cleanup, Python's worker finally: a registered probe
    // releases the provider settle slot; only a classified
    // result is cached (an unknown answer retries next time).
    let had_proc = probe
        .procs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(identity)
        .is_some();
    probe
        .urls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(identity);
    probe
        .inflight
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(identity);
    if had_proc {
        iptv_core::catchup_direct::note_media_released(fetch_url);
    }
    if media != iptv_core::media_type::MEDIA_UNKNOWN {
        probe.cache.store(
            identity,
            channel_url,
            media,
            iptv_core::media_type::SOURCE_PROBE,
        );
    }
}

/// Register a probe's child handle (Python's `on_popen`
/// rule): only while the identity is still in flight —
/// a session that preempted the probe between its guards
/// and the spawn leaves it unregistered, and the worker
/// kills it rather than holding the slot unowned.
fn register_probe_child(
    probe: &MediaProbe,
    identity: &str,
    fetch_url: &str,
    handle: &ProbeChild,
) -> bool {
    let inflight = probe
        .inflight
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !inflight.contains(identity) {
        return false;
    }
    probe
        .procs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(identity.to_string(), Arc::clone(handle));
    probe
        .urls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(identity.to_string(), fetch_url.to_string());
    true
}

/// Wait for a probe child to exit within Python's 20 s
/// budget; true when the budget ran out (the caller kills it
/// and caches nothing). A handle emptied by preemption counts
/// as finished.
fn wait_for_probe_child(handle: &ProbeChild) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let done = {
            let mut slot = handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match slot.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                None => true,
            }
        };
        if done {
            return false;
        }
        if std::time::Instant::now() >= deadline {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Preempt every in-flight classification probe (Python
/// `_terminate_media_probe`): a user-initiated session
/// (playback, recording, download) takes the provider
/// connection — the probe is only advisory and must never
/// hold a one-stream provider's slot against it.
fn terminate_media_probes(probe: &MediaProbe) {
    probe
        .inflight
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    let procs: Vec<(String, ProbeChild)> = probe
        .procs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain()
        .collect();
    let urls = probe
        .urls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for (identity, handle) in procs {
        if let Some(mut child) = handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(url) = urls.get(&identity) {
            iptv_core::catchup_direct::note_media_released(url);
        }
    }
}

/// The media kind the channel's own metadata states (port of
/// Python `media_hint_from_attributes`): `Some(true)` audio,
/// `Some(false)` video, `None` unstated. Group names are never
/// consulted, like the reference.
fn channel_media_is_audio(channel: &Channel) -> Option<bool> {
    let get = |key: &str| {
        channel
            .unknown_attributes
            .get(key)
            .map_or("", String::as_str)
            .trim()
            .to_lowercase()
    };
    match get("radio").as_str() {
        "true" | "1" | "yes" => return Some(true),
        "false" | "0" | "no" => return Some(false),
        _ => {}
    }
    for key in [
        "media-type",
        "mediatype",
        "media_type",
        "stream-type",
        "streamtype",
        "tvg-type",
        "tvgtype",
    ] {
        match get(key).as_str() {
            "audio" | "radio" | "music" => return Some(true),
            "video" | "tv" | "television" => return Some(false),
            _ => {}
        }
    }
    None
}

/// Python `resolve_recording_format` for one media kind: audio
/// streams take the audio preset (clamped to an audio format);
/// everything else takes the video preset as configured.
fn resolve_format_for_media(persist: &Persist, audio: bool) -> String {
    let config = persist.config.borrow();
    if audio {
        let pref = config.string_or("recording_format_audio", "audio_mp3_v0");
        if recording::format_kind(&pref) == "audio" {
            pref
        } else {
            "audio_mp3_v0".to_string()
        }
    } else {
        config.string_or(
            "recording_format_video",
            recording::DEFAULT_RECORDING_FORMAT,
        )
    }
}

/// The recording format for a channel across every entry point
/// (audit R11, N16): manual, scheduled and archive recordings
/// all classify the channel the same way — metadata, then the
/// probe cache — and use the per-media preset; unclassified
/// channels keep the video preset.
fn recording_format_for_channel(
    persist: &Persist,
    cache: &iptv_core::media_type::MediaTypeCache,
    channel: &Channel,
) -> String {
    let (media, _) = classify_channel_media(cache, channel);
    resolve_format_for_media(persist, media == iptv_core::media_type::MEDIA_AUDIO)
}

/// Python `remap_format_for_media` at capture start: a stream
/// positively identified as audio-only never records with a
/// video preset; anything else keeps the requested format.
fn remap_format_for_media(
    persist: &Persist,
    cache: &iptv_core::media_type::MediaTypeCache,
    channel: &Channel,
    format: &str,
) -> String {
    let (media, _) = classify_channel_media(cache, channel);
    if media == iptv_core::media_type::MEDIA_AUDIO && recording::format_kind(format) == "video" {
        resolve_format_for_media(persist, true)
    } else {
        format.to_string()
    }
}

/// How a recording of a channel picks its audio track (port of
/// Python `_recording_audio_intent`): the playing track when the
/// built-in player is showing this channel (the caller's
/// witness), otherwise the remembered track name/slot, the
/// audio-description preference and the preferred-track list.
/// `None` leaves the choice to ffmpeg.
struct AudioIntent {
    keywords: Vec<String>,
    fallback_index: Option<i64>,
    prefer_ad: bool,
}

fn recording_audio_intent(
    persist: &Persist,
    channel: &Channel,
    witness: Option<(i64, String)>,
) -> Option<AudioIntent> {
    if let Some((index, name)) = witness {
        let described = iptv_core::audiotracks::names_audio_description(&name);
        let mut keywords = Vec::new();
        if described {
            keywords.push("audio description".to_string());
        }
        if !name.trim().is_empty() {
            keywords.push(name);
        }
        return Some(AudioIntent {
            keywords,
            fallback_index: Some(index),
            prefer_ad: described,
        });
    }
    let key = channel_audio_key(channel);
    let config = persist.config.borrow();
    let remembered = config
        .value("channel_audio_tracks")
        .and_then(|v| v.get(&key))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let fallback = config
        .value("channel_audio_track_indices")
        .and_then(|v| v.get(&key))
        .and_then(serde_json::Value::as_i64)
        .filter(|index| (0..64).contains(index));
    let prefer_ad = config.bool_or("prefer_audio_description", false);
    let keywords = iptv_core::audiotracks::ordered_preference_keywords(
        remembered.as_deref(),
        prefer_ad,
        Some(&config.string_or("last_audio_track", "")),
        &config.string_list("preferred_audio_tracks"),
    );
    if keywords.is_empty() && fallback.is_none() {
        return None;
    }
    Some(AudioIntent {
        keywords,
        fallback_index: fallback,
        prefer_ad,
    })
}

/// The audio track the built-in player is currently hearing for
/// this channel, as (audio-stream position, track name) — the
/// R10 witness. Only the libVLC engine reports its tracks.
fn player_audio_witness(
    player: &Rc<RefCell<PlayerState>>,
    channel: &Channel,
) -> Option<(i64, String)> {
    let state = player.borrow();
    let current = state.current_channel.as_ref()?;
    if channel_audio_key(current) != channel_audio_key(channel) || state.session.current().is_none()
    {
        return None;
    }
    let engine = state.vlc.as_ref()?;
    let tracks = engine.audio_tracks();
    let window = state.window.as_ref()?;
    let selection = i64::from(window.tracks.get_selection().unwrap_or(0));
    let (_, name) = tracks.get(usize::try_from(selection).ok()?)?;
    Some((selection, name.clone()))
}

/// One probed audio stream's label, in the words the core
/// keyword engine matches (port of `audio_stream_label`).
fn audio_stream_label(position: usize, language: &str, described: bool) -> String {
    use std::fmt::Write as _;
    let mut label = format!("Track {}", position + 1);
    if !language.is_empty() {
        let _ = write!(label, " - [{language}]");
    }
    if described {
        label.push_str(" - audio description");
    }
    label
}

/// Parse the audio stream labels out of an `ffmpeg -i` report
/// (port of `parse_audio_streams`' label shape).
fn parse_audio_stream_labels(report: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for line in report.lines() {
        let Some(stream_at) = line.find("Stream #") else {
            continue;
        };
        let rest = &line[stream_at + "Stream #".len()..];
        let Some(audio_at) = rest.find(": Audio:") else {
            continue;
        };
        let head = &rest[..audio_at];
        let language = head
            .rfind('(')
            .and_then(|open| head[open + 1..].strip_suffix(')').map(str::to_string))
            .unwrap_or_default();
        let tail = &rest[audio_at + ": Audio:".len()..].to_lowercase();
        let described = tail.contains("descript") || tail.contains("visual impaired");
        labels.push(audio_stream_label(labels.len(), &language, described));
    }
    labels
}

/// Ask a stream which audio tracks it carries, with a plain
/// `ffmpeg -i` probe like Python's (audit R10). Runs on a
/// helper thread with a bounded wait; empty when the stream
/// cannot answer — the caller then keeps ffmpeg's own pick.
fn probe_audio_labels(url: &str, headers: &iptv_core::playback::StreamHeaders) -> Vec<String> {
    let (clean_url, _) = iptv_core::playback::split_stream_modifiers(url);
    let mut cmd: Vec<String> = vec![
        "ffmpeg".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-rw_timeout".to_string(),
        "10000000".to_string(),
        "-analyzeduration".to_string(),
        "3000000".to_string(),
    ];
    let lower = clean_url.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        cmd.extend(recording::header_input_args(headers));
    }
    cmd.push("-i".to_string());
    cmd.push(clean_url);
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let report = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .map(|out| String::from_utf8_lossy(&out.stderr).into_owned())
            .unwrap_or_default();
        let _ = sender.send(report);
    });
    // Python's 20 s budget (audit R10): ffmpeg's own rw_timeout
    // (10 s) bounds a stalled probe thread, and the wait only runs
    // when a remembered audio preference exists with no heard
    // witness — the case where cutting the probe short silently
    // recorded the wrong track.
    receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .map(|report| parse_audio_stream_labels(&report))
        .unwrap_or_default()
}

/// The (track, track count) a recording keeps, or (None, 0) to
/// leave the choice to ffmpeg (port of `_recording_audio_choice`).
fn recording_audio_choice(
    persist: &Persist,
    channel: &Channel,
    probe_url: &str,
    headers: &iptv_core::playback::StreamHeaders,
    witness: Option<(i64, String)>,
) -> (Option<i64>, i64) {
    let Some(intent) = recording_audio_intent(persist, channel, witness) else {
        return (None, 0);
    };
    let labels = probe_audio_labels(probe_url, headers);
    if labels.is_empty() {
        return (None, 0);
    }
    let pairs: Vec<(i64, String)> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| (i64::try_from(index).unwrap_or(0), label.clone()))
        .collect();
    match iptv_core::audiotracks::select_preferred_audio_track(
        &pairs,
        &intent.keywords,
        intent.fallback_index,
        intent.prefer_ad,
    ) {
        Some(index) => (Some(index), i64::try_from(labels.len()).unwrap_or(0)),
        None => (None, 0),
    }
}

impl Wiring {
    fn share_ctx(&self) -> ShareCtx {
        ShareCtx {
            pending_shares: Rc::clone(&self.pending_shares),
            relay_worker: Rc::clone(&self.relay_worker),
        }
    }
}

impl PlayerShared {
    fn share_ctx(&self) -> ShareCtx {
        ShareCtx {
            pending_shares: Rc::clone(&self.pending_shares),
            relay_worker: Rc::clone(&self.relay_worker),
        }
    }
}

/// Whether another app-owned media operation holds this
/// provider's one-stream slot (N13; Python's
/// `_single_stream_provider_busy`). Only known one-stream
/// providers have a key at all; everyone else is never busy.
fn single_stream_busy(
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    player: Option<&Rc<RefCell<PlayerState>>>,
    url: &str,
    include_player: bool,
    cast_source_url: Option<&str>,
) -> bool {
    let key = iptv_core::catchup_direct::single_stream_provider_key(url);
    if key.is_empty() {
        return false;
    }
    if recordings.borrow().has_active_connection(&key) {
        return true;
    }
    {
        let registry = buffers.borrow();
        if registry.has_provider(&key) && !registry.owns_source(url) {
            return true;
        }
    }
    if include_player {
        if let Some(player) = player {
            let state = player.borrow();
            if let Some(request) = state.session.current() {
                if iptv_core::catchup_direct::single_stream_provider_key(&request.url) == key {
                    return true;
                }
            }
        }
    }
    // N20: a live cast holds the provider connection too
    // (Python's caster arm, against the retained source URL).
    if let Some(cast_url) = cast_source_url {
        if !cast_url.is_empty()
            && iptv_core::catchup_direct::single_stream_provider_key(cast_url) == key
        {
            return true;
        }
    }
    false
}

/// True while the built-in player is playing this live channel
/// (N13; Python's `_player_is_showing`).
fn player_is_showing(player: &Rc<RefCell<PlayerState>>, channel: &Channel) -> bool {
    let state = player.borrow();
    let Some(current) = &state.current_channel else {
        return false;
    };
    if channel_key(current) != channel_key(channel) {
        return false;
    }
    state.session.current().is_some_and(|request| request.live)
}

/// Start or stop a manual recording of the channel. `player`,
/// when the caller has it, witnesses the heard audio track
/// (audit R10).
#[allow(clippy::too_many_arguments)] // window parts, like prepare_recording
fn toggle_recording(
    parent: &dyn WxWidget,
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    persist: &Persist,
    media: &MediaProbe,
    channel: &Channel,
    status: Status,
    player: Option<&Rc<RefCell<PlayerState>>>,
    share: &ShareCtx,
) {
    let key = channel_key(channel);
    // N13: a second press while a shared start is still
    // settling cancels it, like Python's pending-start toggle;
    // the settle result then gives the channel back.
    if share.pending_shares.borrow_mut().remove(&key) {
        status.set_label(&trf(
            "Stopping recording for {name}...",
            &[("name", &channel.name)],
        ));
        return;
    }
    if recordings.borrow().is_recording(&key) {
        recordings.borrow_mut().stop(&key);
        release_recording_lease(buffers, &key);
        status.set_label(&trf(
            "Stopping recording of {name}.",
            &[("name", &channel.name)],
        ));
        return;
    }
    if channel.url.is_empty() {
        status.set_label("This channel's stream link must be resolved before recording.");
        return;
    }
    // N16: a recording session preempts any classification
    // probe — the probe must never hold a provider connection
    // against the real session (Python `_terminate_media_probe`
    // at the record entry points).
    terminate_media_probes(media);
    // N13 (issue #13 in the reference): recording the channel
    // the player is showing takes one provider connection, not
    // two — the player lets go, the recording opens the stream,
    // and the player watches the recording's own copy through
    // the relay. A channel already feeding a live buffer needs
    // no share: the recording reads the buffer's view instead.
    let headers = iptv_core::playback::StreamHeaders::for_channel(channel);
    let share_with_player = player.is_some_and(|p| player_is_showing(p, channel))
        && buffers.borrow_mut().find(&channel.url, &headers).is_none();
    let cast_url = player_cast_source(player);
    if single_stream_busy(
        recordings,
        buffers,
        player,
        &channel.url,
        !share_with_player,
        cast_url.as_deref(),
    ) {
        status.set_label(iptv_core::catchup_direct::ONE_STREAM_REFUSAL);
        return;
    }
    if share_with_player {
        let player = player.expect("share implies a player");
        player_stop(player, persist, status);
        share.pending_shares.borrow_mut().insert(key.clone());
        share.relay_worker.start(RelayJob::StartShare {
            key,
            channel: channel.clone(),
        });
        status.set_label(&trf(
            "Starting recording of {name}...",
            &[("name", &channel.name)],
        ));
        return;
    }
    let format = recording_format_for_channel(persist, &media.cache, channel);
    let witness = player.and_then(|player| player_audio_witness(player, channel));
    match start_recording(
        recordings,
        buffers,
        persist,
        &media.cache,
        &key,
        channel,
        &format,
        &channel.name.clone(),
        witness,
        None,
    ) {
        Ok(path) => {
            status.set_label(&trf(
                "Recording {name} to {path}.",
                &[
                    ("name", &channel.name),
                    ("path", &path.display().to_string()),
                ],
            ));
            // N16: Python reports the start in a box, and for
            // an audio-only stream says so — the format was
            // resolved from the audio preference, and the
            // automatic choice is announced rather than silent.
            let (media_kind, _) = classify_channel_media(&media.cache, channel);
            let format_label = iptv_core::recording::RECORDING_FORMATS
                .iter()
                .find(|entry| entry.0 == format)
                .map_or(format.as_str(), |entry| entry.1);
            let path_text = path.display().to_string();
            let message = if media_kind == iptv_core::media_type::MEDIA_AUDIO {
                trf(
                    "Audio-only stream detected. Recording started ({fmt}):\n{path}",
                    &[("fmt", format_label), ("path", &path_text)],
                )
            } else {
                trf(
                    "Recording started ({fmt}):\n{path}",
                    &[("fmt", format_label), ("path", &path_text)],
                )
            };
            let _ = MessageDialog::builder(parent, &message, &tr("Recording"))
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build()
                .show_modal_tracked();
        }
        Err(error) => status.set_label(&error),
    }
}

/// Prepare one recording: destination gate, output path and
/// the ffmpeg command. `shared` adds the tee'd MPEG-TS copy on
/// stdout the recording relay serves (N13).
#[allow(clippy::too_many_arguments)] // channel, programme context and the audio witness
fn prepare_recording(
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    persist: &Persist,
    cache: &iptv_core::media_type::MediaTypeCache,
    key: &str,
    channel: &Channel,
    format: &str,
    display_name: &str,
    witness: Option<(i64, String)>,
    shared: bool,
) -> Result<(PathBuf, Vec<String>), String> {
    let dir = recordings_dir(persist);
    // Audit R08: Python's pre-start destination check on every
    // platform — the folder must exist, accept a real write,
    // and hold at least the configured reserve; unknown free
    // space refuses instead of silently skipping the gate.
    let reserve = persist
        .config
        .borrow()
        .value("recording_min_free_mib")
        .and_then(serde_json::Value::as_i64);
    recording::check_destination(&dir, reserve)?;
    // Audit R11: an audio-only stream never records with a
    // video preset, whatever entry point asked.
    let format = &remap_format_for_media(persist, cache, channel, format);
    let format = format.as_str();
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
    // One borrow, one operation (audit R21: the old shape held the
    // find() borrow across a second borrow_mut and panicked).
    if let Some(view_url) = buffers
        .borrow_mut()
        .recording_view(key, &channel.url, &headers)
    {
        input_url = view_url;
        input_headers = iptv_core::playback::StreamHeaders::default();
    }
    // Audit R10: record the heard/preferred audio track, not
    // ffmpeg's default, when anything asks for a track.
    let (audio_track, audio_track_count) =
        recording_audio_choice(persist, channel, &input_url, &input_headers, witness);
    let cmd = recording::ffmpeg_command(
        "ffmpeg",
        &input_url,
        &out.to_string_lossy(),
        format,
        &input_headers,
        &recording::FfmpegOptions {
            show_stats: true,
            // N12: time-stamped log lines are what place each
            // problem in time in the recording's log summary;
            // probed once per ffmpeg path.
            log_datetime: iptv_core::recording_log::ffmpeg_supports_log_datetime("ffmpeg"),
            audio_track,
            audio_track_count,
            copy_to_stdout: shared,
            ..recording::FfmpegOptions::default()
        },
    );
    Ok((out, cmd))
}

/// Spawn ffmpeg for one recording; returns the output path.
/// `witness` is the audio track the built-in player is hearing
/// for this channel, when a caller has it (audit R10).
#[allow(clippy::too_many_arguments)] // channel, programme context and the audio witness
fn start_recording(
    recordings: &Rc<RefCell<RecordingEngine>>,
    buffers: &Rc<RefCell<buffer::BufferRegistry>>,
    persist: &Persist,
    cache: &iptv_core::media_type::MediaTypeCache,
    key: &str,
    channel: &Channel,
    format: &str,
    display_name: &str,
    witness: Option<(i64, String)>,
    planned_seconds: Option<f64>,
) -> Result<PathBuf, String> {
    let (out, cmd) = prepare_recording(
        recordings,
        buffers,
        persist,
        cache,
        key,
        channel,
        format,
        display_name,
        witness,
        false,
    )?;
    let meta = recording::RecordingStartMeta {
        title: display_name.to_string(),
        format: format.to_string(),
        planned_seconds,
    };
    if let Err(error) = recordings.borrow_mut().start(key, &cmd, out.clone(), meta) {
        release_recording_lease(buffers, key);
        return Err(error);
    }
    recordings.borrow_mut().set_connection_key(
        key,
        &iptv_core::catchup_direct::single_stream_provider_key(&channel.url),
    );
    Ok(out)
}

/// Spawn the shared recording behind the relay (N13): ffmpeg
/// tees a copy of the stream to its stdout, the relay serves
/// it, and the caller points the player at the relay URL.
fn start_shared_recording(
    wiring: &Wiring,
    key: &str,
    channel: &Channel,
) -> Result<(PathBuf, std::sync::Arc<relay::RecordingRelay>), String> {
    let format = recording_format_for_channel(&wiring.persist, &wiring.media_probe.cache, channel);
    let witness = player_audio_witness(&wiring.player, channel);
    let (out, cmd) = prepare_recording(
        &wiring.recordings,
        &wiring.buffers,
        &wiring.persist,
        &wiring.media_probe.cache,
        key,
        channel,
        &format,
        &channel.name.clone(),
        witness,
        true,
    )?;
    let meta = recording::RecordingStartMeta {
        title: channel.name.clone(),
        format: format.clone(),
        planned_seconds: None,
    };
    let stdout = match wiring
        .recordings
        .borrow_mut()
        .start_shared(key, &cmd, out.clone(), meta)
    {
        Ok(stdout) => stdout,
        Err(error) => {
            release_recording_lease(&wiring.buffers, key);
            return Err(error);
        }
    };
    wiring.recordings.borrow_mut().set_connection_key(
        key,
        &iptv_core::catchup_direct::single_stream_provider_key(&channel.url),
    );
    match relay::RecordingRelay::new(stdout) {
        Ok(relay) => Ok((out, std::sync::Arc::new(relay))),
        Err(error) => {
            wiring.recordings.borrow_mut().stop(key);
            release_recording_lease(&wiring.buffers, key);
            Err(error)
        }
    }
}

/// Play a channel straight from the provider again after a
/// shared recording (N13), when the player is idle: something
/// the user started meanwhile is never displaced.
fn resume_direct_playback(wiring: &Wiring, channel: &Channel, status: Status) {
    if wiring.player.borrow().session.current().is_some() {
        return;
    }
    let request = iptv_core::playback::PlaybackRequest::for_channel(channel, None);
    play_builtin(&wiring.player, &wiring.persist, channel, request, status);
}

/// Drain shared-playback settle results (N13): start the relay
/// recording once the player's connection has settled, hand
/// the player the relay URL, and resume direct playback when a
/// shared recording ends or a share start was cancelled.
fn poll_relay_jobs(wiring: &Wiring, status: Status) {
    while let Some(job) = wiring.relay_worker.try_recv() {
        match job {
            RelayJob::StartShare { key, channel } => {
                if !wiring.pending_shares.borrow_mut().remove(&key) {
                    // Cancelled while settling: give the
                    // channel back to the player directly.
                    resume_direct_playback(wiring, &channel, status);
                    continue;
                }
                match start_shared_recording(wiring, &key, &channel) {
                    Ok((path, relay)) => {
                        let relay_url = relay.url.clone();
                        wiring.shared_playback.borrow_mut().insert(
                            key,
                            SharedPlayback {
                                channel: channel.clone(),
                                relay_url: relay_url.clone(),
                                relay,
                            },
                        );
                        let mut request =
                            iptv_core::playback::PlaybackRequest::for_channel(&channel, None);
                        request.url = relay_url;
                        request.headers = iptv_core::playback::StreamHeaders::default();
                        request.live = true;
                        request.buffer_bypass = true;
                        play_builtin(&wiring.player, &wiring.persist, &channel, request, status);
                        status.set_label(&trf(
                            "Recording {name} to {path}.",
                            &[
                                ("name", &channel.name),
                                ("path", &path.display().to_string()),
                            ],
                        ));
                    }
                    Err(error) => {
                        status.set_label(&error);
                        resume_direct_playback(wiring, &channel, status);
                    }
                }
            }
            RelayJob::EndShare { channel } => {
                resume_direct_playback(wiring, &channel, status);
            }
        }
    }
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
                status.set_label(&trf(
                    "Resolving the stream link for the scheduled recording of {name}.",
                    &[("name", &channel.name)],
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
    // N13: a one-stream provider whose only connection is in
    // use refuses the scheduled recording with the plain-words
    // reason, like Python's `_start_scheduled_recording`.
    let cast_url = cast_source_url(&wiring.cast_ui);
    if single_stream_busy(
        &wiring.recordings,
        &wiring.buffers,
        Some(&wiring.player),
        &channel.url,
        true,
        cast_url.as_deref(),
    ) {
        wiring.dvr.borrow_mut().mark_finished(
            &job.id,
            false,
            "",
            iptv_core::catchup_direct::ONE_STREAM_REFUSAL,
            now,
        );
        return;
    }
    // N16: the scheduled session preempts classification
    // probes like a manual one.
    terminate_media_probes(&wiring.media_probe);
    match start_recording(
        &wiring.recordings,
        &wiring.buffers,
        &wiring.persist,
        &wiring.media_probe.cache,
        &format!("dvr:{}", job.id),
        channel,
        &job.format.clone(),
        &display,
        None,
        // N12: the job's span is the capture's planned length.
        Some((job.stop_ts - job.start_ts).max(0.0)),
    ) {
        Ok(_) => status.set_label(&trf("Recording {display}.", &[("display", &display)])),
        Err(error) => {
            wiring
                .dvr
                .borrow_mut()
                .mark_finished(&job.id, false, "", &error, now);
        }
    }
}

/// The account a DVR job's channel belongs to, with the
/// account's provider id when it has one (issue #134).
fn job_account(
    job: &iptv_core::dvr::DvrJob,
    accounts: &[ProviderAccount],
) -> Option<(String, Option<String>)> {
    let channel = channel_from_snapshot(&job.channel);
    let key = iptv_core::providers::channel_account_key(&channel, accounts)?;
    let provider_id = accounts
        .iter()
        .find(|a| a.account_key() == key)
        .and_then(ProviderAccount::provider_id);
    Some((key, provider_id))
}

/// Whether a recording of this job's channel would read an
/// existing buffer session instead of opening a new provider
/// connection (the sharing `start_recording` already performs).
fn job_shares_buffer(wiring: &Wiring, job: &iptv_core::dvr::DvrJob) -> bool {
    let channel = channel_from_snapshot(&job.channel);
    let (clean_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
    let headers = iptv_core::playback::StreamHeaders::for_channel(&channel);
    let key = buffer::source_key(&clean_url, &headers);
    wiring.buffers.borrow().sessions.contains_key(&key)
}

/// Connections this app currently holds on one account:
/// playback (direct, or through the buffer that feeds it), plus
/// DVR recordings that are not reading a buffer lease. Other
/// apps' and devices' connections are unknowable and never
/// assumed (issue #134).
fn account_connections_in_use(wiring: &Wiring, accounts: &[ProviderAccount], key: &str) -> usize {
    let mut used = 0;
    {
        let player = wiring.player.borrow();
        if player.session.current().is_some() {
            if let Some(channel) = &player.current_channel {
                if iptv_core::providers::channel_account_key(channel, accounts).as_deref()
                    == Some(key)
                {
                    used += 1;
                }
            }
        }
    }
    let leased: Vec<String> = wiring
        .buffers
        .borrow()
        .recording_leases
        .keys()
        .cloned()
        .collect();
    for job in wiring.dvr.borrow().jobs() {
        if job.status != iptv_core::dvr::STATUS_RECORDING {
            continue;
        }
        if leased.iter().any(|k| k == &format!("dvr:{}", job.id)) {
            continue;
        }
        if job_account(job, accounts).map(|(k, _)| k).as_deref() == Some(key) {
            used += 1;
        }
    }
    used
}

/// Before the DVR tick: release stale capacity holds, and for a
/// due job whose account limit is unknown, hold it and fetch the
/// account info once (the answer fills `account_limits` in the
/// provider event drain). A job is never blocked on an answer
/// that does not come: holds expire and the job proceeds without
/// an assumed capacity.
fn prepare_capacity_holds(wiring: &Wiring, accounts: &[ProviderAccount], now: f64) {
    let expired: Vec<String> = wiring
        .limit_holds
        .borrow()
        .iter()
        .filter(|(_, expiry)| now >= **expiry)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        wiring.dvr.borrow_mut().release_hold(&id);
        wiring.limit_holds.borrow_mut().remove(&id);
    }
    let due: Vec<iptv_core::dvr::DvrJob> = wiring
        .dvr
        .borrow()
        .jobs()
        .iter()
        .filter(|job| {
            matches!(
                job.status.as_str(),
                iptv_core::dvr::STATUS_SCHEDULED | iptv_core::dvr::STATUS_WAITING
            ) && now >= job.start_ts
                && (job.stop_ts <= 0.0 || now < job.stop_ts)
        })
        .cloned()
        .collect();
    for job in due {
        if wiring.dvr.borrow().is_held(&job.id) {
            continue;
        }
        let Some((key, provider_id)) = job_account(&job, accounts) else {
            continue;
        };
        if wiring.account_limits.borrow().contains_key(&key) {
            continue;
        }
        let Some(provider_id) = provider_id else {
            continue;
        };
        wiring.dvr.borrow_mut().hold_job(&job.id);
        wiring
            .limit_holds
            .borrow_mut()
            .insert(job.id.clone(), now + 45.0);
        if wiring.limit_fetch.borrow().is_none() {
            *wiring.limit_fetch.borrow_mut() = Some(provider_id.clone());
            wiring.provider_worker.account(provider_id);
            wiring.provider_pending.set(true);
            assert!(wiring.timer.start(50, false));
        }
    }
}

/// One DVR state-machine step (every 10 s): capacity holds,
/// the gated tick, and its actions.
fn run_dvr_tick(wiring: &Wiring, status: Status, now: f64) {
    let (_, accounts) = configured_entries(wiring);
    prepare_capacity_holds(wiring, &accounts, now);
    // The capacity gate (issue #134): a due job waits when
    // its account's known limit is already fully used by
    // playback or recordings and the job cannot share an
    // existing buffer stream. Unknown limits never block.
    let playing: Option<(String, String)> = {
        let player = wiring.player.borrow();
        match (&player.current_channel, player.session.current()) {
            (Some(channel), Some(_)) => {
                iptv_core::providers::channel_account_key(channel, &accounts)
                    .map(|key| (key, channel.name.clone()))
            }
            _ => None,
        }
    };
    // In-use counts are snapshotted before the tick: the
    // gate runs while the schedule is borrowed.
    let used_by_account: std::collections::BTreeMap<String, usize> = accounts
        .iter()
        .map(|account| {
            let key = account.account_key();
            let used = account_connections_in_use(wiring, &accounts, &key);
            (key, used)
        })
        .collect();
    // Starts granted earlier in this same tick count against
    // later jobs: the in-use snapshot cannot see them yet, and
    // two waiting jobs must never both take one freed slot
    // (issue #135).
    let mut started_this_tick: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut gate = |job: &iptv_core::dvr::DvrJob| -> Option<String> {
        let (key, _) = job_account(job, &accounts)?;
        if job_shares_buffer(wiring, job) {
            return None;
        }
        if wiring.conflict_override.borrow().contains(&job.id) {
            *started_this_tick.entry(key).or_insert(0) += 1;
            return None;
        }
        let limit = wiring.account_limits.borrow().get(&key).copied()??;
        let used = used_by_account.get(&key).copied().unwrap_or(0)
            + started_this_tick.get(&key).copied().unwrap_or(0);
        if used < usize::try_from(limit).unwrap_or(0) {
            *started_this_tick.entry(key).or_insert(0) += 1;
            return None;
        }
        Some(match &playing {
            Some((playing_key, name)) if *playing_key == key => {
                format!("All {limit} provider connection(s) are in use; {name} is playing.")
            }
            _ => format!("All {limit} provider connection(s) are in use."),
        })
    };
    let actions = wiring.dvr.borrow_mut().tick_gated(now, &mut gate);
    run_dvr_actions(wiring, status, now, actions);
    retry_dvr_resolves(wiring, now);
}

/// Carry out one DVR tick's actions: resolutions, stops,
/// starts, and conflict announcements.
fn run_dvr_actions(wiring: &Wiring, status: Status, now: f64, actions: Vec<DvrAction>) {
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
            DvrAction::Start(job_id) => {
                wiring.conflict_override.borrow_mut().remove(&job_id);
                start_dvr_job(wiring, status, &job_id, now);
            }
            DvrAction::Conflict(job_id) => {
                // Announced once, on the transition into
                // waiting; playback keeps running and no
                // dialog opens (no focus theft). The decision
                // waits in Scheduled Recordings (issue #134).
                if let Some(job) = wiring.dvr.borrow().get(&job_id).cloned() {
                    let name = if job.title.is_empty() {
                        job.channel_name.clone()
                    } else {
                        job.title.clone()
                    };
                    player_status(
                    status,
                    &wiring.persist,
                    &format!(
                        "Recording conflict: {name} is waiting for a provider connection. Playback continues; open Scheduled Recordings to decide."
                    ),
                    1,
                );
                }
            }
        }
    }
}

/// Parse the media duration out of an `ffmpeg -i` report
/// ("Duration: 01:02:03.50"), for the catch-up short-capture
/// check (audit R05).
fn parse_ffmpeg_duration(report: &str) -> Option<f64> {
    let at = report.find("Duration: ")? + "Duration: ".len();
    let text = report[at..].split(',').next()?.trim();
    let mut parts = text.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// The duration of the media in a finished capture file, when
/// ffmpeg can read it back.
fn media_duration_seconds(path: &std::path::Path) -> Option<f64> {
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;
    parse_ffmpeg_duration(&String::from_utf8_lossy(&output.stderr))
}

/// N13: when a shared recording (one whose stream the player is
/// watching through the relay) ends, close its relay; if the player
/// is still on the relay URL, stop it and resume the channel
/// directly once the connection has settled.
fn end_shared_playback(wiring: &Wiring, status: Status, key: &str) {
    let Some(shared) = wiring.shared_playback.borrow_mut().remove(key) else {
        return;
    };
    shared.relay.close();
    let on_relay = {
        let state = wiring.player.borrow();
        state
            .session
            .current()
            .is_some_and(|request| request.url == shared.relay_url)
    };
    if on_relay {
        player_stop(&wiring.player, &wiring.persist, status);
        wiring.relay_worker.start(RelayJob::EndShare {
            channel: shared.channel.clone(),
        });
    }
}

/// Per-tick recording housekeeping: collect finishes, run the DVR
/// state machine (every 10 s, like the reference poll), and apply the
/// shutdown-after-recordings gate.
fn poll_recordings(wiring: &Wiring, frame: Frame, status: Status) {
    let now = now_seconds();
    for finished in wiring.recordings.borrow_mut().poll_finished() {
        release_recording_lease(&wiring.buffers, &finished.key);
        wiring.recorded.set(true);
        // Audit R09: ffmpeg exiting 0 is not a recording — an
        // empty or missing output is "Nothing was recorded",
        // never a saved success, for manual and DVR alike.
        let produced = recording::output_has_media(&finished.out_path);
        let success = finished.success && produced;
        if let Some(staged) = wiring.staged_downloads.borrow_mut().remove(&finished.key) {
            finish_staged_download(
                wiring,
                frame,
                status,
                &finished.key,
                staged,
                &finished,
                produced,
            );
            continue;
        }
        // N13: a shared recording ended; close its relay and
        // resume direct playback if the player was watching
        // through it (Python's `_end_shared_playback`).
        end_shared_playback(wiring, status, &finished.key);
        let name = finished
            .out_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(job_id) = finished.key.strip_prefix("dvr:") {
            // Python's DVR finish message: the failure detail
            // when ffmpeg failed, the nothing-recorded sentence
            // when it wrote nothing, empty when it saved.
            let message = if finished.exit_code != 0 {
                finished
                    .stderr_tail
                    .iter()
                    .rev()
                    .take(6)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if !produced {
                "Nothing was recorded: the stream ended before it wrote anything.".to_string()
            } else {
                String::new()
            };
            wiring.dvr.borrow_mut().mark_finished(
                job_id,
                success,
                &finished.out_path.to_string_lossy(),
                &message,
                now,
            );
        }
        // N12: Python's outcome report (a dialog, in Python's
        // four-way wording) for every recording the downloads
        // window does not already report on.
        report_recording_finished(frame, &finished, produced);
        if success {
            let size = std::fs::metadata(&finished.out_path).map_or(0, |m| m.len());
            let mb = format!("{}.{}", size / 1_000_000, (size % 1_000_000) / 100_000);
            status.set_label(&trf(
                "Recording saved: {name} ({mb} MB)",
                &[("name", &name), ("mb", &mb)],
            ));
        } else if finished.success {
            status.set_label("Nothing was recorded: the stream ended before it wrote anything.");
        } else {
            iptv_core::applog::error("recording", &format!("Recording failed: {name}"));
            status.set_label(&trf("Recording failed: {name}", &[("name", &name)]));
        }
    }
    if now - wiring.last_dvr_tick.get() >= 10.0 {
        wiring.last_dvr_tick.set(now);
        run_dvr_tick(wiring, status, now);
        if now - wiring.series_last_scan.get() >= 3600.0 {
            run_series_scan(wiring);
        }
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
        open_shutdown_countdown(wiring, status);
    }
}

/// What ffmpeg said, plus where the rest of what it said was
/// written (N12; Python `_recording_failure_detail`): the totals
/// line first when anything was logged, the last six problem
/// lines, and the log's path when it exists.
fn recording_failure_detail(finished: &recording::RecordingFinished) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some((warnings, errors, fatals)) = finished.problem_counts {
        if warnings + errors + fatals > 0 {
            lines.push(trf(
                "{warnings} warnings, {errors} errors and {fatals} fatal errors were logged in total.",
                &[
                    ("warnings", &warnings.to_string()),
                    ("errors", &errors.to_string()),
                    ("fatals", &fatals.to_string()),
                ],
            ));
        }
    }
    let tail: Vec<&String> = finished.stderr_tail.iter().rev().take(6).rev().collect();
    if tail.is_empty() {
        lines.push(tr("No ffmpeg details were reported."));
    } else {
        lines.extend(tail.iter().map(|line| (*line).clone()));
    }
    if let Some(log) = &finished.log_path {
        if log.exists() {
            lines.push(String::new());
            lines.push(trf(
                "Full ffmpeg log:\n{path}",
                &[("path", &log.display().to_string())],
            ));
        }
    }
    lines.join("\n")
}

/// Python's recording outcome report (N12; `client_recording.py`):
/// saved, nothing recorded, stopped-by-user warning, or error,
/// each a dialog in Python's wording.
fn report_recording_finished(
    frame: Frame,
    finished: &recording::RecordingFinished,
    produced: bool,
) {
    let rc = finished.exit_code;
    let code = rc.to_string();
    let path = finished.out_path.to_string_lossy().into_owned();
    // N18: a recording can finish while another modal is up;
    // the report queues behind it (Python queues these).
    let show = |message: String, title: &str, style: MessageDialogStyle| {
        show_or_queue_notification(frame, &message, &tr(title), style, None);
    };
    if rc == 0 && produced {
        show(
            trf("Recording saved:\n{path}", &[("path", &path)]),
            "Recording Complete",
            MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
        );
    } else if rc == 0 && !finished.stopped_by_user {
        show(
            tr("Nothing was recorded: the stream ended before it wrote anything."),
            "Recording Warning",
            MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
        );
    } else if finished.stopped_by_user {
        show(
            trf(
                "Recording stopped, but ffmpeg reported code {code}.\n\n{detail}\n\nFile:\n{path}",
                &[
                    ("code", &code),
                    ("detail", &recording_failure_detail(finished)),
                    ("path", &path),
                ],
            ),
            "Recording Warning",
            MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
        );
    } else {
        let mut detail = recording_failure_detail(finished);
        if finished.format == "provider_mka" {
            // The MKA preset copies the audio verbatim and never
            // transcodes: say so plainly and point at another
            // audio format instead of silently substituting one.
            detail = trf(
                "The audio could not be copied into an MKA container, so nothing was recorded and nothing was transcoded. Choose another audio format from Recordings > Recording Format and try again.\n\n{detail}",
                &[("detail", &detail)],
            );
        }
        show(
            trf(
                "Recording of {name} ended unexpectedly (code {code}).\n\n{detail}",
                &[
                    ("name", &finished.title),
                    ("code", &code),
                    ("detail", &detail),
                ],
            ),
            "Recording Error",
            MessageDialogStyle::OK | MessageDialogStyle::IconError,
        );
    }
}

/// Fill series rules from the guide (audit R16; Python
/// `_refresh_series_recordings`): for every non-canceled series
/// job, add one job per matching future episode the guide
/// holds for its channel (same title, case-insensitive),
/// skipping episodes already scheduled. Runs hourly and
/// whenever a series rule is newly authored.
fn run_series_scan(wiring: &Wiring) {
    let now = now_seconds();
    wiring.series_last_scan.set(now);
    let rules: Vec<iptv_core::dvr::DvrJob> = wiring
        .dvr
        .borrow()
        .jobs()
        .iter()
        .filter(|job| job.repeat == "series" && job.status != iptv_core::dvr::STATUS_CANCELED)
        .cloned()
        .collect();
    if rules.is_empty() {
        return;
    }
    for rule in &rules {
        let channel = channel_from_snapshot(&rule.channel);
        let at = now_epoch();
        submit_epg_read(
            wiring,
            iptv_core::epg::EpgReadKind::Programmes {
                channel,
                from: at - 7 * 86_400,
                to: at + 7 * 86_400,
            },
            PendingEpgReadUi::SeriesProgrammes {
                rule: Box::new(rule.clone()),
                now,
            },
        );
    }
}

/// The VOD browser window's handles and view state.
#[derive(Clone)]
struct VodUi {
    frame: Frame,
    search: TextCtrl,
    groups: ListBox,
    items: ListBox,
    details: TextCtrl,
    status: Status,
    view: Rc<RefCell<VodView>>,
}

#[derive(Default)]
struct VodView {
    catalog: VodCatalog,
    accounts: Vec<ProviderAccount>,
    showing: Vec<VodItem>,
    /// Drill-in history: the previous list and the row that was
    /// opened from it, so Back restores the same selection
    /// (spec 0006).
    stack: Vec<(Vec<VodItem>, usize)>,
    /// The applied search query (spec 0006); empty = unfiltered.
    query: String,
    /// The group retained while a query is active; clearing the
    /// query restores this group's unfiltered items.
    group_label: Option<String>,
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
    let ui = ensure_vod_ui(wiring);
    ui.frame.show(true);
    ui.frame.raise();
    let accounts = vod_accounts(&wiring.persist);
    ui.view.borrow_mut().accounts.clone_from(&accounts);
    // B17: the catalogue is limited to the active playlist scope;
    // B19: the worker receives the model Arc + extras and does the
    // filtering/cloning on its own thread.
    let (model, extras) = wiring.browser.borrow().catalogue_parts();
    let scope = wiring.browser.borrow().scope().map(str::to_owned);
    // Spec 0006: the M3U source labels keyed by scope id, derived
    // from the configured playlists and their friendly names.
    let (sources, _) = configured_entries(wiring);
    let names = playlist_names(wiring);
    let source_labels: Vec<(String, String)> = sources
        .iter()
        .map(|source| {
            (
                source.scope_id(),
                source_display_label(source.location(), &names),
            )
        })
        .collect();
    wiring
        .vod_worker
        .build(accounts, model, extras, scope, source_labels);
    wiring.vod_pending.set(true);
    assert!(wiring.timer.start(50, false));
    status.set_label("Loading video on demand catalogue...");
}

/// Build the VOD window's widgets: (frame, search, groups, items,
/// item details, play, back, status).
#[allow(clippy::type_complexity)]
fn build_vod_window() -> (
    Frame,
    TextCtrl,
    ListBox,
    ListBox,
    TextCtrl,
    Button,
    Button,
    Status,
) {
    let frame = Frame::builder()
        .with_title("Video on Demand")
        .with_size(Size::new(720, 520))
        .build();
    frame.set_accessibility_label("Video on Demand");
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    // Spec 0006: the search applies only on Enter; editing the
    // field alone changes nothing.
    let search_label = StaticText::builder(&panel).with_label("&Search").build();
    let _ = search_label;
    let search = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    search.set_accessibility_label("Search VOD");
    let groups = ListBox::builder(&panel).build();
    groups.set_accessibility_label("VOD groups");
    let items = ListBox::builder(&panel).build();
    items.set_accessibility_label("VOD items");
    // Spec 0006: the read-only item-details control; the stream
    // URL appears here and nowhere else.
    let details_label = StaticText::builder(&panel)
        .with_label("Item details")
        .build();
    let _ = details_label;
    let item_details_box = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("No item selected.")
        .build();
    item_details_box.set_accessibility_label("Item details");
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
    (
        frame,
        search,
        groups,
        items,
        item_details_box,
        play,
        back,
        status,
    )
}

/// Build the VOD window once and keep it (closing hides it).
#[allow(clippy::too_many_lines)]
fn ensure_vod_ui(wiring: &Wiring) -> VodUi {
    let slot = &wiring.vod_ui;
    if let Some(ui) = slot.borrow().as_ref() {
        return ui.clone();
    }
    let player = &wiring.player;
    let persist = &wiring.persist;
    let worker = &wiring.vod_worker;
    let pending = &wiring.vod_pending;
    let timer = &wiring.timer;
    let (frame, search, groups, items, item_details_box, play, back, status) = build_vod_window();
    let view = Rc::new(RefCell::new(VodView::default()));
    let ui = VodUi {
        frame,
        search,
        groups,
        items,
        details: item_details_box,
        status,
        view,
    };
    // Group selection shows that group's items. Choosing a group
    // leaves search mode (spec 0006): the filter and the field
    // both reset, and the group becomes the retained group.
    {
        let view = Rc::clone(&ui.view);
        let groups_box = groups;
        let items_box = items;
        let search_box = search;
        let worker = Rc::clone(worker);
        let wiring = wiring.clone();
        groups.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
            // B18: changing group supersedes an in-flight episode reply.
            worker.invalidate();
            let Some(selection) = groups_box.get_selection() else {
                return;
            };
            let mut view = view.borrow_mut();
            let Some(label) = view.catalog.order.get(selection as usize).cloned() else {
                return;
            };
            if !view.query.is_empty() {
                view.query.clear();
                search_box.set_value("");
            }
            view.group_label = Some(label.clone());
            view.showing = view.catalog.groups.get(&label).cloned().unwrap_or_default();
            view.stack.clear();
            show_items(items_box, &view.showing);
            let first = (!view.showing.is_empty()).then_some(0);
            refresh_vod_details(&wiring, &view, item_details_box, first);
        });
    }
    // Search (spec 0006): Enter applies the query against the
    // retained catalogue on the worker; focus stays in the field.
    // A blank query clears the filter and restores the retained
    // group's unfiltered items.
    {
        let view = Rc::clone(&ui.view);
        let groups_box = groups;
        let items_box = items;
        let search_box = search;
        let worker = Rc::clone(worker);
        let pending = Rc::clone(pending);
        let timer = Rc::clone(timer);
        let wiring = wiring.clone();
        search.bind_internal(EventType::TEXT_ENTER, move |_| {
            let query = search_box.get_value().trim().to_string();
            if query.is_empty() {
                let mut view = view.borrow_mut();
                if view.query.is_empty() {
                    return;
                }
                view.query.clear();
                view.stack.clear();
                let label = view
                    .group_label
                    .clone()
                    .or_else(|| view.catalog.order.first().cloned());
                if let Some(label) = label {
                    if let Some(position) =
                        view.catalog.order.iter().position(|group| group == &label)
                    {
                        groups_box.set_selection(u32::try_from(position).unwrap_or(0), true);
                    }
                    view.showing = view.catalog.groups.get(&label).cloned().unwrap_or_default();
                    status.set_label(&trf(
                        "Search cleared. Showing {group}.",
                        &[("group", &label)],
                    ));
                }
                show_items(items_box, &view.showing);
                let first = (!view.showing.is_empty()).then_some(0);
                refresh_vod_details(&wiring, &view, item_details_box, first);
                return;
            }
            {
                let mut view = view.borrow_mut();
                if view.query.is_empty() {
                    // Retain the group to restore when the filter
                    // is cleared; the groups selection still names
                    // it even while drilled into a series.
                    if let Some(selection) = groups_box.get_selection() {
                        view.group_label = view.catalog.order.get(selection as usize).cloned();
                    }
                }
                view.query.clone_from(&query);
            }
            worker.search(query);
            pending.set(true);
            assert!(timer.start(50, false));
        });
    }
    // Item selection updates the details control in place
    // (spec 0006): no focus movement.
    {
        let view = Rc::clone(&ui.view);
        let items_box = items;
        let wiring = wiring.clone();
        items.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
            let Some(selection) = items_box.get_selection() else {
                return;
            };
            let view = view.borrow();
            refresh_vod_details(&wiring, &view, item_details_box, Some(selection as usize));
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
        let wiring = wiring.clone();
        play.on_click(move |_| {
            let Some(selection) = items_box.get_selection() else {
                return;
            };
            let item = view.borrow().showing.get(selection as usize).cloned();
            let Some(item) = item else { return };
            match item.kind {
                VodKind::Movie => {
                    if let Some(channel) = &item.channel {
                        play_channel(&player, &persist, &wiring.media_probe, channel, status);
                    }
                }
                VodKind::Series => {
                    if !item.episodes.is_empty() {
                        let mut view = view.borrow_mut();
                        let previous = std::mem::take(&mut view.showing);
                        view.stack.push((previous, selection as usize));
                        view.showing.clone_from(&item.episodes);
                        show_items(items_box, &view.showing);
                        let first = (!view.showing.is_empty()).then_some(0);
                        refresh_vod_details(&wiring, &view, item_details_box, first);
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
    // Back: return to the previous item list with the row that was
    // opened from it reselected (spec 0006).
    {
        let view = Rc::clone(&ui.view);
        let items_box = items;
        let worker = Rc::clone(worker);
        let wiring = wiring.clone();
        back.on_click(move |_| {
            worker.invalidate();
            let mut view = view.borrow_mut();
            if let Some((previous, selection)) = view.stack.pop() {
                view.showing = previous;
                show_items_at(items_box, &view.showing, selection);
                refresh_vod_details(&wiring, &view, item_details_box, Some(selection));
            }
        });
    }
    // Closing the VOD window hides it; the window is reused.
    {
        let frame_hide = frame;
        let worker = Rc::clone(worker);
        frame.on_close(move |event| {
            // B18: a late reply must not reopen obsolete navigation.
            worker.invalidate();
            frame_hide.hide();
            event.skip(false);
        });
    }
    *slot.borrow_mut() = Some(ui.clone());
    ui
}

/// Fill the items list from `VodItem`s and select the first row.
fn show_items(items_box: ListBox, showing: &[VodItem]) {
    show_items_at(items_box, showing, 0);
}

/// Fill the items list and select `selection` (Back restores the
/// row the user opened, spec 0006).
fn show_items_at(items_box: ListBox, showing: &[VodItem], selection: usize) {
    let names: Vec<String> = showing.iter().map(|i| i.name.clone()).collect();
    fill_items(items_box, &names, selection);
}

/// Fill the items list with search-result rows (title, type,
/// group, source in the accessible row text) and select the
/// first row.
fn show_hit_rows(items_box: ListBox, hits: &[VodSearchHit]) {
    let labels: Vec<String> = hits.iter().map(VodSearchHit::row_label).collect();
    fill_items(items_box, &labels, 0);
}

fn fill_items(items_box: ListBox, labels: &[String], selection: usize) {
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    items_box.set_items(&refs);
    if !refs.is_empty() {
        let index = selection.min(refs.len() - 1);
        items_box.set_selection(u32::try_from(index).unwrap_or(0), true);
    }
}

/// The EPG description for an item's channel when one is already
/// available (spec 0006): the current programme's description.
fn vod_item_description(wiring: &Wiring, item: &VodItem) -> Option<String> {
    let channel = item.channel.as_ref()?;
    guide_now_next(wiring, channel)?
        .current
        .map(|programme| programme.description)
}

/// Refresh the item-details control for the row at `selection`
/// (spec 0006): read-only text, no focus movement; the stream
/// URL reaches only this control.
fn refresh_vod_details(
    wiring: &Wiring,
    view: &VodView,
    details: TextCtrl,
    selection: Option<usize>,
) {
    let text = selection
        .and_then(|index| view.showing.get(index))
        .map_or_else(
            || "No item selected.".to_string(),
            |item| {
                let description = vod_item_description(wiring, item);
                item_details(item, description.as_deref())
                    .lines()
                    .join("\n")
            },
        );
    details.set_value(&text);
}

/// Collect VOD worker events into the window.
fn poll_vod(wiring: &Wiring, status: Status) {
    while let Some(event) = wiring.vod_worker.try_recv() {
        // B18: a reply from a superseded build/episode request must
        // never replace newer navigation.
        // A search answers at the build generation it was issued
        // under; a rebuild supersedes it, like a catalogue reply.
        let current = match &event {
            VodEvent::Catalog { generation, .. } | VodEvent::SearchResults { generation, .. } => {
                wiring.vod_worker.is_current_catalog(*generation)
            }
            VodEvent::Episodes { token, .. } => wiring.vod_worker.is_current_episodes(*token),
        };
        if !current {
            continue;
        }
        wiring.vod_pending.set(false);
        let Some(ui) = wiring.vod_ui.borrow().clone() else {
            continue;
        };
        match event {
            VodEvent::Catalog { catalog, .. } => {
                let groups_count = catalog.order.len();
                let items_count: usize = catalog.groups.values().map(Vec::len).sum();
                let labels = catalog.order.clone();
                {
                    let mut view = ui.view.borrow_mut();
                    view.catalog = catalog;
                    // A reload supersedes any applied search
                    // (spec 0006): filter, field, and drill-in
                    // state all reset.
                    view.query.clear();
                    view.group_label = labels.first().cloned();
                    view.stack.clear();
                }
                ui.search.set_value("");
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                ui.groups.set_items(&refs);
                if !refs.is_empty() {
                    ui.groups.set_selection(0, true);
                }
                ui.status.set_label(&trf(
                    "VOD catalogue ready: {groups} groups, {items} items.",
                    &[
                        ("groups", &groups_count.to_string()),
                        ("items", &items_count.to_string()),
                    ],
                ));
                status.set_label("VOD catalogue loaded.");
            }
            VodEvent::SearchResults { query, hits, .. } => {
                apply_vod_search_results(wiring, &ui, &query, &hits);
            }
            VodEvent::Episodes {
                result: Ok(episodes),
                ..
            } => {
                let count = episodes.len();
                let selection = ui.items.get_selection().map_or(0, |row| row as usize);
                let mut view = ui.view.borrow_mut();
                let previous = std::mem::take(&mut view.showing);
                view.stack.push((previous, selection));
                view.showing = episodes;
                show_items(ui.items, &view.showing);
                let first = (!view.showing.is_empty()).then_some(0);
                refresh_vod_details(wiring, &view, ui.details, first);
                ui.status.set_label(&trf(
                    "{count} episodes loaded.",
                    &[("count", &count.to_string())],
                ));
            }
            VodEvent::Episodes {
                result: Err(error), ..
            } => {
                ui.status.set_label(&trf(
                    "Could not load episodes: {error}",
                    &[("error", &error)],
                ));
            }
        }
    }
}

/// Apply one VOD search reply (spec 0006): results for a query
/// the view no longer holds (cleared or replaced) are stale and
/// drop; live results replace the item list at the top level,
/// keep focus where it is, and announce a concise count.
fn apply_vod_search_results(wiring: &Wiring, ui: &VodUi, query: &str, hits: &[VodSearchHit]) {
    let mut view = ui.view.borrow_mut();
    if view.query != query {
        return;
    }
    let count = hits.len();
    view.showing = hits
        .iter()
        .filter_map(|hit| {
            view.catalog
                .groups
                .get(&hit.group)
                .and_then(|items| items.get(hit.index))
                .cloned()
        })
        .collect();
    view.stack.clear();
    show_hit_rows(ui.items, hits);
    let first = (!view.showing.is_empty()).then_some(0);
    refresh_vod_details(wiring, &view, ui.details, first);
    if count == 0 {
        ui.status
            .set_label(&trf("No results for {query}.", &[("query", query)]));
    } else {
        ui.status.set_label(&trf(
            "{count} results for {query}.",
            &[("count", &count.to_string()), ("query", query)],
        ));
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

/// One cast the app believes is live (N20): the device and
/// the channel URL it was started from — the pre-proxy source,
/// which is what the one-stream provider check counts
/// (Python's `_active_cast_source_url`).
#[derive(Clone)]
struct ActiveCast {
    device: CastDevice,
    source_url: String,
}

#[derive(Default)]
struct CastView {
    devices: Vec<CastDevice>,
    channel: Option<Channel>,
    /// The cast playing now, promoted from `pending_cast` when
    /// the worker confirms it.
    active_cast: Option<ActiveCast>,
    /// The cast asked for most recently, awaiting the worker's
    /// verdict.
    pending_cast: Option<ActiveCast>,
}

/// The source URL of the live cast, when one is held.
fn cast_source_url(slot: &Rc<RefCell<Option<CastUi>>>) -> Option<String> {
    slot.borrow().as_ref().and_then(|ui| {
        ui.view
            .borrow()
            .active_cast
            .as_ref()
            .map(|active| active.source_url.clone())
    })
}

/// Whether a cast session is live (N20 refusals consult it,
/// like Python's `caster.is_connected()`).
fn cast_is_active(slot: &Rc<RefCell<Option<CastUi>>>) -> bool {
    slot.borrow()
        .as_ref()
        .is_some_and(|ui| ui.view.borrow().active_cast.is_some())
}

/// The live cast's source URL as seen from a player handle.
fn player_cast_source(player: Option<&Rc<RefCell<PlayerState>>>) -> Option<String> {
    let state = player?.borrow();
    cast_source_url(&state.shared.as_ref()?.cast_ui)
}

/// Python's connected prompt (main.py:8703), shared by the
/// Cast entry points and the cast window's Play button:
/// returns true when a cast was live and the prompt was shown
/// — the caller then does nothing more, whichever way it was
/// answered. A Yes disconnects through the cast worker
/// (Python disconnects on a background thread; the worker is
/// ours).
fn prompt_cast_disconnect(
    parent: &dyn WxWidget,
    view: &Rc<RefCell<CastView>>,
    worker: &CastWorker,
) -> bool {
    let active = view.borrow().active_cast.clone();
    let Some(active) = active else {
        return false;
    };
    let message = trf(
        "Currently connected to: {device}\n\nDisconnect?",
        &[("device", &active.device.name)],
    );
    let dialog = MessageDialog::builder(parent, &message, &tr("Casting"))
        .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
        .build();
    if dialog.show_modal_tracked() == ID_YES {
        worker.stop(active.device.clone());
    }
    true
}

/// Open (or raise) the Cast window and start discovery for the
/// selected channel (shared by the button and the menus).
fn open_cast_window(wiring: &Wiring, list: ListCtrl) {
    let channel = selected_channel(wiring, list);
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
    frame: Frame,
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
            // N20: starting a cast while one is live asks
            // Python's disconnect question instead.
            if prompt_cast_disconnect(&frame, &view, &worker) {
                return;
            }
            let Some(selection) = devices.get_selection() else {
                return;
            };
            let (device, channel) = {
                let view = view.borrow();
                let (Some(device), Some(channel)) = (
                    view.devices.get(selection as usize).cloned(),
                    view.channel.clone(),
                ) else {
                    return;
                };
                (device, channel)
            };
            let (clean_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
            let headers = iptv_core::downloads::channel_headers(&channel);
            // The media type is probed from the original stream and
            // travels independently of the playback URL (S04): the
            // proxy URL below is an opaque `/s/<hash>` path that
            // cannot reveal HLS/TS to the renderer.
            let probed_mime = iptv_core::cast::guess_mime(&clean_url);
            let source_url = clean_url.clone();
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
            view.borrow_mut().pending_cast = Some(ActiveCast {
                device: device.clone(),
                source_url,
            });
            worker.play(
                device.clone(),
                play_url,
                channel.name.clone(),
                Some(probed_mime),
            );
            pending.set(true);
            assert!(timer.start(50, false));
            status.set_label(&trf(
                "Casting {channel} to {device}...",
                &[("channel", &channel.name), ("device", &device.name)],
            ));
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
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    let devices = ListBox::builder(&panel).build();
    devices.set_accessibility_label("Cast devices");
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
        frame, play, stop, refresh, devices, &ui.view, worker, pending, timer, proxy, status,
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
                    ui.status.set_label(&trf(
                        "Found {count} cast device(s). Choose one and press Play.",
                        &[("count", &count.to_string())],
                    ));
                }
                status.set_label("Cast device search finished.");
            }
            CastEvent::Played(Ok(name)) => {
                // N20: the worker confirmed the cast — it is
                // the live session the refusals consult.
                let mut view = ui.view.borrow_mut();
                if let Some(pending) = view.pending_cast.take() {
                    if pending.device.name == name {
                        view.active_cast = Some(pending);
                    }
                }
                drop(view);
                ui.status
                    .set_label(&trf("Casting to {name}.", &[("name", &name)]));
                status.set_label(&trf("Casting to {name}.", &[("name", &name)]));
            }
            CastEvent::Played(Err(error)) => {
                ui.view.borrow_mut().pending_cast = None;
                iptv_core::applog::error("cast", &format!("Casting failed: {error}"));
                ui.status
                    .set_label(&trf("Casting failed: {error}", &[("error", &error)]));
            }
            CastEvent::Stopped(name) => {
                let mut view = ui.view.borrow_mut();
                if view
                    .active_cast
                    .as_ref()
                    .is_some_and(|active| active.device.name == name)
                {
                    view.active_cast = None;
                }
                drop(view);
                ui.status
                    .set_label(&trf("Stopped casting to {name}.", &[("name", &name)]));
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
        category_fav_count: Rc::clone(&wiring.category_fav_count),
        timer: Rc::clone(&wiring.timer),
        model: Rc::clone(&wiring.model),
        player: Rc::clone(&wiring.player),
        epg: Rc::clone(&wiring.epg),
        epg_suffixes: Rc::clone(&wiring.epg_suffixes),
        suffix_worker: Rc::clone(&wiring.suffix_worker),
        suffix_pending: Rc::clone(&wiring.suffix_pending),
        suffix_generation: Rc::clone(&wiring.suffix_generation),
        guide_generation: Rc::clone(&wiring.guide_generation),
        epg_worker: Rc::clone(&wiring.epg_worker),
        epg_read_worker: Rc::clone(&wiring.epg_read_worker),
        epg_read_seq: Rc::clone(&wiring.epg_read_seq),
        pending_epg_reads: Rc::clone(&wiring.pending_epg_reads),
        epg_check_pending: Rc::clone(&wiring.epg_check_pending),
        epg_pending: Rc::clone(&wiring.epg_pending),
        epg_deferred: Rc::clone(&wiring.epg_deferred),
        epg_rows: Rc::clone(&wiring.epg_rows),
        epg_extra_sources: Rc::clone(&wiring.epg_extra_sources),
        epg_inflight_sources: Rc::clone(&wiring.epg_inflight_sources),
        epg_recheck: Rc::clone(&wiring.epg_recheck),
        epg_import_notify: Rc::clone(&wiring.epg_import_notify),
        epg_timer: Rc::clone(&wiring.epg_timer),
        reminders: Rc::clone(&wiring.reminders),
        provider_worker: Rc::clone(&wiring.provider_worker),
        provider_pending: Rc::clone(&wiring.provider_pending),
        resolve_seq: Rc::clone(&wiring.resolve_seq),
        pending_resolves: Rc::clone(&wiring.pending_resolves),
        dvr_resolve: Rc::clone(&wiring.dvr_resolve),
        account_limits: Rc::clone(&wiring.account_limits),
        limit_fetch: Rc::clone(&wiring.limit_fetch),
        limit_holds: Rc::clone(&wiring.limit_holds),
        conflict_override: Rc::clone(&wiring.conflict_override),
        recordings: Rc::clone(&wiring.recordings),
        cast_worker: Rc::clone(&wiring.cast_worker),
        cast_pending: Rc::clone(&wiring.cast_pending),
        cast_ui: Rc::clone(&wiring.cast_ui),
        proxy: Rc::clone(&wiring.proxy),
        guide_ui: Rc::clone(&wiring.guide_ui),
        tray: Rc::clone(&wiring.tray),
        i18n: Rc::clone(&wiring.i18n),
        housekeeping: Rc::clone(&wiring.housekeeping),
        help_focus_topic: Rc::clone(&wiring.help_focus_topic),
        vod_worker: Rc::clone(&wiring.vod_worker),
        vod_pending: Rc::clone(&wiring.vod_pending),
        vod_ui: Rc::clone(&wiring.vod_ui),
        dvr: Rc::clone(&wiring.dvr),
        recorded: Rc::clone(&wiring.recorded),
        shutdown_attempted: Rc::clone(&wiring.shutdown_attempted),
        exit_requested: Rc::clone(&wiring.exit_requested),
        last_dvr_tick: Rc::clone(&wiring.last_dvr_tick),
        scope_ids: Rc::clone(&wiring.scope_ids),
        branch_labels: Rc::clone(&wiring.branch_labels),
        download_worker: Rc::clone(&wiring.download_worker),
        direct_probe_worker: Rc::clone(&wiring.direct_probe_worker),
        direct_probe_pending: Rc::clone(&wiring.direct_probe_pending),
        downloads: Rc::clone(&wiring.downloads),
        downloads_active: Rc::clone(&wiring.downloads_active),
        downloads_ui: Rc::clone(&wiring.downloads_ui),
        next_download_id: Rc::clone(&wiring.next_download_id),
        reminder_seen: Rc::clone(&wiring.reminder_seen),
        reminder_window_open: Rc::clone(&wiring.reminder_window_open),
        staged_downloads: Rc::clone(&wiring.staged_downloads),
        shared_playback: Rc::clone(&wiring.shared_playback),
        pending_shares: Rc::clone(&wiring.pending_shares),
        relay_worker: Rc::clone(&wiring.relay_worker),
        series_last_scan: Rc::clone(&wiring.series_last_scan),
        account_info_ui: Rc::clone(&wiring.account_info_ui),
        account_discovery: Rc::clone(&wiring.account_discovery),
        account_discovery_key: Rc::clone(&wiring.account_discovery_key),
        buffers: Rc::clone(&wiring.buffers),
        media_probe: wiring.media_probe.clone(),
        media_probe_pending: Rc::clone(&wiring.media_probe_pending),
        media_probe_token: Rc::clone(&wiring.media_probe_token),
        format_menu_state: Rc::clone(&wiring.format_menu_state),
        auto_check_due: Rc::clone(&wiring.auto_check_due),
    }
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
                status.set_label(&trf(
                    "Stopping {count} recordings.",
                    &[("count", &count.to_string())],
                ));
            }
        }
        TRAY_EXIT => {
            wiring.exit_requested.set(true);
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
pub const SPEECH_BACKEND_SR: i32 = 5240;
pub const SPEECH_BACKEND_AV: i32 = 5241;
pub const APP_VOICE_VOLUME: i32 = 5242;
pub const APP_VOICE_OUTPUT: i32 = 5243;
pub const APP_VOICE_RATE: i32 = 5244;
pub const DEVICE_POLICY_PAUSE: i32 = 5245;
pub const DEVICE_POLICY_DEFAULT: i32 = 5246;
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
pub const REC_PROBLEM_TIMES: i32 = 5224;
pub const REC_FORMAT_CHECKING: i32 = 5225;

/// The Recording Format submenu inside the Recordings menu
/// (found by its label, the same fetch-by-parent pattern the
/// player menu refreshes use).
fn format_submenu(recordings_menu: &Menu) -> Option<Menu> {
    for pos in 0..recordings_menu.get_item_count() {
        let item = recordings_menu.find_item_by_position(pos)?;
        if item.get_label().replace('&', "").contains("Format") {
            if let Some(sub) = item.get_sub_menu() {
                return Some(sub);
            }
        }
    }
    None
}

/// Refill the Recording Format submenu for the selected
/// channel (N16, Python `_fill_recording_format_menu`): an
/// audio-only channel offers the audio presets only, so a
/// radio recording can never silently land in a video
/// container; the checked item is the channel's effective
/// format; while a probe is still classifying an unknown
/// channel, a disabled "Checking stream type…" item says so
/// instead of guessing.
fn refill_recording_format_menu(wiring: &Wiring, recordings_menu: &Menu, list: ListCtrl) {
    let channel = selected_channel(wiring, list);
    let (media, current) = match &channel {
        Some(channel) => {
            let (media, _) = classify_channel_media(&wiring.media_probe.cache, channel);
            let current =
                recording_format_for_channel(&wiring.persist, &wiring.media_probe.cache, channel);
            (media, current)
        }
        None => (
            iptv_core::media_type::MEDIA_UNKNOWN.to_string(),
            wiring.persist.config.borrow().string_or(
                "recording_format_video",
                recording::DEFAULT_RECORDING_FORMAT,
            ),
        ),
    };
    let checking = media == iptv_core::media_type::MEDIA_UNKNOWN
        && channel.as_ref().is_some_and(|channel| {
            wiring
                .media_probe
                .in_flight(&iptv_core::media_type::channel_record_key(channel))
        });
    // MENU_OPEN fires for every menu, the Format submenu
    // included: while nothing the submenu shows has changed,
    // the refill is a no-op.
    let state_key = format!(
        "{}|{}|{}|{}",
        channel.as_ref().map_or("", |c| c.url.as_str()),
        media,
        current,
        checking
    );
    if *wiring.format_menu_state.borrow() == state_key {
        return;
    }
    let Some(menu) = format_submenu(recordings_menu) else {
        return;
    };
    *wiring.format_menu_state.borrow_mut() = state_key;
    while menu.get_item_count() > 0 {
        let Some(item) = menu.find_item_by_position(0) else {
            break;
        };
        menu_delete_if_present(&menu, item.get_item_id());
    }
    for key in iptv_core::media_type::formats_for_media(&media) {
        let Some(index) = iptv_core::recording::RECORDING_FORMATS
            .iter()
            .position(|entry| entry.0 == key)
        else {
            continue;
        };
        let id = REC_FORMAT_BASE + i32::try_from(index).unwrap_or(0);
        menu_add(
            &menu,
            id,
            iptv_core::recording::RECORDING_FORMATS[index].1,
            "",
            ItemKind::Radio,
        );
        menu.check_item(id, key == current);
    }
    if checking {
        menu_add(
            &menu,
            REC_FORMAT_CHECKING,
            "Checking stream type…",
            "",
            ItemKind::Normal,
        );
        menu.enable_item(REC_FORMAT_CHECKING, false);
    }
}

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
/// Delete a menu item only when it is actually in the menu.
/// `wxMenu::Delete` on an absent id is invalid — debug builds
/// stop on the "invalid item" assert for it. The
/// Audio Track and Subtitles submenu rebuilds delete
/// placeholder ids (`PM_NO_TRACKS`, `PM_SUB_BASE`) that an
/// earlier rebuild may already have removed: the audio
/// placeholder is dropped by the track refresh and only
/// restored when the engine is applied, and the subtitle
/// placeholder shares its id with the first radio item, so
/// the drain on the next refresh removes that id and the
/// unconditional delete that followed it hit nothing.
fn menu_delete_if_present(menu: &Menu, id: i32) {
    if menu.find_item(id).is_some() {
        menu.delete(id);
    }
}

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
    set_translator(&wiring.i18n.borrow());
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
    // N16: refill the Recording Format submenu whenever a
    // menu opens, so it always reflects the selected channel's
    // media type before keyboard focus can enter it (Python
    // binds EVT_MENU_OPEN on the Recordings menu; the refill
    // is a no-op while the state it shows is unchanged).
    let wiring_open = clone_wiring(wiring);
    let list_open = controls.list;
    frame.bind_internal(EventType::MENU_OPEN, move |event| {
        if let Some(bar) = frame.get_menu_bar() {
            if let Some(recordings_menu) = bar.get_menu(4) {
                refill_recording_format_menu(&wiring_open, &recordings_menu, list_open);
            }
        }
        event.skip(true);
    });
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
        REC_PROBLEM_TIMES,
        "Recording Problem &Times...",
        "",
        ItemKind::Normal,
    );
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

/// Options > Subtitle Speech > Read previous subtitle: the
/// player review cursor's previous step (P11), through the
/// shared speech route.
fn menu_read_previous_subtitle(wiring: &Wiring, status: Status) {
    review_subtitle(&wiring.player, &wiring.persist, status, -1);
}

/// Options > Subtitle Speech > Review recent subtitles...
/// (P11): the last 20 cues actually shown, newest first and
/// numbered — never the file's unplayed tail.
fn open_review_subtitles(wiring: &Wiring, status: Status) {
    let recent: Vec<String> = {
        let player = wiring.player.borrow();
        player
            .shown_cues
            .iter()
            .rev()
            .take(20)
            .enumerate()
            .map(|(index, cue)| format!("{}. {}", index + 1, cue.text))
            .collect()
    };
    if recent.is_empty() {
        status.set_label("No recent subtitles.");
        return;
    }
    let (frame, _panel, list, dstatus) = list_dialog("Recent Subtitles", "Recent subtitles");
    list.set_items(&recent.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    dstatus.set_label(&trf(
        "{count} recent subtitles.",
        &[("count", &recent.len().to_string())],
    ));
    frame.show(true);
}

/// Switch the subtitle speech backend (Python `_set_subtitle_backend`):
/// the application voice is refused where SAPI does not exist;
/// every accepted switch stops the application voice and saves.
fn set_speech_backend(wiring: &Wiring, frame: Frame, status: Status, backend: SpeechBackend) {
    {
        let mut state = wiring.player.borrow_mut();
        if !state.cue_speech.set_backend(backend) {
            drop(state);
            let _ = MessageDialog::builder(
                &frame,
                "The application voice is only available on Windows with SAPI.",
                "Subtitle Speech",
            )
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal_tracked();
            return;
        }
        state.app_voice.stop();
    }
    wiring.persist.set_and_save(
        "subtitle_speech_backend",
        serde_json::json!(match backend {
            SpeechBackend::AppVoice => "app_tts",
            SpeechBackend::ScreenReader => "screen_reader",
        }),
    );
    status.set_label(match backend {
        SpeechBackend::AppVoice => "Subtitle speech will use the application voice.",
        SpeechBackend::ScreenReader => "Subtitle speech will use your screen reader.",
    });
}

/// Switch the device-loss policy (Python `_set_app_tts_device_policy`).
fn set_device_policy(wiring: &Wiring, status: Status, policy: DevicePolicy) {
    wiring.persist.set_and_save(
        "subtitle_app_device_policy",
        serde_json::json!(match policy {
            DevicePolicy::Pause => "pause",
            DevicePolicy::Default => "default",
        }),
    );
    let mut note = None;
    {
        let mut state = wiring.player.borrow_mut();
        state.app_voice.stop();
        if state.cue_speech.backend() == SpeechBackend::AppVoice
            && !state.app_voice.validate_output(policy)
        {
            note = state.cue_speech.note_device_lost();
        }
    }
    if let Some(text) = note {
        live_region::announce_with_priority(status.label, text, live_region::Priority::High);
    }
    status.set_label(match policy {
        DevicePolicy::Pause => {
            "If the output device disappears, subtitle speech pauses until it returns."
        }
        DevicePolicy::Default => {
            "If the output device disappears, the application voice falls back to the system default."
        }
    });
}

/// A modal slider prompt (Python's volume/speed dialogs):
/// caption, slider, OK/Cancel; the chosen value on OK.
fn slider_dialog(
    frame: Frame,
    title: &str,
    caption: &str,
    min: i32,
    max: i32,
    value: i32,
) -> Option<i32> {
    let dialog = Dialog::builder(&frame, &tr(title))
        .with_size(380, 190)
        .build();
    dialog.set_accessibility_label(&tr(title));
    bind_escape_cancel(dialog);
    let panel = Panel::builder(&dialog).build();
    let label = StaticText::builder(&panel).with_label(&tr(caption)).build();
    label.set_accessibility_label(&tr(caption));
    let slider = Slider::builder(&panel).build();
    slider.set_range(min, max);
    slider.set_value(value);
    slider.set_accessibility_label(&tr(caption));
    let chosen = Rc::new(std::cell::Cell::new(None::<i32>));
    let ok = Button::builder(&panel).with_label("&OK").build();
    ok.set_accessibility_label("OK");
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    cancel.set_accessibility_label("Cancel");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&label, 0, SizerFlag::All, 8);
    sizer.add(&slider, 0, SizerFlag::Expand | SizerFlag::All, 8);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    let chosen_ok = Rc::clone(&chosen);
    let dialog_ok = dialog;
    ok.on_click(move |_| {
        chosen_ok.set(Some(slider.get_value()));
        dialog_ok.end_modal(ID_OK);
    });
    let dialog_cancel = dialog;
    cancel.on_click(move |_| {
        dialog_cancel.end_modal(ID_CANCEL);
    });
    if dialog.show_modal_tracked() == ID_OK {
        chosen.get()
    } else {
        None
    }
}

/// Options > Subtitle Speech > Application voice volume...
fn app_voice_volume_dialog(wiring: &Wiring, frame: Frame) {
    let current = {
        let state = wiring.player.borrow();
        if !state.app_voice.available() {
            return;
        }
        state.app_voice.volume()
    };
    if let Some(level) = slider_dialog(
        frame,
        "Application voice volume",
        "Volume:",
        0,
        100,
        current,
    ) {
        let mut state = wiring.player.borrow_mut();
        if state.app_voice.set_volume(level) {
            drop(state);
            wiring
                .persist
                .set_and_save("subtitle_app_volume", serde_json::json!(level));
        }
    }
}

/// Options > Subtitle Speech > Application voice speed...
fn app_voice_rate_dialog(wiring: &Wiring, frame: Frame) {
    let current = {
        let state = wiring.player.borrow();
        if !state.app_voice.available() {
            return;
        }
        state.app_voice.rate()
    };
    if let Some(rate) = slider_dialog(
        frame,
        "Application voice speed",
        "Speech speed (-10 to 10; 0 is normal):",
        -10,
        10,
        current,
    ) {
        let mut state = wiring.player.borrow_mut();
        if state.app_voice.set_rate(rate) {
            let applied = state.app_voice.rate();
            drop(state);
            wiring
                .persist
                .set_and_save("subtitle_app_rate", serde_json::json!(applied));
        } else {
            drop(state);
            let _ = MessageDialog::builder(
                &frame,
                "Could not change the application voice speed.",
                "Subtitle Speech",
            )
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal_tracked();
        }
    }
}

/// Options > Subtitle Speech > Application voice output...
/// (Python `_choose_app_tts_output`): preview the choice with a
/// spoken sample while cues are suppressed; keep it only on Yes,
/// restoring the exact prior choice otherwise.
fn app_voice_output_dialog(wiring: &Wiring, frame: Frame, status: Status) {
    let (previous, outputs) = {
        let mut state = wiring.player.borrow_mut();
        if !state.app_voice.available() {
            return;
        }
        (
            state.app_voice.selected_output().to_string(),
            state.app_voice.list_outputs(),
        )
    };
    // Duplicate friendly names stay separate choices, numbered
    // the way Python numbers them.
    let mut labels: Vec<String> = vec![tr("System default")];
    for (index, (_, name)) in outputs.iter().enumerate() {
        let mut label = name.clone();
        if outputs.iter().filter(|(_, n)| n == name).count() > 1 {
            label = format!("{name} ({})", index + 1);
        }
        labels.push(label);
    }
    let identities: Vec<String> = std::iter::once(String::new())
        .chain(outputs.iter().map(|(id, _)| id.clone()))
        .collect();
    let dialog = SingleChoiceDialog::builder(
        &frame,
        &tr("Output device for the application voice:"),
        &tr("Application voice output"),
        &labels.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .build();
    if let Some(position) = identities.iter().position(|id| id == &previous) {
        dialog.set_selection(i32::try_from(position).unwrap_or(0));
    }
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let selection = usize::try_from(dialog.get_selection()).unwrap_or(usize::MAX);
    let Some(choice) = identities.get(selection).cloned() else {
        return;
    };
    let name = labels.get(selection).cloned().unwrap_or_default();
    let policy = iptv_core::speech::device_policy(&wiring.persist.config.borrow());
    let mut keep = false;
    {
        let mut state = wiring.player.borrow_mut();
        state.cue_speech.set_preview_active(true);
        let selected = state.app_voice.set_output(&choice, policy);
        if !selected {
            drop(state);
            let _ = MessageDialog::builder(
                &frame,
                "Could not select that output device.",
                "Subtitle Speech",
            )
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal_tracked();
        } else if state
            .app_voice
            .speak("This is the application voice.", policy)
            != iptv_core::speech::AppSpeakResult::Spoken
        {
            drop(state);
            let _ = MessageDialog::builder(&frame, "Subtitle speech failed.", "Subtitle Speech")
                .with_style(MessageDialogStyle::OK)
                .build()
                .show_modal_tracked();
        } else {
            drop(state);
            keep = MessageDialog::builder(
                &frame,
                &format!("Keep this output device?\n{name}"),
                "Subtitle Speech",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build()
            .show_modal_tracked()
                == ID_YES;
        }
    }
    let mut state = wiring.player.borrow_mut();
    state.app_voice.stop();
    if keep {
        let selected = state.app_voice.selected_output().to_string();
        state.cue_speech.note_route_healthy();
        state.cue_speech.set_preview_active(false);
        drop(state);
        wiring
            .persist
            .set_and_save("subtitle_app_output", serde_json::json!(selected));
        status.set_label(&trf("Application voice output: {name}", &[("name", &name)]));
    } else {
        state.app_voice.set_output(&previous, policy);
        state.cue_speech.set_preview_active(false);
    }
}

/// Options > Subtitle Speech > Subtitle capabilities...
fn show_subtitle_capabilities(frame: Frame, status: Status) {
    let text = "Subtitle support in this build:\n\n- The built-in player shows sidecar subtitle files (SRT, WebVTT, ASS/SSA) loaded with Load Subtitles.\n- Speak subtitles reads each cue aloud when it appears, either through your screen reader or through the application voice. On Windows the application voice uses SAPI; if its output device disappears, speech pauses until the device returns, or falls back to the system default, per the device policy.\n- Read current subtitle and Read previous subtitle speak on demand from the Options > Subtitle Speech menu.\n- External players handle subtitles themselves; their support varies by player.";
    let dialog = MessageDialog::builder(&frame, text, "Subtitle Capabilities")
        .with_style(MessageDialogStyle::OK)
        .build();
    dialog.show_modal_tracked();
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
    // R08: report the same gate recordings face — a probed, writable
    // folder with known free space — not a platform-skippable check.
    // N24: the report text is Python's, verbatim (planning_ui.py);
    // a failed check shows the error alone in an error box, and
    // the box queues behind an open modal like Python's.
    let (text, failed) = match recording::check_destination(
        &dir,
        Some(i64::try_from(reserve).unwrap_or(i64::MAX)),
    ) {
        Ok(check) => (
            format!(
                "Recording folder: {}\nFree space: {} MiB\nRequired reserve: {reserve} MiB.\n\nThis check does not guarantee sufficient space for the entire recording.",
                dir.display(),
                check.free_bytes / (1024 * 1024)
            ),
            false,
        ),
        Err(error) => (error, true),
    };
    let style = if failed {
        MessageDialogStyle::OK | MessageDialogStyle::IconError
    } else {
        MessageDialogStyle::OK | MessageDialogStyle::IconInformation
    };
    show_or_queue_notification(frame, &text, &tr("Recording Storage"), style, None);
    status.set_label("Recording storage shown.");
}

/// The newest recording log by modification time (Python
/// `_latest_recording_log`).
fn latest_recording_log(logs_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(logs_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file()
            || path
                .extension()
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("log"))
        {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(when, _)| mtime > *when) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Recordings > Recording Problem Times... (N12; Python
/// `_show_recording_problem_times` and
/// `RecordingProblemTimesDialog`): the newest recording log's
/// problems placed in time, with the VLC jump hint and a Copy
/// Details button.
fn show_recording_problem_times(wiring: &Wiring, frame: Frame) {
    let logs_dir =
        recordings_dir(&wiring.persist).join(iptv_core::recording_log::RECORDING_LOG_DIRNAME);
    let latest = latest_recording_log(&logs_dir);
    let (heading, problems) = match &latest {
        Some(path) => (
            path.file_stem().map_or_else(
                || tr("Recordings"),
                |stem| stem.to_string_lossy().into_owned(),
            ),
            iptv_core::recording_log::recording_problem_times(
                path,
                iptv_core::recording_log::PROBLEM_TIMES_LIMIT,
            ),
        ),
        None => (tr("Recordings"), Vec::new()),
    };
    let mut lines: Vec<String> = Vec::new();
    for problem in &problems {
        let times = if problem.jump_times.is_empty() {
            &problem.clock_times
        } else {
            &problem.jump_times
        };
        if !times.is_empty() {
            lines.push(format!("{}: {}", times.join(", "), problem.message));
        }
    }
    if lines.is_empty() {
        lines.push(tr(
            "No timed problems were found in the newest recording log.",
        ));
    }
    let dialog = Dialog::builder(&frame, &tr("Recording Problem Times"))
        .with_size(620, 460)
        .build();
    dialog.set_accessibility_label(&tr("Recording Problem Times"));
    bind_escape_cancel(dialog);
    let panel = Panel::builder(&dialog).build();
    let heading_text = StaticText::builder(&panel).with_label(&heading).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label(&tr("Recording Problem Times"));
    list.set_items(&lines.iter().map(String::as_str).collect::<Vec<_>>());
    let hint = StaticText::builder(&panel)
        .with_label(&tr(
            "Open the recorded file in VLC, press Ctrl+T, and enter a listed time. Times use the file's media clock when FFmpeg reported it; otherwise they are elapsed recording time and may be slightly later after an interruption.",
        ))
        .build();
    let copy = Button::builder(&panel).with_label("Copy &Details").build();
    copy.set_accessibility_label("Copy Details");
    let close = Button::builder(&panel).with_label("&Close").build();
    close.set_accessibility_label("Close");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&heading_text, 0, SizerFlag::All, 12);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 12);
    sizer.add(&hint, 0, SizerFlag::All, 12);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&copy, 0, SizerFlag::All, 4);
    buttons.add(&close, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    let dialog_copy = dialog;
    copy.on_click(move |_| {
        if !Clipboard::get().set_text(&lines.join("\n")) {
            let _ = MessageDialog::builder(
                &dialog_copy,
                &tr("Could not open the clipboard."),
                &tr("Recording Problem Times"),
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
            .build()
            .show_modal_tracked();
        }
    });
    let dialog_close = dialog;
    close.on_click(move |_| {
        dialog_close.end_modal(ID_CANCEL);
    });
    list.set_focus();
    dialog.show_modal_tracked();
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
    if dialog.show_modal_tracked() != ID_OK {
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
    status.set_label(&trf(
        "Minimum free space before recording: {normalized} MiB.",
        &[("normalized", &normalized.to_string())],
    ));
}

/// The application-voice items of the Subtitle Speech menu:
/// volume, output, speed, and the device-loss policy radios.
/// All are enabled only where SAPI exists.
fn add_app_voice_items(menu: &Menu, config: &Config, app_available: bool) {
    menu_add(
        menu,
        APP_VOICE_VOLUME,
        "Application voice volume...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        menu,
        APP_VOICE_OUTPUT,
        "Application voice output...",
        "",
        ItemKind::Normal,
    );
    menu_add(
        menu,
        APP_VOICE_RATE,
        "Application voice speed...",
        "",
        ItemKind::Normal,
    );
    let policy = iptv_core::speech::device_policy(config);
    menu_add(
        menu,
        DEVICE_POLICY_PAUSE,
        "On device loss: pause until the device returns",
        "",
        ItemKind::Radio,
    );
    menu_add(
        menu,
        DEVICE_POLICY_DEFAULT,
        "On device loss: use the system default",
        "",
        ItemKind::Radio,
    );
    menu.check_item(DEVICE_POLICY_PAUSE, policy == DevicePolicy::Pause);
    menu.check_item(DEVICE_POLICY_DEFAULT, policy == DevicePolicy::Default);
    for id in [
        APP_VOICE_VOLUME,
        APP_VOICE_OUTPUT,
        APP_VOICE_RATE,
        DEVICE_POLICY_PAUSE,
        DEVICE_POLICY_DEFAULT,
    ] {
        menu.enable_item(id, app_available);
    }
}

fn build_speech_menu(config: &Config) -> Menu {
    let speech_sub = Menu::builder().build();
    // Backend choice first (Python's order): exactly one backend
    // speaks each cue. The application-voice items are enabled
    // only where SAPI exists.
    let app_available = iptv_sapi::platform_available();
    let backend = iptv_core::speech::speech_backend(config);
    menu_add(
        &speech_sub,
        SPEECH_BACKEND_SR,
        "Speak through screen reader",
        "",
        ItemKind::Radio,
    );
    menu_add(
        &speech_sub,
        SPEECH_BACKEND_AV,
        "Speak through application voice",
        "",
        ItemKind::Radio,
    );
    speech_sub.check_item(SPEECH_BACKEND_SR, backend == SpeechBackend::ScreenReader);
    speech_sub.check_item(SPEECH_BACKEND_AV, backend == SpeechBackend::AppVoice);
    speech_sub.enable_item(SPEECH_BACKEND_AV, app_available);
    speech_sub.append_separator();
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
    add_app_voice_items(&speech_sub, config, app_available);
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

/// The User Guide topic a menu item belongs to, for
/// highlight-driven F1 routing (audit S10). Items without a
/// dedicated section return None and leave the current topic
/// alone.
fn menu_item_topic(id: i32) -> Option<&'static str> {
    Some(match id {
        PLAYLIST_MANAGER => "playlist-manager",
        EPG_MANAGER => "epg-manager",
        IMPORT_EPG => "import-epg",
        WHATS_ON_NOW => "whats-on-now",
        CONTINUE_WATCHING => "continue-watching",
        VIEW_VOD => "video-on-demand",
        FAVORITE | GO_TO_FAVORITES | CTX_FAVORITE => "favorites",
        ACCOUNT_INFO => "account-info",
        CAST_TO | CAST_CONNECT => "casting",
        SHOW_PLAYER | PLAY_PAUSE | STOP => "built-in-player",
        PREFERRED_AUDIO => "preferred-audio-track",
        KEYBOARD_SHORTCUTS => "keyboard-shortcuts",
        WHATS_NEW => "updates",
        REC_START | REC_STOP | REC_STOP_ALL | REC_STORAGE | REC_MIN_FREE | REC_SET_FOLDER
        | REC_OPEN_FOLDER | REC_PROBLEM_TIMES | CTX_RECORD => "recordings",
        REC_REMINDERS | CTX_REMINDER => "reminders",
        REC_SCHEDULED | CTX_SCHEDULE => "scheduled-recordings",
        REC_PADDING => "schedule-padding",
        SPEAK_SUBTITLES | READ_CURRENT_SUBTITLE | READ_PREVIOUS_SUBTITLE | REVIEW_SUBTITLES => {
            "subtitle-speech"
        }
        CTX_PLAY => "channel-list",
        CTX_EPG => "channel-epg",
        CTX_CATCHUP => "catch-up",
        _ => return None,
    })
}

/// Go to Favorites (View menu). With no favorites there is
/// nowhere to go: Python shows its box (main.py:1927) and does
/// not switch category.
fn go_to_favorites(
    frame: Frame,
    wiring: &Wiring,
    category_box: TreeCtrl,
    search: TextCtrl,
    status: Status,
) {
    if wiring.browser.borrow().favorite_count() == 0 {
        let dialog = MessageDialog::builder(
            &frame,
            "You have not added any favorites yet. Select a channel and press Ctrl+D to add it.",
            "Favorites",
        )
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
        .build();
        dialog.show_modal_tracked();
        return;
    }
    let favorites_item = {
        let state = wiring.categories.borrow();
        state
            .rows
            .iter()
            .find(|row| row.category == Category::Favorites)
            .and_then(|row| tree_item_at_text_path(category_box, &category_row_text_path(row)))
    };
    if let Some(item) = favorites_item.or_else(|| tree_item_at(category_box, 1)) {
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

/// Dispatch for the parity menu tree.
fn bind_menu_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    bind_menu_option_actions(frame, wiring, controls);
    let status = controls.status;
    let list = controls.list;
    let search = controls.search;
    let category_box = controls.category;
    let scope_choice = controls.scope_choice;
    // Menu browsing also aims help (audit S10): wxdragon exposes
    // MENU_HIGHLIGHT, so the highlighted item's topic becomes the
    // current help topic while a menu is open. The User Guide
    // command (F1) then opens at the item the user is actually
    // on, instead of falling back to the last focused control.
    let highlight_topic = Rc::clone(&wiring.help_focus_topic);
    frame.on_menu_highlighted(move |event| {
        if let Some(topic) = event.get_menu_id().and_then(menu_item_topic) {
            *highlight_topic.borrow_mut() = topic;
        }
    });
    let wiring = clone_wiring(wiring);
    frame.on_menu(move |event| {
        let id = event.get_id();
        let persist = &wiring.persist;
        if menu_dialog_action(id, &wiring, status, scope_choice) {
            return;
        }
        match id {
            4004 => {
                wiring.exit_requested.set(true);
                frame.close(false);
            }
            // The Help menu opens the guide at the topic of the
            // control the user is actually on (S10), not always at
            // the top of the manual.
            5002 => {
                let topic = *wiring.help_focus_topic.borrow();
                open_guide_window_at(&wiring, status, topic);
            }
            5003 => check_updates(&frame, ship_shape::ui::CheckTrigger::Manual),
            5004 => {
                frame.show(true);
                frame.raise();
            }
            IMPORT_EPG => menu_import_epg(&wiring, frame, status),
            WHAT_IS_PLAYING => announce_what_is_playing(&wiring, None, Some(list), status),
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
            FAVORITE => toggle_selected_favorite(&wiring, list, search, status),
            GO_TO_FAVORITES => go_to_favorites(frame, &wiring, category_box, search, status),
            PREVIOUS_CHANNEL => {
                // The second entry of the persisted MRU
                // (P17): playing it promotes it to the front,
                // so repeated Previous toggles A/B.
                let recent = resolved_recent(&wiring);
                match recent.get(1) {
                    Some(channel) => {
                        let channel = channel.clone();
                        play_channel(
                            &wiring.player,
                            persist,
                            &wiring.media_probe,
                            &channel,
                            status,
                        );
                    }
                    None => status.set_label("No previous channel is available."),
                }
            }
            CHANNEL_NUMBER => menu_go_to_channel_number(&wiring, frame, list, status),
            _ => {}
        }
    });
}

/// Recordings menu dispatch. Returns true when the id belonged
/// to the Recordings menu.
#[allow(clippy::too_many_lines)] // grew with R17/R18 recording actions
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
            if let Some(channel) = selected_channel(wiring, list) {
                toggle_recording(
                    &frame,
                    &wiring.recordings,
                    &wiring.buffers,
                    &wiring.persist,
                    &wiring.media_probe,
                    &channel,
                    status,
                    Some(&wiring.player),
                    &wiring.share_ctx(),
                );
            } else {
                status.set_label("Select a channel to record first.");
            }
        }
        REC_STOP => {
            // Python's stop: the selected channel's recording
            // first; otherwise the single-active-recording
            // fallback (audit R18) — exactly one active recording
            // stops even after the selection moved; none says so;
            // several stop all, like Python's fallback.
            let selected_key = selected_channel(wiring, list).map(|channel| channel_key(&channel));
            if let Some(key) = &selected_key {
                if wiring.recordings.borrow().is_recording(key) {
                    wiring.recordings.borrow_mut().stop(key);
                    release_recording_lease(&wiring.buffers, key);
                    status.set_label("Stopping recording.");
                    return true;
                }
            }
            let keys = wiring.recordings.borrow().active_keys();
            match keys.len() {
                0 => status.set_label("No recordings are currently active."),
                1 => {
                    wiring.recordings.borrow_mut().stop(&keys[0]);
                    release_recording_lease(&wiring.buffers, &keys[0]);
                    status.set_label("Stopping recording.");
                }
                count => {
                    for key in &keys {
                        wiring.recordings.borrow_mut().stop(key);
                        release_recording_lease(&wiring.buffers, key);
                    }
                    status.set_label(&trf(
                        "Stopping {count} recordings.",
                        &[("count", &count.to_string())],
                    ));
                }
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
                status.set_label(&trf(
                    "Stopping {count} recordings.",
                    &[("count", &count.to_string())],
                ));
            }
        }
        REC_REMINDERS => open_reminders_dialog(wiring, status),
        REC_SCHEDULED => open_scheduled_dialog(wiring, status),
        REC_STORAGE => show_recording_storage(wiring, frame, status),
        REC_MIN_FREE => prompt_min_free_space(wiring, frame, status),
        REC_PADDING => prompt_schedule_padding(wiring, frame, status),
        REC_PROBLEM_TIMES => show_recording_problem_times(wiring, frame),
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
            if dialog.show_modal_tracked() == ID_OK {
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
            // Audit R17, mirroring Python's arming: the recorded
            // flag restarts at arming — work already finished
            // before arming must not trigger a shutdown; work
            // running or queued at arming is what we wait for.
            wiring.shutdown_attempted.set(false);
            wiring.recorded.set(
                next && (wiring.recordings.borrow().active_count() > 0
                    || wiring.dvr.borrow().pending_count() > 0),
            );
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
                // N16: the choice lands on the preference for
                // the kind of channel it was made on (Python
                // `_set_recording_format`): MP3 picked on a TV
                // channel records TV soundtracks as MP3, and
                // picking it on a radio channel must not leave
                // TV recordings in MP3.
                let audio = selected_channel(wiring, list).is_some_and(|channel| {
                    classify_channel_media(&wiring.media_probe.cache, &channel).0
                        == iptv_core::media_type::MEDIA_AUDIO
                });
                let key = if audio {
                    "recording_format_audio"
                } else {
                    "recording_format_video"
                };
                wiring
                    .persist
                    .set_and_save(key, serde_json::json!(format_id));
                status.set_label(&trf("Recording format: {label}.", &[("label", label)]));
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
            status.set_label(&trf("Media player set to {name}.", &[("name", name)]));
        }
        return true;
    }
    if (LANGUAGE_BASE..LANGUAGE_BASE + 15).contains(&id) {
        let index = usize::try_from(id - LANGUAGE_BASE).unwrap_or(0);
        if let Some((code, _name)) = iptv_core::i18n::language_labels().get(index) {
            if *code != wiring.i18n.borrow().language() {
                persist.set_and_save("language", serde_json::json!(code));
                // Activate at once (S06, Python's `_on_select_language`):
                // the confirmation below — and every dialog opened from
                // now on — uses the new language; already-built menus
                // pick it up on restart.
                let mut fresh = I18n::default();
                fresh.set_language(&iptv_core::i18n::locale_dir(), code);
                *wiring.i18n.borrow_mut() = fresh;
                set_translator(&wiring.i18n.borrow());
                if let Some(bar) = status.bar {
                    let dialog = MessageDialog::builder(
                        &bar,
                        &tr("The language has been changed. Please restart Accessible IPTV Client for the change to take full effect."),
                        &tr("Language"),
                    )
                    .build();
                    dialog.show_modal_tracked();
                }
            }
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
                let next = !persist
                    .config
                    .borrow()
                    .bool_or("show_player_on_enter", true);
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
                let next = flip("speak_subtitles");
                if !next {
                    // Off stops application-owned speech at once.
                    wiring.player.borrow_mut().app_voice.stop();
                }
            }
            SPEECH_BACKEND_SR => {
                set_speech_backend(&wiring, frame, status, SpeechBackend::ScreenReader);
            }
            SPEECH_BACKEND_AV => {
                set_speech_backend(&wiring, frame, status, SpeechBackend::AppVoice);
            }
            APP_VOICE_VOLUME => app_voice_volume_dialog(&wiring, frame),
            APP_VOICE_OUTPUT => app_voice_output_dialog(&wiring, frame, status),
            APP_VOICE_RATE => app_voice_rate_dialog(&wiring, frame),
            DEVICE_POLICY_PAUSE => set_device_policy(&wiring, status, DevicePolicy::Pause),
            DEVICE_POLICY_DEFAULT => set_device_policy(&wiring, status, DevicePolicy::Default),
            PREFERRED_AUDIO => open_audio_prefs(&wiring, status),
            KEYBOARD_SHORTCUTS => open_shortcuts_editor(&wiring, status),
            WHATS_NEW => open_whats_new(&wiring, status),
            READ_CURRENT_SUBTITLE => menu_read_current_subtitle(&wiring, status),
            READ_PREVIOUS_SUBTITLE => menu_read_previous_subtitle(&wiring, status),
            REVIEW_SUBTITLES => open_review_subtitles(&wiring, status),
            SUBTITLE_CAPABILITIES => show_subtitle_capabilities(frame, status),
            OPEN_LOGS => {
                // The real log folder (S05): the sink writes
                // iptvclient_epg_debug.log inside it.
                let dir = iptv_core::applog::logs_dir(&iptv_core::config::user_config_dir());
                let _ = std::fs::create_dir_all(&dir);
                open_folder(&dir);
                status.set_label("Opened the logs folder.");
            }
            COPY_DEBUG => copy_debug_information(&wiring, status),
            ABOUT => open_about(frame),
            _ => {}
        }
    });
}

/// Help > About (audit N25, Python's `AccessibleAboutDialog`):
/// name and version, the feature summary, the community
/// links as real tab stops opening in the default browser,
/// and OK / Close buttons.
fn open_about(frame: Frame) {
    let dialog = Dialog::builder(&frame, &tr("About Accessible IPTV Client"))
        .with_size(560, 380)
        .build();
    dialog.set_accessibility_label(&tr("About Accessible IPTV Client"));
    bind_escape_cancel(dialog);
    let panel = Panel::builder(&dialog).build();
    let heading = StaticText::builder(&panel)
        .with_label(&trf(
            "{name} {version}",
            &[
                ("name", "Accessible IPTV Client"),
                ("version", env!("CARGO_PKG_VERSION")),
            ],
        ))
        .build();
    let _ = heading;
    let description = StaticText::builder(&panel)
        .with_label(&tr(
            "A screen reader accessible IPTV client for Windows and Linux.\n\nSupports M3U/M3U Plus playlists, Stalker Portal, Xtream Codes, built-in VLC playback, casting, and XMLTV EPG.",
        ))
        .build();
    let _ = description;
    let support = StaticText::builder(&panel)
        .with_label(&tr("Community and support"))
        .build();
    let _ = support;
    let ok = Button::builder(&panel).with_label("&OK").build();
    ok.set_accessibility_label("OK");
    let close = Button::builder(&panel).with_label("&Close").build();
    close.set_accessibility_label("Close");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&heading, 0, SizerFlag::All, 12);
    sizer.add(&description, 0, SizerFlag::All, 12);
    sizer.add(&support, 0, SizerFlag::All, 6);
    // Python's three links, labels and URLs verbatim (its
    // `app_links.py`): Telegram group, project page, author
    // page.
    for (label, url) in [
        (
            "Join the SerrebiProjects Telegram group",
            "https://t.me/SerrebiProjects",
        ),
        (
            "Open the Accessible IPTV Client project on GitHub",
            "https://github.com/serrebidev/Accessible-IPTV-Client",
        ),
        ("Follow Serrebi on GitHub", "https://github.com/serrebidev"),
    ] {
        let label_text = tr(label);
        let link = HyperlinkCtrl::builder(&panel)
            .with_label(&label_text)
            .with_url(url)
            .build();
        link.set_accessibility_label(&label_text);
        link.on_clicked(move |_| {
            launch_default_browser(url, BrowserLaunchFlags::Default);
        });
        // The native hyperlink control has no key handling of
        // its own on wxMSW (Python's dialog documents the
        // same fix): Enter, numpad Enter and Space open the
        // link — and only those keys; everything else passes
        // through untouched.
        link.bind_internal(EventType::CHAR_HOOK, move |event| {
            match event.get_key_code() {
                Some(13 | 370 | 32) => {
                    launch_default_browser(url, BrowserLaunchFlags::Default);
                    event.skip(false);
                }
                _ => event.skip(true),
            }
        });
        sizer.add(&link, 0, SizerFlag::All, 8);
    }
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&close, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    let dialog_ok = dialog;
    ok.on_click(move |_| {
        dialog_ok.end_modal(ID_OK);
    });
    let dialog_close = dialog;
    close.on_click(move |_| {
        dialog_close.end_modal(ID_CANCEL);
    });
    dialog.show_modal_tracked();
}

/// Real diagnostics (S05): operational facts plus the redacted,
/// bounded 128 KiB log tail on the clipboard — and a clipboard
/// failure is reported, not swallowed.
fn copy_debug_information(wiring: &Wiring, status: Status) {
    let config = wiring.persist.config.borrow();
    let playlist_count = config
        .value("playlists")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let tail = iptv_core::applog::read_tail(
        &iptv_core::applog::log_path(&iptv_core::config::user_config_dir()),
        iptv_core::applog::DIAGNOSTIC_TAIL_BYTES,
    );
    let text = iptv_core::applog::diagnostics_report(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        &wiring.persist.path,
        config.bool_or("epg_enabled", true),
        playlist_count,
        wiring.recordings.borrow().active_count(),
        &tail,
    );
    drop(config);
    if Clipboard::get().set_text(&text) {
        status.set_label("Log and debug information copied.");
    } else {
        status.set_label("Could not copy diagnostic information: the clipboard is unavailable.");
    }
}

/// File > Import EPG to DB: pick an XMLTV file, remember it as a
/// guide source, and load it now.
fn menu_import_epg(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Import EPG (XMLTV)")
        .with_wildcard("XMLTV (*.xml;*.xml.gz)|*.xml;*.xml.gz|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    // Remember the file under Python's `epgs` key (mirrored to
    // the legacy `epg_urls` so both spellings stay in sync), then
    // load just this source — the per-source import leaves every
    // other source's stored guide intact (audit E01, E04).
    let mut urls = wiring.persist.config.borrow().string_list("epgs");
    if !urls.contains(&path) {
        urls.push(path.clone());
        wiring
            .persist
            .set_and_save("epgs", serde_json::json!(urls.clone()));
        wiring
            .persist
            .set_and_save("epg_urls", serde_json::json!(urls));
    }
    // Python's import guards and announcements (N21,
    // `import_epg`): a disabled guide and an import already
    // running are said out loud, and a started import promises
    // the completion message that `poll_epg` then delivers.
    if !wiring.persist.config.borrow().bool_or("epg_enabled", true) {
        let dialog = MessageDialog::builder(&frame, "EPG is not enabled.", "EPG Not Available")
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
        dialog.show_modal_tracked();
        return;
    }
    if wiring.epg_pending.get() {
        // The load already running re-runs for the new source
        // when it lands (the completion re-check); the user is
        // told an import is in progress, as Python tells them.
        dispatch_epg_load_mode(wiring, vec![path], true);
        let dialog =
            MessageDialog::builder(&frame, "EPG import is already in progress.", "In Progress")
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
        dialog.show_modal_tracked();
        return;
    }
    if !start_manual_epg_import(wiring, vec![path]) {
        let dialog = MessageDialog::builder(
            &frame,
            "The EPG import could not be started.",
            "Import Failed",
        )
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
        .build();
        dialog.show_modal_tracked();
        return;
    }
    status.set_label("Importing EPG data...");
    // Start first, announce second: the announcement promises
    // a completion message, so it must not be shown for an
    // import that never began (Python's ordering).
    let dialog = MessageDialog::builder(
        &frame,
        "EPG import will start in the background. You can keep using the app; a message will tell you when the import has finished.",
        "Import Started",
    )
    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
    .build();
    dialog.show_modal_tracked();
}

/// View > Go to Channel Number...: select the row by its
/// channel number or 1-based position in the current view.
/// The prompt and the two error boxes are Python's (audit
/// N25, `main.py:2962`): an entry that is not a positive
/// number warns, and a number no channel carries and no
/// position reaches reports not found.
fn menu_go_to_channel_number(wiring: &Wiring, frame: Frame, list: ListCtrl, status: Status) {
    let (row, index, number) = {
        let model = wiring.model.borrow();
        if model.is_empty() {
            // Python returns silently when nothing is in scope.
            return;
        }
        let row_count = model.len();
        let dialog =
            TextEntryDialog::builder(&frame, "Enter a channel number:", "Go to Channel Number")
                .build();
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        let Some(text) = dialog.get_value() else {
            return;
        };
        let value = text.trim();
        let number = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
            value.parse::<usize>().unwrap_or(usize::MAX)
        } else {
            0
        };
        if number == 0 {
            let dialog = MessageDialog::builder(
                &frame,
                "Enter a positive channel number.",
                "Go to Channel Number",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
            .build();
            dialog.show_modal_tracked();
            return;
        }
        // Python matches the channel's own number first
        // (`number`, then `tvg-chno`), then falls back to
        // the 1-based position in the scoped list.
        let mut row = None;
        for candidate in 0..row_count {
            let Some(channel) = model.channel(candidate) else {
                continue;
            };
            let own = channel
                .unknown_attributes
                .get("number")
                .filter(|own| !own.is_empty())
                .or_else(|| channel.unknown_attributes.get("tvg-chno"));
            if own.is_some_and(|own| own == value) {
                row = Some(candidate);
                break;
            }
        }
        let row = row.or_else(|| (number <= row_count).then(|| number - 1));
        let Some(row) = row else {
            let number_text = if number == usize::MAX {
                value.to_string()
            } else {
                number.to_string()
            };
            let dialog = MessageDialog::builder(
                &frame,
                &trf(
                    "Channel number {number} was not found.",
                    &[("number", &number_text)],
                ),
                "Go to Channel Number",
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
            .build();
            dialog.show_modal_tracked();
            return;
        };
        let Some(index) = model.playlist_index(row) else {
            return;
        };
        (row, index, number)
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
        status.set_label(&trf(
            "Channel {number}: {name}",
            &[("number", &number.to_string()), ("name", &channel.name)],
        ));
    }
}

/// The Custom Player dialog (audit N25, Python's
/// `CustomPlayerDialog` in `options.py`): a typed path with
/// a Browse picker beside it, seeded with the configured
/// path. Returns the field's text on OK.
fn custom_player_dialog(frame: Frame, initial_path: &str) -> Option<String> {
    let dialog = Dialog::builder(&frame, &tr("Select Custom Player"))
        .with_size(520, 210)
        .build();
    dialog.set_accessibility_label(&tr("Select Custom Player"));
    bind_escape_cancel(dialog);
    let panel = Panel::builder(&dialog).build();
    let label = StaticText::builder(&panel)
        .with_label(&tr("Enter player executable or path:"))
        .build();
    label.set_accessibility_label(&tr("Enter player executable or path:"));
    let text = TextCtrl::builder(&panel).with_value(initial_path).build();
    text.set_accessibility_label(&tr("Enter player executable or path:"));
    let browse = Button::builder(&panel).with_label(&tr("Browse...")).build();
    browse.set_accessibility_label(&tr("Browse..."));
    let ok = Button::builder(&panel).with_label("&OK").build();
    ok.set_accessibility_label("OK");
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    cancel.set_accessibility_label("Cancel");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&label, 0, SizerFlag::All, 8);
    sizer.add(&text, 0, SizerFlag::Expand | SizerFlag::All, 8);
    sizer.add(&browse, 0, SizerFlag::All, 8);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    browse.on_click(move |_| {
        let picker = FileDialog::builder(&dialog)
            .with_message(&tr("Select Player Executable"))
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if picker.show_modal_tracked() == ID_OK {
            if let Some(path) = picker.get_path() {
                text.set_value(&path);
            }
        }
    });
    let chosen = Rc::new(RefCell::new(None::<String>));
    let chosen_ok = Rc::clone(&chosen);
    let dialog_ok = dialog;
    ok.on_click(move |_| {
        *chosen_ok.borrow_mut() = Some(text.get_value());
        dialog_ok.end_modal(ID_OK);
    });
    let dialog_cancel = dialog;
    cancel.on_click(move |_| {
        dialog_cancel.end_modal(ID_CANCEL);
    });
    if dialog.show_modal_tracked() != ID_OK {
        return None;
    }
    let result = chosen.borrow().as_ref().cloned();
    result
}

/// Options > Custom Player...: type or browse to the player
/// executable and make it the media player (audit N25).
/// Python saves nothing for an empty path.
fn menu_custom_player(wiring: &Wiring, frame: Frame, status: Status) {
    let initial = wiring
        .persist
        .config
        .borrow()
        .string_or("custom_player_path", "");
    let Some(path) = custom_player_dialog(frame, &initial) else {
        return;
    };
    let path = path.trim();
    if path.is_empty() {
        return;
    }
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
    if dialog.show_modal_tracked() != ID_OK {
        return None;
    }
    let password = dialog.get_value().unwrap_or_default();
    if password.is_empty() {
        let _ = MessageDialog::builder(&frame, "A backup password is required.", "Settings Backup")
            .with_style(MessageDialogStyle::OK)
            .build()
            .show_modal_tracked();
        return None;
    }
    if confirm {
        let verify =
            TextEntryDialog::builder(&frame, "Enter the password again:", "Settings Backup")
                .password()
                .build();
        if verify.show_modal_tracked() != ID_OK {
            return None;
        }
        if verify.get_value().unwrap_or_default() != password {
            let _ =
                MessageDialog::builder(&frame, "The passwords do not match.", "Settings Backup")
                    .with_style(MessageDialogStyle::OK)
                    .build()
                    .show_modal_tracked();
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
    if dialog.show_modal_tracked() != ID_OK {
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
                .show_modal_tracked();
                status.set_label("Encrypted settings backup saved.");
            }
            Err(error) => status.set_label(&trf(
                "Could not export settings: {error}",
                &[("error", &error.to_string())],
            )),
        },
        Err(error) => status.set_label(&trf(
            "Could not export settings: {error}",
            &[("error", &error)],
        )),
    }
}

/// Work that blocks a settings import (issue #136): an active
/// recording or an unfinished download.
fn import_blocker(wiring: &Wiring) -> Option<&'static str> {
    if wiring.recordings.borrow().active_count() > 0 {
        return Some("recordings are in progress");
    }
    if wiring.downloads.borrow().iter().any(|row| !row.done) {
        return Some("downloads are in progress");
    }
    None
}

/// The recovery step of an import: prompt for the recovery
/// password (entered and confirmed, never stored), save the
/// verified copy where the user chooses, and prove the copy on
/// disk decrypts back to the current settings. Returns the
/// saved path; any failure sets the status and returns None
/// with the settings untouched.
fn make_recovery_copy(wiring: &Wiring, frame: Frame, status: Status) -> Option<String> {
    let recovery_password = backup_password(frame, true)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    let save = FileDialog::builder(&frame)
        .with_message("Save a Recovery Copy of Your Current Settings First")
        .with_default_file(&format!("settings-recovery-{stamp}.aiptv"))
        .with_wildcard("Accessible IPTV backup (*.aiptv)|*.aiptv|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
        .build();
    if save.show_modal_tracked() != ID_OK {
        return None;
    }
    let recovery_path = save.get_path()?;
    let config_json = {
        let config = wiring.persist.config.borrow();
        serde_json::to_vec(&serde_json::Value::Object(config.raw().clone())).unwrap_or_default()
    };
    let recovery = match iptv_core::settings_backup::create_verified_recovery(
        &config_json,
        &recovery_password,
    ) {
        Ok(recovery) => recovery,
        Err(error) => {
            status.set_label(&trf(
                "Import stopped: the recovery copy could not be created: {error} Your settings are unchanged.",
                &[("error", &error)],
            ));
            return None;
        }
    };
    // The copy on disk must verify too — a write that cannot be
    // read back and decrypted blocks the import.
    let on_disk =
        std::fs::write(&recovery_path, &recovery).and_then(|()| std::fs::read(&recovery_path));
    let verified = on_disk.is_ok_and(|bytes| {
        iptv_core::settings_backup::verify_recovery(&bytes, &recovery_password, &config_json)
            .is_ok()
    });
    if !verified {
        status.set_label(
            "Import stopped: the recovery copy could not be verified after writing. Your settings are unchanged.",
        );
        return None;
    }
    status.set_label("Recovery copy saved and verified. Now choose the backup to import.");
    Some(recovery_path)
}

/// Options > Import Settings... (issue #136): before the
/// incoming backup is even read, a verified encrypted recovery
/// copy of the current settings is written to a location the
/// user chooses, under its own entered-and-confirmed password
/// that is never stored. Only then is the backup chosen,
/// decrypted, confirmed, and — after a second active-work
/// check at the moment of applying — swapped in (restart
/// applies, as Python instructs).
fn menu_import_settings(wiring: &Wiring, frame: Frame, status: Status) {
    if let Some(blocker) = import_blocker(wiring) {
        status.set_label(&trf(
            "Import stopped: {blocker}. Finish or stop them, then import. Your settings are unchanged.",
            &[("blocker", blocker)],
        ));
        return;
    }
    let Some(recovery_path) = make_recovery_copy(wiring, frame, status) else {
        return;
    };
    let dialog = FileDialog::builder(&frame)
        .with_message("Import Settings")
        .with_wildcard("Accessible IPTV backup (*.aiptv)|*.aiptv|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal_tracked() != ID_OK {
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
            status.set_label(&trf(
                "Could not import settings: {error}",
                &[("error", &error)],
            ));
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
        &format!(
            "Replace current settings with this backup? A verified recovery copy of your current settings is saved at {recovery_path}. Restart the app after importing."
        ),
        "Import Settings",
    )
    .with_style(MessageDialogStyle::YesNo)
    .build();
    if confirm.show_modal_tracked() != ID_YES {
        return;
    }
    // Rechecked at the moment of applying: work that started
    // while the dialogs were open still blocks the swap.
    if let Some(blocker) = import_blocker(wiring) {
        status.set_label(&trf(
            "Import canceled: {blocker}. Your settings are unchanged; the recovery copy remains saved.",
            &[("blocker", blocker)],
        ));
        return;
    }
    match std::fs::write(&wiring.persist.path, decrypted) {
        Ok(()) => status.set_label("Settings imported. Restart the app to use them."),
        Err(error) => status.set_label(&trf(
            "Could not import settings: {error}",
            &[("error", &error.to_string())],
        )),
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
    let stop_vlc = player.vlc_active;
    player.vlc_active = false;
    if let Some(win) = &player.window {
        win.media.stop();
    }
    player.session.stop();
    drop(player);
    if stop_vlc {
        queue_vlc_stop(&wiring.player);
    }
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
    let text = player
        .subtitle_cues
        .iter()
        .find(|cue| cue.start <= position && position < cue.end)
        .map(|cue| cue.text.clone());
    drop(player);
    match text {
        Some(text) => speak_cue_text(&wiring.player, &wiring.persist, status, &text),
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

/// One automatic update-check attempt: fire the signed check
/// and record the attempt (Python's non-interactive
/// `_start_update_check`, whose worker ends with
/// `_record_auto_update_check_attempt`). Manual checks never
/// touch the epoch. Python records after its worker finishes;
/// ship-shape reports no completion back, so the attempt is
/// recorded as it is launched — the key records when the last
/// automatic check was attempted either way.
fn run_auto_update_check(wiring: &Wiring, frame: &Frame) {
    check_updates(frame, ship_shape::ui::CheckTrigger::Automatic);
    wiring.persist.set_and_save(
        "update_last_auto_check_epoch",
        serde_json::json!(now_epoch()),
    );
}

/// Schedule the launch update check when the user enabled
/// auto-check in Options (Python's
/// `_schedule_auto_update_check`): the check itself is
/// delayed 5 s (Python's `_AUTO_UPDATE_DELAY_AFTER_PLAYLIST_MS`)
/// so playlist loading is not disturbed, and the
/// housekeeping tick fires it when it falls due.
fn maybe_auto_check_updates(wiring: &Wiring) {
    if wiring
        .persist
        .config
        .borrow()
        .bool_or("auto_check_updates", false)
    {
        wiring.auto_check_due.set(now_seconds() + 5.0);
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
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    let toc = ListBox::builder(&panel).build();
    toc.set_accessibility_label("Table of contents");
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
                    status.set_label(&trf("Found \"{query}\".", &[("query", &query)]));
                }
                None => {
                    status.set_label(&trf("Cannot find \"{query}\".", &[("query", &query)]));
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
    ui.status.set_label(&trf(
        "User Guide ({language}). {count} sections. Choose a section, or use Find.",
        &[
            ("language", &guide.language),
            ("count", &guide.sections.len().to_string()),
        ],
    ));
    *ui.guide.borrow_mut() = guide;
}

/// Move the guide text caret to a section.
fn move_caret(ui: &GuideUi, section: &iptv_core::guide::Section) {
    ui.body
        .set_insertion_point(i64::try_from(section.start).unwrap_or(0));
}

/// Context menu command ids on the Channels list (G3).
const DVR_CTX_REFRESH: i32 = 6051;
const DVR_CTX_CANCEL: i32 = 6052;
const DVR_CTX_DELETE: i32 = 6053;
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
            Some(snapshot) => {
                match match_guide_channel(
                    channel,
                    &snapshot.indexes,
                    Some(&snapshot.indexes.known_ids),
                ) {
                    Some(id) => snapshot
                        .guide
                        .programmes
                        .get(&id)
                        .cloned()
                        .unwrap_or_default(),
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        }
    };
    in_memory
}

/// The programme airing now on a channel, when the guide has one.
fn current_programme(wiring: &Wiring, channel: &Channel) -> Option<Programme> {
    guide_now_next(wiring, channel)?.current
}

/// Ask for a reminder lead time in minutes (Python's
/// `NumberEntryDialog`, 0–120; audit R13). None when cancelled
/// or the entry is not a whole number in range.
fn prompt_lead_minutes(frame: Frame, current: i64) -> Option<i64> {
    let dialog = TextEntryDialog::builder(
        &frame,
        "Notify before the programme starts (minutes):",
        "Programme Reminders",
    )
    .with_default_value(&current.to_string())
    .build();
    if dialog.show_modal_tracked() != ID_OK {
        return None;
    }
    dialog
        .get_value()
        .unwrap_or_default()
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|value| (0..=120).contains(value))
}

/// Add a reminder for a programme, asking the lead time first
/// (audit R13: creation offers the 0–120 minute choice, not a
/// fixed five minutes). Creating work restarts the poll (R01).
fn add_reminder(
    wiring: &Wiring,
    frame: Frame,
    channel: &Channel,
    programme: &Programme,
    status: Status,
) {
    let now = now_epoch();
    // Validate before opening the prompt, like Python: a
    // started programme gets the explanation, not a dialog.
    if iptv_core::reminders::build_reminder(channel, programme, 5, now).is_err() {
        status.set_label("Reminders can only be set for programmes that have not started.");
        return;
    }
    let Some(lead) = prompt_lead_minutes(frame, 5) else {
        return;
    };
    match iptv_core::reminders::build_reminder(channel, programme, lead, now) {
        Ok(reminder) => {
            // A (re)created reminder is unseen: it must deliver
            // even if an earlier twin was already shown.
            wiring.reminder_seen.borrow_mut().remove(&reminder.id);
            wiring.reminders.borrow_mut().push(reminder);
            let rows = wiring.reminders.borrow().clone();
            wiring.persist.save_reminders(&rows);
            ensure_polling(wiring);
            status.set_label(&trf(
                "Reminder set for {title} at {time}.",
                &[
                    ("title", &programme.title),
                    ("time", &local_hhmm(programme.start)),
                ],
            ));
        }
        Err(_) => {
            status.set_label("Reminders can only be set for programmes that have not started.");
        }
    }
}

/// Schedule a DVR recording for a programme window, with
/// Python's creation-time checks (audit R15): an exact
/// duplicate is refused (or takes the requested repeat), and
/// overlaps ask first. `repeat` is "none", "daily", "weekly"
/// or "series" (audit R16 authoring). Creating work restarts
/// the poll (R01).
fn add_dvr_job(
    wiring: &Wiring,
    frame: Frame,
    channel: &Channel,
    programme: &Programme,
    status: Status,
    repeat: &str,
) {
    let format = recording_format_for_channel(&wiring.persist, &wiring.media_probe.cache, channel);
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
        repeat,
    };
    let job = match iptv_core::dvr::build_job(&spec, now_seconds()) {
        Ok(job) => job,
        Err(error) => {
            status.set_label(&error);
            return;
        }
    };
    if job.stop_ts <= now_seconds() {
        status.set_label("This programme has already ended.");
        return;
    }
    {
        let mut dvr = wiring.dvr.borrow_mut();
        if let Some(duplicate_id) = dvr.find_duplicate(&job) {
            if repeat != "none" && dvr.set_repeat(&duplicate_id, repeat) {
                dvr.save();
                status.set_label("Recording updated to repeat.");
            } else {
                status.set_label("This programme is already scheduled to record.");
            }
            return;
        }
        let overlaps = dvr.overlapping(&job);
        if !overlaps.is_empty() {
            drop(dvr);
            let answer = MessageDialog::builder(
                &frame,
                &format!(
                    "This recording overlaps {} other scheduled recording(s). Schedule it anyway?",
                    overlaps.len()
                ),
                "Recording Conflict",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build()
            .show_modal_tracked();
            if answer != ID_YES {
                return;
            }
        }
    }
    let mut dvr = wiring.dvr.borrow_mut();
    dvr.add_job(job);
    dvr.save();
    drop(dvr);
    if repeat == "series" {
        // Fill the new rule from the guide on the next tick.
        wiring.series_last_scan.set(0.0);
    }
    ensure_polling(wiring);
    status.set_label(&trf(
        "Recording scheduled: {title} at {time}.",
        &[
            ("title", &programme.title),
            ("time", &local_hhmm(programme.start)),
        ],
    ));
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
        let Some(channel) = selected_channel(&wiring, list) else {
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
        let Some(channel) = selected_channel(&wiring_menu, list) else {
            return;
        };
        match event.get_id() {
            CTX_PLAY => activate_channel(&wiring_menu, list, status),
            CTX_FAVORITE => toggle_selected_favorite(&wiring_menu, list, search, status),
            CTX_RECORD => {
                toggle_recording(
                    &frame,
                    &wiring_menu.recordings,
                    &wiring_menu.buffers,
                    &wiring_menu.persist,
                    &wiring_menu.media_probe,
                    &channel,
                    status,
                    Some(&wiring_menu.player),
                    &wiring_menu.share_ctx(),
                );
                assert!(wiring_menu.timer.start(50, false));
            }
            // On an EPG search row, Reminder and Schedule act on
            // the row's own programme (Python's EPG-row menu —
            // the user already picked that programme). On a plain
            // channel row they open the upcoming-programme
            // chooser instead (audit R14): the airing programme
            // is never the target, since a reminder for it is
            // impossible by construction.
            CTX_REMINDER => {
                let row_programme = selected_row(list)
                    .and_then(|row| row_epg(&wiring_menu, row))
                    .map(|epg_row| epg_row.programme);
                match row_programme {
                    Some(programme) => {
                        add_reminder(&wiring_menu, frame, &channel, &programme, status);
                    }
                    None => open_channel_epg(&wiring_menu, frame, &channel, status),
                }
            }
            CTX_SCHEDULE => {
                let row_programme = selected_row(list)
                    .and_then(|row| row_epg(&wiring_menu, row))
                    .map(|epg_row| epg_row.programme);
                match row_programme {
                    Some(programme) => {
                        add_dvr_job(&wiring_menu, frame, &channel, &programme, status, "none");
                    }
                    None => open_channel_epg(&wiring_menu, frame, &channel, status),
                }
            }
            CTX_EPG => open_channel_epg(&wiring_menu, frame, &channel, status),
            CTX_CATCHUP => play_catchup(&wiring_menu, &channel, status),
            _ => {}
        }
    });
}

/// Resolve and play the catch-up stream for the channel's current
/// programme (Python's Catch-up context action).
fn play_catchup(wiring: &Wiring, channel: &Channel, status: Status) {
    if wiring.epg.borrow().is_none() {
        let now = now_epoch();
        submit_epg_read(
            wiring,
            iptv_core::epg::EpgReadKind::NowNext {
                channel: channel.clone(),
                at: now,
            },
            PendingEpgReadUi::CatchupCurrent {
                channel: channel.clone(),
                status,
            },
        );
        status.set_label("Loading current programme...");
        return;
    }
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
            play_channel(
                &wiring.player,
                &wiring.persist,
                &wiring.media_probe,
                channel,
                status,
            );
        }
        Ok(iptv_core::catchup::ShowResolution::Catchup(url)) => {
            let mut shifted = channel.clone();
            shifted.url = url;
            play_channel(
                &wiring.player,
                &wiring.persist,
                &wiring.media_probe,
                &shifted,
                status,
            );
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
/// Keep a Channel EPG dialog's description field on the
/// highlighted programme as the selection moves.
fn bind_epg_description(
    list: ListBox,
    description: TextCtrl,
    programmes: Rc<RefCell<Vec<Programme>>>,
) {
    list.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |event| {
        let index = list
            .get_selection()
            .map_or(usize::MAX, |selected| selected as usize);
        if let Some(programme) = programmes.borrow().get(index) {
            description.set_value(&programme.description);
        }
        event.skip(true);
    });
}

/// The EPG database predates this build's schema; offer to
/// rebuild it (N21; Python's `_offer_epg_schema_repair`). A
/// store that cannot add the missing columns itself leaves
/// every EPG view dying on them, so one clear dialog with a
/// working fix beats that: Yes starts a forced import (which
/// reports when it finishes), and a start that fails says so.
fn offer_epg_schema_repair(wiring: &Wiring, frame: Frame) {
    let dialog = MessageDialog::builder(
        &frame,
        "The EPG database was created by an older version and is missing newer fields. Import the guide again to update it?\n\n(Without this, the EPG views cannot show programme descriptions.)",
        "EPG Database Update Needed",
    )
    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
    .build();
    if dialog.show_modal_tracked() != ID_YES {
        return;
    }
    if !start_manual_epg_import(wiring, effective_epg_sources(wiring)) {
        let dialog = MessageDialog::builder(
            &frame,
            "The EPG import could not be started.",
            "Import Failed",
        )
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
        .build();
        dialog.show_modal_tracked();
    }
}

#[allow(clippy::too_many_lines)] // grew with R16 repeat buttons
fn open_channel_epg(wiring: &Wiring, frame: Frame, channel: &Channel, _main_status: Status) {
    let now = now_epoch();
    // Python's Channel EPG query: from the programme on air now
    // through seven days of upcoming programmes, uncapped —
    // finished programmes belong to Catch-up, not this window.
    let stored = guide_programmes(wiring, channel);
    if stored.is_empty()
        && !iptv_core::epgstore::store_missing_columns(&epg_store_path()).is_empty()
    {
        // The view would fall back to the store, and the store
        // predates the schema: offer the repair instead of a
        // window that can only fail to load (Python's
        // `_view_channel_epg` schema check, N21).
        offer_epg_schema_repair(wiring, frame);
        return;
    }
    let programmes: Vec<Programme> = stored
        .into_iter()
        .filter(|p| p.stop > now && p.start < now + 7 * 86_400)
        .collect();
    // Begin on the programme airing now, not row zero.
    let initial = airing_index(&programmes, now).unwrap_or(0);
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
        .map(|p| format!("{} {}", epg_time_range(p, now), p.title))
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    if !programmes.is_empty() {
        list.set_selection(u32::try_from(initial).unwrap_or(0), true);
    }
    let reminder_button = Button::builder(&panel).with_label("Set &Reminder").build();
    let schedule_button = Button::builder(&panel)
        .with_label("Schedule &Recording")
        .build();
    let close_button = Button::builder(&panel).with_label("&Close").build();
    let status_label = StaticText::builder(&panel).with_label("EPG status").build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .build();
    details.set_accessibility_label("EPG status");
    let status = Status {
        label: status_label,
        details,
        bar: None,
    };
    // Tab from the list reaches the highlighted programme's
    // description (Python's Channel EPG dialog): a read-only
    // field that follows the selection, never action instructions.
    let description = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value(
            programmes
                .get(initial)
                .map(|p| p.description.as_str())
                .unwrap_or_default(),
        )
        .build();
    description.set_accessibility_label("Episode description");
    let programmes = Rc::new(RefCell::new(programmes));
    bind_epg_description(list, description, Rc::clone(&programmes));
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
            add_reminder(&wiring_r, frame, &channel_r, programme, status);
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
            add_dvr_job(&wiring_s, frame, &channel_s, programme, status, "none");
        }
    });
    // R16: recurrence authoring from programme rows — Record Daily,
    // Weekly and Series, like Python's programme context menu.
    for (label, repeat) in [
        ("Record &Daily", "daily"),
        ("Record &Weekly", "weekly"),
        ("Record &Series", "series"),
    ] {
        let button = Button::builder(&panel).with_label(label).build();
        let wiring_rep = clone_wiring(wiring);
        let programmes_rep = Rc::clone(&programmes);
        let channel_rep = channel.clone();
        button.on_click(move |_| {
            let Some(index) = list.get_selection() else {
                return;
            };
            if let Some(programme) = programmes_rep
                .borrow()
                .get(usize::try_from(index).unwrap_or(usize::MAX))
            {
                add_dvr_job(&wiring_rep, frame, &channel_rep, programme, status, repeat);
            }
        });
    }
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
    // F1 anywhere in this window opens the guide at the channel
    // guide topic (S10).
    bind_f1_help(&frame, wiring, "channel-epg", status);
    bind_f1_help(&list, wiring, "channel-epg", status);
    bind_f1_help(&details, wiring, "channel-epg", status);
    frame.show(true);
    if programmes.borrow().is_empty() {
        status.set_label("Loading guide information...");
        let now = now_epoch();
        submit_epg_read(
            wiring,
            iptv_core::epg::EpgReadKind::Programmes {
                channel: channel.clone(),
                from: now - 7 * 86_400,
                to: now + 7 * 86_400,
            },
            PendingEpgReadUi::ChannelProgrammes {
                list,
                description,
                programmes: Rc::clone(&programmes),
                status,
            },
        );
    }
}

fn bind_playback(
    wiring: &Wiring,
    list: ListCtrl,
    search: TextCtrl,
    status: Status,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
) {
    bind_epg_highlight(wiring, list, episode_description, stream_url);
    let activation = clone_wiring(wiring);
    list.on_item_activated(move |_| activate_channel(&activation, list, status));
    // N22: Delete removes the picked channel from Favorites, and
    // only then (Python main.py:2664 runs the favorite toggle for
    // a favorite row); any other Delete passes through untouched.
    let keys = clone_wiring(wiring);
    list.bind_internal(EventType::KEY_DOWN, move |event| {
        let plain_delete = event.get_key_code() == Some(127)
            && !event.control_down()
            && !event.alt_down()
            && !event.shift_down();
        let remove = plain_delete
            && selected_row(list)
                .and_then(|row| row_playlist_index(&keys, row))
                .is_some_and(|index| keys.browser.borrow().is_favorite(index));
        if remove {
            toggle_selected_favorite(&keys, list, search, status);
            event.skip(false);
        } else {
            event.skip(true);
        }
    });
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
    // An EPG search row plays its programme through its channel:
    // the catch-up resolution decides live versus archive from
    // the programme's window, exactly as Python's activation does.
    if let Some(epg_row) = row_epg(wiring, row) {
        let Some(index) = epg_row.playlist_index else {
            status.set_label("Could not match this programme to a playlist channel.");
            return;
        };
        let Some(channel) = wiring.browser.borrow().channel(index) else {
            status.set_label("Could not match this programme to a playlist channel.");
            return;
        };
        play_catchup_for(wiring, &channel, &epg_row.programme, status);
        return;
    }
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
    play_channel(
        &wiring.player,
        &wiring.persist,
        &wiring.media_probe,
        &channel,
        status,
    );
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
    let wiring = clone_wiring(wiring);
    list.on_item_selected(move |event| {
        let Some(row) = usize::try_from(event.get_item_index()).ok() else {
            return;
        };
        // An EPG search row shows its own programme's description
        // and its channel's URL (Python's EPG-row highlight).
        if let Some(epg_row) = row_epg(&wiring, row) {
            if let Some(channel) = epg_row
                .playlist_index
                .and_then(|index| wiring.browser.borrow().channel(index))
            {
                stream_url.set_value(&channel.url);
            }
            episode_description.set_value(&epg_row.programme.description);
            return;
        }
        let Some(index) = wiring.model.borrow().playlist_index(row) else {
            return;
        };
        let Some(channel) = wiring.browser.borrow().channel(index) else {
            return;
        };
        // N16: warm the media-type cache once the selection
        // settles, so the Recording Format menu and the
        // recording paths know audio-only streams before they
        // are used (Python's highlight handler).
        schedule_media_probe(&wiring, &channel);
        stream_url.set_value(&channel.url);
        // The panel carries the full now/next context — current
        // title, times and description, then the next programme
        // and its description (audit E10) — not just a bare
        // description or a placeholder when a next programme
        // exists.
        if let Some(entry) = guide_now_next(&wiring, &channel) {
            episode_description.set_value(&epg_info_message(
                entry.current.as_ref(),
                entry.next.as_ref(),
            ));
        } else if wiring.epg.borrow().is_none() {
            submit_epg_read(
                &wiring,
                iptv_core::epg::EpgReadKind::NowNext {
                    channel: channel.clone(),
                    at: now_epoch(),
                },
                PendingEpgReadUi::Highlight {
                    channel,
                    list,
                    text: episode_description,
                    model_ptr: wiring.model.borrow().playlist_ptr(),
                },
            );
        } else {
            episode_description.set_value("");
        }
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

/// Monotonic milliseconds since process start (P20): launch
/// debounce, session recovery deadlines and the sleep timer
/// measure *elapsed* time, so a wall-clock correction must not
/// move them. Wall-clock needs (EPG, DVR, stored timestamps)
/// use `now_epoch` / `now_seconds` instead.
fn now_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The answer to the resume choice (P15).
#[derive(Clone, Copy, Eq, PartialEq)]
enum ResumeChoice {
    Resume,
    Beginning,
    Cancel,
}

/// Python's `choose_resume` (P15): "Resume playback from
/// H:MM:SS?" with Resume, Play from Beginning — the default,
/// as Python's `NO_DEFAULT` makes it — and Cancel.
fn choose_resume(player: &Rc<RefCell<PlayerState>>, position_ms: i64) -> ResumeChoice {
    let parent = player
        .borrow()
        .window
        .as_ref()
        .map_or_else(|| Frame::builder().build(), |win| win.frame);
    let total_secs = position_ms / 1000;
    let message = format!(
        "Resume playback from {}:{:02}:{:02}?",
        total_secs / 3600,
        (total_secs % 3600) / 60,
        total_secs % 60
    );
    let dialog = Dialog::builder(&parent, "Resume").build();
    dialog.set_accessibility_label("Resume");
    bind_escape_cancel(dialog);
    let panel = Panel::builder(&dialog).build();
    let label = StaticText::builder(&panel).with_label(&message).build();
    label.set_accessibility_label(&message);
    let chosen = Rc::new(std::cell::Cell::new(ResumeChoice::Cancel));
    let resume = Button::builder(&panel).with_label("&Resume").build();
    resume.set_accessibility_label("Resume");
    let beginning = Button::builder(&panel)
        .with_label("Play from &Beginning")
        .build();
    beginning.set_accessibility_label("Play from Beginning");
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    cancel.set_accessibility_label("Cancel");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&label, 0, SizerFlag::All, 8);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&resume, 0, SizerFlag::All, 4);
    buttons.add(&beginning, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    for (button, choice) in [
        (resume, ResumeChoice::Resume),
        (beginning, ResumeChoice::Beginning),
        (cancel, ResumeChoice::Cancel),
    ] {
        let chosen_b = Rc::clone(&chosen);
        let dialog_b = dialog;
        button.on_click(move |_| {
            chosen_b.set(choice);
            dialog_b.end_modal(ID_OK);
        });
    }
    // "Play from Beginning" is the default button (Python's
    // NO_DEFAULT), so it takes the focus.
    beginning.set_focus();
    if dialog.show_modal_tracked() == ID_OK {
        chosen.get()
    } else {
        ResumeChoice::Cancel
    }
}

fn play_channel(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    media: &MediaProbe,
    channel: &Channel,
    status: Status,
) {
    // N16: playback preempts any classification probe
    // (Python `_terminate_media_probe` at the play entry).
    terminate_media_probes(media);
    {
        // A new play replaces any play waiting on libVLC, and may
        // retry a libVLC that failed to load earlier.
        let mut state = player.borrow_mut();
        state.pending_play = None;
        state.vlc_error = None;
    }
    let probe = PlaybackRequest::for_channel(channel, None);
    let mut resume = None;
    if !probe.live {
        // Python's prepare_vod_resume: the outgoing stream's
        // checkpoint is force-saved before the lookup, and a
        // saved position is offered, never silently applied
        // (P15). Cancel aborts the launch entirely — the
        // current stream keeps playing, the bookmark is
        // untouched.
        checkpoint_resume(player, persist, status, true);
        let before = persist.resume_entries();
        let (entries, key) = iptv_core::bookmarks::lookup(&before, channel);
        if let Some(entry) = entries.get(&key) {
            let position = entry.position_ms;
            match choose_resume(player, position) {
                ResumeChoice::Resume => {
                    resume = Some(position);
                    if entries != before {
                        persist.save_resume(&entries);
                    }
                }
                ResumeChoice::Beginning => {
                    let mut entries = entries;
                    entries.remove(&key);
                    persist.save_resume(&entries);
                }
                ResumeChoice::Cancel => return,
            }
        } else if entries != before {
            persist.save_resume(&entries);
        }
    }
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
                Some(exe) => {
                    if launch_external(player, &name, &exe, &request, status) && request.live {
                        record_recent(persist, channel);
                    }
                }
                None => status.set_label(&trf(
                    "Could not locate {name} executable.",
                    &[("name", &name)],
                )),
            }
        }
        PlayerChoice::Custom(path) => {
            if path.is_empty() {
                status.set_label("No custom player path is configured.");
            } else {
                let name = detect_from_path(&path);
                if launch_external(player, &name, &path, &request, status) && request.live {
                    record_recent(persist, channel);
                }
            }
        }
    }
}

/// Record one live channel in the persisted recent-channels
/// MRU (P17). Python records only live kind, and only on a
/// confirmed start: the built-in route records when the
/// engine confirms Playing (see the poll loops), the external
/// route when the launch is accepted and spawned.
fn record_recent(persist: &Persist, channel: &Channel) {
    let key = channel_key(channel);
    if key.is_empty() {
        return;
    }
    let keys = remember_recent(&persist.recent_keys(), &key);
    persist.save_recent_keys(&keys);
}

/// The built-in route's half of P17 recording: once per play,
/// when the session's current request is live and playback
/// has been confirmed.
fn record_history_once(player: &Rc<RefCell<PlayerState>>, persist: &Persist) {
    let channel = {
        let mut state = player.borrow_mut();
        if state.history_recorded || !state.session.current().is_some_and(|request| request.live) {
            return;
        }
        state.history_recorded = true;
        state.current_channel.clone()
    };
    if let Some(channel) = channel {
        record_recent(persist, &channel);
    }
}

/// Launch an external player; true when the process spawned
/// (a debounced duplicate or a spawn failure is false).
fn launch_external(
    player: &Rc<RefCell<PlayerState>>,
    name: &str,
    exe: &str,
    request: &PlaybackRequest,
    status: Status,
) -> bool {
    let ipc = std::env::temp_dir()
        .join("accessible-iptv-mpv.sock")
        .to_string_lossy()
        .into_owned();
    let accepted = player
        .borrow_mut()
        .gate
        .accept(name, exe, &request.url, now_ms());
    if accepted.is_none() {
        return false; // Debounced duplicate, as in Python: accepted silently.
    }
    let argv = external_argv(name, exe, &request.url, &ipc);
    match Command::new(&argv[0]).args(&argv[1..]).spawn() {
        Ok(_) => {
            status.set_label(&trf(
                "Playing {title} in {name}.",
                &[("title", &request.title), ("name", name)],
            ));
            true
        }
        Err(error) => {
            status.set_label(&trf(
                "Could not start {name}: {error}",
                &[("name", name), ("error", &error.to_string())],
            ));
            false
        }
    }
}

/// Whether the channel's metadata declares it audio-only
/// (radio), which keeps the freeze watchdog's video evidence
/// disarmed (P03).
fn is_audio_only_channel(channel: &Channel) -> bool {
    iptv_core::playback::audio_only_hint(&channel.unknown_attributes)
}

/// Resolve an HLS master URL against the rendition cap on a
/// worker, waiting at most Python's 400 ms probe window; any
/// failure or timeout keeps the master URL (P05).
fn resolve_variant_url(
    url: &str,
    headers: &iptv_core::playback::StreamHeaders,
    cap: f64,
) -> String {
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    let worker_url = url.to_string();
    let worker_headers = headers.clone();
    std::thread::spawn(move || {
        let resolved = buffer::resolve_hls_input(&worker_url, &worker_headers, Some(cap))
            .unwrap_or(worker_url);
        let _ = sender.send(resolved);
    });
    receiver
        .recv_timeout(std::time::Duration::from_millis(400))
        .unwrap_or_else(|_| url.to_string())
}

/// One transport operation for the VLC ops worker (P18).
/// Loads and stops execute in send order on one background
/// thread — Python's `VLCControl` serialization — so a slow
/// open or stop never blocks the keyboard, and a stop can
/// never overtake the load it belongs to.
enum VlcOp {
    Load {
        engine: iptv_vlc::VlcEngine,
        url: String,
        headers: iptv_core::playback::StreamHeaders,
        options: iptv_vlc::MediaOptions,
        reply: std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    },
    Stop {
        engine: iptv_vlc::VlcEngine,
    },
}

fn spawn_vlc_ops() -> std::sync::mpsc::Sender<VlcOp> {
    let (sender, receiver) = std::sync::mpsc::channel::<VlcOp>();
    std::thread::spawn(move || {
        while let Ok(op) = receiver.recv() {
            match op {
                VlcOp::Load {
                    engine,
                    url,
                    headers,
                    options,
                    reply,
                } => {
                    let result = engine.play_url(&url, &headers, &options);
                    if let Ok(mut guard) = reply.lock() {
                        *guard = Some(result);
                    }
                }
                VlcOp::Stop { engine } => engine.stop(),
            }
        }
    });
    sender
}

/// Hand the engine's final release to a detached thread at exit
/// (audit P18 residual): libvlc's player/instance release can
/// block, and Python tears its player down in a worker, so the
/// last engine handles must not drop on the UI thread while the
/// app closes. Taking the sender hangs the ops channel up: the
/// worker drains its queued ops, drops its own engine clones on
/// its thread, and exits; the state's clone drops on the cleanup
/// thread. Any clone still queued inside an op drops with that
/// op on the worker thread.
fn release_engine_off_thread(player: &Rc<RefCell<PlayerState>>) {
    let (vlc, ops) = {
        let mut state = player.borrow_mut();
        (state.vlc.take(), state.vlc_ops.take())
    };
    if vlc.is_none() && ops.is_none() {
        return;
    }
    std::thread::spawn(move || {
        drop(ops);
        drop(vlc);
    });
}

/// Start libVLC on its own thread.
fn spawn_vlc_loader() -> std::sync::mpsc::Receiver<Result<iptv_vlc::VlcEngine, String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(iptv_vlc::VlcEngine::new());
    });
    receiver
}

/// Warm libVLC at startup when the built-in player is the choice,
/// so the first play does not wait for it.
fn preload_vlc_engine(player: &Rc<RefCell<PlayerState>>, persist: &Persist) {
    let built_in = {
        let config = persist.config.borrow();
        player_choice(
            &config.string_or("media_player", "Built-in Player"),
            &config.string_or("custom_player_path", ""),
        ) == PlayerChoice::BuiltIn
    };
    if built_in {
        player.borrow_mut().vlc_loader = Some(spawn_vlc_loader());
    }
}

/// Whether the libVLC start has settled, in either direction. A
/// failure stays in `vlc_error` (the caller falls back to the basic
/// player) until the next user play clears it to retry. Never blocks.
fn vlc_engine_settled(state: &mut PlayerState) -> bool {
    use std::sync::mpsc::TryRecvError;
    if state.vlc.is_some() || state.vlc_error.is_some() {
        return true;
    }
    let loader = state.vlc_loader.get_or_insert_with(spawn_vlc_loader);
    let outcome = match loader.try_recv() {
        Ok(outcome) => outcome,
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => Err("libVLC start was interrupted.".to_string()),
    };
    state.vlc_loader = None;
    match outcome {
        Ok(engine) => {
            state.vlc = Some(engine);
            state.vlc_ops = Some(spawn_vlc_ops());
        }
        Err(error) => state.vlc_error = Some(error),
    }
    true
}

/// Run the play that was waiting for libVLC, once libVLC settled.
fn run_pending_play(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let pending = {
        let mut state = player.borrow_mut();
        if state.pending_play.is_none() || !vlc_engine_settled(&mut state) {
            return;
        }
        state.pending_play.take()
    };
    if let Some((channel, request)) = pending {
        play_builtin(player, persist, &channel, request, status);
    }
}

/// The media options for one load. A retry load carries the cache
/// plan's retry value, a rollover load the quick-start value
/// (P01/P04), and every load keeps the stream's video choice.
/// The system proxy and an on-demand resume position ride along
/// the way Python passes them (audit N10).
fn vlc_media_options(
    state: &PlayerState,
    live: bool,
    retry: bool,
    resume_ms: Option<i64>,
) -> iptv_vlc::MediaOptions {
    iptv_vlc::MediaOptions {
        cache_ms: state
            .cache_plan
            .cache_ms(live, retry, retry && state.reconnect_rollover),
        live,
        video_visible: state.video_visible,
        proxy: iptv_core::playback::system_http_proxy(),
        start_time_seconds: match (live, resume_ms) {
            #[allow(clippy::cast_precision_loss)] // ms to f64 seconds, as Python's /1000
            (false, Some(ms)) if ms > 0 => Some(ms as f64 / 1000.0),
            _ => None,
        },
    }
}

/// Queue a load on the ops worker; the result lands in the
/// state's reply slot for `deliver_vlc_load`. False when no
/// engine or worker exists (the caller falls back).
fn queue_vlc_load(
    player: &Rc<RefCell<PlayerState>>,
    request: &PlaybackRequest,
    options: iptv_vlc::MediaOptions,
    retry: bool,
) -> bool {
    let (engine, sender, generation) = {
        let state = player.borrow();
        (
            state.vlc.clone(),
            state.vlc_ops.clone(),
            state.session.generation(),
        )
    };
    let (Some(engine), Some(sender)) = (engine, sender) else {
        return false;
    };
    let reply = std::sync::Arc::new(std::sync::Mutex::new(None));
    player.borrow_mut().vlc_load_reply = Some((generation, retry, std::sync::Arc::clone(&reply)));
    sender
        .send(VlcOp::Load {
            engine,
            url: request.url.clone(),
            headers: request.headers.clone(),
            options,
            reply,
        })
        .is_ok()
}

/// Queue a stop on the ops worker, ordered after any load
/// already queued.
fn queue_vlc_stop(player: &Rc<RefCell<PlayerState>>) {
    let (engine, sender) = {
        let state = player.borrow();
        (state.vlc.clone(), state.vlc_ops.clone())
    };
    if let (Some(engine), Some(sender)) = (engine, sender) {
        let _ = sender.send(VlcOp::Stop { engine });
    }
}

/// Deliver a finished worker load (P18): a superseded result
/// is dropped; an initial load the full player refused falls
/// back to the basic player for that request; a refused retry
/// feeds the session's recovery as an error.
fn deliver_vlc_load(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let ready = {
        let state = player.borrow();
        state
            .vlc_load_reply
            .as_ref()
            .and_then(|(generation, retry, slot)| {
                slot.lock()
                    .ok()
                    .and_then(|mut guard| guard.take().map(|result| (*generation, *retry, result)))
            })
    };
    let Some((generation, retry, result)) = ready else {
        return;
    };
    player.borrow_mut().vlc_load_reply = None;
    if player.borrow().session.generation() != generation {
        return; // superseded or stopped while loading
    }
    let Err(error) = result else {
        return;
    };
    let mut state = player.borrow_mut();
    if !retry && state.engine == ActiveEngine::Vlc && !state.session.played_once() {
        state.engine = ActiveEngine::Fallback;
        state.vlc_active = false;
        state.vlc_error = Some(error);
        let request = state.session.current().cloned();
        if let (Some(win), Some(request)) = (&state.window, request) {
            win.media.load_uri(&request.url);
            win.media.play();
        }
        drop(state);
        apply_engine_to_playback_menu(player);
        let state = player.borrow();
        if let Some(error) = &state.vlc_error {
            status.set_label(&format!(
                "The full player could not play this stream ({error}); basic player in use."
            ));
        }
    } else {
        let action = state.session.on_event(EngineEvent::Error(error), now_ms());
        drop(state);
        store_session_action(player, persist, status, action);
    }
}
#[allow(clippy::too_many_lines)]
fn play_builtin(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    channel: &Channel,
    mut request: PlaybackRequest,
    status: Status,
) {
    ensure_player_window(player, persist, status);
    {
        let mut state = player.borrow_mut();
        if !vlc_engine_settled(&mut state) {
            // libVLC is still starting: subtitle_tick runs this
            // play when it settles. The UI thread never waits.
            state.pending_play = Some((channel.clone(), request));
            drop(state);
            player_status(status, persist, "Preparing the player...", 3);
            return;
        }
    }
    if !request.buffer_bypass && buffer_intercept(player, persist, channel, &request, status) {
        return;
    }
    // Direct HLS playback honours the rendition bitrate cap
    // (P05): resolve the master against the saved cap the way
    // Python does (bounded wait, master URL on any failure).
    // Buffer-owned and buffer-replay URLs bypass resolution.
    if !request.buffer_bypass {
        let cap = persist
            .config
            .borrow()
            .internal_player_settings()
            .variant_max_mbps;
        let lowered = request.url.to_lowercase();
        if cap > 0.0 && (lowered.contains("m3u8") || lowered.contains("manifest")) {
            request.url = resolve_variant_url(&request.url, &request.headers, cap);
        }
    }
    ensure_player_window(player, persist, status);
    let mut state = player.borrow_mut();
    state.current_channel = Some(channel.clone());
    state.pending_resume_ms = request.resume_ms;
    state.pending_reconnect = None;
    state.reconnect_rollover = false;
    // A new choice restarts the cache plan at the saved opening
    // value (P01) and re-arms the per-channel audio supply (P06).
    state.cache_plan =
        iptv_core::playback::CachePlan::new(&persist.config.borrow().internal_player_settings());
    state.dispatcharr = channel.unknown_attributes.contains_key("catchup-timezone")
        || channel.unknown_attributes.contains_key("dispatcharr");
    let audio_key =
        iptv_core::audiotracks::audio_memory_key(&channel.name, &channel.tvg_name, &channel.tvg_id);
    let (names, indices) = persist.audio_memory();
    state.supply_audio_name = names.get(&audio_key).cloned();
    state.supply_audio_index = indices.get(&audio_key).copied();
    state.audio_key = audio_key;
    state.video_visible = persist
        .config
        .borrow()
        .bool_or("show_player_on_enter", true);
    state.watchdog = iptv_core::playback::FreezeWatchdog::new(
        is_audio_only_channel(channel),
        state.video_visible,
    );
    state.track_attempts = 0;
    // N23: the status line starts fresh for a new channel —
    // no remembered prefix or track, and the bitrate hint is
    // read from this stream's URL.
    state.status_prefix.clear();
    state.audio_track_label.clear();
    state.status_bitrate_mbps = iptv_core::playback::bitrate_hint_from_url(&request.url);
    state.selected_spu = -2;
    state.file_subtitle_track = None;
    state.spu_ids.clear();
    // A channel change discards the old programme's spoken cue
    // and invalidates any in-flight subtitle parse (P09).
    state.app_voice.stop();
    state.subtitle_cues.clear();
    state.last_spoken_cue = None;
    state.subtitle_generation += 1;
    state.pending_subtitles = None;
    // Review starts empty on a new stream (P11), history
    // recording re-arms (P17), and an adopted resume position
    // is protected until the seek is confirmed (P16).
    state.review.reset();
    state.active_cue_index = None;
    state.last_shown_cue = None;
    state.shown_cues.clear();
    state.history_recorded = false;
    state.resume_guard = request
        .resume_ms
        .map(|ms| iptv_core::playback::ResumeGuard::new(ms, now_ms()));
    let action = state.session.play(request, now_ms());
    let Some(win) = state.window.clone() else {
        return;
    };
    if let EngineAction::Load(load) = action {
        state.vlc_active = false;
        if let Some(engine) = &state.vlc {
            // Python's hidden play detaches the window, so libVLC
            // cannot open a video window of its own.
            engine.set_window_handle(if state.video_visible {
                win.media.get_handle() as usize
            } else {
                0
            });
        }
        let options = vlc_media_options(&state, load.live, false, load.resume_ms);
        let has_vlc = state.vlc.is_some();
        drop(state);
        // The load runs on the ops worker (P18); its result is
        // delivered by deliver_vlc_load, which falls back to
        // the basic player if the full player refuses.
        let via_vlc = has_vlc && queue_vlc_load(player, &load, options, false);
        state = player.borrow_mut();
        state.engine = if via_vlc {
            ActiveEngine::Vlc
        } else {
            ActiveEngine::Fallback
        };
        if via_vlc {
            state.vlc_active = true;
            state.vlc_tracks_ready = false;
            state.play_started_ms = now_ms();
            state.watchdog.reset();
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
            status.set_label(&trf(
                "libVLC is not available ({error}); basic player in use, many streams will not play.",
                &[("error", error)],
            ));
        }
    }
    drop(state);
    apply_engine_to_playback_menu(player);
}

/// Reflect the active engine in the player window (issue #129):
/// the full player (libVLC) offers audio-track selection and an
/// audio output device choice; the basic player (`MediaCtrl`)
/// cannot, so its Audio Track submenu says so in words and the
/// Audio Output Device item is disabled — a screen reader user
/// meets the explanation in the menu instead of a control that
/// silently does nothing. Supported controls (play/pause, stop,
/// seek, volume, subtitle cues, buffered playback) stay enabled
/// under both engines.
fn apply_engine_to_playback_menu(player: &Rc<RefCell<PlayerState>>) {
    let (engine, win) = {
        let state = player.borrow();
        (state.engine, state.window.clone())
    };
    let Some(win) = win else {
        return;
    };
    let full = engine == ActiveEngine::Vlc;
    if let Some(menu) = win.frame.get_menu_bar().and_then(|bar| bar.get_menu(0)) {
        menu.enable_item(PM_AUDIO_DEVICE, full);
    }
    if let Some(menu) = audio_track_menu(player) {
        // Drop whatever the previous play left in the submenu,
        // then restore the placeholder with the engine's text.
        let stale: Vec<i32> = {
            let mut state = player.borrow_mut();
            state.track_menu_ids.drain(..).collect()
        };
        for old in stale {
            menu_delete_if_present(&menu, old);
        }
        menu_delete_if_present(&menu, PM_NO_TRACKS);
        if full {
            menu_add(
                &menu,
                PM_NO_TRACKS,
                "No audio tracks available",
                "",
                ItemKind::Normal,
            );
        } else {
            menu_add(
                &menu,
                PM_NO_TRACKS,
                "Audio tracks are not available with the basic player",
                "",
                ItemKind::Normal,
            );
            // The window's track list belongs to the full player
            // too; stale choices must not survive a fallback play.
            win.tracks.clear();
            player.borrow_mut().vlc_track_ids.clear();
        }
        menu.enable_item(PM_NO_TRACKS, false);
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
            {
                let mut state = player.borrow_mut();
                // A failure recovery grows the cache base by one
                // second for the retry (P01; Dispatcharr live and
                // the rollover path never grow).
                let live = request.live;
                let dispatcharr = state.dispatcharr;
                state.cache_plan.grow(dispatcharr, live);
                state.reconnect_rollover = false;
                state.pending_reconnect =
                    Some((state.session.generation(), now_ms() + delay, request));
            }
            // N23: Python names every recovery attempt on the
            // status line at priority 2.
            update_player_status(player, persist, status, Some("Reconnecting..."), 2);
        }
        EngineAction::Rollover(request, delay) => {
            let mut state = player.borrow_mut();
            state.reconnect_rollover = true;
            state.pending_reconnect = Some((state.session.generation(), now_ms() + delay, request));
            player_status(status, persist, "Refreshing stream…", 3);
        }
        EngineAction::Stop => settle_session(player, persist, status),
        _ => {}
    }
}

/// Settle a session that asked to stop: silence the engines, and
/// when the stream was lost say so at top priority with the long
/// report in the status details.
fn settle_session(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let mut stop_vlc = false;
    let lost_report = {
        let mut state = player.borrow_mut();
        state.pending_reconnect = None;
        state.reconnect_rollover = false;
        if state.vlc_active {
            stop_vlc = true;
            state.vlc_active = false;
        }
        if let Some(win) = &state.window {
            win.media.stop();
        }
        state.session.lost_report()
    };
    if stop_vlc {
        queue_vlc_stop(player);
    }
    if let Some(report) = lost_report {
        // N23: Python's give-up — "Stream lost" on the status
        // line at priority 1, and the long form in a queued
        // Stream Lost warning box (it must not open inside
        // another modal, N18).
        update_player_status(player, persist, status, Some("Stream lost"), 1);
        let frame = player.borrow().window.as_ref().map(|win| win.frame);
        if let Some(frame) = frame {
            show_or_queue_notification(
                frame,
                &report,
                &tr("Stream Lost"),
                MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
                None,
            );
        }
    }
}

/// (Re)load a request into the engine that owns the current
/// session. False when no engine accepted the load. A retry
/// load carries the cache plan's retry value; a rollover load
/// the quick-start value (P01/P04).
fn engine_load(player: &Rc<RefCell<PlayerState>>, request: &PlaybackRequest) -> bool {
    if player.borrow().engine == ActiveEngine::Vlc {
        let options = vlc_media_options(&player.borrow(), request.live, true, request.resume_ms);
        // Queued on the ops worker (P18); a refused load comes
        // back through deliver_vlc_load as a session error.
        let cache_ms = options.cache_ms;
        let queued = queue_vlc_load(player, request, options, true);
        if queued {
            let mut state = player.borrow_mut();
            state.vlc_active = true;
            state.vlc_tracks_ready = false;
            state.track_attempts = 0;
            state.play_started_ms = now_ms();
            state.watchdog.reset();
            // N23: the status line's buffer figure is the
            // cache this load actually carries.
            state.status_buffer_seconds = f64::from(cache_ms) / 1000.0;
        }
        queued
    } else {
        let mut state = player.borrow_mut();
        state.status_buffer_seconds = f64::from(state.cache_plan.cache_ms(
            request.live,
            true,
            state.reconnect_rollover,
        )) / 1000.0;
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
    /// The controls panel (everything but the video surface):
    /// hidden in fullscreen (P19).
    controls_panel: Panel,
    fullscreen_button: Button,
}

/// Handles the player window needs from the main wiring (record and
/// cast), installed when the browser wiring is built.
#[derive(Clone)]
struct PlayerShared {
    buffers: Rc<RefCell<buffer::BufferRegistry>>,
    recordings: Rc<RefCell<RecordingEngine>>,
    pending_shares: Rc<RefCell<std::collections::HashSet<String>>>,
    relay_worker: Rc<RelayWorker>,
    cast_ui: Rc<RefCell<Option<CastUi>>>,
    proxy: Rc<RefCell<Option<iptv_core::proxy::ProxyServer>>>,
    cast_worker: Rc<CastWorker>,
    cast_pending: Rc<std::cell::Cell<bool>>,
    timer: Rc<Timer<Frame>>,
    /// Guide window + language, so F1 in the player reaches its
    /// own topic without the full wiring (S10).
    guide_ui: Rc<RefCell<Option<GuideUi>>>,
    i18n: Rc<RefCell<I18n>>,
    /// N16: media-type classification, shared with the main
    /// window's wiring so the player's record action classifies
    /// and preempts probes the same way.
    media_probe: MediaProbe,
    /// N25: What Is Playing routed back to the main wiring
    /// (the player dispatch has no guide access of its own).
    /// Carries the playing channel when there is one, else
    /// None for the recent/selected fallbacks.
    whats_playing: Option<WhatsPlayingHook>,
}

/// What Is Playing on the main wiring, callable from the
/// player window (audit N25).
type WhatsPlayingHook = Rc<dyn Fn(Option<Channel>, Status)>;

/// Build the built-in player window once: video surface, seek and
/// volume sliders, time and subtitle labels, the audio track
#[allow(clippy::too_many_lines)]
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
    // The video surface gets its own panel so fullscreen can
    // hide the controls panel while the video remains (P19).
    let video_panel = Panel::builder(&panel).build();
    let controls_panel = Panel::builder(&panel).build();
    let media = MediaCtrl::builder(&video_panel).build();
    media.set_accessibility_label("Video");
    let seek = Slider::builder(&controls_panel).build();
    seek.set_range(0, 1000);
    seek.set_accessibility_label("Seek");
    let time_label = StaticText::builder(&controls_panel)
        .with_label("Live")
        .build();
    time_label.set_accessibility_label("Playback position");
    let subtitle_label = StaticText::builder(&controls_panel).with_label("").build();
    subtitle_label.set_accessibility_label("Current subtitle");
    let volume_caption = StaticText::builder(&controls_panel)
        .with_label("&Volume")
        .build();
    let _ = volume_caption;
    let volume = Slider::builder(&controls_panel).build();
    volume.set_range(0, 100);
    volume.set_value(100);
    volume.set_accessibility_label("Volume");
    let tracks_caption = StaticText::builder(&controls_panel)
        .with_label("Choose &Audio Track")
        .build();
    let _ = tracks_caption;
    let tracks = Choice::builder(&controls_panel).build();
    tracks.append("Default");
    tracks.set_selection(0);
    tracks.set_accessibility_label("Choose Audio Track");
    let pause = Button::builder(&controls_panel)
        .with_label("&Pause or resume")
        .build();
    pause.set_accessibility_label("Pause or resume");
    let stop = Button::builder(&controls_panel).with_label("&Stop").build();
    stop.set_accessibility_label("Stop");
    let record = Button::builder(&controls_panel)
        .with_label("&Record")
        .build();
    record.set_accessibility_label("Record");
    let cast = Button::builder(&controls_panel).with_label("&Cast").build();
    cast.set_accessibility_label("Cast");
    let fullscreen = Button::builder(&controls_panel)
        .with_label("&Full Screen")
        .build();
    // Python constructs the button named "Toggle Full
    // Screen"; the name follows the label after a toggle.
    fullscreen.set_accessibility_label("Toggle Full Screen");
    let subs = Button::builder(&controls_panel)
        .with_label("Load &subtitles…")
        .build();
    subs.set_accessibility_label("Load subtitles");
    let video_sizer = BoxSizer::builder(Orientation::Vertical).build();
    video_sizer.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    video_panel.set_sizer(video_sizer, true);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
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
    controls_panel.set_sizer(sizer, true);
    let root = BoxSizer::builder(Orientation::Vertical).build();
    root.add(&video_panel, 1, SizerFlag::Expand, 0);
    root.add(&controls_panel, 0, SizerFlag::Expand, 0);
    panel.set_sizer(root, true);

    let win = PlayerWindow {
        frame,
        media,
        seek,
        volume,
        time_label,
        subtitle_label,
        tracks,
        controls_panel,
        fullscreen_button: fullscreen,
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
    // F1 anywhere in the player opens the guide at the player's
    // topic (S10) — previously only the main window had F1 at all.
    bind_player_f1(&win.frame, player, status, "built-in-player");
    bind_player_f1(&win.media, player, status, "built-in-player");
    bind_player_f1(&win.seek, player, status, "built-in-player");
    bind_player_f1(&win.volume, player, status, "built-in-player");
    bind_player_f1(&win.tracks, player, status, "built-in-player");
    player.borrow_mut().window = Some(win);
}

/// Open the guide at a topic from the player window, which holds
/// only its shared slice of the main wiring.
fn open_guide_from_player(shared: &PlayerShared, status: Status, topic: &str) {
    let ui = ensure_guide_ui(&shared.guide_ui);
    let language = shared.i18n.borrow().resolved_language();
    let (guide, section) = iptv_core::guide::open_topic(Some(&language), Some(topic), None);
    show_guide(&ui, guide);
    if let Some(section) = section {
        move_caret(&ui, &section);
    }
    ui.frame.show(true);
    ui.frame.raise();
    status.set_label("User Guide opened.");
}

/// F1 help inside the player window (S10).
fn bind_player_f1<T>(
    control: &T,
    player: &Rc<RefCell<PlayerState>>,
    status: Status,
    topic: &'static str,
) where
    T: wxdragon::event::WxEvtHandler,
{
    let player = Rc::clone(player);
    control.bind_internal(EventType::KEY_DOWN, move |event| {
        if event.get_key_code() == Some(340) {
            let state = player.borrow();
            if let Some(shared) = &state.shared {
                open_guide_from_player(shared, status, topic);
            }
        }
        event.skip(true);
    });
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
const PM_SUB_BASE: i32 = 5400;

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
    menu_add(
        menu,
        PM_STOP,
        &menu_label(keys, "&Stop", "stop"),
        "",
        ItemKind::Normal,
    );
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
    menu_add(
        menu,
        PM_RECORD,
        &menu_label(keys, "&Record", "record"),
        "",
        ItemKind::Normal,
    );
}

/// Build the player window's Playback menu (Python's order and
/// items). Dynamic pieces: the Audio Track submenu fills when
/// libVLC publishes tracks (`apply_vlc_tracks`), and the Speak
#[allow(clippy::too_many_lines)]
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
    menu.append_submenu(
        audio_menu,
        &menu_label(&keys, "Audio Trac&k", "audio_track"),
        "",
    );
    // The Subtitles submenu (P08): Off plus the embedded SPU
    // tracks as radios, rebuilt by refresh_subtitle_menu
    // whenever libVLC publishes a different list.
    let sub_menu = Menu::builder().build();
    menu_add(
        &sub_menu,
        PM_SUB_BASE,
        "No subtitle tracks available",
        "",
        ItemKind::Normal,
    );
    sub_menu.enable_item(PM_SUB_BASE, false);
    menu.append_submenu(sub_menu, &menu_label(&keys, "Sub&titles", "subtitles"), "");
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
        &menu_label(&keys, "Spea&k Subtitles", "speak_subtitles"),
        "",
        ItemKind::Check,
    );
    let speak = persist.config.borrow().bool_or("speak_subtitles", false);
    menu.check_item(PM_SPEAK_SUBS, speak);
    menu_add(
        &menu,
        PM_READ_SUB,
        &menu_label(&keys, "Read &Current Subtitle", "read_subtitle"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_PREV_SUB,
        &menu_label(&keys, "&Previous Subtitle", "previous_subtitle"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_NEXT_SUB,
        &menu_label(&keys, "&Next Subtitle", "next_subtitle"),
        "",
        ItemKind::Normal,
    );
    // Python shows a hard-coded D accelerator on this item.
    menu_add(
        &menu,
        PM_AUDIO_DEVICE,
        "Audio Output &Device...\tD",
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(
        &menu,
        PM_CAST,
        &menu_label(&keys, "&Cast...", "cast"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_FULLSCREEN,
        &menu_label(&keys, "Toggle &Full Screen", "fullscreen"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_WHATS_PLAYING,
        &menu_label(&keys, "What Is &Playing", "what_is_playing"),
        "",
        ItemKind::Normal,
    );
    menu.append_separator();
    menu_add(
        &menu,
        PM_HIDE,
        &menu_label(&keys, "&Hide Window", "hide"),
        "",
        ItemKind::Normal,
    );
    menu_add(
        &menu,
        PM_EXIT,
        &menu_label(&keys, "E&xit Player", "exit"),
        "",
        ItemKind::Normal,
    );
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
    if (PM_SUB_BASE..PM_SUB_BASE + 41).contains(&id) {
        let index = usize::try_from(id - PM_SUB_BASE).unwrap_or(0);
        let spu = player.borrow().spu_ids.get(index).copied();
        if let Some(spu) = spu {
            select_subtitle_track(player, persist, status, spu);
        }
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
                    &win.frame,
                    &shared.recordings,
                    &shared.buffers,
                    persist,
                    &shared.media_probe,
                    channel,
                    status,
                    Some(player),
                    &shared.share_ctx(),
                );
                assert!(shared.timer.start(50, false));
            }
        }
        PM_AUDIO_DEVICE => prompt_audio_device(player, win.frame, persist, status),
        PM_LOAD_SUBS => prompt_subtitle_file(player, win.frame, persist, status),
        PM_SPEAK_SUBS => {
            let next = !persist.config.borrow().bool_or("speak_subtitles", false);
            persist.set_and_save("speak_subtitles", serde_json::json!(next));
            if !next {
                player.borrow_mut().app_voice.stop();
            }
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
        PM_READ_SUB => announce_cue(player, persist, "read_subtitle", status),
        PM_PREV_SUB => announce_cue(player, persist, "previous_subtitle", status),
        PM_NEXT_SUB => announce_cue(player, persist, "next_subtitle", status),
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
        PM_FULLSCREEN => set_fullscreen(player, !win.frame.is_full_screen()),
        PM_WHATS_PLAYING => {
            let state = player.borrow();
            match &state.current_channel {
                Some(channel) => {
                    status.set_label(&trf("Now playing: {name}", &[("name", &channel.name)]));
                }
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

/// The Subtitles submenu (position 9, after Audio Track).
fn subtitle_menu(player: &Rc<RefCell<PlayerState>>) -> Option<Menu> {
    playback_menu(player)?
        .find_item_by_position(9)?
        .get_sub_menu()
}

/// Republish the Subtitles submenu from libVLC (P08): radio
/// items Off + the embedded tracks in description order, the
/// current selection checked. Runs when the audio refresh sees
/// a changed track list and when a selection is made.
fn refresh_subtitle_menu(player: &Rc<RefCell<PlayerState>>) {
    let (tracks, current) = {
        let state = player.borrow();
        if !state.vlc_active {
            return;
        }
        match &state.vlc {
            Some(engine) => (engine.subtitle_tracks(), engine.subtitle_track()),
            None => return,
        }
    };
    let choices: Vec<i32> = std::iter::once(-1)
        .chain(tracks.iter().map(|(id, _)| *id).filter(|id| *id >= 0))
        .collect();
    let selected = if choices.contains(&current) {
        current
    } else {
        player.borrow().selected_spu
    };
    let Some(menu) = subtitle_menu(player) else {
        return;
    };
    let mut state = player.borrow_mut();
    for old in state.spu_menu_ids.drain(..) {
        menu_delete_if_present(&menu, old);
    }
    // "No subtitle tracks available" holds the same id as the first
    // radio. Left in, checking that id hits the plain item: a wx
    // assert dialog whose nested loop trapped every player tick.
    // The drain above already removed that id whenever the radios
    // were in place, so this delete must tolerate its absence.
    menu_delete_if_present(&menu, PM_SUB_BASE);
    for (index, spu) in choices.iter().enumerate() {
        let id = PM_SUB_BASE + i32::try_from(index).unwrap_or(0);
        let label = if *spu == -1 {
            "Off".to_string()
        } else {
            tracks
                .iter()
                .find(|(track_id, _)| track_id == spu)
                .map_or_else(
                    || format!("Track {spu}"),
                    |(_, name)| {
                        if name.is_empty() {
                            format!("Track {spu}")
                        } else {
                            name.clone()
                        }
                    },
                )
        };
        menu_add(&menu, id, &label, "", ItemKind::Radio);
        state.spu_menu_ids.push(id);
        if *spu == selected {
            menu.check_item(id, true);
        }
    }
    state.spu_ids = choices;
    if current >= -1 {
        state.selected_spu = current;
    }
}

/// Select an embedded subtitle track (or Off) in VLC (P08).
/// Selecting an embedded track discards the sidecar file's
/// cues, as Python does; selecting Off keeps already-shown
/// review text but silences display and speech.
fn select_subtitle_track(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    spu: i32,
) {
    let name = {
        let state = player.borrow();
        state
            .vlc
            .as_ref()
            .map(iptv_vlc::VlcEngine::subtitle_tracks)
            .unwrap_or_default()
            .iter()
            .find(|(id, _)| *id == spu)
            .map(|(_, name)| {
                if name.is_empty() {
                    format!("Track {spu}")
                } else {
                    name.clone()
                }
            })
    };
    let mut state = player.borrow_mut();
    state.selected_spu = spu;
    if state.vlc_active {
        if let Some(engine) = &mut state.vlc {
            engine.set_subtitle_track(spu);
        }
    }
    if spu >= 0 && state.file_subtitle_track != Some(spu) {
        state.subtitle_cues.clear();
        state.last_spoken_cue = None;
        state.last_label_cue = None;
        state.file_subtitle_track = None;
        state.subtitle_generation += 1;
        state.pending_subtitles = None;
        // The file's cues are discarded: review starts
        // empty for the new source (P11).
        state.review.reset();
        state.active_cue_index = None;
        state.last_shown_cue = None;
        state.shown_cues.clear();
        if let Some(win) = &state.window {
            win.subtitle_label.set_label("");
        }
    }
    drop(state);
    refresh_subtitle_menu(player);
    player_status(
        status,
        persist,
        &format!(
            "Subtitles: {}",
            if spu == -1 {
                "Off".to_string()
            } else {
                name.unwrap_or_else(|| format!("Track {spu}"))
            }
        ),
        2,
    );
}

/// Cycle subtitles (the player `S` key, P13/P08): Off, then
/// the embedded tracks in order, then back to Off.
fn cycle_subtitle_track(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let (choices, current) = {
        let state = player.borrow();
        if !state.vlc_active {
            status.set_label("Subtitle tracks are not available with the basic player.");
            return;
        }
        match &state.vlc {
            Some(engine) => {
                let mut choices = vec![-1];
                choices.extend(
                    engine
                        .subtitle_tracks()
                        .iter()
                        .map(|(id, _)| *id)
                        .filter(|id| *id >= 0),
                );
                let live = engine.subtitle_track();
                (choices, if live >= -1 { live } else { state.selected_spu })
            }
            None => return,
        }
    };
    let next = choices
        .iter()
        .position(|id| *id == current)
        .map_or(choices[0], |index| choices[(index + 1) % choices.len()]);
    select_subtitle_track(player, persist, status, next);
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
    let (has_vlc, devices) = {
        let state = player.borrow();
        (
            state.vlc.is_some(),
            state
                .vlc
                .as_ref()
                .map(iptv_vlc::VlcEngine::audio_output_devices)
                .unwrap_or_default(),
        )
    };
    if !has_vlc {
        // The basic player cannot choose an output device; say
        // so plainly instead of claiming none exist (issue #129).
        let _ = MessageDialog::builder(
            &frame,
            "Audio output devices can only be chosen with the full player. The basic player is in use because libVLC is not available.",
            "Audio Output Device",
        )
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal_tracked();
        return;
    }
    if devices.is_empty() {
        let _ = MessageDialog::builder(
            &frame,
            "No audio output devices were found.",
            "Audio Output Device",
        )
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal_tracked();
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
/// Audio Track menu share this). A manual choice is persisted
/// (P06): the channel's remembered name and slot, plus the
/// global last track — automatic preference application never
/// writes that memory.
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
    let mut refused = false;
    if state.vlc_active {
        if let Some(id) = state.vlc_track_ids.get(index as usize).copied() {
            if let Some(engine) = &mut state.vlc {
                refused = !engine.set_audio_track(id);
            }
        }
    }
    if refused {
        // Python's failed set reports and keeps the old
        // track (audit N23).
        drop(state);
        update_player_status(player, persist, status, Some("Audio track unavailable"), 1);
        return;
    }
    state.audio_track_label.clone_from(&label);
    let key = state.audio_key.clone();
    drop(state);
    if !key.is_empty() && !label.is_empty() {
        persist.save_audio_choice(&key, &label, i64::from(index));
    }
    // N23: Python names the track on the status line at
    // priority 2, which speaks the line (the old "Audio track
    // preference:" wording was a Rust-only invention).
    update_player_status(player, persist, status, Some(&format!("Audio: {label}")), 2);
}

/// Cycle to the next audio track (the player `A` key, P13):
/// wraps through the published choice entries.
fn cycle_audio_track(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let (count, current, current_name) = {
        let state = player.borrow();
        let Some(win) = &state.window else {
            return;
        };
        (
            state.vlc_track_ids.len(),
            win.tracks.get_selection().unwrap_or(0),
            win.tracks.get_string_selection().unwrap_or_default(),
        )
    };
    if count == 0 {
        // Python names these on the status line at the
        // default priority (audit N23).
        update_player_status(
            player,
            persist,
            status,
            Some("No audio tracks available"),
            3,
        );
        return;
    }
    if count == 1 {
        update_player_status(
            player,
            persist,
            status,
            Some(&format!("Audio: {current_name}")),
            3,
        );
        return;
    }
    let next = (current + 1) % u32::try_from(count).unwrap_or(1);
    if let Some(win) = player.borrow().window.clone() {
        win.tracks.set_selection(next);
    }
    choose_audio_track(player, persist, next, status);
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
            .map(|deadline| ((deadline - now_ms()).max(0) / 1000 + 59) / 60)
    };
    let dialog = TextEntryDialog::builder(
        &frame,
        "Stop built-in playback after 1-1440 minutes; enter 0 to cancel the timer.",
        "Sleep Timer",
    )
    .with_default_value(&current.map(|m| m.to_string()).unwrap_or_default())
    .build();
    if dialog.show_modal_tracked() != ID_OK {
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
    if fade_dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let fade_text = fade_dialog.get_value().unwrap_or_default();
    let Ok(fade) = fade_text.trim().parse::<i64>() else {
        status.set_label("Enter a whole number of fade-out seconds.");
        return;
    };
    if !(0..=fade_cap).contains(&fade) {
        status.set_label(&trf(
            "Enter fade-out seconds between 0 and {fade_cap}.",
            &[("fade_cap", &fade_cap.to_string())],
        ));
        return;
    }
    persist.set_and_save("sleep_fade_seconds", serde_json::json!(fade));
    let mut state = player.borrow_mut();
    state.sleep_deadline = Some(now_ms() + minutes * 60_000);
    state.sleep_fade_secs = fade;
    state.sleep_fading = false;
    status.set_label(&trf(
        "Sleep timer set: playback stops in {minutes} minutes.",
        &[("minutes", &minutes.to_string())],
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
    let now = now_ms();
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
    if fade > 0 && now >= deadline - fade * 1000 {
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
        let remaining_secs = (deadline - now) / 1000;
        let level = (i64::from(start) * remaining_secs / fade).clamp(0, i64::from(start));
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
    drain_fullscreen_layout(player);
    deliver_vlc_load(player, persist, status);
    deliver_buffer_pending(player, persist, status);
    poll_buffer(player, persist, status);
    checkpoint_resume(player, persist, status, false);
    speak_subtitle_cue(player, persist, status);
    drive_speech_watchdog(player, persist, status);
    poll_vlc_state(player, persist, status);
    poll_fallback_state(player, persist, status);
    drive_session(player, persist, status);
    drive_sleep_timer(player, persist, status);
    update_player_chrome(player);
}

/// Timer, buttons, media events, keys and close behavior for the
#[allow(clippy::too_many_lines)]
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
    // Subtitle sampling runs at Python's 150 ms (P10), on its
    // own timer so short cues are never skipped between the
    // one-second ticks.
    let sub_timer = Rc::new(Timer::new(&frame));
    let sub_player = Rc::clone(player);
    let sub_persist = persist.clone();
    sub_timer.on_tick(move |_| {
        subtitle_tick(&sub_player, &sub_persist, status);
    });
    let _ = sub_timer.start(150, false);
    // Dropping a wxDragon timer destroys it: these must outlive this
    // function, or the player never ticks (validation 0071).
    player.borrow_mut().player_timers = vec![timer, sub_timer];

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
        let (Some(shared), Some(channel), Some(win)) =
            (&state.shared, &state.current_channel, &state.window)
        else {
            return;
        };
        toggle_recording(
            &win.frame,
            &shared.recordings,
            &shared.buffers,
            &record_persist,
            &shared.media_probe,
            channel,
            status,
            Some(&record_player),
            &shared.share_ctx(),
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
    let fullscreen_player = Rc::clone(player);
    buttons.fullscreen.on_click(move |_| {
        set_fullscreen(&fullscreen_player, !frame.is_full_screen());
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
            if let Some(action) = rollover_action(&mut state) {
                action
            } else {
                let near_end = state.window.as_ref().is_some_and(|win| {
                    win.media.length() > 0 && win.media.tell() >= win.media.length() - 1500
                });
                let event = if near_end {
                    EngineEvent::Ended
                } else {
                    EngineEvent::Stalled
                };
                state.session.on_event(event, now_ms())
            }
        };
        store_session_action(&stopped_player, &stopped_persist, status, action);
        let state = stopped_player.borrow();
        player_status(status, &stopped_persist, &state.session.announcement(), 3);
    });
    let close_player = Rc::clone(player);
    let close_persist = persist.clone();
    frame.on_close(move |event| {
        checkpoint_resume(&close_player, &close_persist, status, true);
        // Closing the player stops the actual engine, not just
        // the session (P02): VLC stops (sound and upstream
        // connection end), the live-buffer lease is released
        // (buffer ingest ends), and no reconnect stays pending.
        if let Some(buffers) = buffers_of(&close_player) {
            buffers.borrow_mut().release_current();
            buffers.borrow_mut().pending = None;
        }
        cancel_sleep_timer(&close_player);
        let mut state = close_player.borrow_mut();
        state.pending_reconnect = None;
        state.resume_guard = None;
        state.app_voice.stop();
        // Quiesce clears the subtitle cues and review state
        // (P09/P11), as in Python's close path.
        state.subtitle_cues.clear();
        state.last_spoken_cue = None;
        state.last_label_cue = None;
        state.subtitle_generation += 1;
        state.pending_subtitles = None;
        state.review.reset();
        state.active_cue_index = None;
        state.last_shown_cue = None;
        state.shown_cues.clear();
        state.pending_play = None;
        state.session.stop();
        let stop_vlc = state.vlc_active;
        state.vlc_active = false;
        if let Some(win) = &state.window {
            win.media.stop();
            win.subtitle_label.set_label("");
            win.frame.show(false);
        }
        drop(state);
        if stop_vlc {
            queue_vlc_stop(&close_player);
        }
        event.skip(false);
    });
}

/// Seek/volume slider, track choice and player shortcut bindings.
/// One mouse-wheel notch on the player (audit N23; Python's
/// `_on_mouse_wheel`): 2% per notch, status line refreshed
/// silently like a key-driven volume change.
fn wheel_volume(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    rotation: i32,
) {
    if rotation == 0 {
        return;
    }
    let delta = if rotation > 0 { 2 } else { -2 };
    let value = {
        let state = player.borrow();
        state
            .window
            .as_ref()
            .map_or(0, |win| win.volume.get_value())
            + delta
    }
    .clamp(0, 100);
    player_set_volume(player, value);
    update_player_status(player, persist, status, None, 3);
}

fn bind_player_controls(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    win: &PlayerWindow,
) {
    // N23: the wheel drives volume over the video surface
    // and the controls panel (Python binds both panels).
    let wheel_player = Rc::clone(player);
    let wheel_persist = persist.clone();
    win.media
        .bind_internal(EventType::MOUSEWHEEL, move |event| {
            wheel_volume(
                &wheel_player,
                &wheel_persist,
                status,
                event.get_wheel_rotation(),
            );
        });
    let wheel_player = Rc::clone(player);
    let wheel_persist = persist.clone();
    win.controls_panel
        .bind_internal(EventType::MOUSEWHEEL, move |event| {
            wheel_volume(
                &wheel_player,
                &wheel_persist,
                status,
                event.get_wheel_rotation(),
            );
        });
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
    state.app_voice.stop();
    state.pending_play = None;
    state.session.stop();
    let stop_vlc = state.vlc_active;
    state.vlc_active = false;
    if let Some(win) = &state.window {
        win.media.stop();
    }
    drop(state);
    if stop_vlc {
        queue_vlc_stop(player);
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
    // N20: the cast session state exists now, so the engage
    // context consults it (was hardcoded off, validation 0045).
    let casting = player
        .borrow()
        .shared
        .as_ref()
        .is_some_and(|shared| cast_is_active(&shared.cast_ui));
    let wants = iptv_core::timeshift::wants_buffer(
        &settings,
        iptv_core::timeshift::EngageContext {
            builtin_player: true,
            casting,
            live_stream: request.live,
            channel_is_movie: false,
        },
    );
    if casting && buffers.borrow().current.is_some() {
        // Python's refusal (client_timeshift.py): a buffered
        // session is feeding the player while a cast is live —
        // the launch is taken over and nothing plays locally.
        if let Some(frame) = player.borrow().window.as_ref().map(|win| win.frame) {
            let dialog = MessageDialog::builder(
                &frame,
                &tr("Stop local buffered playback before switching to a cast device."),
                &tr("Local Live Buffer"),
            )
            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
            .build();
            dialog.show_modal_tracked();
        }
        return true;
    }
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
                .show_modal_tracked();
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
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(minutes) = text.trim().parse::<i64>() else {
        status.set_label(&trf(
            "\"{text}\" is not a whole number of minutes.",
            &[("text", &text)],
        ));
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
            .show_modal_tracked();
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
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        let text = dialog.get_value().unwrap_or_default();
        let Ok(mib) = text.trim().parse::<i64>() else {
            status.set_label(&trf(
                "\"{text}\" is not a whole number of MiB.",
                &[("text", &text)],
            ));
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
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let text = dialog.get_value().unwrap_or_default();
    let Ok(seconds) = text.trim().parse::<i64>() else {
        status.set_label(&trf(
            "\"{text}\" is not a whole number of seconds.",
            &[("text", &text)],
        ));
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
    .show_modal_tracked();
}

/// Release a recording's buffer lease, if it held one.
fn release_recording_lease(buffers: &Rc<RefCell<buffer::BufferRegistry>>, key: &str) {
    if let Some((session, lease)) = buffers.borrow_mut().recording_leases.remove(key) {
        session.release(lease);
    }
}

/// Stop playback and hide the player window (Stop button, the stop
/// and exit shortcuts). A manual Stop also cancels the sleep
/// timer and restores the pre-fade volume (P14), so a later
/// stream never inherits the old deadline or a faded level.
fn player_stop(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    cancel_sleep_timer(player);
    if let Some(buffers) = buffers_of(player) {
        buffers.borrow_mut().release_current();
    }
    if let Some(buffers) = buffers_of(player) {
        buffers.borrow_mut().pending = None;
    }
    checkpoint_resume(player, persist, status, true);
    let mut state = player.borrow_mut();
    state.pending_reconnect = None;
    state.reconnect_rollover = false;
    state.resume_guard = None;
    state.app_voice.stop();
    // Stop clears the subtitle cues and review state and
    // invalidates any in-flight parse (P09/P11), as Python's
    // stop does.
    state.subtitle_cues.clear();
    state.last_spoken_cue = None;
    state.last_label_cue = None;
    state.subtitle_generation += 1;
    state.pending_subtitles = None;
    state.review.reset();
    state.active_cue_index = None;
    state.last_shown_cue = None;
    state.shown_cues.clear();
    state.pending_play = None;
    state.session.stop();
    let stop_vlc = state.vlc_active;
    state.vlc_active = false;
    if let Some(win) = &state.window {
        win.media.stop();
        win.subtitle_label.set_label("");
        win.frame.show(false);
    }
    drop(state);
    if stop_vlc {
        queue_vlc_stop(player);
    }
    player_status(status, persist, "Stopped.", 2);
}

/// Seek the active engine (libVLC when it drives playback, else
/// the `MediaCtrl`).
fn player_seek_to(player: &Rc<RefCell<PlayerState>>, target_ms: i64) {
    let mut state = player.borrow_mut();
    // A seek discards the cue being spoken: stale words from the
    // old position never finish (issue #131).
    state.app_voice.stop();
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

/// The P04 interception: when the current request is an
/// expected live TS stream that has played, an ending goes to
/// the session's rollover path instead of ordinary recovery.
fn rollover_action(state: &mut PlayerState) -> Option<EngineAction> {
    let request = state.session.current()?;
    if !iptv_core::playback::looks_like_live_ts(&request.url, request.live) {
        return None;
    }
    state.session.note_rollover(now_ms())
}

/// Poll the libVLC engine on the 1 s tick: resume seek, one-time
/// track setup, and end/error transitions into the session
/// machine (with its reconnect handling).
fn poll_vlc_state(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let mut setup_tracks = false;
    let mut confirm_history = false;
    {
        let mut state = player.borrow_mut();
        if !state.vlc_active {
            return;
        }
        // The engine is a shared handle (P18): polling works
        // on a clone so the state stays mutable for the
        // session machine.
        let Some(engine) = state.vlc.clone() else {
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
                    confirm_history = true;
                }
                if let Some(ms) = state.pending_resume_ms.take() {
                    engine.set_time(ms);
                }
                // The freeze watchdog (P03): Playing on the
                // surface while counters/position freeze is a
                // stall and earns an ordinary recovery.
                let playing_now = matches!(state.session.state(), SessionState::Playing);
                let counters = engine.stats();
                let position = engine.time();
                let play_started_ms = state.play_started_ms;
                let base_ms = i64::from(state.cache_plan.base_ms());
                let stalled = state.watchdog.sample(
                    counters,
                    Some(position),
                    playing_now,
                    now_ms(),
                    play_started_ms,
                    base_ms,
                );
                if stalled {
                    let action = state.session.on_event(EngineEvent::Stalled, now_ms());
                    state.vlc_active = false;
                    drop(state);
                    store_session_action(player, persist, status, action);
                    let state = player.borrow();
                    player_status(status, persist, &state.session.announcement(), 3);
                    return;
                }
                // Track publication is retried (P07): the
                // refresh runs every poll until the preference
                // settles or the attempt budget runs out, and
                // republishes the controls whenever the list
                // changes.
                setup_tracks = true;
            }
            iptv_vlc::VlcState::Buffering => {
                state.session.note_buffering(true, now_ms());
            }
            iptv_vlc::VlcState::Ended | iptv_vlc::VlcState::Error => {
                let ended = engine.state() == iptv_vlc::VlcState::Ended;
                // An expected TS segment ending refreshes on
                // the rollover path (P04), never the failure
                // budget.
                if let Some(action) = rollover_action(&mut state) {
                    state.vlc_active = false;
                    drop(state);
                    store_session_action(player, persist, status, action);
                    let state = player.borrow();
                    player_status(status, persist, &state.session.announcement(), 3);
                    return;
                }
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
    if confirm_history {
        record_history_once(player, persist);
    }
    if setup_tracks {
        apply_vlc_tracks(player, persist);
    }
}

/// Poll the basic player (`MediaCtrl`) on the 1 s tick, mirroring
/// `poll_vlc_state` for the fallback engine (issue #129): the
/// session hears Playing only on the control's own confirmation,
/// and a stop after confirmed playback becomes Ended (near the
/// end) or Stalled, so the session's bounded recovery and its
/// Stream lost report work the same under both engines. A stop
/// before any confirmation is left to the session's startup
/// deadline — that deadline is the failed-start path, and
/// `MediaCtrl` also reports Stopped while a load is still in
/// flight, so treating it as failure here would burn the
/// reconnect budget on healthy loads.
fn poll_fallback_state(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    enum Act {
        None,
        Started,
        Settled,
    }
    let act = {
        let mut state = player.borrow_mut();
        if state.vlc_active || state.session.current().is_none() {
            return;
        }
        let Some(win) = state.window.clone() else {
            return;
        };
        match win.media.get_state() {
            MediaState::Playing => {
                state.session.note_buffering(false, now_ms());
                if matches!(
                    state.session.state(),
                    SessionState::Playing | SessionState::Paused
                ) {
                    Act::None
                } else {
                    state.session.on_event(EngineEvent::Started, now_ms());
                    if let Some(ms) = state.pending_resume_ms.take() {
                        win.media.seek(ms, SeekMode::FromStart);
                    }
                    Act::Started
                }
            }
            MediaState::Stopped => {
                // An expected TS segment ending refreshes on
                // the rollover path (P04), never the failure
                // budget.
                if let Some(action) = rollover_action(&mut state) {
                    drop(state);
                    store_session_action(player, persist, status, action);
                    return;
                }
                let confirmed = matches!(
                    state.session.state(),
                    SessionState::Playing | SessionState::Paused
                );
                let (position, length) = (win.media.tell(), win.media.length());
                let near_end = length > 0 && position >= length - 1500;
                // The withholding decision itself lives in the
                // core (fallback_stop) so it carries a regression
                // test; this arm only executes its verdict.
                let event = match iptv_core::playback::fallback_stop(confirmed, near_end) {
                    iptv_core::playback::FallbackStop::Withhold => return,
                    iptv_core::playback::FallbackStop::Ended => EngineEvent::Ended,
                    iptv_core::playback::FallbackStop::Stalled => EngineEvent::Stalled,
                };
                let action = state.session.on_event(event, now_ms());
                drop(state);
                store_session_action(player, persist, status, action);
                Act::Settled
            }
            _ => Act::None,
        }
    };
    if matches!(act, Act::Started) {
        record_history_once(player, persist);
    }
    if matches!(act, Act::Started | Act::Settled) {
        let state = player.borrow();
        player_status(status, persist, &state.session.announcement(), 3);
    }
}

/// Refresh the player window's audio tracks from libVLC
/// (P06/P07): republish the choice and menu whenever the
/// published list changes, and keep applying the preference —
/// this session's manual pick first, then the ordered keyword
/// assembly (channel memory, audio-description set or the
/// global last pick, saved keywords) with the remembered slot
/// as fallback — until it settles or the ~10 s attempt budget
/// runs out. Automatic application never writes the persisted
#[allow(clippy::too_many_lines)]
/// memory; only manual choices do.
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
        // Nothing published yet: count the attempt (two 500 ms
        // units per 1 s poll) and stop applying after ~10 s;
        // publication still lands whenever the list appears.
        let mut state = player.borrow_mut();
        if !state.vlc_tracks_ready {
            state.track_attempts += 2;
            if state.track_attempts >= 20 {
                state.vlc_tracks_ready = true;
            }
        }
        return;
    }
    let ids: Vec<i32> = tracks.iter().map(|(id, _)| *id).collect();
    let changed = ids != player.borrow().vlc_track_ids;
    let win = {
        let mut state = player.borrow_mut();
        if changed {
            state.vlc_track_ids = ids;
        }
        state.window.clone()
    };
    let Some(win) = win else {
        return;
    };
    if changed {
        win.tracks.clear();
        for (_, name) in &tracks {
            win.tracks.append(name);
        }
    }
    if !player.borrow().vlc_tracks_ready {
        let (keywords, prefer_ad, desired, supply_index) = {
            let config = persist.config.borrow();
            let state = player.borrow();
            (
                iptv_core::audiotracks::ordered_preference_keywords(
                    state.supply_audio_name.as_deref(),
                    config.bool_or("prefer_audio_description", false),
                    Some(config.string_or("last_audio_track", "")).as_deref(),
                    &config.string_list("preferred_audio_tracks"),
                ),
                config.bool_or("prefer_audio_description", false),
                state.desired_track.clone(),
                state.supply_audio_index,
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
                iptv_core::audiotracks::select_preferred_audio_track(
                    &pairs,
                    &keywords,
                    supply_index,
                    prefer_ad,
                )
                .map(|id| i32::try_from(id).unwrap_or(-1))
            });
        let mut state = player.borrow_mut();
        state.track_attempts += 2;
        if let Some(id) = chosen {
            if let Some(engine) = &mut state.vlc {
                engine.set_audio_track(id);
            }
            // N23: the applied track is the status line's
            // Audio segment (Python sets its label when the
            // wanted track lands).
            if let Some((_, name)) = tracks.iter().find(|(track_id, _)| *track_id == id) {
                state.audio_track_label.clone_from(name);
            }
            if let Some(position) = state.vlc_track_ids.iter().position(|track| *track == id) {
                win.tracks
                    .set_selection(u32::try_from(position).unwrap_or(0));
            }
            state.vlc_tracks_ready = true;
        } else if state.track_attempts >= 20 {
            state.vlc_tracks_ready = true;
        }
        drop(state);
    }
    if changed {
        if let Some(menu) = audio_track_menu(player) {
            let mut state = player.borrow_mut();
            for old in state.track_menu_ids.drain(..) {
                menu_delete_if_present(&menu, old);
            }
            menu_delete_if_present(&menu, PM_NO_TRACKS);
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
        // The embedded subtitle list republishes alongside.
        refresh_subtitle_menu(player);
    }
}

/// Refresh the player window's time label and seek position
/// from the media position (1 s tick; the subtitle label is
/// sampled separately at 150 ms, P10).
fn update_player_chrome(player: &Rc<RefCell<PlayerState>>) {
    let state = player.borrow_mut();
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
/// Fullscreen (P19, Python `_set_fullscreen`): no-op when
/// already in the target state; the whole controls panel
/// hides while the video surface remains, the button's label
/// and accessible name follow the state, and focus moves to
/// the video surface. The layout and the focus move are NOT
/// done here: Python defers the focus with `wx.CallAfter`,
/// and doing either synchronously inside the mode switch
/// interleaves with the frame's own size handling on Windows
/// — wxMSW ends a deferred child-positioning session that was
/// never started and trips its `EndRepositioningChildren` assert
/// (validation 0095). The toggle only records the request;
/// `drain_fullscreen_layout` runs it on the next player tick,
/// once the switch has settled.
fn set_fullscreen(player: &Rc<RefCell<PlayerState>>, enable: bool) {
    let win = player.borrow().window.clone();
    let Some(win) = win else {
        return;
    };
    if win.frame.is_full_screen() == enable {
        return;
    }
    win.frame.show_full_screen(enable);
    win.controls_panel.show(!enable);
    if enable {
        win.fullscreen_button.set_label("E&xit Full Screen");
        win.fullscreen_button
            .set_accessibility_label("Exit Full Screen");
    } else {
        win.fullscreen_button.set_label("&Full Screen");
        win.fullscreen_button.set_accessibility_label("Full Screen");
    }
    player.borrow_mut().fullscreen_layout_pending = true;
}

/// The deferred half of a fullscreen toggle: relayout the
/// frame now that the controls panel's visibility matches the
/// new mode, and put focus on the video surface. Runs from the
/// player ticks; a toggle reversed before the tick still lands
/// on the same single layout + focus, matching the end state.
fn drain_fullscreen_layout(player: &Rc<RefCell<PlayerState>>) {
    let win = {
        let mut state = player.borrow_mut();
        if !state.fullscreen_layout_pending {
            return;
        }
        state.fullscreen_layout_pending = false;
        state.window.clone()
    };
    let Some(win) = win else {
        return;
    };
    win.frame.layout();
    win.media.set_focus();
}
#[allow(clippy::too_many_lines)]
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
    // Python's order (P13): a configured binding matches
    // first. Only when nothing matches do the fallbacks run —
    // Ctrl+Up/Down synthesise the volume action when its
    // binding is still the plain arrow (the event's Ctrl
    // gives the coarse 5% step in the volume arm below), and
    // Esc leaves fullscreen (P19).
    let binding_for = |name: &str| {
        bindings
            .iter()
            .find(|(action, _)| action.as_str() == name)
            .map(|(_, binding)| binding.clone())
    };
    let action = bindings
        .iter()
        .find(|(_, binding)| binding.eq_ignore_ascii_case(&label))
        .map(|(action, _)| action.clone());
    let action = match action {
        Some(action) => action,
        None if label == "Ctrl+Up" && binding_for("volume_up").as_deref() == Some("Up") => {
            "volume_up".to_string()
        }
        None if label == "Ctrl+Down" && binding_for("volume_down").as_deref() == Some("Down") => {
            "volume_down".to_string()
        }
        None if label == "Esc" => {
            let win = player.borrow().window.clone();
            if let Some(win) = win {
                if win.frame.is_full_screen() {
                    set_fullscreen(player, false);
                    return true;
                }
            }
            return false;
        }
        None => return false,
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
            if let (Some(shared), Some(channel), Some(win)) =
                (&state.shared, &state.current_channel, &state.window)
            {
                toggle_recording(
                    &win.frame,
                    &shared.recordings,
                    &shared.buffers,
                    persist,
                    &shared.media_probe,
                    channel,
                    status,
                    Some(player),
                    &shared.share_ctx(),
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
            // Python's steps: 2% normally, 5% with Ctrl held
            // (the event label carries the modifier).
            let step = if label.starts_with("Ctrl+") { 5 } else { 2 };
            let delta = if action == "volume_up" { step } else { -step };
            let value = (win.volume.get_value() + delta).clamp(0, 100);
            player_set_volume(player, value);
            // N23: Python's volume change only refreshes the
            // status line (its Vol segment) — no prefix, no
            // speech.
            update_player_status(player, persist, status, None, 3);
        }
        "audio_track" => cycle_audio_track(player, persist, status),
        "subtitles" => cycle_subtitle_track(player, persist, status),
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
            announce_cue(player, persist, &action, status);
        }
        "fullscreen" => set_fullscreen(player, !win.frame.is_full_screen()),
        "hide" => win.frame.hide(),
        "what_is_playing" => {
            let (hook, current) = {
                let state = player.borrow();
                (
                    state
                        .shared
                        .as_ref()
                        .and_then(|shared| shared.whats_playing.clone()),
                    state.current_channel.clone(),
                )
            };
            match hook {
                Some(hook) => hook(current, status),
                None => match &current {
                    Some(channel) => {
                        status.set_label(&trf("Now playing: {name}", &[("name", &channel.name)]));
                    }
                    None => status.set_label("No channel is selected."),
                },
            }
        }
        _ => return false,
    }
    true
}

/// Announce the current, previous, or next subtitle cue
/// through the review cursor (P11): only cues already shown
/// can be read, the last shown cue survives gaps, and Next
/// never reads the file's unplayed tail. Review speech goes
/// through the shared cue route (`speak_cue_text`, P12): the
/// selected backend speaks it, exactly one result, whether
/// or not automatic subtitle speech is enabled — never the
/// live region directly.
fn announce_cue(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    action: &str,
    status: Status,
) {
    let step = match action {
        "previous_subtitle" => -1,
        "next_subtitle" => 1,
        _ => 0,
    };
    review_subtitle(player, persist, status, step);
}

/// Speak one review step (shared by the player window and
/// the main window's subtitle-speech menu, as in Python).
fn review_subtitle(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    step: i32,
) {
    // Refresh the sample first so review is right even
    // between 150 ms ticks.
    sample_subtitles(player);
    let text = {
        let mut state = player.borrow_mut();
        let active = state.active_cue_index;
        let last = state.last_shown_cue;
        let cues = state.subtitle_cues.clone();
        state.review.review(&cues, active, last, step)
    };
    speak_cue_text(player, persist, status, &text);
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
    // N20: Python's cast dialog opens with the disconnect
    // prompt when a cast is already live, and nothing else.
    if prompt_cast_disconnect(&ui.frame, &ui.view, worker) {
        return;
    }
    ui.frame.show(true);
    ui.frame.raise();
    ui.view.borrow_mut().channel = channel.cloned();
    match channel {
        Some(channel) => {
            ui.status.set_label(&trf(
                "Looking for cast devices for {name}...",
                &[("name", &channel.name)],
            ));
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

/// A `file://` URI for a subtitle sidecar (Python passes
/// `Path.as_uri()` to `add_slave`).
fn file_uri(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let encoded = text.replace(' ', "%20").replace('#', "%23");
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// Ask for a subtitle file, attach it to VLC for rendering
/// (P08) and parse it on a worker for display and speech
/// (P09). External files are refused during local-buffer
/// playback — they are not synchronized with the buffer.
fn prompt_subtitle_file(
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    persist: &Persist,
    status: Status,
) {
    if buffers_of(player).is_some_and(|buffers| buffers.borrow().current.is_some()) {
        let _ = MessageDialog::builder(
            &frame,
            "External subtitle files are not synchronized with the local live buffer. Use the stream's embedded subtitles.",
            "Load subtitles",
        )
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal_tracked();
        return;
    }
    let dialog = FileDialog::builder(&frame)
        .with_message("Load subtitles")
        .with_wildcard(
            "Subtitles (*.srt;*.vtt;*.ass;*.ssa)|*.srt;*.vtt;*.ass;*.ssa|All files (*.*)|*.*",
        )
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    if player.borrow().vlc_active {
        let uri = file_uri(std::path::Path::new(&path));
        let attached = {
            let mut state = player.borrow_mut();
            state
                .vlc
                .as_mut()
                .is_some_and(|engine| engine.add_subtitle_slave(&uri))
        };
        if !attached {
            player_status(status, persist, "Could not load subtitle file.", 1);
            return;
        }
        // N23: Python names the loaded file on the status
        // line at priority 2, and — when the file's suffix is
        // not a speakable text format and subtitle speech is
        // on — says so at priority 1.
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        update_player_status(
            player,
            persist,
            status,
            Some(&format!("Subtitle file loaded: {name}")),
            2,
        );
        let speakable = std::path::Path::new(&path)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .is_some_and(|ext| matches!(ext.as_str(), "srt" | "vtt" | "ass" | "ssa"));
        if !speakable && persist.config.borrow().bool_or("speak_subtitles", false) {
            update_player_status(
                player,
                persist,
                status,
                Some("Subtitle speech needs a supported text subtitle file."),
                1,
            );
        }
    }
    let generation = {
        let mut state = player.borrow_mut();
        state.subtitle_generation += 1;
        state.subtitle_generation
    };
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    player.borrow_mut().pending_subtitles = Some(std::sync::Arc::clone(&slot));
    let path_buf = PathBuf::from(&path);
    std::thread::spawn(move || {
        let result = load_cues(&path_buf);
        if let Ok(mut guard) = slot.lock() {
            *guard = Some((generation, result));
        }
    });
}

/// Deliver a finished subtitle parse (P09): a result whose
/// generation has moved on (new play, stop, track select) is
/// dropped silently; the current one installs its cues.
fn deliver_subtitles(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let delivered = {
        let state = player.borrow();
        let Some(slot) = &state.pending_subtitles else {
            return;
        };
        let Ok(mut guard) = slot.lock() else {
            return;
        };
        guard.take()
    };
    let Some((generation, result)) = delivered else {
        return;
    };
    let mut state = player.borrow_mut();
    if generation != state.subtitle_generation {
        return; // stale work from a superseded load
    }
    state.pending_subtitles = None;
    match result {
        Ok(cues) => {
            let count = cues.len();
            state.subtitle_cues = cues;
            state.last_spoken_cue = None;
            state.last_label_cue = None;
            // A new source starts review empty (P11).
            state.review.reset();
            state.active_cue_index = None;
            state.last_shown_cue = None;
            state.shown_cues.clear();
            drop(state);
            if count == 0 {
                player_status(status, persist, "No subtitle text found in this file.", 1);
            } else {
                player_status(
                    status,
                    persist,
                    &format!("Loaded {count} subtitle cues."),
                    2,
                );
            }
        }
        Err(error) => {
            drop(state);
            player_status(
                status,
                persist,
                &format!("Could not load subtitles: {error}"),
                1,
            );
        }
    }
}

/// The 150 ms subtitle tick (P10): deliver finished parses,
/// adopt the sidecar's VLC track once it binds, and sample
/// the active cue for the label and for speech — a cue lying
/// entirely between one-second ticks is still presented.
fn subtitle_tick(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    drain_fullscreen_layout(player);
    run_pending_play(player, persist, status);
    deliver_vlc_load(player, persist, status);
    deliver_subtitles(player, persist, status);
    {
        let mut state = player.borrow_mut();
        if !state.subtitle_cues.is_empty() && state.file_subtitle_track.is_none() {
            if let Some(engine) = &state.vlc {
                let spu = engine.subtitle_track();
                if spu >= 0 {
                    state.file_subtitle_track = Some(spu);
                    state.selected_spu = spu;
                }
            }
        }
    }
    sample_subtitles(player);
    update_subtitle_label(player);
    speak_subtitle_cue(player, persist, status);
}

/// The subtitle sampling clock (P10): media position minus
/// the engine's subtitle delay — media-clock only, never
/// wall-clock extrapolation. The fallback engine has no
/// delay.
fn subtitle_position(state: &PlayerState) -> i64 {
    let (position, _) = position_length(state);
    if state.vlc_active {
        if let Some(engine) = &state.vlc {
            return position - engine.subtitle_delay_ms();
        }
    }
    position
}

/// One subtitle sample (P11): maintain the active and
/// latest-shown indexes the review cursor is bounded by, and
/// record each newly delivered cue for review and Recent —
/// non-empty, duplicates by text+start suppressed, capped
/// at the manager's 50.
fn sample_subtitles(player: &Rc<RefCell<PlayerState>>) {
    let mut state = player.borrow_mut();
    if state.subtitle_cues.is_empty() {
        state.active_cue_index = None;
        state.last_shown_cue = None;
        return;
    }
    let position = subtitle_position(&state);
    let active = active_cue(&state.subtitle_cues, position);
    let last = started_cue(&state.subtitle_cues, position);
    let changed = active != state.active_cue_index;
    state.active_cue_index = active;
    state.last_shown_cue = last;
    state.review.on_sample(active, last, changed);
    if changed {
        if let Some(index) = active {
            let cue = state.subtitle_cues[index].clone();
            let duplicate = state
                .shown_cues
                .back()
                .is_some_and(|tail| tail.start == cue.start && tail.text == cue.text);
            if !cue.text.is_empty() && !duplicate {
                state.shown_cues.push_back(cue);
                while state.shown_cues.len() > 50 {
                    state.shown_cues.pop_front();
                }
            }
        }
    }
}

/// Refresh the subtitle label from the media position (the
/// sampling half of the old 1 s chrome update, now at 150 ms).
fn update_subtitle_label(player: &Rc<RefCell<PlayerState>>) {
    let mut state = player.borrow_mut();
    let Some(win) = state.window.clone() else {
        return;
    };
    if state.selected_spu == -1 {
        if state.last_label_cue.is_some() {
            state.last_label_cue = None;
            win.subtitle_label.set_label("");
        }
        return;
    }
    let position = subtitle_position(&state);
    let current = active_cue(&state.subtitle_cues, position);
    if current != state.last_label_cue {
        state.last_label_cue = current;
        let label = current.map_or("", |index| state.subtitle_cues[index].text.as_str());
        win.subtitle_label.set_label(label);
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

/// Startup speech setup (Python's backend construction): build
/// the cue router from the configured backend, apply the saved
/// application-voice volume, rate and output, and persist a
/// legacy output name that migrated to a token ID.
fn init_speech(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let (backend, policy, saved_output, volume, rate) = {
        let config = persist.config.borrow();
        (
            iptv_core::speech::speech_backend(&config),
            iptv_core::speech::device_policy(&config),
            config.string_or("subtitle_app_output", ""),
            config
                .value("subtitle_app_volume")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(100),
            config
                .value("subtitle_app_rate")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        )
    };
    let mut note = None;
    let mut migrated = None;
    {
        let mut state = player.borrow_mut();
        let available = state.app_voice.available();
        state.cue_speech = CueSpeech::new(backend, available);
        if available {
            state
                .app_voice
                .set_volume(i32::try_from(volume).unwrap_or(100));
            state.app_voice.set_rate(i32::try_from(rate).unwrap_or(0));
            if !saved_output.is_empty() {
                if !state.app_voice.set_output(&saved_output, policy) {
                    if backend == SpeechBackend::AppVoice {
                        note = state.cue_speech.note_device_lost();
                    }
                } else if state.app_voice.selected_output() != saved_output {
                    migrated = Some(state.app_voice.selected_output().to_string());
                }
            }
        }
    }
    if let Some(selected) = migrated {
        persist.set_and_save("subtitle_app_output", serde_json::json!(selected));
    }
    if let Some(text) = note {
        live_region::announce_with_priority(status.label, text, live_region::Priority::High);
    }
}

/// Speak one cue's text through the selected backend (automatic
/// cues and manual review share this path, as in Python): exactly
/// one backend per cue; an application-voice failure is reported
/// through the screen reader once per stretch, ungated by the
/// announcement level.
fn speak_cue_text(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    text: &str,
) {
    enum Act {
        ScreenReader(String),
        Note(Option<&'static str>),
        Suppressed,
    }
    let act = {
        let mut state = player.borrow_mut();
        match state.cue_speech.route(text) {
            CueRoute::ScreenReader(text) => Act::ScreenReader(text),
            CueRoute::AppVoice(text) => {
                let policy = iptv_core::speech::device_policy(&persist.config.borrow());
                let result = state.app_voice.speak(&text, policy);
                Act::Note(state.cue_speech.note_app_result(result))
            }
            CueRoute::Suppressed => Act::Suppressed,
        }
    };
    match act {
        Act::ScreenReader(text) => {
            live_region::announce_with_priority(status.label, &text, live_region::Priority::Medium);
        }
        Act::Note(Some(text)) => {
            live_region::announce_with_priority(status.label, text, live_region::Priority::High);
        }
        _ => {}
    }
}

/// The device watchdog (Python's 500 ms output poll, on this
/// app's player tick): while a subtitled session plays through
/// the application voice, a vanished output pauses speech (or
/// falls back, per policy) and is reported once through the
/// screen reader.
fn drive_speech_watchdog(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    let note = {
        let mut state = player.borrow_mut();
        if state.cue_speech.backend() != SpeechBackend::AppVoice
            || state.session.current().is_none()
            || state.subtitle_cues.is_empty()
        {
            return;
        }
        let policy = iptv_core::speech::device_policy(&persist.config.borrow());
        if state.app_voice.validate_output(policy) {
            state.cue_speech.note_route_healthy();
            None
        } else {
            state.cue_speech.note_device_lost()
        }
    };
    if let Some(text) = note {
        live_region::announce_with_priority(status.label, text, live_region::Priority::High);
    }
}

fn speak_subtitle_cue(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    if !persist.config.borrow().bool_or("speak_subtitles", false) {
        return;
    }
    if player.borrow().selected_spu == -1 {
        return; // Subtitles Off: nothing is presented or spoken.
    }
    let position = subtitle_position(&player.borrow());
    if position <= 0 {
        return;
    }
    let mut state = player.borrow_mut();
    if state.subtitle_cues.is_empty() {
        return;
    }
    let text = {
        let current = active_cue(&state.subtitle_cues, position);
        if current == state.last_spoken_cue {
            None
        } else {
            state.last_spoken_cue = current;
            current.map(|index| state.subtitle_cues[index].text.clone())
        }
    };
    drop(state);
    if let Some(text) = text {
        speak_cue_text(player, persist, status, &text);
    }
}

/// Persist the current position for seekable content (Python
/// checkpoint rules decide whether it is kept). While a
/// resumed position is unconfirmed, the guard (P16) suppresses
/// the write so a failed seek cannot overwrite the useful
/// checkpoint; an unconfirmed resume is reported once after
/// 60 s. Non-forced checkpoints are throttled to one per
/// 30 s, as in Python; stop/close/prepare force one.
fn checkpoint_resume(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    force: bool,
) {
    let (channel, position, length) = {
        let state = player.borrow();
        if state.session.current().is_none_or(|r| r.live) {
            return;
        }
        let Some(channel) = &state.current_channel else {
            return;
        };
        let (position, length) = position_length(&state);
        (channel.clone(), position, length)
    };
    if position <= 0 || length <= 0 {
        return;
    }
    {
        let mut state = player.borrow_mut();
        if let Some(guard) = &mut state.resume_guard {
            match guard.on_checkpoint(position, now_ms()) {
                iptv_core::playback::GuardVerdict::Protected => return,
                iptv_core::playback::GuardVerdict::Failed => {
                    state.resume_guard = None;
                    // Python re-anchors the 30 s throttle at
                    // the failure report.
                    state.last_checkpoint_ms = now_ms();
                    drop(state);
                    player_status(
                        status,
                        persist,
                        "The saved playback position could not be restored.",
                        2,
                    );
                    return;
                }
                iptv_core::playback::GuardVerdict::Confirmed => {
                    state.resume_guard = None;
                }
            }
        }
        if !force && now_ms() - state.last_checkpoint_ms < 30_000 {
            return;
        }
        state.last_checkpoint_ms = now_ms();
    }
    let key = iptv_core::bookmarks::item_key(&channel);
    if key.is_empty() {
        return;
    }
    let entries = iptv_core::bookmarks::checkpoint(
        &persist.resume_entries(),
        &key,
        &channel,
        position,
        length,
        now_epoch(),
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
    reload_sources_mode(wiring, false);
}

/// Reload with the provider cache bypassed when `force` is set
/// (B13 refresh; plain sources shed their download caches at the
/// refresh call site instead, so untouched sources stay cached).
fn reload_sources_mode(wiring: &Wiring, force: bool) {
    let (sources, accounts) = configured_entries(wiring);
    // B03: always load, even with no plain sources left — an empty
    // source list must publish an empty catalogue, not keep the
    // removed playlist's channels on screen.
    {
        let mut browser = wiring.browser.borrow_mut();
        browser.reset_scope();
        if force {
            browser.refresh_sources(sources);
        } else {
            browser.load_sources(sources);
        }
    }
    assert!(wiring.timer.start(50, false));
    if accounts.is_empty() {
        // B02: no accounts configured — any provider channels from a
        // removed account must disappear from every surface.
        wiring
            .browser
            .borrow_mut()
            .replace_provider_channels(Vec::new(), "");
    } else if force {
        wiring.provider_worker.load_fresh(accounts);
        wiring.provider_pending.set(true);
        assert!(wiring.timer.start(50, false));
    } else {
        wiring.provider_worker.load(accounts);
        wiring.provider_pending.set(true);
        assert!(wiring.timer.start(50, false));
    }
}

/// The stored friendly names for plain sources (B13), cleaned.
fn playlist_names(wiring: &Wiring) -> BTreeMap<String, String> {
    let raw = wiring
        .persist
        .config
        .borrow()
        .value("playlist_names")
        .cloned();
    let mut map = BTreeMap::new();
    if let Some(serde_json::Value::Object(object)) = raw {
        for (key, value) in &object {
            if let Some(text) = value.as_str() {
                map.insert(key.clone(), text.to_string());
            }
        }
    }
    iptv_core::sources::normalize_source_names(&map)
}

/// The stored friendly names for EPG sources (B13), cleaned.
fn epg_names(wiring: &Wiring) -> BTreeMap<String, String> {
    let raw = wiring.persist.config.borrow().value("epg_names").cloned();
    let mut map = BTreeMap::new();
    if let Some(serde_json::Value::Object(object)) = raw {
        for (key, value) in &object {
            if let Some(text) = value.as_str() {
                map.insert(key.clone(), text.to_string());
            }
        }
    }
    iptv_core::sources::normalize_source_names(&map)
}

/// Where the guide's `SQLite` mirror lives.
fn epg_store_path() -> std::path::PathBuf {
    iptv_core::config::user_config_dir().join("epg.db")
}

/// The guide sources a load should read (audit E01): the user's
/// own list — Python's `epgs` key, unioned with this port's legacy
/// `epg_urls` so existing configurations keep working — merged
/// with provider-discovered guides and (when `use_playlist_epg`
/// allows) playlist-discovered ones, deduplicated the way
/// Python's `reload_epg_sources` does.
fn effective_epg_sources(wiring: &Wiring) -> Vec<String> {
    let config = wiring.persist.config.borrow();
    let mut own = config.string_list("epgs");
    for legacy in config.string_list("epg_urls") {
        if !own.contains(&legacy) {
            own.push(legacy);
        }
    }
    let use_playlist = config.bool_or("use_playlist_epg", true);
    let extra = wiring.epg_extra_sources.borrow();
    // Playlist-linked guides (B09) enter through the browser's
    // header discovery; the merge applies the opt-out.
    let playlist_guides = wiring.browser.borrow().playlist_guide_urls();
    merged_epg_sources(&own, &extra, &playlist_guides, use_playlist)
}

/// Start a guide load (the worker also mirrors the guide to the
/// store on its own thread). Automatic loads honor Python's
/// decision (audit E03): a disabled guide never loads, and an
/// already usable guide inside its refresh interval is not
/// fetched again. `force` is for explicit user actions.
fn dispatch_epg_load(wiring: &Wiring, sources: Vec<String>) {
    dispatch_epg_load_mode(wiring, sources, false);
}

fn dispatch_epg_load_mode(wiring: &Wiring, sources: Vec<String>, force: bool) {
    if sources.is_empty() {
        return;
    }
    if wiring.epg_pending.get() {
        // A load is already running; Python re-runs afterwards
        // when the sources changed under it.
        wiring.epg_recheck.set(true);
        return;
    }
    if wiring.epg_check_pending.get() {
        wiring.epg_recheck.set(true);
        return;
    }
    if !wiring.persist.config.borrow().bool_or("epg_enabled", true) {
        return;
    }
    if force {
        start_epg_load(wiring, sources);
        return;
    }
    wiring.epg_check_pending.set(true);
    submit_epg_read(
        wiring,
        iptv_core::epg::EpgReadKind::UsableData { at: now_epoch() },
        PendingEpgReadUi::AutoImportCheck { sources },
    );
}

fn finish_epg_auto_import(wiring: &Wiring, sources: Vec<String>, usable: bool) {
    wiring.epg_check_pending.set(false);
    if wiring.epg_pending.get() {
        wiring.epg_recheck.set(true);
        return;
    }
    let should_import = {
        let config = wiring.persist.config.borrow();
        #[allow(clippy::cast_possible_truncation)]
        let last_import = config.f64_or("epg_last_import_epoch", 0.0) as i64;
        should_auto_import_epg(
            &sources,
            &config.string_or("epg_last_sources_hash", ""),
            last_import,
            now_epoch(),
            config.f64_or("epg_auto_import_interval_hours", 6.0),
            usable,
        )
    };
    if should_import {
        start_epg_load(wiring, sources);
    }
}

/// Start a guide import the user asked for by hand (N21;
/// Python's `start_epg_import_background(force=True,
/// notify=True)`): the load runs forced, and its completion
/// reports back with the Import Complete / Import Failed box.
/// Returns false when no import was started — no sources, the
/// guide disabled, or one already running — so the caller can
/// say so, as Python's callers do.
fn start_manual_epg_import(wiring: &Wiring, sources: Vec<String>) -> bool {
    if sources.is_empty() || wiring.epg_pending.get() {
        return false;
    }
    if !wiring.persist.config.borrow().bool_or("epg_enabled", true) {
        return false;
    }
    wiring.epg_import_notify.set(true);
    start_epg_load(wiring, sources);
    true
}

fn start_epg_load(wiring: &Wiring, sources: Vec<String>) {
    wiring
        .epg_inflight_sources
        .borrow_mut()
        .clone_from(&sources);
    wiring.epg_worker.load(sources, Some(epg_store_path()));
    wiring.epg_pending.set(true);
    assert!(wiring.timer.start(50, false));
}

/// Reload the guide from the effective sources. While a playlist
/// load is running, the guide waits: at real-world scale the guide
/// is hundreds of MB and racing it starved the playlist download
/// and parse, so channels appeared minutes late (validation 0049).
/// The completion tick dispatches the deferred load the moment the
/// browser settles.
fn reload_epg(wiring: &Wiring) {
    if effective_epg_sources(wiring).is_empty() {
        return;
    }
    if wiring.browser.borrow().is_busy() {
        wiring.epg_deferred.set(true);
        return;
    }
    dispatch_epg_load(wiring, effective_epg_sources(wiring));
}

/// Display name for one `playlists` config entry in the manager.
/// Plain sources show their friendly name when one is stored (B13);
/// URLs otherwise show in full, files by name.
fn entry_display(entry: &serde_json::Value, names: &BTreeMap<String, String>) -> String {
    match entry {
        serde_json::Value::String(text) => {
            let key = iptv_core::sources::source_name_key(text);
            if let Some(name) = names.get(&key) {
                if !name.trim().is_empty() {
                    return name.trim().to_string();
                }
            }
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
fn manager_refresh(list: ListBox, entries: &[serde_json::Value], names: &BTreeMap<String, String>) {
    let labels: Vec<String> = entries
        .iter()
        .map(|entry| entry_display(entry, names))
        .collect();
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
    refresh: Button,
    rename: Button,
    copy_url: Button,
}

/// Working state shared by the Playlist Manager's actions.
#[derive(Clone)]
struct ManagerState {
    entries: Rc<RefCell<Vec<serde_json::Value>>>,
    names: Rc<RefCell<BTreeMap<String, String>>>,
    /// Ctrl+A arms "refresh all" until the highlight moves (B13).
    refresh_all: Rc<std::cell::Cell<bool>>,
}

fn manager_selected(list: ListBox, state: &ManagerState) -> Option<(usize, serde_json::Value)> {
    let selection = list.get_selection()?;
    let index = usize::try_from(selection).unwrap_or(usize::MAX);
    state
        .entries
        .borrow()
        .get(index)
        .cloned()
        .map(|entry| (index, entry))
}

/// Rename the selected entry (B13, Python's F2): accounts rename
/// their `name` field; plain sources rename their stored friendly
/// name. An empty or unchanged name keeps the existing label.
fn manager_rename(manager_frame: Frame, list: ListBox, state: &ManagerState, mstatus: Status) {
    let Some((index, entry)) = manager_selected(list, state) else {
        mstatus.set_label("Choose an entry to rename first.");
        return;
    };
    let current = match &entry {
        serde_json::Value::String(text) => state
            .names
            .borrow()
            .get(&iptv_core::sources::source_name_key(text))
            .cloned()
            .unwrap_or_default(),
        serde_json::Value::Object(map) => map
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => return,
    };
    let dialog = TextEntryDialog::builder(&manager_frame, "Name", "Rename")
        .with_default_value(&current)
        .build();
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    let Some(value) = dialog.get_value() else {
        return;
    };
    let name = value.trim().to_string();
    if name == current {
        return;
    }
    match &entry {
        serde_json::Value::String(text) => {
            let key = iptv_core::sources::source_name_key(text);
            if name.is_empty() {
                state.names.borrow_mut().remove(&key);
            } else {
                state.names.borrow_mut().insert(key, name);
            }
        }
        serde_json::Value::Object(_) => {
            if let Some(serde_json::Value::Object(map)) = state.entries.borrow_mut().get_mut(index)
            {
                if name.is_empty() {
                    map.remove("name");
                } else {
                    map.insert("name".to_string(), serde_json::json!(name));
                }
            }
        }
        _ => {}
    }
    manager_refresh(list, &state.entries.borrow(), &state.names.borrow());
}

/// Remove the selected entry after Python's confirmation (B14):
/// the question names the playlist and defaults to No, so a slip
/// on Del cannot lose an account's credentials.
fn manager_remove(manager_frame: Frame, list: ListBox, state: &ManagerState, mstatus: Status) {
    let Some((index, entry)) = manager_selected(list, state) else {
        mstatus.set_label("Choose an entry to remove first.");
        return;
    };
    let label = entry_display(&entry, &state.names.borrow());
    let answer = MessageDialog::builder(
        &manager_frame,
        &format!(
            "Delete the playlist \u{201c}{label}\u{201d}?\n\nThis removes it from the list of playlists. It cannot be undone."
        ),
        "Delete Playlist",
    )
    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::NoDefault)
    .build()
    .show_modal_tracked();
    if answer != ID_YES {
        return;
    }
    state.entries.borrow_mut().remove(index);
    manager_refresh(list, &state.entries.borrow(), &state.names.borrow());
}

/// Copy the selected entry's URL (B13): the source string itself for
/// URL playlists, the account's base URL for providers.
fn manager_copy_url(list: ListBox, state: &ManagerState, mstatus: Status) {
    let Some((_, entry)) = manager_selected(list, state) else {
        mstatus.set_label("Choose an entry first.");
        return;
    };
    let url = match &entry {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) => map
            .get("base_url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        _ => None,
    };
    let url = url.filter(|text| text.starts_with("http://") || text.starts_with("https://"));
    match url {
        Some(url) if Clipboard::get().set_text(&url) => {
            mstatus.set_label("URL copied to the clipboard.");
        }
        Some(_) => mstatus.set_label("Could not open the clipboard."),
        None => mstatus.set_label("The selected entry has no URL to copy."),
    }
}

/// Refresh playlists (B13, Python's F5): the working list is saved
/// first, as the Python dialog edits the live list. Refresh All (or
/// Ctrl+A armed) bypasses every cache; a single plain source sheds
/// only its own download cache; a single account reloads providers
/// fresh.
fn manager_refresh_sources(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    state: &ManagerState,
    scope_choice: Choice,
    mstatus: Status,
    status: Status,
) {
    let _ = manager_frame;
    wiring.persist.set_and_save(
        "playlists",
        serde_json::Value::Array(state.entries.borrow().clone()),
    );
    wiring.persist.set_and_save(
        "playlist_names",
        serde_json::Value::Object(
            state
                .names
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::json!(value)))
                .collect(),
        ),
    );
    fill_scope_choices(wiring, scope_choice);
    let cache_dir = config::user_config_dir().join("playlist-cache");
    let selected = manager_selected(list, state);
    let all = state.refresh_all.get() || selected.is_none();
    state.refresh_all.set(false);
    if all {
        let (sources, _) = configured_entries(wiring);
        for source in &sources {
            if source.is_url() {
                iptv_core::sources::drop_url_download_cache(&cache_dir, source.location());
            }
        }
        reload_sources_mode(wiring, true);
        status.set_label("Refreshing all playlists...");
        mstatus.set_label("Refreshing all playlists...");
        return;
    }
    let Some((_, entry)) = selected else {
        return;
    };
    match &entry {
        serde_json::Value::String(text) if text.starts_with("http") => {
            iptv_core::sources::drop_url_download_cache(&cache_dir, text);
            let (sources, _) = configured_entries(wiring);
            wiring.browser.borrow_mut().load_sources(sources);
            assert!(wiring.timer.start(50, false));
            status.set_label("Refreshing playlist...");
        }
        serde_json::Value::String(_) => {
            let (sources, _) = configured_entries(wiring);
            wiring.browser.borrow_mut().load_sources(sources);
            assert!(wiring.timer.start(50, false));
            status.set_label("Refreshing playlist...");
        }
        serde_json::Value::Object(_) => {
            let (_, accounts) = configured_entries(wiring);
            wiring.provider_worker.load_fresh(accounts);
            wiring.provider_pending.set(true);
            assert!(wiring.timer.start(50, false));
            status.set_label("Refreshing account...");
        }
        _ => {}
    }
    mstatus.set_label("Refreshing...");
}

/// Bind the Playlist Manager's add/edit/remove actions.
#[allow(clippy::too_many_arguments)]
fn bind_playlist_manager_buttons(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    state: &ManagerState,
    buttons: &PlaylistManagerButtons,
    scope_choice: Choice,
    mstatus: Status,
    status: Status,
) {
    let PlaylistManagerButtons {
        add_file,
        add_url,
        add_xtream,
        add_stalker,
        edit,
        remove,
        refresh,
        rename,
        copy_url,
    } = *buttons;
    let state_f = state.clone();
    add_file.on_click(move |_| {
        let dialog = FileDialog::builder(&manager_frame)
            .with_message("Add playlist file")
            .with_wildcard("M3U playlists (*.m3u;*.m3u8)|*.m3u;*.m3u8|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        if let Some(path) = dialog.get_path() {
            state_f
                .entries
                .borrow_mut()
                .push(serde_json::Value::String(path));
            manager_refresh(list, &state_f.entries.borrow(), &state_f.names.borrow());
        }
    });
    let state_u = state.clone();
    add_url.on_click(move |_| {
        let dialog = TextEntryDialog::builder(&manager_frame, "Playlist URL:", "Add URL").build();
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        if let Some(url) = dialog.get_value() {
            if !url.trim().is_empty() {
                state_u
                    .entries
                    .borrow_mut()
                    .push(serde_json::Value::String(url.trim().to_string()));
                manager_refresh(list, &state_u.entries.borrow(), &state_u.names.borrow());
            }
        }
    });
    let wiring_x = clone_wiring(wiring);
    let state_x = state.clone();
    add_xtream.on_click(move |_| {
        open_account_form(&wiring_x, "xtream", None, &state_x, list, mstatus);
    });
    let wiring_s = clone_wiring(wiring);
    let state_s = state.clone();
    add_stalker.on_click(move |_| {
        open_account_form(&wiring_s, "stalker", None, &state_s, list, mstatus);
    });
    let wiring_r = clone_wiring(wiring);
    let state_r = state.clone();
    refresh.on_click(move |_| {
        manager_refresh_sources(
            &wiring_r,
            manager_frame,
            list,
            &state_r,
            scope_choice,
            mstatus,
            status,
        );
    });
    let state_n = state.clone();
    rename.on_click(move |_| manager_rename(manager_frame, list, &state_n, mstatus));
    let state_c = state.clone();
    copy_url.on_click(move |_| manager_copy_url(list, &state_c, mstatus));
    bind_manager_edit_buttons(wiring, manager_frame, list, state, edit, remove, mstatus);
    bind_manager_keys(
        wiring,
        manager_frame,
        list,
        state,
        scope_choice,
        mstatus,
        status,
    );
}

/// The list's row shortcuts (B13): F5 refresh, Ctrl+A arms refresh
/// all, F2 rename, Del remove (with the B14 confirmation).
#[allow(clippy::too_many_arguments)]
fn bind_manager_keys(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    state: &ManagerState,
    scope_choice: Choice,
    mstatus: Status,
    status: Status,
) {
    let wiring_k = clone_wiring(wiring);
    let state_k = state.clone();
    list.bind_internal(EventType::KEY_DOWN, move |event| {
        let label = key_event_label(event);
        match label.as_deref() {
            Some("F5") => {
                manager_refresh_sources(
                    &wiring_k,
                    manager_frame,
                    list,
                    &state_k,
                    scope_choice,
                    mstatus,
                    status,
                );
                event.skip(false);
            }
            Some("Ctrl+A") => {
                state_k.refresh_all.set(true);
                mstatus.set_label("All playlists selected. Press F5 to refresh them all.");
                event.skip(false);
            }
            Some("F2") => {
                manager_rename(manager_frame, list, &state_k, mstatus);
                event.skip(false);
            }
            _ if event.get_key_code() == Some(127) => {
                manager_remove(manager_frame, list, &state_k, mstatus);
                event.skip(false);
            }
            _ => event.skip(true),
        }
    });
    // Moving the highlight disarms refresh-all (Python behavior).
    let state_m = state.clone();
    list.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
        state_m.refresh_all.set(false);
    });
}

/// Bind the Playlist Manager's edit/remove actions.
#[allow(clippy::too_many_arguments)]
fn bind_manager_edit_buttons(
    wiring: &Wiring,
    manager_frame: Frame,
    list: ListBox,
    state: &ManagerState,
    edit: Button,
    remove: Button,
    mstatus: Status,
) {
    let wiring_e = clone_wiring(wiring);
    let state_e = state.clone();
    edit.on_click(move |_| {
        let Some((index, entry)) = manager_selected(list, &state_e) else {
            mstatus.set_label("Choose an entry to edit first.");
            return;
        };
        match &entry {
            serde_json::Value::String(text) => {
                let dialog =
                    TextEntryDialog::builder(&manager_frame, "Playlist location:", "Edit Playlist")
                        .with_default_value(text)
                        .build();
                if dialog.show_modal_tracked() != ID_OK {
                    return;
                }
                if let Some(value) = dialog.get_value() {
                    state_e.entries.borrow_mut()[index] =
                        serde_json::Value::String(value.trim().to_string());
                    manager_refresh(list, &state_e.entries.borrow(), &state_e.names.borrow());
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
                    &state_e,
                    list,
                    mstatus,
                );
            }
            _ => {}
        }
    });
    let state_r = state.clone();
    remove.on_click(move |_| manager_remove(manager_frame, list, &state_r, mstatus));
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
    bind_escape_hide(manager);
    let panel = Panel::builder(&manager).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label("Playlists");
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

    let state = ManagerState {
        entries,
        names: Rc::new(RefCell::new(playlist_names(wiring))),
        refresh_all: Rc::new(std::cell::Cell::new(false)),
    };
    manager_refresh(list, &state.entries.borrow(), &state.names.borrow());
    let refresh = Button::builder(&panel).with_label("Re&fresh").build();
    let rename = Button::builder(&panel).with_label("Rena&me").build();
    let copy_url = Button::builder(&panel).with_label("Copy &URL").build();
    let manager_frame = manager;
    bind_playlist_manager_buttons(
        wiring,
        manager_frame,
        list,
        &state,
        &PlaylistManagerButtons {
            add_file,
            add_url,
            add_xtream,
            add_stalker,
            edit,
            remove,
            refresh,
            rename,
            copy_url,
        },
        scope_choice,
        mstatus,
        status,
    );
    let wiring_ok = clone_wiring(wiring);
    let state_ok = state.clone();
    ok.on_click(move |_| {
        wiring_ok.persist.set_and_save(
            "playlists",
            serde_json::Value::Array(state_ok.entries.borrow().clone()),
        );
        wiring_ok.persist.set_and_save(
            "playlist_names",
            serde_json::Value::Object(
                state_ok
                    .names
                    .borrow()
                    .iter()
                    .map(|(key, value)| (key.clone(), serde_json::json!(value)))
                    .collect(),
            ),
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
#[allow(clippy::too_many_lines)] // one dialog: fields for both provider shapes
fn open_account_form(
    wiring: &Wiring,
    kind: &str,
    existing: Option<(usize, serde_json::Value)>,
    state: &ManagerState,
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
        .with_size(Size::new(520, 460))
        .build();
    form.set_accessibility_label(title);
    bind_escape_hide(form);
    let panel = Panel::builder(&form).build();
    let get = |key: &str| -> String {
        existing
            .as_ref()
            .and_then(|(_, value)| value.get(key))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let get_bool = |key: &str, default: bool| -> bool {
        existing
            .as_ref()
            .and_then(|(_, value)| value.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    };
    let mut fields: Vec<(String, TextCtrl)> = Vec::new();
    // B05: the forms expose every provider option Python exposes —
    // Stalker username/password, both auto-EPG switches, Xtream
    // playlist type and stream output.
    // N24: each field carries Python's explanatory text as a
    // visible line under the field (Python hangs it on the field
    // as hint/tooltip/help text; here it is dialog text, never a
    // control description).
    let specs: Vec<(&str, &str, bool, &str)> = if kind == "stalker" {
        vec![
            (
                "Name",
                "name",
                false,
                "Optional nickname for this portal account",
            ),
            (
                "Portal URL",
                "base_url",
                false,
                "Base URL of the Stalker/Ministra portal",
            ),
            (
                "Username (optional)",
                "username",
                false,
                "Optional portal account username",
            ),
            (
                "Password (optional)",
                "password",
                true,
                "Optional portal account password",
            ),
            (
                "MAC address",
                "mac",
                false,
                "MAC address presented to the portal",
            ),
        ]
    } else {
        vec![
            (
                "Name",
                "name",
                false,
                "Optional nickname for this Xtream Codes account",
            ),
            (
                "Server URL",
                "base_url",
                false,
                "Base URL of the Xtream Codes server",
            ),
            ("Username", "username", false, "Xtream Codes username"),
            ("Password", "password", true, "Xtream Codes password"),
        ]
    };
    for (label, key, secret, description) in &specs {
        let caption = StaticText::builder(&panel).with_label(label).build();
        let _ = caption;
        let initial = if *key == "mac" && get("mac").is_empty() && existing.is_none() {
            // B06: a new portal account starts from Python's default
            // MAG-prefix MAC, ready to randomise or replace.
            random_mag_mac()
        } else {
            get(key)
        };
        let builder = TextCtrl::builder(&panel).with_value(&initial);
        let field = if *secret {
            builder.with_style(TextCtrlStyle::Password).build()
        } else {
            builder.build()
        };
        field.set_accessibility_label(label);
        let description_text = StaticText::builder(&panel).with_label(description).build();
        let _ = description_text;
        fields.push(((*key).to_string(), field));
    }
    let mac_field = fields
        .iter()
        .find(|(key, _)| key == "mac")
        .map(|(_, field)| *field);
    let mut randomise: Option<Button> = None;
    if let Some(mac_field) = mac_field {
        let button = Button::builder(&panel).with_label("Randomise &MAC").build();
        button.on_click(move |_| mac_field.set_value(&random_mag_mac()));
        randomise = Some(button);
    }
    let _ = randomise;
    let (stream_type_choice, output_choice) = if kind == "stalker" {
        (None, None)
    } else {
        let type_label = StaticText::builder(&panel)
            .with_label("Playlist type")
            .build();
        let _ = type_label;
        let type_choice = Choice::builder(&panel).build();
        type_choice.set_accessibility_label("Playlist type");
        for option in ["m3u_plus", "m3u", "enigma2"] {
            type_choice.append(option);
        }
        let current_type = {
            let value = get("stream_type");
            if value.is_empty() {
                "m3u_plus".to_string()
            } else {
                value
            }
        };
        type_choice.set_selection(
            ["m3u_plus", "m3u", "enigma2"]
                .iter()
                .position(|option| *option == current_type)
                .map_or(0, |pos| u32::try_from(pos).unwrap_or(0)),
        );
        let type_description = StaticText::builder(&panel)
            .with_label("Select the playlist format to download")
            .build();
        let _ = type_description;
        let output_label = StaticText::builder(&panel)
            .with_label("Stream output")
            .build();
        let _ = output_label;
        let output_choice = Choice::builder(&panel).build();
        output_choice.set_accessibility_label("Stream output");
        for option in ["ts", "m3u8", "rtmp"] {
            output_choice.append(option);
        }
        let current_output = {
            let value = get("output");
            if value.is_empty() {
                "ts".to_string()
            } else {
                value
            }
        };
        output_choice.set_selection(
            ["ts", "m3u8", "rtmp"]
                .iter()
                .position(|option| *option == current_output)
                .map_or(0, |pos| u32::try_from(pos).unwrap_or(0)),
        );
        let output_description = StaticText::builder(&panel)
            .with_label("Preferred stream container format")
            .build();
        let _ = output_description;
        (Some(type_choice), Some(output_choice))
    };
    let auto_epg_label = if kind == "stalker" {
        "Attempt to add provider XMLTV"
    } else {
        "Automatically add XMLTV URL"
    };
    let auto_epg = CheckBox::builder(&panel).with_label(auto_epg_label).build();
    auto_epg.set_accessibility_label(auto_epg_label);
    auto_epg.set_value(get_bool("auto_epg", true));
    // N24: Python explains the auto-EPG switch in its tooltip;
    // here it is a visible line under the checkbox.
    let auto_epg_description = StaticText::builder(&panel)
        .with_label(if kind == "stalker" {
            "Try to add the portal's XMLTV guide automatically"
        } else {
            "Include the provider's XMLTV guide automatically"
        })
        .build();
    let _ = auto_epg_description;
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    // F1 in the provider form opens the guide at the account's own
    // topic (S10). Bound before the OK handler takes the fields.
    let form_topic = if kind == "stalker" {
        "stalker-portal"
    } else {
        "xtream-codes"
    };
    bind_f1_help(&form, wiring, form_topic, mstatus);
    for (_, field) in &fields {
        bind_f1_help(field, wiring, form_topic, mstatus);
    }
    let form_hide = form;
    cancel.on_click(move |_| form_hide.hide());
    let kind_owned = kind.to_string();
    let entries = Rc::clone(&state.entries);
    let names = Rc::clone(&state.names);
    ok.on_click(move |_| {
        let value_of = |key: &str| -> String {
            fields
                .iter()
                .find(|(field_key, _)| field_key == key)
                .map(|(_, field)| field.get_value().trim().to_string())
                .unwrap_or_default()
        };
        let base_url = value_of("base_url");
        let username = value_of("username");
        let password = value_of("password");
        let mac_raw = value_of("mac");
        // B06: Python's required-field and MAC checks run before the
        // form may close; the problem is reported and the form stays.
        if let Some(error) =
            account_form_error(&kind_owned, &base_url, &username, &password, &mac_raw)
        {
            mstatus.set_label(&error);
            let _ = MessageDialog::builder(&form, &error, "Validation")
                .with_style(MessageDialogStyle::OK)
                .build()
                .show_modal_tracked();
            return;
        }
        let mut updates = serde_json::Map::new();
        updates.insert("type".to_string(), serde_json::json!(kind_owned));
        updates.insert("base_url".to_string(), serde_json::json!(base_url));
        updates.insert("username".to_string(), serde_json::json!(username));
        updates.insert("password".to_string(), serde_json::json!(password));
        updates.insert(
            "auto_epg".to_string(),
            serde_json::json!(auto_epg.get_value()),
        );
        if kind_owned == "stalker" {
            updates.insert("mac".to_string(), serde_json::json!(sanitize_mac(&mac_raw)));
        } else {
            let stream_type = stream_type_choice
                .and_then(|choice| choice.get_string_selection())
                .unwrap_or_else(|| "m3u_plus".to_string());
            let output = output_choice
                .and_then(|choice| choice.get_string_selection())
                .unwrap_or_else(|| "ts".to_string());
            updates.insert("stream_type".to_string(), serde_json::json!(stream_type));
            updates.insert("output".to_string(), serde_json::json!(output));
        }
        let name = value_of("name");
        if !name.is_empty() {
            updates.insert("name".to_string(), serde_json::json!(name));
        }
        // B04: edits merge into the existing entry — fields the form
        // does not carry (credentials of the other shape, unknown
        // retained settings) survive untouched.
        let base = existing.as_ref().map_or_else(
            || serde_json::Value::Object(serde_json::Map::new()),
            |(_, value)| value.clone(),
        );
        let mut merged = merge_account_fields(&base, updates);
        if name.is_empty() {
            if let serde_json::Value::Object(map) = &mut merged {
                map.remove("name");
            }
        }
        let id = existing
            .as_ref()
            .and_then(|(_, value)| {
                value
                    .get("id")
                    .or_else(|| value.get("provider_id"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| {
                let user = if kind_owned == "stalker" {
                    sanitize_mac(&mac_raw)
                } else {
                    username.clone()
                };
                generated_account_id(&kind_owned, &base_url, &user)
            });
        if let serde_json::Value::Object(map) = &mut merged {
            map.entry("id".to_string())
                .or_insert_with(|| serde_json::json!(id));
        }
        match &existing {
            Some((index, _)) => {
                entries.borrow_mut()[*index] = merged;
            }
            None => entries.borrow_mut().push(merged),
        }
        manager_refresh(manager_list, &entries.borrow(), &names.borrow());
        mstatus.set_label("Account saved in the list. OK in the Playlist Manager writes it.");
        form.hide();
    });
    form.show(true);
}

/// Refresh the EPG manager's list from its working state: friendly
/// name when one is stored, else the URL in full or the file name,
/// matching the playlist manager's plain-source labels.
fn epg_manager_refresh(list: ListBox, urls: &[String], names: &BTreeMap<String, String>) {
    let labels: Vec<String> = urls
        .iter()
        .map(|url| {
            let friendly = names
                .get(&iptv_core::sources::source_name_key(url))
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.trim().to_string());
            match friendly {
                Some(name) => name,
                None if url.starts_with("http://") || url.starts_with("https://") => url.clone(),
                None => scope_display_name(url),
            }
        })
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
}

/// Remove the EPG manager's selected row (B13).
fn epg_remove_row(
    list: ListBox,
    urls: &Rc<RefCell<Vec<String>>>,
    names: &Rc<RefCell<BTreeMap<String, String>>>,
) {
    if let Some(selection) = list.get_selection() {
        let index = usize::try_from(selection).unwrap_or(usize::MAX);
        if index < urls.borrow().len() {
            urls.borrow_mut().remove(index);
            epg_manager_refresh(list, &urls.borrow(), &names.borrow());
        }
    }
}

/// Rename the EPG manager's selected row (B13): the friendly name
/// is stored separately from the source itself.
fn epg_rename_row(
    manager_frame: Frame,
    list: ListBox,
    urls: &Rc<RefCell<Vec<String>>>,
    names: &Rc<RefCell<BTreeMap<String, String>>>,
) {
    let Some(selection) = list.get_selection() else {
        return;
    };
    let index = usize::try_from(selection).unwrap_or(usize::MAX);
    let Some(source) = urls.borrow().get(index).cloned() else {
        return;
    };
    let key = iptv_core::sources::source_name_key(&source);
    let current = names.borrow().get(&key).cloned().unwrap_or_default();
    let dialog = TextEntryDialog::builder(&manager_frame, "Name", "Rename")
        .with_default_value(&current)
        .build();
    if dialog.show_modal_tracked() != ID_OK {
        return;
    }
    if let Some(value) = dialog.get_value() {
        let name = value.trim().to_string();
        if name.is_empty() {
            names.borrow_mut().remove(&key);
        } else if name != current {
            names.borrow_mut().insert(key, name);
        }
        epg_manager_refresh(list, &urls.borrow(), &names.borrow());
    }
}

/// File > EPG Manager: edit the user's own guide sources
/// (Python's `epgs` key; the legacy `epg_urls` spelling is read
/// as the same list and kept in sync on save — audit E01; guides
/// discovered from providers and playlist headers are not editable
/// here, as in Python), with friendly names, the playlist-guide
/// checkbox (B09) and the shared row actions (B13).
#[allow(clippy::too_many_lines)] // one dialog: list, row actions and persistence
fn open_epg_manager(wiring: &Wiring, status: Status) {
    let urls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new({
        let config = wiring.persist.config.borrow();
        let mut own = config.string_list("epgs");
        for legacy in config.string_list("epg_urls") {
            if !own.contains(&legacy) {
                own.push(legacy);
            }
        }
        own
    }));
    let names: Rc<RefCell<BTreeMap<String, String>>> = Rc::new(RefCell::new(epg_names(wiring)));
    let manager = Frame::builder()
        .with_title("EPG Manager")
        .with_size(Size::new(640, 480))
        .build();
    manager.set_accessibility_label("EPG Manager");
    bind_escape_hide(manager);
    let panel = Panel::builder(&manager).build();
    let list = ListBox::builder(&panel).build();
    list.set_accessibility_label("EPG sources");
    epg_manager_refresh(list, &urls.borrow(), &names.borrow());
    let add_url = Button::builder(&panel).with_label("Add &URL").build();
    let add_file = Button::builder(&panel).with_label("Add &File").build();
    let remove = Button::builder(&panel).with_label("&Remove").build();
    let rename = Button::builder(&panel).with_label("Rena&me").build();
    let copy_url = Button::builder(&panel).with_label("Copy &URL").build();
    // B09: the playlist-guide opt-out lives here, as in Python.
    let playlist_epg = CheckBox::builder(&panel)
        .with_label("Also use the guide each playlist links to")
        .build();
    playlist_epg.set_accessibility_label("Also use the guide each playlist links to");
    playlist_epg.set_value(
        wiring
            .persist
            .config
            .borrow()
            .bool_or("use_playlist_epg", true),
    );
    let ok = Button::builder(&panel).with_label("&OK").build();
    let cancel = Button::builder(&panel).with_label("&Cancel").build();
    let manager_frame = manager;
    let urls_u = Rc::clone(&urls);
    let names_u = Rc::clone(&names);
    add_url.on_click(move |_| {
        let dialog =
            TextEntryDialog::builder(&manager_frame, "EPG URL (XMLTV):", "Add EPG URL").build();
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        if let Some(url) = dialog.get_value() {
            if !url.trim().is_empty() {
                urls_u.borrow_mut().push(url.trim().to_string());
                epg_manager_refresh(list, &urls_u.borrow(), &names_u.borrow());
            }
        }
    });
    let urls_f = Rc::clone(&urls);
    let names_f = Rc::clone(&names);
    add_file.on_click(move |_| {
        let dialog = FileDialog::builder(&manager_frame)
            .with_message("Add EPG file")
            .with_wildcard("XMLTV (*.xml;*.xml.gz)|*.xml;*.xml.gz|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal_tracked() != ID_OK {
            return;
        }
        if let Some(path) = dialog.get_path() {
            urls_f.borrow_mut().push(path);
            epg_manager_refresh(list, &urls_f.borrow(), &names_f.borrow());
        }
    });
    let urls_r = Rc::clone(&urls);
    let names_r = Rc::clone(&names);
    remove.on_click(move |_| epg_remove_row(list, &urls_r, &names_r));
    let urls_n = Rc::clone(&urls);
    let names_n = Rc::clone(&names);
    rename.on_click(move |_| epg_rename_row(manager_frame, list, &urls_n, &names_n));
    let urls_c = Rc::clone(&urls);
    copy_url.on_click(move |_| {
        let Some(selection) = list.get_selection() else {
            return;
        };
        let index = usize::try_from(selection).unwrap_or(usize::MAX);
        if let Some(url) = urls_c.borrow().get(index) {
            if url.starts_with("http://") || url.starts_with("https://") {
                let _ = Clipboard::get().set_text(url);
            }
        }
    });
    // Row shortcuts (B13): F2 rename, Del remove.
    let urls_k = Rc::clone(&urls);
    let names_k = Rc::clone(&names);
    list.bind_internal(EventType::KEY_DOWN, move |event| {
        match key_event_label(event).as_deref() {
            Some("F2") => {
                epg_rename_row(manager_frame, list, &urls_k, &names_k);
                event.skip(false);
            }
            _ if event.get_key_code() == Some(127) => {
                epg_remove_row(list, &urls_k, &names_k);
                event.skip(false);
            }
            _ => event.skip(true),
        }
    });
    let wiring_ok = clone_wiring(wiring);
    let persist_urls = Rc::clone(&urls);
    let persist_names = Rc::clone(&names);
    ok.on_click(move |_| {
        wiring_ok
            .persist
            .set_and_save("epgs", serde_json::json!(persist_urls.borrow().clone()));
        wiring_ok
            .persist
            .set_and_save("epg_urls", serde_json::json!(persist_urls.borrow().clone()));
        wiring_ok.persist.set_and_save(
            "epg_names",
            serde_json::Value::Object(
                persist_names
                    .borrow()
                    .iter()
                    .map(|(key, value)| (key.clone(), serde_json::json!(value)))
                    .collect(),
            ),
        );
        wiring_ok.persist.set_and_save(
            "use_playlist_epg",
            serde_json::json!(playlist_epg.get_value()),
        );
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
    /// Worker request id; 0 for rows driven by an ffmpeg
    /// catch-up capture instead of the download worker.
    id: u64,
    /// The catch-up identity (channel + programme window):
    /// a second request for the same programme is refused
    /// while one is active (audit R03).
    key: String,
    name: String,
    destination: String,
    progress: String,
    done: bool,
}

/// One recording the built-in player is watching through the
/// recording relay (N13): when it ends, playback resumes
/// directly if the player is still on the relay URL.
struct SharedPlayback {
    channel: Channel,
    relay_url: String,
    relay: std::sync::Arc<relay::RecordingRelay>,
}

/// A shared-playback step that must wait out the media settle
/// off the UI thread (N13): start a relay recording after the
/// player lets go, or resume direct playback after one ends.
#[derive(Clone)]
enum RelayJob {
    StartShare { key: String, channel: Channel },
    EndShare { channel: Channel },
}

/// The settle worker for shared playback: jobs sleep out the
/// media settle, then come back as results for the completion
/// tick (Python's stop-wait + settle threads).
struct RelayWorker {
    jobs: std::sync::mpsc::Sender<RelayJob>,
    results: std::sync::Mutex<std::sync::mpsc::Receiver<RelayJob>>,
    outstanding: Rc<std::cell::Cell<usize>>,
}

impl RelayWorker {
    fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<RelayJob>();
        let (result_sender, result_receiver) = std::sync::mpsc::channel::<RelayJob>();
        std::thread::spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                iptv_core::catchup_direct::settle_media_session();
                if result_sender.send(job).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            results: std::sync::Mutex::new(result_receiver),
            outstanding: Rc::new(std::cell::Cell::new(0)),
        }
    }

    fn start(&self, job: RelayJob) {
        self.outstanding.set(self.outstanding.get() + 1);
        let _ = self.jobs.send(job);
    }

    fn try_recv(&self) -> Option<RelayJob> {
        let job = self.results.lock().ok()?.try_recv().ok()?;
        self.outstanding
            .set(self.outstanding.get().saturating_sub(1));
        Some(job)
    }

    fn pending(&self) -> bool {
        self.outstanding.get() > 0
    }
}

/// The pieces recording start/stop needs for one-stream
/// sharing (N13), reachable from both the main wiring and the
/// player window's shared handles.
#[derive(Clone)]
struct ShareCtx {
    pending_shares: Rc<RefCell<std::collections::HashSet<String>>>,
    relay_worker: Rc<RelayWorker>,
}

/// One in-flight ffmpeg catch-up capture: everything needed to
/// finalize it (rename the staged `.part` on a clean, complete
/// finish) or retry it once when it comes up short (audit R05).
struct StagedDownload {
    final_path: PathBuf,
    channel: Channel,
    programme: Programme,
    url: String,
    /// N13: headers the direct-file probe verified for the
    /// (possibly redirected) URL; `launch_staged_capture` uses
    /// these instead of the channel's own when the probe found
    /// a direct file, so credentials never follow a redirect.
    headers: Option<iptv_core::playback::StreamHeaders>,
    /// N13: the direct-file probe is still running for this
    /// entry; the capture launches when its result lands.
    probing: bool,
    attempts: u32,
    /// N15: when a failed capture is waiting out its retry
    /// delay, the epoch at which the retry fires; the Cancel
    /// button can still abort the wait.
    retry_at: Option<f64>,
    /// N15: when the current capture attempt launched (for the
    /// Downloads window's Elapsed line).
    started_at: f64,
    /// N15: the current attempt's ffmpeg log, tailed for the
    /// written-seconds progress.
    log_path: Option<PathBuf>,
    /// N15: the programme seconds a complete capture writes
    /// (padded window minus padding), the progress denominator.
    expected_seconds: f64,
    /// N15: the failure detail block shown while a retry waits
    /// (Python's `show_failure` lines), cleared on relaunch.
    failure_detail: Option<String>,
    /// The user cancelled this capture: its finish removes the
    /// staged file and never retries (audit R06).
    cancelled: bool,
}

/// The Downloads window's handles, kept so the poll can refresh
/// its rows live (audit R06).
#[derive(Clone)]
struct DownloadsUi {
    frame: Frame,
    list: ListBox,
    dstatus: Status,
    /// N15: the detail block currently shown, so the 1 s tick
    /// only rewrites the field when a figure actually changed.
    details_text: String,
}

/// N15: the selected capture's detail block, in Python's order
/// (progress, elapsed, time remaining, downloaded) — one text
/// block a screen reader reads as a unit. While a retry waits,
/// the block is Python's failure detail instead.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // display figures, as in Python's floats
fn staged_detail_text(wiring: &Wiring, key: &str) -> Option<String> {
    let staged_map = wiring.staged_downloads.borrow();
    let staged = staged_map.get(key)?;
    if let Some(detail) = &staged.failure_detail {
        return Some(detail.clone());
    }
    let written = staged
        .log_path
        .as_deref()
        .and_then(iptv_core::recording_log::parse_ffmpeg_progress);
    let expected = staged.expected_seconds;
    let percent = written.map(|w| {
        if expected > 0.0 {
            format!("{}", (100.0 * w.min(expected) / expected).round() as i64)
        } else {
            "--".to_string()
        }
    });
    let elapsed = (staged.started_at > 0.0).then(|| (now_seconds() - staged.started_at).max(0.0));
    let remaining = written.map(|w| (expected - w).max(0.0));
    let size = std::fs::metadata(iptv_core::downloads::part_path(&staged.final_path))
        .map_or(0, |m| m.len());
    Some(
        [
            trf(
                "Progress: {percent}%",
                &[("percent", percent.as_deref().unwrap_or("--"))],
            ),
            trf(
                "Elapsed: {time}",
                &[("time", &iptv_core::recording_log::format_duration(elapsed))],
            ),
            trf(
                "Time remaining: {time}",
                &[(
                    "time",
                    &iptv_core::recording_log::format_duration(remaining),
                )],
            ),
            trf(
                "Downloaded: {size}",
                &[(
                    "size",
                    &iptv_core::recording_log::format_size(Some(size as f64)),
                )],
            ),
        ]
        .join("\n"),
    )
}

/// Escape closes secondary windows, as in Python: its
/// dialogs end with Cancel on Escape (`EVT_CHAR_HOOK` plus
/// `EndModal(ID_CANCEL)`) and its modeless windows hide.
/// Neither happens for free here: wx looks for a button
/// carrying the standard Cancel id, and these windows'
/// buttons are closures, so without this binding Escape
/// does nothing at all (Brandon's report, 2026-11-06).
/// `CHAR_HOOK` on the window itself fires whatever control
/// holds focus, exactly like Python's binding.
fn bind_escape_hide(frame: Frame) {
    frame.bind_internal(EventType::CHAR_HOOK, move |event| {
        if event.get_key_code() == Some(27) {
            frame.hide();
            event.skip(false);
        } else {
            event.skip(true);
        }
    });
}

/// The modal-dialog half of `bind_escape_hide`: Escape
/// ends the dialog with `ID_CANCEL`.
fn bind_escape_cancel(dialog: Dialog) {
    dialog.bind_internal(EventType::CHAR_HOOK, move |event| {
        if event.get_key_code() == Some(27) {
            dialog.end_modal(ID_CANCEL);
            event.skip(false);
        } else {
            event.skip(true);
        }
    });
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
    bind_escape_hide(frame);
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
            play_channel(
                &wiring_p.player,
                &wiring_p.persist,
                &wiring_p.media_probe,
                &channel,
                status,
            );
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
            add_dvr_job(&wiring_s, frame, &channel, &programme, status, "none");
        }
    });
    close.on_click(move |_| frame.hide());
}

fn filter_whats_on_rows(
    rows: &Rc<RefCell<Vec<(Channel, Programme)>>>,
    filtered: &Rc<RefCell<Vec<usize>>>,
    query: &str,
) {
    let query = query.trim().to_lowercase();
    let next: Vec<usize> = rows
        .borrow()
        .iter()
        .enumerate()
        .filter_map(|(index, (channel, programme))| {
            (query.is_empty()
                || programme.title.to_lowercase().contains(&query)
                || channel.name.to_lowercase().contains(&query))
            .then_some(index)
        })
        .collect();
    *filtered.borrow_mut() = next;
}

fn render_whats_on_rows(
    rows: &Rc<RefCell<Vec<(Channel, Programme)>>>,
    filtered: &Rc<RefCell<Vec<usize>>>,
    list: ListBox,
    count_label: StaticText,
) {
    let rows = rows.borrow();
    let filtered = filtered.borrow();
    let labels: Vec<String> = filtered
        .iter()
        .filter_map(|index| rows.get(*index))
        .map(|(channel, programme)| {
            format!(
                "{} — {} ({}-{})",
                channel.name,
                programme.title,
                local_hhmm(programme.start),
                local_hhmm(programme.stop)
            )
        })
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    if !labels.is_empty() {
        list.set_selection(0, true);
    }
    count_label.set_label(&trf(
        "{count} programs",
        &[("count", &filtered.len().to_string())],
    ));
}

/// File > What's on Now: every channel's current programme,
/// with Python's filter field (substring over programme title
/// or channel name), the "{count} programs" label, and Play /
/// Schedule Recording / Close.
fn open_whats_on_now(wiring: &Wiring, status: Status) {
    let (frame, panel, list, dstatus) = list_dialog("What's on Now", "Currently airing programs");
    let (filter, count_label, play, schedule, close) = whats_on_layout(panel, list, dstatus);

    let rows = Rc::new(RefCell::new(Vec::new()));
    let filtered = Rc::new(RefCell::new(Vec::new()));
    let render = {
        let rows = Rc::clone(&rows);
        let filtered = Rc::clone(&filtered);
        move |list: ListBox, count_label: StaticText| {
            render_whats_on_rows(&rows, &filtered, list, count_label);
        }
    };
    render(list, count_label);
    dstatus.set_label("Loading What's on Now...");
    let apply_filter = {
        let rows = Rc::clone(&rows);
        let filtered = Rc::clone(&filtered);
        let render = render.clone();
        move |list: ListBox, count_label: StaticText, query: &str| {
            let previous = filtered.borrow().clone();
            filter_whats_on_rows(&rows, &filtered, query);
            if *filtered.borrow() != previous {
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
    let model = wiring.browser.borrow().catalogue_model();
    let snapshot = wiring.epg.borrow().clone();
    submit_epg_read(
        wiring,
        iptv_core::epg::EpgReadKind::Snapshot {
            at: now_epoch(),
            model: Arc::clone(&model),
            snapshot,
        },
        PendingEpgReadUi::WhatsOnSnapshot {
            model,
            rows,
            filtered,
            filter,
            list,
            count_label,
            status: dstatus,
        },
    );
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
            // Resolution is by bookmark identity against the
            // current catalogue (P21): exactly one unambiguous
            // match, never a saved URL.
            let channel = iptv_core::bookmarks::find_current(key, &channels);
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
    dstatus.set_label(&trf(
        "{count} programmes to resume.",
        &[("count", &rows.len().to_string())],
    ));
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
    if let Some(channel) = iptv_core::bookmarks::find_current(&row.key, &fresh) {
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

/// N20: Python refuses a saved-position resume when the
/// player is not the built-in one or a cast is live
/// (main.py:6279) — the position would be silently dropped
/// otherwise. Shows the refusal and returns true when the
/// resume must not start.
fn resume_blocked_by_player_or_cast(wiring: &Wiring, parent: &Frame) -> bool {
    let (media_player, custom_path) = {
        let config = wiring.persist.config.borrow();
        (
            config.string_or("media_player", "Built-in Player"),
            config.string_or("custom_player_path", ""),
        )
    };
    let builtin = matches!(
        player_choice(&media_player, &custom_path),
        PlayerChoice::BuiltIn
    );
    if builtin && !cast_is_active(&wiring.cast_ui) {
        return false;
    }
    let dialog = MessageDialog::builder(
        parent,
        &tr("Saved positions can only be restored in the built-in player. Select it and disconnect casting before continuing."),
        &tr("Continue Watching"),
    )
    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
    .build();
    dialog.show_modal_tracked();
    true
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
                if resume_blocked_by_player_or_cast(&wiring_p, &frame) {
                    return;
                }
                play_channel(
                    &wiring_p.player,
                    &wiring_p.persist,
                    &wiring_p.media_probe,
                    &channel,
                    status,
                );
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
        if dialog.show_modal_tracked() != ID_YES {
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
        if dialog.show_modal_tracked() != ID_YES {
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

/// Resolve the persisted recent-channels keys against the
/// current catalogue (P17): later duplicate keys win; stored
/// keys with no current match stay persisted but are skipped
/// here, so renewed URLs resolve to the fresh entries.
fn resolved_recent(wiring: &Wiring) -> Vec<Channel> {
    let keys = wiring.persist.recent_keys();
    let channels = wiring.browser.borrow().all_channels();
    let mut by_key: BTreeMap<String, Channel> = BTreeMap::new();
    for channel in channels {
        let key = channel_key(&channel);
        if !key.is_empty() {
            by_key.insert(key, channel);
        }
    }
    keys.iter()
        .filter_map(|key| by_key.get(key).cloned())
        .collect()
}

/// View > Recently Watched...: the persisted history (P17),
/// resolved against the current catalogue.
fn open_recent_channels(wiring: &Wiring, status: Status) {
    let history = resolved_recent(wiring);
    if history.is_empty() {
        status.set_label("No channels watched yet.");
        return;
    }
    let (frame, panel, list, dstatus) =
        list_dialog("Recently Watched", "Recently watched channels");
    let labels: Vec<String> = history.iter().map(|c| c.name.clone()).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    list.set_selection(0, true);
    dstatus.set_label(&trf(
        "{count} recently watched channels.",
        &[("count", &history.len().to_string())],
    ));
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
            play_channel(
                &wiring_p.player,
                &wiring_p.persist,
                &wiring_p.media_probe,
                channel,
                status,
            );
            frame.hide();
        }
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The Reminders dialog (audit R13): rows carry time, title,
/// channel, delivery status and lead; the lead is editable,
/// and Clear All / Refresh / Open Channel are offered like
/// Python's. Selection survives a refresh by reminder id.
#[allow(clippy::too_many_lines)] // R13 dialog: lead editing, clear-all, status column
fn open_reminders_dialog(wiring: &Wiring, main_status: Status) {
    let (frame, panel, list, dstatus) = list_dialog("Reminders", "Reminders");
    let render = |wiring: &Wiring, list: ListBox, dstatus: Status| {
        let selected_id = list.get_selection().and_then(|index| {
            wiring
                .reminders
                .borrow()
                .get(usize::try_from(index).unwrap_or(usize::MAX))
                .map(|r| r.id.clone())
        });
        let now = now_epoch();
        let rows = wiring.reminders.borrow();
        let labels: Vec<String> = rows
            .iter()
            .map(|r| {
                let stamp = chrono::TimeZone::timestamp_opt(&chrono::Local, r.start, 0)
                    .single()
                    .map_or_else(
                        || "Unknown time".to_string(),
                        |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
                    );
                let title = if r.title.is_empty() {
                    "(No title)"
                } else {
                    &r.title
                };
                format!(
                    "{} {} — {} — {} — {} min before",
                    stamp,
                    title,
                    r.channel,
                    iptv_core::reminders::display_status(r, now),
                    r.lead
                )
            })
            .collect();
        list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(id) = &selected_id {
            if let Some(index) = rows.iter().position(|r| &r.id == id) {
                list.set_selection(u32::try_from(index).unwrap_or(0), true);
            }
        }
        if rows.is_empty() {
            dstatus.set_label("No reminders.");
        } else {
            dstatus.set_label(&trf(
                "{count} reminders.",
                &[("count", &rows.len().to_string())],
            ));
        }
    };
    render(wiring, list, dstatus);
    if wiring.reminders.borrow().is_empty() {
        main_status.set_label("No reminders set.");
    }
    let edit = Button::builder(&panel)
        .with_label("Set &Reminder...")
        .build();
    let delete = Button::builder(&panel).with_label("&Delete").build();
    let clear = Button::builder(&panel)
        .with_label("Clear &All Reminders")
        .build();
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let selected_reminder = move |wiring: &Wiring| -> Option<Reminder> {
        let index = usize::try_from(list.get_selection()?).ok()?;
        wiring.reminders.borrow().get(index).cloned()
    };
    let wiring_e = clone_wiring(wiring);
    edit.on_click(move |_| {
        let Some(reminder) = selected_reminder(&wiring_e) else {
            dstatus.set_label("Choose a reminder to edit first.");
            return;
        };
        if reminder.start <= now_epoch() {
            dstatus.set_label("That programme has already started.");
            return;
        }
        let Some(lead) = prompt_lead_minutes(frame, reminder.lead) else {
            return;
        };
        let now = now_epoch();
        let mut rows = wiring_e.reminders.borrow_mut();
        if iptv_core::reminders::set_lead(&mut rows, &reminder.id, lead, now).is_ok() {
            let saved = rows.clone();
            drop(rows);
            wiring_e.persist.save_reminders(&saved);
            // Editing re-arms delivery, like Python.
            wiring_e.reminder_seen.borrow_mut().remove(&reminder.id);
            render(&wiring_e, list, dstatus);
        } else {
            dstatus.set_label("That reminder can no longer be edited.");
        }
    });
    let wiring_d = clone_wiring(wiring);
    delete.on_click(move |_| {
        let Some(reminder) = selected_reminder(&wiring_d) else {
            dstatus.set_label("Choose a reminder to delete first.");
            return;
        };
        let mut rows = wiring_d.reminders.borrow_mut();
        rows.retain(|r| r.id != reminder.id);
        let saved = rows.clone();
        drop(rows);
        wiring_d.persist.save_reminders(&saved);
        render(&wiring_d, list, dstatus);
    });
    let wiring_a = clone_wiring(wiring);
    clear.on_click(move |_| {
        wiring_a.reminders.borrow_mut().clear();
        wiring_a.persist.save_reminders(&[]);
        render(&wiring_a, list, dstatus);
    });
    let wiring_r = clone_wiring(wiring);
    refresh.on_click(move |_| render(&wiring_r, list, dstatus));
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The recording-conflict decision (issue #134): opened only
/// when the user asks from Scheduled Recordings — the conflict
/// itself never pops a dialog or takes focus. Playback keeps
/// running until the user chooses otherwise.
fn open_conflict_dialog(wiring: &Wiring, job: &iptv_core::dvr::DvrJob, main_status: Status) {
    let name = if job.title.is_empty() {
        job.channel_name.clone()
    } else {
        job.title.clone()
    };
    let frame = Frame::builder()
        .with_title("Recording Conflict")
        .with_size(Size::new(560, 320))
        .build();
    frame.set_accessibility_label("Recording Conflict");
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    let text = StaticText::builder(&panel)
        .with_label(&format!(
            "{name} is waiting to record. {}\n\nPlayback continues unless you stop it. If you keep watching, the recording starts on its own when a connection becomes free, until its scheduled end.",
            job.wait_reason
        ))
        .build();
    text.set_accessibility_label("Conflict details");
    let keep = Button::builder(&panel)
        .with_label("&Keep Watching and Wait")
        .build();
    keep.set_accessibility_label("Keep watching and wait");
    let swap = Button::builder(&panel)
        .with_label("&Stop Playback and Record")
        .build();
    swap.set_accessibility_label("Stop playback and record");
    let cancel = Button::builder(&panel)
        .with_label("Cancel &This Recording")
        .build();
    cancel.set_accessibility_label("Cancel this recording");
    let close = Button::builder(&panel).with_label("C&lose").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&text, 0, SizerFlag::All, 8);
    sizer.add(&keep, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&swap, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&cancel, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&close, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let job_id = job.id.clone();
    let wiring_k = clone_wiring(wiring);
    let frame_k = frame;
    let name_k = name.clone();
    keep.on_click(move |_| {
        // The job stays waiting; the gate starts it when a
        // connection frees (issue #135's queue). The outcome is
        // announced once, here.
        player_status(
            main_status,
            &wiring_k.persist,
            &format!("Kept watching. {name_k} will record when a provider connection is free."),
            1,
        );
        frame_k.hide();
    });
    let wiring_s = clone_wiring(wiring);
    let frame_s = frame;
    let job_s = job_id.clone();
    swap.on_click(move |_| {
        // Confirm the previous connection closes before opening
        // its replacement: playback stops completely first, and
        // only then does the recording start.
        player_stop(&wiring_s.player, &wiring_s.persist, main_status);
        if wiring_s.player.borrow().session.current().is_some() {
            main_status.set_label("Playback did not stop; the recording stays waiting.");
            return;
        }
        let now = now_seconds();
        // Recheck time eligibility before the delayed action.
        let Some(job) = wiring_s.dvr.borrow().get(&job_s).cloned() else {
            return;
        };
        if job.stop_ts > 0.0 && now >= job.stop_ts {
            main_status.set_label("The recording window has closed; nothing was recorded.");
            frame_s.hide();
            return;
        }
        if job.status != iptv_core::dvr::STATUS_WAITING {
            main_status.set_label("That recording is no longer waiting.");
            frame_s.hide();
            return;
        }
        wiring_s
            .conflict_override
            .borrow_mut()
            .insert(job_s.clone());
        wiring_s.dvr.borrow_mut().mark_recording(&job_s, now);
        start_dvr_job(&wiring_s, main_status, &job_s, now);
        frame_s.hide();
    });
    let wiring_c2 = clone_wiring(wiring);
    let frame_c = frame;
    let name_c = name.clone();
    cancel.on_click(move |_| {
        if wiring_c2.dvr.borrow_mut().cancel_job(&job_id) {
            player_status(
                main_status,
                &wiring_c2.persist,
                &format!("Recording of {name_c} canceled."),
                1,
            );
        }
        frame_c.hide();
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The job ids behind the Scheduled Recordings list's
/// selected rows, in row order.
fn selected_job_ids(wiring: &Wiring, list: ListCtrl) -> Vec<String> {
    let dvr = wiring.dvr.borrow();
    let mut ids = Vec::new();
    let mut item: i64 = -1;
    loop {
        let next = list.get_next_item(item, ListNextItemFlag::All, ListItemState::Selected);
        if next < 0 {
            break;
        }
        if let Some(job) = dvr.jobs().get(usize::try_from(next).unwrap_or(usize::MAX)) {
            ids.push(job.id.clone());
        }
        item = i64::from(next);
    }
    ids
}

/// Render the Scheduled Recordings rows (audit R19): Python's
/// columns — the full padded window, title, channel, status
/// and format — with the selection preserved by job id across
/// the rebuild (waiting jobs carry their conflict reason in
/// the status column).
#[allow(clippy::cast_possible_truncation)] // epoch seconds to i64 for local stamps
fn render_scheduled_jobs(wiring: &Wiring, list: ListCtrl, dstatus: Status) {
    let selected = selected_job_ids(wiring, list);
    let dvr = wiring.dvr.borrow();
    list.delete_all_items();
    let stamp = |ts: f64| {
        chrono::TimeZone::timestamp_opt(&chrono::Local, ts as i64, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    };
    for (index, job) in dvr.jobs().iter().enumerate() {
        let row = i64::try_from(index).unwrap_or(0);
        let window = format!("{} - {}", stamp(job.start_ts), stamp(job.stop_ts));
        list.insert_item(row, &window, None);
        let title = if job.title.is_empty() {
            job.channel_name.clone()
        } else {
            job.title.clone()
        };
        list.set_item_text_by_column(row, 1, &title);
        list.set_item_text_by_column(row, 2, &job.channel_name);
        let status = if job.status == iptv_core::dvr::STATUS_WAITING && !job.wait_reason.is_empty()
        {
            format!("{}: {}", job.status, job.wait_reason)
        } else {
            job.status.clone()
        };
        list.set_item_text_by_column(row, 3, &status);
        let format_label = iptv_core::recording::RECORDING_FORMATS
            .iter()
            .find(|entry| entry.0 == job.format)
            .map_or(job.format.as_str(), |entry| entry.1);
        list.set_item_text_by_column(row, 4, format_label);
        if selected.contains(&job.id) {
            list.set_item_state(row, ListItemState::Selected, ListItemState::Selected);
        }
    }
    dstatus.set_label(&trf(
        "{count} scheduled recordings.",
        &[("count", &dvr.jobs().len().to_string())],
    ));
}

/// Cancel the first selected scheduled job (the Cancel Recording
/// button and the context menu share this; audit R19). Cancelling
/// also stops the capture: a canceled job leaves the Recording
/// state, so the scheduler will never emit its Stop action
/// (issue #130).
fn cancel_first_selected_job(wiring: &Wiring, list: ListCtrl, dstatus: Status) {
    let Some(id) = selected_job_ids(wiring, list).into_iter().next() else {
        dstatus.set_label("Choose a recording to cancel first.");
        return;
    };
    let mut dvr = wiring.dvr.borrow_mut();
    if dvr.cancel_job(&id) {
        dvr.save();
        drop(dvr);
        wiring.recordings.borrow_mut().stop(&format!("dvr:{id}"));
        wiring.dvr_resolve.borrow_mut().remove(&id);
        wiring
            .pending_resolves
            .borrow_mut()
            .retain(|_, pending| !matches!(pending, PendingResolve::Dvr(job) if job == &id));
        render_scheduled_jobs(wiring, list, dstatus);
    }
}

/// Delete every selected finished job (the Delete button and
/// the Delete key share this; audit R19).
fn delete_selected_finished_jobs(wiring: &Wiring, list: ListCtrl, dstatus: Status) {
    let ids = selected_job_ids(wiring, list);
    if ids.is_empty() {
        dstatus.set_label("Choose a finished recording to delete first.");
        return;
    }
    let mut removed = 0;
    {
        let mut dvr = wiring.dvr.borrow_mut();
        for id in &ids {
            if dvr.remove_job(id) {
                removed += 1;
            }
        }
        if removed > 0 {
            dvr.save();
        }
    }
    if removed == 0 {
        dstatus.set_label("Only finished recordings can be deleted.");
    }
    render_scheduled_jobs(wiring, list, dstatus);
}

/// The Scheduled Recordings dialog (audit R19): a multi-select
/// report list with Python's columns, Ctrl+A / Delete keyboard
/// batch operations, and selection-preserving refreshes.
#[allow(clippy::too_many_lines)] // R19 dialog: report list, keyboard actions
fn open_scheduled_dialog(wiring: &Wiring, main_status: Status) {
    let frame = Frame::builder()
        .with_title("Scheduled Recordings")
        .with_size(Size::new(850, 430))
        .build();
    frame.set_accessibility_label("Scheduled Recordings");
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    let list = ListCtrl::builder(&panel)
        .with_style(ListCtrlStyle::Report)
        .build();
    list.set_accessibility_label("Scheduled recordings");
    list.insert_column(0, "Time", ListColumnFormat::Left, 210);
    list.insert_column(1, "Title", ListColumnFormat::Left, 230);
    list.insert_column(2, "Channel", ListColumnFormat::Left, 180);
    list.insert_column(3, "Status", ListColumnFormat::Left, 110);
    list.insert_column(4, "Format", ListColumnFormat::Left, 120);
    let status_label = StaticText::builder(&panel)
        .with_label("Dialog status")
        .build();
    let details = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::MultiLine)
        .with_value("No items yet.")
        .build();
    details.set_accessibility_label("Dialog status");
    let dstatus = Status {
        label: status_label,
        details,
        bar: None,
    };
    let render = render_scheduled_jobs;
    render(wiring, list, dstatus);
    if wiring.dvr.borrow().jobs().is_empty() {
        main_status.set_label("No scheduled recordings.");
    }
    let cancel_job = Button::builder(&panel)
        .with_label("&Cancel Recording")
        .build();
    let resolve_conflict = Button::builder(&panel)
        .with_label("&Resolve Conflict...")
        .build();
    resolve_conflict.set_accessibility_label("Resolve recording conflict");
    let delete_job = Button::builder(&panel).with_label("&Delete").build();
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_c = clone_wiring(wiring);
    cancel_job.on_click(move |_| {
        cancel_first_selected_job(&wiring_c, list, dstatus);
    });
    let wiring_x = clone_wiring(wiring);
    resolve_conflict.on_click(move |_| {
        let Some(id) = selected_job_ids(&wiring_x, list).into_iter().next() else {
            dstatus.set_label("Choose a waiting recording first.");
            return;
        };
        let job = wiring_x.dvr.borrow().get(&id).cloned();
        match job {
            Some(job) if job.status == iptv_core::dvr::STATUS_WAITING => {
                open_conflict_dialog(&wiring_x, &job, main_status);
                render(&wiring_x, list, dstatus);
            }
            Some(_) => dstatus.set_label("That recording is not waiting on a conflict."),
            None => dstatus.set_label("Choose a waiting recording first."),
        }
    });
    let wiring_d = clone_wiring(wiring);
    delete_job.on_click(move |_| {
        delete_selected_finished_jobs(&wiring_d, list, dstatus);
    });
    let wiring_r = clone_wiring(wiring);
    refresh.on_click(move |_| {
        render(&wiring_r, list, dstatus);
    });
    // Keyboard batch operations (Python's list keys): Ctrl+A
    // selects every row; Delete removes the selected finished
    // jobs. Matched keys are consumed.
    let wiring_k = clone_wiring(wiring);
    list.bind_internal(EventType::KEY_DOWN, move |event| {
        if key_event_label(event) == Some("Ctrl+A".to_string()) {
            let count = list.get_item_count();
            for row in 0..i64::from(count) {
                list.set_item_state(row, ListItemState::Selected, ListItemState::Selected);
            }
            event.skip(false);
            return;
        }
        if event.get_key_code() == Some(127) {
            delete_selected_finished_jobs(&wiring_k, list, dstatus);
            event.skip(false);
            return;
        }
        event.skip(true);
    });
    // The right-click / Apps-key menu (Python's Scheduled
    // Recordings context menu, audit R19): Refresh always; Cancel
    // and Delete once the list has rows. Selections arrive as menu
    // events on the dialog frame.
    list.bind_internal(EventType::CONTEXT_MENU, move |_| {
        let mut menu = Menu::builder().build();
        menu.append(DVR_CTX_REFRESH, &tr("Refresh"), "", ItemKind::Normal);
        if list.get_item_count() > 0 {
            menu.append_separator();
            menu.append(DVR_CTX_CANCEL, &tr("Cancel"), "", ItemKind::Normal);
            menu.append(DVR_CTX_DELETE, &tr("Delete\tDel"), "", ItemKind::Normal);
        }
        list.popup_menu(&mut menu, None);
    });
    let wiring_m = clone_wiring(wiring);
    frame.on_menu(move |event| match event.get_id() {
        DVR_CTX_REFRESH => render_scheduled_jobs(&wiring_m, list, dstatus),
        DVR_CTX_CANCEL => cancel_first_selected_job(&wiring_m, list, dstatus),
        DVR_CTX_DELETE => delete_selected_finished_jobs(&wiring_m, list, dstatus),
        _ => {}
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
}

/// The Catch-up dialog for one channel: past programmes with Play
/// and Download.
#[allow(clippy::too_many_lines)] // R20 dialog: descriptions, activation, context menu
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
    let (frame, panel, list, dstatus) = list_dialog(
        &format!("Catch-up — {}", channel.name),
        "Catch-up programmes",
    );
    // N24: Python heads the dialog with this explanation.
    let intro = StaticText::builder(&panel)
        .with_label("Select a programme, then press Enter to open it, or use the context menu to download it. Press Escape to close.")
        .build();
    let _ = intro;
    // Audit R20: rows carry the DATE as well as the clock, so
    // same-time episodes on different days are distinguishable.
    let labels: Vec<String> = programmes
        .iter()
        .map(|p| {
            let day = chrono::TimeZone::timestamp_opt(&chrono::Local, p.start, 0)
                .single()
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            format!(
                "{} {}-{} {}",
                day,
                local_hhmm(p.start),
                local_hhmm(p.stop),
                p.title
            )
        })
        .collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    if !programmes.is_empty() {
        list.set_selection(0, true);
        dstatus.set_label(&trf(
            "{count} catch-up programmes.",
            &[("count", &programmes.len().to_string())],
        ));
    }
    let programmes = Rc::new(RefCell::new(programmes));
    // The selected programme's description is readable in the
    // details text (audit R20), starting with the first row.
    let describe = |programmes: &Rc<RefCell<Vec<Programme>>>, list: ListBox, dstatus: Status| {
        let index = usize::try_from(list.get_selection().unwrap_or(0)).unwrap_or(0);
        if let Some(programme) = programmes.borrow().get(index) {
            let text = if programme.description.trim().is_empty() {
                "No description for this programme.".to_string()
            } else {
                programme.description.clone()
            };
            dstatus.details.set_value(&text);
        }
    };
    describe(&programmes, list, dstatus);
    let programmes_sel = Rc::clone(&programmes);
    list.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
        describe(&programmes_sel, list, dstatus);
    });
    let play = Button::builder(&panel).with_label("&Play").build();
    let download = Button::builder(&panel).with_label("&Download").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let selected_programme = move |programmes: &Rc<RefCell<Vec<Programme>>>| -> Option<Programme> {
        let index = usize::try_from(list.get_selection()?).ok()?;
        programmes.borrow().get(index).cloned()
    };
    let wiring_p = clone_wiring(wiring);
    let programmes_p = Rc::clone(&programmes);
    let channel_p = channel.clone();
    play.on_click(move |_| {
        if let Some(programme) = selected_programme(&programmes_p) {
            // The dialog stays open with its selection in
            // place, so closing the player returns to the
            // played programme (audit R20).
            play_catchup_for(&wiring_p, &channel_p, &programme, main_status);
        }
    });
    // Enter (double-click) activates the row, like Python.
    let wiring_a = clone_wiring(wiring);
    let programmes_a = Rc::clone(&programmes);
    let channel_a = channel.clone();
    list.bind_internal(EventType::COMMAND_LISTBOX_DOUBLECLICKED, move |_| {
        if let Some(programme) = selected_programme(&programmes_a) {
            play_catchup_for(&wiring_a, &channel_a, &programme, main_status);
        }
    });
    let wiring_d = clone_wiring(wiring);
    let programmes_d = Rc::clone(&programmes);
    let channel_d = channel.clone();
    download.on_click(move |_| {
        let Some(programme) = selected_programme(&programmes_d) else {
            return;
        };
        start_catchup_download(&wiring_d, &channel_d, &programme, dstatus);
    });
    // The Apps-key context menu mirrors the buttons.
    list.bind_internal(EventType::CONTEXT_MENU, move |_| {
        let mut menu = Menu::builder()
            .append_item(9901, "Play", "")
            .append_item(9902, "Download", "")
            .build();
        list.popup_menu(&mut menu, None);
    });
    let wiring_f = clone_wiring(wiring);
    let programmes_f = Rc::clone(&programmes);
    let channel_f = channel.clone();
    frame.on_menu(move |event| {
        let Some(programme) = selected_programme(&programmes_f) else {
            return;
        };
        match event.get_id() {
            9901 => play_catchup_for(&wiring_f, &channel_f, &programme, main_status),
            9902 => start_catchup_download(&wiring_f, &channel_f, &programme, dstatus),
            _ => {}
        }
    });
    close.on_click(move |_| frame.hide());
    frame.show(true);
    if programmes.borrow().is_empty() {
        dstatus.set_label("Loading catch-up programmes...");
        let now = now_epoch();
        submit_epg_read(
            wiring,
            iptv_core::epg::EpgReadKind::Programmes {
                channel: channel.clone(),
                from: now - 7 * 86_400,
                to: now + 7 * 86_400,
            },
            PendingEpgReadUi::CatchupProgrammes {
                channel: channel.clone(),
                list,
                programmes: Rc::clone(&programmes),
                status: dstatus,
            },
        );
    }
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

/// The identity of one catch-up download (audit R03): the
/// channel plus the programme window, like Python's hashed
/// recorder key. A second request for the same programme is
/// refused while one is active.
fn catchup_download_key(channel: &Channel, programme: &Programme) -> String {
    format!(
        "catchup:{}:{}:{}",
        channel_key(channel),
        programme.start,
        programme.stop
    )
}

/// The programme window widened by the configured schedule
/// padding (audit R04; Python `_padded_catchup_window`).
fn padding_minutes(wiring: &Wiring) -> (i64, i64) {
    let config = wiring.persist.config.borrow();
    let pre = config
        .value("recording_pre_padding_minutes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0);
    let post = config
        .value("recording_post_padding_minutes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2)
        .max(0);
    (pre, post)
}

fn padded_window(wiring: &Wiring, programme: &Programme) -> (i64, i64) {
    let (pre, post) = padding_minutes(wiring);
    (programme.start - pre * 60, programme.stop + post * 60)
}

/// Total padding inside a padded window, in seconds (N13:
/// Python's truncation expectation subtracts it back out).
fn padding_seconds(wiring: &Wiring) -> i64 {
    let (pre, post) = padding_minutes(wiring);
    (pre + post) * 60
}

/// Stage a catch-up capture for its direct-file probe (N13):
/// the probe worker walks the padded window first, then the
/// programme-exact one; `poll_direct_probes` launches the
/// capture from the result. Used for the first attempt and for
/// N15 retries, which re-run the whole probe flow like Python.
fn stage_direct_probe(wiring: &Wiring, key: &str, mut staged: StagedDownload) {
    let (padded_start, padded_stop) = padded_window(wiring, &staged.programme);
    let programme = staged.programme.clone();
    let url = staged.url.clone();
    let headers = iptv_core::playback::StreamHeaders::for_channel(&staged.channel);
    staged.probing = true;
    wiring
        .staged_downloads
        .borrow_mut()
        .insert(key.to_string(), staged);
    wiring.direct_probe_pending.set(true);
    wiring
        .direct_probe_worker
        .start(iptv_core::catchup_direct::DirectProbeRequest {
            key: key.to_string(),
            url,
            windows: vec![
                (padded_start, padded_stop - padded_start),
                (programme.start, programme.stop - programme.start),
            ],
            headers,
        });
}

/// Start a catch-up download once its URL is known (directly, or
/// after a portal resolution).
#[allow(clippy::too_many_lines)] // R02-R05 staged download flow
fn begin_catchup_download(
    wiring: &Wiring,
    channel: &Channel,
    programme: &Programme,
    url: String,
    status: Status,
) {
    // Audit R08: the destination check gates downloads too —
    // before any provider connection is opened.
    let dir = recordings_dir(&wiring.persist);
    let reserve = wiring
        .persist
        .config
        .borrow()
        .value("recording_min_free_mib")
        .and_then(serde_json::Value::as_i64);
    if let Err(error) = recording::check_destination(&dir, reserve) {
        status.set_label(&error);
        return;
    }
    // N16: a download session preempts classification probes
    // like a playback or recording session.
    terminate_media_probes(&wiring.media_probe);
    // Audit R03: refuse a duplicate active capture of the same
    // programme instead of overwriting its output.
    let key = catchup_download_key(channel, programme);
    let duplicate = wiring.recordings.borrow().is_recording(&key)
        || wiring
            .downloads
            .borrow()
            .iter()
            .any(|row| !row.done && row.key == key);
    if duplicate {
        status.set_label("This catch-up programme is already downloading.");
        return;
    }
    // N13: on a one-stream provider whose only connection is
    // in use, refuse before any probing — the probes are media
    // requests too and would consume the slot ffmpeg needs.
    let cast_url = cast_source_url(&wiring.cast_ui);
    if single_stream_busy(
        &wiring.recordings,
        &wiring.buffers,
        Some(&wiring.player),
        &url,
        true,
        cast_url.as_deref(),
    ) {
        status.set_label(iptv_core::catchup_direct::ONE_STREAM_REFUSAL);
        return;
    }
    // Audit R04: ask the provider for the padded window,
    // falling back to the programme-exact URL the caller
    // resolved when the padded one cannot be built.
    let (padded_start, padded_stop) = padded_window(wiring, programme);
    let now = now_epoch();
    let url = match iptv_core::catchup::resolve_show_url(channel, padded_start, padded_stop, now) {
        Ok(
            iptv_core::catchup::ShowResolution::Catchup(padded)
            | iptv_core::catchup::ShowResolution::Live(padded),
        ) => padded,
        _ => url,
    };
    // Naming (R03): title - channel, stamped with the airtime
    // to the minute, collision-free against disk and in-flight
    // work. The extension follows the recording preset (R04).
    let format = recording_format_for_channel(&wiring.persist, &wiring.media_probe.cache, channel);
    let extension = recording::format_extension(&format);
    let display_name = format!("{} - {}", programme.title, channel.name);
    let stamp = chrono::TimeZone::timestamp_opt(&chrono::Local, programme.start, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H-%M").to_string())
        .unwrap_or_default();
    let mut in_use = wiring.recordings.borrow().in_use_paths();
    in_use.extend(
        wiring
            .downloads
            .borrow()
            .iter()
            .filter(|row| !row.done)
            .map(|row| PathBuf::from(&row.destination)),
    );
    let destination =
        recording::unique_output_path(&dir, &display_name, extension, &stamp, &in_use);
    match iptv_core::downloads::archive_plan(&url, None) {
        // Audit R02: a playlist is not media — capture it with
        // ffmpeg for the finite window instead of saving the
        // manifest text under a media extension.
        iptv_core::downloads::ArchivePlan::FfmpegCapture => {
            // N13 (Python's catch-up direct flow): before the
            // paced HLS capture, probe for the provider's direct
            // file on the probe worker — padded window first,
            // programme-exact second, in one walk. The probe
            // never opens the media itself and settles a
            // one-stream provider before ffmpeg asks; the
            // capture launches from `poll_direct_probes`.
            let staged = StagedDownload {
                final_path: destination.clone(),
                channel: channel.clone(),
                programme: programme.clone(),
                url,
                headers: None,
                probing: true,
                attempts: 1,
                retry_at: None,
                started_at: 0.0,
                log_path: None,
                expected_seconds: 0.0,
                failure_detail: None,
                cancelled: false,
            };
            wiring.downloads.borrow_mut().push(DownloadRow {
                id: 0,
                key: key.clone(),
                name: programme.title.clone(),
                destination: destination.display().to_string(),
                progress: "Checking for a faster download...".to_string(),
                done: false,
            });
            stage_direct_probe(wiring, &key, staged);
        }
        iptv_core::downloads::ArchivePlan::Direct => {
            let id = wiring.next_download_id.get();
            wiring.next_download_id.set(id + 1);
            wiring
                .download_worker
                .start(iptv_core::downloads::DownloadRequest {
                    id,
                    url,
                    headers: iptv_core::downloads::channel_headers(channel),
                    destination: destination.clone(),
                });
            wiring.downloads.borrow_mut().push(DownloadRow {
                id,
                key,
                name: programme.title.clone(),
                destination: destination.display().to_string(),
                progress: "Starting...".to_string(),
                done: false,
            });
        }
    }
    wiring.downloads_active.set(true);
    assert!(wiring.timer.start(50, false));
    status.set_label(&trf(
        "Downloading {title}...",
        &[("title", &programme.title)],
    ));
}

/// Launch (or relaunch) one staged ffmpeg catch-up capture:
/// the media lands in a `.part` sibling and only earns the
/// final name on a clean, complete finish (audit R05).
fn launch_staged_capture(
    wiring: &Wiring,
    key: &str,
    mut staged: StagedDownload,
) -> Result<(), String> {
    let format =
        recording_format_for_channel(&wiring.persist, &wiring.media_probe.cache, &staged.channel);
    let extension = recording::format_extension(&format);
    let part = iptv_core::downloads::part_path(&staged.final_path);
    let (padded_start, padded_stop) = padded_window(wiring, &staged.programme);
    // N15: the Downloads window's live figures read this
    // attempt's log and clock.
    staged.started_at = now_seconds();
    staged.log_path = part
        .parent()
        .map(|parent| iptv_core::recording_log::recording_log_path(parent, &part));
    staged.expected_seconds = f64::from(
        i32::try_from(((padded_stop - padded_start) - padding_seconds(wiring)).max(1)).unwrap_or(0),
    );
    staged.failure_detail = None;
    staged.retry_at = None;
    let headers = staged
        .headers
        .clone()
        .unwrap_or_else(|| iptv_core::playback::StreamHeaders::for_channel(&staged.channel));
    let witness = player_audio_witness(&wiring.player, &staged.channel);
    let (audio_track, audio_track_count) = recording_audio_choice(
        &wiring.persist,
        &staged.channel,
        &staged.url,
        &headers,
        witness,
    );
    let cmd = recording::ffmpeg_command(
        "ffmpeg",
        &staged.url,
        &part.to_string_lossy(),
        &format,
        &headers,
        &recording::FfmpegOptions {
            duration_seconds: Some(f64::from(
                i32::try_from(padded_stop - padded_start).unwrap_or(0),
            )),
            force_format: recording::format_muxer(extension).map(str::to_string),
            show_stats: true,
            log_datetime: iptv_core::recording_log::ffmpeg_supports_log_datetime("ffmpeg"),
            audio_track,
            audio_track_count,
            ..recording::FfmpegOptions::default()
        },
    );
    let meta = recording::RecordingStartMeta {
        title: staged.programme.title.clone(),
        format: format.clone(),
        planned_seconds: Some(f64::from(
            i32::try_from(padded_stop - padded_start).unwrap_or(0),
        )),
    };
    wiring
        .recordings
        .borrow_mut()
        .start(key, &cmd, part, meta)?;
    wiring.recordings.borrow_mut().set_connection_key(
        key,
        &iptv_core::catchup_direct::single_stream_provider_key(&staged.url),
    );
    wiring
        .staged_downloads
        .borrow_mut()
        .insert(key.to_string(), staged);
    Ok(())
}

/// N15: fire any catch-up download retries whose delay has
/// elapsed (the housekeeping tick calls this every second).
/// A retry re-runs the whole probe flow, like Python, so a
/// provider-stale direct URL is derived fresh.
fn process_download_retries(wiring: &Wiring) {
    let now = now_seconds();
    let due: Vec<String> = wiring
        .staged_downloads
        .borrow()
        .iter()
        .filter(|(_, staged)| staged.retry_at.is_some_and(|at| at <= now))
        .map(|(key, _)| key.clone())
        .collect();
    for key in due {
        let Some(mut staged) = wiring.staged_downloads.borrow_mut().remove(&key) else {
            continue;
        };
        if staged.cancelled {
            continue;
        }
        staged.retry_at = None;
        let mut rows = wiring.downloads.borrow_mut();
        if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
            row.progress = "Checking for a faster download...".to_string();
        }
        drop(rows);
        stage_direct_probe(wiring, &key, staged);
        refresh_downloads_window(wiring);
    }
}

/// N15: Python's `show_failure` detail block for a failed
/// download: the plain-words reason (when the exit code has
/// one), the code itself, and the log's first diagnostic line.
fn download_failure_detail(
    staged: &StagedDownload,
    finished: &recording::RecordingFinished,
) -> Option<String> {
    if finished.success {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    if let Some(reason) = iptv_core::catchup::ffmpeg_exit_reason(i64::from(finished.exit_code)) {
        lines.push(reason.to_string());
    }
    lines.push(trf(
        "ffmpeg exit code: {code}",
        &[("code", &finished.exit_code.to_string())],
    ));
    let first = staged
        .log_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| iptv_core::recording_log::first_error_line(&text))
        .unwrap_or_default();
    if first.is_empty() {
        lines.push(tr("No error details were reported."));
    } else {
        lines.push(first);
    }
    Some(lines.join("\n"))
}

/// N15: schedule the retry of a transiently failed download
/// (Python `_maybe_retry_catchup_download`): the partial file
/// goes, the row counts down the doubling delay (2 s, 4 s,
/// 8 s), the failure detail stays on view, and
/// `process_download_retries` re-runs the probe flow when the
/// delay elapses. Cancel can still abort the wait.
fn schedule_download_retry(
    wiring: &Wiring,
    key: &str,
    staged: StagedDownload,
    finished: &recording::RecordingFinished,
) {
    let _ = std::fs::remove_file(&finished.out_path);
    let delay = iptv_core::catchup::CATCHUP_RETRY_BASE_DELAY_SECONDS
        * 2_f64.powi(i32::try_from(staged.attempts - 1).unwrap_or(0));
    let failure_detail = download_failure_detail(&staged, finished);
    let mut rows = wiring.downloads.borrow_mut();
    if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
        row.progress = trf(
            "Download failed - retrying (attempt {attempt} of {total}) in {seconds} seconds...",
            &[
                ("attempt", &staged.attempts.to_string()),
                (
                    "total",
                    &iptv_core::catchup::CATCHUP_RETRY_MAX_ATTEMPTS.to_string(),
                ),
                ("seconds", &format!("{delay:.0}")),
            ],
        );
    }
    drop(rows);
    let mut retry = staged;
    retry.attempts += 1;
    retry.failure_detail = failure_detail;
    retry.retry_at = Some(now_seconds() + delay);
    wiring
        .staged_downloads
        .borrow_mut()
        .insert(key.to_string(), retry);
}

/// Finalize one staged catch-up capture (audit R05): a clean
/// finish with the whole programme renames the `.part` into
/// place; a short or failed capture retries under Python's
/// rule (N15: up to three retries, doubling delay, only for
/// retryable failures), then reports honestly — a truncated
/// final attempt keeps its partial file like Python, a failed
/// one leaves nothing misleading behind.
fn finish_staged_download(
    wiring: &Wiring,
    frame: Frame,
    status: Status,
    key: &str,
    staged: StagedDownload,
    finished: &recording::RecordingFinished,
    produced: bool,
) {
    // N13: Python's truncation expectation is the padded
    // window minus its padding (the programme itself), and a
    // capture counts as truncated below 95% of it
    // (`_catchup_download_is_truncated`). N12: the written
    // seconds are ffmpeg's own reported progress from the
    // recording log (Python's `media_written_seconds`); probing
    // the produced file is only the fallback for a capture
    // whose log carried no stats.
    let (padded_start, padded_stop) = padded_window(wiring, &staged.programme);
    let expected_seconds = ((padded_stop - padded_start) - padding_seconds(wiring)).max(1);
    let expected = f64::from(i32::try_from(expected_seconds).unwrap_or(0));
    let written = finished
        .media_written_seconds
        .or_else(|| media_duration_seconds(&finished.out_path));
    let truncated =
        finished.success && produced && written.is_some_and(|actual| actual < expected * 0.95);
    let complete = !staged.cancelled && finished.success && produced && !truncated;
    if staged.cancelled {
        let _ = std::fs::remove_file(&finished.out_path);
        let mut rows = wiring.downloads.borrow_mut();
        if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
            row.progress = "Cancelled".to_string();
            row.done = true;
        }
        drop(rows);
        let active = wiring.downloads.borrow().iter().any(|row| !row.done);
        wiring.downloads_active.set(active);
        refresh_downloads_window(wiring);
        return;
    }
    let set_row = |wiring: &Wiring, progress: String, done: bool| {
        let mut rows = wiring.downloads.borrow_mut();
        if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
            row.progress = progress;
            row.done = done;
        }
    };
    if complete && std::fs::rename(&finished.out_path, &staged.final_path).is_ok() {
        set_row(wiring, "Done".to_string(), true);
        status.set_label(&format!("Download finished: {}", staged.programme.title));
        // N15: Python confirms a finished download in a box
        // (queued behind any open modal, N18).
        let path = staged.final_path.to_string_lossy().into_owned();
        show_or_queue_notification(
            frame,
            &trf("Download complete:\n{path}", &[("path", &path)]),
            &tr("Catch-up Download"),
            MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
            None,
        );
    } else if staged.attempts <= iptv_core::catchup::CATCHUP_RETRY_MAX_ATTEMPTS
        && iptv_core::catchup::catchup_failure_is_retryable(
            finished.exit_code,
            truncated,
            &finished.stderr_tail.join("\n"),
            finished.stopped_by_user,
        )
    {
        schedule_download_retry(wiring, key, staged, finished);
    } else if truncated {
        finish_truncated_download(wiring, frame, status, key, &staged, finished, written);
    } else {
        let _ = std::fs::remove_file(&finished.out_path);
        let reason = if finished.success {
            "Failed: nothing was recorded"
        } else {
            "Failed"
        };
        set_row(wiring, reason.to_string(), true);
        status.set_label(&format!("Download failed: {}", staged.programme.title));
        if !finished.success {
            report_download_failure(frame, &staged, finished);
        }
    }
    let active = wiring.downloads.borrow().iter().any(|row| !row.done);
    wiring.downloads_active.set(active);
    refresh_downloads_window(wiring);
}

/// N15: a truncated download whose retry budget is spent keeps
/// its partial file; the report says how much of the programme
/// actually arrived (Python's ended-early box).
fn finish_truncated_download(
    wiring: &Wiring,
    frame: Frame,
    status: Status,
    key: &str,
    staged: &StagedDownload,
    finished: &recording::RecordingFinished,
    written: Option<f64>,
) {
    let (padded_start, padded_stop) = padded_window(wiring, &staged.programme);
    let expected = f64::from(
        i32::try_from(((padded_stop - padded_start) - padding_seconds(wiring)).max(1)).unwrap_or(0),
    );
    let _ = std::fs::rename(&finished.out_path, &staged.final_path);
    let mut rows = wiring.downloads.borrow_mut();
    if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
        row.progress = "Ended early".to_string();
        row.done = true;
    }
    drop(rows);
    status.set_label(&format!("Download ended early: {}", staged.programme.title));
    let path = staged.final_path.to_string_lossy().into_owned();
    let message = trf(
        "Download ended early:\n{path}\n\nOnly {written} of {expected} arrived before the provider closed the connection. The incomplete file was kept.",
        &[
            ("path", &path),
            (
                "written",
                &iptv_core::recording_log::format_duration(written),
            ),
            (
                "expected",
                &iptv_core::recording_log::format_duration(Some(expected)),
            ),
        ],
    );
    show_or_queue_notification(
        frame,
        &message,
        &tr("Catch-up Download"),
        MessageDialogStyle::OK | MessageDialogStyle::IconWarning,
        None,
    );
}

/// N14: Python explains a failed download in plain words when
/// the ffmpeg exit code is a known provider failure
/// (`_ffmpeg_exit_reason`), with the failure detail after it;
/// unknown codes keep the numeric form.
fn report_download_failure(
    frame: Frame,
    staged: &StagedDownload,
    finished: &recording::RecordingFinished,
) {
    let tail_start = finished.stderr_tail.len().saturating_sub(6);
    let tail = &finished.stderr_tail[tail_start..];
    let detail = if tail.is_empty() {
        recording_failure_detail(finished)
    } else {
        tail.join("\n")
    };
    let path = staged.final_path.to_string_lossy().into_owned();
    let message = match iptv_core::catchup::ffmpeg_exit_reason(i64::from(finished.exit_code)) {
        Some(reason) => trf(
            "Download failed:\n{path}\n\n{reason}\n\n{detail}",
            &[("path", &path), ("reason", reason), ("detail", &detail)],
        ),
        None => trf(
            "Download failed (code {code}):\n{path}\n\n{detail}",
            &[
                ("code", &finished.exit_code.to_string()),
                ("path", &path),
                ("detail", &detail),
            ],
        ),
    };
    show_or_queue_notification(
        frame,
        &message,
        &tr("Catch-up Download"),
        MessageDialogStyle::OK | MessageDialogStyle::IconError,
        None,
    );
}

/// Render the Downloads rows into a list (shared by the window
/// builder and the live refresh, audit R06).
fn download_row_labels(wiring: &Wiring) -> Vec<String> {
    wiring
        .downloads
        .borrow()
        .iter()
        .map(|row| format!("{} — {} — {}", row.name, row.progress, row.destination))
        .collect()
}

/// Refresh the open Downloads window from the tracked rows,
/// keeping the selection where it was (audit R06: the window
/// is live, not a snapshot).
fn refresh_downloads_window(wiring: &Wiring) {
    let Some(mut ui) = wiring.downloads_ui.borrow().as_ref().cloned() else {
        return;
    };
    let selected = ui.list.get_selection();
    let labels = download_row_labels(wiring);
    ui.list
        .set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    if let Some(index) = selected {
        if (index as usize) < labels.len() {
            ui.list.set_selection(index, true);
        }
    }
    ui.dstatus.set_label(&trf(
        "{count} downloads.",
        &[("count", &labels.len().to_string())],
    ));
    // N15: the selected capture's live detail block under the
    // rows (progress, elapsed, remaining, size so far).
    if let Some(index) = selected {
        let key = wiring
            .downloads
            .borrow()
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .map(|row| row.key.clone());
        if let Some(text) = key.and_then(|key| staged_detail_text(wiring, &key)) {
            if ui.details_text != text {
                ui.dstatus.details.set_value(&text);
                ui.details_text = text;
                *wiring.downloads_ui.borrow_mut() = Some(ui);
            }
        }
    }
}

/// View > Show Downloads: tracked downloads with live progress
/// and per-request cancellation (audit R06).
fn open_downloads_window(wiring: &Wiring, status: Status) {
    if wiring.downloads.borrow().is_empty() {
        status.set_label("No downloads yet. Downloads start from a channel's Catch-up dialog.");
        return;
    }
    if let Some(ui) = wiring.downloads_ui.borrow().as_ref().cloned() {
        refresh_downloads_window(wiring);
        ui.frame.show(true);
        ui.frame.raise();
        return;
    }
    let (frame, panel, list, dstatus) = list_dialog("Downloads", "Downloads");
    let labels = download_row_labels(wiring);
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    dstatus.set_label(&trf(
        "{count} downloads.",
        &[("count", &labels.len().to_string())],
    ));
    *wiring.downloads_ui.borrow_mut() = Some(DownloadsUi {
        frame,
        list,
        dstatus,
        details_text: String::new(),
    });
    let cancel = Button::builder(&panel)
        .with_label("&Cancel Download")
        .build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let wiring_c = clone_wiring(wiring);
    cancel.on_click(move |_| {
        cancel_selected_download(&wiring_c, frame, list, dstatus);
    });
    close.on_click(move |_| frame.hide());
    // N15: Escape hides the window while downloads continue
    // (Python's download dialog rule); Show Downloads brings
    // it back. Bound by list_dialog's bind_escape_hide.
    frame.show(true);
}

/// The Downloads window's Cancel Download button: acts on the
/// SELECTED request only; other downloads, running or queued,
/// are untouched, and starting another download never revives
/// this one. N15: cancelling asks first — a cancelled download
/// cannot be resumed (Python's Cancel Download box).
fn cancel_selected_download(wiring: &Wiring, frame: Frame, list: ListBox, dstatus: Status) {
    let Some(index) = list.get_selection() else {
        dstatus.set_label("Choose a download to cancel first.");
        return;
    };
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    let row = wiring
        .downloads
        .borrow()
        .get(index)
        .map(|row| (row.id, row.key.clone(), row.done));
    match row {
        Some((_, _, true)) => {
            dstatus.set_label("That download has already finished.");
            return;
        }
        None => {
            dstatus.set_label("Choose a download to cancel first.");
            return;
        }
        _ => {}
    }
    let confirm = MessageDialog::builder(
        &frame,
        &tr("Do you really want to cancel this download? It cannot be resumed afterwards."),
        &tr("Cancel Download"),
    )
    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
    .build();
    if confirm.show_modal_tracked() != ID_YES {
        return;
    }
    match row {
        Some((id, _, _)) if id != 0 => {
            wiring.download_worker.cancel(id);
            dstatus.set_label("Cancelling the selected download...");
        }
        Some((_, key, _)) => {
            // An ffmpeg catch-up capture. Waiting out a retry
            // delay, nothing is running: drop the staged entry
            // and close the row out here. Probing or capturing:
            // flag it cancelled (its finish then cleans up
            // without retrying) and ask the engine to stop it.
            let waiting = wiring
                .staged_downloads
                .borrow()
                .get(&key)
                .is_some_and(|staged| staged.retry_at.is_some());
            if waiting {
                wiring.staged_downloads.borrow_mut().remove(&key);
                let mut rows = wiring.downloads.borrow_mut();
                if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
                    row.progress = "Cancelled".to_string();
                    row.done = true;
                }
                drop(rows);
                let active = wiring.downloads.borrow().iter().any(|row| !row.done);
                wiring.downloads_active.set(active);
                refresh_downloads_window(wiring);
                dstatus.set_label("Cancelled the selected download.");
            } else {
                if let Some(staged) = wiring.staged_downloads.borrow_mut().get_mut(&key) {
                    staged.cancelled = true;
                }
                wiring.recordings.borrow_mut().stop(&key);
                dstatus.set_label("Cancelling the selected download...");
            }
        }
        _ => {}
    }
}

/// File > Account Info (audit N17): discovery runs on a
/// worker because it walks every loaded channel URL to
/// autodetect accounts that were never added through the
/// Xtream dialog (Python's `show_account_info`). The result
/// is presented by `poll_account_info` only while the
/// playlist it was discovered from is still loaded.
fn open_account_info(wiring: &Wiring, status: Status) {
    let sources: Vec<serde_json::Value> =
        match wiring.persist.config.borrow().value("playlists").cloned() {
            Some(serde_json::Value::Array(entries)) => entries,
            Some(serde_json::Value::String(_)) => wiring
                .persist
                .config
                .borrow()
                .string_list("playlists")
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
            _ => Vec::new(),
        };
    let channels: Vec<Channel> = wiring
        .browser
        .borrow()
        .catalogue_model()
        .channels()
        .to_vec();
    wiring
        .account_discovery_key
        .set(wiring.model.borrow().playlist_ptr());
    let (sender, receiver) = std::sync::mpsc::channel();
    *wiring.account_discovery.borrow_mut() = Some(receiver);
    status.set_label("Looking for provider accounts...");
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            iptv_core::account_info::discover_accounts(&sources, &channels)
        }))
        .map_err(|_| "the account scan failed".to_string());
        let _ = sender.send(result);
    });
}

/// Present a finished account discovery and drain account
/// lookups into the open Account Info window (audit N17).
fn poll_account_info(wiring: &Wiring, parent: &Frame, _status: Status) {
    let discovered = wiring
        .account_discovery
        .borrow()
        .as_ref()
        .and_then(|receiver| receiver.try_recv().ok());
    if discovered.is_some() {
        *wiring.account_discovery.borrow_mut() = None;
    }
    if let Some(result) = discovered {
        // Python drops a discovery whose playlist changed
        // underneath it, silently.
        if wiring.model.borrow().playlist_ptr() == wiring.account_discovery_key.get() {
            match result {
                Ok(accounts) if !accounts.is_empty() => {
                    open_account_info_dialog(wiring, accounts);
                }
                Ok(_) => {
                    let dialog = MessageDialog::builder(
                        parent,
                        "No provider accounts were found.\n\nXtream Codes and Stalker Portal accounts added in the Playlist Manager are listed here, along with any account detected from a playlist or stream URL.",
                        "No Accounts",
                    )
                    .with_style(
                        MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                    )
                    .build();
                    dialog.show_modal_tracked();
                }
                Err(error) => {
                    let dialog = MessageDialog::builder(
                        parent,
                        &format!("Could not look for provider accounts: {error}"),
                        "Error",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
                    .build();
                    dialog.show_modal_tracked();
                }
            }
        }
    }
    let messages: Vec<AccountLookupMsg> = match wiring.account_info_ui.borrow().as_ref() {
        Some(ui) => {
            let receiver = ui.receiver.borrow();
            let mut drained = Vec::new();
            while let Ok(message) = receiver.try_recv() {
                drained.push(message);
            }
            drained
        }
        None => Vec::new(),
    };
    if messages.is_empty() {
        return;
    }
    if let Some(ui) = wiring.account_info_ui.borrow().as_ref() {
        for (token, index, result) in messages {
            match result {
                Ok(report) => {
                    // Python caches every finished report,
                    // even one whose display went stale.
                    ui.reports.borrow_mut().insert(index, report.clone());
                    apply_account_report(ui, token, &report, true);
                }
                Err(error) => {
                    // Errors are not cached: re-selecting
                    // the account retries it.
                    if token == ui.token.get() {
                        apply_account_report(
                            ui,
                            token,
                            &format!("Could not check this account.\n\n{error}"),
                            false,
                        );
                    }
                }
            }
        }
    }
}

/// Show a finished lookup in the Account Info window when it
/// is still the current request (Python's `_apply_report`).
fn apply_account_report(ui: &AccountInfoUi, token: u64, report: &str, ok: bool) {
    if token != ui.token.get() {
        return;
    }
    ui.details.set_value(report);
    // Reading starts at the top, not wherever the previous
    // report ended.
    ui.details.set_insertion_point(0);
    ui.status
        .set_label(if ok { "Ready." } else { "Check failed." });
    ui.refresh.enable(true);
}

/// Start (or reuse) the lookup for the selected account
/// (Python's `_start_lookup`): cached reports apply at once,
/// Refresh forces a new fetch.
fn start_account_lookup(ui: &AccountInfoUi, force: bool) {
    let Some(selection) = ui.list.get_selection() else {
        return;
    };
    let index = usize::try_from(selection).unwrap_or(usize::MAX);
    let Some(account) = ui.accounts.get(index).cloned() else {
        return;
    };
    ui.token.set(ui.token.get() + 1);
    let token = ui.token.get();
    if !force {
        let cached = ui.reports.borrow().get(&index).cloned();
        if let Some(report) = cached {
            apply_account_report(ui, token, &report, true);
            return;
        }
    }
    ui.details.set_value("Checking account, please wait...");
    ui.status
        .set_label(&format!("Checking {}...", account.label()));
    ui.refresh.enable(false);
    let sender = ui.sender.clone();
    std::thread::spawn(move || {
        let result = iptv_core::account_info::fetch_account_report(&account);
        let _ = sender.send((token, index, result));
    });
}

/// The Account Info window itself (Python's
/// `AccountInfoDialog`): accounts list, read-only details,
/// Refresh and Copy Details.
fn open_account_info_dialog(wiring: &Wiring, accounts: Vec<iptv_core::account_info::Account>) {
    let (frame, panel, list, dstatus) = list_dialog("Account Info", "Accounts");
    let labels: Vec<String> = accounts.iter().map(Account::label).collect();
    list.set_items(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    let copy = Button::builder(&panel).with_label("Copy &Details").build();
    let close = Button::builder(&panel).with_label("&Close").build();
    let (sender, receiver) = std::sync::mpsc::channel();
    *wiring.account_info_ui.borrow_mut() = Some(AccountInfoUi {
        list,
        details: dstatus.details,
        status: dstatus,
        refresh,
        accounts,
        reports: RefCell::new(std::collections::HashMap::new()),
        token: std::cell::Cell::new(0),
        sender,
        receiver: RefCell::new(receiver),
    });
    let ui_slot = Rc::clone(&wiring.account_info_ui);
    list.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |_| {
        if let Some(ui) = ui_slot.borrow().as_ref() {
            start_account_lookup(ui, false);
        }
    });
    let ui_slot = Rc::clone(&wiring.account_info_ui);
    refresh.on_click(move |_| {
        if let Some(ui) = ui_slot.borrow().as_ref() {
            start_account_lookup(ui, true);
        }
    });
    let ui_slot = Rc::clone(&wiring.account_info_ui);
    copy.on_click(move |_| {
        if let Some(ui) = ui_slot.borrow().as_ref() {
            let text = ui.details.get_value();
            if text.is_empty() {
                return;
            }
            if Clipboard::get().set_text(&text) {
                ui.status
                    .set_label("Account details copied to the clipboard.");
            } else {
                ui.status.set_label("Could not open the clipboard.");
            }
        }
    });
    let ui_slot = Rc::clone(&wiring.account_info_ui);
    close.on_click(move |_| {
        *ui_slot.borrow_mut() = None;
        frame.hide();
    });
    if let Some(ui) = wiring.account_info_ui.borrow().as_ref() {
        list.set_selection(0, true);
        start_account_lookup(ui, false);
    }
    frame.show(true);
}

/// Drain download events into the tracked rows (matched by
/// request id, audit R06) and announce completions on the main
/// status line; the open Downloads window refreshes live.
/// One queued row mutation from a download event (audit R06).
type DownloadRowUpdate = (u64, Box<dyn FnOnce(&mut DownloadRow)>);

fn poll_downloads(wiring: &Wiring, status: Status) {
    let mut changed = false;
    while let Some(event) = wiring.download_worker.try_recv() {
        let mut rows = wiring.downloads.borrow_mut();
        let (id, apply): DownloadRowUpdate = match event {
            iptv_core::downloads::DownloadEvent::Progress { id, written, total } => (
                id,
                Box::new(move |row: &mut DownloadRow| {
                    row.progress = match total {
                        Some(total) if total > 0 => {
                            format!("{}% of {}", written * 100 / total, format_bytes(total))
                        }
                        _ => format!("{} downloaded", format_bytes(written)),
                    };
                }),
            ),
            iptv_core::downloads::DownloadEvent::Finished { id, result } => match result {
                Ok(bytes) => (
                    id,
                    Box::new(move |row: &mut DownloadRow| {
                        row.progress = format!("Done ({})", format_bytes(bytes));
                        row.done = true;
                    }),
                ),
                Err(error) => (
                    id,
                    Box::new(move |row: &mut DownloadRow| {
                        row.progress = if error == "Download cancelled" {
                            "Cancelled".to_string()
                        } else {
                            format!("Failed: {error}")
                        };
                        row.done = true;
                    }),
                ),
            },
        };
        if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
            let was_done = row.done;
            apply(row);
            if !was_done && row.done && row.progress.starts_with("Done") {
                let name = row.name.clone();
                drop(rows);
                status.set_label(&trf("Download finished: {name}", &[("name", &name)]));
            }
            changed = true;
        }
    }
    let active = wiring.downloads.borrow().iter().any(|row| !row.done);
    wiring.downloads_active.set(active);
    if changed {
        refresh_downloads_window(wiring);
    }
}

/// Drain catch-up direct-file probe results (N13): a verified
/// direct file retargets the staged capture at it with the
/// host-scoped headers the probe used; no file means the
/// capture proceeds against the playlist URL as before. A
/// capture cancelled while probing ends here, unlaunched.
fn poll_direct_probes(wiring: &Wiring, status: Status) {
    while let Some(result) = wiring.direct_probe_worker.try_recv() {
        let key = result.key;
        let Some(mut staged) = wiring.staged_downloads.borrow_mut().remove(&key) else {
            continue;
        };
        staged.probing = false;
        if staged.cancelled {
            let mut rows = wiring.downloads.borrow_mut();
            if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
                row.progress = "Cancelled".to_string();
                row.done = true;
            }
            drop(rows);
        } else {
            if let Some(direct) = &result.direct_url {
                // Python parity: headers are re-filtered for
                // the file's host before ffmpeg sees them.
                staged.headers = Some(iptv_core::catchup_direct::headers_for_host(
                    &iptv_core::playback::StreamHeaders::for_channel(&staged.channel),
                    &staged.url,
                    direct,
                ));
                staged.url.clone_from(direct);
            }
            let mut rows = wiring.downloads.borrow_mut();
            if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
                row.progress = "Capturing...".to_string();
            }
            drop(rows);
            if let Err(error) = launch_staged_capture(wiring, &key, staged) {
                let mut rows = wiring.downloads.borrow_mut();
                if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
                    row.progress = format!("Failed: {error}");
                    row.done = true;
                }
                status.set_label(&error);
            }
        }
        let still_probing = wiring
            .staged_downloads
            .borrow()
            .values()
            .any(|staged| staged.probing);
        wiring.direct_probe_pending.set(still_probing);
        let active = wiring.downloads.borrow().iter().any(|row| !row.done);
        wiring.downloads_active.set(active);
        refresh_downloads_window(wiring);
    }
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
    let language = wiring.i18n.borrow().resolved_language();
    let (guide, section) = iptv_core::guide::open_topic(Some(&language), Some(topic), None);
    show_guide(&ui, guide);
    if let Some(section) = section {
        move_caret(&ui, &section);
    }
    ui.frame.show(true);
    ui.frame.raise();
    status.set_label("User Guide opened.");
}

/// Attach F1 help to one control: F1 opens the guide at the
/// control's topic, as Python's `set_help_topic` does. Taking focus
/// also records the topic, so the main window's Help menu command
/// opens where the user currently is (S10).
fn bind_f1_help<T>(control: &T, wiring: &Wiring, topic: &'static str, status: Status)
where
    T: wxdragon::event::WxEvtHandler,
{
    let focus_topic = Rc::clone(&wiring.help_focus_topic);
    let wiring = clone_wiring(wiring);
    control.bind_internal(EventType::KEY_DOWN, move |event| {
        if event.get_key_code() == Some(340) {
            open_guide_window_at(&wiring, status, topic);
        }
        event.skip(true);
    });
    control.bind_internal(EventType::SET_FOCUS, move |event| {
        *focus_topic.borrow_mut() = topic;
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
        // A matched hotkey is consumed: skipping it would let the
        // focused control ALSO process the key, so Ctrl+Up/Down
        // moved the list/tree selection while changing volume.
        let handled = dispatch_main_hotkey(&wiring, controls, status, event);
        event.skip(!handled);
    });
}

/// Returns whether the key matched a configured main-window
/// action (and was therefore consumed).
fn dispatch_main_hotkey(
    wiring: &Wiring,
    controls: Controls,
    status: Status,
    event: wxdragon::event::Event,
) -> bool {
    let Some(label) = key_event_label(event) else {
        return false;
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
                status.set_label(&trf(
                    "Volume {value} percent.",
                    &[("value", &value.to_string())],
                ));
            }
            true
        }
        Some("record") => {
            if let Some(channel) = selected_channel(wiring, controls.list) {
                toggle_recording(
                    &controls.list,
                    &wiring.recordings,
                    &wiring.buffers,
                    &wiring.persist,
                    &wiring.media_probe,
                    &channel,
                    status,
                    Some(&wiring.player),
                    &wiring.share_ctx(),
                );
            } else {
                status.set_label("Select a channel to record first.");
            }
            true
        }
        _ => false,
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
    dstatus.set_label(&trf(
        "{count} commands. Edit changes a binding; menus pick up changes on restart.",
        &[("count", &count.to_string())],
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
        if dialog.show_modal_tracked() != ID_OK {
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
                dstatus.set_label(&trf("Shortcut for {label} updated.", &[("label", label)]));
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
    bind_escape_hide(frame);
    let panel = Panel::builder(&frame).build();
    // N24: Python's dialog order — the explanation, the
    // audio-description switch, then the keywords field with
    // its label and the hint under it.
    let intro = StaticText::builder(&panel)
        .with_label("When a channel offers more than one audio track, the built-in player can switch to the track you want on its own.")
        .build();
    let _ = intro;
    let ad_check = CheckBox::builder(&panel)
        .with_label("Prefer an audio description track when the channel has one")
        .build();
    ad_check.set_value(
        wiring
            .persist
            .config
            .borrow()
            .bool_or("prefer_audio_description", false),
    );
    let caption = StaticText::builder(&panel)
        .with_label("Preferred track names or languages, most wanted first:")
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
    keywords.set_accessibility_label("Preferred track names or languages, most wanted first:");
    let hint = StaticText::builder(&panel)
        .with_label("Separate them with commas, for example: audio description, English. A track is used when its name contains one of these words. Leave this empty to keep whatever track the channel starts on.")
        .build();
    let _ = hint;
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

/// Help > What's New: the newest releases' notes, localized
/// through the release-notes catalogue (S09, Python's
/// `ChangelogDialog`), with the older history one button away in the
/// same document.
fn open_whats_new(wiring: &Wiring, status: Status) {
    let (frame, panel, _list, dstatus) = list_dialog("What's New", "Changes");
    let localized = {
        let i18n = wiring.i18n.borrow();
        iptv_core::i18n::localize_notes(&i18n, WHATS_NEW_TEXT)
    };
    let (recent, older) =
        iptv_core::i18n::split_notes_window(&localized, iptv_core::i18n::RELEASE_WINDOW);
    dstatus.details.set_value(&recent);
    dstatus.set_label(&trf(
        "What's new in version {version}",
        &[("version", env!("CARGO_PKG_VERSION"))],
    ));
    if !older.trim().is_empty() {
        let toggle = Button::builder(&panel)
            .with_label(&tr("Show older releases"))
            .build();
        toggle.set_accessibility_label("Show older releases");
        let shown = Rc::new(std::cell::Cell::new(false));
        let details = dstatus.details;
        let handle = toggle;
        toggle.on_click(move |_| {
            let next = !shown.get();
            shown.set(next);
            if next {
                details.set_value(&format!("{recent}\n{older}"));
                handle.set_label(&tr("Hide older releases"));
            } else {
                details.set_value(&recent);
                handle.set_label(&tr("Show older releases"));
            }
        });
    }
    // F1 in this window opens the guide at its own topic (S10).
    bind_f1_help(&frame, wiring, "updates", status);
    bind_f1_help(&dstatus.details, wiring, "updates", status);
    frame.show(true);
}

/// The What's New text for the running version (updated per release).
const WHATS_NEW_TEXT: &str = "Version 0.4.22 Release notes are now written for people, in the changelog in the project repository, and the GitHub release page shows them. Every past release’s notes have been filled in the same way, back to the first one. The release process now refuses to publish a version whose changelog section is missing, so an empty release page cannot ship again.\n\nVersion 0.4.21 Escape now closes windows, as it always did in the Python app: every list window (What’s on Now, Continue Watching, Recently Watched, Reminders, Account Info, Downloads, the programme guide and more), the Playlist and EPG managers, the account form, Scheduled Recordings, Recording Conflicts, the Reminder, VOD, Cast, Audio Preferences, Shutdown Countdown and User Guide windows all hide on Escape, and the hand-built dialogs (sliders, Recording Problem Times, About, Custom Player, Resume) cancel on Escape. Before, Escape did nothing in any of them.\n\nVersion 0.4.20 stops the Windows console window from opening alongside the app: the program is now a proper Windows GUI application, like the Python client. Nothing else changes from 0.4.19.\n\nVersion 0.4.19 removes the Windows debugging-check dialogs for good. They were never app errors: the interface library was being built with its developer consistency checks switched on, so an internal check that a shipped build is supposed to skip could stop the app with a dialog (the menu check and the tree check in earlier versions were the same thing). The library is now built the way release builds are meant to be, with those checks compiled out, matching the Python client. Also fixed: switching the player into or out of fullscreen now settles its layout and focus just after the switch, as the Python client defers them.\n\nVersion 0.4.18 completes the second parity pass against the Python client. Chromecast casting works now: devices are found on your network, the channel plays through the cast proxy, the app asks before disconnecting, saved positions and local buffering are refused while casting, and a casting session counts against one-connection providers. Translations ship with the app: the Python client's catalogues and help guides in 14 languages are bundled, so a non-English system language now actually translates the interface and the guides. A new Account Info window finds your provider accounts, including accounts detected from playlist and channel addresses, and shows each account's status and limits, with Copy Details. Guide imports match Python: gzipped guides resume after an interruption, imports lock across app copies, an old guide database offers Python's repair prompt, and the import's start and finish are announced. Dialogs carry Python's explanations as readable text, the About box gains the community links, What Is Playing and Go to Channel answer the way Python's do, the category tree branches per playlist with an Other playlists branch and channel counts, Favorites messages match Python's, the player status line shows buffer, bitrate and volume in Python's format, audio track changes are spoken, and the mouse wheel over the player changes the volume. Recording formats now follow the selected channel's stream type, failed catch-up downloads retry and explain themselves, and message boxes that arrive while another is open now queue instead of stacking. The guide import lock also clears promptly on Windows and macOS when the copy of the app that held it has closed.\n\nVersion 0.4.14 keeps the window responsive while the saved guide loads, and brings more of the Python client's behaviour across. Channel details, series scans, guide search and What's on Now now read the saved guide on a background thread instead of freezing the window, and results that arrive after you have moved on are dropped instead of replacing what you are looking at. Guide matching now uses the Python client's scored matcher, so channels find their correct guide listings more reliably. Recordings gain the Python client's per-recording logs, outcome reports and Recording Problem Times view, and a failed download now explains the provider error in plain words instead of leaving a raw code.\n\nVersion 0.4.13 makes the built-in player's timers actually run, and stops decoding video when the player window is hidden. Reconnect, resume, subtitle speech and the sleep timer now work in the real app; they were silently dead in every earlier build. Hidden playback uses far less processing power (about 1.5 percent of one core instead of about 12), and pressing Enter while the player engine is still starting no longer freezes the window; playback begins when the engine is ready. Also fixed: two libVLC data layouts that could corrupt memory, and the release packages now bundle the newest VLC 3 release.\\n\\nVersion 0.4.12 adds VOD search and item details, and stops the window freezing during playback. In the VOD window, the new Search VOD field searches movies and series across every group: type a title and press Enter; each result row names its group and where it comes from, the status line says how many items matched, and clearing the search brings back the group you were browsing. The new Item details view shows the selected item's title, type, group, source, its guide description when one is available, and its stream address. Opening a series from the results and going back returns you to the same results with that series still selected. Also fixed: the guide information on channel rows now refreshes on a background thread instead of on the window itself, so alt-tabbing back into the app during playback answers right away instead of taking five to ten seconds, and a refreshed guide merges in much faster.\\n\\nVersion 0.4.11 makes startup and channel changes feel instant with a large guide. The channel rows used to freeze for several seconds after each playlist load, because refreshing the guide information on each row scanned the whole saved guide; it now looks at just the hours around the current time, the same way the Python client does. On a guide with about 800,000 programmes that step drops from about five seconds to a fraction of a second, so new channels are ready to use as soon as they appear.\n\nVersion 0.4.10 speeds up the programme guide and fixes a recording check. The guide now loads and refreshes as fast as the Python client or faster: on a large test guide, a first load dropped from about thirteen minutes to about two and a half, a refresh from about three minutes to about one, and opening the app with a guide already saved is instant instead of taking most of a minute. Recordings work again on non-English Windows systems: the free-space check before recording no longer depends on localized command output (thanks to Farhan Khan for that fix). And the player no longer logs a warning about a plugin-path option that current libVLC removed; it uses the mechanism libVLC supports now.\n\nVersion 0.4.9 refreshes everything under the hood: the cryptography, the guide parser, the database and the platform libraries are all at their newest released versions. Nothing changes on screen, and settings backups stay fully compatible; backups written by earlier versions still restore.\n\nVersion 0.4.8\n\nCasting works with header-protected streams again. The built-in cast proxy now answers the HEAD requests casting devices send before playing, follows redirects so playlists resolve against the address they actually live at, and passes range and duration headers through, so casting devices can probe a stream properly. Streams that need a specific player identity now get the default one automatically. Thanks to Farhan Khan for the fix. The PR validation gate also checks whitespace properly again.\n\nVersion 0.4.7\n\nFaster with big playlists and guides, and the last parity gaps closed. Large playlists now keep a background index, so switching playlists or categories and searching stay quick even with hundreds of thousands of channels, and the guide shown on each row refreshes almost instantly instead of re-checking every channel against the guide every minute. Scheduled Recordings has the right-click menu (Refresh, Cancel, Delete) like the Python client. The preferred audio track check now waits as long as the Python client does, so a slow stream is less likely to lose your saved track. Closing the app no longer waits on the player engine. The log file lives where the Python client puts it. And pressing F1 while a menu is open now opens the help for the menu item you are on.\n\nVersion 0.4.6\n\nFix: single-instance handover now works on Windows (the lock file is opened with the access Windows requires). Plus the Python parity catch-up. Guide sources saved by the Python client now load, the saved guide is there again after a restart, and search finds programmes as well as channels. Provider playlists keep their own channels separate, replace cleanly on refresh, and a failed refresh keeps the channels you had. Resume positions now read the Python client's bookmarks, so Continue Watching carries across. Recordings get unique file names, retries, destination space checks, and honest empty-recording reports; reminders and series recording match the Python dialogs. The app is single-instance (a second launch hands over to the running one), keeps a real log file under Help, checks for updates hourly, and F1 opens the help topic for the window you are in.\n\nVersion 0.4.4\n\nLess talk, working keys. Controls no longer read out instructional sentences when you land on them — Categories, Search, and Channels announce their names, like the Python app, and the channel list no longer repeats the playlist status message every time you tab to it. Ctrl+Up and Ctrl+Down now only change the volume; they no longer move the channel or category selection at the same time. And the search field selects your whole query when you enter it, so typing replaces it, with Ctrl+A selecting all.\n\nVersion 0.4.3\n\nRecording conflicts and safer imports. When a scheduled recording would take a provider connection your watching is using, the recording waits with its reason shown in Scheduled Recordings instead of cutting in; you choose there: keep watching and let it wait, stop playback and record, or cancel the recording. Waiting recordings start fairly, earliest first, when a connection frees. And importing settings now first saves a verified encrypted recovery copy of your current settings where you choose, under its own password, and refuses to run while recordings or downloads are active. The basic player (when libVLC is unavailable) also reports its state honestly now, and its menus no longer offer full-player-only controls.\n\nVersion 0.4.2\n\nReliability and subtitle speech. Playback now confirms before it counts as playing, and a failing stream retries inside one 30-second budget with a clear Stream lost message instead of spinning forever. Recordings that start late say so honestly, and cancelling a scheduled recording now stops its capture. Settings backups interchange with the Python client, verified both ways. Subtitles can now speak through an application voice on Windows (SAPI), not only through your screen reader: pick the backend, output device, volume, and speed under Options, Subtitle Speech, and if the chosen output device disappears, speech pauses until it returns or falls back to the system default, per the device policy.\n\nVersion 0.4.1\n\nLess talk. Channel rows now carry the guide the way the Python app does — channel name first, then the current and next programmes after it — and moving through the list no longer speaks the guide out loud on every step; the description panel still fills in silently.\n\nVersion 0.4.0\n\nSigned updates. Check for Updates now uses the ship-shape updater, the same signed updater as BlindRadio: update packages are cryptographically signed and verified before anything installs, then install and relaunch on their own (Windows and Linux; macOS opens the verified disk image for a manual install). Releases before this one can still update forward onto it.\n\nVersion 0.3.4\n\nFix: large playlists load promptly at startup. The playlist now loads first and the programme guide follows once it settles, instead of the two downloads racing; the guide database write no longer freezes the window; and the channel list says Loading playlist while a load is in flight instead of claiming no playlist is loaded.\n\nVersion 0.3.3\n\nFix: streams play again. The packages now bundle libVLC (the built-in player engine) beside the app on Windows, Linux, and macOS, so playback works with no separate VLC install. If libVLC still cannot load, pressing play now says so instead of failing silently.\n\nVersion 0.3.2\n\nFix: large playlists and guides now load. The HTTP library silently capped downloads at 10 MB, so a big provider playlist (for example a 28 MB, 115,000-channel list) failed to add and its EPG could never load. Playlist, EPG, Xtream, and Stalker fetches now take the full body.\n\nVersion 0.3.1\n\nFix: the Windows build no longer shows a wxWidgets debug dialog (\"Can't expand/collapse hidden root node!\") at startup or when the category list rebuilds. Nothing else changes from 0.3.0.\n\nVersion 0.3.0\n\nMain window: the layout now matches the Python client exactly (Playlist view and Categories on the left; Search, Channels, description and Stream URL on the right), and the menu-less shortcuts from the Python client work here: Ctrl+Up/Down volume, Ctrl+Shift+R record, Ctrl+D favorite and the rest of the documented set.\n\nPlayer: a Playback menu in Python's order, a Sleep Timer with an optional volume fade-out, and an Audio Output Device dialog that lists the real libVLC devices (System default first) and applies the saved choice at playback start. Announcement levels now gate player speech exactly as Python's do (None, Errors only, Important events, Detailed status).\n\nLocal Live Buffer: live TV in the built-in player can be buffered locally (off by default; set it up from the player's Playback menu), which enables pausing live TV, Rewind Live TV and Return to Live. A recording of a buffered channel reads the buffer instead of opening a second provider connection.\n\nDialogs: What's on Now has its filter box with a program count; Continue Watching gains Check Availability, which re-resolves each saved position against the current playlists and reports Available or Unavailable per row. Settings backup and restore now use Python's password-protected encrypted format, so backups interchange with the Python client both ways.\n\nMenus and shell: the Media Player menu lists all 32 players the Python client offers, with its exact per-platform program lookup. The tray icon builds Python's popup fresh on every right-click (Restore, Player Controls, Stop Recording(s) while recording, Exit) and left-click restores the window. The Recordings menu's minimum-free-space reserve and schedule padding use Python's values.";

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
        Err(error) => status.set_label(&trf("Shutdown failed: {error}", &[("error", &error)])),
    }
}

/// Disarm shutdown-after-recordings completely (audit R17):
/// the saved option, the recorded-since-arming flag and the
/// attempted flag all clear, so a later explicit re-arm starts
/// a fresh countdown cycle.
fn disarm_shutdown(wiring: &Wiring) {
    wiring
        .persist
        .set_and_save("shutdown_after_recordings", serde_json::json!(false));
    wiring.recorded.set(false);
    wiring.shutdown_attempted.set(false);
}

/// The shutdown countdown window: 30 seconds to cancel before the
/// computer shuts down after recordings (Python's dialog).
/// Cancelling disarms the option entirely (audit R17).
fn open_shutdown_countdown(wiring: &Wiring, status: Status) {
    let frame = Frame::builder()
        .with_title("Shut Down After Recordings")
        .with_size(Size::new(440, 240))
        .build();
    frame.set_accessibility_label("Shut Down After Recordings");
    bind_escape_hide(frame);
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
    let wiring_tick = clone_wiring(wiring);
    timer.on_tick(move |_| {
        let next = remaining_tick.get() - 1;
        remaining_tick.set(next);
        if next <= 0 {
            timer_tick.stop();
            // The option turns itself off once used (Python).
            disarm_shutdown(&wiring_tick);
            execute_shutdown(status);
            frame.hide();
        } else {
            label.set_label(&trf(
                "All recordings have finished.\n\nThe computer will shut down by itself. Seconds remaining: {next}",
                &[("next", &next.to_string())],
            ));
        }
    });
    let timer_cancel = Rc::clone(&timer);
    let wiring_cancel = clone_wiring(wiring);
    cancel.on_click(move |_| {
        timer_cancel.stop();
        disarm_shutdown(&wiring_cancel);
        status.set_label("Shutdown canceled. Automatic shutdown after recordings is now off.");
        frame.hide();
    });
    let timer_now = Rc::clone(&timer);
    let wiring_now = clone_wiring(wiring);
    now_button.on_click(move |_| {
        timer_now.stop();
        disarm_shutdown(&wiring_now);
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

/// One segment of a category's tree path (N22): `key` is the node's
/// identity (a playlist branch keys on its scope, so two branches
/// with the same label stay separate, as Python's
/// `("playlist", scope, label)` path tuples do), `text` is what the
/// node shows.
#[derive(Clone, PartialEq)]
struct CategoryPathPart {
    key: String,
    text: String,
}

fn category_path_part(text: &str) -> CategoryPathPart {
    CategoryPathPart {
        key: text.to_owned(),
        text: text.to_owned(),
    }
}

/// One category-tree row (N22): the category it selects, its full
/// label (with Python's count where the reference carries one), and
/// its tree path — the playlist branch first, then the group name's
/// own path parts.
#[derive(Clone, PartialEq)]
struct CategoryRow {
    category: Category,
    label: String,
    path: Vec<CategoryPathPart>,
}

/// The category tree's display state: the rows in display order.
/// Tree items are found by walking text paths (wxdragon's
/// `TreeItemId` owns its pointer and cannot be cloned into the
/// state); a row's text path is its path parts' texts with the leaf
/// carrying the row label's count. Branch header nodes own no row —
/// selecting one changes nothing (Python's tree reports no category
/// index for them).
#[derive(Default)]
struct CategoryTreeState {
    rows: Vec<CategoryRow>,
}

/// A row's tree text path: its parts' texts, the last one carrying
/// the label's count suffix when it has one.
fn category_row_text_path(row: &CategoryRow) -> Vec<String> {
    let mut texts: Vec<String> = row.path.iter().map(|part| part.text.clone()).collect();
    if let (Some(text), Some(part)) = (texts.last_mut(), row.path.last()) {
        *text = iptv_core::categories::category_leaf_text(&row.label, &part.text);
    }
    texts
}

/// The text path from the hidden root to the tree item with this
/// id value, or None when it is not in the tree.
fn tree_item_text_path(control: TreeCtrl, target: u64) -> Option<Vec<String>> {
    fn walk(control: TreeCtrl, parent: &TreeItemId, target: u64, path: &mut Vec<String>) -> bool {
        let Some((first, mut cookie)) = control.get_first_child(parent) else {
            return false;
        };
        let mut child = Some(first);
        while let Some(item) = child {
            path.push(control.get_item_text(&item).unwrap_or_default());
            if u64::from(&item) == target || walk(control, &item, target, path) {
                return true;
            }
            path.pop();
            child = control.get_next_child(parent, &mut cookie);
        }
        false
    }
    let root = control.get_root_item()?;
    let mut path = Vec::new();
    walk(control, &root, target, &mut path).then_some(path)
}

/// The tree item at a text path below the hidden root; the first
/// sibling with matching text wins, mirroring how duplicate labels
/// collapse in Python's tree too.
fn tree_item_at_text_path(control: TreeCtrl, texts: &[String]) -> Option<TreeItemId> {
    let mut parent = control.get_root_item()?;
    for text in texts {
        let (first, mut cookie) = control.get_first_child(&parent)?;
        let mut child = Some(first);
        let mut found = None;
        while let Some(item) = child {
            if control.get_item_text(&item).as_deref() == Some(text.as_str()) {
                found = Some(item);
                break;
            }
            child = control.get_next_child(&parent, &mut cookie);
        }
        parent = found?;
    }
    Some(parent)
}

/// The branch header segment for one playlist scope: labeled from
/// the scope picker, or "Other playlists" for a scope no configured
/// source owns (Python `main.py:6180`).
fn branch_path_part(
    scope: &str,
    branch_labels: &std::collections::BTreeMap<String, String>,
) -> CategoryPathPart {
    CategoryPathPart {
        key: format!("playlist:{scope}"),
        text: branch_labels
            .get(scope)
            .cloned()
            .unwrap_or_else(|| "Other playlists".to_string()),
    }
}

/// Build the category tree's rows from the browser's current
/// category list (N22): flat rows keep their settled labels, and
/// the All-playlists branch leaves carry Python's counts
/// ("All Channels (n)" under the branch, "Group (n)").
fn category_rows_display(
    browser: &BrowserWorker,
    branch_labels: &std::collections::BTreeMap<String, String>,
) -> Vec<CategoryRow> {
    let favorite_count = browser.favorite_count();
    browser
        .categories()
        .iter()
        .map(|category| match category {
            Category::PlaylistAll(scope) => CategoryRow {
                category: category.clone(),
                label: format!("All Channels ({})", browser.category_row_count(category)),
                path: vec![
                    branch_path_part(scope, branch_labels),
                    category_path_part("All Channels"),
                ],
            },
            Category::PlaylistGroup(scope, group) => {
                let mut path = vec![branch_path_part(scope, branch_labels)];
                path.extend(
                    iptv_core::categories::category_path_parts(group)
                        .into_iter()
                        .map(|part| category_path_part(&part)),
                );
                CategoryRow {
                    category: category.clone(),
                    label: format!("{group} ({})", browser.category_row_count(category)),
                    path,
                }
            }
            Category::Group(name) => CategoryRow {
                category: category.clone(),
                label: category_display(category, favorite_count),
                path: iptv_core::categories::category_path_parts(name)
                    .into_iter()
                    .map(|part| category_path_part(&part))
                    .collect(),
            },
            _ => {
                let label = category_display(category, favorite_count);
                CategoryRow {
                    category: category.clone(),
                    path: iptv_core::categories::category_path_parts(&label)
                        .into_iter()
                        .map(|part| category_path_part(&part))
                        .collect(),
                    label,
                }
            }
        })
        .collect()
}

/// Refresh the category control from the browser's current playlist without
/// moving keyboard focus: rebuild rows only when the set changed, and keep
/// the selection on the active category.
///
/// N22: the rebuild follows Python's category tree — rows nest by
/// their path (per-playlist branches in the All view, group names
/// split on " > " or "/"), nodes are shared by path prefix, a
/// category that is also a branch keeps its own leaf text, and
/// every branch starts expanded. The hidden root is never expanded
/// (see `category_controls`).
fn sync_category_control(
    control: TreeCtrl,
    state: &RefCell<CategoryTreeState>,
    last_count: &std::cell::Cell<usize>,
    browser: &RefCell<BrowserWorker>,
    branch_labels: &std::collections::BTreeMap<String, String>,
) {
    let browser = browser.borrow();
    let current = category_rows_display(&browser, branch_labels);
    let favorite_count = browser.favorite_count();
    let favorites_active = browser.active_category() == &Category::Favorites;
    // B20: a changed favorite count alone (toggle from All Channels,
    // a scope switch) must rebuild the label too — it used to stay
    // stale unless the category set changed or Favorites was active.
    // The count rides in the row label now, so the row comparison
    // covers it; the explicit triggers stay as before.
    let count_changed = favorite_count != last_count.get();
    if state.borrow().rows != current || favorites_active || count_changed {
        if let Some(root) = control.get_root_item() {
            control.delete_children(&root);
            let mut path_items: std::collections::HashMap<Vec<String>, TreeItemId> =
                std::collections::HashMap::new();
            let mut built = true;
            'rows: for row in &current {
                let mut parent: &TreeItemId = &root;
                let mut key_path: Vec<String> = Vec::new();
                for (index, part) in row.path.iter().enumerate() {
                    key_path.push(part.key.clone());
                    let last = index + 1 == row.path.len();
                    if !path_items.contains_key(&key_path) {
                        let text = if last {
                            iptv_core::categories::category_leaf_text(&row.label, &part.text)
                        } else {
                            part.text.clone()
                        };
                        if let Some(item) = control.append_item(parent, &text, None, None) {
                            path_items.insert(key_path.clone(), item);
                        } else {
                            built = false;
                            break 'rows;
                        }
                    }
                    let item = &path_items[&key_path];
                    if last {
                        // The category itself may also be a branch:
                        // its own label (with its count) wins.
                        control.set_item_text(
                            item,
                            &iptv_core::categories::category_leaf_text(&row.label, &part.text),
                        );
                    }
                    parent = item;
                }
            }
            if built {
                // A source's categories are visible immediately
                // under its branch (Python expands them all).
                for item in path_items.values() {
                    if control.item_has_children(item) {
                        control.expand(item);
                    }
                }
                state.borrow_mut().rows = current;
            }
        }
        last_count.set(favorite_count);
    }
    let active = browser.active_category().clone();
    let state = state.borrow();
    if let Some(row) = state.rows.iter().find(|row| row.category == active) {
        if let Some(item) = tree_item_at_text_path(control, &category_row_text_path(row)) {
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
    // live announcement already made for the deliberate action. The
    // message is status text only: as the list's accessible
    // description NVDA would re-read it on every focus entry.
    status.set_silent(&message);
    list.set_accessibility_description("");
    std::thread::spawn(move || drop(previous));
}

fn channel_text(
    model: &SearchResults,
    suffixes: &(i64, usize, std::collections::HashMap<usize, String>),
    epg_rows: &[EpgSearchRow],
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
        // Rows past the channel rows are the EPG search appends.
        return row
            .checked_sub(model.len())
            .and_then(|offset| epg_rows.get(offset))
            .map(|epg_row| epg_row.label.clone())
            .unwrap_or_default();
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

/// Queue a row-suffix rebuild on the suffix worker
/// (validation 0070). The pipeline that used to run here on the
/// UI tick — store snapshot, guide indexes, the playlist-wide
/// fuzzy match, the row build — measured up to 25 s at
/// real-world scale and froze the window on every refresh;
/// Python runs the same refresh on a background thread and so
/// does this port now. Dispatch only: one job in flight at a
/// time, and none while the browser is busy (Python's gate
/// defers while the user is interacting; the timers retry).
fn request_suffix_refresh(wiring: &Wiring) {
    if !wiring.persist.config.borrow().bool_or("epg_enabled", true) {
        // EPG off clears the suffixes, as the old synchronous
        // refresh did.
        let ptr = wiring.model.borrow().playlist_ptr();
        let mut slot = wiring.epg_suffixes.borrow_mut();
        if slot.1 != ptr || !slot.2.is_empty() {
            *slot = (now_epoch(), ptr, std::collections::HashMap::new());
        }
        return;
    }
    if wiring.suffix_pending.get() || wiring.browser.borrow().is_busy() {
        return;
    }
    let (model, _extras) = wiring.browser.borrow().catalogue_parts();
    if model.channels().is_empty() {
        return;
    }
    let generation = wiring.suffix_generation.get() + 1;
    wiring.suffix_generation.set(generation);
    wiring.suffix_pending.set(true);
    let snapshot = wiring.epg.borrow().clone();
    let guide_generation = snapshot
        .as_ref()
        .map_or(wiring.guide_generation.get(), |snapshot| {
            snapshot.generation
        });
    let indexes = snapshot.map(|snapshot| Arc::clone(&snapshot.indexes));
    wiring.suffix_worker.build(SuffixJob {
        generation,
        guide_generation,
        indexes,
        model,
        store_path: epg_store_path(),
        at: now_epoch(),
    });
    ensure_polling(wiring);
}

/// Apply finished suffix rebuilds (validation 0070): a result
/// lands only while its generation is the newest dispatched
/// AND the playlist it was built from is still the model's —
/// a republish since dispatch makes it stale and it drops.
/// The list repaints only when the map actually changed
/// (Python publishes on change only).
fn drain_suffix_results(wiring: &Wiring, list: ListCtrl) {
    while let Some(result) = wiring.suffix_worker.try_recv() {
        wiring.suffix_pending.set(false);
        if !suffix_result_matches(
            result.generation,
            wiring.suffix_generation.get(),
            result.guide_generation,
            wiring.guide_generation.get(),
            result.playlist_ptr,
            wiring.model.borrow().playlist_ptr(),
        ) {
            continue;
        }
        let Some(suffixes) = result.suffixes else {
            // The store could not be read: keep the on-screen
            // suffixes and let the next cycle retry.
            wiring.epg_suffixes.borrow_mut().0 = now_epoch();
            continue;
        };
        let changed = {
            let mut slot = wiring.epg_suffixes.borrow_mut();
            let changed = slot.2 != suffixes;
            *slot = (now_epoch(), result.playlist_ptr, suffixes);
            changed
        };
        if changed {
            let count = list.get_item_count();
            if count > 0 {
                list.refresh_items(0, i64::from(count) - 1);
            }
        }
    }
}

fn suffix_result_matches(
    result_generation: u64,
    current_generation: u64,
    result_guide_generation: u64,
    current_guide_generation: u64,
    result_playlist_ptr: usize,
    current_playlist_ptr: usize,
) -> bool {
    result_generation == current_generation
        && result_guide_generation == current_guide_generation
        && result_playlist_ptr == current_playlist_ptr
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_core::{search::SearchWorker, BrowseModel};
    use std::sync::Arc;

    #[test]
    fn hidden_stream_keeps_video_off_across_a_reconnect() {
        let mut state = PlayerState {
            video_visible: false,
            ..PlayerState::default()
        };
        for retry in [false, true] {
            let options = vlc_media_options(&state, true, retry, None);
            assert!(options.option_strings().contains(&":no-video".to_string()));
        }
        state.video_visible = true;
        let options = vlc_media_options(&state, true, true, None);
        assert!(!options.option_strings().contains(&":no-video".to_string()));
    }

    #[test]
    fn a_starting_libvlc_defers_the_play_instead_of_blocking() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut state = PlayerState {
            vlc_loader: Some(receiver),
            ..PlayerState::default()
        };
        assert!(!vlc_engine_settled(&mut state), "still starting");
        assert!(state.vlc_loader.is_some(), "the start must not be repeated");
        sender.send(Err("no libVLC".to_string())).unwrap();
        assert!(vlc_engine_settled(&mut state), "a failure settles");
        assert_eq!(state.vlc_error.as_deref(), Some("no libVLC"));
        assert!(state.vlc_loader.is_none());
        assert!(
            vlc_engine_settled(&mut state),
            "no second start until a new play"
        );
        assert!(state.vlc_loader.is_none());
    }

    fn no_suffixes() -> (i64, usize, std::collections::HashMap<usize, String>) {
        (0, 0, std::collections::HashMap::new())
    }

    #[test]
    fn stale_suffix_results_cannot_cross_guide_generations() {
        assert!(suffix_result_matches(2, 2, 4, 4, 10, 10));
        assert!(!suffix_result_matches(2, 2, 3, 4, 10, 10));
        assert!(!suffix_result_matches(1, 2, 4, 4, 10, 10));
        assert!(!suffix_result_matches(2, 2, 4, 4, 9, 10));
    }

    #[test]
    fn cold_guide_results_are_rejected_after_a_guide_generation_changes() {
        assert!(epg_read_generation_matches(4, 4));
        assert!(!epg_read_generation_matches(3, 4));
    }

    #[test]
    fn empty_list_has_no_placeholder_or_stale_accessible_rows() {
        let model = SearchResults::default();
        let suffixes = no_suffixes();
        for index in [-1, 0, 1, i64::MAX] {
            assert_eq!(channel_text(&model, &suffixes, &[], index, 0), "");
            assert_eq!(channel_text(&model, &suffixes, &[], index, 1), "");
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
            channel_text(&model, &suffixes, &[], 0, 0),
            "ABC East HD — News Hour (18:00–19:00) — Next: Movie (19:00)"
        );
        // A channel without guide data keeps the bare name.
        assert_eq!(channel_text(&model, &suffixes, &[], 1, 0), "映画, HD");
    }

    #[test]
    fn epg_search_rows_render_their_labels_past_the_channel_rows() {
        // Audit E08: rows appended below the channel rows render
        // the programme label; rows past them stay empty.
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
        let epg_rows = vec![EpgSearchRow {
            label: "ABC East HD - News Hour (18:00–19:00)".to_owned(),
            programme: Programme {
                channel_id: "abc".to_owned(),
                start: 0,
                stop: 3600,
                title: "News Hour".to_owned(),
                description: String::new(),
            },
            playlist_index: Some(0),
        }];
        assert_eq!(
            channel_text(&model, &suffixes, &epg_rows, 4, 0),
            "ABC East HD - News Hour (18:00–19:00)"
        );
        assert_eq!(channel_text(&model, &suffixes, &epg_rows, 5, 0), "");
        assert_eq!(channel_text(&model, &suffixes, &epg_rows, 4, 1), "");
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
        let suffix = iptv_core::epg::programme_suffix(Some(&now), Some(&next));
        assert!(suffix.starts_with(" — News Hour ("), "{suffix}");
        assert!(suffix.contains(" — Next: Movie ("), "{suffix}");
        assert_eq!(iptv_core::epg::programme_suffix(None, None), "");
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
        assert_eq!(channel_text(&model, &suffixes, &[], 0, 0), "ABC East HD");
        assert_eq!(channel_text(&model, &suffixes, &[], 1, 0), "映画, HD");
        assert_eq!(
            channel_text(&model, &suffixes, &[], 2, 0),
            "Bob's Burgers, Season 1"
        );
        assert_eq!(
            channel_text(&model, &suffixes, &[], 3, 0),
            "Unnamed channel"
        );
        assert_eq!(channel_text(&model, &suffixes, &[], 4, 0), "");
        assert_eq!(channel_text(&model, &suffixes, &[], -1, 0), "");
        assert_eq!(channel_text(&model, &suffixes, &[], 0, 1), "");
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

    // R10: ffmpeg probe reports become Python-shaped audio labels.
    #[test]
    fn audio_probe_labels_match_python_shape() {
        let report = "Input #0, mpegts, from 'x':\n  Stream #0:0: Video: h264\n  Stream #0:1(eng): Audio: aac\n  Stream #0:2(eng): Audio: aac, description: audio description\n";
        assert_eq!(
            parse_audio_stream_labels(report),
            vec![
                "Track 1 - [eng]".to_owned(),
                "Track 2 - [eng] - audio description".to_owned()
            ]
        );
        assert_eq!(
            parse_audio_stream_labels("no streams"),
            Vec::<String>::new()
        );
    }

    // R05: media duration parsing for the short-capture check.
    #[test]
    fn ffmpeg_duration_report_parses() {
        assert_eq!(
            parse_ffmpeg_duration("  Duration: 01:02:03.50, start: 0.0"),
            Some(3723.5)
        );
        assert_eq!(parse_ffmpeg_duration("Duration: N/A"), None);
    }

    // R11: media classification reads only explicit stream metadata.
    #[test]
    fn media_hint_reads_radio_attribute() {
        let mut channel = Channel {
            name: "News".to_owned(),
            group: String::new(),
            url: "https://example.com/news".to_owned(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        };
        assert_eq!(channel_media_is_audio(&channel), None);
        channel
            .unknown_attributes
            .insert("radio".to_owned(), "true".to_owned());
        assert_eq!(channel_media_is_audio(&channel), Some(true));
        channel.unknown_attributes.remove("radio");
        channel
            .unknown_attributes
            .insert("media-type".to_owned(), "video".to_owned());
        assert_eq!(channel_media_is_audio(&channel), Some(false));
    }
}

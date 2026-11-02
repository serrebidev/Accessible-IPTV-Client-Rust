use iptv_core::guide::Guide;
use iptv_core::i18n::I18n;
use iptv_core::updater::{UpdateEvent, UpdateWorker};
use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    cast::{CastDevice, CastEvent, CastWorker},
    categories::Category,
    config::{self, Config},
    dvr::{channel_from_snapshot, DvrAction, DvrSchedule},
    epg::{local_hhmm, match_guide_channel, EpgGuide, EpgWorker, GuideIndexes},
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
            .with_size(Size::new(720, 480))
            .build();
        frame.set_accessibility_label("Accessible IPTV Client");

        let panel = Panel::builder(&frame).build();
        let (open, cancel) = open_cancel_buttons(panel);
        let record = record_button(panel);
        let (category_label, category, toggle_favorite) = category_controls(panel);
        let search_label = StaticText::builder(&panel)
            .with_label("&Search channels")
            .build();
        let search = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        search.set_accessibility_label("Search channels");
        search.set_accessibility_description("Type to filter channel names within the active category. Tab or Enter moves to Channels; Shift+Tab returns to Toggle favorite.");
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
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(&rows.borrow(), index, column)
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
        details.set_accessibility_description("Read loading and search results here. Open playlist chooses a file or retries a failed load.");
        let status = Status {
            label: status_label,
            details,
            bar: Some(frame),
        };
        let (episode_description, stream_url) = info_panels(panel);
        let controls_all = Controls {
            open,
            cancel,
            record,
            category,
            toggle_favorite,
            search,
            list,
            episode_description,
            stream_url,
            status,
        };
        let controls_for_layout = controls_all;
        bind_browser(frame, controls_all, model);
        layout_main(
            frame,
            panel,
            controls_for_layout,
            category_label,
            search_label,
            label,
        );
    })
}

/// The main window layout: controls in browse/tab order, the frame
/// status bar, then show and focus the channel list.
fn layout_main(
    frame: Frame,
    panel: Panel,
    controls: Controls,
    category_label: StaticText,
    search_label: StaticText,
    channels_label: StaticText,
) {
    let layout = BoxSizer::builder(Orientation::Vertical).build();
    layout.add(&controls.open, 0, SizerFlag::All, 8);
    layout.add(&controls.cancel, 0, SizerFlag::All, 8);
    layout.add(&category_label, 0, SizerFlag::All, 8);
    layout.add(&controls.category, 0, SizerFlag::Expand | SizerFlag::All, 8);
    layout.add(&controls.toggle_favorite, 0, SizerFlag::All, 8);
    layout.add(&search_label, 0, SizerFlag::All, 8);
    layout.add(&controls.search, 0, SizerFlag::Expand | SizerFlag::All, 8);
    layout.add(&channels_label, 0, SizerFlag::All, 8);
    layout.add(&controls.list, 1, SizerFlag::Expand | SizerFlag::All, 8);
    layout.add(
        &controls.episode_description,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
    layout.add(
        &controls.stream_url,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
    layout.add(&controls.status.label, 0, SizerFlag::All, 8);
    layout.add(
        &controls.status.details,
        0,
        SizerFlag::Expand | SizerFlag::All,
        8,
    );
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

/// The Category list and the Toggle favorite action, in browse order.
fn category_controls(panel: Panel) -> (StaticText, ListBox, Button) {
    let label = StaticText::builder(&panel).with_label("&Category").build();
    let category = ListBox::builder(&panel).build();
    category.set_accessibility_label("Category");
    category.set_accessibility_description("Choose All Channels, Favorites, or a playlist group. Tab moves to Toggle favorite; Shift+Tab returns to Cancel loading.");
    category.set_items(&["All Channels", "Favorites"]);
    category.set_selection(0, true);
    let toggle = Button::builder(&panel)
        .with_label("&Toggle favorite")
        .build();
    toggle.set_accessibility_label("Toggle favorite");
    toggle.set_accessibility_description("Add or remove the channel selected in Channels from Favorites. The result is announced in Playlist status.");
    (label, category, toggle)
}

#[derive(Clone, Copy)]
struct Status {
    label: StaticText,
    details: TextCtrl,
    bar: Option<Frame>,
}

impl Status {
    fn set_label(self, message: &str) {
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

#[derive(Clone, Copy)]
struct Controls {
    open: Button,
    cancel: Button,
    record: Button,
    category: ListBox,
    toggle_favorite: Button,
    search: TextCtrl,
    list: ListCtrl,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
    status: Status,
}

#[derive(Clone)]
struct Wiring {
    browser: Rc<RefCell<BrowserWorker>>,
    categories: Rc<RefCell<Vec<Category>>>,
    timer: Rc<Timer<Frame>>,
    model: Rc<RefCell<SearchResults>>,
    persist: Persist,
    player: Rc<RefCell<PlayerState>>,
    epg: Rc<RefCell<Option<(EpgGuide, GuideIndexes)>>>,
    epg_worker: Rc<EpgWorker>,
    epg_pending: Rc<std::cell::Cell<bool>>,
    reminders: Rc<RefCell<Vec<Reminder>>>,
    provider_worker: Rc<ProviderWorker>,
    provider_pending: Rc<std::cell::Cell<bool>>,
    pending_resolve: Rc<RefCell<Option<Channel>>>,
    recordings: Rc<RefCell<RecordingEngine>>,
    cast_worker: Rc<CastWorker>,
    cast_pending: Rc<std::cell::Cell<bool>>,
    cast_ui: Rc<RefCell<Option<CastUi>>>,
    guide_ui: Rc<RefCell<Option<GuideUi>>>,
    update_worker: Rc<UpdateWorker>,
    update_pending: Rc<std::cell::Cell<bool>>,
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
}

/// Built-in player window state, created lazily on first use.
struct PlayerState {
    session: PlaybackSession,
    gate: LaunchGate,
    window: Option<(Frame, MediaCtrl)>,
    current_channel: Option<Channel>,
    pending_resume_ms: Option<i64>,
    subtitle_cues: Vec<Cue>,
    last_spoken_cue: Option<usize>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            session: PlaybackSession::new(),
            gate: LaunchGate::default(),
            window: None,
            current_channel: None,
            pending_resume_ms: None,
            subtitle_cues: Vec::new(),
            last_spoken_cue: None,
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
                    entries.insert(
                        key.clone(),
                        iptv_core::playback::ResumeEntry {
                            position_ms,
                            length_ms,
                            updated,
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
                    }),
                )
            })
            .collect();
        let mut config = self.config.borrow_mut();
        config.set_value("resume_positions", serde_json::Value::Object(map));
        let _ = config.save(&self.path);
    }
}

/// The Open playlist / Cancel loading buttons for the main window.
fn open_cancel_buttons(panel: Panel) -> (Button, Button) {
    let open = Button::builder(&panel)
        .with_label("&Open playlist…")
        .build();
    open.set_accessibility_label("Open playlist");
    let cancel = Button::builder(&panel)
        .with_label("&Cancel loading")
        .build();
    cancel.set_accessibility_label("Cancel loading");
    (open, cancel)
}

/// The Record toggle button for the main window.
fn record_button(panel: Panel) -> Button {
    let record = Button::builder(&panel).with_label("&Record").build();
    record.set_accessibility_label("Record");
    record.set_accessibility_description(
        "Start or stop recording the selected channel to the recordings folder.",
    );
    record
}

fn bind_browser(frame: Frame, controls: Controls, model: Rc<RefCell<SearchResults>>) {
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
        epg_worker: Rc::new(EpgWorker::new()),
        epg_pending: Rc::new(std::cell::Cell::new(false)),
        reminders: Rc::new(RefCell::new(Vec::new())),
        provider_worker: Rc::new(ProviderWorker::new()),
        provider_pending: Rc::new(std::cell::Cell::new(false)),
        pending_resolve: Rc::new(RefCell::new(None)),
        recordings: Rc::new(RefCell::new(RecordingEngine::new())),
        cast_worker: Rc::new(CastWorker::new()),
        cast_pending: Rc::new(std::cell::Cell::new(false)),
        cast_ui: Rc::new(RefCell::new(None)),
        guide_ui: Rc::new(RefCell::new(None)),
        update_worker: Rc::new(UpdateWorker::new()),
        update_pending: Rc::new(std::cell::Cell::new(false)),
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
    };
    *wiring.reminders.borrow_mut() = wiring.persist.reminders();
    if wiring.dvr.borrow().pending_count() > 0 {
        assert!(wiring.timer.start(50, false));
    }
    let epg_urls = wiring.persist.config.borrow().string_list("epg_urls");
    if !epg_urls.is_empty() {
        wiring.epg_worker.load(epg_urls);
        wiring.epg_pending.set(true);
        assert!(wiring.timer.start(50, false));
    }
    let (sources, accounts) = configured_entries(&wiring);
    if !sources.is_empty() {
        wiring.browser.borrow_mut().load_sources(sources);
        assert!(wiring.timer.start(50, false));
    }
    if !accounts.is_empty() {
        wiring.provider_worker.load(accounts);
        wiring.provider_pending.set(true);
        assert!(wiring.timer.start(50, false));
    }
    bind_completion(&wiring, controls);
    bind_recording(&wiring, controls);
    build_menu(frame, &wiring, controls);
    maybe_auto_check_updates(&wiring);
    bind_actions(frame, &wiring, controls);
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    let close_persist = wiring.persist.clone();
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
    wiring.timer.on_tick(move |_| {
        let event = completed.browser.borrow_mut().poll(&search.get_value());
        match event {
            Some(BrowseEvent::Results(results)) => {
                publish_results(list, status, &completed.model, results);
                sync_category_control(category, &completed.categories, &completed.browser);
            }
            Some(BrowseEvent::Recovery(error)) => {
                status.set_label(error.message());
                list.set_accessibility_description(error.message());
            }
            None => {}
        }
        poll_epg(&completed, status);
        poll_providers(&completed, controls);
        poll_recordings(&completed, status);
        poll_vod(&completed, status);
        poll_cast(&completed, status);
        poll_updates(&completed, status);
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
            && !completed.update_pending.get()
        {
            if let Some(timer) = completed_timer.upgrade() {
                timer.stop();
            }
        }
    });
}

fn bind_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    let Controls {
        open,
        cancel,
        record: _,
        category,
        toggle_favorite,
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
    category.bind_internal(EventType::COMMAND_LISTBOX_SELECTED, move |event| {
        let Some(selection) = category.get_selection() else {
            event.skip(true);
            return;
        };
        let Some(chosen) = category_choices
            .borrow()
            .get(usize::try_from(selection).unwrap_or(usize::MAX))
            .cloned()
        else {
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
    bind_toggle(wiring, toggle_favorite, list, search, status);
    bind_playback(wiring, list, status, episode_description, stream_url);
    let request = Rc::clone(&browser);
    let poll = Rc::clone(&timer);
    open.on_click(move |_| {
        let dialog = FileDialog::builder(&frame)
            .with_message("Open local playlist")
            .with_wildcard("M3U playlists (*.m3u;*.m3u8)|*.m3u;*.m3u8|All files (*.*)|*.*")
            .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
            .build();
        if dialog.show_modal() != ID_OK {
            return;
        }
        let Some(path) = dialog.get_path() else {
            return;
        };
        request.borrow_mut().load(std::path::PathBuf::from(path));
        status
            .set_label("Loading playlist. Cancel loading or open another playlist to replace it.");
        assert!(poll.start(50, false));
    });
    let cancel_browser = Rc::clone(&browser);
    cancel.on_click(move |_| {
        if cancel_browser.borrow_mut().cancel_load() {
            status.set_label(
                "Load cancelled. Previous channels kept. Open playlist to choose a file or retry.",
            );
        }
    });
}

/// The Toggle favorite action: toggles the channel behind the selected or
/// focused Channels row and announces the result without moving focus.
fn bind_toggle(
    wiring: &Wiring,
    toggle_favorite: Button,
    list: ListCtrl,
    search: TextCtrl,
    status: Status,
) {
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    let model = Rc::clone(&wiring.model);
    let persist = wiring.persist.clone();
    toggle_favorite.on_click(move |_| {
        toggle_selected_favorite(&browser, &model, &persist, &timer, list, search, status);
    });
}

/// Toggle the favorite state of the channel behind the selected or
/// focused Channels row (shared by the button and the View menu).
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
                    wiring.epg_worker.load(epg_urls);
                    wiring.epg_pending.set(true);
                    assert!(wiring.timer.start(50, false));
                }
                for failure in failures {
                    status.set_label(&failure);
                }
            }
            ProviderEvent::Resolved(result) => {
                let pending = wiring.pending_resolve.borrow_mut().take();
                match (pending, result) {
                    (Some(mut channel), Ok(url)) => {
                        channel.url = url;
                        play_channel(&wiring.player, &wiring.persist, &channel, status);
                    }
                    (_, Err(error)) => status.set_label(&error),
                    (None, Ok(_)) => {}
                }
            }
            ProviderEvent::Account(result) => match result {
                Ok(report) => {
                    let first = report.lines().next().unwrap_or_default().to_string();
                    status.set_label(&first);
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
            Ok(guide) => {
                let counts = (guide.channels.len(), guide.programme_count());
                let indexes = GuideIndexes::build(&guide.channels);
                *wiring.epg.borrow_mut() = Some((guide, indexes));
                status.set_label(&format!(
                    "Guide loaded: {} channels, {} programmes.",
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

/// Enter (item activation) on Channels plays the selected channel through
/// the configured player: the built-in window, or an external player.
fn bind_recording(wiring: &Wiring, controls: Controls) {
    let record = controls.record;
    let list = controls.list;
    let status = controls.status;
    let browser = Rc::clone(&wiring.browser);
    let model = Rc::clone(&wiring.model);
    let recordings = Rc::clone(&wiring.recordings);
    let persist = wiring.persist.clone();
    let timer = Rc::clone(&wiring.timer);
    record.on_click(move |_| {
        let Some(channel) = selected_channel(&browser, &model, list) else {
            status.set_label("No channel selected. Choose a channel in Channels first.");
            return;
        };
        toggle_recording(&recordings, &persist, &channel, status);
        assert!(timer.start(50, false));
    });
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
    persist: &Persist,
    channel: &Channel,
    status: Status,
) {
    let key = channel_key(channel);
    if recordings.borrow().is_recording(&key) {
        recordings.borrow_mut().stop(&key);
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
    persist: &Persist,
    key: &str,
    channel: &Channel,
    format: &str,
    display_name: &str,
) -> Result<PathBuf, String> {
    let dir = recordings_dir(persist);
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
    let cmd = recording::ffmpeg_command(
        "ffmpeg",
        &clean_url,
        &out.to_string_lossy(),
        format,
        &headers,
        &recording::FfmpegOptions {
            show_stats: true,
            ..recording::FfmpegOptions::default()
        },
    );
    recordings.borrow_mut().start(key, &cmd, out.clone())?;
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
        wiring.dvr.borrow_mut().mark_finished(
            job_id,
            false,
            "",
            "Scheduled recordings from portal channels are not supported yet.",
            now,
        );
        return;
    }
    let display = if job.title.is_empty() {
        channel.name.clone()
    } else {
        job.title.clone()
    };
    match start_recording(
        &wiring.recordings,
        &wiring.persist,
        &format!("dvr:{job_id}"),
        &channel,
        &job.format.clone(),
        &display,
    ) {
        Ok(_) => status.set_label(&format!("Recording {display}.")),
        Err(error) => {
            wiring
                .dvr
                .borrow_mut()
                .mark_finished(job_id, false, "", &error, now);
        }
    }
}

/// Per-tick recording housekeeping: collect finishes, run the DVR
/// state machine (every 10 s, like the reference poll), and apply the
/// shutdown-after-recordings gate.
fn poll_recordings(wiring: &Wiring, status: Status) {
    let now = now_seconds();
    for finished in wiring.recordings.borrow_mut().poll_finished() {
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
            status.set_label(&format!("Recording saved: {name}"));
        } else {
            status.set_label(&format!("Recording failed: {name}"));
        }
    }
    if now - wiring.last_dvr_tick.get() >= 10.0 {
        wiring.last_dvr_tick.set(now);
        let actions = wiring.dvr.borrow_mut().tick(now);
        for action in actions {
            match action {
                DvrAction::Stop(job_id) => {
                    wiring
                        .recordings
                        .borrow_mut()
                        .stop(&format!("dvr:{job_id}"));
                }
                DvrAction::Start(job_id) => start_dvr_job(wiring, status, &job_id, now),
            }
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
    let ui = ensure_cast_ui(
        &wiring.cast_ui,
        &wiring.cast_worker,
        &wiring.cast_pending,
        &wiring.timer,
    );
    ui.frame.show(true);
    ui.frame.raise();
    let channel = selected_channel(&wiring.browser, &wiring.model, list);
    ui.view.borrow_mut().channel.clone_from(&channel);
    match &channel {
        Some(channel) => {
            ui.status
                .set_label(&format!("Looking for cast devices for {}...", channel.name));
        }
        None => {
            ui.status
                .set_label("Choose a channel in the main window, then discover.");
        }
    }
    wiring.cast_worker.discover();
    wiring.cast_pending.set(true);
    assert!(wiring.timer.start(50, false));
}

fn ensure_cast_ui(
    slot: &Rc<RefCell<Option<CastUi>>>,
    worker: &Rc<CastWorker>,
    pending: &Rc<std::cell::Cell<bool>>,
    timer: &Rc<Timer<Frame>>,
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
    {
        let view = Rc::clone(&ui.view);
        let devices_box = devices;
        let worker = Rc::clone(worker);
        let pending = Rc::clone(pending);
        let timer = Rc::clone(timer);
        play.on_click(move |_| {
            let Some(selection) = devices_box.get_selection() else {
                return;
            };
            let view = view.borrow();
            let (Some(device), Some(channel)) =
                (view.devices.get(selection as usize), view.channel.as_ref())
            else {
                return;
            };
            let (clean_url, _) = iptv_core::playback::split_stream_modifiers(&channel.url);
            worker.play(device.clone(), clean_url, channel.name.clone());
            pending.set(true);
            assert!(timer.start(50, false));
            status.set_label(&format!("Casting {} to {}...", channel.name, device.name));
        });
    }
    {
        let view = Rc::clone(&ui.view);
        let devices_box = devices;
        let worker = Rc::clone(worker);
        stop.on_click(move |_| {
            let Some(selection) = devices_box.get_selection() else {
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
        epg_worker: Rc::clone(&wiring.epg_worker),
        epg_pending: Rc::clone(&wiring.epg_pending),
        reminders: Rc::clone(&wiring.reminders),
        provider_worker: Rc::clone(&wiring.provider_worker),
        provider_pending: Rc::clone(&wiring.provider_pending),
        pending_resolve: Rc::clone(&wiring.pending_resolve),
        recordings: Rc::clone(&wiring.recordings),
        cast_worker: Rc::clone(&wiring.cast_worker),
        cast_pending: Rc::clone(&wiring.cast_pending),
        cast_ui: Rc::clone(&wiring.cast_ui),
        guide_ui: Rc::clone(&wiring.guide_ui),
        update_worker: Rc::clone(&wiring.update_worker),
        update_pending: Rc::clone(&wiring.update_pending),
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

/// The tray icon (kept alive on the wiring): tooltip plus a
/// Show/Exit popup menu.
fn build_tray_icon() -> TaskBarIcon {
    let icon = TaskBarIcon::builder()
        .with_tooltip("Accessible IPTV Client")
        .build();
    if let Some(bitmap) = Bitmap::new(16, 16) {
        icon.set_icon(&bitmap, "Accessible IPTV Client");
    }
    let mut popup = Menu::builder()
        .append_item(5004, "&Show Accessible IPTV Client", "")
        .append_item(4004, "E&xit", "")
        .build();
    icon.set_popup_menu(&mut popup);
    icon
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
pub const CUSTOM_PLAYER: i32 = 5141;
pub const PREFERRED_AUDIO: i32 = 5142;
pub const LANGUAGE_BASE: i32 = 5150;
pub const MINIMIZE_TO_TRAY: i32 = 5171;
pub const SHOW_PLAYER_ON_ENTER: i32 = 5172;
pub const SHOW_STREAM_URL: i32 = 5173;
pub const AUTO_CHECK_UPDATES: i32 = 5174;
pub const EXPORT_SETTINGS: i32 = 5175;
pub const IMPORT_SETTINGS: i32 = 5176;
pub const ANNOUNCE_BASE: i32 = 5180;
pub const SPEAK_SUBTITLES: i32 = 5185;
pub const READ_CURRENT_SUBTITLE: i32 = 5186;
pub const KEYBOARD_SHORTCUTS: i32 = 5190;
pub const WHATS_NEW: i32 = 5191;
pub const OPEN_LOGS: i32 = 5192;
pub const COPY_DEBUG: i32 = 5193;
pub const ABOUT: i32 = 5194;

/// The "Media Player to Use" radio labels, in Python's order.
const PLAYER_LABELS: [&str; 10] = [
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
];

/// Append one item to a menu under construction.
fn menu_add(menu: &Menu, id: i32, label: &str, help: &str, kind: ItemKind) {
    menu.append(id, label, help, kind);
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
    let config = wiring.persist.config.borrow();
    let raw = serde_json::Value::Object(config.raw().clone());
    let keys = iptv_core::shortcuts::effective(&raw, "main");
    let file_menu = build_file_menu(&keys);
    let player_menu = build_player_menu(&keys);
    let view_menu = build_view_menu(&keys);
    let options_menu = build_options_menu(&config);
    let help_menu = build_help_menu();
    let menu_bar = MenuBar::builder()
        .append(file_menu, "&File")
        .append(player_menu, "&Player")
        .append(view_menu, "&View")
        .append(options_menu, "&Options")
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
    file_menu.enable_item(PLAYLIST_MANAGER, false);
    file_menu.enable_item(EPG_MANAGER, false);
    file_menu.enable_item(WHATS_ON_NOW, false);
    file_menu.enable_item(ACCOUNT_INFO, false);

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
    view_menu.enable_item(CONTINUE_WATCHING, false);
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
    view_menu.enable_item(RECENT_CHANNELS, false);
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
    view_menu.enable_item(SHOW_DOWNLOADS, false);

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
    for (offset, name) in ["Off", "Minimal", "Important events", "Detailed status"]
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
    options_menu.enable_item(PREFERRED_AUDIO, false);
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
    options_menu.enable_item(KEYBOARD_SHORTCUTS, false);
    menu_add(
        &options_menu,
        5003,
        "Check for &Updates",
        "Check GitHub for a newer release",
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
    help_menu.enable_item(WHATS_NEW, false);
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
    menu_add(&help_menu, ABOUT, "&About...", "", ItemKind::Normal);

    help_menu
}

/// Dispatch for the parity menu tree.
fn bind_menu_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    bind_menu_option_actions(frame, wiring, controls);
    let status = controls.status;
    let list = controls.list;
    let search = controls.search;
    let category_box = controls.category;
    let wiring = clone_wiring(wiring);
    frame.on_menu(move |event| {
        let id = event.get_id();
        let persist = &wiring.persist;
        match id {
            4004 => frame.close(false),
            5002 => open_guide_window(&wiring, status),
            5003 => {
                start_update_check(&wiring);
                status.set_label("Checking for updates...");
            }
            5004 => {
                frame.show(true);
                frame.raise();
            }
            IMPORT_EPG => menu_import_epg(&wiring, frame, status),
            WHAT_IS_PLAYING => {
                let player = wiring.player.borrow();
                match &player.current_channel {
                    Some(channel) => {
                        status.set_label(&format!("Now playing: {}", channel.name));
                    }
                    None => status.set_label("Nothing is playing."),
                }
            }
            CAST_TO | CAST_CONNECT => open_cast_window(&wiring, list),
            SHOW_PLAYER => {
                let player = wiring.player.borrow();
                match &player.window {
                    Some((player_frame, _)) => {
                        player_frame.show(true);
                        player_frame.raise();
                    }
                    None => {
                        status.set_label("The built-in player opens when you play a channel.");
                    }
                }
            }
            PLAY_PAUSE => {
                let player = wiring.player.borrow();
                if let Some((_, media)) = &player.window {
                    if media.get_state() == MediaState::Playing {
                        media.pause();
                    } else {
                        media.play();
                    }
                }
            }
            STOP => {
                let mut player = wiring.player.borrow_mut();
                if let Some((_, media)) = &player.window {
                    media.stop();
                }
                player.session.stop();
            }
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
                category_box.set_selection(1, true);
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
fn bind_menu_option_actions(frame: Frame, wiring: &Wiring, controls: Controls) {
    let status = controls.status;
    let stream_url = controls.stream_url;
    let wiring = clone_wiring(wiring);
    frame.on_menu(move |event| {
        let id = event.get_id();
        if id < 5130 {
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
            READ_CURRENT_SUBTITLE => menu_read_current_subtitle(&wiring, status),
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
            _ if (PLAYER_RADIO_BASE..PLAYER_RADIO_BASE + 10).contains(&id) => {
                let index = usize::try_from(id - PLAYER_RADIO_BASE).unwrap_or(0);
                if let Some(name) = PLAYER_LABELS.get(index) {
                    persist.set_and_save("media_player", serde_json::json!(name));
                    status.set_label(&format!("Media player set to {name}."));
                }
            }
            _ if (LANGUAGE_BASE..LANGUAGE_BASE + 15).contains(&id) => {
                let index = usize::try_from(id - LANGUAGE_BASE).unwrap_or(0);
                if let Some((code, name)) = iptv_core::i18n::language_labels().get(index) {
                    persist.set_and_save("language", serde_json::json!(code));
                    status.set_label(&format!(
                        "Language set to {name}. Restart to apply everywhere."
                    ));
                }
            }
            _ if (ANNOUNCE_BASE..ANNOUNCE_BASE + 4).contains(&id) => {
                persist.set_and_save(
                    "announcement_level",
                    serde_json::json!(id - ANNOUNCE_BASE),
                );
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
    wiring.epg_worker.load(vec![path]);
    wiring.epg_pending.set(true);
    assert!(wiring.timer.start(50, false));
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
fn menu_export_settings(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Export settings")
        .with_default_file("iptvclient-settings.json")
        .with_wildcard("JSON (*.json)|*.json|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    match std::fs::copy(&wiring.persist.path, &path) {
        Ok(_) => status.set_label("Settings exported."),
        Err(error) => status.set_label(&format!("Export failed: {error}")),
    }
}

/// Options > Import Settings...: replace the config file with a
/// previously exported one (validated as a JSON object first).
fn menu_import_settings(wiring: &Wiring, frame: Frame, status: Status) {
    let dialog = FileDialog::builder(&frame)
        .with_message("Import settings")
        .with_wildcard("JSON (*.json)|*.json|All files (*.*)|*.*")
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if dialog.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dialog.get_path() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        status.set_label("Import failed: cannot read that file.");
        return;
    };
    let valid =
        serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|value| value.is_object());
    if !valid {
        status.set_label("Import failed: that file is not a settings file.");
        return;
    }
    match std::fs::write(&wiring.persist.path, text) {
        Ok(()) => status.set_label("Settings imported. Restart to apply."),
        Err(error) => status.set_label(&format!("Import failed: {error}")),
    }
}

/// Subtitle Speech > Read current subtitle: announce the cue active
/// at the player's current position.
fn menu_read_current_subtitle(wiring: &Wiring, status: Status) {
    let player = wiring.player.borrow();
    let Some((_, media)) = &player.window else {
        status.set_label("No subtitle is showing.");
        return;
    };
    let position = media.tell();
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

/// Start a background update check at launch when the user enabled
/// auto-check in Options.
fn maybe_auto_check_updates(wiring: &Wiring) {
    if wiring
        .persist
        .config
        .borrow()
        .bool_or("auto_check_updates", false)
    {
        start_update_check(wiring);
    }
}

/// Kick a background update check and arm the completion poll.
fn start_update_check(wiring: &Wiring) {
    wiring.update_worker.check(
        "serrebidev",
        "Accessible-IPTV-Client-Rust",
        "update-manifest.json",
        env!("CARGO_PKG_VERSION"),
    );
    wiring.update_pending.set(true);
    assert!(wiring.timer.start(50, false));
}

/// Collect update-check outcomes onto the status line.
fn poll_updates(wiring: &Wiring, status: Status) {
    while let Some(event) = wiring.update_worker.try_recv() {
        wiring.update_pending.set(false);
        match event {
            UpdateEvent::Checked { manifest, newer } => {
                if newer {
                    status.set_label(&format!(
                        "Version {} is available. You have {}.",
                        manifest.version,
                        env!("CARGO_PKG_VERSION")
                    ));
                } else {
                    status.set_label(&format!(
                        "Accessible IPTV Client is up to date ({}).",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
            }
            UpdateEvent::Failed(error) => {
                status.set_label(&format!("Update check failed: {error}"));
            }
        }
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

fn bind_playback(
    wiring: &Wiring,
    list: ListCtrl,
    status: Status,
    episode_description: TextCtrl,
    stream_url: TextCtrl,
) {
    let previous = Rc::clone(&wiring.previous_channel);
    let browser = Rc::clone(&wiring.browser);
    let model = Rc::clone(&wiring.model);
    let player = Rc::clone(&wiring.player);
    let persist = wiring.persist.clone();
    let provider_worker = Rc::clone(&wiring.provider_worker);
    let pending_resolve = Rc::clone(&wiring.pending_resolve);
    let timer = Rc::clone(&wiring.timer);
    list.on_item_activated(move |_| {
        let selected = usize::try_from(list.get_first_selected_item()).ok();
        let focused =
            usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused))
                .ok();
        let Some(row) = pick_row(selected, focused) else {
            status.set_label("No channel selected. Choose a channel in Channels first.");
            return;
        };
        let Some(index) = model.borrow().playlist_index(row) else {
            status.set_label("No channel selected. Choose a channel in Channels first.");
            return;
        };
        let Some(channel) = browser.borrow().channel(index) else {
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
            *pending_resolve.borrow_mut() = Some(channel.clone());
            provider_worker.resolve(key, channel.unknown_attributes.clone());
            status.set_label("Resolving stream link...");
            assert!(timer.start(50, false));
            return;
        }
        if let Some(current) = player.borrow().current_channel.clone() {
            if current.url != channel.url {
                *previous.borrow_mut() = Some(current);
            }
        }
        play_channel(&player, &persist, &channel, status);
    });
    bind_epg_highlight(wiring, list, status, episode_description, stream_url);
}

/// Moving through Channels announces the highlighted channel's now/next
/// programme from the loaded guide, when it has one.
fn bind_epg_highlight(
    wiring: &Wiring,
    list: ListCtrl,
    status: Status,
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
        let guard = epg.borrow();
        let Some((guide, indexes)) = guard.as_ref() else {
            episode_description.set_value("");
            return;
        };
        let known: std::collections::HashSet<String> =
            guide.channels.iter().map(|c| c.id.to_lowercase()).collect();
        let Some(guide_id) = match_guide_channel(&channel, indexes, Some(&known)) else {
            status.set_label("No guide information for this channel.");
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        let (current, next) = guide.now_next(&guide_id, now);
        let description_text = match &current {
            Some(p) if !p.description.is_empty() => p.description.clone(),
            Some(p) => p.title.clone(),
            None => String::new(),
        };
        episode_description.set_value(&description_text);
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = current {
            parts.push(format!("Now: {} until {}.", p.title, local_hhmm(p.stop)));
        }
        if let Some(p) = next {
            parts.push(format!("Next: {} at {}.", p.title, local_hhmm(p.start)));
        }
        if parts.is_empty() {
            status.set_label("No guide information for this channel.");
        } else {
            status.set_label(&parts.join(" "));
        }
    });
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
    ensure_player_window(player, persist, status);
    let mut state = player.borrow_mut();
    state.current_channel = Some(channel.clone());
    state.pending_resume_ms = request.resume_ms;
    state.subtitle_cues.clear();
    state.last_spoken_cue = None;
    let action = state.session.play(request);
    let Some((frame, media)) = state.window else {
        return;
    };
    if let EngineAction::Load(load) = action {
        media.load_uri(&load.url);
        media.play();
    }
    frame.set_title(&format!("{} — Player", state.session.announcement()));
    if persist
        .config
        .borrow()
        .bool_or("show_player_on_enter", true)
    {
        frame.show(true);
    }
    status.set_label(&state.session.announcement());
}

/// Build the built-in player window once: video surface, Pause/Resume and
/// Stop buttons, media events feeding the session, and a 1 s timer that
/// checkpoints resume positions for seekable content.
fn ensure_player_window(player: &Rc<RefCell<PlayerState>>, persist: &Persist, status: Status) {
    if player.borrow().window.is_some() {
        return;
    }
    let frame = Frame::builder()
        .with_title("Player")
        .with_size(Size::new(560, 380))
        .build();
    frame.set_accessibility_label("Player");
    let panel = Panel::builder(&frame).build();
    let media = MediaCtrl::builder(&panel).build();
    media.set_accessibility_label("Video");
    let pause = Button::builder(&panel)
        .with_label("&Pause or resume")
        .build();
    pause.set_accessibility_label("Pause or resume");
    let stop = Button::builder(&panel).with_label("&Stop").build();
    stop.set_accessibility_label("Stop");
    let subs = Button::builder(&panel)
        .with_label("Load &subtitles…")
        .build();
    subs.set_accessibility_label("Load subtitles");
    subs.set_accessibility_description(
        "Load an SRT, WebVTT or ASS subtitle file for this playback. When Speak subtitles is on in settings, cues are announced as they play.",
    );
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&pause, 0, SizerFlag::All, 4);
    buttons.add(&stop, 0, SizerFlag::All, 4);
    buttons.add(&subs, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    wire_player_events(player, persist, status, frame, media, pause, stop, subs);
    player.borrow_mut().window = Some((frame, media));
}

/// Timer, buttons, media events and close behavior for the player window.
#[allow(clippy::too_many_arguments)]
fn wire_player_events(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    frame: Frame,
    media: MediaCtrl,
    pause: Button,
    stop: Button,
    subs: Button,
) {
    let timer = Rc::new(Timer::new(&frame));
    let tick_player = Rc::clone(player);
    let tick_persist = persist.clone();
    let tick_media = media;
    timer.on_tick(move |_| {
        checkpoint_resume(&tick_player, &tick_persist, tick_media);
        speak_subtitle_cue(&tick_player, &tick_persist, tick_media, status);
    });
    let _ = timer.start(1000, false);

    bind_subtitle_load(subs, player, frame, status);
    let pause_player = Rc::clone(player);
    pause.on_click(move |_| {
        let mut state = pause_player.borrow_mut();
        state.session.toggle_pause();
        if let Some((_, media)) = state.window {
            match state.session.state() {
                SessionState::Paused => {
                    media.pause();
                }
                SessionState::Playing => {
                    media.play();
                }
                _ => {}
            }
        }
        status.set_label(&state.session.announcement());
    });
    let stop_player = Rc::clone(player);
    let stop_persist = persist.clone();
    stop.on_click(move |_| {
        checkpoint_resume(&stop_player, &stop_persist, media);
        let mut state = stop_player.borrow_mut();
        state.session.stop();
        if let Some((frame, media)) = state.window {
            media.stop();
            frame.show(false);
        }
        status.set_label("Stopped.");
    });
    let loaded_player = Rc::clone(player);
    media.on_loaded(move |_| {
        let mut state = loaded_player.borrow_mut();
        state.session.on_event(EngineEvent::Started);
        if let (Some((_, media)), Some(ms)) = (state.window, state.pending_resume_ms.take()) {
            media.seek(ms, SeekMode::FromStart);
        }
        status.set_label(&state.session.announcement());
    });
    let stopped_player = Rc::clone(player);
    media.on_stop(move |_| {
        let mut state = stopped_player.borrow_mut();
        let near_end = state
            .window
            .is_some_and(|(_, media)| media.length() > 0 && media.tell() >= media.length() - 1500);
        let event = if near_end {
            EngineEvent::Ended
        } else {
            EngineEvent::Stalled
        };
        if let EngineAction::Reconnect(request) = state.session.on_event(event) {
            if let Some((_, media)) = state.window {
                media.load_uri(&request.url);
                media.play();
            }
        }
        status.set_label(&state.session.announcement());
    });
    let close_player = Rc::clone(player);
    let close_persist = persist.clone();
    frame.on_close(move |event| {
        checkpoint_resume(&close_player, &close_persist, media);
        let mut state = close_player.borrow_mut();
        state.session.stop();
        if let Some((frame, media)) = state.window {
            media.stop();
            frame.show(false);
        }
        event.skip(false);
    });
}

/// The player window's Load subtitles button: pick a file, parse it,
/// and hold its cues on the player state for the speech path.
fn bind_subtitle_load(
    subs: Button,
    player: &Rc<RefCell<PlayerState>>,
    frame: Frame,
    status: Status,
) {
    let subs_player = Rc::clone(player);
    subs.on_click(move |_| {
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
                status.set_label(&format!("Loaded {count} subtitle cues."));
            }
            Err(error) => {
                status.set_label(&format!("Could not load subtitles: {error}"));
            }
        }
    });
}

/// Announce the active subtitle cue when it changes, while the
/// `speak_subtitles` setting is on (the reference's spoken-subtitle
/// path, riding the live region).
fn speak_subtitle_cue(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    media: MediaCtrl,
    status: Status,
) {
    if !persist.config.borrow().bool_or("speak_subtitles", false) {
        return;
    }
    let position = media.tell();
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
fn checkpoint_resume(player: &Rc<RefCell<PlayerState>>, persist: &Persist, media: MediaCtrl) {
    let state = player.borrow();
    if state.session.current().is_none_or(|r| r.live) {
        return;
    }
    let Some(channel) = &state.current_channel else {
        return;
    };
    let key = channel_key(channel);
    let channel = channel.clone();
    drop(state);
    let position = media.tell();
    let length = media.length();
    if position <= 0 || length <= 0 {
        return;
    }
    let _ = channel;
    let mut entries = persist.resume_entries();
    resume_checkpoint(&mut entries, &key, position, length, now_ms() / 1000);
    persist.save_resume(&entries);
}

/// The row an action applies to: the selected row, else the focused row.
fn pick_row(selected: Option<usize>, focused: Option<usize>) -> Option<usize> {
    selected.or(focused)
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
    control: ListBox,
    state: &RefCell<Vec<Category>>,
    browser: &RefCell<BrowserWorker>,
) {
    let browser = browser.borrow();
    let current = browser.categories();
    let favorite_count = browser.favorite_count();
    if *state.borrow() != current {
        let labels: Vec<String> = current
            .iter()
            .map(|category| category_display(category, favorite_count))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        control.set_items(&refs);
        *state.borrow_mut() = current;
    } else if browser.active_category() == &Category::Favorites {
        // The count in the Favorites label can change without a set change.
        let labels: Vec<String> = state
            .borrow()
            .iter()
            .map(|category| category_display(category, favorite_count))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        control.set_items(&refs);
    }
    let active = browser.active_category().clone();
    if let Some(position) = state.borrow().iter().position(|item| item == &active) {
        let position = u32::try_from(position).unwrap_or(u32::MAX);
        if control.get_selection() != Some(position) {
            control.set_selection(position, true);
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

fn channel_text(model: &SearchResults, index: i64, column: i32) -> String {
    if column != 0 {
        return String::new();
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| model.channel(index))
        .map_or_else(String::new, |channel| {
            if channel.name.is_empty() {
                // Stream URLs can contain credentials; never use them as row labels.
                "Unnamed channel".to_owned()
            } else {
                channel.name.clone()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_core::{search::SearchWorker, BrowseModel};
    use std::sync::Arc;

    #[test]
    fn empty_list_has_no_placeholder_or_stale_accessible_rows() {
        let model = SearchResults::default();
        for index in [-1, 0, 1, i64::MAX] {
            assert_eq!(channel_text(&model, index, 0), "");
            assert_eq!(channel_text(&model, index, 1), "");
        }
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
        assert_eq!(channel_text(&model, 0, 0), "ABC East HD");
        assert_eq!(channel_text(&model, 1, 0), "映画, HD");
        assert_eq!(channel_text(&model, 2, 0), "Bob's Burgers, Season 1");
        assert_eq!(channel_text(&model, 3, 0), "Unnamed channel");
        assert_eq!(channel_text(&model, 4, 0), "");
        assert_eq!(channel_text(&model, -1, 0), "");
        assert_eq!(channel_text(&model, 0, 1), "");
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

use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    categories::Category,
    config::{self, Config},
    favorites::{channel_key, FavoriteSet, Toggle},
    playback::{
        candidate_executables, detect_from_path, external_argv, player_choice, resume_checkpoint,
        resume_position, EngineAction, EngineEvent, LaunchGate, PlaybackRequest, PlaybackSession,
        PlayerChoice, SessionState,
    },
    search::SearchResults,
    sources::PlaylistSource,
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
        let open = Button::builder(&panel)
            .with_label("&Open playlist…")
            .build();
        open.set_accessibility_label("Open playlist");
        let cancel = Button::builder(&panel)
            .with_label("&Cancel loading")
            .build();
        cancel.set_accessibility_label("Cancel loading");
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
        };
        bind_browser(
            frame,
            Controls {
                open,
                cancel,
                category,
                toggle_favorite,
                search,
                list,
                status,
            },
            model,
        );
        let layout = BoxSizer::builder(Orientation::Vertical).build();
        layout.add(&open, 0, SizerFlag::All, 8);
        layout.add(&cancel, 0, SizerFlag::All, 8);
        layout.add(&category_label, 0, SizerFlag::All, 8);
        layout.add(&category, 0, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&toggle_favorite, 0, SizerFlag::All, 8);
        layout.add(&search_label, 0, SizerFlag::All, 8);
        layout.add(&search, 0, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&label, 0, SizerFlag::All, 8);
        layout.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&status_label, 0, SizerFlag::All, 8);
        layout.add(&details, 0, SizerFlag::Expand | SizerFlag::All, 8);
        panel.set_sizer(layout, true);

        let frame_layout = BoxSizer::builder(Orientation::Vertical).build();
        frame_layout.add(&panel, 1, SizerFlag::Expand, 0);
        frame.set_sizer(frame_layout, true);

        frame.show(true);
        frame.centre();
        list.set_focus();
    })
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
}

impl Status {
    fn set_label(self, message: &str) {
        self.details.change_value(message);
        live_region::announce_with_priority(self.label, message, live_region::Priority::Medium);
    }

    fn set_silent(self, message: &str) {
        self.details.change_value(message);
    }
}

#[derive(Clone, Copy)]
struct Controls {
    open: Button,
    cancel: Button,
    category: ListBox,
    toggle_favorite: Button,
    search: TextCtrl,
    list: ListCtrl,
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
}

/// Built-in player window state, created lazily on first use.
struct PlayerState {
    session: PlaybackSession,
    gate: LaunchGate,
    window: Option<(Frame, MediaCtrl)>,
    current_channel: Option<Channel>,
    pending_resume_ms: Option<i64>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            session: PlaybackSession::new(),
            gate: LaunchGate::default(),
            window: None,
            current_channel: None,
            pending_resume_ms: None,
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

fn bind_browser(frame: Frame, controls: Controls, model: Rc<RefCell<SearchResults>>) {
    let candidates = config::config_candidates();
    let loaded = config::load(&candidates, &candidates[0]);
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
    };
    let configured = wiring.persist.config.borrow().string_list("playlists");
    if !configured.is_empty() {
        let sources: Vec<PlaylistSource> =
            configured.into_iter().map(PlaylistSource::new).collect();
        wiring.browser.borrow_mut().load_sources(sources);
        assert!(wiring.timer.start(50, false));
    }
    bind_completion(&wiring, controls);
    bind_actions(frame, &wiring, controls);
    let browser = Rc::clone(&wiring.browser);
    let timer = Rc::clone(&wiring.timer);
    frame.on_close(move |event| {
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
        if !completed.browser.borrow().is_busy() {
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
        category,
        toggle_favorite,
        search,
        list,
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
    bind_playback(wiring, list, status);
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
    });
}

/// Enter (item activation) on Channels plays the selected channel through
/// the configured player: the built-in window, or an external player.
fn bind_playback(wiring: &Wiring, list: ListCtrl, status: Status) {
    let browser = Rc::clone(&wiring.browser);
    let model = Rc::clone(&wiring.model);
    let player = Rc::clone(&wiring.player);
    let persist = wiring.persist.clone();
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
        play_channel(&player, &persist, &channel, status);
    });
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
    let action = state.session.play(request);
    let Some((frame, media)) = state.window else {
        return;
    };
    if let EngineAction::Load(load) = action {
        media.load_uri(&load.url);
        media.play();
    }
    frame.set_title(&format!("{} — Player", state.session.announcement()));
    frame.show(true);
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
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    sizer.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&pause, 0, SizerFlag::All, 4);
    buttons.add(&stop, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    wire_player_events(player, persist, status, frame, media, pause, stop);
    player.borrow_mut().window = Some((frame, media));
}

/// Timer, buttons, media events and close behavior for the player window.
fn wire_player_events(
    player: &Rc<RefCell<PlayerState>>,
    persist: &Persist,
    status: Status,
    frame: Frame,
    media: MediaCtrl,
    pause: Button,
    stop: Button,
) {
    let timer = Rc::new(Timer::new(&frame));
    let tick_player = Rc::clone(player);
    let tick_persist = persist.clone();
    let tick_media = media;
    timer.on_tick(move |_| {
        checkpoint_resume(&tick_player, &tick_persist, tick_media);
    });
    let _ = timer.start(1000, false);

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

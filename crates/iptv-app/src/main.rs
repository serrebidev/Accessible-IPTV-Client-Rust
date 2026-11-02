use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    categories::Category,
    config::{self, Config},
    favorites::{FavoriteSet, Toggle},
    search::SearchResults,
};
use std::{cell::RefCell, path::PathBuf, rc::Rc};
use wxdragon::event::EventType;
use wxdragon::prelude::*;
use wxdragon::timer::Timer;

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
    };
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

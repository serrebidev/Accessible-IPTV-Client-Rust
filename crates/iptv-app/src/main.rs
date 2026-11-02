use iptv_core::{
    browser::{BrowseEvent, BrowserWorker},
    category::Category,
    favorites::ToggleOutcome,
    search::SearchResults,
};
use std::{cell::RefCell, rc::Rc};
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
        let category_label = StaticText::builder(&panel)
            .with_label("&Categories")
            .build();
        let category = Choice::builder(&panel).build();
        category.set_accessibility_label("Categories");
        category.set_accessibility_description("Choose All Channels, Favorites or a playlist group with arrow keys. Tab moves to Search channels; Shift+Tab returns to Cancel loading.");
        category.append("All Channels");
        category.append("Favorites");
        category.set_selection(0);
        let search_label = StaticText::builder(&panel)
            .with_label("&Search channels")
            .build();
        let search = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        search.set_accessibility_label("Search channels");
        search.set_accessibility_description("Type to filter channel names. Tab or Enter moves to Channels; Shift+Tab returns to Categories.");
        // Keep the label immediately before the list for native MSW accessible naming.
        let label = StaticText::builder(&panel).with_label("Channels").build();
        let list = build_channel_list(panel);

        let model = Rc::new(RefCell::new(SearchResults::default()));
        let rows = Rc::clone(&model);
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(&rows.borrow(), index, column)
        }));
        list.set_item_count(0);

        let favorite = Button::builder(&panel)
            .with_label("Toggle &favorite")
            .build();
        favorite.set_accessibility_label("Toggle favorite");
        favorite.set_accessibility_description("Add or remove the focused channel from Favorites. Tab moves to Playlist status; Shift+Tab returns to Channels.");

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
            open,
            cancel,
            Filters {
                search,
                category,
                favorite,
            },
            list,
            status,
            model,
        );
        let layout = BoxSizer::builder(Orientation::Vertical).build();
        layout.add(&open, 0, SizerFlag::All, 8);
        layout.add(&cancel, 0, SizerFlag::All, 8);
        layout.add(&category_label, 0, SizerFlag::All, 8);
        layout.add(&category, 0, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&search_label, 0, SizerFlag::All, 8);
        layout.add(&search, 0, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&label, 0, SizerFlag::All, 8);
        layout.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&favorite, 0, SizerFlag::All, 8);
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

fn build_channel_list(panel: Panel) -> ListCtrl {
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
    list
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
}

#[derive(Clone, Copy)]
struct Filters {
    search: TextCtrl,
    category: Choice,
    favorite: Button,
}

fn bind_browser(
    frame: Frame,
    open: Button,
    cancel: Button,
    filters: Filters,
    list: ListCtrl,
    status: Status,
    model: Rc<RefCell<SearchResults>>,
) {
    let Filters {
        search,
        category,
        favorite: _,
    } = filters;
    let browser = Rc::new(RefCell::new(BrowserWorker::default()));
    let timer = Rc::new(Timer::new(&frame));
    let completed_timer = Rc::downgrade(&timer);
    let completed_browser = Rc::clone(&browser);
    let completed_model = Rc::clone(&model);
    timer.on_tick(move |_| {
        let event = completed_browser.borrow_mut().poll(&search.get_value());
        match event {
            Some(BrowseEvent::Results(results)) => {
                publish_categories(category, &results, &completed_model.borrow());
                publish_results(list, status, &completed_model, results);
            }
            Some(BrowseEvent::Recovery(error)) => {
                status.set_label(error.message());
                list.set_accessibility_description(error.message());
            }
            None => {}
        }
        if !completed_browser.borrow().is_busy() {
            if let Some(timer) = completed_timer.upgrade() {
                timer.stop();
            }
        }
    });
    let category_browser = Rc::clone(&browser);
    let poll_category = Rc::clone(&timer);
    let category_model = Rc::clone(&model);
    category.on_selection_changed(move |_| {
        let selected = category.get_selection().and_then(|index| {
            category_model
                .borrow()
                .categories()
                .get(index as usize)
                .cloned()
        });
        if let Some(selected) = selected {
            category_browser
                .borrow_mut()
                .select_category(selected, search.get_value());
            status.details.change_value("Updating category channels...");
            assert!(poll_category.start(50, false));
        }
    });
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

    bind_favorite(
        filters,
        list,
        status,
        model,
        Rc::clone(&browser),
        Rc::clone(&timer),
    );

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
    frame.on_close(move |event| {
        browser.borrow_mut().cancel();
        timer.stop();
        event.skip(true);
    });
}

fn bind_favorite(
    filters: Filters,
    list: ListCtrl,
    status: Status,
    model: Rc<RefCell<SearchResults>>,
    browser: Rc<RefCell<BrowserWorker>>,
    timer: Rc<Timer<Frame>>,
) {
    let Filters {
        search, favorite, ..
    } = filters;
    let favorite_browser = browser;
    let poll_favorite = timer;
    favorite.on_click(move |_| {
        let focused =
            usize::try_from(list.get_next_item(-1, ListNextItemFlag::All, ListItemState::Focused))
                .ok();
        let message = toggle_focused(
            &mut favorite_browser.borrow_mut(),
            &model.borrow(),
            focused,
            search.get_value(),
        );
        status.set_label(&message);
        if favorite_browser.borrow().is_busy() {
            assert!(poll_favorite.start(50, false));
        }
    });
}

fn publish_categories(category: Choice, results: &SearchResults, previous: &SearchResults) {
    if results.categories() != previous.categories() {
        category.clear();
        for entry in results.categories() {
            category.append(&category_text(entry));
        }
    }
    if let Some(index) = results
        .categories()
        .iter()
        .position(|entry| entry == results.category())
        .and_then(|index| u32::try_from(index).ok())
    {
        category.set_selection(index);
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
    status.set_label(&message);
    list.set_accessibility_description(&message);
    std::thread::spawn(move || drop(previous));
}

fn category_text(category: &Category) -> String {
    match category {
        Category::Group(name) if name == "All Channels" || name == "Favorites" => {
            format!("{name} (playlist group)")
        }
        _ => category.label().to_owned(),
    }
}

// A native button has one command handler for Space, Enter, mnemonic and mouse.
// This adapter never requests keyboard focus or uses a stream URL as feedback.
fn toggle_focused(
    browser: &mut BrowserWorker,
    model: &SearchResults,
    focused: Option<usize>,
    query: String,
) -> String {
    let Some(row) = focused else {
        return "Select a channel first. Favorites unchanged.".to_owned();
    };
    match browser.toggle_favorite(model, row, query) {
        Some(ToggleOutcome::Added) => format!(
            "{} added to Favorites.",
            channel_text(model, i64::try_from(row).unwrap_or(-1), 0)
        ),
        Some(ToggleOutcome::Removed) => format!(
            "{} removed from Favorites.",
            channel_text(model, i64::try_from(row).unwrap_or(-1), 0)
        ),
        Some(ToggleOutcome::Unkeyable) => "This entry cannot be added to Favorites.".to_owned(),
        None => "Select a current channel first. Favorites unchanged.".to_owned(),
    }
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
    fn category_identity_does_not_depend_on_the_accessible_label() {
        assert_eq!(category_text(&Category::All), "All Channels");
        assert_eq!(category_text(&Category::Favorites), "Favorites");
        assert_eq!(
            category_text(&Category::Group("Favorites".into())),
            "Favorites (playlist group)"
        );
        assert_eq!(
            category_text(&Category::Group("All Channels".into())),
            "All Channels (playlist group)"
        );
        assert_eq!(
            category_text(&Category::Group("News / Local".into())),
            "News / Local"
        );
    }

    fn wait(browser: &mut BrowserWorker) -> SearchResults {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = browser.poll("") {
                return match event {
                    BrowseEvent::Results(result) => result,
                    BrowseEvent::Recovery(error) => panic!("{error:?}"),
                };
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn favorite_action_reports_each_state_once_and_uses_only_real_focused_rows() {
        let mut browser = BrowserWorker::default();
        browser.load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../iptv-core/tests/fixtures/local.m3u"),
        );
        let all = wait(&mut browser);
        assert_eq!(
            toggle_focused(&mut browser, &all, None, String::new()),
            "Select a channel first. Favorites unchanged."
        );
        assert_eq!(
            toggle_focused(&mut browser, &all, Some(0), String::new()),
            "ABC East HD added to Favorites."
        );
        browser.select_category(Category::Favorites, String::new());
        let favorite = wait(&mut browser);
        assert_eq!(favorite.len(), 1);
        assert_eq!(channel_text(&favorite, 0, 0), "ABC East HD");
        assert_eq!(favorite.restore_row(&all, Some(0)), Some(0));
        assert_eq!(
            toggle_focused(&mut browser, &favorite, Some(0), String::new()),
            "ABC East HD removed from Favorites."
        );
        let empty = wait(&mut browser);
        assert_eq!(empty.len(), 0);
        assert_eq!(channel_text(&empty, 0, 0), "");
        assert!(empty.status().contains("Favorites is empty"));
        assert_eq!(empty.restore_row(&favorite, Some(0)), None);
        assert_eq!(
            toggle_focused(&mut browser, &empty, Some(0), String::new()),
            "Select a current channel first. Favorites unchanged."
        );
        browser.select_category(Category::All, String::new());
        let all = wait(&mut browser);
        assert_eq!(
            toggle_focused(&mut browser, &all, Some(3), String::new()),
            "This entry cannot be added to Favorites."
        );
        assert!(!browser.is_busy());
    }

    #[test]
    fn empty_list_has_no_placeholder_or_stale_accessible_rows() {
        let model = SearchResults::default();
        for index in [-1, 0, 1, i64::MAX] {
            assert_eq!(channel_text(&model, index, 0), "");
            assert_eq!(channel_text(&model, index, 1), "");
        }
    }

    #[test]
    fn category_projection_exposes_real_accessible_rows_and_empty_status() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../iptv-core/tests/fixtures/categories.m3u");
        let source = Arc::new(BrowseModel::open_local(&path).unwrap());
        let worker = SearchWorker::default();
        for (query, count) in [("", 2), ("absent", 0)] {
            worker.submit_category(
                Arc::clone(&source),
                query.to_owned(),
                Category::Group("News".into()),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let result = loop {
                if let Some(result) = worker.try_latest() {
                    break result;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            };
            assert_eq!(result.len(), count);
            assert_eq!(
                channel_text(&result, 0, 0),
                if count == 0 { "" } else { "Same" }
            );
            assert_eq!(channel_text(&result, 2, 0), "");
            assert!(result.status().contains("category News"));
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
}

use iptv_core::{
    search::{SearchResults, SearchWorker},
    BrowseModel,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{mpsc, Arc},
};
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
        let search_label = StaticText::builder(&panel)
            .with_label("&Search channels")
            .build();
        let search = TextCtrl::builder(&panel)
            .with_style(TextCtrlStyle::ProcessEnter)
            .build();
        search.set_accessibility_label("Search channels");
        search.set_accessibility_description("Type to filter channel names. Tab or Enter moves to Channels; Shift+Tab returns to Open playlist.");
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

        let status = StaticText::builder(&panel)
            .with_label("No playlist loaded. Channel list is empty.")
            .build();
        bind_browser(frame, open, search, list, status, model);
        let layout = BoxSizer::builder(Orientation::Vertical).build();
        layout.add(&open, 0, SizerFlag::All, 8);
        layout.add(&search_label, 0, SizerFlag::All, 8);
        layout.add(&search, 0, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&label, 0, SizerFlag::All, 8);
        layout.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 8);
        layout.add(&status, 0, SizerFlag::All, 8);
        panel.set_sizer(layout, true);

        let frame_layout = BoxSizer::builder(Orientation::Vertical).build();
        frame_layout.add(&panel, 1, SizerFlag::Expand, 0);
        frame.set_sizer(frame_layout, true);

        frame.show(true);
        frame.centre();
        list.set_focus();
    })
}

fn bind_browser(
    frame: Frame,
    open: Button,
    search: TextCtrl,
    list: ListCtrl,
    status: StaticText,
    model: Rc<RefCell<SearchResults>>,
) {
    let pending = Rc::new(RefCell::new(
        None::<mpsc::Receiver<std::io::Result<Arc<BrowseModel>>>>,
    ));
    let playlist = Rc::new(RefCell::new(Arc::new(BrowseModel::empty())));
    let worker = Rc::new(SearchWorker::default());
    let searching = Rc::new(Cell::new(false));
    let timer = Rc::new(Timer::new(&frame));
    let completed_timer = Rc::downgrade(&timer);
    let result = Rc::clone(&pending);
    let loaded_playlist = Rc::clone(&playlist);
    let completed_worker = Rc::clone(&worker);
    let completed_searching = Rc::clone(&searching);
    timer.on_tick(move |_| {
        let received = result.borrow().as_ref().map(mpsc::Receiver::try_recv);
        let loaded = match received {
            Some(Ok(loaded)) => Some(loaded),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Some(Err(std::io::Error::other("Playlist worker stopped")))
            }
            _ => None,
        };
        if let Some(loaded) = loaded {
            *result.borrow_mut() = None;
            if let Ok(loaded) = loaded {
                let previous = loaded_playlist.replace(Arc::clone(&loaded));
                std::thread::spawn(move || drop(previous));
                completed_worker.submit(loaded, search.get_value());
                completed_searching.set(true);
            } else {
                let message = "Could not open playlist. Choose another local UTF-8 playlist.";
                status.set_label(message);
                list.set_accessibility_description(message);
            }
        }
        if let Some(results) = completed_worker.try_latest() {
            publish_results(list, status, &model, results);
            completed_searching.set(false);
        }
        if result.borrow().is_none() && !completed_searching.get() {
            if let Some(timer) = completed_timer.upgrade() {
                timer.stop();
            }
        }
    });
    let search_worker = Rc::clone(&worker);
    let search_pending = Rc::clone(&searching);
    let poll_search = Rc::clone(&timer);
    search.on_text_changed(move |_| {
        search_worker.submit(Arc::clone(&playlist.borrow()), search.get_value());
        search_pending.set(true);
        status.set_label("Searching channels...");
        assert!(poll_search.start(50, false));
    });
    // Only an explicit user action moves keyboard focus. A late completion can
    // update item state but never interrupt a new editing session in Search.
    search.on_text_enter(move |_| list.set_focus());

    let request = Rc::clone(&pending);
    let poll = Rc::clone(&timer);
    open.on_click(move |_| {
        // Serialize loads until cancellation/replacement is added in ticket 04.
        if request.borrow().is_some() {
            return;
        }
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
        let (sender, receiver) = mpsc::channel();
        *request.borrow_mut() = Some(receiver);
        status.set_label("Loading playlist...");
        assert!(poll.start(50, false));
        std::thread::spawn(move || {
            let _ = sender.send(BrowseModel::open_local(std::path::Path::new(&path)).map(Arc::new));
        });
    });
    frame.on_close(move |event| {
        timer.stop();
        event.skip(true);
    });
}

fn publish_results(
    list: ListCtrl,
    status: StaticText,
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
}

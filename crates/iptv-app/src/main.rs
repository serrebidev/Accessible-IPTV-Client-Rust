use iptv_core::BrowseModel;
use std::{cell::RefCell, rc::Rc, sync::mpsc};
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

        let model = Rc::new(RefCell::new(BrowseModel::empty()));
        let rows = Rc::clone(&model);
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(&rows.borrow(), index, column)
        }));
        list.set_item_count(0);

        let status = StaticText::builder(&panel)
            .with_label("No playlist loaded. Channel list is empty.")
            .build();
        bind_local_open(frame, open, list, status, model);
        let layout = BoxSizer::builder(Orientation::Vertical).build();
        layout.add(&open, 0, SizerFlag::All, 8);
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

fn bind_local_open(
    frame: Frame,
    open: Button,
    list: ListCtrl,
    status: StaticText,
    model: Rc<RefCell<BrowseModel>>,
) {
    // Only the worker owns file I/O and parsing. The timer drains its result
    // on the UI thread; closing the window never waits for a large file.
    let pending = Rc::new(RefCell::new(
        None::<mpsc::Receiver<std::io::Result<BrowseModel>>>,
    ));
    let timer = Rc::new(Timer::new(&frame));
    let completed_timer = Rc::downgrade(&timer);
    let result = Rc::clone(&pending);
    timer.on_tick(move |_| {
        let received = result.borrow().as_ref().map(mpsc::Receiver::try_recv);
        let loaded = match received {
            Some(Ok(loaded)) => loaded,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err(std::io::Error::other("Playlist worker stopped"))
            }
            _ => return,
        };
        *result.borrow_mut() = None;
        if let Some(timer) = completed_timer.upgrade() {
            timer.stop();
        }
        let message = match loaded {
            Ok(loaded) => {
                let count = i64::try_from(loaded.channels().len()).unwrap_or(i64::MAX);
                // Clear native selection before replacing row identities.
                list.set_item_count(0);
                let previous = model.replace(loaded);
                // Retiring a huge playlist must not free all its rows on the UI thread.
                std::thread::spawn(move || drop(previous));
                list.set_item_count(count);
                if count > 0 {
                    list.set_item_state(
                        0,
                        ListItemState::Selected | ListItemState::Focused,
                        ListItemState::Selected | ListItemState::Focused,
                    );
                    list.refresh_items(0, count - 1);
                }
                format!("{count} channels loaded.")
            }
            Err(_) => "Could not open playlist. Choose another local UTF-8 playlist.".to_owned(),
        };
        status.set_label(&message);
        list.set_accessibility_description(&message);
        // Completion never moves keyboard focus away from the user's control.
    });
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
        status.set_label("Loading playlist…");
        assert!(poll.start(50, false));
        std::thread::spawn(move || {
            let _ = sender.send(BrowseModel::open_local(std::path::Path::new(&path)));
        });
    });
    frame.on_close(move |event| {
        timer.stop();
        event.skip(true);
    });
}

fn channel_text(model: &BrowseModel, index: i64, column: i32) -> String {
    if column != 0 {
        return String::new();
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| model.channels().get(index))
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
        let model = BrowseModel::empty();
        for index in [-1, 0, 1, i64::MAX] {
            assert_eq!(channel_text(&model, index, 0), "");
            assert_eq!(channel_text(&model, index, 1), "");
        }
    }

    #[test]
    fn populated_rows_expose_names_in_source_order_without_stream_credentials() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../iptv-core/tests/fixtures/local.m3u");
        let model = BrowseModel::open_local(&path).unwrap();
        assert_eq!(channel_text(&model, 0, 0), "ABC East HD");
        assert_eq!(channel_text(&model, 1, 0), "映画, HD");
        assert_eq!(channel_text(&model, 2, 0), "Bob's Burgers, Season 1");
        assert_eq!(channel_text(&model, 3, 0), "Unnamed channel");
        assert_eq!(channel_text(&model, 4, 0), "");
        assert_eq!(channel_text(&model, -1, 0), "");
        assert_eq!(channel_text(&model, 0, 1), "");
    }
}

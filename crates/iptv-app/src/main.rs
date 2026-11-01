use iptv_core::BrowseModel;
use wxdragon::event::EventType;
use wxdragon::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    SystemOptions::set_option_by_int("msw.no-manifest-check", 1);
    wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("Accessible IPTV Client")
            .with_size(Size::new(720, 480))
            .build();
        frame.set_accessibility_label("Accessible IPTV Client");

        let panel = Panel::builder(&frame).build();
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

        let model = BrowseModel::empty();
        assert!(list.set_virtual_text_callback(move |index, column| {
            channel_text(&model, index, column)
        }));
        list.set_item_count(0);

        let status = StaticText::builder(&panel)
            .with_label("No playlist loaded. Channel list is empty.")
            .build();
        let layout = BoxSizer::builder(Orientation::Vertical).build();
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

fn channel_text(model: &BrowseModel, index: i64, column: i32) -> String {
    if column != 0 {
        return String::new();
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| model.channels().get(index))
        .map_or_else(String::new, |channel| channel.name.clone())
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
}

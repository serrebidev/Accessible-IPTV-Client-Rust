use iptv_core::BrowseModel;
use wxdragon::prelude::*;

fn main() {
    SystemOptions::set_option_by_int("msw.no-manifest-check", 1);
    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("Accessible IPTV Client")
            .with_size(Size::new(720, 480))
            .build();
        frame.set_accessibility_label("Accessible IPTV Client");

        let list = ListBox::builder(&frame).build();
        list.set_accessibility_label("Channels");
        list.set_accessibility_description("Channel list");
        list.set_size(Size::new(680, 400));

        let model = BrowseModel::empty();
        if model.channels().is_empty() {
            list.append("No playlist loaded");
        }

        frame.show(true);
        frame.centre();
        list.set_focus();
    });
}

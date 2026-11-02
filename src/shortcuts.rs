//! The list of keyboard shortcuts shown in the middle of the window with `?`.

use gtk::pango;
use gtk::prelude::*;

/// A heading, then the keys and what they do.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Playback",
        &[
            ("Space / P", "Play or pause"),
            ("M", "Mute"),
            ("0 / 9", "Volume up / down"),
            ("[ / ]", "Slower / faster"),
            ("Backspace", "Normal speed"),
        ],
    ),
    (
        "Seeking",
        &[
            ("← / →", "Back / forward 5 seconds"),
            ("Shift+← / →", "Back / forward 1 second"),
            ("↑ / ↓", "Forward / back 1 minute"),
            ("PgUp / PgDn", "Next / previous chapter"),
            (", / .", "Previous / next frame"),
        ],
    ),
    ("Queue", &[("< / >", "Previous / next video")]),
    (
        "View",
        &[
            ("C", "Subtitles"),
            ("F", "Fullscreen"),
            ("Shift+F", "Theater mode"),
            ("Esc", "Leave fullscreen or theater mode"),
            ("?", "This list"),
        ],
    ),
    ("Navigation", &[("Alt+← / →", "Back / forward")]),
];

pub struct Widgets {
    /// The panel with the list, shown and hidden as a whole.
    pub root: adw::Clamp,
    /// The list itself, kept inside the room the video has.
    pub scroll: gtk::ScrolledWindow,
}

pub fn build() -> Widgets {
    let grid = gtk::Grid::builder()
        .row_spacing(5)
        .column_spacing(18)
        .build();

    let mut row = 0;
    for (heading, shortcuts) in GROUPS {
        let title = gtk::Label::builder()
            .label(*heading)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .css_classes(["shortcut-heading"])
            .build();
        if row > 0 {
            title.set_margin_top(10);
        }
        grid.attach(&title, 0, row, 2, 1);
        row += 1;

        for (keys, action) in *shortcuts {
            let key = gtk::Label::builder()
                .label(*keys)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::Start)
                .css_classes(["shortcut-key"])
                .build();
            let action = gtk::Label::builder()
                .label(*action)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::Start)
                .wrap(true)
                .wrap_mode(pango::WrapMode::Word)
                .hexpand(true)
                .css_classes(["shortcut-action"])
                .build();
            grid.attach(&key, 0, row, 1, 1);
            grid.attach(&action, 1, row, 1, 1);
            row += 1;
        }
    }

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .child(&grid)
        .build();

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("shortcuts");
    card.append(&scroll);

    let root = adw::Clamp::builder()
        .maximum_size(460)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .visible(false)
        .child(&card)
        .build();

    Widgets { root, scroll }
}

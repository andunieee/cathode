//! The widgets. They are built once; `nav` decides what they show.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::controls;
use crate::fetch::{Entry, SearchFilter, SearchSort};
use crate::library;
use crate::nav;
use crate::state::CaptionMode;
use crate::player::Player;
use crate::preferences;
use crate::profile;

/// Playback speeds on offer, 1.0 being the first selected.
const SPEEDS: [f64; 9] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
/// The index of the normal speed in `SPEEDS`.
pub const NORMAL_SPEED: u32 = 2;

pub struct CacheRow {
    pub entry: Entry,
    /// The name of the file, which tells apart the sizes a video was downloaded at.
    pub stem: String,
    pub check: gtk::CheckButton,
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    /// Shows the progress of the startup check of the external tools.
    pub setup_banner: adw::Banner,
    pub back: gtk::Button,
    pub forward: gtk::Button,
    pub downloads: gtk::Button,
    pub history_button: gtk::Button,
    /// Opens the sidebar listing the subscriptions.
    pub subs_toggle: gtk::ToggleButton,
    pub queue_toggle: gtk::ToggleButton,
    pub subscriptions_list: gtk::ListBox,
    /// Turns the restricted mode on or off.
    pub controls_button: gtk::Button,
    pub settings_button: gtk::Button,
    pub title_stack: gtk::Stack,
    pub search_entry: gtk::SearchEntry,
    pub sort: gtk::DropDown,
    pub filter: gtk::DropDown,
    pub stack: gtk::Stack,

    // Search and channel pages show their results the same way.
    pub channel_header: gtk::Box,
    pub channel_title: gtk::Label,
    pub channel_entry: gtk::SearchEntry,
    pub allow_channel: gtk::Button,
    pub subscribe_button: gtk::Button,
    pub results_status: gtk::Label,
    /// The channels a search found, above its videos.
    pub channel_results: gtk::Box,
    pub channel_results_row: gtk::ScrolledWindow,
    pub store: gio::ListStore,

    pub video_picture: gtk::Picture,
    pub video_title: gtk::Label,
    pub video_channel: gtk::LinkButton,
    pub video_meta: gtk::Label,
    pub video_description: gtk::Label,
    pub video_chapters: gtk::ListBox,
    /// The chapters of the video playing, in the player controls.
    pub chapters_button: gtk::MenuButton,
    pub chapters_list: gtk::ListBox,
    /// The caption choices of the player's settings, in the order of `CaptionMode`: none,
    /// original and the default language.
    pub caption_modes: Vec<gtk::CheckButton>,
    pub captions_button: gtk::MenuButton,
    pub captions_list: gtk::ListBox,
    /// The caption tracks the list shows, and the one selected.
    pub caption_tracks: RefCell<(Vec<crate::player::CaptionTrack>, Option<i64>)>,
    pub allow_video: gtk::Button,
    pub related: gtk::Button,

    pub cache_list: gtk::ListBox,
    pub cache_select_all: gtk::CheckButton,
    pub cache_summary: gtk::Label,
    pub cache_queue: gtk::Button,
    pub cache_save: gtk::Button,
    pub cache_delete: gtk::Button,
    pub cache_rows: RefCell<Vec<CacheRow>>,

    /// The latest videos of the subscriptions, on the home page.
    pub feed_store: gio::ListStore,
    pub feed_status: gtk::Label,

    pub history_scroll: gtk::ScrolledWindow,
    /// The history page is one list per day, the one most recently added to is kept here.
    pub history_box: gtk::Box,
    pub history_current: RefCell<Option<gtk::ListBox>>,
    pub history_empty: gtk::Label,

    pub controls: controls::Widgets,
    pub library: library::Widgets,

    /// The queue sidebar, which takes the whole window in fullscreen.
    pub queue_split: adw::OverlaySplitView,
    pub queue_header: adw::HeaderBar,
    pub queue_scroll: gtk::ScrolledWindow,
    /// Keeps the player in proportion, in the queue sidebar.
    pub player_frame: gtk::AspectFrame,
    pub player: Rc<Player>,
    pub player_status: gtk::Label,
    /// A quick message at the top of the video, like the volume after changing it.
    pub player_message: gtk::Label,
    pub player_message_timer: RefCell<Option<glib::SourceId>>,
    pub play_button: gtk::Button,
    pub seek: gtk::Scale,
    /// Paints the sponsor segments over the seek bar.
    pub seek_marks: gtk::DrawingArea,
    /// Duration of the video, and the `(start, end)` of its sponsor segments in seconds.
    pub sponsor_spans: Rc<RefCell<(f64, Vec<(f64, f64)>)>>,
    pub time_label: gtk::Label,
    /// The sizes the video in the player can be downloaded at, in the order of `QUALITIES`.
    pub size_buttons: Vec<gtk::CheckButton>,
    pub size_status: gtk::Label,
    pub seek_row: gtk::Box,
    pub speed: gtk::DropDown,
    pub volume: gtk::MenuButton,
    pub volume_scale: gtk::Scale,
    /// Skip buttons, which show when the progress bar is hidden, and whether each one is only
    /// for fullscreen.
    pub skip_buttons: Vec<(gtk::Button, bool)>,

    pub queue_list: gtk::ListBox,

    /// Set while the widgets are being updated by code, so their signals can be ignored.
    pub syncing: Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// The index of this playback speed in the list, if it is there.
pub fn speed_index(speed: f64) -> Option<u32> {
    SPEEDS.iter().position(|&s| s == speed).map(|i| i as u32)
}

/// The playback speed at this index of the speed list.
pub fn speed_at(index: u32) -> Option<f64> {
    SPEEDS.get(index as usize).copied()
}

/// Handles the keys of the player, which follow the ones of mpv. Returns whether it was one.
fn player_key(key: gdk::Key, shift: bool) -> bool {
    let ui = get();
    if ui.player.id().is_none() {
        return false;
    }

    let focus = GtkWindowExt::focus(&ui.window);
    let inside = |check: &dyn Fn(&gtk::Widget) -> bool| {
        std::iter::successors(focus.clone(), |widget| widget.parent()).any(|widget| check(&widget))
    };
    // Typing, and the widgets that use the arrows themselves.
    if focus.as_ref().is_some_and(|widget| widget.is::<gtk::Editable>()) {
        return false;
    }
    let arrows_taken = inside(&|widget| {
        widget.is::<gtk::Scale>()
            || widget.is::<gtk::ListBox>()
            || widget.is::<gtk::DropDown>()
            || widget.is::<gtk::Popover>()
    });
    let activates = focus.as_ref().is_some_and(|widget| {
        widget.is::<gtk::Button>() || widget.is::<gtk::Scale>() || widget.is::<gtk::DropDown>()
    });

    match key {
        gdk::Key::space if activates => return false,
        gdk::Key::space | gdk::Key::p | gdk::Key::P => nav::toggle_pause(),
        gdk::Key::f | gdk::Key::F => nav::toggle_fullscreen(),
        gdk::Key::m | gdk::Key::M => nav::toggle_mute(),
        gdk::Key::c | gdk::Key::C => nav::toggle_captions(),
        gdk::Key::_0 | gdk::Key::asterisk | gdk::Key::KP_Multiply => nav::change_volume(0.02),
        gdk::Key::_9 | gdk::Key::slash | gdk::Key::KP_Divide => nav::change_volume(-0.02),
        gdk::Key::bracketright => nav::step_speed(1),
        gdk::Key::bracketleft => nav::step_speed(-1),
        gdk::Key::BackSpace => nav::reset_speed(),
        gdk::Key::greater => nav::queue_step(1),
        gdk::Key::period => nav::step_frame(true),
        gdk::Key::less => nav::queue_step(-1),
        gdk::Key::comma => nav::step_frame(false),
        gdk::Key::Page_Up => nav::step_chapter(1),
        gdk::Key::Page_Down => nav::step_chapter(-1),
        gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down if arrows_taken => {
            return false;
        }
        gdk::Key::Left => nav::skip(if shift { -1.0 } else { -5.0 }),
        gdk::Key::Right => nav::skip(if shift { 1.0 } else { 5.0 }),
        gdk::Key::Down => nav::skip(-60.0),
        gdk::Key::Up => nav::skip(60.0),
        _ => return false,
    }
    true
}

pub fn get() -> Rc<Ui> {
    UI.with(|ui| ui.borrow().clone().expect("The UI is not built yet"))
}

/// What the widgets of one row of a result list need to be bound to an entry.
struct Card {
    root: gtk::Box,
    picture: gtk::Picture,
    title: gtk::Label,
    channel: gtk::LinkButton,
    details: gtk::Label,
    entry: Rc<RefCell<Option<Entry>>>,
}

impl Card {
    fn new() -> Self {
        let entry: Rc<RefCell<Option<Entry>>> = Rc::default();

        let picture = gtk::Picture::builder()
            .width_request(112)
            .height_request(63)
            .content_fit(gtk::ContentFit::Cover)
            .valign(gtk::Align::Center)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["card"])
            .build();

        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();

        let channel = gtk::LinkButton::builder().cursor(&crate::ui::pointer())
            .uri("cathode:channel")
            .halign(gtk::Align::Start)
            .build();
        channel.set_has_tooltip(false);
        channel.connect_activate_link({
            let entry = entry.clone();
            move |_| {
                if let Some(entry) = entry.borrow().as_ref() {
                    nav::open_channel(&entry.channel_id, &entry.channel);
                }
                glib::Propagation::Stop
            }
        });

        let details = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .hexpand(true)
            .build();

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&channel);
        line.append(&details);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&title);
        text.append(&line);

        let queue = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("list-add-symbolic")
            .tooltip_text("Add to queue")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        queue.connect_clicked({
            let entry = entry.clone();
            move |_| {
                if let Some(entry) = entry.borrow().clone() {
                    nav::queue_add(entry);
                }
            }
        });

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.set_margin_top(6);
        root.set_margin_bottom(6);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.append(&picture);
        root.append(&text);
        root.append(&queue);

        Self {
            root,
            picture,
            title,
            channel,
            details,
            entry,
        }
    }

    fn bind(&self, entry: &Entry) {
        self.title.set_text(&entry.title);
        self.channel.set_label(&entry.channel);
        self.channel.set_visible(!entry.channel.is_empty());
        // In the restricted mode only the channels that are allowed can be visited.
        self.channel
            .set_sensitive(!profile::restricted() || profile::allows_channel(&entry.channel_id));
        self.details.set_text(&nav::details(entry));
        *self.entry.borrow_mut() = Some(entry.clone());

        // The pictures are recycled, the name tells which video a late thumbnail is for.
        self.picture.set_widget_name(&entry.id);
        self.picture
            .set_paintable(nav::thumbnail(&entry.id, false, Some(&self.picture)).as_ref());
    }
}

fn results_view(store: &gio::ListStore) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let card = Card::new();
        item.set_child(Some(&card.root));
        // SAFETY: only ever read back as a `Card` below.
        unsafe { item.set_data("card", card) };
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let entry = object.borrow::<Entry>().clone();
        // SAFETY: set in `connect_setup` as a `Card`.
        let card = unsafe { item.data::<Card>("card").unwrap().as_ref() };
        card.bind(&entry);
    });

    let view = gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(store.clone()))),
        Some(factory),
    );
    view.set_single_click_activate(true);
    view.add_css_class("rich-list");
    view.connect_activate({
        let store = store.clone();
        move |_, position| {
            if let Some(object) = store.item(position).and_downcast::<glib::BoxedAnyObject>() {
                nav::open_video(object.borrow::<Entry>().clone());
            }
        }
    });
    view
}

fn point_window_controls(widget: &gtk::Widget) {
    if widget.is::<gtk::WindowControls>() {
        widget.set_cursor(Some(&pointer()));
        return;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        point_window_controls(&current);
        child = current.next_sibling();
    }
}

/// The hand cursor the web shows over clickable things; GTK widgets don't get it by default.
pub fn pointer() -> gtk::gdk::Cursor {
    gtk::gdk::Cursor::from_name("pointer", None).expect("the default cursor theme has a pointer")
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder().cursor(&crate::ui::pointer())
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build()
}

/// The queue sidebar's width when the window is small, and what it's reserved in the layout.
const QUEUE_MIN_WIDTH: f64 = 360.0;

pub fn build(app: &adw::Application) -> Rc<Ui> {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Cathode")
        .default_width(1100)
        .default_height(720)
        .build();

    // Header bar
    let header = adw::HeaderBar::new();
    let back = icon_button("go-previous-symbolic", "Back");
    let forward = icon_button("go-next-symbolic", "Forward");
    back.connect_clicked(|_| nav::back());
    forward.connect_clicked(|_| nav::forward());
    let home = icon_button("go-home-symbolic", "Home");
    home.connect_clicked(|_| nav::open_home());
    header.pack_start(&back);
    header.pack_start(&forward);
    header.pack_start(&home);

    // The window buttons are made by the header bar itself, so they are found once it has them.
    header.connect_map(|header| point_window_controls(header.upcast_ref()));

    // Straight to the restricted mode, asking for the password if there is one.
    let controls_button = icon_button("system-users-symbolic", "Turn on the restricted mode");
    controls_button.connect_clicked(|_| controls::toggle_profile());
    header.pack_start(&controls_button);

    let subs_toggle = gtk::ToggleButton::builder().cursor(&crate::ui::pointer())
        .icon_name("starred-symbolic")
        .tooltip_text("Subscriptions and collections")
        .build();
    header.pack_start(&subs_toggle);

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search YouTube")
        .hexpand(true)
        .build();
    let sort = gtk::DropDown::from_strings(&["Relevance", "Newest", "Most viewed"]);
    sort.set_cursor(Some(&crate::ui::pointer()));
    let filter = gtk::DropDown::from_strings(&[
        "No filter",
        "Last hour",
        "Today",
        "This week",
        "This month",
        "This year",
        "Under 4 minutes",
        "4 to 20 minutes",
        "Over 20 minutes",
    ]);
    filter.set_cursor(Some(&crate::ui::pointer()));
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_box.append(&search_entry);
    search_box.append(&sort);
    search_box.append(&filter);
    // The search bar is replaced by a title in the restricted mode.
    let title_stack = gtk::Stack::new();
    title_stack.add_named(
        &adw::Clamp::builder()
            .maximum_size(640)
            .child(&search_box)
            .build(),
        Some("search"),
    );
    title_stack.add_named(
        &gtk::Label::builder()
            .label("Restricted mode")
            .css_classes(["heading"])
            .build(),
        Some("restricted"),
    );
    header.set_title_widget(Some(&title_stack));

    let queue_toggle = gtk::ToggleButton::builder().cursor(&crate::ui::pointer())
        .icon_name("view-list-symbolic")
        .tooltip_text("Queue")
        .build();
    let downloads = icon_button("folder-download-symbolic", "Downloaded videos");
    downloads.connect_clicked(|_| nav::open_cache());
    let settings_button = icon_button("emblem-system-symbolic", "Settings");
    settings_button.connect_clicked(|_| nav::open_settings());
    let history_button = icon_button("document-open-recent-symbolic", "History");
    history_button.connect_clicked(|_| nav::open_history());
    header.pack_end(&settings_button);
    header.pack_end(&queue_toggle);
    header.pack_end(&downloads);
    header.pack_end(&history_button);

    // Re-running the search is what changing either dropdown does, so that results follow.
    let search = |ui: &Ui| {
        nav::search(
            &ui.search_entry.text(),
            SearchSort::ALL[ui.sort.selected() as usize],
            SearchFilter::ALL[ui.filter.selected() as usize],
        );
    };
    search_entry.connect_activate(move |_| {
        let ui = get();
        search(&ui);
    });
    sort.connect_selected_notify(move |_| {
        let ui = get();
        if !ui.syncing.get() {
            search(&ui);
        }
    });
    filter.connect_selected_notify(move |_| {
        let ui = get();
        if !ui.syncing.get() {
            search(&ui);
        }
    });

    // Results page, for searches and channels
    let channel_title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .focusable(false)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["title-2"])
        .build();
    let channel_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search this channel")
        .width_chars(28)
        .build();
    channel_entry.connect_activate(|entry| nav::search_channel(&entry.text()));
    let channel_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    channel_header.set_margin_top(12);
    channel_header.set_margin_bottom(6);
    channel_header.set_margin_start(12);
    channel_header.set_margin_end(12);
    channel_header.append(&channel_title);
    channel_header.append(&channel_entry);
    let allow_channel = gtk::Button::builder().cursor(&crate::ui::pointer())
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    allow_channel.connect_clicked(|_| nav::toggle_allowed());
    let subscribe_button = gtk::Button::builder().cursor(&crate::ui::pointer())
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    subscribe_button.connect_clicked(|_| nav::toggle_subscription());
    channel_header.append(&subscribe_button);
    channel_header.append(&allow_channel);

    let results_status = gtk::Label::builder()
        .css_classes(["dim-label"])
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .wrap(true)
        .build();
    // Errors can point to the settings.
    results_status.connect_activate_link(|_, _| {
        nav::open_settings();
        glib::Propagation::Stop
    });
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let channel_results = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    channel_results.set_margin_start(12);
    channel_results.set_margin_end(12);
    channel_results.set_margin_bottom(6);
    let channel_results_row = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .visible(false)
        .child(&channel_results)
        .build();
    let results = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results.append(&channel_header);
    results.append(&channel_results_row);
    results.append(&results_status);
    results.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&results_view(&store))
            .build(),
    );

    // Video page
    let video_picture = gtk::Picture::builder()
        .width_request(320)
        .height_request(180)
        .content_fit(gtk::ContentFit::Cover)
        .valign(gtk::Align::Start)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["card"])
        .build();
    let video_title = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .focusable(false)
        .css_classes(["title-2"])
        .build();
    let video_channel = gtk::LinkButton::builder().cursor(&crate::ui::pointer())
        .uri("cathode:channel")
        .halign(gtk::Align::Start)
        .build();
    video_channel.connect_activate_link(|_| {
        nav::open_current_channel();
        glib::Propagation::Stop
    });
    let video_meta = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .focusable(false)
        .css_classes(["dim-label"])
        .build();
    let play = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Play")
        .css_classes(["suggested-action"])
        .build();
    play.connect_clicked(|_| nav::play_current());
    let queue_video = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Queue")
        .build();
    queue_video.connect_clicked(|_| nav::queue_current());
    let buttons = adw::WrapBox::builder()
        .child_spacing(8)
        .line_spacing(8)
        .margin_top(6)
        .build();
    buttons.append(&play);
    buttons.append(&queue_video);
    let related = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Related")
        .build();
    related.connect_clicked(|_| nav::open_related());
    buttons.append(&related);
    let library = library::Widgets::new();
    buttons.append(&library.save);
    let allow_video = gtk::Button::builder().cursor(&crate::ui::pointer())
        .visible(false)
        .build();
    allow_video.connect_clicked(|_| nav::toggle_allowed());
    buttons.append(&allow_video);

    let video_info = gtk::Box::new(gtk::Orientation::Vertical, 6);
    video_info.set_hexpand(true);
    video_info.append(&video_title);
    video_info.append(&video_channel);
    video_info.append(&video_meta);
    video_info.append(&buttons);

    let video_top = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    video_top.append(&video_picture);
    video_top.append(&video_info);

    let video_description = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .selectable(true)
        .focusable(false)
        .build();
    let video_chapters = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .visible(false)
        .build();
    video_chapters.connect_row_activated(|_, row| nav::play_chapter(row.index() as usize));
    let video_page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    video_page.set_margin_top(18);
    video_page.set_margin_bottom(18);
    video_page.set_margin_start(18);
    video_page.set_margin_end(18);
    video_page.append(&video_top);
    video_page.append(&video_chapters);
    video_page.append(&video_description);
    let video_scroll = gtk::ScrolledWindow::builder().child(&video_page).build();

    // Cache page
    let cache_select_all = gtk::CheckButton::with_label("Select all");
    cache_select_all.set_cursor(Some(&crate::ui::pointer()));
    let cache_summary = gtk::Label::builder()
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let cache_queue = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Add to queue")
        .sensitive(false)
        .build();
    let cache_save = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Save")
        .sensitive(false)
        .build();
    let cache_delete = gtk::Button::builder().cursor(&crate::ui::pointer())
        .label("Delete")
        .sensitive(false)
        .css_classes(["destructive-action"])
        .build();
    cache_select_all.connect_toggled(|check| {
        if !get().syncing.get() {
            nav::cache_select_all(check.is_active());
        }
    });
    cache_queue.connect_clicked(|_| nav::cache_queue_selected());
    cache_save.connect_clicked(|_| nav::cache_save_selected());
    cache_delete.connect_clicked(|_| nav::cache_delete_selected());

    let cache_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cache_bar.set_margin_top(12);
    cache_bar.set_margin_bottom(6);
    cache_bar.set_margin_start(18);
    cache_bar.set_margin_end(18);
    cache_bar.append(&cache_select_all);
    cache_bar.append(&cache_summary);
    cache_bar.append(&cache_queue);
    cache_bar.append(&cache_save);
    cache_bar.append(&cache_delete);

    let cache_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(18)
        .margin_end(18)
        .margin_bottom(18)
        .valign(gtk::Align::Start)
        .build();
    cache_list.connect_row_activated(|_, row| nav::cache_open(row.index() as usize));
    let cache_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    cache_page.append(&cache_bar);
    cache_page.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&cache_list)
            .build(),
    );

    // Home page, once there are subscriptions
    let feed_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let feed_status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    let feed_refresh = icon_button("view-refresh-symbolic", "Check for new videos");
    feed_refresh.add_css_class("flat");
    feed_refresh.connect_clicked(|_| nav::refresh_feed());
    let feed_heading = gtk::Label::builder()
        .label("Latest videos")
        .xalign(0.0)
        .css_classes(["title-2"])
        .build();
    let feed_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    feed_bar.set_margin_top(12);
    feed_bar.set_margin_bottom(6);
    feed_bar.set_margin_start(12);
    feed_bar.set_margin_end(12);
    feed_bar.append(&feed_heading);
    feed_bar.append(&feed_status);
    feed_bar.append(&feed_refresh);
    let feed = gtk::Box::new(gtk::Orientation::Vertical, 0);
    feed.append(&feed_bar);
    feed.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&results_view(&feed_store))
            .build(),
    );

    // History page
    let history_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let history_empty = gtk::Label::builder()
        .label("Nothing watched yet")
        .css_classes(["dim-label"])
        .margin_top(24)
        .visible(false)
        .build();
    let history_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    history_page.set_margin_top(18);
    history_page.set_margin_bottom(18);
    history_page.set_margin_start(18);
    history_page.set_margin_end(18);
    history_page.append(
        &gtk::Label::builder()
            .label("History")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    history_page.append(&history_empty);
    history_page.append(&history_box);
    let history_scroll = gtk::ScrolledWindow::builder()
        .child(
            &adw::Clamp::builder()
                .maximum_size(720)
                .child(&history_page)
                .build(),
        )
        .build();
    // More is read from the file when the end is near, or when the page doesn't even scroll.
    let adjustment = history_scroll.vadjustment();
    adjustment.connect_value_changed(|_| nav::history_check(true));
    adjustment.connect_upper_notify(|_| nav::history_check(false));

    // Player, which lives in the queue sidebar
    let player = Player::new();
    let player_message = gtk::Label::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .can_target(false)
        .visible(false)
        .css_classes(["player-message"])
        .build();
    let player_status = gtk::Label::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["osd", "title-3"])
        .build();

    let play_button = icon_button("media-playback-pause-symbolic", "Play/Pause");
    play_button.connect_clicked(|_| nav::toggle_pause());
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
    seek.set_cursor(Some(&crate::ui::pointer()));
    seek.set_hexpand(true);
    seek.add_css_class("seek-bar");
    seek.set_draw_value(false);
    seek.connect_change_value(|_, _, value| {
        nav::seek(value.clamp(0.0, 1.0));
        glib::Propagation::Proceed
    });
    seek.set_has_tooltip(true);
    seek.connect_query_tooltip(|seek, x, _y, _keyboard, tooltip| {
        match nav::seek_tooltip(x as f64, seek.width() as f64) {
            Some(text) => {
                tooltip.set_text(Some(&text));
                true
            }
            None => false,
        }
    });
    let sponsor_spans: Rc<RefCell<(f64, Vec<(f64, f64)>)>> = Default::default();
    let seek_marks = gtk::DrawingArea::new();
    seek_marks.set_can_target(false);
    seek_marks.set_draw_func({
        let spans = sponsor_spans.clone();
        move |_, context, width, height| {
            let (duration, segments) = &*spans.borrow();
            if *duration <= 0.0 {
                return;
            }
            // The colour SponsorBlock uses for sponsors.
            context.set_source_rgba(0.0, 0.83, 0.0, 0.75);
            let (width, bar) = (width as f64, 6.0);
            for (start, end) in segments {
                let (from, to) = (start / duration, (end / duration).min(1.0));
                context.rectangle(from * width, (height as f64 - bar) / 2.0, (to - from) * width, bar);
            }
            let _ = context.fill();
        }
    });
    let seek_overlay = gtk::Overlay::new();
    seek_overlay.set_hexpand(true);
    seek_overlay.set_child(Some(&seek));
    seek_overlay.add_overlay(&seek_marks);
    let time_label = gtk::Label::new(Some("0:00 / 0:00"));
    time_label.add_css_class("numeric");
    let speed = gtk::DropDown::from_strings(
        &SPEEDS
            .map(|speed| format!("{speed}×"))
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    speed.set_cursor(Some(&crate::ui::pointer()));
    speed.set_selected(SPEEDS.iter().position(|&speed| speed == 1.0).unwrap() as u32);
    speed.set_tooltip_text(Some("Playback speed"));
    speed.connect_selected_notify(|speed| {
        if let Some(&speed) = SPEEDS.get(speed.selected() as usize) {
            nav::set_speed(speed);
        }
    });
    let volume_scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
    volume_scale.set_cursor(Some(&crate::ui::pointer()));
    volume_scale.set_inverted(true);
    volume_scale.set_draw_value(false);
    volume_scale.set_height_request(140);
    volume_scale.set_value(1.0);
    let volume = gtk::MenuButton::builder().cursor(&crate::ui::pointer())
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Volume")
        .direction(gtk::ArrowType::Up)
        .popover(&gtk::Popover::builder().child(&volume_scale).build())
        .build();
    volume_scale.connect_value_changed({
        let volume = volume.clone();
        move |scale| {
            let value = scale.value();
            volume.set_icon_name(nav::volume_icon(value, get().player.is_muted()));
            nav::set_volume(value);
        }
    });
    let chapters_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    chapters_list.connect_row_activated(|_, row| nav::play_playing_chapter(row.index() as usize));
    let chapters_button = gtk::MenuButton::builder().cursor(&crate::ui::pointer())
        .icon_name("view-list-symbolic")
        .tooltip_text("Chapters")
        .direction(gtk::ArrowType::Up)
        .visible(false)
        .popover(
            &gtk::Popover::builder()
                .child(
                    &gtk::ScrolledWindow::builder()
                        .child(&chapters_list)
                        .propagate_natural_width(true)
                        .max_content_height(360)
                        .propagate_natural_height(true)
                        .build(),
                )
                .build(),
        )
        .build();
    let captions_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    captions_list.connect_row_activated(|_, row| nav::pick_caption(row.index() as usize));
    let captions_button = gtk::MenuButton::builder()
        .cursor(&crate::ui::pointer())
        .icon_name("media-view-subtitles-symbolic")
        .tooltip_text("Subtitles")
        .direction(gtk::ArrowType::Up)
        .visible(false)
        .popover(
            &gtk::Popover::builder()
                .child(
                    &gtk::ScrolledWindow::builder()
                        .child(&captions_list)
                        .propagate_natural_width(true)
                        .max_content_height(360)
                        .propagate_natural_height(true)
                        .build(),
                )
                .build(),
        )
        .build();
    let stop_button = icon_button("media-playback-stop-symbolic", "Stop and close the player");
    stop_button.connect_clicked(|_| nav::stop_player());
    let fullscreen = icon_button("view-fullscreen-symbolic", "Fullscreen");
    fullscreen.connect_clicked(|_| nav::toggle_fullscreen());

    // The settings of the player
    let progress_bar = gtk::CheckButton::with_label("Show the progress bar");
    progress_bar.set_cursor(Some(&crate::ui::pointer()));
    progress_bar.set_active(!crate::settings::get().hide_progress_bar);
    progress_bar.connect_toggled(|button| nav::set_progress_bar(button.is_active()));
    let mut caption_modes: Vec<gtk::CheckButton> = Vec::new();
    for mode in [CaptionMode::Off, CaptionMode::Original, CaptionMode::Default] {
        let button = gtk::CheckButton::with_label(&nav::caption_mode_label(mode));
        button.set_cursor(Some(&crate::ui::pointer()));
        if let Some(first) = caption_modes.first() {
            button.set_group(Some(first));
        }
        button.connect_toggled(move |button| {
            if button.is_active() && !get().syncing.get() {
                nav::set_caption_mode(mode);
            }
        });
        caption_modes.push(button);
    }
    let size_heading = gtk::Label::builder()
        .label("Download size")
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let size_status = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    let mut size_buttons: Vec<gtk::CheckButton> = Vec::new();
    for (height, label) in crate::settings::QUALITIES.iter() {
        let button = gtk::CheckButton::with_label(label);
        button.set_cursor(Some(&crate::ui::pointer()));
        if let Some(first) = size_buttons.first() {
            button.set_group(Some(first));
        }
        let height = *height;
        button.connect_toggled(move |button| {
            if button.is_active() && !get().syncing.get() {
                nav::set_size(height);
            }
        });
        size_buttons.push(button);
    }
    let settings_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    settings_box.append(&progress_bar);
    settings_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    for button in &caption_modes {
        settings_box.append(button);
    }
    settings_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    settings_box.append(&size_heading);
    for button in &size_buttons {
        settings_box.append(button);
    }
    settings_box.append(&size_status);
    let player_settings = gtk::MenuButton::builder()
        .cursor(&crate::ui::pointer())
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Settings")
        .direction(gtk::ArrowType::Up)
        .popover(&gtk::Popover::builder().child(&settings_box).build())
        .build();

    // Two rows, the sidebar is narrow.
    let seek_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    seek_row.append(&seek_overlay);
    seek_row.append(&time_label);
    let skip_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    skip_row.set_halign(gtk::Align::Center);
    skip_row.set_hexpand(true);
    let mut skip_buttons = Vec::new();
    for seconds in [-300_i32, -60, -30, -10, 10, 30, 60, 300] {
        let magnitude: i32 = seconds.abs();
        let label = format!(
            "{}{}",
            if seconds < 0 { "−" } else { "+" },
            if magnitude >= 60 {
                format!("{}min", magnitude / 60)
            } else {
                format!("{magnitude}s")
            }
        );
        let button = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .label(label)
            .css_classes(["flat"])
            .build();
        button.connect_clicked(move |_| nav::skip(seconds as f64));
        button.set_visible(false);
        skip_buttons.push((button.clone(), magnitude != 30));
        skip_row.append(&button);
    }
    let buttons_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    buttons_row.append(&play_button);
    buttons_row.append(&stop_button);
    buttons_row.append(&speed);
    buttons_row.append(&skip_row);
    buttons_row.append(&volume);
    buttons_row.append(&captions_button);
    buttons_row.append(&chapters_button);
    buttons_row.append(&player_settings);
    buttons_row.append(&fullscreen);
    let player_controls = gtk::Box::new(gtk::Orientation::Vertical, 0);
    player_controls.add_css_class("osd");
    player_controls.set_halign(gtk::Align::Fill);
    player_controls.set_valign(gtk::Align::End);
    player_controls.set_margin_bottom(8);
    player_controls.set_margin_start(8);
    player_controls.set_margin_end(8);
    player_controls.append(&seek_row);
    player_controls.append(&buttons_row);

    let click = gtk::GestureClick::new();
    click.connect_released(|_, presses, _, _| match presses {
        1 => nav::toggle_pause(),
        2 => nav::toggle_fullscreen(),
        _ => {}
    });
    player.area.add_controller(click);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&player.area));
    overlay.add_overlay(&player_status);
    overlay.add_overlay(&player_message);
    overlay.add_overlay(&player_controls);
    let player_frame = gtk::AspectFrame::new(0.5, 0.5, 16.0 / 9.0, false);
    player_frame.set_child(Some(&overlay));
    // The player's expand flags would otherwise share the sidebar's height with the queue.
    player_frame.set_vexpand(false);
    player_frame.set_valign(gtk::Align::End);
    // Nothing to show until something plays.
    player_frame.set_visible(false);

    // Pages
    let stack = gtk::Stack::new();
    let controls = controls::Widgets::new();
    controls.home.add_named(&feed, Some("feed"));
    stack.add_named(&controls.home, Some("home"));
    stack.add_named(&preferences::build(&controls.settings), Some("settings"));
    stack.add_named(&results, Some("results"));
    stack.add_named(&video_scroll, Some("video"));
    stack.add_named(&cache_page, Some("cache"));
    stack.add_named(&history_scroll, Some("history"));
    stack.add_named(&library.page, Some("collection"));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let setup_banner = adw::Banner::new("");
    toolbar.add_top_bar(&setup_banner);
    toolbar.set_content(Some(&stack));

    // Queue sidebar
    let queue_header = adw::HeaderBar::new();
    queue_header.set_show_end_title_buttons(false);
    queue_header.set_title_widget(Some(
        &gtk::Label::builder()
            .label("Queue")
            .css_classes(["heading"])
            .build(),
    ));

    let queue_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["navigation-sidebar"])
        .build();
    let queue_bar = adw::ToolbarView::new();
    queue_bar.add_top_bar(&queue_header);
    let queue_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let queue_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&queue_list)
        .build();
    queue_content.append(&queue_scroll);
    queue_content.append(&player_frame);
    queue_bar.set_content(Some(&queue_content));

    let split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::End)
        .collapsed(false)
        .show_sidebar(false)
        .min_sidebar_width(QUEUE_MIN_WIDTH)
        .max_sidebar_width(480.0)
        .sidebar_width_fraction(0.25)
        .sidebar(&queue_bar)
        .content(&toolbar)
        .build();
    queue_toggle
        .bind_property("active", &split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();

    // Subscriptions sidebar, on the other side
    let subs_header = adw::HeaderBar::new();
    subs_header.set_show_end_title_buttons(false);
    subs_header.set_title_widget(Some(
        &gtk::Label::builder()
            .label("Library")
            .css_classes(["heading"])
            .build(),
    ));
    let subscriptions_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["navigation-sidebar"])
        .build();
    let subs_heading = gtk::Label::builder()
        .label("Subscriptions")
        .xalign(0.0)
        .margin_top(12)
        .margin_start(12)
        .css_classes(["heading"])
        .build();
    let sidebar_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_content.append(&subs_heading);
    sidebar_content.append(&subscriptions_list);
    sidebar_content.append(&library.sidebar);
    let subs_bar = adw::ToolbarView::new();
    subs_bar.add_top_bar(&subs_header);
    subs_bar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(&sidebar_content)
            .build(),
    ));

    let subs_split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::Start)
        .collapsed(false)
        .show_sidebar(false)
        .min_sidebar_width(260.0)
        .sidebar(&subs_bar)
        .content(&split)
        .build();
    subs_toggle
        .bind_property("active", &subs_split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();
    // Without the toggle there is no way to open it, not even by swiping.
    subs_toggle
        .bind_property("visible", &subs_split, "enable-show-gesture")
        .sync_create()
        .build();
    subs_split.connect_show_sidebar_notify({
        let header = header.clone();
        let subs_toggle = subs_toggle.clone();
        move |split| {
            // Resizing can open it on its own, which the restricted mode doesn't allow.
            if split.shows_sidebar() && !subs_toggle.is_visible() {
                split.set_show_sidebar(false);
            }
            header.set_show_start_title_buttons(!split.shows_sidebar())
        }
    });

    // On a small screen the search pickers make way.
    let small = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        800.0,
        adw::LengthUnit::Sp,
    ));
    small.add_setter(&sort, "visible", Some(&false.to_value()));
    small.add_setter(&filter, "visible", Some(&false.to_value()));
    window.add_breakpoint(small);

    // The queue sidebar floats over the page when, at its minimal width, it would squeeze the
    // main content below its own minimum. The subscriptions sidebar pushes the content instead,
    // and so counts against it when open.
    let update_overlay = {
        let window = window.clone();
        let split = split.clone();
        let subs_split = subs_split.clone();
        let toolbar = toolbar.clone();
        move || {
            let width = f64::from(window.width());
            let content_min = f64::from(toolbar.measure(gtk::Orientation::Horizontal, -1).0);
            let subs = if subs_split.shows_sidebar() {
                (width * subs_split.sidebar_width_fraction())
                    .clamp(subs_split.min_sidebar_width(), subs_split.max_sidebar_width())
            } else {
                0.0
            };
            // At the threshold itself the window can't shrink further, so it counts as overflow.
            let overflow = width > 0.0 && width <= content_min + subs + QUEUE_MIN_WIDTH;
            if split.is_collapsed() != overflow {
                // Collapsing or expanding resets whether the sidebar is shown.
                let shown = split.shows_sidebar();
                split.set_collapsed(overflow);
                split.set_show_sidebar(shown);
            }
        }
    };
    // The content's minimum is asked for only when it can have changed: on startup, on every
    // screen change, when the subscriptions sidebar opens or closes, and when the window resizes.
    update_overlay();
    stack.connect_visible_child_name_notify({
        let update = update_overlay.clone();
        move |_| update()
    });
    subs_split.connect_show_sidebar_notify({
        let update = update_overlay.clone();
        move |_| update()
    });
    window.connect_realize(move |window| {
        if let Some(surface) = window.surface() {
            let update = update_overlay.clone();
            surface.connect_layout(move |_, _, _| update());
        }
    });

    window.connect_fullscreened_notify(|_| nav::fullscreen_changed());

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&subs_split));
    window.set_content(Some(&toasts));

    // Mouse back/forward buttons and keyboard shortcuts
    let buttons = gtk::GestureClick::new();
    buttons.set_button(0);
    buttons.set_propagation_phase(gtk::PropagationPhase::Capture);
    buttons.connect_pressed(|gesture, _, _, _| match gesture.current_button() {
        8 => nav::back(),
        9 => nav::forward(),
        _ => {}
    });
    window.add_controller(buttons);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, modifiers| {
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::Left if alt => nav::back(),
            gdk::Key::Right if alt => nav::forward(),
            gdk::Key::Escape if get().window.is_fullscreen() => nav::toggle_fullscreen(),
            _ if alt || ctrl => return glib::Propagation::Proceed,
            _ => {
                if !player_key(key, shift) {
                    return glib::Propagation::Proceed;
                }
            }
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);

    let ui = Rc::new(Ui {
        window,
        toasts,
        setup_banner,
        back,
        forward,
        downloads,
        history_button,
        subs_toggle,
        queue_toggle,
        subscriptions_list,
        title_stack,
        controls_button,
        settings_button,
        search_entry,
        sort,
        filter,
        stack,
        channel_header,
        channel_title,
        channel_entry,
        allow_channel,
        subscribe_button,
        results_status,
        channel_results,
        channel_results_row,
        store,
        video_picture,
        video_title,
        video_channel,
        video_meta,
        video_description,
        video_chapters,
        chapters_button,
        chapters_list,
        caption_modes,
        captions_button,
        captions_list,
        caption_tracks: RefCell::default(),
        allow_video,
        related,
        cache_list,
        cache_select_all,
        cache_summary,
        cache_queue,
        cache_save,
        cache_delete,
        cache_rows: RefCell::new(Vec::new()),
        feed_store,
        feed_status,
        history_scroll,
        history_box,
        history_current: RefCell::new(None),
        history_empty,
        controls,
        library,
        queue_split: split,
        queue_header,
        queue_scroll,
        player_frame,
        player,
        player_status,
        player_message,
        player_message_timer: RefCell::new(None),
        play_button,
        seek,
        seek_marks,
        sponsor_spans,
        time_label,
        size_buttons,
        size_status,
        seek_row,
        speed,
        volume,
        volume_scale,
        skip_buttons,
        queue_list,
        syncing: Cell::new(false),
    });
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));
    nav::apply_progress_bar(false);

    // Position and state of the player
    glib::timeout_add_local(std::time::Duration::from_millis(250), || {
        nav::tick();
        glib::ControlFlow::Continue
    });

    ui
}

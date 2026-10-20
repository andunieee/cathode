//! The widgets. They are built once; `nav` decides what they show.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::controls;
use crate::fetch::{Entry, SearchFilter, SearchSort};
use crate::nav;
use crate::player::Player;
use crate::profile;

/// Playback speeds on offer, 1.0 being the first selected.
const SPEEDS: [f64; 9] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

pub struct CacheRow {
    pub entry: Entry,
    pub check: gtk::CheckButton,
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub split: adw::OverlaySplitView,
    pub toolbar: adw::ToolbarView,
    /// Shows the progress of the startup check of the external tools.
    pub setup_banner: adw::Banner,
    pub back: gtk::Button,
    pub forward: gtk::Button,
    pub downloads: gtk::Button,
    /// "open" has the entries to turn the profile on, "restricted" the one to turn it off.
    pub controls_stack: gtk::Stack,
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
    pub results_status: gtk::Label,
    pub store: gio::ListStore,

    pub video_picture: gtk::Picture,
    pub video_title: gtk::Label,
    pub video_channel: gtk::LinkButton,
    pub video_meta: gtk::Label,
    pub video_description: gtk::Label,
    pub allow_video: gtk::Button,

    pub cache_list: gtk::ListBox,
    pub cache_select_all: gtk::CheckButton,
    pub cache_summary: gtk::Label,
    pub cache_queue: gtk::Button,
    pub cache_delete: gtk::Button,
    pub cache_rows: RefCell<Vec<CacheRow>>,

    pub controls: controls::Widgets,

    pub player: Rc<Player>,
    pub player_title: gtk::Label,
    pub player_status: gtk::Label,
    pub play_button: gtk::Button,
    pub seek: gtk::Scale,
    pub time_label: gtk::Label,

    pub queue_list: gtk::ListBox,

    /// Set while the widgets are being updated by code, so their signals can be ignored.
    pub syncing: Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
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

        let channel = gtk::LinkButton::builder()
            .uri("lumpla:channel")
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

        let queue = gtk::Button::builder()
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
        // In the restricted profile only the channels that are allowed can be visited.
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

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build()
}

pub fn build(app: &adw::Application) -> Rc<Ui> {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Lumpla")
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

    let turn_on = gtk::Button::builder()
        .label("Turn on restricted profile")
        .css_classes(["flat"])
        .build();
    let set_up = gtk::Button::builder()
        .label("Set up restricted profile…")
        .css_classes(["flat"])
        .build();
    let turn_off = gtk::Button::builder()
        .label("Turn off restricted profile…")
        .css_classes(["flat"])
        .build();
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu_box.append(&turn_on);
    menu_box.append(&set_up);
    let controls_stack = gtk::Stack::new();
    controls_stack.add_named(&menu_box, Some("open"));
    controls_stack.add_named(&turn_off, Some("restricted"));
    let popover = gtk::Popover::builder().child(&controls_stack).build();
    let controls_menu = gtk::MenuButton::builder()
        .icon_name("system-users-symbolic")
        .tooltip_text("Parental controls")
        .popover(&popover)
        .build();
    {
        let popover = popover.clone();
        turn_on.connect_clicked(move |_| {
            popover.popdown();
            nav::set_restricted(true);
        });
    }
    {
        let popover = popover.clone();
        set_up.connect_clicked(move |_| {
            popover.popdown();
            nav::open_settings();
        });
    }
    turn_off.connect_clicked(move |_| {
        popover.popdown();
        controls::unlock_profile();
    });
    header.pack_start(&controls_menu);

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search YouTube")
        .hexpand(true)
        .build();
    let sort = gtk::DropDown::from_strings(&["Relevance", "Newest", "Most viewed"]);
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
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_box.append(&search_entry);
    search_box.append(&sort);
    search_box.append(&filter);
    // The search bar is replaced by a title in the restricted profile.
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
            .label("Restricted profile")
            .css_classes(["heading"])
            .build(),
        Some("restricted"),
    );
    header.set_title_widget(Some(&title_stack));

    let queue_toggle = gtk::ToggleButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Queue")
        .build();
    let downloads = icon_button("folder-download-symbolic", "Downloaded videos");
    downloads.connect_clicked(|_| nav::open_cache());
    header.pack_end(&queue_toggle);
    header.pack_end(&downloads);

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
    let allow_channel = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    allow_channel.connect_clicked(|_| nav::toggle_allowed());
    channel_header.append(&allow_channel);

    let results_status = gtk::Label::builder()
        .css_classes(["dim-label"])
        .margin_top(6)
        .margin_bottom(6)
        .build();
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let results = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results.append(&channel_header);
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
    let video_channel = gtk::LinkButton::builder()
        .uri("lumpla:channel")
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
    let play = gtk::Button::builder()
        .label("Play")
        .css_classes(["suggested-action", "pill"])
        .build();
    play.connect_clicked(|_| nav::play_current());
    let queue_video = gtk::Button::builder()
        .label("Add to queue")
        .css_classes(["pill"])
        .build();
    queue_video.connect_clicked(|_| nav::queue_current());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_margin_top(6);
    buttons.append(&play);
    buttons.append(&queue_video);
    let allow_video = gtk::Button::builder()
        .css_classes(["pill"])
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
    let video_page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    video_page.set_margin_top(18);
    video_page.set_margin_bottom(18);
    video_page.set_margin_start(18);
    video_page.set_margin_end(18);
    video_page.append(&video_top);
    video_page.append(&video_description);
    let video_scroll = gtk::ScrolledWindow::builder().child(&video_page).build();

    // Cache page
    let cache_select_all = gtk::CheckButton::with_label("Select all");
    let cache_summary = gtk::Label::builder()
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let cache_queue = gtk::Button::builder()
        .label("Add to queue")
        .sensitive(false)
        .build();
    let cache_delete = gtk::Button::builder()
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
    cache_delete.connect_clicked(|_| nav::cache_delete_selected());

    let cache_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cache_bar.set_margin_top(12);
    cache_bar.set_margin_bottom(6);
    cache_bar.set_margin_start(18);
    cache_bar.set_margin_end(18);
    cache_bar.append(&cache_select_all);
    cache_bar.append(&cache_summary);
    cache_bar.append(&cache_queue);
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

    // Player page
    let player = Player::new();
    let player_title = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["osd", "heading"])
        .build();
    let player_status = gtk::Label::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["osd", "title-3"])
        .build();

    let play_button = icon_button("media-playback-pause-symbolic", "Play/Pause");
    play_button.connect_clicked(|_| nav::toggle_pause());
    let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
    seek.set_hexpand(true);
    seek.set_draw_value(false);
    seek.connect_change_value(|_, _, value| {
        nav::seek(value.clamp(0.0, 1.0));
        glib::Propagation::Proceed
    });
    let time_label = gtk::Label::new(Some("0:00 / 0:00"));
    time_label.add_css_class("numeric");
    let speed = gtk::DropDown::from_strings(
        &SPEEDS
            .map(|speed| format!("{speed}×"))
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    speed.set_selected(SPEEDS.iter().position(|&speed| speed == 1.0).unwrap() as u32);
    speed.set_tooltip_text(Some("Playback speed"));
    speed.connect_selected_notify(|speed| {
        if let Some(&speed) = SPEEDS.get(speed.selected() as usize) {
            nav::set_speed(speed);
        }
    });
    let volume_scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
    volume_scale.set_inverted(true);
    volume_scale.set_draw_value(false);
    volume_scale.set_height_request(140);
    volume_scale.set_value(1.0);
    let volume = gtk::MenuButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Volume")
        .direction(gtk::ArrowType::Up)
        .popover(&gtk::Popover::builder().child(&volume_scale).build())
        .build();
    volume_scale.connect_value_changed({
        let volume = volume.clone();
        move |scale| {
            let value = scale.value();
            volume.set_icon_name(match value {
                v if v <= 0.0 => "audio-volume-muted-symbolic",
                v if v < 0.34 => "audio-volume-low-symbolic",
                v if v < 0.67 => "audio-volume-medium-symbolic",
                _ => "audio-volume-high-symbolic",
            });
            nav::set_volume(value);
        }
    });
    let fullscreen = icon_button("view-fullscreen-symbolic", "Fullscreen");
    fullscreen.connect_clicked(|_| nav::toggle_fullscreen());

    let player_controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    player_controls.add_css_class("osd");
    player_controls.set_halign(gtk::Align::Fill);
    player_controls.set_valign(gtk::Align::End);
    player_controls.set_margin_bottom(12);
    player_controls.set_margin_start(12);
    player_controls.set_margin_end(12);
    player_controls.append(&play_button);
    player_controls.append(&seek);
    player_controls.append(&time_label);
    player_controls.append(&speed);
    player_controls.append(&volume);
    player_controls.append(&fullscreen);

    let click = gtk::GestureClick::new();
    click.connect_released(|_, presses, _, _| match presses {
        1 => nav::toggle_pause(),
        2 => nav::toggle_fullscreen(),
        _ => {}
    });
    player.area.add_controller(click);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&player.area));
    overlay.add_overlay(&player_title);
    overlay.add_overlay(&player_status);
    overlay.add_overlay(&player_controls);

    // Pages
    let stack = gtk::Stack::new();
    let controls = controls::Widgets::new();
    stack.add_named(&controls.home, Some("home"));
    stack.add_named(&controls.settings, Some("settings"));
    stack.add_named(&results, Some("results"));
    stack.add_named(&video_scroll, Some("video"));
    stack.add_named(&cache_page, Some("cache"));
    stack.add_named(&overlay, Some("player"));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let setup_banner = adw::Banner::new("");
    toolbar.add_top_bar(&setup_banner);
    toolbar.set_content(Some(&stack));

    // Queue sidebar
    let queue_header = adw::HeaderBar::new();
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
    queue_bar.set_content(Some(
        &gtk::ScrolledWindow::builder().child(&queue_list).build(),
    ));

    let split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::End)
        .collapsed(false)
        .show_sidebar(false)
        .min_sidebar_width(300.0)
        .sidebar(&queue_bar)
        .content(&toolbar)
        .build();
    queue_toggle
        .bind_property("active", &split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();

    // With the sidebar open, it is the one on the edge of the window.
    split.connect_show_sidebar_notify({
        let header = header.clone();
        move |split| header.set_show_end_title_buttons(!split.shows_sidebar())
    });

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&split));
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
        match key {
            gdk::Key::Left if alt => nav::back(),
            gdk::Key::Right if alt => nav::forward(),
            gdk::Key::Escape if get().window.is_fullscreen() => nav::toggle_fullscreen(),
            gdk::Key::space => {
                let ui = get();
                let typing = GtkWindowExt::focus(&ui.window)
                    .is_some_and(|widget| widget.is::<gtk::Editable>());
                if typing || ui.stack.visible_child_name().as_deref() != Some("player") {
                    return glib::Propagation::Proceed;
                }
                nav::toggle_pause();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);

    let ui = Rc::new(Ui {
        window,
        toasts,
        split,
        toolbar,
        setup_banner,
        back,
        forward,
        downloads,
        title_stack,
        controls_stack,
        search_entry,
        sort,
        filter,
        stack,
        channel_header,
        channel_title,
        channel_entry,
        allow_channel,
        results_status,
        store,
        video_picture,
        video_title,
        video_channel,
        video_meta,
        video_description,
        allow_video,
        cache_list,
        cache_select_all,
        cache_summary,
        cache_queue,
        cache_delete,
        cache_rows: RefCell::new(Vec::new()),
        controls,
        player,
        player_title,
        player_status,
        play_button,
        seek,
        time_label,
        queue_list,
        syncing: Cell::new(false),
    });
    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));

    // Position and state of the player
    glib::timeout_add_local(std::time::Duration::from_millis(250), || {
        nav::tick();
        glib::ControlFlow::Continue
    });

    ui
}

//! The parental controls: the home page, which shows what the restricted mode allows or
//! lets one turn it on, and the page where the profile is set up.

use adw::prelude::*;
use gtk::gio;

use crate::collections;
use crate::fetch::{Entry, Request};
use crate::format;
use crate::nav;
use crate::profile::{self, ChannelRef, Item, PlaylistRef};
use crate::ui;

/// A titled list of rows.
struct Section {
    root: gtk::Box,
    list: gtk::ListBox,
}

fn section(title: &str) -> Section {
    let heading = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&heading);
    root.append(&list);
    Section { root, list }
}

fn clear(list: &gtk::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
}

fn page_box() -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.set_margin_top(18);
    page.set_margin_bottom(18);
    page.set_margin_start(18);
    page.set_margin_end(18);
    page
}

fn clamp(child: &impl IsA<gtk::Widget>) -> adw::Clamp {
    adw::Clamp::builder().maximum_size(720).child(child).build()
}

pub struct Widgets {
    /// Home page: "open" when the profile is off, "restricted" when it is on.
    pub home: gtk::Stack,
    pub settings: gtk::ScrolledWindow,

    home_channels: Section,
    home_playlists: Section,
    home_videos: Section,
    home_collections: Section,
    home_empty: gtk::Label,

    settings_entry: gtk::Entry,
    settings_status: gtk::Label,
    settings_channels: Section,
    settings_playlists: Section,
    settings_videos: Section,
    settings_collections: Section,
    password_row: adw::ActionRow,
    password_set: gtk::Button,
    password_remove: gtk::Button,
}

impl Widgets {
    pub fn new() -> Self {
        // Home, profile off
        let open = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title("Search YouTube")
            .description("Type in the search bar to look for videos")
            .build();

        // Home, profile on
        let home_empty = gtk::Label::builder()
            .label("Nothing is allowed in restricted mode yet")
            .css_classes(["dim-label"])
            .margin_top(24)
            .build();
        let (home_channels, home_playlists, home_videos, home_collections) = (
            section("Channels"),
            section("Playlists"),
            section("Videos"),
            section("Collections"),
        );

        let restricted = page_box();
        restricted.append(&home_empty);
        restricted.append(&home_channels.root);
        restricted.append(&home_playlists.root);
        restricted.append(&home_collections.root);
        restricted.append(&home_videos.root);

        let home = gtk::Stack::new();
        home.add_named(&open, Some("open"));
        home.add_named(
            &gtk::ScrolledWindow::builder()
                .child(&clamp(&restricted))
                .build(),
            Some("restricted"),
        );

        // Setting the profile up
        let heading = gtk::Label::builder()
            .label("Restricted mode")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build();
        let explanation = gtk::Label::builder()
            .label("While restricted mode is on, there is no search and only what is listed here can be watched: the videos, the collections, and the videos of the channels and of the playlists.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();

        let settings_entry = gtk::Entry::builder()
            .placeholder_text("Link to a video, channel or playlist, or a @handle")
            .hexpand(true)
            .build();
        settings_entry.connect_activate(|_| add_typed_item());
        let add = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .label("Add")
            .css_classes(["suggested-action"])
            .build();
        add.connect_clicked(|_| add_typed_item());
        let adding = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        adding.append(&settings_entry);
        adding.append(&add);

        let settings_status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();

        let (settings_channels, settings_playlists, settings_videos, settings_collections) = (
            section("Channels"),
            section("Playlists"),
            section("Videos"),
            section("Collections"),
        );

        let password_row = adw::ActionRow::builder().title("Password").build();
        let password_set = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .valign(gtk::Align::Center)
            .build();
        password_set.connect_clicked(|_| set_password());
        let password_remove = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .label("Remove")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        password_remove.connect_clicked(|_| remove_password());
        password_row.add_suffix(&password_set);
        password_row.add_suffix(&password_remove);
        let password = adw::PreferencesGroup::builder()
            .title("Password")
            .description("Asked for to turn the restricted mode on or off")
            .build();
        password.add(&password_row);

        let settings_page = page_box();
        settings_page.append(&heading);
        settings_page.append(&explanation);
        settings_page.append(&adding);
        settings_page.append(&settings_status);
        settings_page.append(&settings_channels.root);
        settings_page.append(&settings_playlists.root);
        settings_page.append(&settings_collections.root);
        settings_page.append(&settings_videos.root);
        settings_page.append(&password);

        Self {
            home,
            settings: gtk::ScrolledWindow::builder()
                .child(&clamp(&settings_page))
                .build(),
            home_channels,
            home_playlists,
            home_videos,
            home_collections,
            home_empty,
            settings_entry,
            settings_status,
            settings_channels,
            settings_playlists,
            settings_videos,
            settings_collections,
            password_row,
            password_set,
            password_remove,
        }
    }
}

// Rows

fn row(title: &str, subtitle: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .use_markup(false)
        .activatable(true)
        .cursor(&crate::ui::pointer())
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row
}

fn remove_button(on_remove: impl Fn() + 'static) -> gtk::Button {
    let button = gtk::Button::builder()
        .cursor(&crate::ui::pointer())
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remove from restricted mode")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    button.connect_clicked(move |_| on_remove());
    button
}

fn channel_row(channel: &ChannelRef, removable: bool) -> adw::ActionRow {
    let row = row(&channel.name, "Channel", "avatar-default-symbolic");
    row.connect_activated({
        let channel = channel.clone();
        move |_| nav::open_channel(&channel.id, &channel.name)
    });
    if removable {
        let id = channel.id.clone();
        row.add_suffix(&remove_button(move || {
            remove(|p| p.channels.retain(|c| c.id != id))
        }));
    }
    row
}

fn playlist_row(playlist: &PlaylistRef, removable: bool) -> adw::ActionRow {
    let row = row(&playlist.title, "Playlist", "view-list-symbolic");
    row.connect_activated({
        let playlist = playlist.clone();
        move |_| nav::open_playlist(&playlist.id, &playlist.title)
    });
    if removable {
        let id = playlist.id.clone();
        row.add_suffix(&remove_button(move || {
            remove(|p| p.playlists.retain(|p| p.id != id))
        }));
    }
    row
}

fn collection_row(name: &str, removable: bool) -> adw::ActionRow {
    let count = collections::get(name).map_or(0, |collection| collection.videos.len());
    let row = row(
        name,
        &format!("Collection · {count} videos"),
        "folder-symbolic",
    );
    row.connect_activated({
        let name = name.to_string();
        move |_| nav::open_collection(&name)
    });
    if removable {
        let name = name.to_string();
        row.add_suffix(&remove_button(move || {
            remove(|p| p.collections.retain(|c| c != &name))
        }));
    }
    row
}

fn video_row(entry: &Entry, removable: bool) -> adw::ActionRow {
    let mut subtitle = Vec::new();
    if !entry.channel.is_empty() {
        subtitle.push(entry.channel.clone());
    }
    if let Some(duration) = entry.duration {
        subtitle.push(format::duration(duration));
    }

    let row = adw::ActionRow::builder()
        .title(&entry.title)
        .subtitle(subtitle.join("  ·  "))
        .use_markup(false)
        .activatable(true)
        .cursor(&crate::ui::pointer())
        .build();

    let picture = gtk::Picture::builder()
        .width_request(112)
        .height_request(63)
        .content_fit(gtk::ContentFit::Cover)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["card"])
        .build();
    // The pictures are recycled, the name tells which video a late thumbnail is for.
    picture.set_widget_name(&entry.id);
    picture.set_paintable(nav::thumbnail(&entry.id, false, Some(&picture)).as_ref());
    row.add_prefix(&picture);

    row.connect_activated({
        let entry = entry.clone();
        move |_| nav::open_video(entry.clone())
    });
    if removable {
        let id = entry.id.clone();
        row.add_suffix(&remove_button(move || {
            remove(|p| p.videos.retain(|v| v.id != id))
        }));
    }
    row
}

/// Takes something out of the profile.
fn remove(change: impl FnOnce(&mut profile::Profile)) {
    if let Err(error) = profile::update(change) {
        nav::toast(&format!("Could not save restricted mode: {error}"));
    }
    show_settings();
}

fn fill(
    channels: &Section,
    playlists: &Section,
    collections: &Section,
    videos: &Section,
    removable: bool,
) -> bool {
    let profile = profile::read(|profile| profile.clone());

    clear(&channels.list);
    for channel in &profile.channels {
        channels.list.append(&channel_row(channel, removable));
    }
    clear(&playlists.list);
    for playlist in &profile.playlists {
        playlists.list.append(&playlist_row(playlist, removable));
    }
    clear(&collections.list);
    // Collections that are gone are not worth showing.
    let names: Vec<&String> = profile
        .collections
        .iter()
        .filter(|name| collections::exists(name))
        .collect();
    for name in &names {
        collections.list.append(&collection_row(name, removable));
    }
    clear(&videos.list);
    for video in &profile.videos {
        videos.list.append(&video_row(video, removable));
    }

    channels.root.set_visible(!profile.channels.is_empty());
    playlists.root.set_visible(!profile.playlists.is_empty());
    collections.root.set_visible(!names.is_empty());
    videos.root.set_visible(!profile.videos.is_empty());

    profile.channels.is_empty()
        && profile.playlists.is_empty()
        && names.is_empty()
        && profile.videos.is_empty()
}

// Pages

pub fn show_home() {
    let ui = ui::get();
    let widgets = &ui.controls;

    if profile::restricted() {
        widgets.home.set_visible_child_name("restricted");
        let empty = fill(
            &widgets.home_channels,
            &widgets.home_playlists,
            &widgets.home_collections,
            &widgets.home_videos,
            false,
        );
        widgets.home_empty.set_visible(empty);
    } else {
        widgets.home.set_visible_child_name("open");
    }
}

pub fn show_settings() {
    let ui = ui::get();
    let widgets = &ui.controls;

    fill(
        &widgets.settings_channels,
        &widgets.settings_playlists,
        &widgets.settings_collections,
        &widgets.settings_videos,
        true,
    );

    let has_password = profile::has_password();
    widgets
        .password_row
        .set_subtitle(if has_password { "Set" } else { "Not set" });
    widgets
        .password_set
        .set_label(if has_password { "Change…" } else { "Set…" });
    widgets.password_remove.set_visible(has_password);
}

// Adding things

fn set_status(text: &str) {
    ui::get().controls.settings_status.set_text(text);
}

fn add_typed_item() {
    let ui = ui::get();
    let text = ui.controls.settings_entry.text();

    match profile::parse_input(&text) {
        Some(input) => {
            set_status("Looking it up…");
            nav::send(Request::Resolve { input });
        }
        None => set_status("That is not a link to a video, channel or playlist"),
    }
}

/// What the lookup of something typed in found.
pub fn resolved(result: Result<Item, String>) {
    let ui = ui::get();

    match result {
        Ok(item) => {
            let name = match &item {
                Item::Video(entry) => entry.title.clone(),
                Item::Channel(channel) => channel.name.clone(),
                Item::Playlist(playlist) => playlist.title.clone(),
                Item::Collection(name) => name.clone(),
            };

            match profile::add(item) {
                Ok(true) => {
                    ui.controls.settings_entry.set_text("");
                    set_status(&format!("Added “{name}”"));
                }
                Ok(false) => set_status(&format!("“{name}” is already allowed in restricted mode")),
                Err(error) => set_status(&format!("Could not save restricted mode: {error}")),
            }

            if ui.stack.visible_child_name().as_deref() == Some("settings") {
                show_settings();
            }
        }
        Err(error) => set_status(&format!("Could not add it: {error}")),
    }
}

// Passwords

/// Asks for passwords, `on_confirm` gets what was typed in each of the fields.
fn prompt(
    heading: &str,
    body: &str,
    confirm: &str,
    fields: &[&str],
    on_confirm: impl FnOnce(Vec<String>) + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", confirm)]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");

    let entries: Vec<gtk::PasswordEntry> = fields
        .iter()
        .map(|placeholder| {
            gtk::PasswordEntry::builder()
                .placeholder_text(*placeholder)
                .show_peek_icon(true)
                .activates_default(true)
                .build()
        })
        .collect();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for entry in &entries {
        content.append(entry);
    }
    dialog.set_extra_child(Some(&content));

    dialog.choose(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |response| {
            if response == "ok" {
                on_confirm(
                    entries
                        .iter()
                        .map(|entry| entry.text().to_string())
                        .collect(),
                );
            }
        },
    );
}

fn set_password() {
    prompt(
        "Set a password",
        "It will be asked for to turn the restricted mode on or off.",
        "Set",
        &["New password", "Repeat the password"],
        |passwords| {
            if passwords[0].is_empty() {
                nav::toast("The password cannot be empty");
            } else if passwords[0] != passwords[1] {
                nav::toast("The passwords do not match");
            } else {
                match profile::set_password(Some(&passwords[0])) {
                    Ok(()) => nav::toast("Password set"),
                    Err(error) => nav::toast(&format!("Could not save restricted mode: {error}")),
                }
                show_settings();
            }
        },
    );
}

fn remove_password() {
    prompt(
        "Remove the password",
        "Enter the current password.",
        "Remove",
        &["Password"],
        |passwords| {
            if !profile::check_password(&passwords[0]) {
                nav::toast("Wrong password");
            } else {
                match profile::set_password(None) {
                    Ok(()) => nav::toast("Password removed"),
                    Err(error) => nav::toast(&format!("Could not save restricted mode: {error}")),
                }
                show_settings();
            }
        },
    );
}

/// Turns the restricted mode on or off, after asking for the password if there is one.
pub fn toggle_profile() {
    let turn_on = !profile::restricted();
    if !profile::has_password() {
        nav::set_restricted(turn_on);
        return;
    }

    let (heading, confirm) = if turn_on {
        ("Turn on the restricted mode", "Turn on")
    } else {
        ("Turn off the restricted mode", "Turn off")
    };
    prompt(
        heading,
        "Enter the password.",
        confirm,
        &["Password"],
        move |passwords| {
            if profile::check_password(&passwords[0]) {
                nav::set_restricted(turn_on);
            } else {
                nav::toast("Wrong password");
            }
        },
    );
}

//! The collections on screen: their section of the left sidebar, their page, the Save button of
//! the video page, and the dialogs that make, import and remove them.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, pango};

use crate::collections::{self, Collection, SavedVideo};
use crate::format;
use crate::nav;
use crate::profile;
use crate::state;
use crate::ui;

pub struct Widgets {
    /// The page of a collection.
    pub page: gtk::Box,
    title: gtk::Label,
    summary: gtk::Label,
    allow: gtk::Button,
    list: gtk::ListBox,
    /// The name of the collection on the page.
    shown: RefCell<Option<String>>,

    /// The section of the sidebar.
    pub sidebar: gtk::Box,
    sidebar_list: gtk::ListBox,

    /// Saves the video on screen. The arrow picks the collection.
    pub save: adw::SplitButton,
    save_choices: gtk::Box,
    save_popover: gtk::Popover,
}

fn flat_button(label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .cursor(&crate::ui::pointer())
        .css_classes(["flat"])
        .build();
    let label = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    button.set_child(Some(&label));
    button
}

fn remove_button(tooltip: &str, icon: &str, on_click: impl Fn() + 'static) -> gtk::Button {
    let button = gtk::Button::builder()
        .cursor(&crate::ui::pointer())
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    button.connect_clicked(move |_| on_click());
    button
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
}

fn clear_box(content: &gtk::Box) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
}

fn videos(count: usize) -> String {
    match count {
        1 => "1 video".to_string(),
        count => format!("{count} videos"),
    }
}

impl Widgets {
    pub fn new() -> Self {
        // The page
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["title-2"])
            .build();
        let summary = gtk::Label::builder().css_classes(["dim-label"]).build();
        let allow = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        allow.connect_clicked(|_| nav::toggle_allowed());

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.append(&title);
        header.append(&summary);
        header.append(&allow);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .valign(gtk::Align::Start)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        content.append(&header);
        content.append(&list);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(
            &gtk::ScrolledWindow::builder()
                .vexpand(true)
                .child(
                    &adw::Clamp::builder()
                        .maximum_size(720)
                        .child(&content)
                        .build(),
                )
                .build(),
        );

        // The sidebar
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let new = flat_button("New collection…");
        let import = flat_button("Import a folder…");
        menu.append(&new);
        menu.append(&import);
        let popover = gtk::Popover::builder().child(&menu).build();
        new.connect_clicked({
            let popover = popover.clone();
            move |_| {
                popover.popdown();
                new_collection();
            }
        });
        import.connect_clicked({
            let popover = popover.clone();
            move |_| {
                popover.popdown();
                import_folder();
            }
        });
        let add = gtk::MenuButton::builder()
            .cursor(&crate::ui::pointer())
            .icon_name("list-add-symbolic")
            .tooltip_text("New collection or import a folder")
            .popover(&popover)
            .css_classes(["flat", "circular"])
            .build();

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.set_margin_top(12);
        heading.set_margin_start(12);
        heading.set_margin_end(6);
        heading.append(
            &gtk::Label::builder()
                .label("Collections")
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        heading.append(&add);

        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.append(&heading);
        sidebar.append(&sidebar_list);

        // The Save button
        let save_choices = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let save_popover = gtk::Popover::builder().child(&save_choices).build();
        let save = adw::SplitButton::builder()
            .cursor(&crate::ui::pointer())
            .label("Save")
            .popover(&save_popover)
            .visible(false)
            .build();
        save.connect_clicked(|_| nav::save_current(default_target()));

        Self {
            page,
            title,
            summary,
            allow,
            list,
            shown: RefCell::new(None),
            sidebar,
            sidebar_list,
            save,
            save_choices,
            save_popover,
        }
    }
}

/// The collection the Save button saves to: the last one picked, as long as it can still be
/// saved to.
pub fn default_target() -> Option<String> {
    let savable = collections::savable();
    state::get()
        .save_collection
        .filter(|name| savable.contains(name))
        .or_else(|| savable.into_iter().next())
}

/// Fills in what depends on the collections there are: the sidebar and the Save button.
pub fn refresh() {
    let ui = ui::get();
    let library = &ui.library;

    clear_list(&library.sidebar_list);
    let counts = collections::counts();
    if counts.is_empty() {
        library.sidebar_list.append(
            &gtk::Label::builder()
                .label("No collections yet.")
                .css_classes(["dim-label"])
                .margin_top(12)
                .margin_bottom(12)
                .build(),
        );
    }
    for (name, count) in counts {
        let row = adw::ActionRow::builder()
            .title(&name)
            .subtitle(videos(count))
            .title_lines(1)
            .use_markup(false)
            .activatable(true)
            .cursor(&crate::ui::pointer())
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        row.connect_activated({
            let name = name.clone();
            move |_| nav::open_collection(&name)
        });
        row.add_suffix(&remove_button(
            "Forget this collection",
            "window-close-symbolic",
            {
                let name = name.clone();
                move || confirm_forget(&name)
            },
        ));
        library.sidebar_list.append(&row);
    }

    clear_box(&library.save_choices);
    for name in collections::savable() {
        let choice = flat_button(&name);
        choice.connect_clicked({
            let name = name.clone();
            move |_| {
                let ui = ui::get();
                ui.library.save_popover.popdown();
                let remember = state::update(|state| state.save_collection = Some(name.clone()));
                if let Err(error) = remember {
                    crate::log::warning!("Cannot write the state: {error}");
                }
                update_save_button();
                nav::save_current(Some(name.clone()));
            }
        });
        library.save_choices.append(&choice);
    }
    let new = flat_button("New collection…");
    new.connect_clicked(|_| {
        ui::get().library.save_popover.popdown();
        new_collection();
    });
    library.save_choices.append(&new);

    update_save_button();
}

/// The label and whether the Save button can be used, once the collection it saves to is known.
pub fn update_save_button() {
    let save = &ui::get().library.save;
    let target = default_target();
    save.set_sensitive(target.is_some());
    save.set_tooltip_text(Some(&match &target {
        Some(name) => format!("Save to “{name}”"),
        None => "There is no collection to save to".to_string(),
    }));
}

/// Shows the Save button, or hides it.
pub fn show_save_button(visible: bool) {
    ui::get().library.save.set_visible(visible);
}

/// The button of the collection page that adds it to the restricted mode.
pub fn set_allow_button(visible: bool, label: &str) {
    let allow = &ui::get().library.allow;
    allow.set_visible(visible);
    allow.set_label(label);
}

// The page

pub fn show_collection(name: &str) {
    let ui = ui::get();
    let library = &ui.library;
    *library.shown.borrow_mut() = Some(name.to_string());
    library.title.set_text(name);
    clear_list(&library.list);

    let Some(collection) = collections::get(name) else {
        library
            .summary
            .set_text("This collection does not exist anymore");
        return;
    };

    let mut summary = videos(collection.videos.len());
    summary.push_str(&match &collection.directory {
        Some(directory) => format!(" · {}", directory.display()),
        None => " · imported folder".to_string(),
    });
    library.summary.set_text(&summary);
    library.summary.set_ellipsize(pango::EllipsizeMode::Start);
    library.summary.set_tooltip_text(Some(&summary));

    if collection.videos.is_empty() {
        library.list.append(
            &gtk::Label::builder()
                .label("Nothing in this collection yet")
                .css_classes(["dim-label"])
                .margin_top(24)
                .margin_bottom(24)
                .build(),
        );
    }
    for video in &collection.videos {
        library.list.append(&video_row(&collection, video));
    }
}

fn video_row(collection: &Collection, video: &SavedVideo) -> adw::ActionRow {
    let entry = video.entry.clone();
    let present = video.path.exists();

    let mut subtitle = Vec::new();
    if !present {
        subtitle.push("File missing".to_string());
    }
    if !entry.channel.is_empty() {
        subtitle.push(entry.channel.clone());
    }
    if let Some(duration) = entry.duration {
        subtitle.push(format::duration(duration));
    }
    if let Ok(metadata) = std::fs::metadata(&video.path) {
        subtitle.push(format::bytes(metadata.len()));
    }
    if entry.url.is_empty() {
        subtitle.push(video.path.display().to_string());
    }

    let row = adw::ActionRow::builder()
        .title(&entry.title)
        .subtitle(subtitle.join("  ·  "))
        .title_lines(2)
        .subtitle_lines(1)
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

    let queue = remove_button("Add to queue", "list-add-symbolic", {
        let entry = entry.clone();
        move || nav::queue_add(entry.clone())
    });
    nav::track_queue_button(&queue, present, {
        let id = entry.id.clone();
        move || Some(id.clone())
    });
    let play = remove_button("Play", "media-playback-start-symbolic", {
        let entry = entry.clone();
        move || nav::play_now(entry.clone())
    });
    play.set_sensitive(present);
    row.add_suffix(&play);
    row.add_suffix(&queue);

    // The restricted mode only watches.
    if !profile::restricted() {
        let (tooltip, icon) = if collection.owns(video) {
            ("Delete the video", "user-trash-symbolic")
        } else {
            ("Remove from the collection", "window-close-symbolic")
        };
        row.add_suffix(&remove_button(tooltip, icon, {
            let (name, owns) = (collection.name.clone(), collection.owns(video));
            let entry = entry.clone();
            move || confirm_remove_video(&name, &entry, owns)
        }));
    }

    row.connect_activated(move |_| nav::open_video(entry.clone()));
    row
}

/// Redraws what shows the collections, after they changed.
fn changed() {
    refresh();
    let ui = ui::get();
    let shown = ui.library.shown.borrow().clone();
    if let Some(name) = shown {
        if ui.stack.visible_child_name().as_deref() == Some("collection") {
            show_collection(&name);
        }
    }
}

/// Called when a video was saved.
pub fn saved() {
    changed();
}

// Dialogs

fn confirm_remove_video(name: &str, entry: &crate::fetch::Entry, owns: bool) {
    let (heading, body, label) = if owns {
        (
            "Delete this video?",
            format!("“{}” will be deleted from the disk.", entry.title),
            "Delete",
        )
    } else {
        (
            "Remove this video?",
            format!(
                "“{}” will be taken out of “{name}”. The file stays where it is.",
                entry.title
            ),
            "Remove",
        )
    };

    let dialog = adw::AlertDialog::new(Some(heading), Some(&body));
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", label)]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let (name, id) = (name.to_string(), entry.id.clone());
    dialog.choose(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |response| {
            if response == "remove" {
                if let Err(error) = collections::remove_video(&name, &id) {
                    nav::toast(&format!("Could not save the collection: {error}"));
                }
                changed();
            }
        },
    );
}

fn confirm_forget(name: &str) {
    let saves = collections::get(name).is_some_and(|collection| collection.saves());
    let body = if saves {
        format!("“{name}” will be gone from the list. The videos saved in it stay in their folder.")
    } else {
        format!("“{name}” will be gone from the list. The videos stay where they are.")
    };

    let dialog = adw::AlertDialog::new(Some("Forget this collection?"), Some(&body));
    dialog.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
    dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let name = name.to_string();
    dialog.choose(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |response| {
            if response == "forget" {
                forget(&name);
            }
        },
    );
}

fn forget(name: &str) {
    if let Err(error) = collections::remove(name) {
        nav::toast(&format!("Could not delete the collection file: {error}"));
        return;
    }
    let result = profile::update(|profile| profile.collections.retain(|c| c != name));
    if let Err(error) = result {
        nav::toast(&format!("Could not save restricted mode: {error}"));
    }

    changed();
    nav::collection_forgotten(name);
}

/// Asks for a name and a folder.
pub fn new_collection() {
    let dialog = adw::AlertDialog::new(
        Some("New collection"),
        Some("A place to save videos to, in a folder of its own."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("create", false);

    let name = gtk::Entry::builder()
        .placeholder_text("Name")
        .activates_default(true)
        .build();

    // Without a choice, the folder is named after the collection.
    let folder: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let folder_label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .css_classes(["dim-label"])
        .build();
    let show_folder = {
        let (name, folder, folder_label) = (name.clone(), folder.clone(), folder_label.clone());
        Rc::new(move || {
            let path = folder
                .borrow()
                .clone()
                .unwrap_or_else(|| collections::default_directory(name.text().trim()));
            folder_label.set_text(&path.display().to_string());
            folder_label.set_tooltip_text(Some(&path.display().to_string()));
        })
    };
    show_folder();

    name.connect_changed({
        let (dialog, show_folder) = (dialog.clone(), show_folder.clone());
        move |name| {
            dialog.set_response_enabled("create", !name.text().trim().is_empty());
            show_folder();
        }
    });

    let choose = gtk::Button::with_label("Choose folder…");

    choose.set_cursor(Some(&crate::ui::pointer()));
    choose.connect_clicked({
        let (folder, show_folder) = (folder.clone(), show_folder.clone());
        move |_| {
            let folder = folder.clone();
            let show_folder = show_folder.clone();
            pick_folder("Folder for the saved videos", move |path| {
                *folder.borrow_mut() = Some(path);
                show_folder();
            });
        }
    });

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&folder_label);
    row.append(&choose);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(&name);
    content.append(&row);
    dialog.set_extra_child(Some(&content));

    let typed = name.clone();
    dialog.choose(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |response| {
            if response != "create" {
                return;
            }
            match collections::create(&typed.text(), folder.borrow().clone()) {
                Ok(name) => nav::toast(&format!("Created “{name}”")),
                Err(error) => nav::toast(&format!("Could not create the collection: {error}")),
            }
            changed();
        },
    );
    name.grab_focus();
}

/// Makes a collection of the videos in a folder.
pub fn import_folder() {
    pick_folder(
        "Folder with videos",
        |path| match collections::import_folder(&path) {
            Ok((name, count)) => {
                nav::toast(&format!("Imported {} into “{name}”", videos(count)));
                changed();
                nav::open_collection(&name);
            }
            Err(error) => nav::toast(&format!("Could not import the folder: {error}")),
        },
    );
}

fn pick_folder(title: &str, on_pick: impl FnOnce(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    dialog.select_folder(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |result| {
            // Closing the dialog is an error too, and nothing to complain about.
            if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                on_pick(path);
            }
        },
    );
}

//! The Settings page: a sidebar with the general settings and the parental control ones.

use adw::prelude::*;

use crate::nav;
use crate::settings::{self, BROWSERS, QUALITIES};

fn save(change: impl FnOnce(&mut settings::Settings)) {
    if let Err(error) = settings::update(change) {
        nav::toast(&format!("Could not save the settings: {error}"));
    }
}

fn entry_row(
    title: &str,
    subtitle: &str,
    text: &str,
    on_apply: impl Fn(String) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .text(text)
        .show_apply_button(true)
        .tooltip_text(subtitle)
        .build();
    row.connect_apply(move |row| on_apply(row.text().trim().to_string()));
    row
}

fn general() -> adw::PreferencesPage {
    let current = settings::get();

    let quality = adw::ComboRow::builder()
        .title("Maximum quality")
        .subtitle("The best video resolution to download")
        .model(&gtk::StringList::new(&QUALITIES.map(|(_, label)| label)))
        .selected(
            QUALITIES
                .iter()
                .position(|(height, _)| *height == current.max_height)
                .unwrap_or(0) as u32,
        )
        .build();
    quality.connect_selected_notify(|row| {
        let height = QUALITIES[row.selected() as usize].0;
        save(|s| s.max_height = height);
    });

    let sponsors = adw::SwitchRow::builder()
        .title("Remove sponsor segments")
        .subtitle("Cuts the sponsored parts out of downloaded videos (SponsorBlock)")
        .active(current.remove_sponsors)
        .build();
    sponsors.connect_active_notify(|row| {
        let active = row.is_active();
        save(|s| s.remove_sponsors = active);
    });

    let rate = entry_row(
        "Download speed limit",
        "Like 500K or 2M per second, empty for no limit",
        &current.limit_rate,
        |text| save(|s| s.limit_rate = text),
    );

    let downloads = adw::PreferencesGroup::builder().title("Downloads").build();
    downloads.add(&quality);
    downloads.add(&sponsors);
    downloads.add(&rate);

    let mut browsers = vec!["None"];
    browsers.extend(BROWSERS);
    let cookies = adw::ComboRow::builder()
        .title("Cookies from browser")
        .subtitle("Uses the browser's sign-in, for age-restricted videos and sign-in checks")
        .model(&gtk::StringList::new(&browsers))
        .selected(
            current
                .cookies_from_browser
                .as_deref()
                .and_then(|name| BROWSERS.iter().position(|b| *b == name))
                .map_or(0, |index| index as u32 + 1),
        )
        .build();
    cookies.connect_selected_notify(|row| {
        let browser = (row.selected() as usize)
            .checked_sub(1)
            .map(|index| BROWSERS[index].to_string());
        save(|s| s.cookies_from_browser = browser);
    });

    let proxy = entry_row(
        "Proxy",
        "Like socks5://127.0.0.1:1080, empty for none",
        &current.proxy,
        |text| save(|s| s.proxy = text),
    );

    let network = adw::PreferencesGroup::builder()
        .title("Network and account")
        .description("Used for everything yt-dlp does, searching included")
        .build();
    network.add(&cookies);
    network.add(&proxy);

    let page = adw::PreferencesPage::new();
    page.add(&downloads);
    page.add(&network);
    page
}

/// The page, with `parental` as the parental control one.
pub fn build(parental: &impl IsA<gtk::Widget>) -> gtk::Box {
    let stack = gtk::Stack::new();
    stack.add_named(&general(), Some("general"));
    stack.add_named(parental, Some("parental"));

    let sidebar = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .width_request(200)
        .build();
    for (name, title, icon) in [
        ("general", "General", "preferences-system-symbolic"),
        ("parental", "Parental Control", "system-users-symbolic"),
    ] {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&gtk::Label::new(Some(title)));
        let row = gtk::ListBoxRow::builder().child(&content).build();
        row.set_widget_name(name);
        sidebar.append(&row);
    }
    sidebar.connect_row_selected({
        let stack = stack.clone();
        move |_, row| {
            if let Some(row) = row {
                stack.set_visible_child_name(&row.widget_name());
            }
        }
    });
    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&sidebar);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    stack.set_hexpand(true);
    root.append(&stack);
    root
}

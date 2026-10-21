//! The Settings page: a sidebar with the general settings and the parental control ones.

use std::cell::{Cell, RefCell};

use adw::prelude::*;

use crate::fetch::Request;
use crate::nav;
use crate::settings::{self, BROWSERS, QUALITIES};

const COOKIES_SUBTITLE: &str =
    "Uses the browser's sign-in, for age-restricted videos and sign-in checks";

struct CookiesRow {
    row: adw::ComboRow,
    /// Set while the row is changed by code, so its signal can be ignored.
    syncing: Cell<bool>,
}

thread_local! {
    static COOKIES: RefCell<Option<std::rc::Rc<CookiesRow>>> = const { RefCell::new(None) };
}

fn cookies_row() -> Option<std::rc::Rc<CookiesRow>> {
    COOKIES.with(|cookies| cookies.borrow().clone())
}

/// The row position of the browser saved in the settings.
fn saved_position() -> u32 {
    settings::get()
        .cookies_from_browser
        .as_deref()
        .and_then(|name| BROWSERS.iter().position(|b| *b == name))
        .map_or(0, |index| index as u32 + 1)
}

/// Accepts the browser if yt-dlp could use its cookies, otherwise shows why it couldn't and
/// puts the row back to what is saved.
pub fn cookies_checked(browser: &str, result: Result<(), String>) {
    let Some(cookies) = cookies_row() else { return };
    let row = &cookies.row;

    // The choice may have moved on while yt-dlp was running.
    let chosen = (row.selected() as usize)
        .checked_sub(1)
        .map(|index| BROWSERS[index]);
    if chosen != Some(browser) {
        return;
    }

    row.set_sensitive(true);
    match result {
        Ok(()) => {
            row.set_subtitle(COOKIES_SUBTITLE);
            row.remove_css_class("error");
            save(|s| s.cookies_from_browser = Some(browser.to_string()));
        }
        Err(error) => {
            row.set_subtitle(&format!("Could not use {browser}: {error}"));
            row.add_css_class("error");
            cookies.syncing.set(true);
            row.set_selected(saved_position());
            cookies.syncing.set(false);
        }
    }
}

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
        .subtitle(COOKIES_SUBTITLE)
        .model(&gtk::StringList::new(&browsers))
        .selected(saved_position())
        .build();
    let cookies_state = std::rc::Rc::new(CookiesRow {
        row: cookies.clone(),
        syncing: Cell::new(false),
    });
    COOKIES.with(|slot| *slot.borrow_mut() = Some(cookies_state.clone()));
    cookies.connect_selected_notify(move |row| {
        if cookies_state.syncing.get() {
            return;
        }
        row.remove_css_class("error");

        // A browser has to prove it works before it is saved. Having none needs no proof.
        match (row.selected() as usize).checked_sub(1) {
            Some(index) => {
                row.set_subtitle("Checking…");
                row.set_sensitive(false);
                nav::send(Request::CheckCookies {
                    browser: BROWSERS[index].to_string(),
                });
            }
            None => {
                row.set_subtitle(COOKIES_SUBTITLE);
                save(|s| s.cookies_from_browser = None);
            }
        }
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
        ("parental", "Restricted Mode", "system-users-symbolic"),
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

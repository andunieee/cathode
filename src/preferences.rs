//! The Settings page: a sidebar with the general settings and the parental control ones.

use std::cell::{Cell, RefCell};

use adw::prelude::*;

use crate::extension;
use crate::fetch::Request;
use crate::nav;
use crate::remote;
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
        .title("Skip sponsor segments")
        .subtitle("Jumps over the sponsored parts while playing (SponsorBlock)")
        .active(current.skip_sponsors)
        .build();
    sponsors.connect_active_notify(|row| {
        let active = row.is_active();
        save(|s| s.skip_sponsors = active);
    });

    let rate = entry_row(
        "Download speed limit",
        "Like 500K or 2M per second, empty for no limit",
        &current.limit_rate,
        |text| save(|s| s.limit_rate = text),
    );

    let language = entry_row(
        "Default language",
        "Like en or pt-BR. Empty for what YouTube gives by default",
        &current.language,
        |text| {
            save(|s| s.language = text);
            nav::default_language_changed();
        },
    );
    let languages = adw::PreferencesGroup::builder()
        .title("Language")
        .description("The language of the titles in lists and of the subtitles to download")
        .build();
    languages.add(&language);

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

    let browser_button = gtk::Button::builder()
        .label("Set up")
        .valign(gtk::Align::Center)
        .build();
    browser_button.connect_clicked(|button| match extension::install() {
        Ok(dir) => {
            let uri = gtk::gio::File::for_path(&dir).uri();
            if let Err(error) =
                gtk::gio::AppInfo::launch_default_for_uri(&uri, None::<&gtk::gio::AppLaunchContext>)
            {
                nav::toast(&format!("Could not open {}: {error}", dir.display()));
                return;
            }
            button.clipboard().set_text("chrome://extensions");
            nav::toast(
                "Paste chrome://extensions in Chrome, turn on Developer mode, click Load unpacked and pick the folder",
            );
        }
        Err(error) => nav::toast(&format!("Could not write the extension: {error}")),
    });
    let browser_row = adw::ActionRow::builder()
        .title("Chrome extension")
        .subtitle("Alt+C, the toolbar button or the right-click menu on YouTube opens the page in Cathode")
        .build();
    browser_row.add_suffix(&browser_button);
    let browser = adw::PreferencesGroup::builder().title("Browser").build();
    browser.add(&browser_row);

    let page = adw::PreferencesPage::new();
    page.add(&languages);
    page.add(&downloads);
    page.add(&network);
    page.add(&browser);
    page
}

/// The page with the switch of the phone remote and the way to pair a phone.
fn remote_page() -> adw::PreferencesPage {
    let switch = adw::SwitchRow::builder()
        .title("Phone remote")
        .subtitle("Play, pause, search and manage the queue from a phone with a hyperb browser")
        .active(remote::enabled())
        .build();
    let pair_button = gtk::Button::builder()
        .label("Show QR code")
        .valign(gtk::Align::Center)
        .sensitive(remote::enabled())
        .build();
    pair_button.connect_clicked(|button| pairing_dialog().present(Some(button)));
    switch.connect_active_notify({
        let pair_button = pair_button.clone();
        move |row| {
            remote::set_enabled(row.is_active());
            pair_button.set_sensitive(row.is_active());
        }
    });
    let pair = adw::ActionRow::builder()
        .title("Pair a phone")
        .subtitle("Scan the code with the camera of the phone, then open the link in hyperb")
        .build();
    pair.add_suffix(&pair_button);

    let group = adw::PreferencesGroup::builder().title("Remote").build();
    group.add(&switch);
    group.add(&pair);
    let page = adw::PreferencesPage::new();
    page.add(&group);
    page
}

fn pairing_dialog() -> adw::Dialog {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .width_request(300)
        .height_request(300)
        .build();
    let address = gtk::Label::builder()
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Char)
        .css_classes(["caption", "monospace"])
        .max_width_chars(40)
        .build();
    let show = {
        let (picture, address) = (picture.clone(), address.clone());
        move || {
            let text = remote::address();
            picture.set_paintable(remote::qr_code(&text).as_ref());
            address.set_text(&text);
        }
    };
    show();

    let reset = gtk::Button::builder()
        .label("Lock out the paired phones")
        .halign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    reset.connect_clicked(move |_| {
        remote::reset();
        show();
    });
    let note = gtk::Label::builder()
        .label("Anyone with this address can control Cathode. Keep it to yourself, and lock the phones out if it gets around: a new code is made and the old one stops working.")
        .wrap(true)
        .max_width_chars(40)
        .css_classes(["dim-label"])
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&picture);
    content.append(&address);
    content.append(&note);
    content.append(&reset);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&content));
    adw::Dialog::builder()
        .title("Pair a phone")
        .child(&view)
        .build()
}

const APP_ID: &str = "io.github.andunieee.Cathode";
const WEBSITE: &str = "https://github.com/andunieee/cathode";

fn about_dialog() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_icon(APP_ID)
        .application_name("Cathode")
        .developer_name("George V")
        .developers(["George V"])
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Searches YouTube, downloads videos with yt-dlp and plays them in a built-in mpv player.")
        .website(WEBSITE)
        .issue_url(format!("{WEBSITE}/issues"))
        .license_type(gtk::License::Gpl30Only)
        .copyright("© George V")
        .build()
}

fn link_row(title: &str, uri: String) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(&uri)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row.connect_activated(move |row| {
        let launcher = gtk::UriLauncher::new(&uri);
        launcher.launch(
            row.root().and_downcast_ref::<gtk::Window>(),
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
    });
    row
}

fn about() -> adw::PreferencesPage {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 6);
    header.set_margin_top(24);
    header.set_margin_bottom(12);
    let icon = gtk::Image::from_icon_name(APP_ID);
    icon.set_pixel_size(128);
    icon.add_css_class("icon-dropshadow");
    let name = gtk::Label::new(Some("Cathode"));
    name.add_css_class("title-1");
    let version = gtk::Label::new(Some(env!("CARGO_PKG_VERSION")));
    version.add_css_class("dim-label");
    version.set_selectable(true);
    header.append(&icon);
    header.append(&name);
    header.append(&version);
    let top = adw::PreferencesGroup::new();
    top.add(&header);

    let details = adw::ActionRow::builder()
        .title("About Cathode")
        .activatable(true)
        .build();
    details.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    details.connect_activated(|row| about_dialog().present(Some(row)));

    let links = adw::PreferencesGroup::new();
    links.add(&details);
    links.add(&link_row("Website", WEBSITE.to_string()));
    links.add(&link_row("Report an issue", format!("{WEBSITE}/issues")));

    let page = adw::PreferencesPage::new();
    page.add(&top);
    page.add(&links);
    page
}

/// The page, with `parental` as the parental control one.
pub fn build(parental: &impl IsA<gtk::Widget>) -> gtk::Box {
    let stack = gtk::Stack::new();
    stack.add_named(&general(), Some("general"));
    stack.add_named(parental, Some("parental"));
    stack.add_named(&remote_page(), Some("remote"));
    stack.add_named(&about(), Some("about"));

    let sidebar = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .width_request(200)
        .build();
    for (name, title, icon) in [
        ("general", "General", "preferences-system-symbolic"),
        ("parental", "Restricted Mode", "system-users-symbolic"),
        ("remote", "Remote Control", "phone-symbolic"),
        ("about", "About", "help-about-symbolic"),
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

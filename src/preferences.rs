//! The Settings page: a sidebar with the general settings and the parental control ones.

use std::cell::{Cell, RefCell};

use adw::prelude::*;

use crate::extension;
use crate::fetch::Request;
use crate::muted;
use crate::nav;
use crate::remote;
use crate::settings::{self, BROWSERS, QUALITIES};
use crate::social;

struct CookiesRow {
    row: adw::ComboRow,
    /// Set while the row is changed by code, so its signal can be ignored.
    syncing: Cell<bool>,
}

thread_local! {
    static COOKIES: RefCell<Option<std::rc::Rc<CookiesRow>>> = const { RefCell::new(None) };
    /// The Restricted Mode page in the settings sidebar.
    static RESTRICTED_ROW: RefCell<Option<gtk::ListBoxRow>> = const { RefCell::new(None) };
    /// The Social page in the settings sidebar.
    static SOCIAL_ROW: RefCell<Option<gtk::ListBoxRow>> = const { RefCell::new(None) };
    /// The groups of the Muted & Hidden page, with what is muted and hidden in them.
    static MUTED_CHANNELS: RefCell<Option<adw::PreferencesGroup>> = const { RefCell::new(None) };
    static HIDDEN_VIDEOS: RefCell<Option<adw::PreferencesGroup>> = const { RefCell::new(None) };
    /// The rows put in those groups, so they can be taken out again. A group is not a container
    /// whose children can be walked, and its own child is its internal box, not a row.
    static MUTED_CHANNEL_ROWS: RefCell<Vec<gtk::Widget>> = const { RefCell::new(Vec::new()) };
    static HIDDEN_VIDEO_ROWS: RefCell<Vec<gtk::Widget>> = const { RefCell::new(Vec::new()) };
}

/// Shows or hides a page in the settings sidebar.
fn show_row(slot: &RefCell<Option<gtk::ListBoxRow>>, show: bool) {
    let Some(row) = slot.borrow().clone() else {
        return;
    };
    row.set_visible(show);
    // The page being looked at just went away.
    if !show
        && row.is_selected()
        && let Some(list) = row
            .parent()
            .and_then(|parent| parent.downcast::<gtk::ListBox>().ok())
        && let Some(first) = list.row_at_index(0)
    {
        list.select_row(Some(&first));
    }
}

/// Shows or hides the Restricted Mode page in the settings sidebar.
pub fn show_restricted_controls(show: bool) {
    RESTRICTED_ROW.with(|slot| show_row(slot, show));
}

/// Shows or hides the Social page in the settings sidebar.
pub fn show_social(show: bool) {
    SOCIAL_ROW.with(|slot| show_row(slot, show));
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
    // The apply button is internal to the row, give it the hand cursor too.
    point_buttons(&row);
    row.connect_apply(move |row| on_apply(row.text().trim().to_string()));
    row
}

/// Sets the hand cursor on every button inside `widget`, the ones libadwaita keeps to itself
/// included.
fn point_buttons(widget: &impl IsA<gtk::Widget>) {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if current.is::<gtk::Button>() {
            current.set_cursor(Some(&crate::ui::pointer()));
        }
        point_buttons(&current);
        child = current.next_sibling();
    }
}

/// A switch that saves one setting and applies it right away.
fn toggle_row(
    title: &str,
    subtitle: &str,
    active: bool,
    set: fn(&mut settings::Settings, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .cursor(&crate::ui::pointer())
        .active(active)
        .build();
    row.connect_active_notify(move |row| {
        let active = row.is_active();
        save(|s| set(s, active));
        nav::controls_changed();
    });
    row
}

fn general() -> adw::PreferencesPage {
    let current = settings::get();

    let quality = adw::ComboRow::builder()
        .title("Maximum quality")
        .subtitle("The best video resolution to download")
        .cursor(&crate::ui::pointer())
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
        .title("Skip sponsor segments (SponsorBlock)")
        .cursor(&crate::ui::pointer())
        .active(current.skip_sponsors)
        .build();
    sponsors.connect_active_notify(|row| {
        let active = row.is_active();
        save(|s| s.skip_sponsors = active);
    });

    let remove_played = adw::SwitchRow::builder()
        .title("Remove played videos from the queue")
        .cursor(&crate::ui::pointer())
        .active(current.remove_played_from_queue)
        .build();
    remove_played.connect_active_notify(|row| {
        let active = row.is_active();
        save(|s| s.remove_played_from_queue = active);
    });
    let queue = adw::PreferencesGroup::builder().title("Queue").build();
    queue.add(&remove_played);

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
    let languages = adw::PreferencesGroup::builder().title("Language").build();
    languages.add(&language);

    let middle_thumbnails = adw::SwitchRow::builder()
        .title("Natural thumbnails extracted from the videos")
        .cursor(&crate::ui::pointer())
        .active(current.middle_thumbnails)
        .build();
    middle_thumbnails.connect_active_notify(|row| {
        let active = row.is_active();
        // Nothing to refresh when the switch is only being set up.
        let changed = settings::get().middle_thumbnails != active;
        save(|s| s.middle_thumbnails = active);
        if changed {
            nav::thumbnails_changed();
        }
    });

    let hide_shorts = adw::SwitchRow::builder()
        .title("Hide Shorts")
        .cursor(&crate::ui::pointer())
        .active(current.hide_shorts)
        .build();
    hide_shorts.connect_active_notify(|row| {
        let active = row.is_active();
        // Nothing to refresh when the switch is only being set up.
        let changed = settings::get().hide_shorts != active;
        save(|s| s.hide_shorts = active);
        if changed {
            nav::shorts_hidden_changed();
        }
    });

    let downloads = adw::PreferencesGroup::builder().title("Downloads").build();
    downloads.add(&quality);
    downloads.add(&sponsors);
    downloads.add(&rate);

    let thumbnails = adw::PreferencesGroup::builder().title("Thumbnails").build();
    thumbnails.add(&middle_thumbnails);

    let channels = adw::PreferencesGroup::builder().title("Channels").build();
    channels.add(&hide_shorts);

    let mut browsers = vec!["None"];
    browsers.extend(BROWSERS);
    let cookies = adw::ComboRow::builder()
        .title("Cookies from browser")
        .cursor(&crate::ui::pointer())
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
        .build();
    network.add(&cookies);
    network.add(&proxy);

    let browser_button = gtk::Button::builder()
        .label("Set up")
        .cursor(&crate::ui::pointer())
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
    let browser_row = adw::ActionRow::builder().title("Chrome extension").build();
    browser_row.add_suffix(&browser_button);
    let browser = adw::PreferencesGroup::builder().title("Browser").build();
    browser.add(&browser_row);

    let player_buttons = adw::PreferencesGroup::builder()
        .title("Player controls")
        .build();
    player_buttons.add(&toggle_row(
        "Fullscreen button",
        "The button that takes the whole screen",
        current.show_fullscreen_button,
        |s, on| s.show_fullscreen_button = on,
    ));
    player_buttons.add(&toggle_row(
        "Theater mode button",
        "The button that fills the window with the video",
        current.show_theater_button,
        |s, on| s.show_theater_button = on,
    ));
    player_buttons.add(&toggle_row(
        "Pop-out button",
        "The button that opens the video in its own window",
        current.show_popout_button,
        |s, on| s.show_popout_button = on,
    ));
    player_buttons.add(&toggle_row(
        "Playback speed button",
        "The speed menu of the player",
        current.show_speed_button,
        |s, on| s.show_speed_button = on,
    ));
    player_buttons.add(&toggle_row(
        "Chapters button",
        "Only the button in the player controls; the chapters of the video page stay",
        current.show_chapters_button,
        |s, on| s.show_chapters_button = on,
    ));
    player_buttons.add(&toggle_row(
        "Restricted mode controls",
        "The buttons and pages that set up the restricted mode, here and on the video pages, in the navigation bar and in the remote",
        current.show_restricted_controls,
        |s, on| s.show_restricted_controls = on,
    ));
    player_buttons.add(&toggle_row(
        "Display social recommendations",
        "The Social settings page here, and the Recommendations tab on the home page",
        current.show_social,
        |s, on| s.show_social = on,
    ));

    let page = adw::PreferencesPage::new();
    page.add(&languages);
    page.add(&downloads);
    page.add(&thumbnails);
    page.add(&channels);
    page.add(&queue);
    page.add(&player_buttons);
    page.add(&network);
    page.add(&browser);
    page
}

/// The page with the switch of the phone remote and the way to pair a phone.
fn remote_page() -> adw::PreferencesPage {
    let switch = adw::SwitchRow::builder()
        .title("Phone remote")
        .subtitle("If you have a hyperb:// browser in your phone that can be used to control this desktop remotely")
        .cursor(&crate::ui::pointer())
        .active(remote::enabled())
        .build();
    let pair_button = gtk::Button::builder()
        .label("Show QR code")
        .cursor(&crate::ui::pointer())
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
    let pair = adw::ActionRow::builder().title("Pair a phone").build();
    pair.add_suffix(&pair_button);

    let android = gtk::LinkButton::builder()
        .label("Android")
        .uri("https://github.com/hypereffect/hyperb#installation")
        .cursor(&crate::ui::pointer())
        .valign(gtk::Align::Center)
        .build();
    let web = gtk::LinkButton::builder()
        .label("Web")
        .uri("https://hypereffect.github.io/hyperb/")
        .cursor(&crate::ui::pointer())
        .valign(gtk::Align::Center)
        .build();
    let app = adw::ActionRow::builder()
        .title("hyperb")
        .subtitle("Install it on Android, or use the web version as a fallback")
        .build();
    app.add_suffix(&android);
    app.add_suffix(&web);

    let group = adw::PreferencesGroup::builder().title("Remote").build();
    group.add(&switch);
    group.add(&pair);
    group.add(&app);

    let devices = adw::PreferencesGroup::builder()
        .title("Paired phones")
        .build();
    let rows = std::rc::Rc::new(RefCell::new(Vec::new()));
    fill_devices(&devices, &rows);
    let weak = devices.downgrade();
    remote::connect_changed(move || {
        let Some(devices) = weak.upgrade() else {
            return false;
        };
        fill_devices(&devices, &rows);
        true
    });

    let page = adw::PreferencesPage::new();
    page.add(&group);
    page.add(&devices);
    page
}

/// The page with who the user is on Nostr.
fn social_page() -> adw::PreferencesPage {
    let config = social::config();

    let name = entry_row("Name", "The name you go by", &config.name, |text| {
        if let Err(error) = social::update(|config| config.name = text) {
            nav::toast(&format!("Could not save the name: {error}"));
        }
    });

    let public_key = adw::ActionRow::builder()
        .title("Public key")
        .subtitle(social::public_key())
        .subtitle_selectable(true)
        .subtitle_lines(2)
        .css_classes(["property"])
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy")
        .cursor(&crate::ui::pointer())
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    copy.connect_clicked({
        let public_key = public_key.clone();
        move |button| {
            button
                .clipboard()
                .set_text(&public_key.subtitle().unwrap_or_default());
            nav::toast("Public key copied");
        }
    });
    public_key.add_suffix(&copy);

    // The row has its own button to reveal the key.
    let secret_key = adw::PasswordEntryRow::builder()
        .title("Secret key")
        .text(&config.secret_key)
        .show_apply_button(true)
        .tooltip_text(
            "Generated for you, in hex. Paste another one, in hex or as an nsec, to use it instead",
        )
        .build();
    point_buttons(&secret_key);
    secret_key.connect_apply({
        let public_key = public_key.clone();
        move |row| match social::set_secret_key(&row.text()) {
            Ok(public) => {
                row.remove_css_class("error");
                row.set_text(&social::config().secret_key);
                public_key.set_subtitle(&public);
            }
            Err(error) => {
                row.add_css_class("error");
                nav::toast(&format!("Not a valid secret key: {error}"));
            }
        }
    });

    let group = adw::PreferencesGroup::builder().title("You").build();
    group.add(&name);
    group.add(&secret_key);
    group.add(&public_key);

    let page = adw::PreferencesPage::new();
    page.add(&group);
    page
}

/// Puts the rows of the paired phones in `group`, in place of the ones in `rows`.
fn fill_devices(group: &adw::PreferencesGroup, rows: &RefCell<Vec<adw::ActionRow>>) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let devices = remote::devices();
    if devices.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No phones paired yet")
            .css_classes(["dim-label"])
            .build();
        group.add(&row);
        rows.borrow_mut().push(row);
    }
    for device in devices {
        let paired = gtk::glib::DateTime::from_unix_local(device.paired)
            .and_then(|time| time.format("%x"))
            .map(|date| format!("Paired on {date}"))
            .unwrap_or_default();
        let row = adw::ActionRow::builder()
            .title(format!("Device {}", &device.key[..8.min(device.key.len())]))
            .subtitle(paired)
            .build();
        let unpair = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Unpair")
            .cursor(&crate::ui::pointer())
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        unpair.connect_clicked(move |_| remote::unpair(&device.key));
        row.add_suffix(&unpair);
        group.add(&row);
        rows.borrow_mut().push(row);
    }
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
    let show = |picture: &gtk::Picture, address: &gtk::Label| {
        let text = remote::address();
        picture.set_paintable(remote::qr_code(&text).as_ref());
        address.set_text(&text);
    };
    show(&picture, &address);
    // The code changes once a phone pairs with it.
    let (weak_picture, weak_address) = (picture.downgrade(), address.downgrade());
    remote::connect_changed(move || {
        let (Some(picture), Some(address)) = (weak_picture.upgrade(), weak_address.upgrade())
        else {
            return false;
        };
        show(&picture, &address);
        true
    });

    let note = gtk::Label::builder()
        .label("The code pairs one phone, then a new one is shown. The old one stops working, and the paired phones stay paired.")
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

/// A row showing where something is kept, the path selectable so that it can be copied.
fn location_row(title: &str, path: std::path::PathBuf, description: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(path.display().to_string())
        .subtitle_selectable(true)
        // Wraps rather than cutting a long location short.
        .subtitle_lines(2)
        .tooltip_text(description)
        .build()
}

fn link_row(title: &str, uri: String) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(&uri)
        .activatable(true)
        .cursor(&crate::ui::pointer())
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

/// Empties a group of the rows put in it before, and remembers the rows added after.
fn clear_group(
    slot: &'static std::thread::LocalKey<RefCell<Vec<gtk::Widget>>>,
    group: &adw::PreferencesGroup,
) {
    slot.with(|rows| {
        for row in rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
    });
}

/// Empties a group and puts in it what is muted or hidden now, with a way to undo each.
pub fn refresh_muted() {
    MUTED_CHANNELS.with(|slot| {
        let Some(group) = slot.borrow().clone() else {
            return;
        };
        clear_group(&MUTED_CHANNEL_ROWS, &group);
        let channels = muted::channels();
        if channels.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No muted channels")
                .css_classes(["dim-label"])
                .build();
            group.add(&row);
            MUTED_CHANNEL_ROWS.with(|rows| rows.borrow_mut().push(row.upcast()));
        }
        for channel in channels {
            let row = adw::ActionRow::builder()
                .title(&channel.name)
                .subtitle("Channel")
                .build();
            let button = gtk::Button::builder()
                .label("Unmute")
                .cursor(&crate::ui::pointer())
                .valign(gtk::Align::Center)
                .build();
            let id = channel.id.clone();
            button.connect_clicked(move |_| nav::unmute_channel(&id));
            row.add_suffix(&button);
            group.add(&row);
            MUTED_CHANNEL_ROWS.with(|rows| rows.borrow_mut().push(row.upcast()));
        }
    });

    HIDDEN_VIDEOS.with(|slot| {
        let Some(group) = slot.borrow().clone() else {
            return;
        };
        clear_group(&HIDDEN_VIDEO_ROWS, &group);
        let videos = muted::videos();
        if videos.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No hidden videos")
                .css_classes(["dim-label"])
                .build();
            group.add(&row);
            HIDDEN_VIDEO_ROWS.with(|rows| rows.borrow_mut().push(row.upcast()));
        }
        for video in videos {
            let title = if video.title.is_empty() {
                &video.id
            } else {
                &video.title
            };
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(if video.channel.is_empty() {
                    "Video"
                } else {
                    video.channel.as_str()
                })
                .build();
            let button = gtk::Button::builder()
                .label("Unhide")
                .cursor(&crate::ui::pointer())
                .valign(gtk::Align::Center)
                .build();
            let id = video.id.clone();
            button.connect_clicked(move |_| nav::unhide_video(&id));
            row.add_suffix(&button);
            group.add(&row);
            HIDDEN_VIDEO_ROWS.with(|rows| rows.borrow_mut().push(row.upcast()));
        }
    });
}

/// The page with the channels muted and the videos hidden, and the way to undo each.
fn muted_page() -> adw::PreferencesPage {
    let channels = adw::PreferencesGroup::builder()
        .title("Muted channels")
        .description("Their videos are left out of search results, recommendations, channel lists and the rest")
        .build();
    let videos = adw::PreferencesGroup::builder()
        .title("Hidden videos")
        .description(
            "They are left out of search results, recommendations, channel lists and the rest",
        )
        .build();
    MUTED_CHANNELS.with(|slot| *slot.borrow_mut() = Some(channels.clone()));
    HIDDEN_VIDEOS.with(|slot| *slot.borrow_mut() = Some(videos.clone()));
    refresh_muted();

    let page = adw::PreferencesPage::new();
    page.add(&channels);
    page.add(&videos);
    page
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
        .cursor(&crate::ui::pointer())
        .build();
    details.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    details.connect_activated(|row| about_dialog().present(Some(row)));

    let links = adw::PreferencesGroup::new();
    links.add(&details);
    links.add(&link_row("Website", WEBSITE.to_string()));
    links.add(&link_row("Report an issue", format!("{WEBSITE}/issues")));

    let files = adw::PreferencesGroup::builder()
        .title("File paths")
        .tooltip_text("Where do we save each type of data used by Cathode")
        .build();
    files.add(&location_row(
        "Settings",
        crate::paths::config().join("settings.json"),
        "Global options, subscriptions, collections and restricted mode settings.",
    ));
    files.add(&location_row(
        "Videos",
        crate::fetch::cache_dir(),
        "Videos and their subtitles, deleted after 48 hours unless saved.",
    ));
    files.add(&location_row(
        "Permanent non-config data",
        crate::paths::data(),
        "Queue and other current app views, history, log and saved videos.",
    ));
    files.add(&location_row(
        "Downloaded tools",
        crate::tools::dir(),
        "The programs Cathode downloads and requires in order to work, mostly yt-dlp, ffmpeg and deno",
    ));

    let page = adw::PreferencesPage::new();
    page.add(&top);
    page.add(&links);
    page.add(&files);
    page
}

/// The page, with `parental` as the parental control one.
pub fn build(parental: &impl IsA<gtk::Widget>) -> gtk::Box {
    let stack = gtk::Stack::new();
    stack.add_named(&general(), Some("general"));
    stack.add_named(&muted_page(), Some("muted"));
    stack.add_named(parental, Some("parental"));
    stack.add_named(&remote_page(), Some("remote"));
    stack.add_named(&social_page(), Some("social"));
    stack.add_named(&about(), Some("about"));

    let sidebar = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .width_request(200)
        .build();
    for (name, title, icon) in [
        ("general", "General", "preferences-system-symbolic"),
        ("muted", "Muted & Hidden", "view-conceal-symbolic"),
        ("parental", "Restricted Mode", "system-users-symbolic"),
        ("remote", "Remote Control", "phone-symbolic"),
        ("social", "Social", "system-users-symbolic"),
        ("about", "About", "help-about-symbolic"),
    ] {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&gtk::Label::new(Some(title)));
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .cursor(&crate::ui::pointer())
            .build();
        row.set_widget_name(name);
        match name {
            "parental" => RESTRICTED_ROW.with(|slot| *slot.borrow_mut() = Some(row.clone())),
            "social" => SOCIAL_ROW.with(|slot| *slot.borrow_mut() = Some(row.clone())),
            _ => {}
        }
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
    show_restricted_controls(settings::get().show_restricted_controls);
    show_social(settings::get().show_social);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&sidebar);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    stack.set_hexpand(true);
    root.append(&stack);
    root
}

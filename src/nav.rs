//! Pages, history, the queue and everything that decides what the widgets show.
//! Everything here runs on the GTK main thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tokio::sync::mpsc::UnboundedSender;

use crate::cache;
use crate::controls;
use crate::fetch::{Entry, Request, Response, Source, VideoInfo};
use crate::format;
use crate::profile::{self, ChannelRef, Item, PlaylistRef};
use crate::ui::{self, CacheRow, Ui};

#[derive(Clone)]
enum Page {
    Home,
    Settings,
    Search {
        query: String,
        by_date: bool,
    },
    Channel {
        id: String,
        name: String,
        query: String,
    },
    Playlist {
        id: String,
        title: String,
    },
    Video(Entry),
    Cache,
    Player(Entry),
}

impl Page {
    /// Identifies the page's content, and is what responses are matched against.
    fn key(&self) -> String {
        match self {
            Page::Home => "home".to_string(),
            Page::Settings => "settings".to_string(),
            Page::Playlist { id, .. } => format!("l:{id}"),
            Page::Search { query, by_date } => format!("s:{by_date}:{query}"),
            Page::Channel { id, query, .. } => format!("c:{id}:{query}"),
            Page::Video(entry) => format!("v:{}", entry.id),
            Page::Cache => "cache".to_string(),
            Page::Player(entry) => format!("p:{}", entry.id),
        }
    }
}

/// A listing as far as it was received.
struct Listing {
    entries: Vec<Entry>,
    done: bool,
}

enum QueueState {
    Waiting,
    Downloading,
    Ready,
    Failed(String),
}

struct QueueItem {
    entry: Entry,
    state: QueueState,
}

struct State {
    requests: Option<UnboundedSender<Request>>,
    history: Vec<Page>,
    index: usize,

    /// Listings and video details already fetched, so going back doesn't wait on yt-dlp again.
    listings: HashMap<String, Listing>,
    infos: HashMap<String, VideoInfo>,
    /// Videos whose details were asked for because the cache doesn't know them.
    info_lookups: HashSet<String>,

    /// Small thumbnails for lists, and large ones for video pages.
    thumbnails: [HashMap<String, gdk::Texture>; 2],
    thumbnails_requested: HashSet<(String, bool)>,
    waiting_pictures: HashMap<String, Vec<glib::WeakRef<gtk::Picture>>>,

    queue: Vec<QueueItem>,
    /// The queue downloads one video at a time, this is the one being downloaded.
    queue_downloading: Option<String>,

    /// Downloads running, whoever asked for them.
    inflight: HashMap<String, Entry>,
    progress: HashMap<String, f64>,
    /// Video the player page is waiting for to be downloaded.
    awaiting_play: Option<String>,
    /// Files opened from the command line.
    local_files: HashMap<String, PathBuf>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        requests: None,
        history: vec![Page::Home],
        index: 0,
        listings: HashMap::new(),
        infos: HashMap::new(),
        info_lookups: HashSet::new(),
        thumbnails: [HashMap::new(), HashMap::new()],
        thumbnails_requested: HashSet::new(),
        waiting_pictures: HashMap::new(),
        queue: Vec::new(),
        queue_downloading: None,
        inflight: HashMap::new(),
        progress: HashMap::new(),
        awaiting_play: None,
        local_files: HashMap::new(),
    });
}

/// Never call into GTK from inside `f`, signal handlers may well come back here.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

pub fn send(request: Request) {
    with(|state| {
        if let Some(requests) = &state.requests {
            let _ = requests.send(request);
        }
    });
}

fn current() -> Page {
    with(|state| state.history[state.index].clone())
}

pub fn toast(message: &str) {
    ui::get().toasts.add_toast(adw::Toast::new(message));
}

/// "3:21  ·  1.2M views"
pub fn details(entry: &Entry) -> String {
    let mut details = Vec::new();
    if let Some(duration) = entry.duration {
        details.push(format::duration(duration));
    }
    if let Some(views) = entry.views {
        details.push(format!("{} views", format::count(views)));
    }
    if let Some(published) = entry.published {
        details.push(format::ago(published));
    }
    details.join("  ·  ")
}

// Thumbnails

/// The thumbnail of a video if we have it. Otherwise it is requested, and `picture` is given
/// the texture when it arrives.
pub fn thumbnail(id: &str, large: bool, picture: Option<&gtk::Picture>) -> Option<gdk::Texture> {
    let (texture, request) = with(|state| {
        if let Some(texture) = state.thumbnails[large as usize].get(id) {
            return (Some(texture.clone()), false);
        }

        if let Some(picture) = picture {
            state
                .waiting_pictures
                .entry(id.to_string())
                .or_default()
                .push(picture.downgrade());
        }
        (
            None,
            state.thumbnails_requested.insert((id.to_string(), large)),
        )
    });

    if request {
        send(Request::Thumbnail {
            id: id.to_string(),
            large,
        });
    }
    texture
}

// Results

fn append_entries(ui: &Ui, entries: &[Entry]) {
    let objects: Vec<glib::BoxedAnyObject> = entries
        .iter()
        .cloned()
        .map(glib::BoxedAnyObject::new)
        .collect();
    ui.store.extend_from_slice(&objects);
}

fn update_results_status(key: &str) {
    let (count, done) = with(|state| {
        state
            .listings
            .get(key)
            .map_or((0, true), |listing| (listing.entries.len(), listing.done))
    });

    let text = match (count, done) {
        (0, true) => "No results".to_string(),
        (0, false) => "Loading…".to_string(),
        (count, true) => format!("{count} results"),
        (count, false) => format!("{count} results, loading more…"),
    };
    ui::get().results_status.set_text(&text);
}

fn show_listing(key: String, source: Source) {
    let ui = ui::get();
    ui.stack.set_visible_child_name("results");
    ui.store.remove_all();

    let cached = with(|state| state.listings.get(&key).map(|l| l.entries.clone()));
    match cached {
        Some(entries) => {
            note_listed(&key, &entries);
            append_entries(&ui, &entries);
        }
        None => {
            with(|state| {
                state.listings.insert(
                    key.clone(),
                    Listing {
                        entries: Vec::new(),
                        done: false,
                    },
                )
            });
            send(Request::Listing {
                key: key.clone(),
                source,
            });
        }
    }
    update_results_status(&key);
}

// Video page

fn show_video(entry: &Entry) {
    let ui = ui::get();
    let info = with(|state| state.infos.get(&Page::Video(entry.clone()).key()).cloned());

    ui.video_title.set_text(&entry.title);
    ui.video_channel.set_label(&entry.channel);
    ui.video_channel.set_visible(!entry.channel_id.is_empty());
    ui.video_channel
        .set_sensitive(!profile::restricted() || profile::allows_channel(&entry.channel_id));

    let mut meta = Vec::new();
    // The exact date comes with the details, a listing only knows roughly how long ago it was.
    if let Some(date) = info.as_ref().and_then(|info| info.upload_date.as_deref()) {
        meta.push(format::date(date));
    } else if let Some(published) = entry.published {
        meta.push(format::ago(published));
    }
    if let Some(views) = info.as_ref().and_then(|info| info.views).or(entry.views) {
        meta.push(format!("{} views", format::count(views)));
    }
    if let Some(likes) = info.as_ref().and_then(|info| info.likes) {
        meta.push(format!("{} likes", format::count(likes)));
    }
    if let Some(duration) = entry.duration {
        meta.push(format::duration(duration));
    }
    if cache::find(&entry.id).is_some() {
        meta.push("Downloaded".to_string());
    }
    ui.video_meta.set_text(&meta.join("  ·  "));

    ui.video_description.set_text(&match &info {
        Some(info) => info.description.clone(),
        None => "Loading details…".to_string(),
    });

    let large = thumbnail(&entry.id, true, None);
    let small = with(|state| state.thumbnails[0].get(&entry.id).cloned());
    ui.video_picture.set_paintable(large.or(small).as_ref());
}

// The restricted profile

/// Whether the video can be watched, given the restricted profile if it is on.
fn video_allowed(entry: &Entry) -> bool {
    !profile::restricted() || profile::allows_video(entry)
}

/// The restricted profile only gets to see what it was given.
fn page_allowed(page: &Page) -> bool {
    if !profile::restricted() {
        return true;
    }

    match page {
        Page::Home => true,
        Page::Settings | Page::Search { .. } | Page::Cache => false,
        Page::Channel { id, .. } => profile::allows_channel(id),
        Page::Playlist { id, .. } => profile::allows_playlist(id),
        Page::Video(entry) | Page::Player(entry) => profile::allows_video(entry),
    }
}

/// Videos listed by an allowed channel or playlist are allowed too.
fn note_listed(key: &str, entries: &[Entry]) {
    let allowed = if let Some(id) = key.strip_prefix("l:") {
        profile::allows_playlist(id)
    } else if let Some(rest) = key.strip_prefix("c:") {
        rest.split(':').next().is_some_and(profile::allows_channel)
    } else {
        false
    };

    if allowed {
        profile::remember_listed(entries);
    }
}

fn apply_mode() {
    let ui = ui::get();
    let restricted = profile::restricted();

    ui.title_stack
        .set_visible_child_name(if restricted { "restricted" } else { "search" });
    ui.downloads.set_visible(!restricted);
    ui.controls_stack
        .set_visible_child_name(if restricted { "restricted" } else { "open" });
}

/// Turns the restricted profile on or off, starting over from the home page.
pub fn set_restricted(restricted: bool) {
    if let Err(error) = profile::update(|profile| profile.restricted = restricted) {
        toast(&format!("Could not save the profile: {error}"));
    }
    profile::forget_listed();

    with(|state| {
        state.history = vec![Page::Home];
        state.index = 0;
    });
    apply_mode();
    show();
}

fn update_allow_buttons() {
    let ui = ui::get();
    let label = |allowed: bool| {
        if allowed {
            "Remove from restricted profile"
        } else {
            "Add to restricted profile"
        }
    };

    let (channel, video) = match current() {
        Page::Channel { id, .. } => (Some(label(profile::allows_channel(&id))), None),
        Page::Playlist { id, .. } => (Some(label(profile::allows_playlist(&id))), None),
        Page::Video(entry) => (None, Some(label(profile::lists_video(&entry.id)))),
        _ => (None, None),
    };

    // Only the one in charge of the profile gets to change it.
    let editable = !profile::restricted();
    ui.allow_channel.set_visible(editable && channel.is_some());
    ui.allow_channel.set_label(channel.unwrap_or_default());
    ui.allow_video.set_visible(editable && video.is_some());
    ui.allow_video.set_label(video.unwrap_or_default());
}

/// Adds the page on screen to the restricted profile, or takes it out of it.
pub fn toggle_allowed() {
    let toggle = |allowed: bool, item: Item, remove: Box<dyn FnOnce(&mut profile::Profile)>| {
        if allowed {
            profile::update(remove).map(|_| false)
        } else {
            profile::add(item)
        }
    };

    let result = match current() {
        Page::Channel { id, name, .. } => toggle(
            profile::allows_channel(&id),
            Item::Channel(ChannelRef {
                id: id.clone(),
                name,
            }),
            Box::new(move |p| p.channels.retain(|c| c.id != id)),
        ),
        Page::Playlist { id, title } => toggle(
            profile::allows_playlist(&id),
            Item::Playlist(PlaylistRef {
                id: id.clone(),
                title,
            }),
            Box::new(move |p| p.playlists.retain(|p| p.id != id)),
        ),
        Page::Video(entry) => {
            let id = entry.id.clone();
            toggle(
                profile::lists_video(&entry.id),
                Item::Video(entry),
                Box::new(move |p| p.videos.retain(|v| v.id != id)),
            )
        }
        _ => return,
    };

    match result {
        Ok(true) => toast("Added to the restricted profile"),
        Ok(false) => toast("Removed from the restricted profile"),
        Err(error) => toast(&format!("Could not save the profile: {error}")),
    }
    update_allow_buttons();
}

// Navigation

/// Makes the UI show the current history entry.
fn show() {
    let ui = ui::get();
    let (page, can_back, can_forward) = with(|state| {
        (
            state.history[state.index].clone(),
            state.index > 0,
            state.index + 1 < state.history.len(),
        )
    });
    ui.back.set_sensitive(can_back);
    ui.forward.set_sensitive(can_forward);

    if !matches!(page, Page::Player(_)) {
        if ui.player.id().is_some() {
            ui.player.set_paused(true);
        }
        if ui.window.is_fullscreen() {
            ui.window.set_fullscreened(false);
            ui.toolbar.set_reveal_top_bars(true);
        }
    }

    let key = page.key();
    match page {
        Page::Search { query, by_date } => {
            ui.channel_header.set_visible(false);

            ui.syncing.set(true);
            ui.search_entry.set_text(&query);
            ui.sort.set_selected(by_date as u32);
            ui.syncing.set(false);

            show_listing(key, Source::Search { query, by_date });
        }
        Page::Home => {
            ui.stack.set_visible_child_name("home");
            controls::show_home();
        }
        Page::Settings => {
            ui.stack.set_visible_child_name("settings");
            controls::show_settings();
        }
        Page::Playlist { id, title } => {
            ui.channel_header.set_visible(true);
            ui.channel_title.set_text(&title);
            // There is nothing to search in a playlist.
            ui.channel_entry.set_visible(false);

            show_listing(key, Source::Playlist { id });
        }
        Page::Channel { id, name, query } => {
            ui.channel_header.set_visible(true);
            ui.channel_title.set_text(&name);
            ui.channel_entry.set_text(&query);
            ui.channel_entry.set_visible(true);

            show_listing(key, Source::Channel { id, query });
        }
        Page::Video(entry) => {
            ui.stack.set_visible_child_name("video");
            show_video(&entry);

            if !with(|state| state.infos.contains_key(&key)) {
                send(Request::Info {
                    key,
                    url: entry.url,
                });
            }
        }
        Page::Cache => show_cache(),
        Page::Player(entry) => show_player(&entry),
    }

    update_allow_buttons();
}

fn navigate(page: Page) {
    if !page_allowed(&page) {
        toast("This is not available in the restricted profile");
        return;
    }

    let reload = with(|state| {
        let key = page.key();
        if state.history[state.index].key() == key {
            // Repeating a search refreshes it, anything else is already on screen.
            let reload = matches!(page, Page::Search { .. } | Page::Cache);
            if reload {
                state.listings.remove(&key);
            }
            return reload;
        }

        state.history.truncate(state.index + 1);
        state.history.push(page);
        state.index += 1;
        true
    });

    if reload {
        show();
    }
}

pub fn init(requests: UnboundedSender<Request>) {
    with(|state| state.requests = Some(requests));
    profile::init();
    apply_mode();
    // Forgets what's left of videos that are gone.
    cache::scan();
    show();
}

pub fn search(query: &str, by_date: bool) {
    let query = query.trim();
    if !query.is_empty() {
        navigate(Page::Search {
            query: query.to_string(),
            by_date,
        });
    }
}

pub fn search_channel(query: &str) {
    if let Page::Channel { id, name, .. } = current() {
        navigate(Page::Channel {
            id,
            name,
            query: query.trim().to_string(),
        });
    }
}

pub fn open_channel(id: &str, name: &str) {
    if !id.is_empty() {
        navigate(Page::Channel {
            id: id.to_string(),
            name: name.to_string(),
            query: String::new(),
        });
    }
}

pub fn open_current_channel() {
    if let Page::Video(entry) = current() {
        open_channel(&entry.channel_id, &entry.channel);
    }
}

pub fn open_video(entry: Entry) {
    navigate(Page::Video(entry));
}

pub fn open_home() {
    navigate(Page::Home);
}

pub fn open_settings() {
    navigate(Page::Settings);
}

pub fn open_playlist(id: &str, title: &str) {
    navigate(Page::Playlist {
        id: id.to_string(),
        title: title.to_string(),
    });
}

pub fn open_cache() {
    navigate(Page::Cache);
}

pub fn back() {
    if with(|state| {
        let can = state.index > 0;
        if can {
            state.index -= 1;
        }
        can
    }) {
        show();
    }
}

pub fn forward() {
    if with(|state| {
        let can = state.index + 1 < state.history.len();
        if can {
            state.index += 1;
        }
        can
    }) {
        show();
    }
}

/// Plays a file from the command line.
pub fn open_file(path: PathBuf) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = format!("file:{}", path.display());

    with(|state| state.local_files.insert(id.clone(), path));
    navigate(Page::Player(Entry {
        id,
        url: String::new(),
        title: name,
        channel: String::new(),
        channel_id: String::new(),
        duration: None,
        views: None,
        published: None,
    }));
}

// Downloads

fn start_download(entry: Entry) {
    let start = with(|state| {
        if state.inflight.contains_key(&entry.id) {
            false
        } else {
            state.inflight.insert(entry.id.clone(), entry.clone());
            true
        }
    });

    if start {
        send(Request::Download { entry });
    }
}

// Player

pub fn play_current() {
    if let Page::Video(entry) = current() {
        navigate(Page::Player(entry));
    }
}

fn show_player(entry: &Entry) {
    let ui = ui::get();
    ui.stack.set_visible_child_name("player");
    ui.player_title.set_text(&entry.title);

    if ui.player.id().as_deref() == Some(&entry.id) {
        ui.player_status.set_visible(false);
        return;
    }

    let local = with(|state| state.local_files.get(&entry.id).cloned());
    match local.or_else(|| cache::find(&entry.id)) {
        Some(path) => start_playback(entry, &path),
        None => {
            with(|state| state.awaiting_play = Some(entry.id.clone()));
            ui.player_status.set_text("Downloading…");
            ui.player_status.set_visible(true);
            start_download(entry.clone());
        }
    }
}

fn start_playback(entry: &Entry, path: &std::path::Path) {
    let ui = ui::get();
    ui.player.load(&entry.id, &path.to_string_lossy());
    ui.player_title.set_text(&entry.title);
    ui.player_status.set_visible(false);
    ui.play_button
        .set_icon_name("media-playback-pause-symbolic");
    refresh_queue();
}

pub fn toggle_pause() {
    let ui = ui::get();
    if ui.player.id().is_some() {
        ui.player.set_paused(!ui.player.is_paused());
        tick();
    }
}

pub fn set_speed(speed: f64) {
    ui::get().player.set_speed(speed);
}

/// `volume` goes from 0 to 1.
pub fn set_volume(volume: f64) {
    ui::get().player.set_volume(volume * 100.0);
}

pub fn seek(fraction: f64) {
    let ui = ui::get();
    let duration = ui.player.duration();
    if duration > 0.0 {
        ui.player.seek_to(fraction * duration);
    }
}

pub fn toggle_fullscreen() {
    let ui = ui::get();
    let fullscreen = !ui.window.is_fullscreen();
    if fullscreen && ui.stack.visible_child_name().as_deref() != Some("player") {
        return;
    }

    ui.window.set_fullscreened(fullscreen);
    ui.toolbar.set_reveal_top_bars(!fullscreen);
    if fullscreen {
        ui.split.set_show_sidebar(false);
    }
}

/// Called a few times a second to keep the player controls up to date.
pub fn tick() {
    let ui = ui::get();
    if ui.stack.visible_child_name().as_deref() != Some("player") || ui.player.id().is_none() {
        return;
    }

    let (position, duration) = (ui.player.position(), ui.player.duration());
    ui.time_label.set_text(&format!(
        "{} / {}",
        format::duration(position),
        format::duration(duration)
    ));
    // Don't fight the user dragging the slider.
    if duration > 0.0 && !ui.seek.state_flags().contains(gtk::StateFlags::ACTIVE) {
        ui.seek.set_value(position / duration);
    }
    ui.play_button.set_icon_name(if ui.player.is_paused() {
        "media-playback-start-symbolic"
    } else {
        "media-playback-pause-symbolic"
    });

    if ui.player.take_ended() {
        play_next_in_queue();
    }
}

fn play_next_in_queue() {
    let ui = ui::get();
    let Some(id) = ui.player.id() else {
        return;
    };

    let next = with(|state| {
        let position = state.queue.iter().position(|item| item.entry.id == id)?;
        state.queue[position + 1..]
            .iter()
            .find(|item| matches!(item.state, QueueState::Ready) && video_allowed(&item.entry))
            .map(|item| item.entry.clone())
    });

    if let Some(next) = next {
        navigate(Page::Player(next));
    }
}

// Queue

pub fn queue_current() {
    if let Page::Video(entry) = current() {
        queue_add(entry);
    }
}

pub fn queue_add(entry: Entry) {
    if !video_allowed(&entry) {
        toast("This video is not available in the restricted profile");
        return;
    }

    let added = with(|state| {
        if state.queue.iter().any(|item| item.entry.id == entry.id) {
            return false;
        }
        state.queue.push(QueueItem {
            entry: entry.clone(),
            state: QueueState::Waiting,
        });
        true
    });

    if added {
        toast("Added to the queue");
        pump_queue();
    } else {
        toast("Already in the queue");
    }
}

fn queue_remove(id: &str) {
    let cancel = with(|state| {
        state.queue.retain(|item| item.entry.id != id);
        let downloading = state.queue_downloading.as_deref() == Some(id);
        if downloading {
            state.queue_downloading = None;
        }
        // The player may need that download too.
        downloading && state.awaiting_play.as_deref() != Some(id)
    });

    if cancel {
        send(Request::Cancel { id: id.to_string() });
    }
    pump_queue();
}

/// Starts the next download of the queue, if none is going on.
fn pump_queue() {
    loop {
        let next = with(|state| {
            if state.queue_downloading.is_some() {
                return None;
            }
            state
                .queue
                .iter()
                .position(|item| matches!(item.state, QueueState::Waiting))
        });
        let Some(index) = next else {
            break;
        };

        let entry = with(|state| state.queue[index].entry.clone());
        if cache::find(&entry.id).is_some() {
            with(|state| state.queue[index].state = QueueState::Ready);
            continue;
        }

        with(|state| {
            state.queue[index].state = QueueState::Downloading;
            state.queue_downloading = Some(entry.id.clone());
        });
        start_download(entry);
        break;
    }

    refresh_queue();
}

fn refresh_queue() {
    let ui = ui::get();
    while let Some(row) = ui.queue_list.first_child() {
        ui.queue_list.remove(&row);
    }

    let playing = ui.player.id();
    let items: Vec<(Entry, String, bool)> = with(|state| {
        state
            .queue
            .iter()
            .map(|item| {
                let status = match &item.state {
                    QueueState::Waiting => "Waiting".to_string(),
                    QueueState::Downloading => match state.progress.get(&item.entry.id) {
                        Some(fraction) => format!("Downloading {:.0}%", fraction * 100.0),
                        None => "Downloading…".to_string(),
                    },
                    QueueState::Ready => "Ready".to_string(),
                    QueueState::Failed(error) => format!("Failed: {error}"),
                };
                let ready = matches!(item.state, QueueState::Ready);
                (item.entry.clone(), status, ready)
            })
            .collect()
    });

    if items.is_empty() {
        ui.queue_list.append(
            &gtk::Label::builder()
                .label("The queue is empty")
                .css_classes(["dim-label"])
                .margin_top(24)
                .build(),
        );
    }

    for (entry, status, ready) in items {
        let is_playing = playing.as_deref() == Some(&entry.id);

        let row = adw::ActionRow::builder()
            .title(&entry.title)
            .subtitle(if is_playing {
                format!("Playing · {status}")
            } else {
                status
            })
            .title_lines(2)
            .use_markup(false)
            .build();

        let play = gtk::Button::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Play")
            .valign(gtk::Align::Center)
            .sensitive(ready && !is_playing && video_allowed(&entry))
            .css_classes(["flat", "circular"])
            .build();
        play.connect_clicked({
            let entry = entry.clone();
            move |_| navigate(Page::Player(entry.clone()))
        });
        row.add_prefix(&play);

        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove from the queue")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        remove.connect_clicked({
            let id = entry.id.clone();
            move |_| queue_remove(&id)
        });
        row.add_suffix(&remove);

        ui.queue_list.append(&row);
    }
}

// Cache page

fn show_cache() {
    let ui = ui::get();
    ui.stack.set_visible_child_name("cache");

    while let Some(row) = ui.cache_list.first_child() {
        ui.cache_list.remove(&row);
    }

    let videos = cache::scan();
    let total: u64 = videos.iter().map(|video| video.size).sum();
    let mut rows = Vec::new();

    for video in videos {
        let entry = video.entry;

        // Videos we know nothing about get their details looked up, once.
        if !video.known {
            let key = Page::Video(entry.clone()).key();
            let lookup = with(|state| {
                !state.infos.contains_key(&key) && state.info_lookups.insert(key.clone())
            });
            if lookup {
                send(Request::Info {
                    key,
                    url: entry.url.clone(),
                });
            }
        }

        let mut subtitle = Vec::new();
        if !entry.channel.is_empty() {
            subtitle.push(entry.channel.clone());
        }
        if let Some(duration) = entry.duration {
            subtitle.push(format::duration(duration));
        }
        subtitle.push(format::bytes(video.size));
        if let Some(modified) = video
            .modified
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        {
            subtitle.push(format::epoch_date(modified.as_secs()));
        }

        let row = adw::ActionRow::builder()
            .title(&entry.title)
            .subtitle(subtitle.join("  ·  "))
            .use_markup(false)
            .activatable(true)
            .build();

        let check = gtk::CheckButton::new();
        check.set_valign(gtk::Align::Center);
        check.connect_toggled(|_| {
            if !ui::get().syncing.get() {
                update_cache_selection();
            }
        });
        row.add_prefix(&check);

        let queue = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add to queue")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        queue.connect_clicked({
            let entry = entry.clone();
            move |_| queue_add(entry.clone())
        });
        row.add_suffix(&queue);

        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete from the cache")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        delete.connect_clicked({
            let entry = entry.clone();
            move |_| confirm_delete(vec![entry.clone()])
        });
        row.add_suffix(&delete);

        ui.cache_list.append(&row);
        rows.push(CacheRow { entry, check });
    }

    let count = rows.len();
    *ui.cache_rows.borrow_mut() = rows;
    ui.cache_summary
        .set_text(&format!("{count} videos · {}", format::bytes(total)));
    update_cache_selection();
}

fn selected_cache_entries() -> Vec<Entry> {
    ui::get()
        .cache_rows
        .borrow()
        .iter()
        .filter(|row| row.check.is_active())
        .map(|row| row.entry.clone())
        .collect()
}

fn update_cache_selection() {
    let ui = ui::get();
    let (selected, total) = {
        let rows = ui.cache_rows.borrow();
        (
            rows.iter().filter(|row| row.check.is_active()).count(),
            rows.len(),
        )
    };

    ui.syncing.set(true);
    ui.cache_select_all
        .set_active(total > 0 && selected == total);
    ui.cache_select_all
        .set_inconsistent(selected > 0 && selected < total);
    ui.syncing.set(false);

    ui.cache_queue.set_sensitive(selected > 0);
    ui.cache_delete.set_sensitive(selected > 0);
    ui.cache_queue.set_label(&if selected > 0 {
        format!("Add {selected} to queue")
    } else {
        "Add to queue".to_string()
    });
    ui.cache_delete.set_label(&if selected > 0 {
        format!("Delete {selected}")
    } else {
        "Delete".to_string()
    });
}

pub fn cache_select_all(selected: bool) {
    let ui = ui::get();
    ui.syncing.set(true);
    for row in ui.cache_rows.borrow().iter() {
        row.check.set_active(selected);
    }
    ui.syncing.set(false);
    update_cache_selection();
}

pub fn cache_queue_selected() {
    for entry in selected_cache_entries() {
        queue_add(entry);
    }
}

pub fn cache_delete_selected() {
    confirm_delete(selected_cache_entries());
}

pub fn cache_open(index: usize) {
    let entry = ui::get()
        .cache_rows
        .borrow()
        .get(index)
        .map(|row| row.entry.clone());
    if let Some(entry) = entry {
        open_video(entry);
    }
}

fn confirm_delete(entries: Vec<Entry>) {
    if entries.is_empty() {
        return;
    }

    let (heading, body) = match entries.as_slice() {
        [entry] => (
            "Delete this video?".to_string(),
            format!("“{}” will be removed from the cache.", entry.title),
        ),
        entries => (
            format!("Delete {} videos?", entries.len()),
            "They will be removed from the cache.".to_string(),
        ),
    };

    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose(
        Some(&ui::get().window),
        gio::Cancellable::NONE,
        move |response| {
            if response == "delete" {
                delete_from_cache(&entries);
            }
        },
    );
}

fn delete_from_cache(entries: &[Entry]) {
    let ids: Vec<String> = entries.iter().map(|entry| entry.id.clone()).collect();
    cache::delete(&ids);

    // Downloaded videos of the queue would just be downloaded again otherwise.
    with(|state| {
        state.queue.retain(|item| {
            !(matches!(item.state, QueueState::Ready) && ids.contains(&item.entry.id))
        })
    });
    refresh_queue();

    toast(&match ids.len() {
        1 => "Deleted 1 video".to_string(),
        count => format!("Deleted {count} videos"),
    });
    if matches!(current(), Page::Cache) {
        show_cache();
    }
}

// Responses

pub fn handle(response: Response) {
    let ui = ui::get();

    match response {
        Response::Entries { key, entries } => {
            with(|state| {
                state
                    .listings
                    .entry(key.clone())
                    .or_insert_with(|| Listing {
                        entries: Vec::new(),
                        done: false,
                    })
                    .entries
                    .extend(entries.iter().cloned())
            });

            note_listed(&key, &entries);

            if current().key() == key {
                append_entries(&ui, &entries);
                update_results_status(&key);
            }
        }
        Response::ListingDone { key, error } => {
            let failed = error.is_some();
            with(|state| {
                if failed {
                    // Not remembered, so that visiting the page again tries again.
                    state.listings.remove(&key);
                } else if let Some(listing) = state.listings.get_mut(&key) {
                    listing.done = true;
                }
            });

            if current().key() == key {
                match error {
                    Some(error) => ui
                        .results_status
                        .set_text(&format!("Failed to load: {error}")),
                    None => update_results_status(&key),
                }
            }
        }
        Response::Info { key, result } => {
            let on_page = current().key() == key;

            match result {
                Ok(info) => {
                    // Videos that were in the cache before we kept track of them.
                    let unknown = cache::find(&info.entry.id).is_some()
                        && !cache::scan()
                            .iter()
                            .any(|v| v.known && v.entry.id == info.entry.id);
                    if unknown {
                        cache::record(&info.entry);
                    }

                    with(|state| state.infos.insert(key, info));
                    if on_page {
                        if let Page::Video(entry) = current() {
                            show_video(&entry);
                        }
                    }
                    if unknown && matches!(current(), Page::Cache) {
                        show_cache();
                    }
                }
                Err(error) if on_page => {
                    ui.video_description
                        .set_text(&format!("Failed to load details: {error}"));
                }
                Err(_) => {}
            }
        }
        Response::Resolved { result } => controls::resolved(result),
        Response::Setup(status) => {
            use crate::tools::Status;

            let banner = &ui.setup_banner;
            match status {
                Status::Checking => {}
                Status::Installing { tool, fraction } => {
                    banner.set_title(&match fraction {
                        Some(fraction) => {
                            format!("Downloading {tool}… {:.0}%", fraction * 100.0)
                        }
                        None => format!("Setting up {tool}…"),
                    });
                    banner.set_revealed(true);
                }
                Status::Done { errors } => {
                    if errors.is_empty() {
                        banner.set_revealed(false);
                    } else {
                        banner.set_title(&errors.join(" · "));
                        banner.set_button_label(Some("Dismiss"));
                        banner.connect_button_clicked(|banner| banner.set_revealed(false));
                        banner.set_revealed(true);
                    }
                }
            }
        }
        Response::Thumbnail { id, large, image } => {
            let bytes = glib::Bytes::from_owned(image.rgba);
            let texture: gdk::Texture = gdk::MemoryTexture::new(
                image.width as i32,
                image.height as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &bytes,
                image.width as usize * 4,
            )
            .upcast();

            let pictures = with(|state| {
                state.thumbnails[large as usize].insert(id.clone(), texture.clone());
                state.waiting_pictures.remove(&id).unwrap_or_default()
            });
            for picture in pictures.iter().filter_map(|picture| picture.upgrade()) {
                // The picture may have been recycled for another video since.
                if picture.widget_name() == id {
                    picture.set_paintable(Some(&texture));
                }
            }

            if let Page::Video(entry) = current() {
                let have_large = with(|state| state.thumbnails[1].contains_key(&id));
                if entry.id == id && (large || !have_large) {
                    ui.video_picture.set_paintable(Some(&texture));
                }
            }
        }
        Response::Progress { id, fraction } => {
            let changed = with(|state| {
                let before = state.progress.insert(id.clone(), fraction);
                before.is_none_or(|before| (before * 100.0).floor() != (fraction * 100.0).floor())
            });
            if !changed {
                return;
            }

            if with(|state| state.queue_downloading.as_deref() == Some(&id)) {
                refresh_queue();
            }
            if with(|state| state.awaiting_play.as_deref() == Some(&id)) {
                ui.player_status
                    .set_text(&format!("Downloading… {:.0}%", fraction * 100.0));
            }
        }
        Response::Downloaded { id, result } => {
            let entry = with(|state| {
                state.progress.remove(&id);
                state.inflight.remove(&id)
            });

            if let (Ok(_), Some(entry)) = (&result, &entry) {
                cache::record(entry);
            }

            // The queue and the player may both have wanted this video.
            let queued = with(|state| {
                if state.queue_downloading.as_deref() == Some(&id) {
                    state.queue_downloading = None;
                }
                if let Some(item) = state.queue.iter_mut().find(|item| item.entry.id == id) {
                    item.state = match &result {
                        Ok(_) => QueueState::Ready,
                        Err(error) => QueueState::Failed(error.clone()),
                    };
                }
                state
                    .awaiting_play
                    .take_if(|awaiting| *awaiting == id)
                    .is_some()
            });

            if queued {
                match (&result, current()) {
                    (Ok(path), Page::Player(entry)) if entry.id == id => {
                        start_playback(&entry, path)
                    }
                    (Err(error), Page::Player(entry)) if entry.id == id => {
                        ui.player_status
                            .set_text(&format!("Download failed: {error}"));
                    }
                    _ => {}
                }
            }

            if let Err(error) = &result {
                if error != "Cancelled" {
                    toast(&format!("Download failed: {error}"));
                }
            }

            pump_queue();
            if matches!(current(), Page::Cache) {
                show_cache();
            }
        }
    }
}

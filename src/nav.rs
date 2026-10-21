//! Pages, history, the queue and everything that decides what the widgets show.
//! Everything here runs on the GTK main thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tokio::sync::mpsc::UnboundedSender;

use crate::cache;
use crate::collections;
use crate::controls;
use crate::fetch::{Chapter, Entry, Request, Response, SearchFilter, SearchSort, Source, VideoInfo};
use crate::format;
use crate::history;
use crate::library;
use crate::settings;
use crate::profile::{self, ChannelRef, Item, PlaylistRef};
use crate::state;
use crate::subscriptions::{self, Subscription};
use crate::ui::{self, CacheRow, Ui};

/// The videos shown at first on the history page, and each time it is scrolled to the end.
const HISTORY_PAGE: usize = 10;
/// How often the subscriptions are checked for new videos.
const FEED_REFRESH_SECONDS: u32 = 30 * 60;

#[derive(Clone)]
enum Page {
    Home,
    Settings,
    Search {
        query: String,
        sort: SearchSort,
        filter: SearchFilter,
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
    /// What YouTube suggests after a video.
    Related(Entry),
    Cache,
    History,
    Collection(String),
}

impl Page {
    /// Identifies the page's content, and is what responses are matched against.
    fn key(&self) -> String {
        match self {
            Page::Home => "home".to_string(),
            Page::Settings => "settings".to_string(),
            Page::Playlist { id, .. } => format!("l:{id}"),
            Page::Search {
                query,
                sort,
                filter,
            } => {
                format!("s:{}:{}:{}", *sort as u8, *filter as u8, query)
            }
            Page::Channel { id, query, .. } => format!("c:{id}:{query}"),
            Page::Video(entry) => format!("v:{}", entry.id),
            Page::Related(entry) => format!("r:{}", entry.id),
            Page::Cache => "cache".to_string(),
            Page::History => "history".to_string(),
            Page::Collection(name) => format!("collection:{name}"),
        }
    }
}

/// A listing as far as it was received.
struct Listing {
    entries: Vec<Entry>,
    /// The channels of a search.
    channels: Vec<ChannelRef>,
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
    /// The size picked in the player for a video, when it is not the usual one.
    picked: HashMap<String, u32>,
    /// The video in the player that another size is being downloaded for.
    switching: Option<String>,
    progress: HashMap<String, f64>,
    /// The video in the player, which may still be downloading.
    playing: Option<Entry>,
    /// Video the player is waiting for to be downloaded.
    awaiting_play: Option<String>,
    /// The video to load paused, when restoring the last session.
    start_paused: Option<String>,
    /// Videos to save to a collection (the name) once they are downloaded.
    pending_saves: HashMap<String, String>,
    /// Files opened from the command line.
    local_files: HashMap<String, PathBuf>,
    /// Chapter picked for a video that is still loading: where to seek to once it plays.
    pending_seek: Option<(String, f64)>,
    /// Where each video was left, by YouTube id, whatever file it was played from.
    positions: HashMap<String, f64>,
    /// Width limits of the queue sidebar to go back to, while in fullscreen.
    sidebar_widths: Option<(f64, f64)>,
    /// Where to pause, when the playback started from a chapter.
    chapter_stop: Option<f64>,
    /// The sponsor segments of the videos looked up, `(start, end)` in seconds. Videos without
    /// any are there too, with an empty list.
    segments: HashMap<String, Vec<(f64, f64)>>,
    segment_lookups: HashSet<String>,

    /// Ids of the videos of the feed on screen, so it is only rebuilt when it changed.
    feed_ids: Vec<String>,

    /// Where to read the next entries of the history file from, and the day of the last one shown.
    history_offset: u64,
    history_done: bool,
    history_day: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        requests: None,
        history: vec![Page::Home],
        index: 0,
        listings: HashMap::new(),
        pending_seek: None,
        positions: HashMap::new(),
        chapter_stop: None,
        segments: HashMap::new(),
        segment_lookups: HashSet::new(),
        sidebar_widths: None,
        infos: HashMap::new(),
        info_lookups: HashSet::new(),
        thumbnails: [HashMap::new(), HashMap::new()],
        thumbnails_requested: HashSet::new(),
        waiting_pictures: HashMap::new(),
        queue: Vec::new(),
        queue_downloading: None,
        inflight: HashMap::new(),
        picked: HashMap::new(),
        switching: None,
        progress: HashMap::new(),
        playing: None,
        awaiting_play: None,
        start_paused: None,
        pending_saves: HashMap::new(),
        local_files: HashMap::new(),
        feed_ids: Vec::new(),
        history_offset: 0,
        history_done: true,
        history_day: None,
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

/// Shows a message at the top of the video for a moment.
pub fn player_message(message: &str) {
    let ui = ui::get();
    ui.player_message.set_text(message);
    ui.player_message.set_visible(true);
    if let Some(timer) = ui.player_message_timer.borrow_mut().take() {
        timer.remove();
    }
    let timer = glib::timeout_add_local_once(std::time::Duration::from_millis(1200), || {
        let ui = ui::get();
        ui.player_message_timer.borrow_mut().take();
        ui.player_message.set_visible(false);
    });
    *ui.player_message_timer.borrow_mut() = Some(timer);
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
    // Files have no thumbnail to be had.
    if id.starts_with(collections::LOCAL_PREFIX) {
        return None;
    }

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

fn append_to(store: &gio::ListStore, entries: &[Entry]) {
    let objects: Vec<glib::BoxedAnyObject> = entries
        .iter()
        .cloned()
        .map(glib::BoxedAnyObject::new)
        .collect();
    store.extend_from_slice(&objects);
}

fn append_entries(ui: &Ui, entries: &[Entry]) {
    append_to(&ui.store, entries);
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

/// Fills the row of channels above the results, which is hidden when there are none.
fn show_channel_results(channels: &[ChannelRef]) {
    let ui = ui::get();
    while let Some(child) = ui.channel_results.first_child() {
        ui.channel_results.remove(&child);
    }
    for channel in channels
        .iter()
        .filter(|c| !profile::restricted() || profile::allows_channel(&c.id))
    {
        let button = gtk::Button::builder().cursor(&crate::ui::pointer())
            .label(&channel.name)
            .tooltip_text("Open the channel")
            .build();
        let (id, name) = (channel.id.clone(), channel.name.clone());
        button.connect_clicked(move |_| {
            navigate(Page::Channel {
                id: id.clone(),
                name: name.clone(),
                query: String::new(),
            })
        });
        ui.channel_results.append(&button);
    }
    ui.channel_results_row
        .set_visible(ui.channel_results.first_child().is_some());
}

fn show_listing(key: String, source: Source) {
    let ui = ui::get();
    ui.stack.set_visible_child_name("results");
    ui.store.remove_all();

    show_channel_results(&[]);
    let cached = with(|state| {
        state
            .listings
            .get(&key)
            .map(|l| (l.entries.clone(), l.channels.clone()))
    });
    match cached {
        Some((entries, channels)) => {
            show_channel_results(&channels);
            note_listed(&key, &entries);
            append_entries(&ui, &entries);
        }
        None => {
            with(|state| {
                state.listings.insert(
                    key.clone(),
                    Listing {
                        entries: Vec::new(),
                        channels: Vec::new(),
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
    if local_path(&entry.id).is_some() {
        let mut saved = if entry.url.is_empty() {
            "On disk"
        } else {
            "Downloaded"
        }
        .to_string();
        let sizes: Vec<String> = cache::sizes(&entry.id).into_iter().map(size_name).collect();
        if !sizes.is_empty() {
            saved.push_str(&format!(" (also {})", sizes.join(", ")));
        }
        meta.push(saved);
    }
    ui.video_meta.set_text(&meta.join("  ·  "));

    ui.video_description.set_text(&match &info {
        Some(info) => info.description.clone(),
        None if entry.url.is_empty() => local_path(&entry.id)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        None => "Loading details…".to_string(),
    });

    let chapters = chapters_of(&entry.id);
    clear_rows(&ui.video_chapters);
    ui.video_chapters.set_visible(!chapters.is_empty());
    for chapter in &chapters {
        let row = adw::ActionRow::builder()
            .title(&chapter.title)
            .subtitle(format::duration(chapter.start))
            .use_markup(false)
            .activatable(true)
            .cursor(&crate::ui::pointer())
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("media-playback-start-symbolic"));
        ui.video_chapters.append(&row);
    }

    let large = thumbnail(&entry.id, true, None);
    let small = with(|state| state.thumbnails[0].get(&entry.id).cloned());
    ui.video_picture.set_paintable(large.or(small).as_ref());
}

fn clear_rows(list: &gtk::ListBox) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
}

/// The chapters of a video, once its details are known. Details are stored under the key of its video page.
fn chapters_of(id: &str) -> Vec<Chapter> {
    with(|state| {
        state
            .infos
            .get(&format!("v:{id}"))
            .map(|info| info.chapters.clone())
            .unwrap_or_default()
    })
}

/// Fills the chapters of the player controls, and marks them on the seek bar.
fn show_player_chapters() {
    let ui = ui::get();
    let chapters = ui
        .player
        .id()
        .map(|id| chapters_of(&id))
        .unwrap_or_default();

    clear_rows(&ui.chapters_list);
    ui.seek.clear_marks();
    ui.chapters_button.set_visible(!chapters.is_empty());

    for chapter in &chapters {
        let label = gtk::Label::builder()
            .label(format!(
                "{}  {}",
                format::duration(chapter.start),
                chapter.title
            ))
            .xalign(0.0)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        ui.chapters_list.append(&label);
    }

    if let Some(total) = chapters.last().map(|chapter| chapter.end) {
        for chapter in chapters.iter().skip(1) {
            ui.seek
                .add_mark(chapter.start / total, gtk::PositionType::Bottom, None);
        }
    }
}

/// What the seek bar tells about the point `x` of its `width` pixels: the chapter and sponsor
/// segment there.
pub fn seek_tooltip(x: f64, width: f64) -> Option<String> {
    let ui = ui::get();
    let id = ui.player.id()?;
    let duration = ui.player.duration();
    if duration <= 0.0 || width <= 0.0 {
        return None;
    }
    let time = (x / width).clamp(0.0, 1.0) * duration;

    let mut lines = Vec::new();
    if let Some(chapter) = chapters_of(&id)
        .iter()
        .find(|chapter| time >= chapter.start && time < chapter.end)
    {
        lines.push(chapter.title.clone());
    }
    if ui
        .sponsor_spans
        .borrow()
        .1
        .iter()
        .any(|(start, end)| time >= *start && time < *end)
    {
        lines.push("Sponsor".to_string());
    }
    lines.push(format::duration(time));
    Some(lines.join("\n"))
}

/// Paints the sponsor segments on the seek bar.
fn show_sponsor_marks(segments: Vec<(f64, f64)>) {
    let ui = ui::get();
    ui.sponsor_spans.borrow_mut().1 = segments;
    ui.seek_marks.queue_draw();
}

/// Plays a chapter of the video on the current page, and pauses when it ends.
pub fn play_chapter(index: usize) {
    if let Page::Video(entry) = current() {
        play_chapter_of(entry, index);
    }
}

/// Plays a chapter of the video in the player.
pub fn play_playing_chapter(index: usize) {
    if let Some(entry) = with(|state| state.playing.clone()) {
        play_chapter_of(entry, index);
    }
}

fn play_chapter_of(entry: Entry, index: usize) {
    let Some(chapter) = chapters_of(&entry.id).get(index).cloned() else {
        return;
    };

    let ui = ui::get();
    with(|state| state.chapter_stop = Some(chapter.end));
    if ui.player.id().as_deref() == Some(&entry.id) {
        ui.player.seek_to(chapter.start);
        ui.player.set_paused(false);
    } else {
        // The video isn't loaded yet, tick() seeks once it is.
        with(|state| state.pending_seek = Some((entry.id.clone(), chapter.start)));
        play(entry);
    }
}

/// The file of a video that is on disk: opened from the command line, cached or in a collection.
fn local_path(id: &str) -> Option<PathBuf> {
    with(|state| state.local_files.get(id).cloned())
        .or_else(|| cache::find_size(id, wanted_size(id)))
        .or_else(|| cache::find(id))
        .or_else(|| collections::find(id))
}

// The restricted mode

/// Whether the video can be watched, given the restricted mode if it is on.
fn video_allowed(entry: &Entry) -> bool {
    !profile::restricted() || profile::allows_video(entry)
}

/// The restricted mode only gets to see what it was given.
fn page_allowed(page: &Page) -> bool {
    if !profile::restricted() {
        return true;
    }

    match page {
        Page::Home => true,
        Page::Settings | Page::Search { .. } | Page::Related(_) | Page::Cache | Page::History => {
            false
        }
        Page::Channel { id, .. } => profile::allows_channel(id),
        Page::Playlist { id, .. } => profile::allows_playlist(id),
        Page::Collection(name) => profile::allows_collection(name),
        Page::Video(entry) => profile::allows_video(entry),
    }
}

/// Videos listed by an allowed channel or playlist are allowed too.
fn note_listed(key: &str, entries: &[Entry]) {
    let allowed = if let Some(id) = key.strip_prefix("l:") {
        profile::allows_playlist(id)
    } else if let Some(rest) = key.strip_prefix("c:") {
        // The id has a colon of its own, the query comes after the next one.
        let end = rest.match_indices(':').nth(1).map_or(rest.len(), |(i, _)| i);
        profile::allows_channel(&rest[..end])
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
    ui.related.set_visible(!restricted);
    ui.history_button.set_visible(!restricted);
    // The subscriptions would be a way around the restricted mode.
    ui.subs_toggle.set_visible(!restricted);
    if restricted {
        set_sidebar_toggle(&ui.subs_toggle, false);
    }
    ui.settings_button.set_visible(!restricted);
    ui.controls_button.set_tooltip_text(Some(if restricted {
        "Turn off the restricted mode"
    } else {
        "Turn on the restricted mode"
    }));
}

/// Turns the restricted mode on or off, starting over from the home page.
pub fn set_restricted(restricted: bool) {
    // Each mode keeps its own queue and player.
    save_session();
    unload_player();
    let cancel = with(|state| {
        state.queue.clear();
        state.queue_downloading.take()
    });
    if let Some(id) = cancel {
        send(Request::Cancel { id });
    }

    if let Err(error) = profile::set_restricted(restricted) {
        toast(&format!("Could not save the restricted mode: {error}"));
    }
    profile::forget_listed();

    with(|state| {
        state.history = vec![Page::Home];
        state.index = 0;
    });
    apply_mode();
    restore_session();
    show();
    if !restricted {
        refresh_feed();
    }
}

fn update_allow_buttons() {
    let ui = ui::get();
    let label = |allowed: bool| {
        if allowed {
            "Remove from restricted mode"
        } else {
            "Add to restricted mode"
        }
    };

    let (channel, video) = match current() {
        Page::Channel { id, .. } => (Some(label(profile::allows_channel(&id))), None),
        Page::Playlist { id, .. } => (Some(label(profile::allows_playlist(&id))), None),
        Page::Video(entry) => (None, Some(label(profile::lists_video(&entry.id)))),
        _ => (None, None),
    };
    let collection = match current() {
        Page::Collection(name) => Some(label(profile::allows_collection(&name))),
        _ => None,
    };

    // Only the one in charge of the profile gets to change it.
    let editable = !profile::restricted();
    ui.allow_channel.set_visible(editable && channel.is_some());
    ui.allow_channel.set_label(channel.unwrap_or_default());
    ui.allow_video.set_visible(editable && video.is_some());
    ui.allow_video.set_label(video.unwrap_or_default());
    library::set_allow_button(
        editable && collection.is_some(),
        collection.unwrap_or_default(),
    );

    // Videos opened from files can't be saved, they are already where they are.
    let savable = match current() {
        Page::Video(entry) => editable && !entry.url.is_empty(),
        _ => false,
    };
    library::show_save_button(savable);
}

/// Adds the page on screen to the restricted mode, or takes it out of it.
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
        Page::Collection(name) => toggle(
            profile::allows_collection(&name),
            Item::Collection(name.clone()),
            Box::new(move |p| p.collections.retain(|c| c != &name)),
        ),
        _ => return,
    };

    match result {
        Ok(true) => toast("Added to the restricted mode"),
        Ok(false) => toast("Removed from the restricted mode"),
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

    let key = page.key();
    match page {
        Page::Search {
            query,
            sort,
            filter,
        } => {
            ui.channel_header.set_visible(false);

            ui.syncing.set(true);
            ui.search_entry.set_text(&query);
            ui.sort.set_selected(sort as u8 as u32);
            ui.filter.set_selected(filter as u8 as u32);
            ui.syncing.set(false);

            show_listing(
                key,
                Source::Search {
                    query,
                    sort,
                    filter,
                },
            );
        }
        Page::Home => {
            ui.stack.set_visible_child_name("home");
            controls::show_home();
            if !profile::restricted() {
                show_feed();
            }
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

            // A file has nothing to look up.
            if !entry.url.is_empty() && !with(|state| state.infos.contains_key(&key)) {
                send(Request::Info {
                    key,
                    url: entry.url,
                });
            }
        }
        Page::Related(entry) => {
            ui.channel_header.set_visible(true);
            ui.channel_title
                .set_text(&format!("Suggested after {}", entry.title));
            ui.channel_entry.set_visible(false);

            show_listing(key, Source::Related { id: entry.id });
        }
        Page::Cache => show_cache(),
        Page::History => show_history(),
        Page::Collection(name) => {
            ui.stack.set_visible_child_name("collection");
            library::show_collection(&name);
        }
    }

    update_allow_buttons();
    update_subscribe_button();
}

fn navigate(page: Page) {
    if !page_allowed(&page) {
        toast("This is not available in the restricted mode");
        return;
    }

    let reload = with(|state| {
        let key = page.key();
        if state.history[state.index].key() == key {
            // Repeating a search refreshes it, anything else is already on screen.
            let reload = matches!(page, Page::Search { .. } | Page::Cache | Page::History);
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
    subscriptions::init();
    collections::init();
    state::init();
    restore_sidebars();
    restore_playback();
    restore_session();
    library::refresh();
    apply_mode();
    // Videos that were not saved don't stay for long.
    let expired = cache::expire();
    if expired > 0 {
        eprintln!("Deleted {expired} downloaded videos older than 48 hours");
    }
    // Forgets what's left of videos that are gone.
    cache::scan();
    refresh_subscriptions();
    show();

    ui::get().window.connect_close_request(|_| {
        save_session();
        glib::Propagation::Proceed
    });
    glib::timeout_add_seconds_local(15, || {
        if !ui::get().player.is_paused() {
            save_session();
        }
        glib::ControlFlow::Continue
    });

    refresh_feed();
    glib::timeout_add_seconds_local(FEED_REFRESH_SECONDS, || {
        refresh_feed();
        glib::ControlFlow::Continue
    });
}

pub fn search(query: &str, sort: SearchSort, filter: SearchFilter) {
    let query = query.trim();
    if let Some(id) = crate::fetch::video_id_of_link(query) {
        // The details fill in the rest once they arrive.
        navigate(Page::Video(Entry {
            id: crate::fetch::youtube_id(&id),
            url: format!("https://www.youtube.com/watch?v={id}"),
            title: String::new(),
            channel: String::new(),
            channel_id: String::new(),
            duration: None,
            views: None,
            published: None,
        }));
    } else if !query.is_empty() {
        navigate(Page::Search {
            query: query.to_string(),
            sort,
            filter,
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

pub fn open_related() {
    if let Page::Video(entry) = current() {
        navigate(Page::Related(entry));
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

pub fn open_history() {
    navigate(Page::History);
}

pub fn open_collection(name: &str) {
    navigate(Page::Collection(name.to_string()));
}

/// Leaves the page of a collection that is gone.
pub fn collection_forgotten(name: &str) {
    if matches!(current(), Page::Collection(shown) if shown == name) {
        open_home();
    }
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
    play(Entry {
        id,
        url: String::new(),
        title: name,
        channel: String::new(),
        channel_id: String::new(),
        duration: None,
        views: None,
        published: None,
    });
}

// Downloads

/// The size picked in the player for the video, `None` for the usual one.
fn wanted_size(id: &str) -> Option<u32> {
    with(|state| state.picked.get(id).copied())
}

/// How a size is called: `720p`, or `best`.
fn size_name(height: u32) -> String {
    if height == 0 {
        "best".to_string()
    } else {
        format!("{height}p")
    }
}

fn start_download(entry: Entry) {
    let height = wanted_size(&entry.id);
    let start = with(|state| {
        if state.inflight.contains_key(&entry.id) {
            false
        } else {
            state.inflight.insert(entry.id.clone(), entry.clone());
            true
        }
    });

    if start {
        send(Request::Download { entry, height });
    }
}

/// Checks the size the video has in the player in the settings menu of the player.
fn sync_size_buttons(id: &str) {
    let ui = ui::get();
    let height = wanted_size(id).unwrap_or(settings::get().max_height);

    ui.syncing.set(true);
    for (button, (size, _)) in ui.size_buttons.iter().zip(settings::QUALITIES) {
        if size == height {
            button.set_active(true);
        }
    }
    ui.syncing.set(false);
}

/// Plays the video from another file, from where it was.
fn swap_playback(entry: &Entry, path: &std::path::Path) {
    let ui = ui::get();
    let (position, paused) = (ui.player.position(), ui.player.is_paused());

    ui.player.load(&entry.id, &path.to_string_lossy());
    with(|state| state.pending_seek = Some((entry.id.clone(), position)));
    ui.player.set_paused(paused);
}

/// Picks the size of the video in the player. If it is not on disk it is downloaded, which
/// stops the download going on for the video, and the player moves on to it once it is there.
pub fn set_size(height: u32) {
    let ui = ui::get();
    let Some(entry) = with(|state| state.playing.clone()).filter(|entry| !entry.url.is_empty())
    else {
        return;
    };

    let size = (height != settings::get().max_height).then_some(height);
    if wanted_size(&entry.id) == size {
        return;
    }
    with(|state| match size {
        Some(size) => {
            state.picked.insert(entry.id.clone(), size);
        }
        None => {
            state.picked.remove(&entry.id);
        }
    });

    let loaded = ui.player.id().as_deref() == Some(&entry.id);
    if let Some(path) = cache::find_size(&entry.id, size) {
        let stop = with(|state| state.switching.take_if(|id| *id == entry.id).is_some());
        if stop {
            send(Request::Cancel {
                id: entry.id.clone(),
            });
        }
        ui.size_status.set_visible(false);
        if loaded {
            swap_playback(&entry, &path);
        } else {
            start_playback(&entry, &path);
        }
        return;
    }

    with(|state| {
        state.inflight.insert(entry.id.clone(), entry.clone());
        state.progress.remove(&entry.id);
        if loaded {
            state.switching = Some(entry.id.clone());
        } else {
            state.awaiting_play = Some(entry.id.clone());
        }
    });
    if loaded {
        ui.size_status
            .set_text(&format!("Downloading {}…", size_name(height)));
        ui.size_status.set_visible(true);
    } else {
        ui.player_status.set_text("Downloading…");
        ui.player_status.set_visible(true);
    }
    send(Request::Download {
        entry,
        height: size,
    });
}

/// Shows or hides the progress bar of the player.
pub fn set_progress_bar(visible: bool) {
    if let Err(error) = settings::update(|s| s.hide_progress_bar = !visible) {
        toast(&format!("Could not save the settings: {error}"));
    }
    apply_progress_bar(ui::get().window.is_fullscreen());
}

/// Shows the progress bar with the time, or the skip buttons when it is hidden. Fullscreen has
/// more of them.
pub fn apply_progress_bar(fullscreen: bool) {
    let ui = ui::get();
    let hidden = settings::get().hide_progress_bar;
    ui.seek_row.set_visible(!hidden);
    for (button, fullscreen_only) in &ui.skip_buttons {
        button.set_visible(hidden && (fullscreen || !fullscreen_only));
    }
}

/// Jumps forward or back in the video.
pub fn skip(seconds: f64) {
    let ui = ui::get();
    let duration = ui.player.duration();
    if duration > 0.0 {
        with(|state| state.chapter_stop = None);
        let target = (ui.player.position() + seconds).clamp(0.0, duration);
        ui.player.seek_to(target);
        player_message(&format!(
            "{} / {}",
            format::duration(target),
            format::duration(duration)
        ));
    }
}

// Saving

/// Saves the video on screen to the collection, or to the one picked last.
pub fn save_current(collection: Option<String>) {
    let Page::Video(entry) = current() else {
        return;
    };
    match collection {
        Some(collection) => save_video(entry, collection),
        None => toast("There is no collection to save to"),
    }
}

/// Moves the video out of the cache into the collection. A video that isn't downloaded yet is
/// saved as soon as it is.
fn save_video(entry: Entry, collection: String) {
    if profile::restricted() || entry.url.is_empty() {
        return;
    }
    if collections::contains(&collection, &entry.id) {
        toast(&format!("Already saved to “{collection}”"));
        return;
    }

    let source = cache::find(&entry.id)
        .map(|path| (path, false))
        .or_else(|| {
            // Saved in another collection, which keeps its copy.
            collections::find(&entry.id).map(|path| (path, true))
        });
    match source {
        Some((path, copy)) => {
            if finish_save(&entry, &collection, &path, copy).is_some() {
                refresh_after_save(&entry);
            }
        }
        None => {
            with(|state| {
                state
                    .pending_saves
                    .insert(entry.id.clone(), collection.clone())
            });
            toast(&format!("Downloading, then saving to “{collection}”"));
            start_download(entry);
        }
    }
}

/// Where the video is after being saved, if that worked.
fn finish_save(
    entry: &Entry,
    collection: &str,
    source: &std::path::Path,
    copy: bool,
) -> Option<PathBuf> {
    match collections::save(collection, entry, source, copy) {
        Ok(path) => {
            if !copy {
                // The file is gone from the cache, this forgets what the index knew about it.
                cache::delete(&[cache::stem(&entry.id, None)]);
            }
            toast(&format!("Saved to “{collection}”"));
            Some(path)
        }
        Err(error) => {
            toast(&format!("Could not save the video: {error}"));
            None
        }
    }
}

fn refresh_after_save(entry: &Entry) {
    library::saved();
    match current() {
        Page::Cache => show_cache(),
        Page::Video(shown) if shown.id == entry.id => show_video(&shown),
        _ => {}
    }
}

// Player

pub fn play_current() {
    if let Page::Video(entry) = current() {
        if video_allowed(&entry) {
            enqueue(entry.clone());
        }
        play(entry);
    }
}

/// Plays a video in the player of the queue sidebar, wherever the user is.
pub fn play(entry: Entry) {
    if !video_allowed(&entry) {
        toast("This is not available in the restricted mode");
        return;
    }

    let ui = ui::get();
    set_sidebar_toggle(&ui.queue_toggle, true);
    with(|state| state.playing = Some(entry.clone()));
    show_player(&entry);

    // The chapters come with the details.
    let key = Page::Video(entry.clone()).key();
    if !entry.url.is_empty() && !with(|state| state.infos.contains_key(&key)) {
        send(Request::Info {
            key,
            url: entry.url,
        });
    }
}

fn show_player(entry: &Entry) {
    let ui = ui::get();
    ui.player_frame.set_visible(true);
    sync_size_buttons(&entry.id);
    if with(|state| state.switching.as_deref() != Some(&entry.id)) {
        ui.size_status.set_visible(false);
    }

    if ui.player.id().as_deref() == Some(&entry.id) {
        ui.player_status.set_visible(false);
        show_player_chapters();
        return;
    }

    match local_path(&entry.id) {
        Some(path) => start_playback(entry, &path),
        None => {
            with(|state| state.awaiting_play = Some(entry.id.clone()));
            ui.player_status.set_text("Downloading…");
            ui.player_status.set_visible(true);
            start_download(entry.clone());
        }
    }
}

/// Writes the queue, the video in the player and where the queued videos were left off.
fn save_session() {
    remember_position();
    let (queue, playing, positions) = with(|state| {
        let queue: Vec<Entry> = state.queue.iter().map(|item| item.entry.clone()).collect();
        let positions = state
            .positions
            .iter()
            .filter(|(id, _)| queue.iter().any(|entry| &entry.id == *id))
            .map(|(id, position)| (id.clone(), *position))
            .collect();
        (queue, state.playing.clone(), positions)
    });
    let restricted = profile::restricted();
    let result = state::update(|state| {
        *state.session_mut(restricted) = state::Session {
            queue,
            playing,
            positions,
        };
    });
    if let Err(error) = result {
        eprintln!("Cannot write the state: {error}");
    }
}

/// Brings back the queue and the player as they were left, the video paused.
fn restore_session() {
    let saved = state::get().session_mut(profile::restricted()).clone();
    with(|state| {
        state.queue = saved
            .queue
            .into_iter()
            .filter(|entry| video_allowed(entry))
            .map(|entry| QueueItem {
                entry,
                state: QueueState::Waiting,
            })
            .collect();
        state.positions = saved.positions;
    });
    pump_queue();

    if let Some(entry) = saved.playing.filter(|entry| video_allowed(entry)) {
        with(|state| state.start_paused = Some(entry.id.clone()));
        play(entry);
    }
}

/// Keeps where the video in the player is, to go back to it later.
fn remember_position() {
    let ui = ui::get();
    let Some(id) = ui.player.id() else {
        return;
    };

    let (position, duration) = (ui.player.position(), ui.player.duration());
    with(|state| {
        // A video that played to its end starts over.
        if duration > 0.0 && position < duration - 1.0 {
            state.positions.insert(id, position);
        } else if duration > 0.0 {
            state.positions.remove(&id);
        }
    });
}

fn start_playback(entry: &Entry, path: &std::path::Path) {
    let ui = ui::get();
    remember_position();
    ui.player.load(&entry.id, &path.to_string_lossy());
    let paused = with(|state| state.start_paused.take()).as_deref() == Some(&entry.id);
    if paused {
        ui.player.set_paused(true);
    }
    with(|state| {
        if state.pending_seek.as_ref().map(|(id, _)| id) != Some(&entry.id) {
            state.chapter_stop = None;
            state.pending_seek = state
                .positions
                .get(&entry.id)
                .map(|&position| (entry.id.clone(), position));
        }
    });
    show_player_chapters();
    show_sponsor_marks(Vec::new());
    if settings::get().skip_sponsors && !entry.url.is_empty() {
        if let Some(segments) = with(|state| state.segments.get(&entry.id).cloned()) {
            show_sponsor_marks(segments.clone());
            ui.player.set_skips(segments);
        }
        let lookup = with(|state| state.segment_lookups.insert(entry.id.clone()));
        if lookup {
            send(Request::Segments {
                id: entry.id.clone(),
            });
        }
    }
    // Files from the command line aren't YouTube videos.
    if !entry.url.is_empty() {
        if let Err(error) = history::record(entry) {
            eprintln!("Cannot write the history: {error}");
        }
    }
    ui.player_status.set_visible(false);
    ui.play_button.set_icon_name(if paused {
        "media-playback-start-symbolic"
    } else {
        "media-playback-pause-symbolic"
    });
    refresh_queue();
    save_session();
}

pub fn toggle_pause() {
    let ui = ui::get();
    if ui.player.id().is_some() {
        if !ui.player.is_paused() {
            remember_position();
        }
        ui.player.set_paused(!ui.player.is_paused());
        save_session();
        tick();
    }
}

/// Closes the player entirely, unlike pausing.
pub fn stop_player() {
    unload_player();
    save_session();
}

fn unload_player() {
    let ui = ui::get();
    if ui.window.is_fullscreen() {
        toggle_fullscreen();
    }
    ui.player.stop();
    with(|state| {
        state.positions.clear();
        state.playing = None;
        state.awaiting_play = None;
        state.chapter_stop = None;
    });
    ui.player_status.set_visible(false);
    ui.player_frame.set_visible(false);
    ui.play_button
        .set_icon_name("media-playback-pause-symbolic");
    refresh_queue();
}

pub fn set_speed(speed: f64) {
    ui::get().player.set_speed(speed);
    remember_playback(|state| state.speed = Some(speed));
}

/// `volume` goes from 0 to 1.
pub fn set_volume(volume: f64) {
    ui::get().player.set_volume(volume * 100.0);
    remember_playback(|state| state.volume = Some(volume));
}

/// Writes the volume or the speed to the state, unless nothing changed.
fn remember_playback(change: impl Fn(&mut state::State)) {
    let mut before = state::get();
    change(&mut before);
    let now = state::get();
    if (before.volume, before.speed) == (now.volume, now.speed) {
        return;
    }
    if let Err(error) = state::update(change) {
        eprintln!("Cannot write the state: {error}");
    }
}

/// Puts the volume and the speed back as they were left.
fn restore_playback() {
    let ui = ui::get();
    let saved = state::get();
    if let Some(volume) = saved.volume {
        ui.volume_scale.set_value(volume.clamp(0.0, 1.0));
    }
    let index = saved.speed.and_then(|speed| ui::speed_index(speed));
    if let Some(index) = index {
        ui.speed.set_selected(index);
    }
}

pub fn volume_icon(volume: f64, muted: bool) -> &'static str {
    match volume {
        _ if muted || volume <= 0.0 => "audio-volume-muted-symbolic",
        v if v < 0.34 => "audio-volume-low-symbolic",
        v if v < 0.67 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

pub fn toggle_mute() {
    let ui = ui::get();
    let muted = !ui.player.is_muted();
    ui.player.set_muted(muted);
    ui.volume
        .set_icon_name(volume_icon(ui.volume_scale.value(), muted));
    player_message(if muted { "Muted" } else { "Unmuted" });
}

/// Moves the volume by `delta`, on the scale of 0 to 1.
pub fn change_volume(delta: f64) {
    let ui = ui::get();
    if ui.player.is_muted() {
        toggle_mute();
    }
    ui.volume_scale
        .set_value((ui.volume_scale.value() + delta).clamp(0.0, 1.0));
    player_message(&format!("Volume: {}%", (ui.volume_scale.value() * 100.0).round()));
}

/// Picks the next or the previous of the playback speeds on offer.
pub fn step_speed(direction: i32) {
    let ui = ui::get();
    let last = ui.speed.model().map_or(0, |model| model.n_items()).saturating_sub(1);
    let selected = (ui.speed.selected() as i64 + direction as i64).clamp(0, last as i64);
    ui.speed.set_selected(selected as u32);
    show_speed();
}

fn show_speed() {
    if let Some(speed) = ui::speed_at(ui::get().speed.selected()) {
        player_message(&format!("Speed: {speed}×"));
    }
}

pub fn reset_speed() {
    ui::get().speed.set_selected(ui::NORMAL_SPEED);
    show_speed();
}

pub fn step_frame(forward: bool) {
    let ui = ui::get();
    ui.player.step_frame(forward);
    player_message(if forward { "Next frame" } else { "Previous frame" });
}

/// Jumps to the start of the next chapter, or of the current one if it was left a while ago.
pub fn step_chapter(direction: i32) {
    let ui = ui::get();
    let Some(id) = ui.player.id() else {
        return;
    };
    let chapters = chapters_of(&id);
    let position = ui.player.position();
    let current = chapters.iter().rposition(|chapter| chapter.start <= position);
    let target = match (current, direction) {
        (None, d) if d > 0 => Some(0),
        (None, _) => None,
        (Some(i), d) if d > 0 => Some(i + 1),
        (Some(i), _) if position - chapters[i].start > 3.0 => Some(i),
        (Some(i), _) => i.checked_sub(1),
    };
    match target {
        Some(i) if i < chapters.len() => {
            ui.player.seek_to(chapters[i].start);
            player_message(&chapters[i].title);
        }
        _ if direction < 0 => ui.player.seek_to(0.0),
        _ => {}
    }
}

/// Plays the next or the previous video of the queue that is ready.
pub fn queue_step(direction: i32) {
    let ui = ui::get();
    let Some(id) = ui.player.id() else {
        return;
    };
    let target = with(|state| {
        let position = state.queue.iter().position(|item| item.entry.id == id)?;
        let ready = |item: &&QueueItem| {
            matches!(item.state, QueueState::Ready) && video_allowed(&item.entry)
        };
        if direction > 0 {
            state.queue[position + 1..].iter().find(ready)
        } else {
            state.queue[..position].iter().rev().find(ready)
        }
        .map(|item| item.entry.clone())
    });
    match target {
        Some(entry) => play(entry),
        None => player_message(if direction > 0 {
            "No next video in the queue"
        } else {
            "No previous video in the queue"
        }),
    }
}

pub fn seek(fraction: f64) {
    let ui = ui::get();
    let duration = ui.player.duration();
    if duration > 0.0 {
        with(|state| state.chapter_stop = None);
        ui.player.seek_to(fraction * duration);
    }
}

pub fn toggle_fullscreen() {
    let ui = ui::get();
    let fullscreen = !ui.window.is_fullscreen();
    if fullscreen && ui.player.id().is_none() {
        return;
    }

    // The window only reports being fullscreen once the compositor agrees, don't wait for that.
    ui.window.set_fullscreened(fullscreen);
    show_fullscreen(fullscreen);
}

/// Fullscreen makes the sidebar take the whole window, with only the player in it.
fn show_fullscreen(fullscreen: bool) {
    let ui = ui::get();
    let split = &ui.queue_split;
    apply_progress_bar(fullscreen);
    let saved = with(|state| state.sidebar_widths);
    if fullscreen == saved.is_some() {
        return;
    }

    if fullscreen {
        with(|state| {
            state.sidebar_widths = Some((split.max_sidebar_width(), split.sidebar_width_fraction()))
        });
        set_sidebar_toggle(&ui.subs_toggle, false);
        set_sidebar_toggle(&ui.queue_toggle, true);
        split.set_max_sidebar_width(100000.0);
        split.set_sidebar_width_fraction(1.0);
    } else if let Some((max, fraction)) = saved {
        with(|state| state.sidebar_widths = None);
        split.set_max_sidebar_width(max);
        split.set_sidebar_width_fraction(fraction);
    }

    // The player takes the shape of the screen rather than a video's.
    let screen = ui
        .window
        .surface()
        .and_then(|surface| surface.display().monitor_at_surface(&surface))
        .map(|monitor| monitor.geometry())
        .filter(|geometry| geometry.height() > 0);
    ui.player_frame.set_ratio(match screen {
        Some(geometry) if fullscreen => geometry.width() as f32 / geometry.height() as f32,
        _ => 16.0 / 9.0,
    });

    // Sit under the queue normally, take the whole sidebar once the queue is hidden.
    ui.player_frame.set_vexpand(fullscreen);
    ui.player_frame.set_valign(if fullscreen { gtk::Align::Fill } else { gtk::Align::End });

    if let Some(content) = split.content() {
        content.set_visible(!fullscreen);
    }
    ui.queue_header.set_visible(!fullscreen);
    ui.queue_scroll.set_visible(!fullscreen);
}

/// The window left or entered fullscreen on its own, for instance from the compositor.
pub fn fullscreen_changed() {
    show_fullscreen(ui::get().window.is_fullscreen());
}

/// Called a few times a second to keep the player controls up to date.
pub fn tick() {
    let ui = ui::get();
    if ui.player.id().is_none() {
        return;
    }

    let (position, duration) = (ui.player.position(), ui.player.duration());
    if duration > 0.0 {
        let id = ui.player.id();
        let start = with(|state| {
            state
                .pending_seek
                .take_if(|(pending, _)| Some(&*pending) == id.as_ref())
        });
        if let Some((_, start)) = start {
            ui.player.seek_to(start);
        }
    }
    if with(|state| state.chapter_stop.is_some_and(|stop| position >= stop - 0.05)) {
        with(|state| state.chapter_stop = None);
        ui.player.set_paused(true);
    }
    if ui.sponsor_spans.borrow().0 != duration {
        ui.sponsor_spans.borrow_mut().0 = duration;
        ui.seek_marks.queue_draw();
    }
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
        if let Some(id) = ui.player.id() {
            with(|state| state.positions.remove(&id));
        }
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
        play(next);
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
        toast("This video is not available in the restricted mode");
        return;
    }

    if !enqueue(entry) {
        toast("Already in the queue");
    }
}

/// Puts the video at the end of the queue, unless it is there already.
fn enqueue(entry: Entry) -> bool {
    let added = with(|state| {
        if state.queue.iter().any(|item| item.entry.id == entry.id) {
            return false;
        }
        state.queue.push(QueueItem {
            entry,
            state: QueueState::Waiting,
        });
        true
    });

    if added {
        save_session();
        pump_queue();
    }
    added
}

fn queue_remove(id: &str) {
    if ui::get().player.id().as_deref() == Some(id) {
        return;
    }
    let cancel = with(|state| {
        state.queue.retain(|item| item.entry.id != id);
        let downloading = state.queue_downloading.as_deref() == Some(id);
        if downloading {
            state.queue_downloading = None;
        }
        // The player may need that download too.
        downloading && state.awaiting_play.as_deref() != Some(id)
    });

    save_session();
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
        if local_path(&entry.id).is_some() {
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

        let play = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Play")
            .valign(gtk::Align::Center)
            .sensitive(ready && !is_playing && video_allowed(&entry))
            .css_classes(["flat", "circular"])
            .build();
        play.connect_clicked({
            let entry = entry.clone();
            move |_| self::play(entry.clone())
        });
        row.add_prefix(&play);

        if is_playing {
            row.add_css_class("queue-playing");
        }

        let remove = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove from the queue")
            .valign(gtk::Align::Center)
            .sensitive(!is_playing)
            .css_classes(["flat", "circular"])
            .build();
        remove.connect_clicked({
            let id = entry.id.clone();
            move |_| queue_remove(&id)
        });
        row.add_suffix(&remove);

        // Dragging a row onto another one moves it to that place.
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare({
            let id = entry.id.clone();
            move |_, _, _| Some(gdk::ContentProvider::for_value(&id.to_value()))
        });
        source.connect_drag_begin({
            let row = row.clone();
            move |source, _| {
                let paintable = gtk::WidgetPaintable::new(Some(&row));
                source.set_icon(Some(&paintable), 0, 0);
            }
        });
        row.add_controller(source);

        let target = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
        target.connect_drop({
            let id = entry.id.clone();
            move |_, value, _, _| match value.get::<String>() {
                Ok(dragged) => {
                    // The list is rebuilt, not while the drag is still ending.
                    let id = id.clone();
                    glib::idle_add_local_once(move || queue_move(&dragged, &id));
                    true
                }
                Err(_) => false,
            }
        });
        row.add_controller(target);

        ui.queue_list.append(&row);
    }
}

/// Moves a video of the queue to the place of another one.
fn queue_move(id: &str, onto: &str) {
    let moved = with(|state| {
        let from = state.queue.iter().position(|item| item.entry.id == id)?;
        let to = state.queue.iter().position(|item| item.entry.id == onto)?;
        if from == to {
            return None;
        }
        let item = state.queue.remove(from);
        state.queue.insert(to, item);
        Some(())
    });

    if moved.is_some() {
        save_session();
        refresh_queue();
    }
}

// Sidebars

/// Opens or closes a sidebar from code, which is not the user's choice to remember.
fn set_sidebar_toggle(toggle: &gtk::ToggleButton, open: bool) {
    let ui = ui::get();
    ui.syncing.set(true);
    toggle.set_active(open);
    ui.syncing.set(false);
}

/// Opens the sidebars that were open last time, and remembers when the user changes them.
fn restore_sidebars() {
    let ui = ui::get();
    let saved = state::get();
    set_sidebar_toggle(&ui.queue_toggle, saved.queue_sidebar_open);
    set_sidebar_toggle(&ui.subs_toggle, saved.subscriptions_sidebar_open);

    let remember = |toggle: &gtk::ToggleButton, change: fn(&mut state::State, bool)| {
        toggle.connect_toggled(move |toggle| {
            if !ui::get().syncing.get() {
                if let Err(error) = state::update(|state| change(state, toggle.is_active())) {
                    eprintln!("Cannot write the state: {error}");
                }
            }
        });
    };
    remember(&ui.queue_toggle, |state, open| {
        state.queue_sidebar_open = open
    });
    remember(&ui.subs_toggle, |state, open| {
        state.subscriptions_sidebar_open = open
    });
}

// Subscriptions

fn unsubscribe(id: &str) {
    if let Err(error) = subscriptions::unsubscribe(id) {
        toast(&format!("Could not save the subscriptions: {error}"));
    }
    refresh_subscriptions();
    update_subscribe_button();
    if matches!(current(), Page::Home) {
        show_feed();
    }
}

/// Subscribes to the channel on screen, or unsubscribes from it.
pub fn toggle_subscription() {
    let Page::Channel { id, name, .. } = current() else {
        return;
    };

    if subscriptions::is_subscribed(&id) {
        unsubscribe(&id);
        return;
    }

    if let Err(error) = subscriptions::subscribe(Subscription {
        id: id.clone(),
        name,
    }) {
        toast(&format!("Could not save the subscriptions: {error}"));
    }
    request_latest(&id);
    refresh_subscriptions();
    update_subscribe_button();
}

fn update_subscribe_button() {
    let ui = ui::get();
    let channel = match current() {
        Page::Channel { id, .. } if !profile::restricted() => Some(id),
        _ => None,
    };

    ui.subscribe_button.set_visible(channel.is_some());
    if let Some(id) = channel {
        let subscribed = subscriptions::is_subscribed(&id);
        ui.subscribe_button.set_label(if subscribed {
            "Unsubscribe"
        } else {
            "Subscribe"
        });
        if subscribed {
            ui.subscribe_button.remove_css_class("suggested-action");
        } else {
            ui.subscribe_button.add_css_class("suggested-action");
        }
    }
}

/// Fills the sidebar.
fn refresh_subscriptions() {
    let ui = ui::get();
    while let Some(row) = ui.subscriptions_list.first_child() {
        ui.subscriptions_list.remove(&row);
    }

    let list = subscriptions::list();
    if list.is_empty() {
        ui.subscriptions_list.append(
            &gtk::Label::builder()
                .label("No subscriptions yet.")
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .margin_top(24)
                .margin_start(12)
                .margin_end(12)
                .build(),
        );
    }

    for subscription in list {
        let row = adw::ActionRow::builder()
            .title(&subscription.name)
            .title_lines(1)
            .use_markup(false)
            .activatable(true)
            .cursor(&crate::ui::pointer())
            .build();
        row.connect_activated({
            let subscription = subscription.clone();
            move |_| open_channel(&subscription.id, &subscription.name)
        });

        let remove = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("window-close-symbolic")
            .tooltip_text("Unsubscribe")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        remove.connect_clicked({
            let id = subscription.id.clone();
            move |_| unsubscribe(&id)
        });
        row.add_suffix(&remove);

        ui.subscriptions_list.append(&row);
    }
}

fn request_latest(id: &str) {
    if subscriptions::mark_pending(id) {
        send(Request::Latest {
            channel_id: id.to_string(),
        });
    }
    update_feed_status();
}

/// Asks for the latest videos of every subscription.
pub fn refresh_feed() {
    if profile::restricted() {
        return;
    }
    for subscription in subscriptions::list() {
        request_latest(&subscription.id);
    }
}

/// The home page: the latest videos of the subscriptions, or the invitation to search if there
/// are none.
fn show_feed() {
    let ui = ui::get();
    if subscriptions::list().is_empty() {
        ui.controls.home.set_visible_child_name("open");
        return;
    }
    ui.controls.home.set_visible_child_name("feed");

    let feed = subscriptions::feed();
    let ids: Vec<String> = feed.iter().map(|entry| entry.id.clone()).collect();
    if with(|state| std::mem::replace(&mut state.feed_ids, ids.clone())) != ids {
        ui.feed_store.remove_all();
        append_to(&ui.feed_store, &feed);
    }
    update_feed_status();
}

fn update_feed_status() {
    let ui = ui::get();
    let pending = subscriptions::pending_count();
    let (failed, error) = subscriptions::failures();

    let text = if pending > 0 {
        format!("Checking {pending} channels…")
    } else if let (true, Some(error)) = (failed > 0, error) {
        format!("{failed} channels could not be checked: {error}")
    } else {
        String::new()
    };
    ui.feed_status.set_text(&text);
    ui.feed_status.set_tooltip_text(Some(&text));
}

/// Videos of the channel listing on screen go to the feed too.
fn channel_listing_id(key: &str) -> Option<&str> {
    let (id, query) = key.strip_prefix("c:")?.split_once(':')?;
    query.is_empty().then_some(id)
}

// History page

fn show_history() {
    let ui = ui::get();
    ui.stack.set_visible_child_name("history");

    while let Some(list) = ui.history_box.first_child() {
        ui.history_box.remove(&list);
    }
    *ui.history_current.borrow_mut() = None;
    ui.history_empty.set_visible(false);
    ui.history_scroll.vadjustment().set_value(0.0);

    with(|state| {
        state.history_offset = 0;
        state.history_done = false;
        state.history_day = None;
    });
    history_more();
}

/// Called as the page is scrolled (`scrolled`) or resized: reads more of the file if the end of
/// what is shown is near, or if what is shown doesn't even fill the page.
pub fn history_check(scrolled: bool) {
    let ui = ui::get();
    if ui.stack.visible_child_name().as_deref() != Some("history")
        || with(|state| state.history_done)
    {
        return;
    }

    let adjustment = ui.history_scroll.vadjustment();
    let page = adjustment.page_size();
    let bottom = adjustment.value() + page;
    let more = if scrolled {
        bottom >= adjustment.upper() - 200.0
    } else {
        page > 0.0 && adjustment.upper() <= page
    };
    if more {
        history_more();
    }
}

fn history_more() {
    let ui = ui::get();
    let (offset, done) = with(|state| (state.history_offset, state.history_done));
    if done {
        return;
    }

    let (watched, offset, done) = history::read(offset, HISTORY_PAGE);
    with(|state| {
        state.history_offset = offset;
        state.history_done = done;
    });

    for item in watched {
        let Ok(time) = glib::DateTime::from_unix_local(item.watched) else {
            continue;
        };
        let day = time
            .format("%Y-%m-%d")
            .map(|day| day.to_string())
            .unwrap_or_default();

        // Each day gets a heading and a list of its own.
        if with(|state| state.history_day.replace(day.clone())).as_deref() != Some(&day) {
            ui.history_box.append(
                &gtk::Label::builder()
                    .label(day_name(&time))
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .build(),
            );
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            ui.history_box.append(&list);
            *ui.history_current.borrow_mut() = Some(list);
        }
        let Some(list) = ui.history_current.borrow().clone() else {
            continue;
        };

        let entry = item.entry;
        let mut subtitle = Vec::new();
        if !entry.channel.is_empty() {
            subtitle.push(entry.channel.clone());
        }
        if let Some(duration) = entry.duration {
            subtitle.push(format::duration(duration));
        }
        if let Ok(clock) = time.format("%H:%M") {
            subtitle.push(clock.to_string());
        }

        let picture = gtk::Picture::builder()
            .width_request(112)
            .height_request(63)
            .content_fit(gtk::ContentFit::Cover)
            .valign(gtk::Align::Center)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["card"])
            .build();
        picture.set_widget_name(&entry.id);
        picture.set_paintable(thumbnail(&entry.id, false, Some(&picture)).as_ref());

        let row = adw::ActionRow::builder()
            .title(&entry.title)
            .subtitle(subtitle.join("  ·  "))
            .title_lines(2)
            .use_markup(false)
            .activatable(true)
            .cursor(&crate::ui::pointer())
            .build();
        row.add_prefix(&picture);
        row.connect_activated(move |_| open_video(entry.clone()));
        list.append(&row);
    }

    if with(|state| state.history_day.is_none()) {
        ui.history_empty.set_visible(true);
    }
}

/// "Today", "Yesterday" or the date.
fn day_name(time: &glib::DateTime) -> String {
    let same_day = |other: &glib::DateTime| {
        (time.year(), time.day_of_year()) == (other.year(), other.day_of_year())
    };

    if let Ok(now) = glib::DateTime::now_local() {
        if same_day(&now) {
            return "Today".to_string();
        }
        if now.add_days(-1).is_ok_and(|yesterday| same_day(&yesterday)) {
            return "Yesterday".to_string();
        }
    }
    time.format("%A, %-d %B %Y")
        .map(|name| name.to_string())
        .unwrap_or_default()
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
        let stem = video.stem;
        let variant = video.height.is_some();

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
        if let Some(height) = video.height {
            subtitle.push(size_name(height));
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
            .cursor(&crate::ui::pointer())
            .build();

        let check = gtk::CheckButton::new();

        check.set_cursor(Some(&crate::ui::pointer()));
        check.set_valign(gtk::Align::Center);
        check.connect_toggled(|_| {
            if !ui::get().syncing.get() {
                update_cache_selection();
            }
        });
        row.add_prefix(&check);

        let queue = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("list-add-symbolic")
            .tooltip_text("Add to queue")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        queue.connect_clicked({
            let entry = entry.clone();
            move |_| queue_add(entry.clone())
        });
        // Only the usual size can be queued or saved, those go by the id of the video.
        queue.set_visible(!variant);
        row.add_suffix(&queue);

        let save = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("document-save-symbolic")
            .tooltip_text("Save to a collection")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        save.connect_clicked({
            let entry = entry.clone();
            move |_| match library::default_target() {
                Some(collection) => save_video(entry.clone(), collection),
                None => toast("There is no collection to save to"),
            }
        });
        save.set_visible(!variant);
        row.add_suffix(&save);

        let delete = gtk::Button::builder().cursor(&crate::ui::pointer())
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete from the cache")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        delete.connect_clicked({
            let (entry, stem) = (entry.clone(), stem.clone());
            move |_| confirm_delete(vec![(entry.clone(), stem.clone())])
        });
        row.add_suffix(&delete);

        ui.cache_list.append(&row);
        rows.push(CacheRow { entry, stem, check });
    }

    let count = rows.len();
    *ui.cache_rows.borrow_mut() = rows;
    ui.cache_summary
        .set_text(&format!("{count} videos · {}", format::bytes(total)));
    update_cache_selection();
}

fn selected_cache_rows() -> Vec<(Entry, String)> {
    ui::get()
        .cache_rows
        .borrow()
        .iter()
        .filter(|row| row.check.is_active())
        .map(|row| (row.entry.clone(), row.stem.clone()))
        .collect()
}

/// The selected videos of the usual size, which are the ones that can be queued or saved.
fn selected_cache_entries() -> Vec<Entry> {
    selected_cache_rows()
        .into_iter()
        .filter(|(entry, stem)| *stem == cache::stem(&entry.id, None))
        .map(|(entry, _)| entry)
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
    ui.cache_save.set_sensitive(selected > 0);
    ui.cache_delete.set_sensitive(selected > 0);
    ui.cache_save.set_label(&if selected > 0 {
        format!("Save {selected}")
    } else {
        "Save".to_string()
    });
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

pub fn cache_save_selected() {
    let Some(collection) = library::default_target() else {
        toast("There is no collection to save to");
        return;
    };
    for entry in selected_cache_entries() {
        save_video(entry, collection.clone());
    }
}

pub fn cache_delete_selected() {
    confirm_delete(selected_cache_rows());
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

fn confirm_delete(entries: Vec<(Entry, String)>) {
    if entries.is_empty() {
        return;
    }

    let (heading, body) = match entries.as_slice() {
        [(entry, _)] => (
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

fn delete_from_cache(entries: &[(Entry, String)]) {
    let stems: Vec<String> = entries.iter().map(|(_, stem)| stem.clone()).collect();
    // Only the usual size of a video is what the queue plays.
    let ids: Vec<String> = entries
        .iter()
        .filter(|(entry, stem)| *stem == cache::stem(&entry.id, None))
        .map(|(entry, _)| entry.id.clone())
        .collect();
    cache::delete(&stems);

    // Downloaded videos of the queue would just be downloaded again otherwise.
    with(|state| {
        state.queue.retain(|item| {
            !(matches!(item.state, QueueState::Ready) && ids.contains(&item.entry.id))
        })
    });
    refresh_queue();

    toast(&match stems.len() {
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
            let newest = with(|state| {
                let listing = state
                    .listings
                    .entry(key.clone())
                    .or_insert_with(|| Listing {
                        entries: Vec::new(),
                        channels: Vec::new(),
                        done: false,
                    });
                listing.entries.extend(entries.iter().cloned());
                listing.entries.clone()
            });

            // A subscribed channel's page tells what's new there as well as a refresh would.
            if let Some(id) = channel_listing_id(&key).filter(|id| subscriptions::is_subscribed(id))
            {
                subscriptions::set_latest(id, newest);
            }

            note_listed(&key, &entries);

            if current().key() == key {
                append_entries(&ui, &entries);
                update_results_status(&key);
            }
        }
        Response::Channels { key, channels } => {
            let all = with(|state| {
                state.listings.get_mut(&key).map(|listing| {
                    listing.channels.extend(channels);
                    listing.channels.clone()
                })
            });
            if let Some(all) = all.filter(|_| current().key() == key) {
                show_channel_results(&all);
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
                    Some(error) => {
                        let message = glib::markup_escape_text(&format!("Failed to load: {error}"));
                        let link = if error.starts_with(crate::fetch::COOKIES_ERROR) {
                            " <a href=\"cathode:settings\">Open the settings</a>"
                        } else {
                            ""
                        };
                        ui.results_status.set_markup(&format!("{message}{link}"));
                    }
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

                    let playing = ui.player.id().is_some_and(|id| format!("v:{id}") == key);
                    let entry = info.entry.clone();
                    with(|state| state.infos.insert(key, info));
                    if on_page {
                        if let Page::Video(shown) = current() {
                            // A pasted link only knew the id, the details know the rest.
                            let shown = if shown.title.is_empty() {
                                with(|state| {
                                    let index = state.index;
                                    state.history[index] = Page::Video(entry.clone());
                                });
                                entry
                            } else {
                                shown
                            };
                            show_video(&shown);
                        }
                    }
                    if playing {
                        show_player_chapters();
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
        Response::Latest { channel_id, result } => {
            match result {
                Ok(entries) => subscriptions::set_latest(&channel_id, entries),
                Err(error) => subscriptions::set_failed(&channel_id, error),
            }
            if matches!(current(), Page::Home) && !profile::restricted() {
                show_feed();
            }
        }
        Response::Resolved { result } => controls::resolved(result),
        Response::CookiesChecked { browser, result } => {
            crate::preferences::cookies_checked(&browser, result)
        }
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
        Response::Segments { id, segments } => {
            let ui = ui::get();
            if settings::get().skip_sponsors && ui.player.id().as_deref() == Some(&id) {
                show_sponsor_marks(segments.clone());
                ui.player.set_skips(segments.clone());
            }
            with(|state| state.segments.insert(id, segments));
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
            if with(|state| state.switching.as_deref() == Some(&id)) {
                let size = wanted_size(&id).unwrap_or(0);
                ui.size_status.set_text(&format!(
                    "Downloading {}… {:.0}%",
                    size_name(size),
                    fraction * 100.0
                ));
            }
            if with(|state| state.awaiting_play.as_deref() == Some(&id)) {
                ui.player_status
                    .set_text(&format!("Downloading… {:.0}%", fraction * 100.0));
            }
        }
        Response::Downloaded { id, height, result } => {
            let entry = with(|state| {
                state.progress.remove(&id);
                state.inflight.remove(&id)
            });

            if let (Ok(_), Some(entry)) = (&result, &entry) {
                cache::touch(&entry.id, height);
                cache::record(entry);
            }

            // The player moves on to the size it was waiting for.
            let switched = with(|state| {
                (height == state.picked.get(&id).copied())
                    .then(|| state.switching.take_if(|switching| *switching == id))
                    .flatten()
                    .is_some()
            });
            if switched {
                ui.size_status.set_visible(false);
                let playing = with(|state| state.playing.clone()).filter(|entry| entry.id == id);
                match (&result, playing) {
                    (Ok(path), Some(entry)) => {
                        swap_playback(&entry, path);
                        toast(&format!("Playing at {}", size_name(height.unwrap_or(settings::get().max_height))));
                    }
                    _ => {
                        with(|state| state.picked.remove(&id));
                        sync_size_buttons(&id);
                    }
                }
            }

            // Saving moves the file, so whoever else is waiting for it has to be told where it is.
            let wanted = with(|state| state.pending_saves.remove(&id));
            let result = match (&result, &entry, wanted) {
                (Ok(path), Some(entry), Some(collection)) => {
                    match finish_save(entry, &collection, path, false) {
                        Some(saved) => {
                            library::saved();
                            Ok(saved)
                        }
                        None => result,
                    }
                }
                _ => result,
            };

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
                let entry = with(|state| state.playing.clone()).filter(|entry| entry.id == id);
                match (&result, entry) {
                    (Ok(path), Some(entry)) => start_playback(&entry, path),
                    (Err(error), Some(_)) => {
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

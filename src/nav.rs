//! Pages, history and the caches behind them. Everything here runs on the UI thread.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{Model, ModelRc, VecModel};
use tokio::sync::mpsc::UnboundedSender;

use crate::fetch::{Entry, Request, Response, Source, VideoInfo};
use crate::{App, SearchResult, VideoDetails};

#[derive(Clone)]
enum Page {
    Search { query: String, by_date: bool },
    Channel { id: String, name: String, query: String },
    Video(Entry),
}

impl Page {
    /// Identifies the page's content, and is what responses are matched against.
    fn key(&self) -> String {
        match self {
            Page::Search { query, by_date } => format!("s:{by_date}:{query}"),
            Page::Channel { id, query, .. } => format!("c:{id}:{query}"),
            Page::Video(entry) => format!("v:{}", entry.id),
        }
    }
}

struct State {
    requests: Option<UnboundedSender<Request>>,
    history: Vec<Page>,
    index: usize,
    /// Listings and video details already fetched, so going back doesn't wait on yt-dlp again.
    listings: HashMap<String, Vec<Entry>>,
    infos: HashMap<String, VideoInfo>,
    thumbnails: HashMap<String, slint::Image>,
    /// Id of the video whose download should start playback once it finishes.
    downloading: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        requests: None,
        history: vec![Page::Search { query: String::new(), by_date: false }],
        index: 0,
        listings: HashMap::new(),
        infos: HashMap::new(),
        thumbnails: HashMap::new(),
        downloading: None,
    });
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

fn send(request: Request) {
    with(|state| {
        if let Some(requests) = &state.requests {
            let _ = requests.send(request);
        }
    });
}

fn current() -> Page {
    with(|state| state.history[state.index].clone())
}

fn format_duration(secs: Option<f64>) -> String {
    let Some(secs) = secs else {
        return "live".into();
    };

    let secs = secs as u64;
    let (hours, minutes, seconds) = (secs / 3600, (secs / 60) % 60, secs % 60);

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn format_count(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=999_999 => format!("{:.1}K", count as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", count as f64 / 1e6),
        _ => format!("{:.1}B", count as f64 / 1e9),
    }
}

/// yt-dlp reports dates as YYYYMMDD.
fn format_date(date: &str) -> String {
    if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])
    } else {
        date.to_string()
    }
}

fn row(entry: &Entry, thumbnails: &HashMap<String, slint::Image>) -> SearchResult {
    let mut details = vec![format_duration(entry.duration)];
    if let Some(views) = entry.views {
        details.push(format!("{} views", format_count(views)));
    }

    SearchResult {
        id: entry.id.clone().into(),
        title: entry.title.clone().into(),
        channel: entry.channel.clone().into(),
        channel_id: entry.channel_id.clone().into(),
        details: details.join("  ·  ").into(),
        thumbnail: thumbnails.get(&entry.id).cloned().unwrap_or_default(),
    }
}

fn video_details(
    entry: &Entry,
    info: Option<&VideoInfo>,
    thumbnails: &HashMap<String, slint::Image>,
) -> VideoDetails {
    let mut meta = Vec::new();
    if let Some(date) = info.and_then(|info| info.upload_date.as_deref()) {
        meta.push(format_date(date));
    }
    if let Some(views) = info.and_then(|info| info.views).or(entry.views) {
        meta.push(format!("{} views", format_count(views)));
    }
    if let Some(likes) = info.and_then(|info| info.likes) {
        meta.push(format!("{} likes", format_count(likes)));
    }
    meta.push(format_duration(entry.duration));

    VideoDetails {
        id: entry.id.clone().into(),
        title: entry.title.clone().into(),
        channel: entry.channel.clone().into(),
        channel_id: entry.channel_id.clone().into(),
        meta: meta.join("  ·  ").into(),
        description: match info {
            Some(info) => info.description.clone().into(),
            None => "Loading details…".into(),
        },
        thumbnail: thumbnails.get(&entry.id).cloned().unwrap_or_default(),
    }
}

fn set_results(app: &App, entries: &[Entry]) {
    let rows: Vec<SearchResult> =
        with(|state| entries.iter().map(|e| row(e, &state.thumbnails)).collect());
    app.set_results(ModelRc::new(VecModel::from(rows)));
}

fn apply_listing(app: &App, entries: &[Entry]) {
    set_results(app, entries);
    app.set_status(if entries.is_empty() {
        "No results".into()
    } else {
        "".into()
    });
}

fn load_listing(app: &App, key: String, source: Source) {
    match with(|state| state.listings.get(&key).cloned()) {
        Some(entries) => apply_listing(app, &entries),
        None => {
            set_results(app, &[]);
            app.set_status("Loading…".into());
            send(Request::Listing { key, source });
        }
    }
}

/// Makes the UI show the current history entry.
fn show(app: &App) {
    let (page, can_back, can_forward) = with(|state| {
        (
            state.history[state.index].clone(),
            state.index > 0,
            state.index + 1 < state.history.len(),
        )
    });
    app.set_can_back(can_back);
    app.set_can_forward(can_forward);
    app.set_status("".into());

    let key = page.key();
    match page {
        Page::Search { query, by_date } => {
            app.set_page("search".into());
            app.set_query_text(query.clone().into());
            app.set_sort_index(by_date as i32);

            if query.is_empty() {
                set_results(app, &[]);
            } else {
                load_listing(app, key, Source::Search { query, by_date });
            }
        }
        Page::Channel { id, name, query } => {
            app.set_page("channel".into());
            app.set_channel_name(name.into());
            app.set_channel_query(query.clone().into());

            load_listing(app, key, Source::Channel { id, query });
        }
        Page::Video(entry) => {
            app.set_page("video".into());

            let info = with(|state| state.infos.get(&key).cloned());
            let details = with(|state| video_details(&entry, info.as_ref(), &state.thumbnails));
            app.set_video(details);

            if info.is_none() {
                send(Request::Info {
                    key,
                    url: entry.url,
                });
            }
        }
    }
}

fn navigate(app: &App, page: Page) {
    let reload = with(|state| {
        let key = page.key();
        if state.history[state.index].key() == key {
            // Repeating a search refreshes it, anything else is already on screen.
            let reload = matches!(page, Page::Search { .. });
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
        show(app);
    }
}

pub fn init(app: &App, requests: UnboundedSender<Request>) {
    with(|state| state.requests = Some(requests));
    show(app);
}

pub fn search(app: &App, query: &str, by_date: bool) {
    let query = query.trim();
    if !query.is_empty() {
        navigate(
            app,
            Page::Search {
                query: query.to_string(),
                by_date,
            },
        );
    }
}

pub fn search_channel(app: &App, query: &str) {
    if let Page::Channel { id, name, .. } = current() {
        navigate(
            app,
            Page::Channel {
                id,
                name,
                query: query.trim().to_string(),
            },
        );
    }
}

pub fn open_channel(app: &App, id: &str, name: &str) {
    if !id.is_empty() {
        navigate(
            app,
            Page::Channel {
                id: id.to_string(),
                name: name.to_string(),
                query: String::new(),
            },
        );
    }
}

/// Opens the page of a video from the listing currently on screen.
pub fn open_video(app: &App, id: &str) {
    let key = current().key();
    let entry = with(|state| {
        state
            .listings
            .get(&key)
            .and_then(|entries| entries.iter().find(|entry| entry.id == id))
            .cloned()
    });

    if let Some(entry) = entry {
        navigate(app, Page::Video(entry));
    }
}

pub fn back(app: &App) {
    if with(|state| {
        let can = state.index > 0;
        if can {
            state.index -= 1;
        }
        can
    }) {
        show(app);
    }
}

pub fn forward(app: &App) {
    if with(|state| {
        let can = state.index + 1 < state.history.len();
        if can {
            state.index += 1;
        }
        can
    }) {
        show(app);
    }
}

/// Starts downloading the video of the current page, playback follows once it is done.
pub fn play(app: &App) {
    let Page::Video(entry) = current() else {
        return;
    };

    app.set_busy(true);
    app.set_status(format!("Downloading {}…", entry.title).into());
    with(|state| state.downloading = Some(entry.id.clone()));
    send(Request::Download {
        id: entry.id,
        url: entry.url,
    });
}

pub fn handle(app: &App, response: Response) {
    match response {
        Response::Listing { key, result } => match result {
            Ok(entries) => {
                with(|state| state.listings.insert(key.clone(), entries.clone()));
                if current().key() == key {
                    apply_listing(app, &entries);
                }
            }
            Err(error) => {
                if current().key() == key {
                    app.set_status(format!("Failed to load: {error}").into());
                }
            }
        },
        Response::Info { key, result } => {
            let Page::Video(entry) = current() else {
                return;
            };

            match result {
                Ok(info) => {
                    with(|state| state.infos.insert(key.clone(), info.clone()));
                    if entry.id == key[2..] {
                        let details =
                            with(|state| video_details(&entry, Some(&info), &state.thumbnails));
                        app.set_video(details);
                    }
                }
                Err(error) => {
                    if entry.id == key[2..] {
                        let mut details = app.get_video();
                        details.description = format!("Failed to load details: {error}").into();
                        app.set_video(details);
                    }
                }
            }
        }
        Response::Thumbnail { id, image } => {
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &image.rgba,
                image.width,
                image.height,
            );
            let image = slint::Image::from_rgba8(buffer);
            with(|state| state.thumbnails.insert(id.clone(), image.clone()));

            let results = app.get_results();
            if let Some(row) = results.iter().position(|result| result.id == id) {
                let mut result = results.row_data(row).unwrap();
                result.thumbnail = image.clone();
                results.set_row_data(row, result);
            }

            let mut details = app.get_video();
            if details.id == id {
                details.thumbnail = image;
                app.set_video(details);
            }
        }
        Response::Downloaded { id, result } => {
            app.set_busy(false);
            let waiting = with(|state| state.downloading.take()).as_deref() == Some(&id);
            let on_page = matches!(current(), Page::Video(entry) if entry.id == id);

            match result {
                Ok(path) if waiting && on_page => {
                    *crate::PENDING_FILE.lock().unwrap() =
                        Some(path.to_string_lossy().into_owned());
                    app.set_status("".into());
                    app.set_is_paused(false);
                    app.set_playing(true);
                }
                Ok(_) => app.set_status("".into()),
                Err(error) => app.set_status(format!("Download failed: {error}").into()),
            }
        }
    }
}

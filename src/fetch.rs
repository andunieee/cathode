use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Notify, Semaphore};

use crate::profile::{ChannelRef, Input, Item, PlaylistRef};
use crate::state::CaptionMode;

const SEARCH_MAX_RESULTS: usize = 100;
const CHANNEL_MAX_RESULTS: usize = 1000;
/// How much of a Rumble channel is listed, each of its videos taking a request of its own.
const RUMBLE_MAX_RESULTS: usize = 30;
/// How much of a long listing is fetched first, to have something to show early.
const FIRST_PAGE: usize = 30;

/// How many of a channel's newest videos the subscriptions keep.
pub const LATEST_PER_CHANNEL: usize = 15;
/// Channels whose newest videos are looked up at the same time.
const LATEST_PARALLEL: usize = 3;
/// Details of videos looked up at the same time.
const INFO_PARALLEL: usize = 3;

/// Thumbnails shown in lists are scaled down to keep a thousand of them cheap to hold.
const SMALL_THUMBNAIL: (u32, u32) = (168, 94);

/// What the id of a YouTube video starts with, so it can't clash with the id of a file.
pub const YOUTUBE_PREFIX: &str = "yt:";

/// The id of a YouTube video, as an `Entry` has it.
pub fn youtube_id(id: &str) -> String {
    if id.is_empty() {
        return String::new();
    }
    format!("{YOUTUBE_PREFIX}{}", bare_id(id))
}

/// What the id of a Rumble video or channel starts with.
pub const RUMBLE_PREFIX: &str = "rumble:";

/// The id of a Rumble video or channel, as an `Entry` or a `ChannelRef` has it.
pub fn rumble_id(id: &str) -> String {
    format!(
        "{RUMBLE_PREFIX}{}",
        id.strip_prefix(RUMBLE_PREFIX).unwrap_or(id)
    )
}

fn rumble_url(text: &str) -> Option<reqwest::Url> {
    let text = text.trim();
    if text.contains(char::is_whitespace) {
        return None;
    }
    let url = reqwest::Url::parse(text)
        .or_else(|_| reqwest::Url::parse(&format!("https://{text}")))
        .ok()?;
    let host = url.host_str()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (host == "rumble.com").then_some(url)
}

/// The id and the address of the video a Rumble link points to, like `v5abcde` for
/// `rumble.com/v5abcde-some-title.html`. `None` for anything else.
pub fn rumble_video_of_link(text: &str) -> Option<(String, String)> {
    let url = rumble_url(text)?;
    let page = url.path_segments()?.next()?;
    let slug = page.strip_suffix(".html")?;
    let id = slug.split('-').next()?;
    let valid =
        id.len() > 1 && id.starts_with('v') && id.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| (id.to_string(), format!("https://rumble.com/{page}")))
}

/// The path of the Rumble channel a link points to, `c/Name` or `user/Name`. `None` for
/// anything else.
pub fn rumble_channel_of_link(text: &str) -> Option<String> {
    let url = rumble_url(text)?;
    let mut segments = url.path_segments()?;
    let kind = segments.next()?;
    let name = segments.next().filter(|name| !name.is_empty())?;
    matches!(kind, "c" | "user").then(|| format!("{kind}/{name}"))
}

/// The address of the channel, which is a Rumble one when its id says so.
pub fn channel_url(id: &str) -> String {
    match id.strip_prefix(RUMBLE_PREFIX) {
        Some(path) => format!("https://rumble.com/{path}"),
        None => format!("https://www.youtube.com/channel/{}", bare_id(id)),
    }
}

/// The address listing the videos of the channel.
fn channel_videos_url(id: &str) -> String {
    if id.starts_with(RUMBLE_PREFIX) {
        channel_url(id)
    } else {
        format!("{}/videos", channel_url(id))
    }
}

/// The id of the video a YouTube link points to, `None` for anything else.
pub fn video_id_of_link(text: &str) -> Option<String> {
    let text = text.trim();
    if text.contains(char::is_whitespace) {
        return None;
    }
    let url = reqwest::Url::parse(text)
        .or_else(|_| reqwest::Url::parse(&format!("https://{text}")))
        .ok()?;
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let mut segments = url.path_segments()?;
    let id = match host {
        "youtu.be" => segments.next()?.to_string(),
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => match segments.next()? {
            "watch" => url
                .query_pairs()
                .find(|(key, _)| key == "v")?
                .1
                .into_owned(),
            "shorts" | "embed" | "live" | "v" => segments.next()?.to_string(),
            _ => return None,
        },
        _ => return None,
    };
    let valid = id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then_some(id)
}

/// The id YouTube knows the video by, without the prefix.
pub fn bare_id(id: &str) -> &str {
    id.strip_prefix(YOUTUBE_PREFIX).unwrap_or(id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub channel_id: String,
    pub duration: Option<f64>,
    pub views: Option<u64>,
    /// When it was uploaded, in seconds since the epoch. Listings only know it approximately.
    #[serde(default)]
    pub published: Option<i64>,
}

/// What yt-dlp knows about a video beyond what the listings carry.
#[derive(Debug, Clone)]
pub struct VideoInfo {
    /// The video as a listing would describe it, for videos we only know the id of.
    pub entry: Entry,
    pub description: String,
    pub upload_date: Option<String>,
    pub views: Option<u64>,
    pub likes: Option<u64>,
    /// The language of the video as YouTube has it, like `pt-BR`. Not every video has one.
    pub language: Option<String>,
    pub chapters: Vec<Chapter>,
}

/// A titled stretch of a video, as its uploader marked it.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
    pub end: f64,
}

/// How a search orders its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Relevance,
    Newest,
    Views,
}

impl SearchSort {
    /// In the order the dropdown shows them.
    pub const ALL: [Self; 3] = [Self::Relevance, Self::Newest, Self::Views];
}

/// What a search keeps in its results, besides sorting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFilter {
    None,
    Hour,
    Today,
    Week,
    Month,
    Year,
    Short,
    Medium,
    Long,
}

impl SearchFilter {
    /// In the order the dropdown shows them.
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::Hour,
        Self::Today,
        Self::Week,
        Self::Month,
        Self::Year,
        Self::Short,
        Self::Medium,
        Self::Long,
    ];
}

pub enum Source {
    Search {
        query: String,
        sort: SearchSort,
        filter: SearchFilter,
    },
    /// A channel's uploads, or the results of searching inside that channel when `query` is set.
    Channel {
        id: String,
        query: String,
        /// The language of the channel if it is a subscription that has one, its titles are
        /// asked for in it.
        language: Option<String>,
    },
    Playlist {
        id: String,
    },
    /// What YouTube suggests to watch after the video.
    Related {
        id: String,
    },
}

pub enum Request {
    /// `key` is echoed back in the responses so the caller can tell which page they were for.
    Listing {
        key: String,
        source: Source,
    },
    Info {
        key: String,
        url: String,
    },
    /// Download the video into the cache directory, if it isn't there yet. `height` is a size
    /// other than the usual one, `0` for the best. Asking for a video that is being downloaded
    /// stops that download, and only the new one is reported.
    Download {
        entry: Entry,
        height: Option<u32>,
    },
    /// Gets the captions for the mode of a video that is on disk at `video`, and answers with
    /// `Response::Captions`. Nothing is downloaded if they are there already.
    Captions {
        entry: Entry,
        video: PathBuf,
        mode: CaptionMode,
    },
    /// Stops the download of the video with this id, if one is running.
    Cancel {
        id: String,
    },
    Thumbnail {
        id: String,
        large: bool,
    },
    /// The picture of a channel.
    Avatar {
        id: String,
    },
    /// Looks up what a link typed in by the user points to.
    Resolve {
        input: Input,
        /// Whether to open what is found instead of adding it to the restricted mode.
        open: bool,
    },
    /// The newest videos of a channel, for the subscriptions.
    Latest {
        channel_id: String,
        /// The language of the channel if it is known, otherwise it is looked for.
        language: Option<String>,
    },
    /// What YouTube suggests to watch after a video, for the suggestions of the home page.
    Suggest {
        id: String,
    },
    /// The sponsor segments of a video, from SponsorBlock.
    Segments {
        id: String,
    },
    /// Tries yt-dlp with the cookies of the browser, before the setting is accepted.
    CheckCookies {
        browser: String,
    },
}

impl Request {
    fn summary(&self) -> String {
        match self {
            Request::Listing { key, .. } => format!("listing {key}"),
            Request::Info { key, url } => format!("info {key} ({url})"),
            Request::Download { entry, height } => {
                format!("download {} at height {height:?}", entry.id)
            }
            Request::Captions { entry, mode, .. } => {
                format!("captions of {} for {mode:?}", entry.id)
            }
            Request::Cancel { id } => format!("cancel the download of {id}"),
            Request::Thumbnail { id, large } => format!("thumbnail of {id}, large: {large}"),
            Request::Avatar { id } => format!("avatar of {id}"),
            Request::Resolve { .. } => "resolve a link".to_string(),
            Request::Latest { channel_id, .. } => format!("latest videos of {channel_id}"),
            Request::Suggest { id } => format!("suggestions after {id}"),
            Request::Segments { id } => format!("sponsor segments of {id}"),
            Request::CheckCookies { browser } => format!("check the cookies of {browser}"),
        }
    }
}

impl Response {
    /// A line for the log, none for the responses too frequent to be worth one.
    pub fn summary(&self) -> Option<String> {
        Some(match self {
            Response::Entries { key, entries } => format!("{} entries for {key}", entries.len()),
            Response::Channels { key, channels } => {
                format!("{} channels for {key}", channels.len())
            }
            Response::ListingDone { key, error } => format!("listing {key} done, error: {error:?}"),
            Response::Info { key, result } => match result {
                Ok(_) => format!("info {key} arrived"),
                Err(error) => format!("info {key} failed: {error}"),
            },
            Response::Downloaded { id, height, result } => match result {
                Ok(path) => format!("downloaded {id} at {height:?} to {}", path.display()),
                Err(error) => format!("download of {id} failed: {error}"),
            },
            Response::Captions {
                id,
                mode,
                found,
                failed,
            } => {
                format!(
                    "captions of {id} for {mode:?}: {:?}, failed: {failed}",
                    found.as_ref().map(|f| &f.0)
                )
            }
            _ => return None,
        })
    }
}

pub enum Response {
    /// A batch of a listing's entries; a listing arrives in several of these.
    Entries {
        key: String,
        entries: Vec<Entry>,
    },
    /// The channels a search turned up, they come before its videos.
    Channels {
        key: String,
        channels: Vec<ChannelRef>,
    },
    ListingDone {
        key: String,
        error: Option<String>,
    },
    Info {
        key: String,
        result: Result<VideoInfo, String>,
    },
    Progress {
        id: String,
        fraction: f64,
    },
    Downloaded {
        id: String,
        height: Option<u32>,
        result: Result<PathBuf, String>,
    },
    /// The captions asked for with `Request::Captions`: the track and its file, none if the
    /// video has no such captions.
    Captions {
        id: String,
        mode: CaptionMode,
        found: Option<(String, PathBuf)>,
        /// The video has captions for the mode, but they could not be downloaded.
        failed: bool,
    },
    Thumbnail {
        id: String,
        large: bool,
        image: Thumbnail,
    },
    Avatar {
        id: String,
        image: Thumbnail,
    },
    Latest {
        channel_id: String,
        result: Result<Vec<Entry>, String>,
        /// The language found for a channel that had none.
        language: Option<String>,
    },
    Resolved {
        result: Result<Item, String>,
        open: bool,
    },
    Suggested {
        id: String,
        result: Result<Vec<Entry>, String>,
    },
    /// The `(start, end)` of the parts to skip, in seconds.
    Segments {
        id: String,
        segments: Vec<(f64, f64)>,
    },
    CookiesChecked {
        browser: String,
        result: Result<(), String>,
    },
    /// Progress of the startup check of the external tools.
    Setup(crate::tools::Status),
}

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn cache_dir() -> PathBuf {
    crate::paths::cache()
}

/// Spawns a background thread running yt-dlp requests, returns the sender used to queue them.
/// `on_response` is called from the worker threads.
pub fn spawn(on_response: impl Fn(Response) + Send + Sync + 'static) -> UnboundedSender<Request> {
    let (tx, rx) = unbounded_channel();
    let on_response = Arc::new(on_response);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to start tokio runtime");
        rt.block_on(run(rx, on_response));
    });

    tx
}

async fn run(
    mut rx: UnboundedReceiver<Request>,
    on_response: Arc<impl Fn(Response) + Send + Sync + 'static>,
) {
    let thumbnail_slots = Arc::new(Semaphore::new(8));
    let avatar_slots = Arc::new(Semaphore::new(2));
    let latest_slots = Arc::new(Semaphore::new(LATEST_PARALLEL));
    // A queue of videos asks for the details of all of them at once.
    let info_slots = Arc::new(Semaphore::new(INFO_PARALLEL));
    let downloads: Arc<Mutex<HashMap<String, Arc<Notify>>>> = Default::default();

    // Without the tools there is nothing to run, so the first start waits for them. Later ones
    // only update in the background.
    let report = {
        let on_response = on_response.clone();
        move |status| on_response(Response::Setup(status))
    };
    if crate::tools::missing() {
        crate::tools::update(report).await;
    } else {
        tokio::spawn(crate::tools::update(report));
    }

    while let Some(request) = rx.recv().await {
        crate::log::info!("request: {}", request.summary());
        let on_response = on_response.clone();

        match request {
            Request::Listing { key, source } => {
                tokio::spawn(async move { listing(key, source, on_response.as_ref()).await });
            }
            Request::Info { key, url } => {
                let slots = info_slots.clone();
                tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    on_response(Response::Info {
                        key,
                        result: info(&url).await,
                    });
                });
            }
            Request::Download { entry, height } => {
                let cancel = Arc::new(Notify::new());
                let before = downloads
                    .lock()
                    .unwrap()
                    .insert(entry.id.clone(), cancel.clone());
                if let Some(before) = before {
                    before.notify_one();
                }

                let downloads = downloads.clone();
                tokio::spawn(async move {
                    let result =
                        download(&entry, height, cancel.clone(), on_response.as_ref()).await;
                    let replaced = {
                        let mut downloads = downloads.lock().unwrap();
                        let ours = downloads
                            .get(&entry.id)
                            .is_some_and(|current| Arc::ptr_eq(current, &cancel));
                        if ours {
                            downloads.remove(&entry.id);
                        }
                        !ours
                    };
                    // A newer download of the video took over, it is the one to report.
                    if !replaced {
                        on_response(Response::Downloaded {
                            id: entry.id,
                            height,
                            result,
                        });
                    }
                });
            }
            Request::Captions { entry, video, mode } => {
                tokio::spawn(async move {
                    let (found, failed) = fetch_captions(&entry, &video, mode).await;
                    on_response(Response::Captions {
                        id: entry.id,
                        mode,
                        found,
                        failed,
                    });
                });
            }
            Request::Cancel { id } => {
                if let Some(cancel) = downloads.lock().unwrap().get(&id) {
                    cancel.notify_one();
                }
            }
            Request::Latest {
                channel_id,
                language,
            } => {
                let slots = latest_slots.clone();
                tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    let (result, language) = latest(&channel_id, language).await;
                    on_response(Response::Latest {
                        result,
                        channel_id,
                        language,
                    });
                });
            }
            Request::Suggest { id } => {
                tokio::spawn(async move {
                    let result = match related(&id).await {
                        // A video can have none, which isn't worth asking about again.
                        Err(error) if error == NO_SUGGESTIONS => Ok(Vec::new()),
                        result => result,
                    };
                    on_response(Response::Suggested { id, result });
                });
            }
            Request::Resolve { input, open } => {
                tokio::spawn(async move {
                    on_response(Response::Resolved {
                        open,
                        result: resolve(input).await,
                    });
                });
            }
            Request::CheckCookies { browser } => {
                tokio::spawn(async move {
                    on_response(Response::CookiesChecked {
                        result: check_cookies(&browser).await,
                        browser,
                    });
                });
            }
            Request::Segments { id } => {
                tokio::spawn(async move {
                    // Not worth bothering the user when SponsorBlock is out of reach.
                    let segments = sponsor_segments(&id).await.unwrap_or_default();
                    on_response(Response::Segments { id, segments });
                });
            }
            Request::Avatar { id } => {
                let slots = avatar_slots.clone();
                tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    match avatar(&id).await {
                        Ok(image) => on_response(Response::Avatar { id, image }),
                        Err(error) => crate::log::info!("no avatar for {id}: {error}"),
                    }
                });
            }
            Request::Thumbnail { id, large } => {
                let slots = thumbnail_slots.clone();
                tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    if let Ok(image) = thumbnail(&id, large).await {
                        on_response(Response::Thumbnail { id, large, image });
                    }
                });
            }
        }
    }
}

/// What the errors caused by the browser cookies setting say, so they can be told apart.
pub const COOKIES_ERROR: &str =
    "yt-dlp could not read the cookies of the browser chosen in the settings.";

fn error_line(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.contains("ERROR"))
        .or_else(|| stderr.lines().last())
        .unwrap_or("yt-dlp failed")
        .trim();
    crate::log::warning!("yt-dlp failed: {line}");

    // A broken cookie database comes out as a bare SQLite error ("no such table: meta").
    let lower = line.to_lowercase();
    if [
        "cookie",
        "no such table",
        "database is locked",
        "unable to open database",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
    {
        format!("{COOKIES_ERROR} ({line})")
    } else {
        line.to_string()
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_string)
}

/// Streams the entries of a listing to `on_response` as yt-dlp finds them.
async fn listing(key: String, source: Source, on_response: &impl Fn(Response)) {
    let finish = |error| {
        on_response(Response::ListingDone {
            key: key.clone(),
            error,
        })
    };

    let mut language = None;
    let (url, limit) = match source {
        Source::Related { id } => {
            let result = related(&id).await;
            return match result {
                Ok(entries) => {
                    on_response(Response::Entries {
                        key: key.clone(),
                        entries,
                    });
                    finish(None)
                }
                Err(error) => finish(Some(error)),
            };
        }
        // Only YouTube's own results URL lists channels among the videos, and `sp` is a
        // protobuf carrying the sort in the first field and the filter in a second one.
        Source::Search {
            query,
            sort,
            filter,
        } => {
            let sp = search_sp(sort, filter);
            match reqwest::Url::parse_with_params(
                "https://www.youtube.com/results",
                [("search_query", query.as_str()), ("sp", &sp)],
            ) {
                Ok(url) => (url.to_string(), SEARCH_MAX_RESULTS),
                Err(e) => return finish(Some(e.to_string())),
            }
        }
        Source::Playlist { id } => (
            format!("https://www.youtube.com/playlist?list={}", bare_id(&id)),
            CHANNEL_MAX_RESULTS,
        ),
        Source::Channel {
            id,
            query,
            language: own,
        } if query.is_empty() => {
            language = own;
            (channel_videos_url(&id), CHANNEL_MAX_RESULTS)
        }
        Source::Channel {
            id, language: own, ..
        } if id.starts_with(RUMBLE_PREFIX) => {
            // Rumble channels can't be searched, what is typed is ignored.
            language = own;
            (channel_videos_url(&id), RUMBLE_MAX_RESULTS)
        }
        Source::Channel {
            id,
            query,
            language: own,
        } => match reqwest::Url::parse_with_params(
            &format!("{}/search", channel_url(&id)),
            [("query", query.as_str())],
        ) {
            Ok(url) => {
                language = own;
                (url.to_string(), CHANNEL_MAX_RESULTS)
            }
            Err(e) => return finish(Some(e.to_string())),
        },
    };

    // yt-dlp only prints a listing once it has all of it, which takes a while for a long
    // channel. A short one comes first so there is something to look at in the meantime.
    let mut seen = HashSet::new();
    if limit > SEARCH_MAX_RESULTS
        && let Err(error) = stream_listing(
            &key,
            &url,
            FIRST_PAGE,
            &mut seen,
            on_response,
            language.clone(),
        )
        .await
    {
        return finish(Some(error));
    }
    finish(
        stream_listing(&key, &url, limit, &mut seen, on_response, language)
            .await
            .err(),
    );
}

/// The newest videos of a channel, in its language so that their titles are the original ones.
/// YouTube has no language for channels, so one that isn't known yet is taken from its newest
/// video, and returned to be kept.
async fn latest(
    channel_id: &str,
    known: Option<String>,
) -> (Result<Vec<Entry>, String>, Option<String>) {
    let url = channel_videos_url(channel_id);
    let list = |language: Option<String>| {
        let url = url.clone();
        async move {
            let entries = Mutex::new(Vec::new());
            stream_listing(
                "",
                &url,
                LATEST_PER_CHANNEL,
                &mut HashSet::new(),
                &|response| {
                    if let Response::Entries { entries: batch, .. } = response {
                        entries.lock().unwrap().extend(batch);
                    }
                },
                language,
            )
            .await?;
            Ok::<_, String>(entries.into_inner().unwrap())
        }
    };

    if known.is_some() || channel_id.starts_with(RUMBLE_PREFIX) {
        return (list(known).await, None);
    }

    let entries = match list(None).await {
        Ok(entries) => entries,
        Err(error) => return (Err(error), None),
    };
    let found = match entries.first() {
        Some(entry) if !entry.url.is_empty() => info(&entry.url)
            .await
            .ok()
            .and_then(|info| info.language)
            .filter(|language| crate::settings::valid_language(language)),
        _ => None,
    };
    let default = crate::settings::language();
    match found {
        // The titles asked for in the default language are already the right ones.
        Some(language) if default.as_deref() != Some(language.as_str()) => {
            // If this fails the titles we have are still good, and so is the language.
            let entries = list(Some(language.clone())).await.unwrap_or(entries);
            (Ok(entries), Some(language))
        }
        found => (Ok(entries), found),
    }
}

const NO_SUGGESTIONS: &str = "YouTube returned no suggestions";

/// Asks YouTube's `next` endpoint, the one the watch page uses, for the videos suggested next
/// to this one. yt-dlp doesn't expose them.
async fn related(id: &str) -> Result<Vec<Entry>, String> {
    if id.starts_with(RUMBLE_PREFIX) {
        return Ok(Vec::new());
    }
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": "2.20250101.00.00",
                "hl": crate::settings::language().unwrap_or_else(|| "en".to_string()),
                "gl": "US",
            },
        },
        "videoId": bare_id(id),
    });

    let response = reqwest::Client::new()
        .post("https://www.youtube.com/youtubei/v1/next?prettyPrint=false")
        .header("content-type", "application/json")
        .header("user-agent", "Mozilla/5.0")
        .body(body.to_string())
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    let response: Value = serde_json::from_slice(&response).map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    collect_lockups(&response, id, &mut entries);
    if entries.is_empty() {
        Err(NO_SUGGESTIONS.to_string())
    } else {
        Ok(entries)
    }
}

/// Collects the videos of every `lockupViewModel` in the response, which is how the suggestions
/// are laid out, other than the one being watched.
fn collect_lockups(value: &Value, current: &str, entries: &mut Vec<Entry>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "lockupViewModel" {
                    let entry = parse_lockup(value).filter(|entry| entry.id != current);
                    entries.extend(entry.filter(|entry| !entries.iter().any(|e| e.id == entry.id)));
                } else {
                    collect_lockups(value, current, entries);
                }
            }
        }
        Value::Array(array) => {
            for value in array {
                collect_lockups(value, current, entries);
            }
        }
        _ => (),
    }
}

/// Parses a suggested video. Playlists, mixes and the like are left out.
fn parse_lockup(lockup: &Value) -> Option<Entry> {
    if lockup["contentType"] != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let id = string(lockup, "contentId")?;
    let metadata = &lockup["metadata"]["lockupMetadataViewModel"];

    // Rows of small parts: the channel name, then the views and how long ago.
    let rows: Vec<Vec<&str>> = metadata["metadata"]["contentMetadataViewModel"]["metadataRows"]
        .as_array()?
        .iter()
        .map(|row| {
            row["metadataParts"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part["text"]["content"].as_str())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    let duration = lockup["contentImage"]["thumbnailViewModel"]["overlays"]
        .as_array()?
        .iter()
        .flat_map(|overlay| {
            overlay["thumbnailBottomOverlayViewModel"]["badges"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .find_map(|badge| badge["thumbnailBadgeViewModel"]["text"].as_str())
        .and_then(parse_clock)?;

    let channel_id =
        metadata["image"]["decoratedAvatarViewModel"]["rendererContext"]["commandContext"]["onTap"]
            ["innertubeCommand"]["browseEndpoint"]["browseId"]
            .as_str()
            .unwrap_or_default();

    let stats = rows.iter().find(|row| row.len() == 2);

    Some(Entry {
        url: format!("https://www.youtube.com/watch?v={id}"),
        id: youtube_id(&id),
        title: string(&metadata["title"], "content")?,
        channel: rows
            .first()
            .and_then(|row| row.first())
            .map(|name| name.to_string())
            .unwrap_or_default(),
        channel_id: youtube_id(channel_id),
        duration: Some(duration),
        views: stats.and_then(|stats| parse_count(stats[0])),
        published: stats.and_then(|stats| parse_ago(stats[1])),
    })
}

/// "4:05" or "1:02:03", in seconds.
fn parse_clock(text: &str) -> Option<f64> {
    text.split(':').try_fold(0.0, |total, part| {
        part.parse::<f64>().ok().map(|part| total * 60.0 + part)
    })
}

/// "207M", "1.6B" or "952", the way YouTube abbreviates view counts.
fn parse_count(text: &str) -> Option<u64> {
    let (number, factor) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 1e3),
        'M' => (&text[..text.len() - 1], 1e6),
        'B' => (&text[..text.len() - 1], 1e9),
        _ => (text, 1.0),
    };
    number.parse::<f64>().ok().map(|n| (n * factor) as u64)
}

/// "3d ago" or "16y ago", as the approximate moment it was uploaded.
fn parse_ago(text: &str) -> Option<i64> {
    let text = text.strip_suffix(" ago")?;
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let amount: i64 = text[..split].parse().ok()?;
    let seconds = match &text[split..] {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "mo" => 30 * 86_400,
        "y" => 365 * 86_400,
        _ => return None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(now - amount * seconds)
}

/// The `sp` parameter for a search on YouTube's own results URL, as base64 protobuf. Field 1
/// is the sort (1 = rating, 2 = upload date, 3 = view count) and field 2 is a submessage, with
/// 1 = upload window (1 = hour .. 5 = year) and 3 = duration (1 = under 4 minutes, 2 = over 20
/// minutes, 3 = 4 to 20 minutes). Composing the two as a flat concatenation is what YouTube
/// itself does, and what yt-dlp's tests round-trip.
fn search_sp(sort: SearchSort, filter: SearchFilter) -> String {
    let mut sp = match sort {
        SearchSort::Relevance => Vec::new(),
        SearchSort::Newest => vec![0x08, 0x02],
        SearchSort::Views => vec![0x08, 0x03],
    };
    match filter {
        SearchFilter::None => (),
        SearchFilter::Hour => sp.extend_from_slice(&[0x12, 0x02, 0x08, 0x01]),
        SearchFilter::Today => sp.extend_from_slice(&[0x12, 0x02, 0x08, 0x02]),
        SearchFilter::Week => sp.extend_from_slice(&[0x12, 0x02, 0x08, 0x03]),
        SearchFilter::Month => sp.extend_from_slice(&[0x12, 0x02, 0x08, 0x04]),
        SearchFilter::Year => sp.extend_from_slice(&[0x12, 0x02, 0x08, 0x05]),
        SearchFilter::Short => sp.extend_from_slice(&[0x12, 0x02, 0x18, 0x01]),
        SearchFilter::Medium => sp.extend_from_slice(&[0x12, 0x02, 0x18, 0x03]),
        SearchFilter::Long => sp.extend_from_slice(&[0x12, 0x02, 0x18, 0x02]),
    };
    BASE64.encode(&sp)
}

/// Streams what yt-dlp lists at `url`, up to `end` entries, skipping the ones in `seen`.
/// Fails if nothing at all could be listed.
/// The yt-dlp command listing `url` up to `end` entries, one JSON line each. The results and
/// channel pages keep paging forever, so the limit is needed.
fn listing_command(url: &str, end: usize, language: Vec<String>) -> tokio::process::Command {
    let mut command = crate::settings::yt_dlp();
    // The entries of a Rumble channel are bare links, the title and the date are only known
    // once each video has been looked at.
    if rumble_channel_of_link(url).is_none() {
        command.arg("--flat-playlist");
    }
    command
        .args([
            "--no-warnings",
            // Without this the listings have no dates, the extractor works them out from
            // the relative ones YouTube shows ("3 days ago").
            "--extractor-args",
            "youtubetab:approximate_date",
            &format!("--playlist-end={end}"),
            "-j",
        ])
        .args(language)
        .arg(url);
    command
}

/// The dates of what is listed at `url`, by video id, from a listing in English.
async fn listing_dates(url: &str, end: usize) -> HashMap<String, i64> {
    let output = listing_command(
        url,
        end,
        vec!["--extractor-args".into(), "youtube:lang=en".into()],
    )
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .output()
    .await;
    let Ok(output) = output else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| parse_entry(&serde_json::from_str(line).ok()?))
        .filter_map(|entry| Some((entry.id, entry.published?)))
        .collect()
}

/// Gives the entries without a date the one the English listing has.
async fn fill_dates(
    batch: &mut [Entry],
    task: &mut Option<tokio::task::JoinHandle<HashMap<String, i64>>>,
    dates: &mut Option<HashMap<String, i64>>,
) {
    if let Some(task) = task.take() {
        *dates = task.await.ok();
    }
    if let Some(dates) = dates {
        for entry in batch.iter_mut().filter(|e| e.published.is_none()) {
            entry.published = dates.get(&entry.id).copied();
        }
    }
}

async fn stream_listing(
    key: &str,
    url: &str,
    end: usize,
    seen: &mut HashSet<String>,
    on_response: &impl Fn(Response),
    language: Option<String>,
) -> Result<(), String> {
    // The dates come from the relative ones YouTube shows ("3 days ago"), which yt-dlp can only
    // read in English. In another language they are taken from a listing in English.
    let mut dates = (crate::settings::listing_language(language.clone())
        .is_some_and(|language| !language.starts_with("en"))
        && rumble_channel_of_link(url).is_none())
    .then(|| {
        let url = url.to_string();
        tokio::spawn(async move { listing_dates(&url, end).await })
    });
    let mut dates_by_id: Option<HashMap<String, i64>> = None;

    let mut child = listing_command(url, end, crate::settings::language_args(language))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    let mut stderr = child.stderr.take().unwrap();
    let stderr = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        buffer
    });

    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut batch = Vec::new();

    loop {
        // Entries tend to arrive a page at a time, so wait a little to send them together.
        let line = if batch.is_empty() {
            lines.next_line().await
        } else {
            match tokio::time::timeout(Duration::from_millis(100), lines.next_line()).await {
                Ok(line) => line,
                Err(_) => {
                    fill_dates(&mut batch, &mut dates, &mut dates_by_id).await;
                    on_response(Response::Entries {
                        key: key.to_string(),
                        entries: std::mem::take(&mut batch),
                    });
                    continue;
                }
            }
        };

        let Ok(Some(line)) = line else { break };
        let value = serde_json::from_str::<Value>(&line).ok();
        if let Some(channel) = value.as_ref().and_then(parse_channel) {
            if seen.insert(channel.id.clone()) {
                on_response(Response::Channels {
                    key: key.to_string(),
                    channels: vec![channel],
                });
            }
        } else if let Some(entry) = value.as_ref().and_then(parse_entry)
            && seen.insert(entry.id.clone())
        {
            batch.push(entry);
        }
    }

    if !batch.is_empty() {
        fill_dates(&mut batch, &mut dates, &mut dates_by_id).await;
        on_response(Response::Entries {
            key: key.to_string(),
            entries: batch,
        });
    }

    let success = child.wait().await.is_ok_and(|status| status.success());
    let stderr = stderr.await.unwrap_or_default();

    // yt-dlp exits non-zero when it skipped some entries but still printed the rest.
    if seen.is_empty() && !success {
        Err(error_line(&stderr))
    } else {
        Ok(())
    }
}

/// Parses a channel row of a search, which has no duration and links to `/channel/`.
fn parse_channel(entry: &Value) -> Option<ChannelRef> {
    if !string(entry, "url")?.contains("youtube.com/channel/") || entry["duration"].is_number() {
        return None;
    }
    let id = string(entry, "channel_id").or_else(|| string(entry, "id"))?;
    let name = string(entry, "channel")
        .or_else(|| string(entry, "title"))
        .or_else(|| string(entry, "uploader"))?;
    Some(ChannelRef {
        id: youtube_id(&id),
        name,
    })
}

/// Parses a Rumble video of a listing, whose entries can lack a duration and the channel.
fn parse_rumble_entry(entry: &Value, id: String, url: String) -> Option<Entry> {
    Some(Entry {
        id: rumble_id(&id),
        url,
        title: string(entry, "title")?,
        channel: string(entry, "channel")
            .or_else(|| string(entry, "uploader"))
            .or_else(|| string(entry, "playlist_channel"))
            .or_else(|| string(entry, "playlist_uploader"))
            .unwrap_or_default(),
        channel_id: string(entry, "channel_url")
            .or_else(|| string(entry, "playlist_webpage_url"))
            .and_then(|url| rumble_channel_of_link(&url))
            .map(|path| rumble_id(&path))
            .unwrap_or_default(),
        duration: entry["duration"].as_f64(),
        views: entry["view_count"].as_u64(),
        published: entry["timestamp"].as_i64(),
    })
}

/// Parses one line of `yt-dlp --flat-playlist -j` output.
fn parse_entry(entry: &Value) -> Option<Entry> {
    let url = string(entry, "url");
    let page = string(entry, "webpage_url").or_else(|| url.clone())?;
    if let Some((id, url)) = rumble_video_of_link(&page) {
        return parse_rumble_entry(entry, id, url);
    }
    let url = url?;
    // Playlists and channels show up in search results too.
    if !url.contains("/watch?v=") && !url.contains("/shorts/") {
        return None;
    }

    if let Some(availability) = entry["availability"].as_str()
        && availability != "public"
        && availability != "unlisted"
    {
        return None;
    }

    // Ongoing and upcoming live streams have no duration.
    let duration = entry["duration"].as_f64()?;

    // Entries of a channel's own tabs don't repeat which channel they belong to.
    Some(Entry {
        id: youtube_id(&string(entry, "id")?),
        url,
        title: string(entry, "title")?,
        channel: string(entry, "channel")
            .or_else(|| string(entry, "uploader"))
            .or_else(|| string(entry, "playlist_channel"))
            .or_else(|| string(entry, "playlist_uploader"))
            .unwrap_or_default(),
        channel_id: youtube_id(
            &string(entry, "channel_id")
                .or_else(|| string(entry, "playlist_channel_id"))
                .unwrap_or_default(),
        ),
        duration: Some(duration),
        views: entry["view_count"].as_u64(),
        published: entry["timestamp"].as_i64(),
    })
}

/// Runs yt-dlp with the cookies of the browser. They are read as it starts, so a search for a
/// single result is enough to find out whether they can be.
async fn check_cookies(browser: &str) -> Result<(), String> {
    let mut command = crate::tools::command("yt-dlp");
    command.args([
        "--cookies-from-browser",
        &crate::settings::browser_argument(browser),
    ]);
    let proxy = crate::settings::get().proxy;
    if !proxy.trim().is_empty() {
        command.args(["--proxy", proxy.trim()]);
    }

    let output = command
        .args([
            "--no-warnings",
            "--simulate",
            "--flat-playlist",
            "--playlist-end=1",
            "ytsearch1:test",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(error_line(&output.stderr))
    }
}

/// What a video offers in captions, from its details.
#[derive(Debug, Clone, Default)]
struct CaptionSource {
    language: Option<String>,
    /// Languages with subtitles somebody made, which come first.
    manual: Vec<String>,
    /// Languages with the captions YouTube generates, `pt-orig` being the speech as it was said.
    automatic: Vec<String>,
}

impl CaptionSource {
    fn parse(video: &Value) -> Self {
        let keys = |name: &str| -> Vec<String> {
            video[name]
                .as_object()
                .map(|tracks| tracks.keys().cloned().collect())
                .unwrap_or_default()
        };
        Self {
            language: string(video, "language"),
            manual: keys("subtitles"),
            automatic: keys("automatic_captions"),
        }
    }

    /// The track for `language`, as yt-dlp names it: the subtitles somebody made when there are,
    /// otherwise the generated captions. With `original` it is the generated captions of what
    /// was said that are looked for, `pt-orig`: a dubbed video has captions for each dub, and
    /// only the language of the video tells which one is the real speech.
    fn track(&self, language: &str, original: bool) -> Option<String> {
        let base = language.split('-').next()?;
        let has = |tracks: &[String], key: &str| tracks.iter().any(|track| track == key);

        [language, base]
            .into_iter()
            .find(|key| has(&self.manual, key))
            .map(str::to_string)
            .or_else(|| {
                let keys = if original {
                    [format!("{language}-orig"), format!("{base}-orig")]
                } else {
                    [language.to_string(), base.to_string()]
                };
                keys.into_iter().find(|key| has(&self.automatic, key))
            })
    }

    /// The track to download for the mode, `default` being the default language.
    fn track_for(&self, mode: CaptionMode, default: Option<&str>) -> Option<String> {
        let base = |language: &str| language.split('-').next().unwrap_or(language).to_string();
        let original = self.language.as_deref();

        match mode {
            CaptionMode::Off => None,
            CaptionMode::Original => self.track(original?, true),
            CaptionMode::Default => {
                let default = default?;
                match original {
                    // The language of the video is the one wanted.
                    Some(original) if base(original) == base(default) => self.track(original, true),
                    _ => self.track(default, false),
                }
            }
        }
    }
}

/// The caption sources of the videos whose details were fetched, by bare id.
static CAPTION_SOURCES: Mutex<Option<HashMap<String, CaptionSource>>> = Mutex::new(None);

async fn info(url: &str) -> Result<VideoInfo, String> {
    let output = crate::settings::yt_dlp()
        .args([
            "--no-warnings",
            "--no-playlist",
            "--skip-download",
            "--dump-single-json",
            url,
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    let Ok(video) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Err(error_line(&output.stderr));
    };

    let raw_id = string(&video, "id").ok_or("yt-dlp did not report the video id")?;
    let rumble = rumble_video_of_link(url).is_some();
    let id = if rumble {
        rumble_id(&raw_id)
    } else {
        youtube_id(&raw_id)
    };
    CAPTION_SOURCES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(bare_id(&id).to_string(), CaptionSource::parse(&video));

    Ok(VideoInfo {
        entry: Entry {
            id,
            url: string(&video, "webpage_url").unwrap_or_else(|| url.to_string()),
            title: string(&video, "title").unwrap_or_default(),
            channel: string(&video, "channel")
                .or_else(|| string(&video, "uploader"))
                .unwrap_or_default(),
            channel_id: if rumble {
                string(&video, "channel_url")
                    .and_then(|url| rumble_channel_of_link(&url))
                    .map(|path| rumble_id(&path))
                    .unwrap_or_default()
            } else {
                youtube_id(&string(&video, "channel_id").unwrap_or_default())
            },
            duration: video["duration"].as_f64(),
            views: video["view_count"].as_u64(),
            published: video["timestamp"].as_i64(),
        },
        description: string(&video, "description").unwrap_or_default(),
        upload_date: string(&video, "upload_date"),
        views: video["view_count"].as_u64(),
        likes: video["like_count"].as_u64(),
        language: string(&video, "language"),
        chapters: video["chapters"]
            .as_array()
            .map(|chapters| {
                chapters
                    .iter()
                    .filter_map(|chapter| {
                        Some(Chapter {
                            title: string(chapter, "title").unwrap_or_default(),
                            start: chapter["start_time"].as_f64()?,
                            end: chapter["end_time"].as_f64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

async fn download(
    entry: &Entry,
    height: Option<u32>,
    cancel: Arc<Notify>,
    on_response: &impl Fn(Response),
) -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    if let Some(path) = crate::cache::find_size(&entry.id, height) {
        return Ok(path);
    }

    let mut child = crate::settings::yt_dlp()
        .args([
            "--no-warnings",
            "--no-playlist",
            "--progress",
            "--newline",
            "--progress-template",
            "download:PROG %(progress.downloaded_bytes|0)s %(progress.total_bytes|0)s %(progress.total_bytes_estimate|0)s",
            "--merge-output-format",
            "mp4",
            "--no-simulate",
            "--print",
            "after_move:filepath",
        ])
        .args(crate::settings::download_args(height))
        .arg("-o")
        .arg(dir.join(format!("{}.%(ext)s", crate::cache::stem(&entry.id, height))))
        .arg(&entry.url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    let mut stderr = child.stderr.take().unwrap();
    let stderr = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        buffer
    });

    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut path = None;

    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = cancel.notified() => {
                let _ = child.kill().await;
                return Err("Cancelled".to_string());
            }
        };

        let Ok(Some(line)) = line else { break };
        if let Some(progress) = line.strip_prefix("PROG ") {
            let numbers: Vec<f64> = progress
                .split_whitespace()
                .filter_map(|number| number.parse().ok())
                .collect();
            if let [downloaded, total, estimate] = numbers[..] {
                let total = if total > 0.0 { total } else { estimate };
                if total > 0.0 {
                    on_response(Response::Progress {
                        id: entry.id.clone(),
                        fraction: (downloaded / total).min(1.0),
                    });
                }
            }
        } else if !line.trim().is_empty() {
            path = Some(PathBuf::from(line.trim()));
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;
    if !status.success() {
        return Err(error_line(&stderr.await.unwrap_or_default()));
    }

    path.or_else(|| crate::cache::find_size(&entry.id, height))
        .ok_or_else(|| "yt-dlp did not report the downloaded file".to_string())
}

/// Gets the captions the mode asks for, as `<video>.<track>.vtt` next to the video, and returns
/// the track and the file. `None` when the video has none or they can't be had, a video is fine
/// without them.
async fn fetch_captions(
    entry: &Entry,
    video: &std::path::Path,
    mode: CaptionMode,
) -> (Option<(String, PathBuf)>, bool) {
    if mode == CaptionMode::Off || entry.url.is_empty() || entry.id.starts_with(RUMBLE_PREFIX) {
        return (None, false);
    }

    let id = bare_id(&entry.id).to_string();
    let known = |id: &str| {
        CAPTION_SOURCES
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.get(id).cloned())
    };
    let source = match known(&id) {
        Some(source) => source,
        None => {
            if info(&entry.url).await.is_err() {
                return (None, true);
            }
            match known(&id) {
                Some(source) => source,
                None => return (None, true),
            }
        }
    };

    let track = source.track_for(mode, crate::settings::language().as_deref());
    crate::log::info!(
        "captions of {id}: mode {mode:?}, default language {:?}, language of the video {:?}, track {track:?}",
        crate::settings::language(),
        source.language
    );
    let Some(track) = track else {
        return (None, false);
    };
    let base = video.with_extension("");
    let file = PathBuf::from(format!("{}.{track}.vtt", base.display()));
    if file.exists() {
        crate::log::info!("captions of {id}: {} is there already", file.display());
        return (Some((track, file)), false);
    }

    let output = crate::settings::yt_dlp()
        .args([
            "--no-warnings",
            "--no-playlist",
            "--skip-download",
            "--write-subs",
            "--write-auto-subs",
            "--sub-format",
            "vtt",
            "--sub-langs",
            &track,
            "-o",
        ])
        .arg(format!("{}.%(ext)s", base.display()))
        .arg(&entry.url)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await;
    match &output {
        Ok(output) if !output.status.success() => {
            crate::log::warning!(
                "captions of {id}: yt-dlp for {track} failed with {}: {}",
                output.status,
                error_line(&output.stderr)
            );
        }
        Err(error) => crate::log::warning!("captions of {id}: cannot run yt-dlp: {error}"),
        Ok(_) => {}
    }
    crate::log::info!(
        "captions of {id}: {} exists: {}",
        file.display(),
        file.exists()
    );
    if file.exists() {
        (Some((track, file)), false)
    } else {
        // The video has the captions, they could not be had.
        (None, true)
    }
}

/// The sponsor segments SponsorBlock knows for a video. Only the start of the hash of the id is
/// sent, so that the service doesn't learn which video it is.
async fn sponsor_segments(id: &str) -> Result<Vec<(f64, f64)>, String> {
    use sha2::{Digest, Sha256};

    if id.starts_with(RUMBLE_PREFIX) {
        return Ok(Vec::new());
    }
    let id = bare_id(id);
    let hash = Sha256::digest(id.as_bytes());
    let url = reqwest::Url::parse_with_params(
        &format!(
            "https://sponsor.ajay.app/api/skipSegments/{:02x}{:02x}",
            hash[0], hash[1]
        ),
        [("category", "sponsor"), ("actionType", "skip")],
    )
    .map_err(|e| e.to_string())?;

    let response = reqwest::get(url).await.map_err(|e| e.to_string())?;
    // Nothing is known about the videos without segments.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let bytes = response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let videos: Vec<Value> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

    let mut segments: Vec<(f64, f64)> = videos
        .iter()
        .filter(|video| video["videoID"] == id)
        .filter_map(|video| video["segments"].as_array())
        .flatten()
        .filter_map(|segment| {
            let range = segment["segment"].as_array()?;
            Some((range.first()?.as_f64()?, range.get(1)?.as_f64()?))
        })
        .filter(|(start, end)| end > start)
        .collect();
    segments.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(segments)
}

async fn thumbnail(id: &str, large: bool) -> Result<Thumbnail, String> {
    if id.starts_with(RUMBLE_PREFIX) {
        return Err("Rumble videos have no thumbnail here".to_string());
    }
    let bytes = reqwest::get(format!(
        "https://i.ytimg.com/vi/{}/mqdefault.jpg",
        bare_id(id)
    ))
    .await
    .and_then(|response| response.error_for_status())
    .map_err(|e| e.to_string())?
    .bytes()
    .await
    .map_err(|e| e.to_string())?;

    let mut image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    if !large {
        image = image.thumbnail(SMALL_THUMBNAIL.0, SMALL_THUMBNAIL.1);
    }
    let image = image.into_rgba8();

    Ok(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

/// How big the avatars are kept, they are shown at 40 pixels at most.
const AVATAR_SIZE: u32 = 96;

/// The picture of a YouTube channel, from the disk if it was fetched before.
async fn avatar(id: &str) -> Result<Thumbnail, String> {
    if id.starts_with(RUMBLE_PREFIX) {
        return Err("Rumble channels have no avatar here".to_string());
    }
    let file = cache_dir()
        .join("avatars")
        .join(format!("{}.jpg", bare_id(id)));
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(_) => {
            let head = listing_head(&channel_videos_url(id)).await?;
            let url = head["thumbnails"]
                .as_array()
                .and_then(|list| {
                    list.iter()
                        .find(|t| t["id"] == "avatar_uncropped")
                        .and_then(|t| t["url"].as_str())
                })
                .ok_or("The channel has no avatar")?;
            // The address ends with the size asked for, 0 being the original.
            let url = match url.rsplit_once("=s0") {
                Some((base, _)) => format!("{base}=s{AVATAR_SIZE}-c"),
                None => url.to_string(),
            };
            let bytes = reqwest::get(url)
                .await
                .and_then(|response| response.error_for_status())
                .map_err(|e| e.to_string())?
                .bytes()
                .await
                .map_err(|e| e.to_string())?
                .to_vec();
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&file, &bytes);
            bytes
        }
    };

    let image = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    Ok(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

/// The first entry of a listing, with the listing's own details.
async fn listing_head(url: &str) -> Result<Value, String> {
    let output = crate::settings::yt_dlp()
        .args([
            "--no-warnings",
            "--flat-playlist",
            "--playlist-end=1",
            "--dump-single-json",
            url,
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    serde_json::from_slice::<Value>(&output.stdout).map_err(|_| error_line(&output.stderr))
}

async fn resolve(input: Input) -> Result<Item, String> {
    match input {
        Input::Video(url) => Ok(Item::Video(info(&url).await?.entry)),
        Input::Channel(url) => {
            let rumble = rumble_channel_of_link(&url);
            let channel = listing_head(&match rumble {
                Some(_) => url.clone(),
                None => format!("{url}/videos"),
            })
            .await?;
            Ok(Item::Channel(ChannelRef {
                id: match rumble {
                    Some(path) => rumble_id(&path),
                    None => youtube_id(
                        &string(&channel, "channel_id").ok_or("This channel could not be found")?,
                    ),
                },
                name: string(&channel, "channel")
                    .or_else(|| string(&channel, "uploader"))
                    .unwrap_or_else(|| "Unnamed channel".to_string()),
            }))
        }
        Input::Playlist(url) => {
            let playlist = listing_head(&url).await?;
            Ok(Item::Playlist(PlaylistRef {
                id: youtube_id(&string(&playlist, "id").ok_or("This playlist could not be found")?),
                title: string(&playlist, "title").unwrap_or_else(|| "Unnamed playlist".to_string()),
            }))
        }
    }
}

#[cfg(test)]
mod link_tests {
    use super::video_id_of_link;

    #[test]
    fn finds_the_video_of_a_link() {
        for link in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3s",
            "youtu.be/dQw4w9WgXcQ?si=x",
            "https://m.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                video_id_of_link(link).as_deref(),
                Some("dQw4w9WgXcQ"),
                "{link}"
            );
        }
        for text in [
            "cats",
            "rust video",
            "https://example.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/results?search_query=a",
        ] {
            assert_eq!(video_id_of_link(text), None, "{text}");
        }
    }
}

#[cfg(test)]
mod caption_tests {
    use super::*;

    fn source(language: &str, manual: &[&str], automatic: &[&str]) -> CaptionSource {
        let own = |keys: &[&str]| keys.iter().map(|key| key.to_string()).collect();
        CaptionSource {
            language: Some(language.to_string()),
            manual: own(manual),
            automatic: own(automatic),
        }
    }

    #[test]
    fn the_track_follows_the_mode() {
        use CaptionMode::*;

        // A dubbed video has the captions of every dub, only one is the real speech.
        let dubbed = source("pt-BR", &[], &["en-orig", "es-orig", "pt-orig", "pt", "en"]);
        assert_eq!(
            dubbed.track_for(Original, Some("en")).as_deref(),
            Some("pt-orig")
        );
        assert_eq!(dubbed.track_for(Default, Some("en")).as_deref(), Some("en"));
        assert_eq!(
            dubbed.track_for(Default, Some("pt")).as_deref(),
            Some("pt-orig")
        );
        assert_eq!(dubbed.track_for(Off, Some("en")), None);

        // Subtitles somebody made win over the generated captions.
        let made = source("en", &["de", "en"], &["en-orig", "en"]);
        assert_eq!(made.track_for(Original, None).as_deref(), Some("en"));
        assert_eq!(made.track_for(Default, Some("de")).as_deref(), Some("de"));

        let regional = source("en-US", &[], &["en-US-orig", "es-US-orig"]);
        assert_eq!(
            regional.track_for(Original, None).as_deref(),
            Some("en-US-orig")
        );

        // Without the language of the video there is no original, only the default one.
        let unknown = CaptionSource {
            language: None,
            manual: vec!["en".to_string()],
            automatic: Vec::new(),
        };
        assert_eq!(unknown.track_for(Original, Some("en")), None);
        assert_eq!(
            unknown.track_for(Default, Some("en")).as_deref(),
            Some("en")
        );
        assert_eq!(unknown.track_for(Default, None), None);
    }
}

#[cfg(test)]
mod rumble_tests {
    use super::*;

    #[test]
    fn a_fully_extracted_video_is_an_entry() {
        let entry = parse_entry(&serde_json::json!({
            "id": "v5abcde",
            "webpage_url": "https://rumble.com/v5abcde-some-title.html",
            "title": "Some title",
            "channel": "Some channel",
            "channel_url": "https://rumble.com/c/SomeChannel",
            "duration": 61.0,
            "timestamp": 1700000000,
        }))
        .unwrap();
        assert_eq!(entry.id, "rumble:v5abcde");
        assert_eq!(entry.channel_id, "rumble:c/SomeChannel");
        assert_eq!(entry.published, Some(1700000000));
        assert_eq!(
            channel_url(&entry.channel_id),
            "https://rumble.com/c/SomeChannel"
        );
    }

    #[test]
    fn a_bare_link_is_not() {
        let bare = serde_json::json!({"url": "https://rumble.com/v5abcde-some-title.html"});
        assert!(parse_entry(&bare).is_none());
    }
}

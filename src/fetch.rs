use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Notify, Semaphore};

use crate::profile::{ChannelRef, Input, Item, PlaylistRef};

const SEARCH_MAX_RESULTS: usize = 100;
const CHANNEL_MAX_RESULTS: usize = 1000;
/// How much of a long listing is fetched first, to have something to show early.
const FIRST_PAGE: usize = 30;

/// How many of a channel's newest videos the subscriptions keep.
pub const LATEST_PER_CHANNEL: usize = 15;
/// Channels whose newest videos are looked up at the same time.
const LATEST_PARALLEL: usize = 3;

/// Thumbnails shown in lists are scaled down to keep a thousand of them cheap to hold.
const SMALL_THUMBNAIL: (u32, u32) = (168, 94);

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
    /// Download the video into the cache directory, if it isn't there yet.
    Download {
        entry: Entry,
    },
    /// Stops the download of the video with this id, if one is running.
    Cancel {
        id: String,
    },
    Thumbnail {
        id: String,
        large: bool,
    },
    /// Looks up what a link typed in by the user points to.
    Resolve {
        input: Input,
    },
    /// The newest videos of a channel, for the subscriptions.
    Latest {
        channel_id: String,
    },
    /// Tries yt-dlp with the cookies of the browser, before the setting is accepted.
    CheckCookies {
        browser: String,
    },
}

pub enum Response {
    /// A batch of a listing's entries; a listing arrives in several of these.
    Entries {
        key: String,
        entries: Vec<Entry>,
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
        result: Result<PathBuf, String>,
    },
    Thumbnail {
        id: String,
        large: bool,
        image: Thumbnail,
    },
    Latest {
        channel_id: String,
        result: Result<Vec<Entry>, String>,
    },
    Resolved {
        result: Result<Item, String>,
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
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lumpla")
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
    let latest_slots = Arc::new(Semaphore::new(LATEST_PARALLEL));
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
        let on_response = on_response.clone();

        match request {
            Request::Listing { key, source } => {
                tokio::spawn(async move { listing(key, source, on_response.as_ref()).await });
            }
            Request::Info { key, url } => {
                tokio::spawn(async move {
                    on_response(Response::Info {
                        key,
                        result: info(&url).await,
                    });
                });
            }
            Request::Download { entry } => {
                let cancel = Arc::new(Notify::new());
                downloads
                    .lock()
                    .unwrap()
                    .insert(entry.id.clone(), cancel.clone());

                let downloads = downloads.clone();
                tokio::spawn(async move {
                    let result = download(&entry, cancel, on_response.as_ref()).await;
                    downloads.lock().unwrap().remove(&entry.id);
                    on_response(Response::Downloaded {
                        id: entry.id,
                        result,
                    });
                });
            }
            Request::Cancel { id } => {
                if let Some(cancel) = downloads.lock().unwrap().get(&id) {
                    cancel.notify_one();
                }
            }
            Request::Latest { channel_id } => {
                let slots = latest_slots.clone();
                tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    on_response(Response::Latest {
                        result: latest(&channel_id).await,
                        channel_id,
                    });
                });
            }
            Request::Resolve { input } => {
                tokio::spawn(async move {
                    on_response(Response::Resolved {
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
        Source::Search {
            query,
            sort: SearchSort::Relevance,
            filter: SearchFilter::None,
        } => (
            format!("ytsearch{SEARCH_MAX_RESULTS}:{query}"),
            SEARCH_MAX_RESULTS,
        ),
        // The other sorts don't exist as yt-dlp search keys (`ytsearchdate` was dropped), so
        // they go through YouTube's own results URL, where `sp` is a protobuf carrying the
        // sort in the first field and the filter in a second one.
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
            format!("https://www.youtube.com/playlist?list={id}"),
            CHANNEL_MAX_RESULTS,
        ),
        Source::Channel { id, query } if query.is_empty() => (
            format!("https://www.youtube.com/channel/{id}/videos"),
            CHANNEL_MAX_RESULTS,
        ),
        Source::Channel { id, query } => match reqwest::Url::parse_with_params(
            &format!("https://www.youtube.com/channel/{id}/search"),
            [("query", query.as_str())],
        ) {
            Ok(url) => (url.to_string(), CHANNEL_MAX_RESULTS),
            Err(e) => return finish(Some(e.to_string())),
        },
    };

    // yt-dlp only prints a listing once it has all of it, which takes a while for a long
    // channel. A short one comes first so there is something to look at in the meantime.
    let mut seen = HashSet::new();
    if limit > SEARCH_MAX_RESULTS {
        if let Err(error) = stream_listing(&key, &url, FIRST_PAGE, &mut seen, on_response).await {
            return finish(Some(error));
        }
    }
    finish(
        stream_listing(&key, &url, limit, &mut seen, on_response)
            .await
            .err(),
    );
}

/// The newest videos of a channel.
async fn latest(channel_id: &str) -> Result<Vec<Entry>, String> {
    let url = format!("https://www.youtube.com/channel/{channel_id}/videos");
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
    )
    .await?;
    Ok(entries.into_inner().unwrap())
}

/// Asks YouTube's `next` endpoint, the one the watch page uses, for the videos suggested next
/// to this one. yt-dlp doesn't expose them.
async fn related(id: &str) -> Result<Vec<Entry>, String> {
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": "2.20250101.00.00",
                "hl": "en",
                "gl": "US",
            },
        },
        "videoId": id,
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
        Err("YouTube returned no suggestions".to_string())
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

    let channel_id = metadata["image"]["decoratedAvatarViewModel"]["rendererContext"]
        ["commandContext"]["onTap"]["innertubeCommand"]["browseEndpoint"]["browseId"]
        .as_str()
        .unwrap_or_default();

    let stats = rows.iter().find(|row| row.len() == 2);

    Some(Entry {
        url: format!("https://www.youtube.com/watch?v={id}"),
        id,
        title: string(&metadata["title"], "content")?,
        channel: rows
            .first()
            .and_then(|row| row.first())
            .map(|name| name.to_string())
            .unwrap_or_default(),
        channel_id: channel_id.to_string(),
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
async fn stream_listing(
    key: &str,
    url: &str,
    end: usize,
    seen: &mut HashSet<String>,
    on_response: &impl Fn(Response),
) -> Result<(), String> {
    // The results and channel pages keep paging forever, so the limit is needed here.
    let mut child = crate::settings::yt_dlp()
        .args([
            "--no-warnings",
            "--flat-playlist",
            // Without this the listings have no dates, the extractor works them out from
            // the relative ones YouTube shows ("3 days ago").
            "--extractor-args",
            "youtubetab:approximate_date",
            &format!("--playlist-end={end}"),
            "-j",
            url,
        ])
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
                    on_response(Response::Entries {
                        key: key.to_string(),
                        entries: std::mem::take(&mut batch),
                    });
                    continue;
                }
            }
        };

        let Ok(Some(line)) = line else { break };
        if let Some(entry) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|value| parse_entry(&value))
        {
            if seen.insert(entry.id.clone()) {
                batch.push(entry);
            }
        }
    }

    if !batch.is_empty() {
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

/// Parses one line of `yt-dlp --flat-playlist -j` output.
fn parse_entry(entry: &Value) -> Option<Entry> {
    let url = string(entry, "url")?;
    // Playlists and channels show up in search results too.
    if !url.contains("/watch?v=") && !url.contains("/shorts/") {
        return None;
    }

    if let Some(availability) = entry["availability"].as_str() {
        if availability != "public" && availability != "unlisted" {
            return None;
        }
    }

    // Ongoing and upcoming live streams have no duration.
    let duration = entry["duration"].as_f64()?;

    // Entries of a channel's own tabs don't repeat which channel they belong to.
    Some(Entry {
        id: string(entry, "id")?,
        url,
        title: string(entry, "title")?,
        channel: string(entry, "channel")
            .or_else(|| string(entry, "uploader"))
            .or_else(|| string(entry, "playlist_channel"))
            .or_else(|| string(entry, "playlist_uploader"))
            .unwrap_or_default(),
        channel_id: string(entry, "channel_id")
            .or_else(|| string(entry, "playlist_channel_id"))
            .unwrap_or_default(),
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

    Ok(VideoInfo {
        entry: Entry {
            id: string(&video, "id").ok_or("yt-dlp did not report the video id")?,
            url: string(&video, "webpage_url").unwrap_or_else(|| url.to_string()),
            title: string(&video, "title").unwrap_or_default(),
            channel: string(&video, "channel")
                .or_else(|| string(&video, "uploader"))
                .unwrap_or_default(),
            channel_id: string(&video, "channel_id").unwrap_or_default(),
            duration: video["duration"].as_f64(),
            views: video["view_count"].as_u64(),
            published: video["timestamp"].as_i64(),
        },
        description: string(&video, "description").unwrap_or_default(),
        upload_date: string(&video, "upload_date"),
        views: video["view_count"].as_u64(),
        likes: video["like_count"].as_u64(),
    })
}

async fn download(
    entry: &Entry,
    cancel: Arc<Notify>,
    on_response: &impl Fn(Response),
) -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    if let Some(path) = crate::cache::find(&entry.id) {
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
        .args(crate::settings::download_args())
        .arg("-o")
        .arg(dir.join(format!("{}.%(ext)s", entry.id)))
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

    path.or_else(|| crate::cache::find(&entry.id))
        .ok_or_else(|| "yt-dlp did not report the downloaded file".to_string())
}

async fn thumbnail(id: &str, large: bool) -> Result<Thumbnail, String> {
    let bytes = reqwest::get(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"))
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
            let channel = listing_head(&format!("{url}/videos")).await?;
            Ok(Item::Channel(ChannelRef {
                id: string(&channel, "channel_id").ok_or("This channel could not be found")?,
                name: string(&channel, "channel")
                    .or_else(|| string(&channel, "uploader"))
                    .unwrap_or_else(|| "Unnamed channel".to_string()),
            }))
        }
        Input::Playlist(url) => {
            let playlist = listing_head(&url).await?;
            Ok(Item::Playlist(PlaylistRef {
                id: string(&playlist, "id").ok_or("This playlist could not be found")?,
                title: string(&playlist, "title").unwrap_or_else(|| "Unnamed playlist".to_string()),
            }))
        }
    }
}

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const MAX_RESULTS: usize = 50;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub channel_id: String,
    pub duration: Option<f64>,
    pub views: Option<u64>,
}

/// What yt-dlp knows about a video beyond what the listings carry.
#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub description: String,
    pub upload_date: Option<String>,
    pub views: Option<u64>,
    pub likes: Option<u64>,
}

pub enum Source {
    Search { query: String, by_date: bool },
    /// A channel's uploads, or the results of searching inside that channel when `query` is set.
    Channel { id: String, query: String },
}

pub enum Request {
    /// `key` is echoed back in the response so the caller can tell which page it was for.
    Listing { key: String, source: Source },
    Info { key: String, url: String },
    /// Download the video into the cache directory, if it isn't there yet.
    Download { id: String, url: String },
}

pub enum Response {
    Listing {
        key: String,
        result: Result<Vec<Entry>, String>,
    },
    Info {
        key: String,
        result: Result<VideoInfo, String>,
    },
    Downloaded {
        id: String,
        result: Result<PathBuf, String>,
    },
    Thumbnail {
        id: String,
        image: Thumbnail,
    },
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
/// `on_response` is called from the worker thread.
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
    // Thumbnails already fetched (or being fetched) during this run.
    let thumbnails_requested = Arc::new(Mutex::new(HashSet::new()));

    while let Some(request) = rx.recv().await {
        let on_response = on_response.clone();
        let thumbnails_requested = thumbnails_requested.clone();

        tokio::spawn(async move {
            match request {
                Request::Listing { key, source } => {
                    let result = listing(source).await;

                    // Thumbnails trickle in after the results are on screen.
                    if let Ok(entries) = &result {
                        for entry in entries {
                            if !thumbnails_requested.lock().unwrap().insert(entry.id.clone()) {
                                continue;
                            }

                            let id = entry.id.clone();
                            let on_response = on_response.clone();
                            let thumbnails_requested = thumbnails_requested.clone();
                            tokio::spawn(async move {
                                match thumbnail(&id).await {
                                    Ok(image) => on_response(Response::Thumbnail { id, image }),
                                    Err(_) => {
                                        thumbnails_requested.lock().unwrap().remove(&id);
                                    }
                                }
                            });
                        }
                    }

                    on_response(Response::Listing { key, result });
                }
                Request::Info { key, url } => {
                    on_response(Response::Info {
                        key,
                        result: info(&url).await,
                    });
                }
                Request::Download { id, url } => {
                    let result = download(&id, &url).await;
                    on_response(Response::Downloaded { id, result });
                }
            }
        });
    }
}

/// Runs yt-dlp and parses its stdout as a single JSON document.
async fn yt_dlp_json(args: &[&str]) -> Result<Value, String> {
    let output = Command::new("yt-dlp")
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    // yt-dlp exits non-zero when it skipped some entries but still prints the rest.
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(json) if !json.is_null() => Ok(json),
        _ => Err(error_line(&output.stderr)),
    }
}

fn error_line(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    stderr
        .lines()
        .rev()
        .find(|line| line.contains("ERROR"))
        .or_else(|| stderr.lines().last())
        .unwrap_or("yt-dlp failed")
        .trim()
        .to_string()
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_string)
}

async fn listing(source: Source) -> Result<Vec<Entry>, String> {
    let url = match source {
        Source::Search {
            query,
            by_date: false,
        } => format!("ytsearch{MAX_RESULTS}:{query}"),
        // yt-dlp dropped `ytsearchdate`, so date order goes through YouTube's own results URL,
        // where `sp=CAI=` is the "sort by upload date" filter.
        Source::Search {
            query,
            by_date: true,
        } => reqwest::Url::parse_with_params(
            "https://www.youtube.com/results",
            [("search_query", query.as_str()), ("sp", "CAI=")],
        )
        .map_err(|e| e.to_string())?
        .to_string(),
        Source::Channel { id, query } if query.is_empty() => {
            format!("https://www.youtube.com/channel/{id}/videos")
        }
        Source::Channel { id, query } => reqwest::Url::parse_with_params(
            &format!("https://www.youtube.com/channel/{id}/search"),
            [("query", query.as_str())],
        )
        .map_err(|e| e.to_string())?
        .to_string(),
    };

    // The results and channel pages keep paging forever, so the limit is needed here.
    let playlist = yt_dlp_json(&[
        "--no-warnings",
        "--flat-playlist",
        &format!("--playlist-end={MAX_RESULTS}"),
        "--dump-single-json",
        &url,
    ])
    .await?;

    Ok(parse_entries(&playlist))
}

fn parse_entries(playlist: &Value) -> Vec<Entry> {
    // Entries of a channel's own tabs don't repeat which channel they belong to.
    let fallback_channel = string(playlist, "channel").or_else(|| string(playlist, "uploader"));
    let fallback_channel_id = string(playlist, "channel_id");

    playlist["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
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

            Some(Entry {
                id: string(entry, "id")?,
                url,
                title: string(entry, "title")?,
                channel: string(entry, "channel")
                    .or_else(|| string(entry, "uploader"))
                    .or_else(|| fallback_channel.clone())
                    .unwrap_or_default(),
                channel_id: string(entry, "channel_id")
                    .or_else(|| fallback_channel_id.clone())
                    .unwrap_or_default(),
                duration: Some(duration),
                views: entry["view_count"].as_u64(),
            })
        })
        .collect()
}

async fn info(url: &str) -> Result<VideoInfo, String> {
    let video = yt_dlp_json(&[
        "--no-warnings",
        "--no-playlist",
        "--skip-download",
        "--dump-single-json",
        url,
    ])
    .await?;

    Ok(VideoInfo {
        description: string(&video, "description").unwrap_or_default(),
        upload_date: string(&video, "upload_date"),
        views: video["view_count"].as_u64(),
        likes: video["like_count"].as_u64(),
    })
}

fn find_cached(dir: &Path, id: &str) -> Option<PathBuf> {
    let prefix = format!("{id}.");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| matches!(ext, "mp4" | "mkv" | "webm" | "mov"))
        })
}

async fn download(id: &str, url: &str) -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    if let Some(path) = find_cached(&dir, id) {
        return Ok(path);
    }

    let output = Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "--no-playlist",
            "--no-progress",
            "-f",
            "bv*[height<=1080]+ba/b[height<=1080]/b",
            "--merge-output-format",
            "mp4",
            "--no-simulate",
            "--print",
            "after_move:filepath",
            "-o",
        ])
        .arg(dir.join(format!("{id}.%(ext)s")))
        .arg(url)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if !output.status.success() {
        return Err(error_line(&output.stderr));
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .map(PathBuf::from)
        .or_else(|| find_cached(&dir, id))
        .ok_or_else(|| "yt-dlp did not report the downloaded file".to_string())
}

async fn thumbnail(id: &str) -> Result<Thumbnail, String> {
    let bytes = reqwest::get(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"))
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    let image = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .into_rgba8();

    Ok(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

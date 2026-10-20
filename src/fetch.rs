use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use yt_dlp::client::deps::Libraries;
use yt_dlp::Downloader;

const MAX_RESULTS: usize = 20;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub duration: Option<f64>,
}

pub enum Request {
    Search(String),
    /// Download the video into the cache directory, if it isn't there yet.
    Download { id: String, url: String },
}

pub enum Response {
    Results(Result<Vec<Entry>, String>),
    Downloaded(Result<PathBuf, String>),
    Thumbnail { id: String, image: Thumbnail },
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
    let downloader = async {
        let dir = cache_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let libraries = Libraries::new(PathBuf::from("yt-dlp"), PathBuf::from("ffmpeg"));
        Downloader::builder(libraries, dir)
            .build()
            .await
            .map(Arc::new)
            .map_err(|e| e.to_string())
    }
    .await;

    while let Some(request) = rx.recv().await {
        let on_response = on_response.clone();
        let downloader = match &downloader {
            Ok(downloader) => downloader.clone(),
            Err(error) => {
                on_response(match request {
                    Request::Search(_) => Response::Results(Err(error.clone())),
                    Request::Download { .. } => Response::Downloaded(Err(error.clone())),
                });
                continue;
            }
        };

        tokio::spawn(async move {
            on_response(match request {
                Request::Search(query) => {
                    let results = search(&downloader, &query).await;

                    // Thumbnails trickle in after the results are on screen.
                    if let Ok(entries) = &results {
                        for entry in entries {
                            let id = entry.id.clone();
                            let on_response = on_response.clone();
                            tokio::spawn(async move {
                                if let Ok(image) = thumbnail(&id).await {
                                    on_response(Response::Thumbnail { id, image });
                                }
                            });
                        }
                    }

                    Response::Results(results)
                }
                Request::Download { id, url } => {
                    Response::Downloaded(download(&downloader, &id, &url).await)
                }
            });
        });
    }
}

async fn search(downloader: &Downloader, query: &str) -> Result<Vec<Entry>, String> {
    let playlist = downloader
        .youtube_extractor()
        .search(query, MAX_RESULTS)
        .await
        .map_err(|e| e.to_string())?;

    Ok(playlist
        .entries
        .into_iter()
        .filter(|entry| entry.is_available())
        .map(|entry| Entry {
            id: entry.id,
            url: entry.url,
            title: entry.title,
            uploader: entry.uploader.unwrap_or_default(),
            duration: entry.duration,
        })
        .collect())
}

async fn download(downloader: &Downloader, id: &str, url: &str) -> Result<PathBuf, String> {
    let file_name = format!("{id}.mp4");
    let path = downloader.output_dir().join(&file_name);

    if path.exists() {
        return Ok(path);
    }

    let video = downloader
        .fetch_video_infos(url)
        .await
        .map_err(|e| e.to_string())?;

    downloader
        .download_video(&video, &file_name)
        .await
        .map_err(|e| e.to_string())
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

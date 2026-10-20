//! The download cache. The files in the cache directory are the source of truth; `index.json`
//! only remembers what we know about them (titles, channels...) so they can be listed nicely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fetch::{cache_dir, Entry};

const INDEX_FILE: &str = "index.json";
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "mkv", "webm", "mov", "m4v"];

pub struct CachedVideo {
    pub entry: Entry,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Whether `entry` comes from the index, as opposed to being made up from the file name.
    pub known: bool,
}

type Index = HashMap<String, Entry>;

fn read_index(dir: &Path) -> Index {
    std::fs::read(dir.join(INDEX_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_index(dir: &Path, index: &Index) {
    if let Ok(bytes) = serde_json::to_vec_pretty(index) {
        // Written aside and renamed so a crash can't leave a half-written index.
        let temporary = dir.join(format!("{INDEX_FILE}.tmp"));
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(temporary, dir.join(INDEX_FILE));
        }
    }
}

fn video_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext))
        })
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?.to_string();
            Some((id, path))
        })
        .collect()
}

/// The file of an already downloaded video.
pub fn find(id: &str) -> Option<PathBuf> {
    video_files(&cache_dir())
        .into_iter()
        .find(|(file_id, _)| file_id == id)
        .map(|(_, path)| path)
}

/// Lists the videos on disk, newest first, dropping index entries whose file is gone.
pub fn scan() -> Vec<CachedVideo> {
    let dir = cache_dir();
    let mut index = read_index(&dir);
    let files = video_files(&dir);

    let before = index.len();
    index.retain(|id, _| files.iter().any(|(file_id, _)| file_id == id));
    if index.len() != before {
        write_index(&dir, &index);
    }

    let mut videos: Vec<CachedVideo> = files
        .into_iter()
        .map(|(id, path)| {
            let metadata = std::fs::metadata(&path).ok();
            let known = index.contains_key(&id);
            let entry = index.get(&id).cloned().unwrap_or_else(|| Entry {
                url: format!("https://www.youtube.com/watch?v={id}"),
                title: id.clone(),
                id,
                channel: String::new(),
                channel_id: String::new(),
                duration: None,
                views: None,
                published: None,
            });

            CachedVideo {
                entry,
                size: metadata.as_ref().map_or(0, |m| m.len()),
                modified: metadata.and_then(|m| m.modified().ok()),
                known,
            }
        })
        .collect();

    videos.sort_by(|a, b| b.modified.cmp(&a.modified));
    videos
}

/// Remembers what we know about a video that was just downloaded.
pub fn record(entry: &Entry) {
    let dir = cache_dir();
    let mut index = read_index(&dir);
    index.insert(entry.id.clone(), entry.clone());
    write_index(&dir, &index);
}

/// Deletes the files of these videos, and forgets about them.
pub fn delete(ids: &[String]) {
    let dir = cache_dir();

    for (id, path) in video_files(&dir) {
        if ids.contains(&id) {
            let _ = std::fs::remove_file(path);
        }
    }

    let mut index = read_index(&dir);
    let before = index.len();
    index.retain(|id, _| !ids.contains(id));
    if index.len() != before {
        write_index(&dir, &index);
    }
}

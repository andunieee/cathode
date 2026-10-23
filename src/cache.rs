//! The download cache. The files in the cache directory are the source of truth; `index.json`
//! only remembers what we know about them (titles, channels...) so they can be listed nicely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::fetch::{bare_id, cache_dir, youtube_id, Entry};

const INDEX_FILE: &str = "index.json";
/// Downloaded videos that are not saved anywhere are deleted once they are this old.
const MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "mkv", "webm", "mov", "m4v"];

pub struct CachedVideo {
    pub entry: Entry,
    /// The file name without its extension: the id, plus the size for the other sizes of a video.
    pub stem: String,
    /// The size of a video that was downloaded at one other than the usual, `0` for the best.
    pub height: Option<u32>,
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

/// The name of the file of a video, without its extension. The usual size is just the id, the
/// other ones add the size, like `id.720p`.
pub fn stem(id: &str, height: Option<u32>) -> String {
    let id = bare_id(id);
    match height {
        None => id.to_string(),
        Some(0) => format!("{id}.best"),
        Some(height) => format!("{id}.{height}p"),
    }
}

/// The id and the size of the video a file stem is for.
pub fn parse_stem(stem: &str) -> (&str, Option<u32>) {
    let Some((id, size)) = stem.split_once('.') else {
        return (stem, None);
    };
    match size {
        "best" => (id, Some(0)),
        _ => match size.strip_suffix('p').and_then(|height| height.parse().ok()) {
            Some(height) => (id, Some(height)),
            None => (stem, None),
        },
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

/// The file of an already downloaded video, the one of the usual size.
pub fn find(id: &str) -> Option<PathBuf> {
    find_size(id, None)
}

/// The file of an already downloaded video at that size, `None` being the usual one.
pub fn find_size(id: &str, height: Option<u32>) -> Option<PathBuf> {
    let wanted = stem(id, height);
    video_files(&cache_dir())
        .into_iter()
        .find(|(file_stem, _)| *file_stem == wanted)
        .map(|(_, path)| path)
}

/// The caption files next to a video, as `(language, path)`. They are named after the video,
/// like `id.en.vtt` for `id.mp4`.
pub fn caption_files(video: &Path) -> Vec<(String, PathBuf)> {
    let (Some(dir), Some(stem)) = (
        video.parent(),
        video.file_stem().and_then(|stem| stem.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files: Vec<(String, PathBuf)> = read_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let language = name.strip_prefix(stem)?.strip_prefix('.')?.strip_suffix(".vtt")?;
            // The sizes of a video share its id as a prefix, their languages have no dot.
            (!language.is_empty() && !language.contains('.'))
                .then(|| (language.to_string(), entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// The other sizes a video was downloaded at.
pub fn sizes(id: &str) -> Vec<u32> {
    let mut sizes: Vec<u32> = video_files(&cache_dir())
        .into_iter()
        .filter_map(|(file_stem, _)| match parse_stem(&file_stem) {
            (file_id, Some(height)) if file_id == bare_id(id) => Some(height),
            _ => None,
        })
        .collect();
    sizes.sort_by(|a, b| b.cmp(a));
    sizes
}

/// Lists the videos on disk, newest first, dropping index entries whose file is gone.
pub fn scan() -> Vec<CachedVideo> {
    let dir = cache_dir();
    let mut index = read_index(&dir);
    let files = video_files(&dir);

    let before = index.len();
    index.retain(|id, _| files.iter().any(|(stem, _)| parse_stem(stem).0 == id));
    if index.len() != before {
        write_index(&dir, &index);
    }

    let mut videos: Vec<CachedVideo> = files
        .into_iter()
        .map(|(stem, path)| {
            let (id, height) = parse_stem(&stem);
            let id = id.to_string();
            let metadata = std::fs::metadata(&path).ok();
            let known = index.contains_key(&id);
            let entry = index.get(&id).cloned().unwrap_or_else(|| Entry {
                url: format!("https://www.youtube.com/watch?v={id}"),
                title: id.clone(),
                id: youtube_id(&id),
                channel: String::new(),
                channel_id: String::new(),
                duration: None,
                views: None,
                published: None,
            });

            CachedVideo {
                entry,
                stem,
                height,
                size: metadata.as_ref().map_or(0, |m| m.len()),
                modified: metadata.and_then(|m| m.modified().ok()),
                known,
            }
        })
        .collect();

    videos.sort_by(|a, b| b.modified.cmp(&a.modified));
    videos
}

/// Deletes the videos older than 48 hours, and forgets about them. Returns how many there were.
pub fn expire() -> usize {
    let now = SystemTime::now();
    let old: Vec<String> = video_files(&cache_dir())
        .into_iter()
        .filter(|(_, path)| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > MAX_AGE)
        })
        .map(|(id, _)| id)
        .collect();

    delete(&old);
    old.len()
}

/// Makes a video that was just downloaded count as new: its age is counted from now, whatever
/// date the file came with.
pub fn touch(id: &str, height: Option<u32>) {
    if let Some(path) = find_size(id, height) {
        let touched = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.set_modified(SystemTime::now()));
        if let Err(error) = touched {
            eprintln!("Cannot touch {id}: {error}");
        }
    }
}

/// Remembers what we know about a video that was just downloaded.
pub fn record(entry: &Entry) {
    let dir = cache_dir();
    let mut index = read_index(&dir);
    index.insert(bare_id(&entry.id).to_string(), entry.clone());
    write_index(&dir, &index);
}

/// Deletes the files with these names (see `stem`), and forgets about the videos that have
/// none left.
pub fn delete(stems: &[String]) {
    let dir = cache_dir();

    for (stem, path) in video_files(&dir) {
        if stems.contains(&stem) {
            for (_, captions) in caption_files(&path) {
                let _ = std::fs::remove_file(captions);
            }
            let _ = std::fs::remove_file(path);
        }
    }

    let left = video_files(&dir);
    let mut index = read_index(&dir);
    let before = index.len();
    index.retain(|id, _| left.iter().any(|(stem, _)| parse_stem(stem).0 == id));
    if index.len() != before {
        write_index(&dir, &index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_tell_the_sizes_apart() {
        assert_eq!(parse_stem("abc-_1"), ("abc-_1", None));
        assert_eq!(parse_stem(&stem("abc", Some(720))), ("abc", Some(720)));
        assert_eq!(parse_stem(&stem("abc", Some(0))), ("abc", Some(0)));
        assert_eq!(stem("abc", None), "abc");
    }

    #[test]
    fn captions_belong_to_their_video() {
        let dir = std::env::temp_dir().join(format!("cathode-subs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["abc.mp4", "abc.en.vtt", "abc.fr.vtt", "abc.720p.mp4", "abc.720p.en.vtt"] {
            std::fs::File::create(dir.join(name)).unwrap();
        }

        let languages = |video: &str| -> Vec<String> {
            caption_files(&dir.join(video)).into_iter().map(|(l, _)| l).collect()
        };
        assert_eq!(languages("abc.mp4"), ["en", "fr"]);
        assert_eq!(languages("abc.720p.mp4"), ["en"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_videos_expire() {
        let root = std::env::temp_dir().join(format!("cathode-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("XDG_CACHE_HOME", &root);
        let dir = cache_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let age = |name: &str, hours: u64| {
            let file = std::fs::File::create(dir.join(name)).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
                .unwrap();
        };
        age("old.mp4", 49);
        age("recent.mp4", 47);
        age("old.txt", 100);
        write_index(&dir, &Index::new());

        assert_eq!(expire(), 1);
        assert!(!dir.join("old.mp4").exists());
        assert!(dir.join("recent.mp4").exists() && dir.join("old.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}

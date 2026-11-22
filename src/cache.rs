//! The download cache. The files in the cache directory are the source of truth; `index.json`
//! only remembers what we know about them (titles, channels...) so they can be listed nicely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::fetch::{Entry, bare_id, cache_dir, youtube_id};

const INDEX_FILE: &str = "index.json";
/// The subdirectory of the cache where the downloaded videos are kept.
const VIDEOS_DIR: &str = "videos";
/// Downloaded videos that are not saved anywhere are deleted once they are this old.
const MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "mkv", "webm", "mov", "m4v"];
/// The extensions of the streams yt-dlp downloads before it merges them into a video.
const MEDIA_EXTENSIONS: [&str; 12] = [
    "mp4", "mkv", "webm", "mov", "m4v", "m4a", "mp3", "opus", "ogg", "aac", "flac", "wav",
];

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

/// Where the downloaded videos are kept.
pub fn videos_dir() -> PathBuf {
    cache_dir().join(VIDEOS_DIR)
}

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
        _ => match size
            .strip_suffix('p')
            .and_then(|height| height.parse().ok())
        {
            Some(height) => (id, Some(height)),
            None => (stem, None),
        },
    }
}

/// Whether the stem of a media file is the name of a finished video: the id alone, or the id and
/// a size, like `id.720p` or `id.best`.
fn is_final_stem(stem: &str) -> bool {
    let Some((id, size)) = stem.split_once('.') else {
        return true;
    };
    if id.is_empty() {
        return false;
    }
    size == "best"
        || size
            .strip_suffix('p')
            .is_some_and(|height| !height.is_empty() && height.chars().all(|c| c.is_ascii_digit()))
}

/// Whether a file of the cache is part of a download that hasn't finished: yt-dlp's `.part` and
/// `.ytdl` files, the temporary merges, and the separate streams it downloads before merging.
fn is_partial(name: &str) -> bool {
    if name.contains(".part")
        || name.ends_with(".ytdl")
        || name.contains(".temp.")
        || name.ends_with(".temp")
    {
        return true;
    }

    let path = Path::new(name);
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    if !MEDIA_EXTENSIONS.contains(&extension) {
        return false;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| !is_final_stem(stem))
}

/// Whether a file of the cache is part of an unfinished download of the video `id`.
fn is_partial_of(name: &str, id: &str) -> bool {
    name.strip_prefix(id)
        .is_some_and(|rest| rest.starts_with('.'))
        && is_partial(name)
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
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !is_partial(name))
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
    video_files(&videos_dir())
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
            let language = name
                .strip_prefix(stem)?
                .strip_prefix('.')?
                .strip_suffix(".vtt")?;
            // The sizes of a video share its id as a prefix, their languages have no dot.
            (!language.is_empty() && !language.contains('.'))
                .then(|| (language.to_string(), entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// The picture of the frames of a video shown over the seek bar, next to the video file and named
/// after it like its captions, with a different extension.
pub fn storyboard(video: &Path) -> Option<PathBuf> {
    video.file_stem()?;
    Some(video.with_extension("jpg"))
}

/// The other sizes a video was downloaded at.
pub fn sizes(id: &str) -> Vec<u32> {
    let mut sizes: Vec<u32> = video_files(&videos_dir())
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
    let files = video_files(&videos_dir());

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

    videos.sort_by_key(|b| std::cmp::Reverse(b.modified));
    videos
}

/// Deletes the videos older than 48 hours, and forgets about them. Returns how many there were.
pub fn expire() -> usize {
    let now = SystemTime::now();
    let old: Vec<String> = video_files(&videos_dir())
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

    // The storyboards of videos saved elsewhere are left behind, they are cheap to make again.
    let dir = videos_dir();
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jpg") {
                continue;
            }
            let age = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok());
            if age.is_some_and(|age| age > MAX_AGE) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

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
            crate::log::warning!("Cannot touch {id}: {error}");
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

    for (stem, path) in video_files(&videos_dir()) {
        if stems.contains(&stem) {
            for (_, captions) in caption_files(&path) {
                let _ = std::fs::remove_file(captions);
            }
            if let Some(storyboard) = storyboard(&path) {
                let _ = std::fs::remove_file(storyboard);
            }
            let _ = std::fs::remove_file(path);
        }
    }

    let left = video_files(&videos_dir());
    let mut index = read_index(&dir);
    let before = index.len();
    index.retain(|id, _| left.iter().any(|(stem, _)| parse_stem(stem).0 == id));
    if index.len() != before {
        write_index(&dir, &index);
    }
}

/// Deletes what is left of a download that didn't finish for a video: its `.part` and `.ytdl`
/// files, temporary merges, and the streams that were waiting to be merged. Returns how many files
/// there were.
pub fn delete_partials(id: &str) -> usize {
    let id = bare_id(id);
    let Ok(read_dir) = std::fs::read_dir(videos_dir()) else {
        return 0;
    };

    let mut deleted = 0;
    for entry in read_dir.filter_map(|entry| entry.ok()) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_partial_of(name, id) && std::fs::remove_file(entry.path()).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// Deletes the unfinished downloads of videos that aren't in `keep`, which are what a crash or a
/// stopped download leaves behind. Returns how many files there were.
pub fn cleanup_partials(keep: &[String]) -> usize {
    let keep: Vec<&str> = keep.iter().map(|id| bare_id(id)).collect();
    let Ok(read_dir) = std::fs::read_dir(videos_dir()) else {
        return 0;
    };

    let mut deleted = 0;
    for entry in read_dir.filter_map(|entry| entry.ok()) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((id, _)) = name.split_once('.') else {
            continue;
        };
        if keep.contains(&id) || !is_partial(name) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            deleted += 1;
        }
    }
    deleted
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
        for name in [
            "abc.mp4",
            "abc.en.vtt",
            "abc.fr.vtt",
            "abc.720p.mp4",
            "abc.720p.en.vtt",
        ] {
            std::fs::File::create(dir.join(name)).unwrap();
        }

        let languages = |video: &str| -> Vec<String> {
            caption_files(&dir.join(video))
                .into_iter()
                .map(|(l, _)| l)
                .collect()
        };
        assert_eq!(languages("abc.mp4"), ["en", "fr"]);
        assert_eq!(languages("abc.720p.mp4"), ["en"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unfinished_downloads_are_not_videos() {
        for name in [
            "abc.f137.mp4",
            "abc.720p.f251.webm",
            "abc.mp4.part",
            "abc.mp4.part-Frag12",
            "abc.ytdl",
            "abc.temp.mp4",
            "abc.720p.temp.mp4",
        ] {
            assert!(is_partial(name), "{name} is part of a download");
        }
        for name in [
            "abc.mp4",
            "abc.720p.mp4",
            "abc.best.mkv",
            "abc.en.vtt",
            "abc.720p.en.vtt",
            "abc.jpg",
        ] {
            assert!(!is_partial(name), "{name} is a finished video");
        }
        assert!(is_partial_of("abc.f137.mp4", "abc"));
        assert!(!is_partial_of("abcd.f137.mp4", "abc"));
    }

    #[test]
    fn old_videos_expire() {
        let root = std::env::temp_dir().join(format!("cathode-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // SAFETY: This test owns the environment for the process; nothing else reads it concurrently.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &root) };
        let dir = cache_dir();
        let videos = videos_dir();
        std::fs::create_dir_all(&videos).unwrap();

        let age = |name: &str, hours: u64| {
            let file = std::fs::File::create(videos.join(name)).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
                .unwrap();
        };
        age("old.mp4", 49);
        age("recent.mp4", 47);
        age("old.txt", 100);
        write_index(&dir, &Index::new());

        assert_eq!(expire(), 1);
        assert!(!videos.join("old.mp4").exists());
        assert!(videos.join("recent.mp4").exists() && videos.join("old.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}

//! The collections: places where videos are kept for good, as opposed to the cache that is
//! emptied after 48 hours. There are two kinds, both described by a file in the `collections`
//! directory of the config directory of the app (`collections/<name>.json`):
//! - a place to save to, which has a directory the saved videos are moved into (by default
//!   `saved` in the data directory of the app);
//! - an imported folder, which has no directory of its own: the file only points to the videos
//!   that were already there, and they are never moved or deleted.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;

/// What the default place to save to is called.
pub const DEFAULT_NAME: &str = "Saved";
const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "webm", "mov", "m4v", "avi"];
/// How deep an imported folder is searched.
const IMPORT_DEPTH: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedVideo {
    pub entry: Entry,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    /// Where saved videos go. Imported folders don't have one.
    pub directory: Option<PathBuf>,
    pub videos: Vec<SavedVideo>,
    /// The name of the file, without `.json`.
    #[serde(skip)]
    file: String,
}

impl Collection {
    /// Whether videos can be saved to it.
    pub fn saves(&self) -> bool {
        self.directory.is_some()
    }

    /// Whether the file is ours to delete, which is not the case for the ones that were imported.
    pub fn owns(&self, video: &SavedVideo) -> bool {
        self.directory
            .as_ref()
            .is_some_and(|directory| video.path.starts_with(directory))
    }
}

thread_local! {
    static LIST: RefCell<Vec<Collection>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(f: impl FnOnce(&mut Vec<Collection>) -> R) -> R {
    LIST.with(|list| f(&mut list.borrow_mut()))
}

fn directory() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lumpla")
        .join("collections")
}

/// Where the videos saved to a collection without a directory of its own go.
pub fn default_directory(name: &str) -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lumpla");
    if name == DEFAULT_NAME {
        base.join("saved")
    } else {
        base.join(file_name(name))
    }
}

/// The name made fit to be the name of a file.
fn file_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        "_".to_string()
    } else {
        name.to_string()
    }
}

fn write(collection: &Collection) -> Result<(), String> {
    let directory = directory();
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    // Written aside and renamed so a crash can't leave a half-written file.
    let path = directory.join(format!("{}.json", collection.file));
    let temporary = directory.join(format!("{}.json.tmp", collection.file));
    let bytes = serde_json::to_vec_pretty(collection).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

pub fn init() {
    let directory = directory();
    let first_run = !directory.exists();

    let mut list: Vec<Collection> = std::fs::read_dir(&directory)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let loaded = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Collection>(&bytes).map_err(|e| e.to_string())
                });
            match loaded {
                Ok(mut collection) => {
                    collection.file = path.file_stem()?.to_string_lossy().into_owned();
                    if collection.name.is_empty() {
                        collection.name = collection.file.clone();
                    }
                    Some(collection)
                }
                Err(error) => {
                    eprintln!("Cannot read {}: {error}", path.display());
                    None
                }
            }
        })
        .collect();
    sort(&mut list);
    with(|slot| *slot = list);

    if first_run {
        if let Err(error) = create(DEFAULT_NAME, Some(default_directory(DEFAULT_NAME))) {
            eprintln!("Cannot create the default collection: {error}");
        }
    }
}

fn sort(list: &mut [Collection]) {
    list.sort_by_key(|collection| collection.name.to_lowercase());
}

/// A name nobody has used yet, "Name", "Name 2"...
fn unique_name(list: &[Collection], wanted: &str) -> String {
    let taken = |name: &str| {
        let file = file_name(name).to_lowercase();
        list.iter()
            .any(|c| c.name.to_lowercase() == name.to_lowercase() || c.file.to_lowercase() == file)
    };

    let mut name = wanted.to_string();
    let mut number = 2;
    while taken(&name) {
        name = format!("{wanted} {number}");
        number += 1;
    }
    name
}

fn add(list: &mut Vec<Collection>, mut collection: Collection) -> Result<String, String> {
    collection.name = unique_name(list, &collection.name);
    collection.file = file_name(&collection.name);
    write(&collection)?;

    let name = collection.name.clone();
    list.push(collection);
    sort(list);
    Ok(name)
}

/// Makes a new place to save to. Returns the name it got, which differs from the one asked for
/// if that was taken.
pub fn create(name: &str, directory: Option<PathBuf>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("The name cannot be empty".to_string());
    }
    let directory = directory.unwrap_or_else(|| default_directory(name));
    with(|list| {
        add(
            list,
            Collection {
                name: name.to_string(),
                directory: Some(directory),
                ..Collection::default()
            },
        )
    })
}

/// Makes a collection of the videos in a folder, which stay where they are. Returns its name and
/// how many videos it found.
pub fn import_folder(folder: &Path) -> Result<(String, usize), String> {
    let mut paths = Vec::new();
    find_videos(folder, IMPORT_DEPTH, &mut paths);
    paths.sort();
    if paths.is_empty() {
        return Err("There are no videos in that folder".to_string());
    }

    let videos: Vec<SavedVideo> = paths
        .into_iter()
        .map(|path| SavedVideo {
            entry: local_entry(&path),
            path,
        })
        .collect();
    let count = videos.len();

    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported videos".to_string());
    let name = with(|list| {
        add(
            list,
            Collection {
                name,
                directory: None,
                videos,
                file: String::new(),
            },
        )
    })?;
    Ok((name, count))
}

fn find_videos(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(folder) else {
        return;
    };

    for path in read_dir.filter_map(|entry| entry.ok()).map(|e| e.path()) {
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }

        if path.is_dir() {
            if depth > 0 {
                find_videos(&path, depth - 1, found);
            }
        } else if path.to_str().is_some()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        {
            found.push(path);
        }
    }
}

/// The id videos that are just a file get. It tells them from YouTube videos.
pub const LOCAL_PREFIX: &str = "file:";

pub fn local_entry(path: &Path) -> Entry {
    Entry {
        id: format!("{LOCAL_PREFIX}{}", path.display()),
        url: String::new(),
        title: path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        channel: String::new(),
        channel_id: String::new(),
        duration: None,
        views: None,
        published: None,
    }
}

/// The places videos can be saved to.
pub fn savable() -> Vec<String> {
    with(|list| {
        list.iter()
            .filter(|c| c.saves())
            .map(|c| c.name.clone())
            .collect()
    })
}

pub fn get(name: &str) -> Option<Collection> {
    with(|list| list.iter().find(|c| c.name == name).cloned())
}

pub fn exists(name: &str) -> bool {
    with(|list| list.iter().any(|c| c.name == name))
}

/// How many videos each collection has.
pub fn counts() -> Vec<(String, usize)> {
    with(|list| {
        list.iter()
            .map(|c| (c.name.clone(), c.videos.len()))
            .collect()
    })
}

pub fn contains(name: &str, id: &str) -> bool {
    with(|list| {
        list.iter()
            .find(|c| c.name == name)
            .is_some_and(|c| c.videos.iter().any(|v| v.entry.id == id))
    })
}

/// The file of a video kept in some collection.
pub fn find(id: &str) -> Option<PathBuf> {
    with(|list| {
        list.iter()
            .flat_map(|c| &c.videos)
            .filter(|v| v.entry.id == id)
            .map(|v| v.path.clone())
            .find(|path| path.exists())
    })
}

/// Forgets the collection. The videos it points to are left alone.
pub fn remove(name: &str) -> Result<(), String> {
    with(|list| {
        let Some(position) = list.iter().position(|c| c.name == name) else {
            return Ok(());
        };
        let collection = list.remove(position);
        let path = directory().join(format!("{}.json", collection.file));
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    })
}

/// Puts the file of a video in the collection's directory, moving it there or copying it.
/// Returns where it is now.
pub fn save(name: &str, entry: &Entry, source: &Path, copy: bool) -> Result<PathBuf, String> {
    with(|list| {
        let collection = list
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or("That collection does not exist")?;
        let directory = collection
            .directory
            .clone()
            .ok_or("Videos cannot be saved to an imported folder")?;
        if let Some(saved) = collection.videos.iter().find(|v| v.entry.id == entry.id) {
            return Ok(saved.path.clone());
        }

        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let destination = free_path(&directory, entry, source);
        if copy {
            std::fs::copy(source, &destination).map_err(|e| e.to_string())?;
        } else if std::fs::rename(source, &destination).is_err() {
            // The cache may be on another disk.
            std::fs::copy(source, &destination).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(source);
        }

        collection.videos.insert(
            0,
            SavedVideo {
                entry: entry.clone(),
                path: destination.clone(),
            },
        );
        write(collection)?;
        Ok(destination)
    })
}

/// "Title [id].mp4", not overwriting anything.
fn free_path(directory: &Path, entry: &Entry, source: &Path) -> PathBuf {
    let title: String = file_name(&entry.title).chars().take(100).collect();
    let extension = source
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".to_string());

    let mut path = directory.join(format!("{title} [{}].{extension}", crate::fetch::bare_id(&entry.id)));
    let mut number = 2;
    while path.exists() {
        path = directory.join(format!("{title} [{}] {number}.{extension}", crate::fetch::bare_id(&entry.id)));
        number += 1;
    }
    path
}

/// Takes a video out of the collection, and deletes its file if the collection keeps its own.
pub fn remove_video(name: &str, id: &str) -> Result<(), String> {
    with(|list| {
        let collection = list
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or("That collection does not exist")?;
        let Some(position) = collection.videos.iter().position(|v| v.entry.id == id) else {
            return Ok(());
        };

        let video = collection.videos.remove(position);
        if collection.owns(&video) {
            let _ = std::fs::remove_file(&video.path);
        }
        write(collection)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(file_name("Saved"), "Saved");
        assert_eq!(file_name("a/b: c?"), "a_b_ c_");
        assert_eq!(file_name("  ..hidden "), "hidden");
        assert_eq!(file_name("///"), "___");
        assert_eq!(file_name(""), "_");
    }

    #[test]
    fn names_are_unique() {
        let mut list = Vec::new();
        for name in ["Music", "music", "Mu/sic", "Mu_sic"] {
            let collection = Collection {
                name: unique_name(&list, name),
                file: String::new(),
                ..Collection::default()
            };
            list.push(Collection {
                file: file_name(&collection.name),
                ..collection
            });
        }
        let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Music", "music 2", "Mu/sic", "Mu_sic 2"]);
    }

    #[test]
    fn saving_and_importing() {
        let root = std::env::temp_dir().join(format!("lumpla-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));

        // The first run makes the default place to save to.
        init();
        assert_eq!(savable(), [DEFAULT_NAME]);
        assert!(root.join("config/lumpla/collections/Saved.json").exists());

        let source = root.join("cache/abc.mp4");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"video").unwrap();
        let mut entry = local_entry(&source);
        entry.id = "yt:abc".to_string();
        entry.title = "A/B title".to_string();

        let saved = save(DEFAULT_NAME, &entry, &source, false).unwrap();
        assert_eq!(saved, root.join("data/lumpla/saved/A_B title [abc].mp4"));
        assert!(saved.exists() && !source.exists());
        assert!(contains(DEFAULT_NAME, "yt:abc") && find("yt:abc") == Some(saved.clone()));

        // What was written can be read back after a restart.
        with(|list| list.clear());
        init();
        assert!(contains(DEFAULT_NAME, "yt:abc"));

        // Imported folders point to the files, which are never touched.
        let folder = root.join("movies");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("one.mkv"), b"1").unwrap();
        std::fs::write(folder.join("sub/two.MP4"), b"2").unwrap();
        std::fs::write(folder.join("notes.txt"), b"3").unwrap();
        assert_eq!(import_folder(&folder).unwrap(), ("movies".to_string(), 2));
        let imported = get("movies").unwrap();
        assert!(!imported.saves() && imported.videos[0].entry.title == "one");
        assert!(save("movies", &entry, &saved, true).is_err());

        remove_video("movies", &imported.videos[0].entry.id).unwrap();
        assert!(folder.join("one.mkv").exists());
        remove("movies").unwrap();
        assert!(!root.join("config/lumpla/collections/movies.json").exists());
        assert!(folder.join("sub/two.MP4").exists());

        // The ones that are ours are deleted for real.
        remove_video(DEFAULT_NAME, "yt:abc").unwrap();
        assert!(!saved.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let collection: Collection = serde_json::from_str(r#"{"name": "A"}"#).unwrap();
        assert!(collection.videos.is_empty() && !collection.saves());
    }
}

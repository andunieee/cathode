//! The suggestions of the home page: the videos YouTube suggests next to the ones in the history,
//! the oldest first, one request at a time. Each video suggested is worth 2^(line / HALF_LIFE)
//! points, `line` being the line of the history that suggested it (1 for the oldest), so a line
//! always counts the same relative to the ones before it however long the history gets. The ones
//! suggested again add up. The sums are kept as base 2 logarithms, which can't overflow.
//! It lives in `scores_v1.json` in the data directory of the app, with how far into the history
//! the requests have got, so they go on from there on the next start, and the ids of the videos
//! dismissed, which are left out of everything suggested afterwards.
//! Once the whole history is done, a copy of the scores is combined with what is suggested next to
//! the newest unwatched videos of the subscriptions feed, each worth as much as the last line of
//! the history. That copy is what is shown, and it is never saved.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;
use crate::history;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Suggested {
    entry: Entry,
    /// The base 2 logarithm of the points.
    score: f64,
}

/// The lines of the history after which a suggestion is worth half as much.
const HALF_LIFE: f64 = 50.0;

/// log2(2^a + 2^b), without leaving the logarithms.
fn log_add(a: f64, b: f64) -> f64 {
    let (high, low) = if a > b { (a, b) } else { (b, a) };
    high + (1.0 + (low - high).exp2()).log2()
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    /// The line of the history that was asked about last, counted from 1 on the oldest, and the
    /// video on it.
    last_index: usize,
    last_id: String,
    videos: HashMap<String, Suggested>,
    /// The videos the user dismissed.
    dismissed: HashSet<String>,
}

/// The scores with the videos of the feed counted in, and the ones of the feed still to ask about.
struct Projection {
    videos: HashMap<String, Suggested>,
    queue: Vec<Entry>,
}

/// How many videos of the feed are counted in.
const FEED_VIDEOS: usize = 5;

#[derive(Default)]
struct State {
    file: File,
    projection: Option<Projection>,
    /// A request is out.
    asking: bool,
    /// The wait after the last one isn't over.
    cooling_down: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

fn path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cathode")
        .join("scores_v1.json")
}

fn save(file: &File) -> Result<(), String> {
    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    // Written aside and renamed so a crash can't leave a half-written file.
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(file).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

pub fn init() {
    let path = path();
    let file = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<File>(&bytes).unwrap_or_else(|error| {
            crate::log::warning!("Cannot read {}: {error}", path.display());
            File::default()
        }),
        Err(_) => File::default(),
    };
    with(|state| state.file = file);
}

/// The next video of the history to ask about, with its line number, unless a request is out or
/// the last one was too recent. The answer goes to `finish`.
/// Once the history is done, it is a video of the feed instead, with the line 0.
pub fn begin(feed: &[Entry]) -> Option<(usize, Entry)> {
    if with(|state| state.asking || state.cooling_down) {
        return None;
    }
    let last = with(|state| state.file.last_index);
    let next = match history::after(last) {
        Some(next) => {
            with(|state| state.projection = None);
            next
        }
        // Without a last line there is nothing to weigh the feed by.
        None if last == 0 => return None,
        None => with(|state| {
            if state.projection.is_none() {
                let mut queue: Vec<Entry> = feed
                    .iter()
                    .filter(|entry| {
                        !history::is_watched(&entry.id) && !state.file.dismissed.contains(&entry.id)
                    })
                    .take(FEED_VIDEOS)
                    .cloned()
                    .collect();
                // Popped from the end, the newest first.
                queue.reverse();
                // The feed may not be loaded yet.
                if queue.is_empty() {
                    return None;
                }
                state.projection = Some(Projection {
                    videos: state.file.videos.clone(),
                    queue,
                });
            }
            state
                .projection
                .as_mut()
                .and_then(|projection| projection.queue.pop())
                .map(|entry| (0, entry))
        })?,
    };
    with(|state| state.asking = true);
    Some(next)
}

/// Adds what was suggested for the video on line `index`, and saves. On failure the same video
/// is asked about again next time.
pub fn finish(index: usize, id: &str, result: Result<Vec<Entry>, String>) -> Result<(), String> {
    with(|state| {
        state.asking = false;
        state.cooling_down = true;
    });
    let entries = result?;

    if index == 0 {
        with(|state| {
            let score = state.file.last_index as f64 / HALF_LIFE;
            let File { dismissed, .. } = &state.file;
            if let Some(projection) = &mut state.projection {
                for entry in entries {
                    if history::is_watched(&entry.id) || dismissed.contains(&entry.id) {
                        continue;
                    }
                    match projection.videos.get_mut(&entry.id) {
                        Some(video) => video.score = log_add(video.score, score),
                        None => {
                            projection
                                .videos
                                .insert(entry.id.clone(), Suggested { entry, score });
                        }
                    }
                }
            }
        });
        return Ok(());
    }

    with(|state| {
        let score = index as f64 / HALF_LIFE;
        for entry in entries {
            if history::is_watched(&entry.id) || state.file.dismissed.contains(&entry.id) {
                continue;
            }
            match state.file.videos.get_mut(&entry.id) {
                Some(video) => video.score = log_add(video.score, score),
                None => {
                    state
                        .file
                        .videos
                        .insert(entry.id.clone(), Suggested { entry, score });
                }
            }
        }
        // The ones watched since are no use to keep.
        state.file.videos.retain(|id, _| !history::is_watched(id));
        state.file.last_index = index;
        state.file.last_id = id.to_string();
        save(&state.file)
    })
}

/// Takes the video out of the suggestions, because it was watched.
pub fn watched(id: &str) {
    with(|state| {
        if let Some(projection) = &mut state.projection {
            projection.videos.remove(id);
        }
        if state.file.videos.remove(id).is_some() {
            if let Err(error) = save(&state.file) {
                crate::log::warning!("Cannot save the suggestions: {error}");
            }
        }
    });
}

/// Takes the video out of the suggestions for good.
pub fn dismiss(id: &str) {
    with(|state| {
        if let Some(projection) = &mut state.projection {
            projection.videos.remove(id);
        }
        state.file.videos.remove(id);
        state.file.dismissed.insert(id.to_string());
        if let Err(error) = save(&state.file) {
            crate::log::warning!("Cannot save the suggestions: {error}");
        }
    });
}

/// The wait is over.
pub fn cooled_down() {
    with(|state| state.cooling_down = false);
}

/// The unwatched videos suggested, with the feed counted in once the history is done, the ones with the most points first.
pub fn ranked() -> Vec<Entry> {
    with(|state| {
        let videos = match &state.projection {
            Some(projection) => &projection.videos,
            None => &state.file.videos,
        };
        let mut videos: Vec<&Suggested> = videos
            .values()
            .filter(|video| !history::is_watched(&video.entry.id))
            .collect();
        videos.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        videos
            .into_iter()
            .map(|video| video.entry.clone())
            .collect()
    })
}

/// How many videos of the history were asked about, and how many there are.
pub fn progress() -> (usize, usize) {
    (with(|state| state.file.last_index), history::count())
}

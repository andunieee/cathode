//! The suggestions of the home page: the videos YouTube suggests next to the ones in the history,
//! the oldest first, one request at a time. Each video suggested is worth 2^(line / HALF_LIFE)
//! points, `line` being the line of the history that suggested it (1 for the oldest), so a line
//! always counts the same relative to the ones before it however long the history gets. The ones
//! suggested again add up. The sums are kept as base 2 logarithms, which can't overflow.
//! It lives in `scores_v1.json` in the data directory of the app, with how far into the history
//! the requests have got, so they go on from there on the next start.

use std::cell::RefCell;
use std::collections::HashMap;
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
}

#[derive(Default)]
struct State {
    file: File,
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
pub fn begin() -> Option<(usize, Entry)> {
    if with(|state| state.asking || state.cooling_down) {
        return None;
    }
    let next = history::after(with(|state| state.file.last_index))?;
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

    with(|state| {
        let score = index as f64 / HALF_LIFE;
        for entry in entries {
            if history::is_watched(&entry.id) {
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

/// The wait is over.
pub fn cooled_down() {
    with(|state| state.cooling_down = false);
}

/// The unwatched videos suggested, the ones with the most points first.
pub fn ranked() -> Vec<Entry> {
    with(|state| {
        let mut videos: Vec<&Suggested> = state
            .file
            .videos
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

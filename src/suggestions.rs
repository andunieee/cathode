//! The suggestions of the home page: every two days, a round picks a few videos at random among
//! the recent ones of the history and the newest unwatched ones of the subscriptions feed, asks
//! YouTube what it suggests next to each, one request at a time, and merges the answers. The
//! videos suggested next to more of them come first, then the ones suggested higher up.
//! What a round finds replaces the last round's once it is done, so the suggestions shown don't
//! change under the user's eyes, except on the first round, which is shown as it goes.
//! It lives in `suggestions_v2.json` in the data directory of the app, with the round going on,
//! so it goes on from there on the next start, and the ids of the videos dismissed, which are
//! left out of everything suggested afterwards.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;
use crate::{history, subscriptions};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Suggested {
    entry: Entry,
    /// How many of the videos asked about suggested it.
    count: u32,
    /// The highest place it was suggested at, 0 for the first.
    best: usize,
}

/// How long a round's suggestions last before the next round starts.
const ROUND_SECONDS: i64 = 2 * 24 * 60 * 60;
/// How many of the latest videos of the history are picked from, and how many are picked.
const RECENT_HISTORY: usize = 100;
const HISTORY_PICKS: usize = 10;
/// The same for the newest unwatched videos of the feed.
const RECENT_FEED: usize = 30;
const FEED_PICKS: usize = 5;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Round {
    /// When it started, in seconds since the epoch.
    started: i64,
    /// The videos still to ask about, popped from the end.
    queue: Vec<Entry>,
    /// How many there were to ask about.
    total: usize,
    videos: HashMap<String, Suggested>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    /// What the last finished round found.
    videos: HashMap<String, Suggested>,
    round: Round,
    /// The videos the user dismissed.
    dismissed: HashSet<String>,
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
    crate::paths::data().join("suggestions_v2.json")
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

fn save_or_warn(file: &File) {
    if let Err(error) = save(file) {
        crate::log::warning!("Cannot save the suggestions: {error}");
    }
}

/// The videos dismissed in `scores_v1.json`, the file of the way suggestions used to be made.
fn dismissed_v1() -> HashSet<String> {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct V1 {
        dismissed: HashSet<String>,
    }

    std::fs::read(crate::paths::data().join("scores_v1.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<V1>(&bytes).ok())
        .unwrap_or_default()
        .dismissed
}

pub fn init() {
    let path = path();
    let file = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<File>(&bytes).unwrap_or_else(|error| {
            crate::log::warning!("Cannot read {}: {error}", path.display());
            File::default()
        }),
        Err(_) => File {
            dismissed: dismissed_v1(),
            ..File::default()
        },
    };
    with(|state| state.file = file);
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs() as i64)
}

/// Up to `count` of the entries, picked at random.
fn pick(mut entries: Vec<Entry>, count: usize) -> Vec<Entry> {
    // A partial Fisher-Yates shuffle of the first `count`.
    let count = count.min(entries.len());
    let mut random = vec![0u8; count * 8];
    if getrandom::fill(&mut random).is_err() {
        entries.truncate(count);
        return entries;
    }
    for (i, bytes) in random.chunks_exact(8).enumerate() {
        let number = u64::from_le_bytes(bytes.try_into().unwrap());
        let j = i + (number % (entries.len() - i) as u64) as usize;
        entries.swap(i, j);
    }
    entries.truncate(count);
    entries
}

/// Starts a round if the last one is two days old, unless the feed is still loading.
fn start_round(state: &mut State) {
    let now = now();
    if now - state.file.round.started < ROUND_SECONDS {
        return;
    }
    let feed = subscriptions::feed();
    if feed.is_empty() && subscriptions::pending_count() > 0 {
        return;
    }

    let dismissed = &state.file.dismissed;
    let (watched, _, _) = history::read(u64::MAX, RECENT_HISTORY);
    let watched = watched
        .into_iter()
        .map(|watched| watched.entry)
        .filter(|entry| !entry.url.is_empty())
        .collect();
    let feed = feed
        .into_iter()
        .filter(|entry| !history::is_watched(&entry.id) && !dismissed.contains(&entry.id))
        .take(RECENT_FEED)
        .collect();
    let mut queue = pick(watched, HISTORY_PICKS);
    queue.extend(pick(feed, FEED_PICKS));
    if queue.is_empty() {
        return;
    }
    crate::log::info!("Looking for suggestions next to {} videos", queue.len());
    state.file.round = Round {
        started: now,
        total: queue.len(),
        queue,
        videos: HashMap::new(),
    };
    save_or_warn(&state.file);
}

/// The next video to ask about, unless a request is out or the last one was too recent. The
/// answer goes to `finish`.
pub fn begin() -> Option<Entry> {
    with(|state| {
        if state.asking || state.cooling_down {
            return None;
        }
        if state.file.round.queue.is_empty() {
            start_round(state);
        }
        let next = state.file.round.queue.last().cloned()?;
        state.asking = true;
        Some(next)
    })
}

/// Merges what was suggested next to the video, and saves. Once the round has asked about all
/// its videos, they replace the last round's, unless it found nothing, which is more likely to
/// be a failure than how things are.
pub fn finish(id: &str, result: Result<Vec<Entry>, String>) -> Result<(), String> {
    with(|state| {
        state.asking = false;
        state.cooling_down = true;

        let File {
            round, dismissed, ..
        } = &mut state.file;
        // Failed or not, it isn't asked about again: another round will pick others.
        if let Some(position) = round.queue.iter().rposition(|entry| entry.id == id) {
            round.queue.remove(position);
        }
        let merged = result.as_ref().map(|entries| {
            for (place, entry) in entries.iter().enumerate() {
                if history::is_watched(&entry.id) || dismissed.contains(&entry.id) {
                    continue;
                }
                round
                    .videos
                    .entry(entry.id.clone())
                    .and_modify(|video| {
                        video.count += 1;
                        video.best = video.best.min(place);
                    })
                    .or_insert_with(|| Suggested {
                        entry: entry.clone(),
                        count: 1,
                        best: place,
                    });
            }
        });

        if round.queue.is_empty() && !round.videos.is_empty() {
            state.file.videos = std::mem::take(&mut round.videos);
        }
        save(&state.file)?;
        merged.map_err(Clone::clone)
    })
}

/// Takes the video out of the suggestions, because it was watched.
pub fn watched(id: &str) {
    with(|state| {
        let round = state.file.round.videos.remove(id).is_some();
        if state.file.videos.remove(id).is_some() || round {
            save_or_warn(&state.file);
        }
    });
}

/// Takes the video out of the suggestions for good.
pub fn dismiss(id: &str) {
    with(|state| {
        state.file.round.videos.remove(id);
        state.file.videos.remove(id);
        state.file.dismissed.insert(id.to_string());
        save_or_warn(&state.file);
    });
}

/// The wait is over.
pub fn cooled_down() {
    with(|state| state.cooling_down = false);
}

/// The unwatched videos suggested, the ones suggested next to the most videos first, then the
/// ones suggested highest. Until a round is done, the one going on.
pub fn ranked() -> Vec<Entry> {
    with(|state| {
        let videos = if state.file.videos.is_empty() {
            &state.file.round.videos
        } else {
            &state.file.videos
        };
        let mut videos: Vec<&Suggested> = videos
            .values()
            .filter(|video| !history::is_watched(&video.entry.id))
            .collect();
        videos.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(a.best.cmp(&b.best))
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        videos
            .into_iter()
            .map(|video| video.entry.clone())
            .collect()
    })
}

/// How many videos of the round going on were asked about, and how many there are.
pub fn progress() -> (usize, usize) {
    with(|state| {
        let round = &state.file.round;
        (round.total - round.queue.len(), round.total)
    })
}

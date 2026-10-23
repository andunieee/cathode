//! The local subscriptions: channels followed without an account. They live in
//! `subscriptions.json` in the config directory of the app. The latest videos of each channel
//! are only kept in memory, they are asked for again on every start and every half hour.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::{Entry, LATEST_PER_CHANNEL};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    /// The language of the channel, taken from its latest video, which its titles are asked for
    /// in. YouTube has none for channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct State {
    list: Vec<Subscription>,
    /// The latest videos of each channel, newest first.
    latest: HashMap<String, Vec<Entry>>,
    /// Channels whose latest videos were asked for and haven't come yet.
    pending: HashSet<String>,
    /// Why the last attempt failed, for the channels it did.
    failed: HashMap<String, String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cathode")
        .join("subscriptions.json")
}

fn save(list: &[Subscription]) -> Result<(), String> {
    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    // Written aside and renamed so a crash can't leave a half-written file.
    let temporary = path.with_extension("json.tmp");
    let file = File {
        subscriptions: list.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

pub fn init() {
    let path = path();
    let list = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<File>(&bytes)
            .map(|file| file.subscriptions)
            .unwrap_or_else(|error| {
                crate::log::warning!("Cannot read {}: {error}", path.display());
                Vec::new()
            }),
        Err(_) => Vec::new(),
    };
    with(|state| state.list = list);
}

pub fn list() -> Vec<Subscription> {
    with(|state| state.list.clone())
}

pub fn is_subscribed(id: &str) -> bool {
    with(|state| state.list.iter().any(|s| s.id == id))
}

/// Keeps the language found for a channel.
pub fn set_language(id: &str, language: String) {
    with(|state| {
        if let Some(subscription) = state.list.iter_mut().find(|s| s.id == id) {
            subscription.language = Some(language);
            if let Err(error) = save(&state.list) {
                crate::log::warning!("Cannot save the subscriptions: {error}");
            }
        }
    });
}

pub fn language(id: &str) -> Option<String> {
    with(|state| state.list.iter().find(|s| s.id == id)?.language.clone())
}

/// Does nothing if the channel is already there.
pub fn subscribe(subscription: Subscription) -> Result<(), String> {
    with(|state| {
        if state.list.iter().any(|s| s.id == subscription.id) {
            return Ok(());
        }
        state.list.push(subscription);
        save(&state.list)
    })
}

pub fn unsubscribe(id: &str) -> Result<(), String> {
    with(|state| {
        state.list.retain(|s| s.id != id);
        state.latest.remove(id);
        state.pending.remove(id);
        state.failed.remove(id);
        save(&state.list)
    })
}

/// Notes that the latest videos of the channel are being asked for. False if that is already
/// going on.
pub fn mark_pending(id: &str) -> bool {
    with(|state| state.pending.insert(id.to_string()))
}

pub fn pending_count() -> usize {
    with(|state| state.pending.len())
}

/// Takes the newest videos of a channel, which may come from its page as well as from a refresh.
pub fn set_latest(id: &str, mut entries: Vec<Entry>) {
    with(|state| {
        state.pending.remove(id);
        let Some(subscription) = state.list.iter().find(|s| s.id == id) else {
            return;
        };

        entries.truncate(LATEST_PER_CHANNEL);
        for entry in &mut entries {
            if entry.channel.is_empty() {
                entry.channel = subscription.name.clone();
            }
            if entry.channel_id.is_empty() {
                entry.channel_id = id.to_string();
            }
        }
        state.failed.remove(id);
        state.latest.insert(id.to_string(), entries);
    });
}

pub fn set_failed(id: &str, error: String) {
    with(|state| {
        state.pending.remove(id);
        if state.list.iter().any(|s| s.id == id) {
            state.failed.insert(id.to_string(), error);
        }
    });
}

/// How many channels could not be refreshed, and why for one of them.
pub fn failures() -> (usize, Option<String>) {
    with(|state| (state.failed.len(), state.failed.values().next().cloned()))
}

/// The latest videos of every channel, newest first. The dates of listings are only approximate
/// (they are worked out from "3 days ago"), so videos of the same day stay in the order of
/// their channel's listing.
pub fn feed() -> Vec<Entry> {
    with(|state| {
        let mut entries: Vec<Entry> = state
            .list
            .iter()
            .filter_map(|s| state.latest.get(&s.id))
            .flatten()
            .cloned()
            .collect();
        entries.sort_by(|a, b| b.published.cmp(&a.published));
        entries
    })
}

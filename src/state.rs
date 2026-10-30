//! What the app remembers between runs but nobody sets on purpose, like which sidebars are open.
//! It lives in `state.json` in the data directory of the app, not in the config one: it is fine to
//! lose and not worth carrying from one machine to another.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub queue_sidebar_open: bool,
    pub subscriptions_sidebar_open: bool,
    /// The share of the window the queue sidebar was dragged to, none until it is.
    pub queue_sidebar_fraction: Option<f64>,
    /// The same, for when it floats over the page.
    pub queue_overlay_fraction: Option<f64>,
    /// Whether the restricted mode is on.
    pub restricted: bool,
    /// The collection the Save button saves to.
    pub save_collection: Option<String>,
    /// The volume from 0 to 1, none until it is changed.
    pub volume: Option<f64>,
    /// The playback speed, none until it is changed.
    pub speed: Option<f64>,
    /// Which captions are downloaded and shown. It was called `subtitle_mode`.
    #[serde(alias = "subtitle_mode")]
    pub caption_mode: CaptionMode,
    /// The queue and the player of the normal mode.
    pub session: Session,
    /// The same, for the restricted mode.
    pub restricted_session: Session,
}

/// Which captions the player goes for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptionMode {
    #[default]
    Off = 0,
    /// The language the video was made in.
    Original = 1,
    /// The default language of the settings.
    Default = 2,
}

/// What the queue and the player were up to, each mode has its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// The videos in the queue, in order.
    pub queue: Vec<Entry>,
    /// The video in the player, which comes back paused.
    pub playing: Option<Entry>,
    /// Where the videos of the queue were left off.
    pub positions: HashMap<String, f64>,
}

impl State {
    pub fn session_mut(&mut self, restricted: bool) -> &mut Session {
        if restricted {
            &mut self.restricted_session
        } else {
            &mut self.session
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn path() -> PathBuf {
    crate::paths::data().join("state.json")
}

pub fn init() {
    let state = std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    STATE.with(|slot| *slot.borrow_mut() = state);
}

pub fn get() -> State {
    STATE.with(|state| state.borrow().clone())
}

/// Changes the state and writes it to disk.
pub fn update(change: impl FnOnce(&mut State)) -> Result<(), String> {
    let state = STATE.with(|state| {
        let mut state = state.borrow_mut();
        change(&mut state);
        state.clone()
    });

    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

//! What the app remembers between runs but nobody sets on purpose, like which sidebars are open.
//! It lives in `state.json` in the data directory of the app, not in the config one: it is fine to
//! lose and not worth carrying from one machine to another.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub queue_sidebar_open: bool,
    pub subscriptions_sidebar_open: bool,
    /// Whether the restricted mode is on.
    pub restricted: bool,
    /// The collection the Save button saves to.
    pub save_collection: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lumpla")
        .join("state.json")
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

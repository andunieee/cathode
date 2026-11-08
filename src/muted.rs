//! Channels muted and videos hidden: their videos are left out of every list of videos the app
//! shows, from search results to the recommendations. They live in `state.json` in the data
//! directory of the app, and are kept in memory here for the lists to sift through quickly.

use std::cell::RefCell;

use crate::fetch::Entry;
use crate::state::{self, MutedChannel};

#[derive(Default, Clone)]
struct Muted {
    channels: Vec<MutedChannel>,
    videos: Vec<Entry>,
}

impl Muted {
    /// Whether the video is left out of the lists: it was hidden itself, or its channel was
    /// muted.
    fn hides(&self, entry: &Entry) -> bool {
        self.videos.iter().any(|video| video.id == entry.id)
            || (!entry.channel_id.is_empty()
                && self
                    .channels
                    .iter()
                    .any(|channel| channel.id == entry.channel_id))
    }
}

thread_local! {
    static MUTED: RefCell<Option<Muted>> = const { RefCell::new(None) };
}

/// Reads the muted channels and hidden videos from the state, once it is loaded.
pub fn init() {
    MUTED.with(|slot| *slot.borrow_mut() = Some(load()));
}

/// What is muted and hidden now, loaded from the state the first time it is asked for.
fn load() -> Muted {
    let state = state::get();
    Muted {
        channels: state.muted_channels,
        videos: state.hidden_videos,
    }
}

fn read<R>(f: impl FnOnce(&Muted) -> R) -> R {
    MUTED.with(|slot| {
        let mut slot = slot.borrow_mut();
        f(slot.get_or_insert_with(load))
    })
}

fn current() -> Muted {
    read(Muted::clone)
}

/// Changes what is muted and hidden, and writes it to the state on disk. Nothing is changed if
/// it cannot be saved.
fn update(change: impl FnOnce(&mut Muted)) -> Result<(), String> {
    let mut muted = current();
    change(&mut muted);

    let channels = muted.channels.clone();
    let videos = muted.videos.clone();
    state::update(move |state| {
        state.muted_channels = channels;
        state.hidden_videos = videos;
    })?;

    MUTED.with(|slot| *slot.borrow_mut() = Some(muted));
    Ok(())
}

/// Whether the video is left out of the lists: it was hidden itself, or its channel was muted.
pub fn hides(entry: &Entry) -> bool {
    read(|muted| muted.hides(entry))
}

pub fn channel_muted(id: &str) -> bool {
    !id.is_empty() && read(|muted| muted.channels.iter().any(|channel| channel.id == id))
}

pub fn video_hidden(id: &str) -> bool {
    read(|muted| muted.videos.iter().any(|video| video.id == id))
}

pub fn channels() -> Vec<MutedChannel> {
    read(|muted| muted.channels.clone())
}

pub fn videos() -> Vec<Entry> {
    read(|muted| muted.videos.clone())
}

/// Mutes a channel, leaving its videos out of the lists.
pub fn mute_channel(id: &str, name: &str) -> Result<(), String> {
    if id.is_empty() {
        return Ok(());
    }
    update(|muted| {
        if !muted.channels.iter().any(|channel| channel.id == id) {
            muted.channels.push(MutedChannel {
                id: id.to_string(),
                name: name.to_string(),
            });
        }
    })
}

pub fn unmute_channel(id: &str) -> Result<(), String> {
    update(|muted| muted.channels.retain(|channel| channel.id != id))
}

/// Hides a video, leaving it out of the lists.
pub fn hide_video(entry: &Entry) -> Result<(), String> {
    if entry.id.is_empty() {
        return Ok(());
    }
    update(|muted| {
        if !muted.videos.iter().any(|video| video.id == entry.id) {
            muted.videos.push(entry.clone());
        }
    })
}

pub fn unhide_video(id: &str) -> Result<(), String> {
    update(|muted| muted.videos.retain(|video| video.id != id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, channel_id: &str) -> Entry {
        Entry {
            id: id.to_string(),
            url: String::new(),
            title: String::new(),
            channel: String::new(),
            channel_id: channel_id.to_string(),
            duration: None,
            views: None,
            published: None,
        }
    }

    #[test]
    fn muted_channels_and_hidden_videos_are_left_out() {
        let muted = Muted {
            channels: vec![MutedChannel {
                id: "channel".to_string(),
                name: "A channel".to_string(),
            }],
            videos: vec![entry("hidden", "elsewhere")],
        };

        assert!(muted.hides(&entry("video", "channel")));
        assert!(muted.hides(&entry("hidden", "elsewhere")));
        assert!(!muted.hides(&entry("video", "other")));
        // A video without a channel is only hidden when it is itself.
        assert!(!muted.hides(&entry("video", "")));
    }
}

//! The restricted mode: the only videos, channels, playlists and collections that can be
//! reached while it is on, and the optional password protecting it. It lives in `restricted/default.json` in the
//! config directory of the app.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::collections;
use crate::fetch::Entry;

const HASH_ROUNDS: u32 = 100_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub password_hash: Option<String>,
    pub channels: Vec<ChannelRef>,
    pub playlists: Vec<PlaylistRef>,
    pub videos: Vec<Entry>,
    /// Names of collections.
    pub collections: Vec<String>,
}

/// Something that can be allowed in the profile.
#[derive(Debug, Clone)]
pub enum Item {
    Video(Entry),
    Channel(ChannelRef),
    Playlist(PlaylistRef),
    Collection(String),
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cathode")
        .join("restricted")
        .join("default.json")
}

impl Profile {
    fn load() -> Self {
        let path = path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => return Self::default(),
        };

        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            // Better to lock everything than to let a broken file switch the lock off.
            eprintln!("Cannot read {}: {error}", path.display());
            BROKEN.with(|broken| broken.set(true));
            Self::default()
        })
    }

    fn save(&self) -> Result<(), String> {
        let path = path();
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

        // Written aside and renamed so a crash can't leave a half-written file.
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(temporary, path).map_err(|e| e.to_string())
    }
}

thread_local! {
    /// The file could not be read, which keeps the restricted mode on.
    static BROKEN: Cell<bool> = Cell::new(false);
    static PROFILE: RefCell<Profile> = RefCell::new(Profile::default());
    /// Videos seen in the listings of allowed channels and playlists.
    static LISTED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

pub fn init() {
    PROFILE.with(|profile| *profile.borrow_mut() = Profile::load());
}

pub fn read<R>(f: impl FnOnce(&Profile) -> R) -> R {
    PROFILE.with(|profile| f(&profile.borrow()))
}

/// Changes the profile and writes it to disk.
pub fn update(f: impl FnOnce(&mut Profile)) -> Result<(), String> {
    PROFILE.with(|profile| {
        let mut profile = profile.borrow_mut();
        f(&mut profile);
        profile.save()
    })
}

/// Whether the restricted mode is on. Whether it is on lives in the state of the app, not in
/// the profile.
pub fn restricted() -> bool {
    BROKEN.with(Cell::get) || crate::state::get().restricted
}

pub fn set_restricted(restricted: bool) -> Result<(), String> {
    crate::state::update(|state| state.restricted = restricted)
}

pub fn allows_channel(id: &str) -> bool {
    !id.is_empty() && read(|profile| profile.channels.iter().any(|c| c.id == id))
}

pub fn allows_playlist(id: &str) -> bool {
    read(|profile| profile.playlists.iter().any(|p| p.id == id))
}

pub fn allows_collection(name: &str) -> bool {
    read(|profile| profile.collections.iter().any(|c| c == name))
}

pub fn lists_video(id: &str) -> bool {
    read(|profile| profile.videos.iter().any(|v| v.id == id))
}

/// Whether a video can be watched: it is allowed itself, or it comes from an allowed channel,
/// playlist or collection.
pub fn allows_video(entry: &Entry) -> bool {
    lists_video(&entry.id)
        || allows_channel(&entry.channel_id)
        || read(|profile| {
            profile
                .collections
                .iter()
                .any(|name| collections::contains(name, &entry.id))
        })
        || LISTED.with(|listed| listed.borrow().contains(&entry.id))
}

pub fn remember_listed(entries: &[Entry]) {
    LISTED.with(|listed| {
        listed
            .borrow_mut()
            .extend(entries.iter().map(|entry| entry.id.clone()))
    });
}

pub fn forget_listed() {
    LISTED.with(|listed| listed.borrow_mut().clear());
}

/// Adds the item to the profile, returns false if it was already there.
pub fn add(item: Item) -> Result<bool, String> {
    let mut added = true;
    update(|profile| match item {
        Item::Video(entry) if profile.videos.iter().any(|v| v.id == entry.id) => added = false,
        Item::Video(entry) => profile.videos.push(entry),
        Item::Channel(channel) if profile.channels.iter().any(|c| c.id == channel.id) => {
            added = false
        }
        Item::Channel(channel) => profile.channels.push(channel),
        Item::Playlist(playlist) if profile.playlists.iter().any(|p| p.id == playlist.id) => {
            added = false
        }
        Item::Playlist(playlist) => profile.playlists.push(playlist),
        Item::Collection(name) if profile.collections.contains(&name) => added = false,
        Item::Collection(name) => profile.collections.push(name),
    })?;
    Ok(added)
}

// Password

pub fn has_password() -> bool {
    read(|profile| profile.password_hash.is_some())
}

pub fn set_password(password: Option<&str>) -> Result<(), String> {
    let hash = password.map(hash_password);
    update(|profile| profile.password_hash = hash)
}

/// True if there is no password at all, or it is this one.
pub fn check_password(password: &str) -> bool {
    read(|profile| match &profile.password_hash {
        Some(hash) => verify_password(hash, password),
        None => true,
    })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

fn derive(salt: &[u8], password: &str, rounds: u32) -> Vec<u8> {
    let mut hash = Sha256::new()
        .chain_update(salt)
        .chain_update(password.as_bytes())
        .finalize();
    for _ in 1..rounds {
        hash = Sha256::new()
            .chain_update(hash)
            .chain_update(salt)
            .chain_update(password.as_bytes())
            .finalize();
    }
    hash.to_vec()
}

fn random_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    if getrandom::fill(&mut salt).is_err() {
        // No randomness to be had, the clock will have to do.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        salt.copy_from_slice(&Sha256::digest(now.to_le_bytes())[..16]);
    }
    salt
}

/// "sha256$<rounds>$<salt>$<hash>"
fn hash_password(password: &str) -> String {
    let salt = random_salt();
    format!(
        "sha256${HASH_ROUNDS}${}${}",
        to_hex(&salt),
        to_hex(&derive(&salt, password, HASH_ROUNDS))
    )
}

fn verify_password(hash: &str, password: &str) -> bool {
    let parts: Vec<&str> = hash.split('$').collect();
    let [algorithm, rounds, salt, expected] = parts[..] else {
        return false;
    };
    let (Ok(rounds), Some(salt), Some(expected)) =
        (rounds.parse::<u32>(), from_hex(salt), from_hex(expected))
    else {
        return false;
    };

    algorithm == "sha256" && rounds > 0 && derive(&salt, password, rounds) == expected
}

// What can be typed in to add something

/// What was typed in, as the address yt-dlp can look it up with.
#[derive(Debug, Clone)]
pub enum Input {
    Video(String),
    Channel(String),
    Playlist(String),
}

fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn video(id: &str) -> Option<Input> {
    is_id(id).then(|| Input::Video(format!("https://www.youtube.com/watch?v={id}")))
}

fn playlist(id: &str) -> Option<Input> {
    is_id(id).then(|| Input::Playlist(format!("https://www.youtube.com/playlist?list={id}")))
}

fn channel(path: &str) -> Option<Input> {
    Some(Input::Channel(format!("https://www.youtube.com/{path}")))
}

/// Understands links to videos, channels and playlists, and handles and ids on their own.
pub fn parse_input(input: &str) -> Option<Input> {
    let input = input.trim();
    if input.is_empty() || input.contains(char::is_whitespace) {
        return None;
    }

    if input.starts_with('@') {
        return channel(input);
    }
    if input.len() == 24 && input.starts_with("UC") && is_id(input) {
        return channel(&format!("channel/{input}"));
    }
    if input.len() > 11
        && ["PL", "UU", "OL", "FL"]
            .iter()
            .any(|p| input.starts_with(p))
        && is_id(input)
    {
        return playlist(input);
    }
    if input.len() == 11 && is_id(input) {
        return video(input);
    }

    let url = reqwest::Url::parse(input)
        .or_else(|_| reqwest::Url::parse(&format!("https://{input}")))
        .ok()?;
    let host = url.host_str()?;
    let host = host
        .strip_prefix("www.")
        .or(host.strip_prefix("m."))
        .unwrap_or(host);
    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());

    match host {
        "youtu.be" => video(segments.next()?),
        "youtube.com" | "music.youtube.com" => match segments.next()? {
            "watch" => match (query("v"), query("list")) {
                (Some(v), _) => video(&v),
                (None, Some(list)) => playlist(&list),
                _ => None,
            },
            "playlist" => playlist(&query("list")?),
            "shorts" | "embed" | "live" => video(segments.next()?),
            "channel" => channel(&format!("channel/{}", segments.next()?)),
            handle if handle.starts_with('@') => channel(handle),
            kind @ ("c" | "user") => channel(&format!("{kind}/{}", segments.next()?)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(input: &str) -> Option<String> {
        parse_input(input).map(|input| match input {
            Input::Video(url) => format!("video {url}"),
            Input::Channel(url) => format!("channel {url}"),
            Input::Playlist(url) => format!("playlist {url}"),
        })
    }

    #[test]
    fn videos() {
        let expected = Some("video https://www.youtube.com/watch?v=jNQXAC9IVRw".to_string());
        for input in [
            "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            "https://www.youtube.com/watch?v=jNQXAC9IVRw&list=PL1234567890abc",
            "https://youtu.be/jNQXAC9IVRw?t=3",
            "https://m.youtube.com/shorts/jNQXAC9IVRw",
            "youtube.com/watch?v=jNQXAC9IVRw",
            "jNQXAC9IVRw",
        ] {
            assert_eq!(kind(input), expected, "{input}");
        }
    }

    #[test]
    fn channels() {
        assert_eq!(
            kind("https://www.youtube.com/@LinusTechTips/videos"),
            Some("channel https://www.youtube.com/@LinusTechTips".to_string())
        );
        assert_eq!(
            kind("@LinusTechTips"),
            Some("channel https://www.youtube.com/@LinusTechTips".to_string())
        );
        let id = "UCXuqSBlHAE6Xw-yeJA0Tunw";
        let expected = Some(format!("channel https://www.youtube.com/channel/{id}"));
        assert_eq!(kind(id), expected);
        assert_eq!(
            kind(&format!("https://www.youtube.com/channel/{id}/videos")),
            expected
        );
        assert_eq!(
            kind("https://www.youtube.com/c/SomeName"),
            Some("channel https://www.youtube.com/c/SomeName".to_string())
        );
    }

    #[test]
    fn playlists() {
        let expected = Some(
            "playlist https://www.youtube.com/playlist?list=PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf"
                .to_string(),
        );
        assert_eq!(
            kind("https://www.youtube.com/playlist?list=PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf"),
            expected
        );
        assert_eq!(kind("PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf"), expected);
        assert_eq!(
            kind("https://www.youtube.com/watch?list=PLrAXtmErZgOeiKm4sgNOknGvNjby9efdf"),
            expected
        );
    }

    #[test]
    fn rejects_the_rest() {
        for input in [
            "",
            "   ",
            "hello world",
            "https://example.com/watch?v=jNQXAC9IVRw",
            "https://www.youtube.com/feed/trending",
            "nope",
        ] {
            assert_eq!(kind(input), None, "{input}");
        }
    }

    #[test]
    fn passwords() {
        let hash = hash_password("correct horse");
        assert!(verify_password(&hash, "correct horse"));
        assert!(!verify_password(&hash, "wrong"));
        assert!(!verify_password(&hash, ""));
        // Salted: the same password does not always give the same hash.
        assert_ne!(hash, hash_password("correct horse"));
        assert!(!verify_password("garbage", "x"));
        assert!(!verify_password("sha256$0$00$00", "x"));
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let profile: Profile = serde_json::from_str("{}").unwrap();
        assert!(profile.channels.is_empty());

        let profile: Profile = serde_json::from_str(
            r#"{"channels": [{"id": "yt:UC1", "name": "A"}]}"#,
        )
        .unwrap();
        assert!(profile.channels[0].id == "yt:UC1");
    }
}

//! The social recommendations: videos the people followed recommend, as Nostr events of kind
//! 1032, each one a video:
//!
//! ```json
//! {
//!   "kind": 1032,
//!   "content": "What the video is about",
//!   "tags": [
//!     ["i", "dQw4w9WgXcQ"],
//!     ["title", "Never Gonna Give You Up"],
//!     ["thumb", "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"],
//!     ["duration", "213"],
//!     ["channel", "Rick Astley"],
//!     ["channel_id", "UCuAXFkgsw1L7xaCfnd5JJOw"],
//!     ["published", "1256453052"],
//!     ["t", "rickroll"],
//!     ["l", "Music performance & songs"]
//!   ]
//! }
//! ```
//!
//! `published` is when the video came out, in seconds since the epoch. The `t` tags are its
//! hashtags and the `l` tags its categories, any number of each, which the recommendations can
//! be narrowed down by. `examples/publish_videos.rs` publishes a list of videos this way.
//!
//! Who the user is and who they follow live in `social.json` in the config directory of the app.
//! The people followed are filled in with a few to start with the first time the recommendations
//! are turned on, and from then on are only what that file says. Each person's recommendations are
//! looked for only on the relays they say they write to, in their relay list as the indexers have
//! it. Their names and pictures come from their metadata, kept with the events. Opening the page
//! of a person asks those relays for their latest recommendations, followed or not, and the page
//! shows the latest 20 of them.
//!
//! The events are kept in `social.redb` in the data directory, and each followed person is
//! synced on their own: `social_sync.json` there keeps up to when each one was downloaded, so
//! the next sync asks only for what came after. Their deletions (kind 5 with a `k` tag of 1032)
//! are downloaded too, and take the events they point at out of the database.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use ritualistic::{
    EventDatabase, Filter, ID, Kind, Network, PubKey, RedbEventDatabase, SecretKey,
    SubscriptionOptions, TagQuery, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::fetch::Entry;
use crate::history;

const METADATA: Kind = Kind(0);
const RECOMMENDATION: Kind = Kind(1032);
const DELETION: Kind = Kind(5);
const RELAY_LIST: Kind = Kind(10002);

/// Who is followed to start with, the first time the recommendations are turned on.
const DEFAULT_FOLLOWS: [&str; 2] = [
    "be8d81b9f7f35bcd9eda92625bb1225c207535c5eb13b0fb72bbb0f740f23048",
    "738b7e8d7741fe3175c63178c50cc6f7ae7ef0f69e037736463d0c4a7394698b",
];
/// Where the lists of relays people write to are looked for.
const INDEXERS: [&str; 5] = [
    "wss://purplepag.es",
    "wss://nos.lol",
    "wss://indexer.coracle.social",
    "wss://relay.primal.net",
    "wss://relay.ditto.pub",
];
/// How many of the relays a person writes to are asked.
const OUTBOX_RELAYS: usize = 4;
/// How many of the latest recommendations the page of a person shows.
const LATEST_RECOMMENDATIONS: usize = 20;

/// A person synced more recently than this isn't synced again.
const SYNC_AGAIN_SECONDS: u32 = 10 * 60;
/// How far back from the last sync the next one starts, for the events that took a while to
/// reach the relays.
const OVERLAP_SECONDS: u32 = 10 * 60;
/// How long the relays get to answer.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const QUERY_WAIT: Duration = Duration::from_secs(30);
/// How long the metadata of a person gets to arrive.
const METADATA_WAIT: Duration = Duration::from_secs(30);
/// How many events a relay is asked for at once.
const PAGE_EVENTS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The name the user goes by.
    pub name: String,
    /// The Nostr secret key of the user, in hex.
    pub secret_key: String,
    /// The public keys of the people whose recommendations are shown, in hex. None until the
    /// recommendations are first turned on.
    pub follows: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: String::new(),
            secret_key: SecretKey::generate().to_hex(),
            follows: None,
        }
    }
}

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

fn config_path() -> PathBuf {
    crate::paths::config().join("social.json")
}

fn write_json(path: &PathBuf, value: &impl Serialize) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    // Written aside and renamed so a crash can't leave a half-written file.
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

fn load() -> Config {
    let path = config_path();
    if let Ok(bytes) = std::fs::read(&path) {
        match serde_json::from_slice::<Config>(&bytes) {
            Ok(config) if config.secret_key.parse::<SecretKey>().is_ok() => return config,
            Ok(_) => crate::log::warning!("The secret key in {} is not valid", path.display()),
            Err(error) => crate::log::warning!("Cannot read {}: {error}", path.display()),
        }
        // A broken file is left for the user to look at, nothing is written over it.
        return Config::default();
    }
    // The key generated has to stay the same from then on.
    let config = Config::default();
    if let Err(error) = write_json(&path, &config) {
        crate::log::warning!("Cannot save {}: {error}", path.display());
    }
    config
}

pub fn config() -> Config {
    if let Some(config) = CONFIG.read().unwrap().as_ref() {
        return config.clone();
    }
    let mut guard = CONFIG.write().unwrap();
    guard.get_or_insert_with(load).clone()
}

/// Changes the config and writes it to disk.
pub fn update(change: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut config = config();
    change(&mut config);
    write_json(&config_path(), &config)?;
    *CONFIG.write().unwrap() = Some(config);
    Ok(())
}

/// The public key of the user, in hex.
pub fn public_key() -> String {
    config()
        .secret_key
        .parse::<SecretKey>()
        .map(|key| key.pubkey().to_hex())
        .unwrap_or_default()
}

/// Makes `text`, in hex or as an `nsec`, the secret key of the user. Answers with the public
/// key that goes with it.
pub fn set_secret_key(text: &str) -> Result<String, String> {
    let key = text
        .trim()
        .parse::<SecretKey>()
        .map_err(|error| error.to_string())?;
    update(|config| config.secret_key = key.to_hex())?;
    Ok(key.pubkey().to_hex())
}

/// Starts following the default people, unless the user has a list of people followed already,
/// even an empty one.
pub fn ensure_follows() -> Result<(), String> {
    if config().follows.is_some() {
        return Ok(());
    }
    update(|config| {
        config
            .follows
            .get_or_insert_with(|| DEFAULT_FOLLOWS.map(String::from).to_vec());
    })
}

/// The people followed, as the config has them.
pub fn follows() -> Vec<PubKey> {
    config()
        .follows
        .unwrap_or_default()
        .iter()
        .filter_map(|key| key.parse().ok())
        .collect()
}

/// Whether the recommendations of the person are followed.
pub fn is_followed(pubkey: PubKey) -> bool {
    follows().contains(&pubkey)
}

/// Starts or stops following the recommendations of the person.
pub fn set_followed(pubkey: PubKey, followed: bool) -> Result<(), String> {
    let hex = pubkey.to_hex();
    update(|config| {
        let follows = config.follows.get_or_insert_with(Vec::new);
        follows.retain(|key| key.parse::<PubKey>().ok() != Some(pubkey));
        if followed {
            follows.push(hex);
        }
    })?;
    RANKED.set(None);
    Ok(())
}

// The database

static DATABASE: OnceLock<Result<RedbEventDatabase, String>> = OnceLock::new();

fn database() -> Result<&'static RedbEventDatabase, String> {
    DATABASE
        .get_or_init(|| {
            let path = crate::paths::data().join("social.redb");
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            RedbEventDatabase::open(&path)
                .map_err(|error| format!("cannot open {}: {error}", path.display()))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn event_of(archived: &ritualistic::ArchivedEvent) -> Option<ritualistic::Event> {
    rkyv::deserialize::<ritualistic::Event, rkyv::rancor::Error>(archived).ok()
}

/// Whether a deletion of the event by its author is in the database.
fn is_deleted(db: &RedbEventDatabase, event: &ritualistic::Event) -> Result<bool, String> {
    let mut filters = [Filter {
        kinds: Some(vec![DELETION]),
        authors: Some(vec![event.pubkey]),
        tags: Some(vec![TagQuery("e".to_string(), vec![event.id.to_hex()])]),
        limit: Some(1),
        ..Filter::default()
    }];
    Ok(db
        .query_events(&mut filters)
        .map_err(|e| e.to_string())?
        .next()
        .is_some())
}

/// Saves the deletion, and takes the recommendations it points at out of the database. Answers
/// with how many were.
fn apply_deletion(db: &RedbEventDatabase, deletion: &ritualistic::Event) -> Result<usize, String> {
    save(db, deletion)?;
    let ids: Vec<ID> = deletion
        .tags
        .find_all("e")
        .filter_map(|tag| ID::from_hex(&tag[1]).ok())
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut filters = [Filter {
        ids: Some(ids),
        ..Filter::default()
    }];
    // Only the author of an event can delete it.
    let doomed: Vec<ID> = db
        .query_events(&mut filters)
        .map_err(|e| e.to_string())?
        .filter_map(|archived| event_of(&archived))
        .filter(|event| event.pubkey == deletion.pubkey && event.kind == RECOMMENDATION)
        .map(|event| event.id)
        .collect();
    for id in &doomed {
        db.delete_event(id.short()).map_err(|e| e.to_string())?;
    }
    Ok(doomed.len())
}

/// Saves the event, answers whether it wasn't there already.
fn save(db: &RedbEventDatabase, event: &ritualistic::Event) -> Result<bool, String> {
    match db.save_event(event) {
        Ok(()) => Ok(true),
        Err(ritualistic::database::DatabaseError::DuplicateEvent) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

// Syncing, in the worker threads

/// What a sync of one person did.
#[derive(Debug)]
pub struct Synced {
    /// Up to when the events were downloaded, where the next sync starts from.
    pub until: u32,
    pub added: usize,
    pub deleted: usize,
}

/// The events matching `filter` on the relays, and whether every relay answered in time.
async fn collect(
    network: &Network,
    relays: &[String],
    filter: Filter,
) -> (Vec<ritualistic::Event>, bool) {
    let mut options = SubscriptionOptions::default();
    options.label = Some("cathode".to_string());
    let mut occurrences = network.subscribe(relays, filter, options).await;
    let mut events = Vec::new();
    let complete = tokio::time::timeout(QUERY_WAIT, async {
        while let Some(occurrence) = occurrences.recv().await {
            match occurrence {
                ritualistic::Occurrence::Event(event, _) => events.push(*event),
                _ => break,
            }
        }
    })
    .await
    .is_ok();
    (events, complete)
}

/// The events matching `filter` on the relay, and whether it answered in time. Relays send only
/// so many events for a query, so they're asked a page at a time, each going back from the oldest
/// event of the one before.
async fn collect_paged(
    network: Network,
    relay: String,
    filter: Filter,
) -> (Vec<ritualistic::Event>, bool) {
    let relays = [relay];
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    let mut until = filter.until;
    loop {
        let page = Filter {
            until,
            limit: Some(PAGE_EVENTS),
            ..filter.clone()
        };
        let (found, complete) = collect(&network, &relays, page).await;
        if !complete {
            return (events, false);
        }
        let oldest = found.iter().map(|event| event.created_at).min();
        let before = events.len();
        events.extend(found.into_iter().filter(|event| seen.insert(event.id)));
        // The page starts at the second the last one ended, so it's over once only what was
        // already seen comes.
        match oldest {
            Some(oldest) if events.len() > before => until = Some(oldest),
            _ => return (events, true),
        }
    }
}

/// The events matching `filter` on each of the relays, and whether every one answered in time.
async fn collect_all(
    network: &Network,
    relays: &[String],
    filter: Filter,
) -> (Vec<ritualistic::Event>, bool) {
    let mut queries = tokio::task::JoinSet::new();
    for relay in relays {
        queries.spawn(collect_paged(
            network.clone(),
            relay.clone(),
            filter.clone(),
        ));
    }
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    let mut complete = true;
    while let Some(result) = queries.join_next().await {
        let Ok((found, answered)) = result else {
            complete = false;
            continue;
        };
        complete &= answered;
        events.extend(found.into_iter().filter(|event| seen.insert(event.id)));
    }
    (events, complete)
}

/// The relays that answer, among `relays`.
async fn reachable(network: &Network, relays: Vec<String>) -> Vec<String> {
    let mut tries = tokio::task::JoinSet::new();
    for relay in relays {
        let network = network.clone();
        tries.spawn(async move {
            let connected = tokio::time::timeout(CONNECT_WAIT, network.ensure_relay(&relay)).await;
            matches!(connected, Ok(Ok(_))).then_some(relay)
        });
    }
    let mut reachable = Vec::new();
    while let Some(result) = tries.join_next().await {
        if let Ok(Some(relay)) = result {
            reachable.push(relay);
        }
    }
    reachable
}

/// The relays the person says they write to, in the relay list the indexers have of them.
async fn relays_of(network: &Network, author: PubKey) -> Vec<String> {
    let indexers = INDEXERS.map(String::from);
    let filter = Filter {
        kinds: Some(vec![RELAY_LIST]),
        authors: Some(vec![author]),
        limit: Some(1),
        ..Filter::default()
    };
    let (lists, _) = collect(network, &indexers, filter).await;
    let newest = lists
        .into_iter()
        .filter(|list| list.pubkey == author && list.verify_signature())
        .max_by_key(|list| list.created_at);
    // Kept to show on the page of the person.
    if let (Some(list), Ok(db)) = (&newest, database())
        && let Err(error) = db.replace_event(list, false)
    {
        crate::log::warning!("Cannot save the relay list of {}: {error}", author.to_hex());
    }

    match &newest {
        Some(list) => write_relays_of(list),
        None => Vec::new(),
    }
}

/// The relays in the relay list that the person writes to, normalized, without duplicates, and
/// only as many as are asked.
fn write_relays_of(list: &ritualistic::Event) -> Vec<String> {
    let mut relays = Vec::new();
    let mut seen = HashSet::new();
    for relay in list
        .tags
        .find_all("r")
        .filter(|tag| tag.get(2).is_none_or(|marker| marker == "write"))
        .map(|tag| tag[1].clone())
        .take(OUTBOX_RELAYS)
    {
        if let Ok(url) = ritualistic::normalize_url(&relay)
            && seen.insert(url.to_string())
        {
            relays.push(url.to_string());
        }
    }
    relays
}

/// The relays the person writes to, from their relay list in the database, if it has one.
fn saved_write_relays(pubkey: PubKey) -> Option<Vec<String>> {
    newest_of(pubkey, RELAY_LIST).map(|list| write_relays_of(&list))
}

/// Both of the TLS backends of rustls end up built in, the relay connections can't pick one by
/// themselves.
fn pick_tls() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Downloads the recommendations of the person and their deletions of them, from `since` on.
pub async fn sync(network: &Network, pubkey: &str, since: u32) -> Result<Synced, String> {
    pick_tls();

    let author: PubKey = pubkey.parse().map_err(|error| format!("{error}"))?;
    let until = Timestamp::now().0;

    let relays = relays_of(network, author).await;
    if relays.is_empty() {
        return Err("no relay list was found".to_string());
    }
    let relays = reachable(network, relays).await;
    if relays.is_empty() {
        return Err("none of the relays could be reached".to_string());
    }

    let since = (since > 0).then(|| Timestamp(since.saturating_sub(OVERLAP_SECONDS)));
    let recommendations = Filter {
        kinds: Some(vec![RECOMMENDATION]),
        authors: Some(vec![author]),
        since,
        ..Filter::default()
    };
    let deletions = Filter {
        kinds: Some(vec![DELETION]),
        authors: Some(vec![author]),
        tags: Some(vec![TagQuery(
            "k".to_string(),
            vec![RECOMMENDATION.0.to_string()],
        )]),
        since,
        ..Filter::default()
    };
    let ((recommendations, complete), (deletions, deletions_complete)) = tokio::join!(
        collect_all(network, &relays, recommendations),
        collect_all(network, &relays, deletions),
    );

    let db = database()?;
    let genuine = |event: &ritualistic::Event, kind: Kind| {
        event.pubkey == author && event.kind == kind && event.verify_signature()
    };
    let mut deleted = 0;
    for deletion in deletions.iter().filter(|event| genuine(event, DELETION)) {
        deleted += apply_deletion(db, deletion)?;
    }
    let mut added = 0;
    for event in recommendations
        .iter()
        .filter(|event| genuine(event, RECOMMENDATION))
    {
        if !is_deleted(db, event)? && save(db, event)? {
            added += 1;
        }
    }

    // What was found is kept, but the next sync asks again from where this one started.
    if !complete || !deletions_complete {
        return Err("the relays took too long to answer".to_string());
    }
    Ok(Synced {
        until,
        added,
        deleted,
    })
}

/// Downloads the latest recommendations of the person from the relays they write to, saves them
/// and answers them, newest first, with at most `LATEST_RECOMMENDATIONS` of them.
pub async fn fetch_latest(network: &Network, pubkey: &str) -> Result<Vec<Entry>, String> {
    pick_tls();

    let author: PubKey = pubkey.parse().map_err(|error| format!("{error}"))?;
    let relays = match saved_write_relays(author) {
        Some(relays) if !relays.is_empty() => relays,
        _ => relays_of(network, author).await,
    };
    if relays.is_empty() {
        return Err("no relay list was found".to_string());
    }
    let relays = reachable(network, relays).await;
    if relays.is_empty() {
        return Err("none of the relays could be reached".to_string());
    }

    let filter = Filter {
        kinds: Some(vec![RECOMMENDATION]),
        authors: Some(vec![author]),
        limit: Some(LATEST_RECOMMENDATIONS),
        ..Filter::default()
    };
    let mut queries = tokio::task::JoinSet::new();
    for relay in relays {
        let network = network.clone();
        let filter = filter.clone();
        queries.spawn(async move { collect(&network, &[relay], filter).await });
    }
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    while let Some(result) = queries.join_next().await {
        let Ok((found, _)) = result else {
            continue;
        };
        events.extend(found.into_iter().filter(|event| seen.insert(event.id)));
    }

    let db = database()?;
    let mut events: Vec<ritualistic::Event> = events
        .into_iter()
        .filter(|event| {
            event.pubkey == author && event.kind == RECOMMENDATION && event.verify_signature()
        })
        .collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.created_at));

    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    for event in events {
        if is_deleted(db, &event)? {
            continue;
        }
        let Some(video) = recommendation_of(&event) else {
            continue;
        };
        if !ids.insert(video.entry.id.clone()) {
            continue;
        }
        save(db, &event)?;
        entries.push(video.entry);
        if entries.len() == LATEST_RECOMMENDATIONS {
            break;
        }
    }
    Ok(entries)
}

/// What looking up a person found.
#[derive(Debug)]
pub struct Found {
    /// The address of their picture, if their metadata has one.
    pub picture: Option<String>,
    /// Whether the indexers have a relay list of theirs.
    pub relay_list: bool,
}

/// Downloads the metadata and the relay list of the person and keeps them in the database.
pub async fn fetch_profile(network: &Network, pubkey: &str) -> Result<Found, String> {
    pick_tls();

    let author: PubKey = pubkey.parse().map_err(|error| format!("{error}"))?;
    let (profile, relays) = tokio::join!(
        tokio::time::timeout(
            METADATA_WAIT,
            ritualistic::Profile::from_metadata_fetch(network, author),
        ),
        relays_of(network, author),
    );
    let picture = match profile {
        Ok(profile) => {
            let event = profile.event.filter(|event| {
                event.pubkey == author && event.kind == METADATA && event.verify_signature()
            });
            match event {
                Some(event) => {
                    database()?
                        .replace_event(&event, false)
                        .map_err(|e| e.to_string())?;
                    profile.metadata.picture.filter(|url| !url.is_empty())
                }
                None => {
                    crate::log::info!("no metadata for {pubkey}");
                    None
                }
            }
        }
        Err(_) => {
            crate::log::info!("the metadata of {pubkey} took too long to arrive");
            None
        }
    };
    Ok(Found {
        picture,
        relay_list: !relays.is_empty(),
    })
}

// Keeping track of the syncs, in the main thread

#[derive(Default)]
struct State {
    /// Up to when each person was downloaded, by public key.
    cursors: HashMap<String, u32>,
    /// The people being synced.
    pending: HashSet<String>,
    /// Why the last sync of a person failed.
    failures: HashMap<String, String>,
    loaded: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn cursors_path() -> PathBuf {
    crate::paths::data().join("social_sync.json")
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.loaded {
            state.loaded = true;
            state.cursors = std::fs::read(cursors_path())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        }
        f(&mut state)
    })
}

/// The people due a sync, with when each was last downloaded up to, marked as being synced.
pub fn due() -> Vec<(String, u32)> {
    let now = Timestamp::now().0;
    let follows = follows();
    with(|state| {
        follows
            .into_iter()
            .map(|key| key.to_hex())
            .filter_map(|key| {
                let since = state.cursors.get(&key).copied().unwrap_or(0);
                let due = !state.pending.contains(&key)
                    && now.saturating_sub(since) >= SYNC_AGAIN_SECONDS;
                due.then(|| {
                    state.pending.insert(key.clone());
                    (key, since)
                })
            })
            .collect()
    })
}

/// The sync of the person is over. Answers whether it changed the recommendations.
pub fn synced(pubkey: &str, result: Result<Synced, String>) -> bool {
    RANKED.set(None);
    with(|state| {
        state.pending.remove(pubkey);
        match result {
            Ok(synced) => {
                state.failures.remove(pubkey);
                state.cursors.insert(pubkey.to_string(), synced.until);
                if let Err(error) = write_json(&cursors_path(), &state.cursors) {
                    crate::log::warning!("Cannot save the social sync: {error}");
                }
                synced.added > 0 || synced.deleted > 0
            }
            Err(error) => {
                crate::log::warning!("Cannot sync the recommendations of {pubkey}: {error}");
                state.failures.insert(pubkey.to_string(), error);
                // Whatever was saved before it failed is new.
                true
            }
        }
    })
}

/// Reads the recommendations from the database again next time, after new ones were saved.
pub fn forget_ranked() {
    RANKED.set(None);
}

/// How many people are being synced, how many failed the last time and why one did.
pub fn progress() -> (usize, usize, Option<String>) {
    with(|state| {
        (
            state.pending.len(),
            state.failures.len(),
            state.failures.values().next().cloned(),
        )
    })
}

/// The newest event of the kind by the person in the database.
fn newest_of(pubkey: PubKey, kind: Kind) -> Option<ritualistic::Event> {
    let mut filters = [Filter {
        kinds: Some(vec![kind]),
        authors: Some(vec![pubkey]),
        limit: Some(1),
        ..Filter::default()
    }];
    database()
        .ok()
        .and_then(|db| db.query_events(&mut filters).ok())
        .and_then(|mut events| events.next())
        .and_then(|archived| event_of(&archived))
}

/// The person as their metadata in the database has them, or with nothing but their public key.
pub fn profile(pubkey: PubKey) -> ritualistic::Profile {
    newest_of(pubkey, METADATA)
        .map(ritualistic::Profile::from_event)
        .unwrap_or_else(|| ritualistic::Profile::blank_from_pubkey(pubkey))
}

/// The relays of the person, with whether they read or write there when they say, as their relay
/// list in the database has them. None until a sync finds the list.
pub fn relays(pubkey: PubKey) -> Option<Vec<(String, Option<String>)>> {
    let list = newest_of(pubkey, RELAY_LIST)?;
    Some(
        list.tags
            .find_all("r")
            .map(|tag| (tag[1].clone(), tag.get(2).cloned()))
            .collect(),
    )
}

/// The latest videos the person recommends, the most recently recommended first.
pub fn videos_of(pubkey: PubKey) -> Vec<Entry> {
    let mut filters = [Filter {
        kinds: Some(vec![RECOMMENDATION]),
        authors: Some(vec![pubkey]),
        ..Filter::default()
    }];
    let events = match database().and_then(|db| {
        db.query_events(&mut filters)
            .map_err(|error| error.to_string())
    }) {
        Ok(events) => events,
        Err(error) => {
            crate::log::warning!("Cannot read the recommendations: {error}");
            return Vec::new();
        }
    };
    let mut events: Vec<ritualistic::Event> =
        events.filter_map(|archived| event_of(&archived)).collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.created_at));
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(recommendation_of)
        .filter(|video| seen.insert(video.entry.id.clone()))
        .take(LATEST_RECOMMENDATIONS)
        .map(|video| video.entry)
        .collect()
}

// Reading the recommendations

/// A video the people followed recommend.
#[derive(Debug, Clone)]
pub struct Recommendation {
    pub entry: Entry,
    /// Its hashtags and categories, from all the people who recommend it.
    pub hashtags: Vec<String>,
    pub categories: Vec<String>,
    /// The people followed who recommend it, in the order they are followed in.
    pub by: Vec<PubKey>,
}

/// Whether `id` looks like a YouTube video id.
fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The video a recommendation is about.
fn recommendation_of(event: &ritualistic::Event) -> Option<Recommendation> {
    let tag = |name: &str| {
        event
            .tags
            .find(name)
            .map(|tag| tag[1].trim().to_string())
            .unwrap_or_default()
    };
    let all = |name: &str, normal: fn(&str) -> String| -> Vec<String> {
        let mut values: Vec<String> = Vec::new();
        for tag in event.tags.find_all(name) {
            let value = normal(tag[1].trim());
            if !value.is_empty() && !values.contains(&value) {
                values.push(value);
            }
        }
        values
    };
    let id = tag("i");
    if !is_video_id(&id) {
        return None;
    }
    Some(Recommendation {
        entry: Entry {
            url: format!("https://www.youtube.com/watch?v={id}"),
            id,
            title: tag("title"),
            channel: tag("channel"),
            channel_id: tag("channel_id"),
            duration: tag("duration").parse().ok(),
            views: None,
            published: tag("published").parse().ok(),
        },
        hashtags: all("t", str::to_lowercase),
        categories: all("l", str::to_string),
        by: vec![event.pubkey],
    })
}

/// The recommendations in the database, ranked, with where each video is among them.
struct Ranked {
    videos: Vec<Recommendation>,
    by_id: HashMap<String, usize>,
}

thread_local! {
    /// What was read from the database, until a sync changes it.
    static RANKED: RefCell<Option<Rc<Ranked>>> = const { RefCell::new(None) };
}

/// All the videos the people followed recommend, the ones recommended by more of them first,
/// then the most recently recommended.
fn ranked() -> Rc<Ranked> {
    if let Some(ranked) = RANKED.with_borrow(Clone::clone) {
        return ranked;
    }
    let ranked = Rc::new(read_ranked());
    RANKED.set(Some(ranked.clone()));
    ranked
}

fn read_ranked() -> Ranked {
    let empty = Ranked {
        videos: Vec::new(),
        by_id: HashMap::new(),
    };
    let follows = follows();
    if follows.is_empty() {
        return empty;
    }
    let db = match database() {
        Ok(db) => db,
        Err(error) => {
            crate::log::warning!("Cannot read the recommendations: {error}");
            return empty;
        }
    };
    let mut filters = [Filter {
        kinds: Some(vec![RECOMMENDATION]),
        authors: Some(follows.clone()),
        ..Filter::default()
    }];
    let events = match db.query_events(&mut filters) {
        Ok(results) => results,
        Err(error) => {
            crate::log::warning!("Cannot read the recommendations: {error}");
            return empty;
        }
    };

    struct Recommended {
        video: Recommendation,
        by: HashSet<PubKey>,
        newest: Timestamp,
    }
    let mut videos: HashMap<String, Recommended> = HashMap::new();
    for event in events.filter_map(|archived| event_of(&archived)) {
        let Some(video) = recommendation_of(&event) else {
            continue;
        };
        let (pubkey, created_at) = (event.pubkey, event.created_at);
        let recommended = videos
            .entry(video.entry.id.clone())
            .or_insert_with(|| Recommended {
                video: Recommendation {
                    hashtags: Vec::new(),
                    categories: Vec::new(),
                    ..video.clone()
                },
                by: HashSet::new(),
                newest: created_at,
            });
        // Everyone gets to label it.
        for (all, new) in [
            (&mut recommended.video.hashtags, video.hashtags),
            (&mut recommended.video.categories, video.categories),
        ] {
            for label in new {
                if !all.contains(&label) {
                    all.push(label);
                }
            }
        }
        recommended.by.insert(pubkey);
        recommended.newest = recommended.newest.max(created_at);
    }

    let mut videos: Vec<Recommended> = videos.into_values().collect();
    videos.sort_by(|a, b| {
        b.by.len()
            .cmp(&a.by.len())
            .then(b.newest.cmp(&a.newest))
            .then_with(|| a.video.entry.id.cmp(&b.video.entry.id))
    });
    let videos: Vec<Recommendation> = videos
        .into_iter()
        .map(|video| Recommendation {
            by: follows
                .iter()
                .filter(|pubkey| video.by.contains(pubkey))
                .copied()
                .collect(),
            ..video.video
        })
        .collect();
    let by_id = videos
        .iter()
        .enumerate()
        .map(|(at, video)| (video.entry.id.clone(), at))
        .collect();
    Ranked { videos, by_id }
}

/// The unwatched videos the people followed recommend, the ones recommended by more of them
/// first, then the most recently recommended.
pub fn recommended() -> Vec<Recommendation> {
    ranked()
        .videos
        .iter()
        .filter(|video| !history::is_watched(&video.entry.id))
        .cloned()
        .collect()
}

/// The hashtags and the categories of a video recommended, and who recommends it, if it is.
pub fn labels_of(id: &str) -> Option<(Vec<String>, Vec<String>, Vec<PubKey>)> {
    let ranked = ranked();
    let video = &ranked.videos[*ranked.by_id.get(id)?];
    Some((
        video.hashtags.clone(),
        video.categories.clone(),
        video.by.clone(),
    ))
}

/// The hashtags of a video, if it is recommended.
pub fn hashtags_of(id: &str) -> Vec<String> {
    let ranked = ranked();
    ranked
        .by_id
        .get(id)
        .map(|&at| ranked.videos[at].hashtags.clone())
        .unwrap_or_default()
}

thread_local! {
    /// The names of the people, read from their metadata once, by public key in hex.
    static NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// The name the person goes by, as short as the lists need it.
pub fn name_of(pubkey: PubKey) -> String {
    let hex = pubkey.to_hex();
    if let Some(name) = NAMES.with_borrow(|names| names.get(&hex).cloned()) {
        return name;
    }
    let name = profile(pubkey).render_name();
    NAMES.with_borrow_mut(|names| names.insert(hex, name.clone()));
    name
}

/// Reads the name of the person again next time, their metadata changed.
pub fn forget_name(pubkey: &str) {
    NAMES.with_borrow_mut(|names| names.remove(pubkey));
}

/// The recommendations in the category, or all of them, in their order.
pub fn in_category(videos: &[Recommendation], category: Option<&str>) -> Vec<Entry> {
    videos
        .iter()
        .filter(|video| {
            category.is_none_or(|category| video.categories.iter().any(|c| c == category))
        })
        .map(|video| video.entry.clone())
        .collect()
}

/// Labels with how many videos have each, the most common first.
pub type Counted = Vec<(String, usize)>;

/// The categories of the videos.
pub fn categories(videos: &[Recommendation]) -> Counted {
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for category in videos.iter().flat_map(|video| &video.categories) {
        *counts.entry(category).or_default() += 1;
    }
    let mut counts: Counted = counts
        .into_iter()
        .map(|(category, count)| (category.clone(), count))
        .collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tags: Vec<Vec<&str>>) -> ritualistic::Event {
        ritualistic::Event {
            id: ID::from_bytes([0; 32]),
            pubkey: SecretKey::generate().pubkey(),
            created_at: Timestamp(0),
            kind: RECOMMENDATION,
            tags: ritualistic::Tags(
                tags.into_iter()
                    .map(|tag| tag.into_iter().map(String::from).collect())
                    .collect(),
            ),
            content: String::new(),
            sig: ritualistic::Signature::from_bytes([0; 64]),
        }
    }

    fn signed(key: &SecretKey, kind: Kind, tags: Vec<Vec<&str>>) -> ritualistic::Event {
        ritualistic::EventTemplate {
            created_at: Timestamp::now(),
            kind,
            tags: event(tags).tags,
            content: String::new(),
        }
        .finalize(key)
    }

    #[test]
    fn deletions_take_out_only_their_author_s_events() {
        let path =
            std::env::temp_dir().join(format!("cathode-social-test-{}.redb", std::process::id()));
        let db = RedbEventDatabase::open(&path).unwrap();
        let (author, stranger) = (SecretKey::generate(), SecretKey::generate());
        let first = signed(&author, RECOMMENDATION, vec![vec!["i", "dQw4w9WgXcQ"]]);
        let second = signed(&author, RECOMMENDATION, vec![vec!["i", "jNQXAC9IVRw"]]);
        assert!(save(&db, &first).unwrap());
        assert!(save(&db, &second).unwrap());
        assert!(!save(&db, &first).unwrap());

        let id = first.id.to_hex();
        let not_theirs = signed(&stranger, DELETION, vec![vec!["e", &id], vec!["k", "1032"]]);
        assert_eq!(apply_deletion(&db, &not_theirs).unwrap(), 0);
        let deletion = signed(&author, DELETION, vec![vec!["e", &id], vec!["k", "1032"]]);
        assert_eq!(apply_deletion(&db, &deletion).unwrap(), 1);

        let mut filters = [Filter {
            kinds: Some(vec![RECOMMENDATION]),
            ..Filter::default()
        }];
        let left: Vec<ID> = db
            .query_events(&mut filters)
            .unwrap()
            .filter_map(|archived| event_of(&archived))
            .map(|event| event.id)
            .collect();
        assert_eq!(left, vec![second.id]);
        // Coming back from another relay later, it stays out.
        assert!(is_deleted(&db, &first).unwrap());
        assert!(!is_deleted(&db, &second).unwrap());

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_relays_leave_out_the_read_only_ones() {
        let list = event(vec![
            vec!["r", "wss://write.example"],
            vec!["r", "wss://both.example"],
            vec!["r", "wss://read.example", "read"],
            vec!["r", "wss://also-write.example", "write"],
            vec!["r", "wss://write.example"],
        ]);
        let relays = write_relays_of(&list);
        assert_eq!(relays.len(), 3);
        assert!(relays.iter().all(|relay| !relay.contains("read.example")));
        assert_eq!(
            relays
                .iter()
                .filter(|relay| relay.contains("//write.example"))
                .count(),
            1
        );
    }

    /// Hits the network, and saves to the data directory: `XDG_DATA_HOME` should point to an
    /// empty place.
    #[tokio::test]
    #[ignore]
    async fn relay_lists_come_from_the_indexers() {
        pick_tls();
        let network = Network::new();
        let pubkey: PubKey = DEFAULT_FOLLOWS[0].parse().unwrap();
        let written = relays_of(&network, pubkey).await;
        assert!(!written.is_empty());
        assert!(relays(pubkey).is_some_and(|saved| !saved.is_empty()));
    }

    #[test]
    fn recommendations_become_entries() {
        let video = recommendation_of(&event(vec![
            vec!["i", "dQw4w9WgXcQ"],
            vec!["title", "Never Gonna Give You Up"],
            vec!["duration", "213"],
            vec!["channel", "Rick Astley"],
            vec!["channel_id", "UCuAXFkgsw1L7xaCfnd5JJOw"],
            vec!["published", "1256453052"],
            vec!["t", "Rickroll"],
            vec!["t", "rickroll"],
            vec!["t", "80s"],
            vec!["l", "Music performance & songs"],
        ]))
        .unwrap();
        let entry = &video.entry;
        assert_eq!(entry.url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(entry.title, "Never Gonna Give You Up");
        assert_eq!(entry.duration, Some(213.0));
        assert_eq!(entry.channel_id, "UCuAXFkgsw1L7xaCfnd5JJOw");
        assert_eq!(entry.published, Some(1256453052));
        assert_eq!(video.hashtags, ["rickroll", "80s"]);
        assert_eq!(video.categories, ["Music performance & songs"]);

        assert!(recommendation_of(&event(vec![vec!["title", "no video"]])).is_none());
        assert!(
            recommendation_of(&event(vec![vec!["i", "https://youtu.be/dQw4w9WgXcQ"]])).is_none()
        );
    }

    fn video(id: &str, categories: &[&str]) -> Recommendation {
        Recommendation {
            entry: Entry {
                id: id.to_string(),
                url: String::new(),
                title: String::new(),
                channel: String::new(),
                channel_id: String::new(),
                duration: None,
                views: None,
                published: None,
            },
            hashtags: Vec::new(),
            categories: categories.iter().map(|c| c.to_string()).collect(),
            by: Vec::new(),
        }
    }

    #[test]
    fn categories_narrow_down() {
        let videos = [
            video("a", &["Programming"]),
            video("b", &["Talks"]),
            video("c", &["Programming", "Talks"]),
            video("d", &[]),
        ];
        let ids = |category| -> Vec<String> {
            in_category(&videos, category)
                .into_iter()
                .map(|entry| entry.id)
                .collect()
        };
        assert_eq!(ids(None), ["a", "b", "c", "d"]);
        assert_eq!(ids(Some("Talks")), ["b", "c"]);
        assert_eq!(
            categories(&videos),
            [("Programming".to_string(), 2), ("Talks".to_string(), 2)]
        );
    }
}

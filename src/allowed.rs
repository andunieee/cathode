//! Searching what the restricted mode allows. The videos of the restricted mode and of its
//! collections are matched by title right away, each playlist is listed whole and matched by title
//! too, and each channel is searched on the site. Listings are kept for a while, so typing another
//! search soon after doesn't fetch them again. Results come as they are found, the local ones
//! first.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::fetch::{Entry, Request, Response, Source};
use crate::{collections, nav, profile, subscriptions};

/// The keys of the listings of these searches, so `nav` leaves them alone.
const KEY_PREFIX: &str = "allowed:";
/// How long a listing is kept.
const KEEP: Duration = Duration::from_secs(20 * 60);

/// What a search hands over: videos found, and whether nothing more is coming.
pub type Sink = Box<dyn FnMut(Vec<Entry>, bool)>;

struct Search {
    /// The words of the query, in lower case, that a title must all have.
    words: Vec<String>,
    /// The listings not done yet.
    waiting: HashSet<String>,
    seen: HashSet<String>,
    sink: Sink,
}

/// A listing being fetched or fetched already.
#[derive(Default)]
struct Listing {
    entries: Vec<Entry>,
    /// When it was done, none while it is being fetched.
    done: Option<Instant>,
}

#[derive(Default)]
struct Local {
    searches: HashMap<u64, Search>,
    started: u64,
    listings: HashMap<String, Listing>,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local::default());
}

fn with<R>(f: impl FnOnce(&mut Local) -> R) -> R {
    LOCAL.with(|local| f(&mut local.borrow_mut()))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split_whitespace().map(str::to_owned).collect()
}

fn matches(words: &[String], title: &str) -> bool {
    let title = title.to_lowercase();
    words.iter().all(|word| title.contains(word.as_str()))
}

fn playlist_key(id: &str) -> String {
    format!("{KEY_PREFIX}l:{id}")
}

fn channel_key(id: &str, query: &str) -> String {
    format!("{KEY_PREFIX}c:{id}:{query}")
}

/// Whether the entries of the listing are all matches, which is the case of the searches inside a
/// channel. The playlists are matched here.
fn searched(key: &str) -> bool {
    key.starts_with(&format!("{KEY_PREFIX}c:"))
}

/// Starts a search, `sink` gets its results. Returns its id to cancel it with.
pub fn start(query: &str, sink: Sink) -> u64 {
    let query = query.trim().to_string();
    let words = words(&query);
    let id = with(|local| {
        local.started += 1;
        local.started
    });

    let (videos, collection_names, playlists, channels) = profile::read(|profile| {
        (
            profile.videos.clone(),
            profile.collections.clone(),
            profile.playlists.clone(),
            profile.channels.clone(),
        )
    });
    let mut found: Vec<Entry> = videos;
    for name in &collection_names {
        if let Some(collection) = collections::get(name) {
            found.extend(collection.videos.into_iter().map(|video| video.entry));
        }
    }

    let mut search = Search {
        words,
        waiting: HashSet::new(),
        seen: HashSet::new(),
        sink,
    };
    let mut first = take_new(&mut search, found, false);

    let mut requests = Vec::new();
    with(|local| {
        local
            .listings
            .retain(|_, listing| listing.done.is_none_or(|done| done.elapsed() < KEEP));
        let sources = playlists
            .iter()
            .map(|playlist| (playlist_key(&playlist.id), Source::Playlist { id: playlist.id.clone() }))
            .chain(channels.iter().map(|channel| {
                (
                    channel_key(&channel.id, &query),
                    Source::Channel {
                        id: channel.id.clone(),
                        query: query.clone(),
                        language: subscriptions::language(&channel.id),
                    },
                )
            }));
        for (key, source) in sources {
            match local.listings.get(&key) {
                Some(listing) => {
                    let entries = listing.entries.clone();
                    // What was listed before is forgotten when the restricted mode is turned on.
                    profile::remember_listed(&entries);
                    first.extend(take_new(&mut search, entries, searched(&key)));
                    if listing.done.is_none() {
                        search.waiting.insert(key);
                    }
                }
                None => {
                    local.listings.insert(key.clone(), Listing::default());
                    search.waiting.insert(key.clone());
                    requests.push(Request::Listing { key, source });
                }
            }
        }
    });

    let done = search.waiting.is_empty();
    (search.sink)(first, done);
    if !done {
        with(|local| local.searches.insert(id, search));
    }
    for request in requests {
        nav::send(request);
    }
    id
}

/// Stops handing over the results of a search.
pub fn cancel(id: u64) {
    with(|local| local.searches.remove(&id));
}

/// The entries the search hasn't handed over yet, of those that match. `all` takes them all, they
/// were matched already.
fn take_new(search: &mut Search, entries: Vec<Entry>, all: bool) -> Vec<Entry> {
    entries
        .into_iter()
        .filter(|entry| all || matches(&search.words, &entry.title))
        .filter(|entry| search.seen.insert(entry.id.clone()))
        .collect()
}

/// Takes the responses to the listings of these searches, hands back the others.
pub fn intercept(response: Response) -> Option<Response> {
    let key = match &response {
        Response::Entries { key, .. }
        | Response::Channels { key, .. }
        | Response::ListingDone { key, .. } => key,
        _ => return Some(response),
    };
    if !key.starts_with(KEY_PREFIX) {
        return Some(response);
    }

    let (key, entries, done) = match response {
        Response::Entries { key, entries } => {
            // The videos of an allowed playlist are allowed to be watched.
            profile::remember_listed(&entries);
            (key, entries, false)
        }
        Response::ListingDone { key, error } => {
            with(|local| match error {
                Some(error) => {
                    crate::log::info!("allowed search: {key} failed: {error}");
                    // Not kept, so that the next search tries again.
                    local.listings.remove(&key);
                }
                None => {
                    if let Some(listing) = local.listings.get_mut(&key) {
                        listing.done = Some(Instant::now());
                    }
                }
            });
            (key, Vec::new(), true)
        }
        _ => return None,
    };

    // The sinks are called out of `LOCAL`, they could start another search.
    let mut handed = Vec::new();
    with(|local| {
        if let Some(listing) = local.listings.get_mut(&key) {
            listing.entries.extend(entries.iter().cloned());
        }
        let ids: Vec<u64> = local
            .searches
            .iter()
            .filter(|(_, search)| search.waiting.contains(&key))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let mut search = local.searches.remove(&id).unwrap();
            let new = take_new(&mut search, entries.clone(), searched(&key));
            if done {
                search.waiting.remove(&key);
            }
            handed.push((id, search, new));
        }
    });
    for (id, mut search, new) in handed {
        let finished = search.waiting.is_empty();
        if !new.is_empty() || finished {
            (search.sink)(new, finished);
        }
        if !finished {
            with(|local| local.searches.insert(id, search));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_need_every_word() {
        let query = words("  Cats  DOGS ");
        assert!(matches(&query, "Dogs and cats"));
        assert!(!matches(&query, "Only cats"));
        assert!(matches(&words(""), "Anything"));
    }
}

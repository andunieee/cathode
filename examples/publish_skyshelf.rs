//! Publishes the videos of `skyshelf.jsonl` as recommendations Cathode reads (see
//! `src/social.rs`), one Nostr event of kind 1032 for each:
//!
//! ```sh
//! cargo run --release --example publish_skyshelf -- skyshelf.jsonl skyshelf_secret_key [wss://relay…]
//! ```
//!
//! The listing has the video, its title, channel, hashtags and categories from
//! <https://skyshelf.app/>, but it doesn't know how long the videos are, which channel they are
//! from or when they came out, so that is read from their YouTube pages first and kept in
//! `skyshelf.meta.jsonl`. The events signed, and the relays that took each one, are kept in
//! `skyshelf.published.jsonl`, so running it again only does what is left: it doesn't sign the
//! same video twice, and retries the relays that failed. Videos YouTube says are gone aren't
//! published.
//!
//! The key's metadata and relay list are published first, so Cathode knows who recommends the
//! videos and where to find the recommendations.
//!
//! The videos are sent one at a time, from the end of the listing to its start, so the videos at
//! the top of it are the most recent recommendations, which Cathode shows first. Each video's
//! page is read just before its event is sent, and there is a 30 second pause between videos. A
//! relay that won't take an event is ignored: the event still goes to the others, and the next
//! video is next.
//!
//! `--dry-run` reads the first video's page and prints its event without signing or sending
//! anything, `--limit N` takes only the first N videos of the listing.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ritualistic::{
    Event, EventTemplate, Kind, Metadata, Network, PublishOptions, SecretKey, Tags, Timestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};

const RECOMMENDATION: Kind = Kind(1032);
const RELAY_LIST: Kind = Kind(10002);

/// The relays Cathode looks at when the user hasn't changed them.
const DEFAULT_RELAYS: [&str; 3] = [
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://offchain.pub",
];
/// Where Cathode looks for the relays people write to.
const INDEXERS: [&str; 2] = ["wss://purplepag.es", "wss://indexer.coracle.social"];

/// How long to wait between two videos.
const INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Video {
    vid: String,
    title: String,
    channel: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    categories: Vec<Category>,
}

#[derive(Deserialize)]
struct Category {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

impl Category {
    /// The category Cathode shows, which is its name in the listing. The id is the fallback for
    /// older listings that only have that.
    fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            self.id.trim()
        } else {
            self.name.trim()
        }
    }
}

/// What the YouTube page of a video says about it.
#[derive(Clone, serde::Serialize, Deserialize)]
struct Meta {
    vid: String,
    /// Whether it can still be watched.
    available: bool,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    channel_id: String,
    /// When it came out, in seconds since the epoch.
    #[serde(default)]
    published: Option<i64>,
}

fn sibling(input: &Path, suffix: &str) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}.{suffix}"))
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append(file: &mut std::fs::File, value: &impl serde::Serialize) {
    let mut line = serde_json::to_vec(value).unwrap();
    line.push(b'\n');
    file.write_all(&line).expect("cannot write the progress");
}

fn open_append(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("cannot open {}: {error}", path.display()))
}

// Reading the YouTube pages

/// The string after `"key":"` in the page.
fn quoted_after<'a>(page: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let start = page.find(&needle)? + needle.len();
    let end = page[start..].find('"')?;
    Some(&page[start..start + end])
}

/// Days since the epoch of a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `2016-08-05T17:05:49-07:00`, or just `2016-08-05`, in seconds since the epoch.
fn parse_date(text: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?);
    let mut seconds = days * 86_400;
    if text.len() >= 19 {
        seconds += number(11..13)? * 3600 + number(14..16)? * 60 + number(17..19)?;
        if let Some(sign @ ('+' | '-')) = text[19..].chars().next() {
            let offset = number(20..22)? * 3600 + number(23..25)? * 60;
            seconds -= if sign == '+' { offset } else { -offset };
        }
    }
    Some(seconds)
}

/// What the page says about the video, or nothing when YouTube didn't say, which may change
/// if it is asked again later.
fn meta_of(vid: &str, page: &str) -> Option<Meta> {
    let status = page
        .split("\"playabilityStatus\":{\"status\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default();
    let duration = quoted_after(page, "lengthSeconds").and_then(|text| text.parse().ok());
    match (status, duration) {
        (_, Some(duration)) => Some(Meta {
            vid: vid.to_string(),
            available: true,
            duration: Some(duration),
            channel_id: quoted_after(page, "externalChannelId")
                .or_else(|| quoted_after(page, "channelId"))
                .unwrap_or_default()
                .to_string(),
            published: quoted_after(page, "publishDate")
                .or_else(|| quoted_after(page, "uploadDate"))
                .and_then(parse_date),
        }),
        // Taken down, made private, or never there.
        ("ERROR" | "UNPLAYABLE", None) => Some(Meta {
            vid: vid.to_string(),
            available: false,
            duration: None,
            channel_id: String::new(),
            published: None,
        }),
        // Most likely the page asking to prove it isn't a bot.
        _ => None,
    }
}

async fn fetch_meta(client: &reqwest::Client, vid: &str) -> Result<Option<Meta>, String> {
    let page = client
        .get(format!("https://www.youtube.com/watch?v={vid}"))
        .send()
        .await
        .map_err(|error| format!("{error:?}"))?
        .text()
        .await
        .map_err(|error| error.to_string())?;
    Ok(meta_of(vid, &page))
}

fn youtube_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("accept-language", "en-US,en;q=0.9".parse().unwrap());
    // Past the cookie consent page YouTube shows in Europe.
    headers.insert("cookie", "SOCS=CAI; CONSENT=YES+".parse().unwrap());
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0")
        .timeout(Duration::from_secs(30))
        // YouTube resets the HTTP/2 streams of this client.
        .http1_only()
        .build()
        .unwrap()
}

// The events

fn tag(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn recommendation(video: &Video, meta: &Meta, created_at: Timestamp) -> EventTemplate {
    let mut tags = vec![
        tag(&["i", &video.vid]),
        tag(&["title", &video.title]),
        tag(&["channel", &video.channel]),
    ];
    if !meta.channel_id.is_empty() {
        tags.push(tag(&["channel_id", &meta.channel_id]));
    }
    if let Some(duration) = meta.duration {
        tags.push(tag(&["duration", &duration.to_string()]));
    }
    if let Some(published) = meta.published {
        tags.push(tag(&["published", &published.to_string()]));
    }
    let mut seen = HashSet::new();
    for hashtag in &video.tags {
        let hashtag = hashtag.trim().to_lowercase().replace(" ", "-");
        if !hashtag.is_empty() && seen.insert(hashtag.clone()) {
            tags.push(tag(&["t", &hashtag]));
        }
    }
    for category in &video.categories {
        let category = category.label();
        if !category.is_empty() && seen.insert(format!("l:{category}")) {
            tags.push(tag(&["t", category]));
        }
    }
    EventTemplate {
        created_at,
        kind: RECOMMENDATION,
        tags: Tags(tags),
        content: video.summary.clone(),
    }
}

/// Sends the event to the relays once, answers with the ones that took it. Relays that fail are
/// left alone; the next video is next either way.
async fn publish(network: &Network, event: &Event, relays: &[String]) -> Vec<String> {
    let mut network = network.clone();
    let mut results = network
        .publish_many(relays, event.clone(), PublishOptions::default())
        .await;
    let mut accepted = Vec::new();
    while let Some(result) = results.recv().await {
        match result.error {
            None => accepted.push(result.relay_url),
            // Already there is as good as taken.
            Some(error) if error.contains("duplicate") => accepted.push(result.relay_url),
            Some(error) => eprintln!("  {} refused {}: {error}", result.relay_url, event.id),
        }
    }
    accepted
}

#[tokio::main]
async fn main() {
    // Both of the TLS backends of rustls end up built in, neither the relay connections nor
    // reqwest can pick one by themselves.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = take_flag(&mut args, "--dry-run");
    let limit = take_value(&mut args, "--limit").map(|n| n.parse::<usize>().expect("--limit N"));
    let [input, key_path, relays @ ..] = args.as_slice() else {
        eprintln!(
            "usage: publish_skyshelf [--dry-run] [--limit N] skyshelf.jsonl secret_key_file [relay…]"
        );
        std::process::exit(2);
    };
    let input = Path::new(input);
    let relays: Vec<String> = if relays.is_empty() {
        DEFAULT_RELAYS.map(String::from).to_vec()
    } else {
        relays.to_vec()
    };
    let key: SecretKey = std::fs::read_to_string(key_path)
        .expect("cannot read the secret key")
        .trim()
        .parse()
        .expect("not a valid secret key");
    eprintln!("Publishing as {}", key.pubkey().to_hex());

    let mut videos: Vec<Video> = read_lines(input)
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    if let Some(limit) = limit {
        videos.truncate(limit);
    }
    eprintln!("{} videos in {}", videos.len(), input.display());

    let meta_path = sibling(input, "meta.jsonl");
    let mut meta: HashMap<String, Meta> = read_lines(&meta_path)
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Meta>(value).ok())
        .map(|meta| (meta.vid.clone(), meta))
        .collect();
    let client = youtube_client();

    // One time for the whole run, so the recommendations are one batch.
    let created_at = Timestamp::now();

    if dry_run {
        for video in &videos {
            let video_meta = match meta.get(&video.vid) {
                Some(meta) => Some(meta.clone()),
                None => match fetch_meta(&client, &video.vid).await {
                    Ok(meta) => meta,
                    Err(error) => {
                        eprintln!("  Cannot read the page of {}: {error}", video.vid);
                        None
                    }
                },
            };
            if let Some(video_meta) = video_meta {
                let template = recommendation(video, &video_meta, created_at);
                let event = json!({
                    "kind": template.kind.0,
                    "created_at": template.created_at.0,
                    "content": template.content,
                    "tags": template.tags.0,
                });
                println!("{}", serde_json::to_string_pretty(&event).unwrap());
                break;
            }
        }
        return;
    }

    let network = Network::new();

    // Where the events are, for Cathode to find them.
    let mut list_tags = vec![];
    for relay in &relays {
        list_tags.push(tag(&["r", relay]));
    }
    let list = EventTemplate {
        created_at: Timestamp::now(),
        kind: RELAY_LIST,
        tags: Tags(list_tags),
        content: String::new(),
    }
    .finalize(&key);
    let targets: Vec<String> = relays
        .iter()
        .cloned()
        .chain(INDEXERS.map(String::from))
        .collect();
    let took = publish(&network, &list, &targets).await;
    eprintln!(
        "Relay list taken by {} of {} relays",
        took.len(),
        targets.len()
    );

    // Who the recommendations are from, so Cathode can show it on their page.
    let metadata = Metadata {
        name: Some("Skyshelf".to_string()),
        about: Some(
            "Curated video directory: https://skyshelf.app/ — made by Hypergraph Labs.

About
-----

A hand-picked index of good YouTube channels and videos, organized by subject, kept deliberately small.

What this is
------------

Skyshelf is a manually curated directory. Every entry has been watched, evaluated, and added individually. There is no recommendation engine, no engagement metric, and no sponsored placement.

The focus is on channels that value depth over frequency, with a bias towards smaller creators.

Selection criteria
------------------

01 Accuracy. Content is factually sound and updated or annotated when errors are found.
02 Depth. Explanations go beyond surface level. The viewer comes away with a genuine model of something.
03 Clarity. Complex ideas are communicated precisely, not simplified into uselessness.
04 Independence. No significant sponsorship pressure distorting the content.
05 Longevity. Preference for content that will be relevant in five years, not just five weeks.

Who's the curator?
------------------

It-s-a-me, https://rakhim.org/. I'm also building https://minifeed.net — a place for well-curated RSS feeds, and https://exotext.com — a simple blogging platform.
".to_string(),
        ),
        picture: Some("https://skyshelf.app/apple-touch-icon.png".to_string()),
        ..Metadata::default()
    }
    .to_event_template()
    .finalize(&key);
    let took = publish(&network, &metadata, &relays).await;
    eprintln!(
        "Metadata taken by {} of {} relays",
        took.len(),
        relays.len()
    );

    // What earlier runs did.
    let published_path = sibling(input, "published.jsonl");
    let mut signed: HashMap<String, Event> = HashMap::new();
    let mut taken: HashMap<String, HashSet<String>> = HashMap::new();
    for line in read_lines(&published_path) {
        if let Some(event) = line.get("event")
            && let Ok(event) = serde_json::from_value::<Event>(event.clone())
            && event.pubkey == key.pubkey()
            && let Some(vid) = event.tags.find("i").map(|tag| tag[1].clone())
        {
            signed.insert(vid, event);
        } else if let (Some(id), Some(relay)) = (line["id"].as_str(), line["relay"].as_str()) {
            taken
                .entry(id.to_string())
                .or_default()
                .insert(relay.to_string());
        }
    }
    let mut log = open_append(&published_path);
    let mut meta_file = open_append(&meta_path);

    // From the bottom of the listing up, so its top ends up the newest.
    let todo: Vec<&Video> = videos.iter().rev().collect();
    eprintln!("Sending {} videos to {}", todo.len(), relays.join(", "));

    let (mut sent, mut partial, mut skipped, mut unread) = (0, 0, 0, 0);
    for (index, &video) in todo.iter().enumerate() {
        let mut worked = false;

        let video_meta = match meta.get(&video.vid) {
            Some(meta) => Some(meta.clone()),
            None => {
                worked = true;
                match fetch_meta(&client, &video.vid).await {
                    Ok(Some(found)) => {
                        append(&mut meta_file, &found);
                        meta.insert(video.vid.clone(), found.clone());
                        Some(found)
                    }
                    Ok(None) => {
                        eprintln!("  YouTube didn't tell about {}", video.vid);
                        unread += 1;
                        None
                    }
                    Err(error) => {
                        eprintln!("  Cannot read the page of {}: {error}", video.vid);
                        unread += 1;
                        None
                    }
                }
            }
        };

        if let Some(video_meta) = video_meta.filter(|meta| meta.available) {
            let event = match signed.get(&video.vid) {
                Some(event) => event.clone(),
                None => {
                    let event = recommendation(video, &video_meta, created_at).finalize(&key);
                    append(&mut log, &json!({ "event": event }));
                    event
                }
            };
            let done = taken.get(&event.id.to_hex()).cloned().unwrap_or_default();
            let missing: Vec<String> = relays
                .iter()
                .filter(|relay| !done.contains(*relay))
                .cloned()
                .collect();
            if missing.is_empty() {
                skipped += 1;
            } else {
                worked = true;
                let accepted = publish(&network, &event, &missing).await;
                for relay in &accepted {
                    append(
                        &mut log,
                        &json!({ "id": event.id.to_hex(), "relay": relay }),
                    );
                }
                if accepted.len() == missing.len() {
                    sent += 1;
                } else {
                    partial += 1;
                }
                eprintln!(
                    "  {} of {} videos: {} of {} relays took {}",
                    index + 1,
                    todo.len(),
                    accepted.len(),
                    missing.len(),
                    video.vid
                );
            }
        }

        if worked && index + 1 < todo.len() {
            tokio::time::sleep(INTERVAL).await;
        }
    }
    eprintln!(
        "Sent {sent} to every relay, {partial} to only some (run it again to retry those), \
         {skipped} were already everywhere, {unread} pages could not be read"
    );
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|arg| arg != flag);
    args.len() != before
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.remove(at);
    (at < args.len()).then(|| args.remove(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("2016-08-05T17:05:49-07:00"), Some(1_470_441_949));
        assert_eq!(parse_date("2012-11-11T00:00:00Z"), Some(1_352_592_000));
        assert_eq!(parse_date("1970-01-02"), Some(86_400));
        assert_eq!(parse_date("nonsense"), None);
    }
}

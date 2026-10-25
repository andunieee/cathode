//! The videos watched, one JSON object per line in `history.jsonl` in the data directory of
//! the app, the latest last. A new entry is appended to the file, unless the video is there
//! already, and reading goes backwards from its end, which gives the most recent ones, as far
//! back as one cares to go.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;

/// How much of the file is read at a time when going backwards.
const CHUNK: u64 = 8192;

/// The ids of the videos watched, read from the file the first time they are asked for.
static IDS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watched {
    /// When it was started, in seconds since the epoch.
    pub watched: i64,
    pub entry: Entry,
}

fn path() -> PathBuf {
    crate::paths::data()
        .join("history.jsonl")
}

/// Puts the video at the end of the history, unless it is there already.
pub fn record(entry: &Entry) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs() as i64);
    let mut line = serde_json::to_string(&Watched {
        watched: now,
        entry: entry.clone(),
    })
    .map_err(|e| e.to_string())?;
    line.push('\n');

    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    // One write, so a line is either all there or not at all.
    let mut ids = IDS.lock().unwrap();
    if ids.get_or_insert_with(read_ids).contains(&entry.id) {
        return Ok(());
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| e.to_string())?;
    // Not read yet means it will have this one when it is.
    if let Some(ids) = ids.as_mut() {
        ids.insert(entry.id.clone());
    }
    Ok(())
}

/// Takes the video out of the history.
pub fn remove(id: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Line {
        entry: Entry,
    }

    let path = path();
    let mut ids = IDS.lock().unwrap();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        // Lines that can't be read are kept as they are.
        if serde_json::from_str::<Line>(line).is_ok_and(|l| l.entry.id == id) {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    // Written aside and renamed, so the history is never left half written.
    let temporary = path.with_extension("jsonl.tmp");
    std::fs::write(&temporary, kept)
        .and_then(|_| std::fs::rename(&temporary, &path))
        .map_err(|e| e.to_string())?;
    if let Some(ids) = ids.as_mut() {
        ids.remove(id);
    }
    Ok(())
}

/// Whether the video is in the history.
pub fn is_watched(id: &str) -> bool {
    IDS.lock()
        .unwrap()
        .get_or_insert_with(read_ids)
        .contains(id)
}

/// The first entry after the first `index` lines of the file, with its line number, counted from
/// 1 on the oldest. Lines that can't be read are skipped, but they count.
pub fn after(index: usize) -> Option<(usize, Entry)> {
    let file = std::fs::File::open(path()).ok()?;
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .enumerate()
        .skip(index)
        .find_map(|(number, line)| {
            let watched = serde_json::from_str::<Watched>(&line).ok()?;
            Some((number + 1, watched.entry))
        })
}

/// How many lines the file has.
pub fn count() -> usize {
    std::fs::File::open(path())
        .map(|file| BufReader::new(file).lines().map_while(Result::ok).count())
        .unwrap_or(0)
}

fn read_ids() -> HashSet<String> {
    #[derive(Deserialize)]
    struct Line {
        entry: Id,
    }
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }

    let Ok(file) = std::fs::File::open(path()) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Line>(&line).ok())
        .map(|line| line.entry.id)
        .collect()
}

/// Up to `count` entries going back from the byte `before` (`u64::MAX` for the end of the file),
/// with the offset to go on from and whether that was the start of the file. Lines that can't be
/// read are skipped.
pub fn read(before: u64, count: usize) -> (Vec<Watched>, u64, bool) {
    let Ok(mut file) = std::fs::File::open(path()) else {
        return (Vec::new(), 0, true);
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return (Vec::new(), 0, true);
    };

    // `buffer` holds the file from `position` up to the lines already taken.
    let mut position = before.min(length);
    let mut buffer = Vec::new();
    let mut entries = Vec::new();
    let mut offset = position;
    while offset > 0 && entries.len() < count {
        let body = buffer.len() - usize::from(buffer.last() == Some(&b'\n'));
        let start = match buffer[..body].iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => newline + 1,
            None if position == 0 => 0,
            None => {
                // The line goes on further back: read the chunk before.
                let before = position.saturating_sub(CHUNK);
                let mut chunk = vec![0; (position - before) as usize];
                if file.seek(SeekFrom::Start(before)).is_err()
                    || file.read_exact(&mut chunk).is_err()
                {
                    return (entries, 0, true);
                }
                chunk.extend_from_slice(&buffer);
                buffer = chunk;
                position = before;
                continue;
            }
        };
        if let Ok(watched) = serde_json::from_slice::<Watched>(&buffer[start..body]) {
            entries.push(watched);
        }
        buffer.truncate(start);
        offset = position + start as u64;
    }
    (entries, offset, offset == 0)
}

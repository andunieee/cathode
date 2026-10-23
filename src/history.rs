//! The videos watched, one JSON object per line in `history.jsonl` in the data directory of
//! the app, the latest last. A new entry is appended to the file, and reading goes backwards from
//! its end, which gives the most recent ones, as far back as one cares to go.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;

/// How much of the file is read at a time when going backwards.
const CHUNK: u64 = 8192;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watched {
    /// When it was started, in seconds since the epoch.
    pub watched: i64,
    pub entry: Entry,
}

fn path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cathode")
        .join("history.jsonl")
}

/// Puts the video at the end of the history.
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
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| e.to_string())
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

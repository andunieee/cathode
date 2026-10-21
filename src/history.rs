//! The videos watched, one JSON object per line in `history.jsonl` in the config directory of
//! the app, the latest first. A new entry goes on top of the file, so reading from the start
//! gives the most recent ones, as far down as one cares to go.

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fetch::Entry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watched {
    /// When it was started, in seconds since the epoch.
    pub watched: i64,
    pub entry: Entry,
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cathode")
        .join("history.jsonl")
}

/// Puts the video on top of the history.
pub fn record(entry: &Entry) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs() as i64);
    let line = serde_json::to_string(&Watched {
        watched: now,
        entry: entry.clone(),
    })
    .map_err(|e| e.to_string())?;

    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    // The new line, then the old file, written aside and renamed so a crash can't leave a
    // half-written history.
    let temporary = path.with_extension("jsonl.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())?;
    if let Ok(mut old) = std::fs::File::open(&path) {
        std::io::copy(&mut old, &mut file).map_err(|e| e.to_string())?;
    }
    drop(file);
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

/// Up to `count` entries from the byte `offset` on, with the offset to go on from and whether
/// that was the end of the file. Lines that can't be read are skipped.
pub fn read(offset: u64, count: usize) -> (Vec<Watched>, u64, bool) {
    let Ok(mut file) = std::fs::File::open(path()) else {
        return (Vec::new(), offset, true);
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (Vec::new(), offset, true);
    }

    let mut reader = BufReader::new(file);
    let (mut entries, mut offset) = (Vec::new(), offset);
    let mut line = String::new();
    while entries.len() < count {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return (entries, offset, true),
            Ok(read) => {
                offset += read as u64;
                if let Ok(watched) = serde_json::from_str::<Watched>(&line) {
                    entries.push(watched);
                }
            }
        }
    }

    let done = reader.fill_buf().is_ok_and(|rest| rest.is_empty());
    (entries, offset, done)
}

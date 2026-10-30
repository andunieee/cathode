//! The log of what the app does. It goes to `cathode.log` in the data directory, as the app
//! detaches from the terminal and its output is lost, and to stderr when it runs in the
//! foreground. The previous run's log is kept as `cathode.old.log`.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    crate::paths::data().join("cathode.log")
}

/// Opens the log for this run, and has the panics written to it.
pub fn init() {
    let path = path();
    if let Some(directory) = path.parent() {
        let _ = std::fs::create_dir_all(directory);
    }
    let _ = std::fs::rename(&path, path.with_file_name("cathode.old.log"));
    *FILE.lock().unwrap() = File::create(&path).ok();

    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write("PANIC", format_args!("{info}"));
        default(info);
    }));
    info!(
        "cathode {} started, logging to {}",
        env!("CARGO_PKG_VERSION"),
        path.display()
    );
}

pub fn write(level: &str, message: std::fmt::Arguments) {
    let line = format!("{} {level:<5} {message}\n", timestamp());
    eprint!("{line}");
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// The time in UTC, like `2026-10-23 14:05:09.123`.
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (days, rest) = (now.as_secs() / 86400, now.as_secs() % 86400);

    // The date of a day count, by the civil calendar algorithm.
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        now.subsec_millis()
    )
}

macro_rules! info {
    ($($argument:tt)*) => { $crate::log::write("INFO", format_args!($($argument)*)) };
}
macro_rules! warning {
    ($($argument:tt)*) => { $crate::log::write("WARN", format_args!($($argument)*)) };
}
macro_rules! error {
    ($($argument:tt)*) => { $crate::log::write("ERROR", format_args!($($argument)*)) };
}
pub(crate) use {error, info, warning};

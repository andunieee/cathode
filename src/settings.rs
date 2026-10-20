//! The general settings, applied to every yt-dlp run. They live in `settings.json` in the config
//! directory of the app. The worker threads read them too, hence the lock.

use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Heights on offer for the downloads, `0` being the best there is.
pub const QUALITIES: [(u32, &str); 7] = [
    (0, "Best available"),
    (2160, "2160p (4K)"),
    (1440, "1440p"),
    (1080, "1080p"),
    (720, "720p"),
    (480, "480p"),
    (360, "360p"),
];

/// The browsers yt-dlp can take the cookies of, as it names them.
pub const BROWSERS: [&str; 10] = [
    "brave", "chrome", "chromium", "edge", "firefox", "helium", "opera", "safari", "vivaldi",
    "whale",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Highest video height to download, `0` for no limit.
    pub max_height: u32,
    /// Browser whose cookies yt-dlp uses (`--cookies-from-browser`), to get past age and
    /// sign-in checks.
    pub cookies_from_browser: Option<String>,
    /// `--proxy`, empty for none.
    pub proxy: String,
    /// `--limit-rate` (like `2M`), empty for none.
    pub limit_rate: String,
    /// Cuts the sponsor segments out of the downloaded videos (SponsorBlock).
    pub remove_sponsors: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_height: 1080,
            cookies_from_browser: None,
            proxy: String::new(),
            limit_rate: String::new(),
            remove_sponsors: false,
        }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lumpla")
        .join("settings.json")
}

fn load() -> Settings {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn get() -> Settings {
    if let Some(settings) = SETTINGS.read().unwrap().as_ref() {
        return settings.clone();
    }
    let mut guard = SETTINGS.write().unwrap();
    guard.get_or_insert_with(load).clone()
}

/// Changes the settings and writes them to disk.
pub fn update(change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut settings = get();
    change(&mut settings);

    let path = path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())?;

    *SETTINGS.write().unwrap() = Some(settings);
    Ok(())
}

/// What `--cookies-from-browser` gets for the browser. yt-dlp doesn't know Helium, but it is
/// Chromium underneath, so it is Chromium looking in the profile directory of Helium.
pub fn browser_argument(browser: &str) -> String {
    if browser != "helium" {
        return browser.to_string();
    }

    #[cfg(target_os = "windows")]
    let profile =
        dirs::data_local_dir().map(|dir| dir.join("imput").join("Helium").join("User Data"));
    #[cfg(not(target_os = "windows"))]
    let profile = dirs::config_dir().map(|dir| dir.join("net.imput.helium"));

    match profile {
        // The profile goes after the first colon; a colon inside the path is fine.
        Some(profile) => format!("chromium:{}", profile.display()),
        None => "chromium".to_string(),
    }
}

/// A yt-dlp command with the settings that go for everything it does.
pub fn yt_dlp() -> Command {
    let settings = get();
    let mut command = crate::tools::command("yt-dlp");

    if let Some(browser) = &settings.cookies_from_browser {
        command.args(["--cookies-from-browser", &browser_argument(browser)]);
    }
    let proxy = settings.proxy.trim();
    if !proxy.is_empty() {
        command.args(["--proxy", proxy]);
    }
    command
}

/// The settings that only matter for downloading a video.
pub fn download_args() -> Vec<String> {
    let settings = get();
    let mut args = Vec::new();

    let format = if settings.max_height == 0 {
        "bv*+ba/b".to_string()
    } else {
        let h = settings.max_height;
        format!("bv*[height<={h}]+ba/b[height<={h}]/b")
    };
    args.extend(["-f".to_string(), format]);

    let rate = settings.limit_rate.trim();
    if !rate.is_empty() {
        args.extend(["--limit-rate".to_string(), rate.to_string()]);
    }
    if settings.remove_sponsors {
        args.extend(["--sponsorblock-remove".to_string(), "sponsor".to_string()]);
    }
    args
}

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
    /// Skips the sponsor segments (SponsorBlock) while playing.
    #[serde(alias = "remove_sponsors")]
    pub skip_sponsors: bool,
    /// Hides the seek bar of the player.
    pub hide_progress_bar: bool,
    /// The language the titles of the listings and the subtitles are wanted in, like `en` or
    /// `pt-BR`. Empty for what YouTube gives by default.
    pub language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_height: 1080,
            cookies_from_browser: None,
            proxy: String::new(),
            limit_rate: String::new(),
            skip_sponsors: false,
            hide_progress_bar: false,
            language: "en".to_string(),
        }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

fn path() -> PathBuf {
    crate::paths::config()
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
    let profile = host_config_dir().map(|dir| dir.join("net.imput.helium"));

    match profile {
        // The profile goes after the first colon; a colon inside the path is fine.
        Some(profile) => format!("chromium:{}", profile.display()),
        None => "chromium".to_string(),
    }
}

/// The `~/.config` of the user. In a Flatpak `XDG_CONFIG_HOME` points inside the sandbox, but the
/// browsers keep their profiles in the real one.
#[cfg(not(target_os = "windows"))]
fn host_config_dir() -> Option<std::path::PathBuf> {
    if std::path::Path::new("/.flatpak-info").exists() {
        dirs::home_dir().map(|home| home.join(".config"))
    } else {
        dirs::config_dir()
    }
}

/// A yt-dlp command with the settings that go for everything it does.
pub fn yt_dlp() -> Command {
    let settings = get();
    let mut command = crate::tools::command("yt-dlp");

    if let Some(browser) = &settings.cookies_from_browser {
        command.args(["--cookies-from-browser", &browser_argument(browser)]);
    }
    // yt-dlp looks for the Chromium profiles under `XDG_CONFIG_HOME`.
    #[cfg(not(target_os = "windows"))]
    if let Some(dir) = host_config_dir() {
        command.env("XDG_CONFIG_HOME", dir);
    }
    let proxy = settings.proxy.trim();
    if !proxy.is_empty() {
        command.args(["--proxy", proxy]);
    }
    command
}

/// The settings that only matter for downloading a video, at `height` instead of the usual one
/// when it is given.
pub fn download_args(height: Option<u32>) -> Vec<String> {
    let settings = get();
    let mut args = Vec::new();

    let height = height.unwrap_or(settings.max_height);
    let format = if height == 0 {
        "bv*+ba/b".to_string()
    } else {
        let h = height;
        format!("bv*[height<={h}]+ba/b[height<={h}]/b")
    };
    args.extend(["-f".to_string(), format]);

    let rate = settings.limit_rate.trim();
    if !rate.is_empty() {
        args.extend(["--limit-rate".to_string(), rate.to_string()]);
    }
    args
}

/// The default language, if it is a code YouTube can be asked for: letters, digits and dashes.
pub fn language() -> Option<String> {
    let language = get().language.trim().to_string();
    valid_language(&language).then_some(language)
}

pub fn valid_language(language: &str) -> bool {
    !language.is_empty()
        && language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The codes yt-dlp accepts for `youtube:lang`, case-sensitive; it refuses any other.
const YOUTUBE_LANGUAGES: &[&str] = &[
    "af", "az", "id", "ms", "bs", "ca", "cs", "da", "de", "et", "en-IN", "en-GB", "en", "es",
    "es-419", "es-US", "eu", "fil", "fr", "fr-CA", "gl", "hr", "zu", "is", "it", "sw", "lv", "lt",
    "hu", "nl", "no", "uz", "pl", "pt-PT", "pt", "ro", "sq", "sk", "sl", "sr-Latn", "fi", "sv",
    "vi", "tr", "be", "bg", "ky", "kk", "mk", "mn", "ru", "sr", "uk", "el", "hy", "iw", "ur", "ar",
    "fa", "ne", "mr", "hi", "as", "bn", "pa", "gu", "or", "ta", "te", "kn", "ml", "si", "th", "lo",
    "my", "ka", "am", "km", "zh-CN", "zh-TW", "zh-HK", "ja", "ko",
];

/// The code YouTube accepts closest to a language tag: the tag itself, like `pt-PT`, otherwise its
/// plain language, like `en` for `en-US`, which YouTube spells some other ways.
fn youtube_language(tag: &str) -> Option<String> {
    let tag = tag.replace('_', "-");
    let plain = tag.split('-').next().unwrap_or(&tag).to_ascii_lowercase();
    let plain = match plain.as_str() {
        "he" => "iw",
        "nb" | "nn" => "no",
        "tl" => "fil",
        "zh" => "zh-CN",
        other => other,
    };
    let known = |code: &str| YOUTUBE_LANGUAGES.contains(&code);
    if known(&tag) {
        Some(tag)
    } else {
        known(plain).then(|| plain.to_string())
    }
}

/// Makes yt-dlp ask for the titles of the listings in the default language. YouTube gives
/// English ones otherwise, translated if the uploader has no English title.
/// A `language` replaces the default one, for the channels that have their own.
pub fn language_args(own: Option<String>) -> Vec<String> {
    match listing_language(own) {
        Some(language) => vec![
            "--extractor-args".to_string(),
            format!("youtube:lang={language}"),
        ],
        None => Vec::new(),
    }
}

/// The language the listings are asked in, as YouTube names it.
pub fn listing_language(own: Option<String>) -> Option<String> {
    own.or_else(language)
        .and_then(|language| youtube_language(&language))
}

#[cfg(test)]
mod youtube_language_tests {
    use super::youtube_language;

    #[test]
    fn tags_become_codes_youtube_accepts() {
        assert_eq!(youtube_language("en-US").as_deref(), Some("en"));
        assert_eq!(youtube_language("en-GB").as_deref(), Some("en-GB"));
        assert_eq!(youtube_language("pt-BR").as_deref(), Some("pt"));
        assert_eq!(youtube_language("he").as_deref(), Some("iw"));
        assert_eq!(youtube_language("zh-Hans").as_deref(), Some("zh-CN"));
        assert_eq!(youtube_language("xx"), None);
    }
}

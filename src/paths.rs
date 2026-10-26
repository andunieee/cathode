//! Where Cathode keeps its files. A named profile (`--profile NAME`) gets directories of its
//! own, so that it shares nothing with the default one and can run at the same time.

use std::path::PathBuf;
use std::sync::OnceLock;

static PROFILE: OnceLock<Option<String>> = OnceLock::new();

/// Sets the profile for the whole run. Only letters, digits, `-` and `_` are accepted.
pub fn set_profile(name: Option<String>) -> Result<(), String> {
    if let Some(name) = &name {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!(
                "invalid profile name {name:?}: use only letters, digits, - and _"
            ));
        }
    }
    let _ = PROFILE.set(name);
    Ok(())
}

pub fn profile() -> Option<&'static str> {
    PROFILE.get().and_then(|name| name.as_deref())
}

fn folder() -> String {
    match profile() {
        Some(name) => format!("cathode-{name}"),
        None => "cathode".to_string(),
    }
}

/// In a Flatpak the `XDG_*_HOME` variables point into `~/.var/app/<id>`. The sandbox has access to
/// the home folder, so the real directories are used, the same ones the unpackaged app uses.
fn base(sandboxed: impl Fn(PathBuf) -> PathBuf, native: Option<PathBuf>) -> Option<PathBuf> {
    if cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists() {
        dirs::home_dir().map(sandboxed)
    } else {
        native
    }
}

pub fn config() -> PathBuf {
    base(|home| home.join(".config"), dirs::config_dir()).unwrap_or_else(|| PathBuf::from(".")).join(folder())
}

pub fn data() -> PathBuf {
    base(|home| home.join(".local/share"), dirs::data_dir()).unwrap_or_else(|| PathBuf::from(".")).join(folder())
}

pub fn cache() -> PathBuf {
    base(|home| home.join(".cache"), dirs::cache_dir()).unwrap_or_else(std::env::temp_dir).join(folder())
}

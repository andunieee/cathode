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

pub fn config() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(folder())
}

pub fn data() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(folder())
}

pub fn cache() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join(folder())
}

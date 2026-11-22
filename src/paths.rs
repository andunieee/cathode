//! Where Cathode keeps its files. A named profile (`--profile NAME`) gets directories of its
//! own, so that it shares nothing with the default one and can run at the same time.
//!
//! Every folder returned here is absolute. The system's folders are preferred, then the absolute
//! ones Windows keeps in the environment (`%APPDATA%`, `%LOCALAPPDATA%`, `%USERPROFILE%`), then a
//! temporary folder. The current directory is never used: for an installed copy it is the folder
//! next to the executable, often under `C:\Program Files`, where a normal user cannot write.

use std::path::PathBuf;
use std::sync::{Once, OnceLock};

static PROFILE: OnceLock<Option<String>> = OnceLock::new();

/// Sets the profile for the whole run. Only letters, digits, `-` and `_` are accepted.
pub fn set_profile(name: Option<String>) -> Result<(), String> {
    if let Some(name) = &name
        && (name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(format!(
            "invalid profile name {name:?}: use only letters, digits, - and _"
        ));
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

/// A folder the system named, as long as it is absolute: a relative one would be resolved against
/// the working directory, which is what must never happen.
fn absolute(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|path| path.is_absolute())
}

/// One of the absolute folders Windows keeps in the environment. Unlike the known-folder API,
/// reading these works on any thread, without COM, and before GTK has started.
#[cfg(windows)]
fn from_env(name: &str) -> Option<PathBuf> {
    absolute(std::env::var_os(name).map(PathBuf::from))
}

/// `%USERPROFILE%\AppData\Roaming` or `...\Local`, for when `%APPDATA%`/`%LOCALAPPDATA%` is missing.
#[cfg(windows)]
fn app_data(roaming: bool) -> Option<PathBuf> {
    from_env("USERPROFILE").map(|home| {
        home.join("AppData")
            .join(if roaming { "Roaming" } else { "Local" })
    })
}

/// The last resort, used only when neither the system nor the environment names a folder. It is
/// still absolute, so nothing is ever written next to the executable.
fn fallback() -> PathBuf {
    static WARNED: Once = Once::new();
    let temp = std::env::temp_dir();
    let folder = if temp.is_absolute() {
        temp
    } else {
        absolute(dirs::home_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or(temp)
    };
    WARNED.call_once(|| {
        crate::log::warning!(
            "No user folder for Cathode's files was found, using {}",
            folder.display()
        )
    });
    folder
}

/// In a Flatpak the `XDG_*_HOME` variables point into `~/.var/app/<id>`. The sandbox has access to
/// the home folder, so the real directories are used, the same ones the unpackaged app uses.
fn base(sandboxed: impl Fn(PathBuf) -> PathBuf, native: Option<PathBuf>) -> Option<PathBuf> {
    if cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists() {
        absolute(dirs::home_dir().map(sandboxed))
    } else {
        absolute(native)
    }
}

pub fn config() -> PathBuf {
    #[cfg(windows)]
    let native = from_env("APPDATA")
        .or_else(|| app_data(true))
        .or_else(|| absolute(dirs::config_dir()));
    #[cfg(not(windows))]
    let native = base(|home| home.join(".config"), dirs::config_dir());

    native.unwrap_or_else(fallback).join(folder())
}

pub fn data() -> PathBuf {
    #[cfg(windows)]
    let native = from_env("APPDATA")
        .or_else(|| app_data(true))
        .or_else(|| absolute(dirs::data_dir()));
    #[cfg(not(windows))]
    let native = base(|home| home.join(".local/share"), dirs::data_dir());

    native.unwrap_or_else(fallback).join(folder())
}

pub fn cache() -> PathBuf {
    #[cfg(windows)]
    let native = from_env("LOCALAPPDATA")
        .or_else(|| app_data(false))
        .or_else(|| absolute(dirs::cache_dir()));
    #[cfg(not(windows))]
    let native = base(|home| home.join(".cache"), dirs::cache_dir());

    native.unwrap_or_else(fallback).join(folder())
}

/// The app's folder in the local, non-roaming data: where videos saved to a collection without a
/// folder of its own go. They are large and there is no reason to sync them between machines.
pub fn local_data() -> PathBuf {
    #[cfg(windows)]
    let native = from_env("LOCALAPPDATA")
        .or_else(|| app_data(false))
        .or_else(|| absolute(dirs::data_local_dir()))
        .or_else(|| from_env("APPDATA"))
        .or_else(|| absolute(dirs::data_dir()));
    #[cfg(not(windows))]
    let native = base(|home| home.join(".local/share"), dirs::data_local_dir());

    native.unwrap_or_else(fallback).join(folder())
}

#[cfg(test)]
mod tests {
    #[test]
    fn folders_are_absolute() {
        for path in [super::config(), super::data(), super::cache(), super::local_data()] {
            assert!(path.is_absolute(), "{}", path.display());
            assert!(path.file_name().is_some(), "{}", path.display());
        }
    }
}

//! `cathode --install-desktop-entry`: makes a Cathode that isn't installed by a package
//! (the tarball, the AppImage, a cargo build) show up in the application menu and handle
//! `cathode://` links.

use std::path::{Path, PathBuf};
use std::process::Command;

const ID: &str = "io.github.andunieee.Cathode";
const ENTRY: &str = include_str!("../packaging/io.github.andunieee.Cathode.desktop");
const ICON: &str = include_str!("../assets/io.github.andunieee.Cathode.svg");

/// Quotes a path for the Exec key of a desktop entry.
fn quote(path: &Path) -> String {
    let path = path.to_string_lossy();
    let escaped: String = path
        .chars()
        .flat_map(|c| match c {
            '"' | '`' | '$' | '\\' => vec!['\\', '\\', c],
            '%' => vec!['%', '%'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

/// The file to launch: the AppImage itself when running from one, not its temporary mount.
fn executable() -> Result<PathBuf, String> {
    match std::env::var_os("APPIMAGE") {
        Some(path) => Ok(PathBuf::from(path)),
        None => std::env::current_exe().map_err(|e| e.to_string()),
    }
}

pub fn install() -> Result<(), String> {
    let data = dirs::data_dir().ok_or("no data directory")?;
    let applications = data.join("applications");
    let icons = data.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&applications).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&icons).map_err(|e| e.to_string())?;

    let entry = ENTRY.replace("Exec=cathode %U", &format!("Exec={} %U", quote(&executable()?)));
    let entry_path = applications.join(format!("{ID}.desktop"));
    std::fs::write(&entry_path, entry).map_err(|e| e.to_string())?;
    std::fs::write(icons.join(format!("{ID}.svg")), ICON).map_err(|e| e.to_string())?;

    // Best effort: the entry works without these, just not right away.
    let _ = Command::new("update-desktop-database").arg(&applications).status();
    let _ = Command::new("xdg-mime")
        .args(["default", &format!("{ID}.desktop"), "x-scheme-handler/cathode"])
        .status();
    let _ = Command::new("gtk-update-icon-cache")
        .args(["-q", "-t"])
        .arg(data.join("icons/hicolor"))
        .status();

    println!("Installed {}", entry_path.display());
    Ok(())
}

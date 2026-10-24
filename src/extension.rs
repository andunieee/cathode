//! The browser extension that sends YouTube pages to Cathode. Chrome can't install
//! one silently, so the files are written to a folder to load unpacked.

use std::path::PathBuf;

const FILES: [(&str, &str); 3] = [
    ("manifest.json", include_str!("../packaging/extension/manifest.json")),
    ("background.js", include_str!("../packaging/extension/background.js")),
    ("content.js", include_str!("../packaging/extension/content.js")),
];

pub fn directory() -> PathBuf {
    crate::paths::data()
        .join("extension")
}

/// Writes the extension, replacing an older copy, and returns its folder.
pub fn install() -> Result<PathBuf, String> {
    let dir = directory();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, content) in FILES {
        std::fs::write(dir.join(name), content).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

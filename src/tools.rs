//! The external programs Lumpla runs, kept in `<cache>/tools`.
//!
//! * `yt-dlp` is always managed by us and checked daily: it breaks every time YouTube changes
//!   something, so an old copy (including a distro's) is the most likely thing to fail.
//! * `ffmpeg` and `ffprobe` (yt-dlp needs them to merge video and audio) and `deno` (yt-dlp needs
//!   a JavaScript runtime to solve YouTube's challenges) are only downloaded when they can't be
//!   found in `PATH`. Once downloaded, they are checked weekly.
//!
//! Everything is checked against a SHA-256: the digest GitHub reports for release assets, or the
//! `.sha256` file next to the download for the macOS FFmpeg builds (which are not on GitHub).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::fetch::cache_dir;

const DAY: u64 = 24 * 60 * 60;
const STATE_FILE: &str = "state.json";

/// Where a download comes from.
enum Origin {
    /// An asset of the latest release, GitHub reports its digest.
    Github {
        repo: &'static str,
        asset: &'static str,
    },
    /// A URL redirecting to the latest build, which has a `.sha256` file next to it.
    Mirror { url: &'static str },
}

#[derive(Clone, Copy)]
enum Archive {
    /// The download is the executable itself.
    Raw,
    Zip,
    TarXz,
}

struct Package {
    origin: Origin,
    archive: Archive,
}

/// `program` with the extension executables have on this platform.
fn exe(program: &str) -> String {
    format!("{program}{}", std::env::consts::EXE_SUFFIX)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    YtDlp,
    /// Provides both `ffmpeg` and `ffprobe`.
    Ffmpeg,
    Deno,
}

impl Tool {
    const ALL: [Tool; 3] = [Tool::YtDlp, Tool::Ffmpeg, Tool::Deno];

    pub fn name(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Ffmpeg => "ffmpeg",
            Tool::Deno => "deno",
        }
    }

    /// The programs that must be runnable for this tool to count as available.
    fn programs(self) -> &'static [&'static str] {
        match self {
            Tool::YtDlp => &["yt-dlp"],
            Tool::Ffmpeg => &["ffmpeg", "ffprobe"],
            Tool::Deno => &["deno"],
        }
    }

    fn always_managed(self) -> bool {
        self == Tool::YtDlp
    }

    fn check_interval(self) -> u64 {
        match self {
            Tool::YtDlp => DAY,
            // The FFmpeg builds are about 150 MB and published daily, not worth chasing.
            Tool::Ffmpeg | Tool::Deno => 7 * DAY,
        }
    }

    /// What to download on this platform, `None` where there is no build.
    fn packages(self) -> Option<Vec<Package>> {
        use Archive::*;

        let github = |repo, asset, archive| Package {
            origin: Origin::Github { repo, asset },
            archive,
        };
        let mirror = |url| Package {
            origin: Origin::Mirror { url },
            archive: Zip,
        };

        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        Some(match (self, os, arch) {
            (Tool::YtDlp, "linux", "x86_64") => vec![github("yt-dlp/yt-dlp", "yt-dlp_linux", Raw)],
            (Tool::YtDlp, "linux", "aarch64") => {
                vec![github("yt-dlp/yt-dlp", "yt-dlp_linux_aarch64", Raw)]
            }
            (Tool::YtDlp, "windows", "x86_64") => vec![github("yt-dlp/yt-dlp", "yt-dlp.exe", Raw)],
            (Tool::YtDlp, "macos", _) => vec![github("yt-dlp/yt-dlp", "yt-dlp_macos", Raw)],

            (Tool::Ffmpeg, "linux", "x86_64") => vec![github(
                "yt-dlp/FFmpeg-Builds",
                "ffmpeg-master-latest-linux64-gpl.tar.xz",
                TarXz,
            )],
            (Tool::Ffmpeg, "linux", "aarch64") => vec![github(
                "yt-dlp/FFmpeg-Builds",
                "ffmpeg-master-latest-linuxarm64-gpl.tar.xz",
                TarXz,
            )],
            (Tool::Ffmpeg, "windows", "x86_64") => vec![github(
                "yt-dlp/FFmpeg-Builds",
                "ffmpeg-master-latest-win64-gpl.zip",
                Zip,
            )],
            (Tool::Ffmpeg, "windows", "aarch64") => vec![github(
                "yt-dlp/FFmpeg-Builds",
                "ffmpeg-master-latest-winarm64-gpl.zip",
                Zip,
            )],
            // No GitHub builds for the Mac, these are the ones linked from ffmpeg.org.
            (Tool::Ffmpeg, "macos", "aarch64") => vec![
                mirror("https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip"),
                mirror("https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffprobe.zip"),
            ],
            (Tool::Ffmpeg, "macos", "x86_64") => vec![
                mirror("https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffmpeg.zip"),
                mirror("https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffprobe.zip"),
            ],

            (Tool::Deno, "linux", "x86_64") => {
                vec![github("denoland/deno", "deno-x86_64-unknown-linux-gnu.zip", Zip)]
            }
            (Tool::Deno, "linux", "aarch64") => {
                vec![github("denoland/deno", "deno-aarch64-unknown-linux-gnu.zip", Zip)]
            }
            (Tool::Deno, "windows", "x86_64") => {
                vec![github("denoland/deno", "deno-x86_64-pc-windows-msvc.zip", Zip)]
            }
            (Tool::Deno, "windows", "aarch64") => {
                vec![github("denoland/deno", "deno-aarch64-pc-windows-msvc.zip", Zip)]
            }
            (Tool::Deno, "macos", "x86_64") => {
                vec![github("denoland/deno", "deno-x86_64-apple-darwin.zip", Zip)]
            }
            (Tool::Deno, "macos", "aarch64") => {
                vec![github("denoland/deno", "deno-aarch64-apple-darwin.zip", Zip)]
            }
            _ => return None,
        })
    }

    /// Whether it can be run, from our directory or from `PATH`.
    fn available(self) -> bool {
        self.programs()
            .iter()
            .all(|program| find(program).is_some())
    }

    /// Whether there is a copy of ours, as opposed to relying on `PATH`.
    fn managed(self) -> bool {
        self.programs()
            .iter()
            .all(|program| dir().join(exe(program)).is_file())
    }
}

pub fn dir() -> PathBuf {
    cache_dir().join("tools")
}

fn find(program: &str) -> Option<PathBuf> {
    let program = exe(program);
    let ours = dir().join(&program);
    if ours.is_file() {
        return Some(ours);
    }

    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(&program))
        .find(|path| path.is_file())
}

/// A command for one of the tools. Our directory comes first in `PATH`, so that yt-dlp finds the
/// ffmpeg and deno we downloaded as well.
pub fn command(program: &str) -> Command {
    let mut paths = vec![dir()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }

    // Found by hand: where the program is looked up when the PATH given to the child differs
    // from ours is not the same on every platform.
    let mut command = Command::new(find(program).unwrap_or_else(|| program.into()));
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW, or a console flashes up on each run
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
}

/// What the startup check is up to, for the user to see.
#[derive(Clone)]
pub enum Status {
    Checking,
    /// `fraction` is `None` while the size is unknown or the file is being unpacked.
    Installing {
        tool: &'static str,
        fraction: Option<f64>,
    },
    /// The check is over. `errors` lists what could not be installed or updated.
    Done {
        errors: Vec<String>,
    },
}

/// Whether something needed is missing, which means requests have to wait for [`update`].
pub fn missing() -> bool {
    Tool::ALL.iter().any(|tool| {
        let usable = tool.available() && (!tool.always_managed() || tool.managed());
        !usable
    })
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    tools: std::collections::HashMap<String, Installed>,
}

#[derive(Serialize, Deserialize)]
struct Installed {
    /// The release tag, for display.
    version: String,
    /// The digest of the asset that was installed, what updates are detected with.
    digest: String,
    /// When we last asked GitHub, in seconds since the epoch.
    checked: u64,
}

fn read_state() -> State {
    std::fs::read(dir().join(STATE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_state(state: &State) {
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        let temporary = dir().join(format!("{STATE_FILE}.tmp"));
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(temporary, dir().join(STATE_FILE));
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Installs what is missing and updates what is old. Failing to *update* is not worth reporting
/// as long as the tool still works, so offline use is fine once everything was installed.
pub async fn update(report: impl Fn(Status)) {
    report(Status::Checking);
    let mut errors = Vec::new();

    if let Err(e) = std::fs::create_dir_all(dir()) {
        report(Status::Done {
            errors: vec![format!("Failed to create {}: {e}", dir().display())],
        });
        return;
    }

    let client = match reqwest::Client::builder()
        .user_agent(concat!("lumpla/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            report(Status::Done {
                errors: vec![e.to_string()],
            });
            return;
        }
    };

    let mut state = read_state();

    for tool in Tool::ALL {
        let name = tool.name();
        let Some(packages) = tool.packages() else {
            if !tool.available() {
                errors.push(format!(
                    "No {name} build for {} {}, install it yourself",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
            }
            continue;
        };

        let usable = tool.available() && (!tool.always_managed() || tool.managed());
        let installed = state.tools.get(name);

        // On PATH is good enough for the ones that aren't always managed, and then we leave
        // alone what the user has. Ours, once downloaded, are kept up to date.
        let due = if usable {
            tool.managed()
                && installed
                    .is_none_or(|i| now().saturating_sub(i.checked) >= tool.check_interval())
        } else {
            true
        };
        if !due {
            continue;
        }

        let result = install(
            &client,
            tool,
            &packages,
            installed.map(|i| i.digest.as_str()),
            &report,
        )
        .await;
        match result {
            Ok(Some(new)) => {
                state.tools.insert(name.to_string(), new);
                write_state(&state);
            }
            Ok(None) => {
                // Up to date, only remember we asked.
                if let Some(installed) = state.tools.get_mut(name) {
                    installed.checked = now();
                    write_state(&state);
                }
            }
            Err(e) if usable => eprintln!("Failed to update {name}: {e}"),
            Err(e) => errors.push(format!("Failed to install {name}: {e}")),
        }
    }

    report(Status::Done { errors });
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    updated_at: Option<String>,
}

/// A package at its latest version.
struct Resolved {
    url: String,
    /// The expected SHA-256, when known.
    sha256: Option<String>,
    /// What tells this version apart from the others.
    identity: String,
    /// For display.
    version: String,
}

async fn get(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
    client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())
}

async fn resolve(client: &reqwest::Client, package: &Package) -> Result<Resolved, String> {
    match package.origin {
        Origin::Github { repo, asset } => {
            let bytes = get(
                client,
                &format!("https://api.github.com/repos/{repo}/releases/latest"),
            )
            .await?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
            let release: Release = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

            let found = release
                .assets
                .into_iter()
                .find(|a| a.name == asset)
                .ok_or_else(|| format!("release {} has no {asset}", release.tag_name))?;

            let sha256 = found
                .digest
                .as_deref()
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(str::to_string);
            // Without a digest, the upload time still tells when it changed.
            let identity = sha256
                .clone()
                .or(found.updated_at)
                .unwrap_or_else(|| release.tag_name.clone());

            Ok(Resolved {
                url: found.browser_download_url,
                sha256,
                identity,
                version: release.tag_name,
            })
        }
        Origin::Mirror { url } => {
            // The redirect is read rather than followed: the address it leads to contains the
            // version, and the checksum lives next to it.
            let plain = reqwest::Client::builder()
                .user_agent(concat!("lumpla/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())?;
            let response = plain.get(url).send().await.map_err(|e| e.to_string())?;
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .ok_or("the download address did not redirect")?;
            let target = response
                .url()
                .join(location)
                .map_err(|e| e.to_string())?
                .to_string();

            let checksum = get(client, &format!("{target}.sha256"))
                .await?
                .text()
                .await
                .map_err(|e| e.to_string())?;
            let sha256 = checksum
                .split_whitespace()
                .next()
                .filter(|hash| hash.len() == 64)
                .ok_or("unreadable checksum file")?
                .to_string();

            // `.../<timestamp>_<version>/ffmpeg.zip`
            let version = target
                .rsplit('/')
                .nth(1)
                .and_then(|folder| folder.split_once('_'))
                .map_or_else(|| target.clone(), |(_, version)| version.to_string());

            Ok(Resolved {
                url: target,
                sha256: Some(sha256.clone()),
                identity: sha256,
                version,
            })
        }
    }
}

/// Downloads `tool` unless `current` is already the latest. Returns what was installed.
async fn install(
    client: &reqwest::Client,
    tool: Tool,
    packages: &[Package],
    current: Option<&str>,
    report: &impl Fn(Status),
) -> Result<Option<Installed>, String> {
    let name = tool.name();

    let mut resolved = Vec::new();
    for package in packages {
        resolved.push(resolve(client, package).await?);
    }
    let identity = resolved
        .iter()
        .map(|r| r.identity.as_str())
        .collect::<Vec<_>>()
        .join(",");

    // The binaries could have been deleted since, then it's not up to date.
    if current == Some(identity.as_str()) && tool.managed() {
        return Ok(None);
    }

    report(Status::Installing {
        tool: name,
        fraction: None,
    });

    for (index, (package, resolved)) in packages.iter().zip(&resolved).enumerate() {
        // The percentage of the whole, when there are several files.
        let share = |fraction: f64| (index as f64 + fraction) / packages.len() as f64;

        let download = dir().join(format!("{name}.{index}.download"));
        let result = fetch(
            client,
            &resolved.url,
            &download,
            resolved.sha256.as_deref(),
            |fraction| {
                report(Status::Installing {
                    tool: name,
                    fraction: fraction.map(share),
                })
            },
        )
        .await;

        let result = match result {
            Ok(()) => {
                report(Status::Installing {
                    tool: name,
                    fraction: None,
                });
                let (source, archive) = (download.clone(), package.archive);
                tokio::task::spawn_blocking(move || unpack(tool, archive, &source))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            }
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&download);
        result?;
    }

    if !tool.managed() {
        return Err("the downloaded files did not have the expected programs".to_string());
    }

    Ok(Some(Installed {
        version: resolved
            .iter()
            .map(|r| r.version.as_str())
            .collect::<Vec<_>>()
            .join(","),
        digest: identity,
        checked: now(),
    }))
}

/// Downloads `url` into `path`, verifying its SHA-256 when it is known.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    expected: Option<&str>,
    progress: impl Fn(Option<f64>),
) -> Result<(), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;

    let total = response.content_length().filter(|&n| n > 0);
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut last = 0.0;

    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        done += chunk.len() as u64;

        if let Some(total) = total {
            let fraction = done as f64 / total as f64;
            // Not every chunk is worth a UI update.
            if fraction - last >= 0.01 {
                last = fraction;
                progress(Some(fraction));
            }
        }
    }
    file.flush().map_err(|e| e.to_string())?;

    if let Some(expected) = expected {
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("the download is corrupted (checksum mismatch)".to_string());
        }
    }
    Ok(())
}

/// Puts the executables of a downloaded file in place, each one atomically. From an archive,
/// the programs of `tool` are taken wherever they are in it.
fn unpack(tool: Tool, archive: Archive, source: &Path) -> Result<(), String> {
    let open = || std::fs::File::open(source).map_err(|e| e.to_string());
    let wanted = |path: &Path| {
        let file_name = path.file_name()?.to_str()?;
        tool.programs()
            .iter()
            .copied()
            .find(|program| exe(program) == file_name)
    };

    match archive {
        Archive::Raw => place(tool.programs()[0], &mut open()?),
        Archive::Zip => {
            let mut archive = zip::ZipArchive::new(open()?).map_err(|e| e.to_string())?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
                let program = entry
                    .is_file()
                    .then(|| entry.enclosed_name())
                    .flatten()
                    .and_then(|p| wanted(&p));
                if let Some(program) = program {
                    place(program, &mut entry)?;
                }
            }
            Ok(())
        }
        Archive::TarXz => {
            let mut archive = tar::Archive::new(liblzma::read::XzDecoder::new(open()?));
            for entry in archive.entries().map_err(|e| e.to_string())? {
                let mut entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?.into_owned();
                if let Some(program) = entry
                    .header()
                    .entry_type()
                    .is_file()
                    .then(|| wanted(&path))
                    .flatten()
                {
                    place(program, &mut entry)?;
                }
            }
            Ok(())
        }
    }
}

/// Writes `content` as the executable `program` in the tools directory, through a temporary file
/// so that a running copy keeps working and a failure never leaves a half-written one.
fn place(program: &str, content: &mut impl Read) -> Result<(), String> {
    let program = exe(program);
    let target = dir().join(&program);
    let temporary = dir().join(format!("{program}.part"));

    let mut write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        std::io::copy(content, &mut file)?;
        file.flush()?;
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))?;
        }
        // A running program can be renamed on Windows but not replaced.
        #[cfg(windows)]
        {
            let old = dir().join(format!("{program}.old"));
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&target, old);
        }
        std::fs::rename(&temporary, &target)
    };

    write().map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("failed to write {program}: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the network. `XDG_CACHE_HOME` and `PATH` should point to empty places.
    #[tokio::test]
    #[ignore]
    async fn installs_everything() {
        assert!(missing());
        update(|_| {}).await;
        assert!(!missing());
        for program in ["yt-dlp", "ffmpeg", "ffprobe", "deno"] {
            assert!(dir().join(exe(program)).is_file(), "{program}");
        }
    }

    /// The other platforms' downloads, as far as they can be checked from here.
    #[tokio::test]
    #[ignore]
    async fn resolves_other_platforms() {
        let client = reqwest::Client::builder()
            .user_agent("lumpla-test")
            .build()
            .unwrap();
        let github = |repo, asset| Package {
            origin: Origin::Github { repo, asset },
            archive: Archive::Zip,
        };

        for package in [
            github("yt-dlp/yt-dlp", "yt-dlp.exe"),
            github("yt-dlp/yt-dlp", "yt-dlp_macos"),
            github("yt-dlp/FFmpeg-Builds", "ffmpeg-master-latest-win64-gpl.zip"),
            github(
                "yt-dlp/FFmpeg-Builds",
                "ffmpeg-master-latest-winarm64-gpl.zip",
            ),
            github("denoland/deno", "deno-x86_64-pc-windows-msvc.zip"),
            github("denoland/deno", "deno-aarch64-pc-windows-msvc.zip"),
            github("denoland/deno", "deno-x86_64-apple-darwin.zip"),
            github("denoland/deno", "deno-aarch64-apple-darwin.zip"),
        ] {
            let resolved = resolve(&client, &package).await.unwrap();
            assert!(resolved.sha256.is_some(), "{}", resolved.url);
        }

        // The Mac builds are zips with the bare executable inside.
        for arch in ["arm64", "amd64"] {
            for program in ["ffmpeg", "ffprobe"] {
                let url: &'static str =
                    Box::leak(format!("https://ffmpeg.martin-riedl.de/redirect/latest/macos/{arch}/release/{program}.zip").into());
                let package = Package {
                    origin: Origin::Mirror { url },
                    archive: Archive::Zip,
                };
                let resolved = resolve(&client, &package).await.unwrap();
                println!("{arch} {program}: {} {}", resolved.version, resolved.url);
                assert_eq!(resolved.sha256.as_ref().map(String::len), Some(64));
            }
        }
        let package = Package {
            origin: Origin::Mirror {
                url:
                    "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffprobe.zip",
            },
            archive: Archive::Zip,
        };
        let resolved = resolve(&client, &package).await.unwrap();
        std::fs::create_dir_all(dir()).unwrap();
        let path = dir().join("ffprobe.zip");
        fetch(
            &client,
            &resolved.url,
            &path,
            resolved.sha256.as_deref(),
            |_| {},
        )
        .await
        .unwrap();
        unpack(Tool::Ffmpeg, Archive::Zip, &path).unwrap();
        assert!(dir().join("ffprobe").is_file());
    }
}

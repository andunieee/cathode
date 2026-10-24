//! One Cathode per profile. The first one holds a lock file for as long as it lives and
//! listens on a local port; later ones hand it what they were asked to open and exit.
//! The port and a secret that senders must prove they know are in `instance.json` in the
//! data directory, which only the user can read.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

#[derive(Serialize, Deserialize)]
struct Message {
    token: String,
    /// Links and absolute file paths to open. Empty means just show the window.
    targets: Vec<String>,
}

pub struct Primary {
    listener: TcpListener,
    token: String,
    /// Held so the lock lasts until the process ends, which also releases it if it crashes.
    _lock: File,
}

pub enum Role {
    Primary(Primary),
    /// Another instance of this profile is running.
    Secondary,
}

fn endpoint_path() -> PathBuf {
    crate::paths::data().join("instance.json")
}

/// Takes the lock for this profile, or finds that someone else has it.
pub fn acquire() -> Result<Role, String> {
    let dir = crate::paths::data();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(Role::Secondary),
        Err(std::fs::TryLockError::Error(e)) => return Err(e.to_string()),
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let endpoint = Endpoint {
        port: listener.local_addr().map_err(|e| e.to_string())?.port(),
        token: token.clone(),
    };

    // Written aside and renamed so a sender never reads half of it.
    let path = endpoint_path();
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    file.write_all(&serde_json::to_vec(&endpoint).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;

    Ok(Role::Primary(Primary {
        listener,
        token,
        _lock: lock,
    }))
}

/// Gives the running instance what to open. False if there is none that answers.
pub fn send(targets: &[String]) -> bool {
    let Ok(bytes) = std::fs::read(endpoint_path()) else {
        return false;
    };
    let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes) else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(
        &(Ipv4Addr::LOCALHOST, endpoint.port).into(),
        Duration::from_secs(1),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let message = Message {
        token: endpoint.token,
        targets: targets.to_vec(),
    };
    let Ok(mut line) = serde_json::to_vec(&message) else {
        return false;
    };
    line.push(b'\n');
    if stream.write_all(&line).is_err() {
        return false;
    }

    // The answer says the message was accepted, not that the instance acted on it yet.
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).is_ok() && answer.trim() == "ok"
}

/// Like `send`, for an instance that is still starting and may not have written its port.
pub fn send_waiting(targets: &[String]) -> bool {
    for _ in 0..25 {
        if send(targets) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

impl Primary {
    /// Calls `on_targets`, from a background thread, with what each later launch asks for.
    pub fn listen(self, on_targets: impl Fn(Vec<String>) + Send + 'static) {
        std::thread::spawn(move || {
            // Keeps the lock for as long as this thread, so the whole process.
            let Primary { listener, token, _lock } = self;
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                // Not a line of ours, or not from someone who read the file: dropped.
                if reader.by_ref().take(1 << 20).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Message>(&line) else {
                    continue;
                };
                if message.token != token {
                    continue;
                }
                let _ = (&stream).write_all(b"ok\n");
                on_targets(message.targets);
            }
            drop(_lock);
        });
    }
}

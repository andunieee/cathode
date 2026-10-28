//! Controlling Cathode from a phone. With the remote turned on, Cathode answers over iroh to
//! hyperb browsers, which show the page of `remote_app`. Only paired phones get the page, known by
//! the key they connect with. A phone pairs with the address of the QR code,
//! `hyperb://<endpoint id>/#k=<code>`, whose code comes in the params of its `hello`. A code
//! pairs one phone and is never kept: a new one is made when it is used and each time the server
//! starts, so only the keys of the phones are saved.
//!
//! The server lives on a thread of its own, the player and the queue on the main one. Commands go
//! over to the main thread, and the state of the player comes back as a snapshot that is sent to
//! the phones that are connected.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use hypereffect::{LineBuffer, Outbox, RendererMessage};
use iroh::endpoint::{presets, Incoming, RecvStream};
use iroh::{Endpoint, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::{watch, Notify};

use crate::fetch::{Entry, Request, Response, SearchFilter, SearchSort, Source};
use crate::allowed;
use crate::nav;
use crate::profile;
use crate::subscriptions;
use crate::remote_app::RemoteApp;

/// The prefix of the ALPN of hyperb sessions over iroh.
const ALPN: &str = "hyperb/0";
/// The keys of the listings that are searches of the phone, so `nav` leaves them alone.
const KEY_PREFIX: &str = "remote:";
/// How many results a search sends back.
const RESULTS: usize = 10;
/// How many results a search of what the restricted mode allows sends back.
const ALLOWED_RESULTS: usize = 50;
/// Results to remember, for the phone to add them to the queue.
const REMEMBERED: usize = 200;
/// How long a phone has to say hello before it is let go.
const HELLO_WAIT: Duration = Duration::from_secs(10);

/// What the player and the queue are up to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The id of the video in the player.
    pub playing: Option<String>,
    pub title: String,
    pub channel: String,
    pub paused: bool,
    pub position: f64,
    pub duration: f64,
    pub queue: Vec<QueueRow>,
    /// The latest videos of the subscriptions, none in the restricted mode.
    pub feed: Vec<Hit>,
    /// The volume from 0 to 1.
    pub volume: f64,
    pub speed: f64,
    /// The height wanted for the video, `0` being the best there is.
    pub size: u32,
    /// The caption tracks of the video, by id and name.
    pub captions: Vec<(i64, String)>,
    /// The id of the caption track shown, `-1` when there is none.
    pub caption: i64,
    /// The chapters of the video, by title and start.
    pub chapters: Vec<(String, f64)>,
    /// Whether the restricted mode is on.
    pub restricted: bool,
    /// Whether a password guards the restricted mode.
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueRow {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub state: &'static str,
}

/// A search result, as the phone sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: String,
    pub title: String,
    /// The channel, the length and so on.
    pub details: String,
}

/// What a phone is told besides the status.
pub enum Reply {
    /// The results of a search.
    Results(Result<Vec<Hit>, String>),
    /// More results of the search of what the restricted mode allows numbered `search`, and
    /// whether that was all of them.
    Allowed {
        search: u64,
        hits: Vec<Hit>,
        done: bool,
    },
    /// Something the phone asked could not be done.
    Failed(String),
}

pub enum Command {
    /// Resumes when paused, pauses otherwise.
    Toggle,
    Stop,
    Next,
    Previous,
    Jump(String),
    /// Adds a video of the last searches to the queue.
    Add(String),
    Search {
        query: String,
        reply: UnboundedSender<Reply>,
    },
    /// Searches what the restricted mode allows. `search` is echoed back in the replies.
    SearchAllowed {
        query: String,
        search: u64,
        reply: UnboundedSender<Reply>,
    },
    /// The volume from 0 to 1.
    Volume(f64),
    Speed(f64),
    /// The height of the video, `0` being the best there is.
    Size(u32),
    /// Shows a caption track, `-1` for none.
    Caption(i64),
    /// Plays a chapter of the video, by its index.
    Chapter(usize),
    /// Turns the restricted mode on or off.
    SetRestricted {
        on: bool,
        password: String,
        reply: UnboundedSender<Reply>,
    },
    /// Sends the status again, a phone just connected.
    Refresh,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    enabled: bool,
    /// The secret key of the iroh endpoint, in hex. Its public key is the address of Cathode.
    secret: String,
    /// The phones let in.
    devices: Vec<Device>,
}

/// A paired phone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// The public key the phone connects with, in hex.
    pub key: String,
    /// When it paired, in seconds since the epoch.
    pub paired: i64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn random_hex(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    getrandom::fill(&mut bytes).expect("system random number generator");
    hex(&bytes)
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0u8; 32];
    if text.len() != 64 {
        return None;
    }
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

impl Config {
    fn path() -> PathBuf {
        crate::paths::config().join("remote.json")
    }

    fn load() -> Self {
        let mut config: Config = std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if unhex(&config.secret).is_none() {
            config.secret = hex(&SecretKey::generate().to_bytes());
            config.save();
        }
        config
    }

    /// The file holds the keys, only the user gets to read it.
    fn save(&self) {
        let path = Self::path();
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(temporary, &path)
        })();
        if let Err(error) = result {
            crate::log::info!("could not write {}: {error}", path.display());
        }
    }

    fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&unhex(&self.secret).expect("checked on load"))
    }

    fn keys(&self) -> HashSet<String> {
        self.devices.iter().map(|device| device.key.clone()).collect()
    }
}

/// What the server threads and the main one both reach.
struct Shared {
    /// The code of the address in the QR code, good for pairing one phone.
    code: Mutex<String>,
    /// The keys of the paired phones. A phone is let go as soon as its key is taken out.
    devices: watch::Sender<HashSet<String>>,
    status: watch::Sender<Status>,
    /// How many phones are connected.
    sessions: AtomicUsize,
    stop: Notify,
}

struct Pending {
    reply: UnboundedSender<Reply>,
    hits: Vec<Hit>,
}

impl Pending {
    fn finish(self, result: Result<Vec<Hit>, String>) {
        let _ = self.reply.send(Reply::Results(result));
    }
}

#[derive(Default)]
struct Local {
    config: Option<Config>,
    shared: Option<Arc<Shared>>,
    searches: HashMap<String, Pending>,
    searched: u64,
    /// The search of what the restricted mode allows running for each phone, by the sender of its
    /// replies.
    allowed: Vec<(UnboundedSender<Reply>, u64)>,
    /// The videos of the latest searches by id, in the order they came.
    found: HashMap<String, Entry>,
    found_order: Vec<String>,
    /// What to call when the code or the paired phones change, until it returns `false`.
    listeners: Vec<Box<dyn Fn() -> bool>>,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local::default());
}

fn with<R>(f: impl FnOnce(&mut Local) -> R) -> R {
    LOCAL.with(|local| f(&mut local.borrow_mut()))
}

/// Starts the server if the remote is turned on, and keeps the phones up to date.
pub fn init() {
    with(|local| local.config = Some(Config::load()));
    if enabled() {
        start();
    }
    glib::timeout_add_seconds_local(1, || {
        push_status();
        glib::ControlFlow::Continue
    });
}

pub fn enabled() -> bool {
    with(|local| local.config.as_ref().is_some_and(|config| config.enabled))
}

pub fn set_enabled(enabled: bool) {
    with(|local| {
        if let Some(config) = &mut local.config {
            config.enabled = enabled;
            config.save();
        }
    });
    if enabled {
        start();
    } else {
        stop();
    }
}

/// The address that pairs a phone, for the QR code. Empty while the remote is off.
pub fn address() -> String {
    with(|local| {
        let (Some(config), Some(shared)) = (&local.config, &local.shared) else {
            return String::new();
        };
        let code = shared.code.lock().unwrap();
        format!("hyperb://{}/#k={code}", config.secret_key().public())
    })
}

/// Makes a new code, the old address does not pair anymore. Paired phones stay paired.
pub fn new_code() {
    if let Some(shared) = with(|local| local.shared.clone()) {
        *shared.code.lock().unwrap() = random_hex(16);
    }
    changed();
}

/// The paired phones, the latest last.
pub fn devices() -> Vec<Device> {
    with(|local| local.config.as_ref().map(|config| config.devices.clone()).unwrap_or_default())
}

/// Forgets a phone, and lets it go if it is connected.
pub fn unpair(key: &str) {
    with(|local| {
        if let Some(config) = &mut local.config {
            config.devices.retain(|device| device.key != key);
            config.save();
        }
        if let Some(shared) = &local.shared {
            shared.devices.send_modify(|keys| {
                keys.remove(key);
            });
        }
    });
    changed();
}

/// Calls `listener` when the code or the paired phones change, until it returns `false`.
pub fn connect_changed(listener: impl Fn() -> bool + 'static) {
    with(|local| local.listeners.push(Box::new(listener)));
}

fn changed() {
    // The listeners are called out of `LOCAL`, they read it.
    let mut listeners = with(|local| std::mem::take(&mut local.listeners));
    listeners.retain(|listener| listener());
    with(|local| {
        listeners.append(&mut local.listeners);
        local.listeners = listeners;
    });
}

/// Saves a phone that just paired, on the main thread.
fn paired(key: String) {
    with(|local| {
        let Some(config) = &mut local.config else {
            return;
        };
        if !config.devices.iter().any(|device| device.key == key) {
            config.devices.push(Device {
                key,
                paired: glib::real_time() / 1_000_000,
            });
            config.save();
        }
    });
    changed();
}

fn start() {
    if with(|local| local.shared.is_some()) {
        return;
    }
    let (secret, keys) = with(|local| {
        let config = local.config.as_ref().expect("initialized");
        (config.secret_key(), config.keys())
    });
    let shared = Arc::new(Shared {
        code: Mutex::new(random_hex(16)),
        devices: watch::channel(keys).0,
        status: watch::channel(Status::default()).0,
        sessions: AtomicUsize::new(0),
        stop: Notify::new(),
    });
    with(|local| local.shared = Some(shared.clone()));
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to start tokio runtime");
        runtime.block_on(serve(secret, shared));
    });
}

fn stop() {
    if let Some(shared) = with(|local| local.shared.take()) {
        shared.stop.notify_one();
    }
}

/// Sends the status to the phones, if there are any and it changed.
fn push_status() {
    let Some(shared) = with(|local| local.shared.clone()) else {
        return;
    };
    if shared.sessions.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut status = nav::remote_status();
    status.position = status.position.floor();
    shared.status.send_if_modified(|old| {
        let changed = *old != status;
        if changed {
            *old = status;
        }
        changed
    });
}

/// Does what a phone asked, from any thread.
pub fn command(command: Command) {
    glib::MainContext::default().invoke(move || run(command));
}

fn run(command: Command) {
    match command {
        Command::Toggle => {
            let player = &crate::ui::get().player;
            if player.id().is_none() || player.is_paused() {
                nav::remote_play();
            } else {
                nav::remote_pause();
            }
        }
        Command::Stop => nav::stop_player(),
        Command::Next => nav::queue_step(1),
        Command::Previous => nav::queue_step(-1),
        Command::Jump(id) => nav::queue_play(&id),
        Command::Add(id) => {
            let found = with(|local| local.found.get(&id).cloned())
                .or_else(|| subscriptions::feed().into_iter().find(|entry| entry.id == id));
            if let Some(entry) = found {
                nav::queue_add(entry);
            }
        }
        Command::Volume(volume) => nav::remote_volume(volume),
        Command::Speed(speed) => nav::remote_speed(speed),
        Command::Size(height) => nav::set_size(height),
        Command::Caption(id) => nav::remote_caption(id),
        Command::Chapter(index) => nav::play_playing_chapter(index),
        Command::SetRestricted {
            on,
            password,
            reply,
        } => {
            if profile::has_password() && !profile::check_password(&password) {
                let _ = reply.send(Reply::Failed("Wrong password".into()));
            } else if on != profile::restricted() {
                nav::set_restricted(on);
            }
        }
        Command::Search {
            query,
            reply,
        } => {
            let key = with(|local| {
                local.searched += 1;
                let key = format!("{KEY_PREFIX}{}", local.searched);
                local.searches.insert(
                    key.clone(),
                    Pending {
                        reply,
                        hits: Vec::new(),
                    },
                );
                key
            });
            nav::send(Request::Listing {
                key,
                source: Source::Search {
                    query,
                    sort: SearchSort::Relevance,
                    filter: SearchFilter::None,
                },
            });
        }
        Command::SearchAllowed {
            query,
            search,
            reply,
        } => {
            // A phone sees the results of its latest search only.
            let before: Vec<u64> = with(|local| {
                let (same, others) = std::mem::take(&mut local.allowed)
                    .into_iter()
                    .partition(|(sender, _)| sender.same_channel(&reply));
                local.allowed = others;
                same.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
            });
            for id in before {
                allowed::cancel(id);
            }
            let sender = reply.clone();
            let mut sent = 0;
            let id = allowed::start(
                &query,
                Box::new(move |entries, done| {
                    if sent >= ALLOWED_RESULTS {
                        return;
                    }
                    let hits: Vec<Hit> = with(|local| {
                        entries
                            .into_iter()
                            .take(ALLOWED_RESULTS - sent)
                            .map(|entry| {
                                let hit = hit(&entry);
                                remember(local, entry);
                                hit
                            })
                            .collect()
                    });
                    sent += hits.len();
                    let _ = sender.send(Reply::Allowed {
                        search,
                        hits,
                        done: done || sent >= ALLOWED_RESULTS,
                    });
                }),
            );
            with(|local| local.allowed.push((reply, id)));
        }
        Command::Refresh => {}
    }
    push_status();
}

/// A video as the phone sees it.
pub fn hit(entry: &Entry) -> Hit {
    let mut details = nav::details(entry);
    if !entry.channel.is_empty() {
        details = if details.is_empty() {
            entry.channel.clone()
        } else {
            format!("{}  ·  {details}", entry.channel)
        };
    }
    Hit {
        id: entry.id.clone(),
        title: entry.title.clone(),
        details,
    }
}

/// Keeps a video the phone was shown, for it to add it to the queue.
fn remember(local: &mut Local, entry: Entry) {
    if local.found.insert(entry.id.clone(), entry.clone()).is_none() {
        local.found_order.push(entry.id);
    }
    while local.found_order.len() > REMEMBERED {
        let old = local.found_order.remove(0);
        local.found.remove(&old);
    }
}

/// Takes the responses to the searches of the phones, hands back the others.
pub fn intercept(response: Response) -> Option<Response> {
    let key = match &response {
        Response::Entries { key, .. }
        | Response::Channels { key, .. }
        | Response::ListingDone { key, .. } => key,
        _ => return Some(response),
    };
    if !key.starts_with(KEY_PREFIX) {
        return Some(response);
    }
    match response {
        Response::Entries { key, entries } => with(|local| {
            let Some(pending) = local.searches.get_mut(&key) else {
                return;
            };
            let mut shown = Vec::new();
            for entry in entries {
                if pending.hits.len() >= RESULTS {
                    break;
                }
                if !nav::video_allowed(&entry) || pending.hits.iter().any(|hit| hit.id == entry.id) {
                    continue;
                }
                pending.hits.push(hit(&entry));
                shown.push(entry);
            }
            for entry in shown {
                remember(local, entry);
            }
            if local.searches[&key].hits.len() >= RESULTS {
                let pending = local.searches.remove(&key).unwrap();
                let hits = pending.hits.clone();
                pending.finish(Ok(hits));
            }
        }),
        Response::ListingDone { key, error } => with(|local| {
            if let Some(pending) = local.searches.remove(&key) {
                let result = match error {
                    Some(error) if pending.hits.is_empty() => Err(error),
                    _ => Ok(pending.hits.clone()),
                };
                pending.finish(result);
            }
        }),
        _ => {}
    }
    None
}

async fn serve(secret: SecretKey, shared: Arc<Shared>) {
    let endpoint = match Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.as_bytes().to_vec()])
        .bind()
        .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            crate::log::info!("the remote is not listening: {error}");
            return;
        }
    };
    crate::log::info!("the remote is listening as {}", endpoint.id());
    loop {
        tokio::select! {
            _ = shared.stop.notified() => break,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(error) = session(incoming, shared).await {
                            crate::log::info!("remote session: {error}");
                        }
                    });
                }
                None => break,
            },
        }
    }
    endpoint.close().await;
}

/// Counts a connected phone.
struct Connected(Arc<Shared>);

impl Connected {
    fn new(shared: Arc<Shared>) -> Self {
        shared.sessions.fetch_add(1, Ordering::Relaxed);
        command(Command::Refresh);
        Self(shared)
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.0.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Lets a phone in if it is paired, or pairs it if it has the code, which can't be used again.
fn admit(shared: &Shared, key: &str, params: &BTreeMap<String, String>) -> bool {
    if shared.devices.borrow().contains(key) {
        return true;
    }
    let mut code = shared.code.lock().unwrap();
    if params.get("k") != Some(&*code) {
        return false;
    }
    *code = random_hex(16);
    drop(code);
    shared.devices.send_modify(|keys| {
        keys.insert(key.to_owned());
    });
    let key = key.to_owned();
    glib::MainContext::default().invoke(move || paired(key));
    true
}

/// Reads up to the `hello`, for its params and the lines that came after it.
async fn hello(
    recv: &mut RecvStream,
    input: &mut LineBuffer,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut buffer = [0u8; 4096];
    loop {
        let count = recv
            .read(&mut buffer)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("closed before the hello")?;
        let mut lines = input.push(&buffer[..count]).into_iter();
        while let Some(line) = lines.next() {
            if let Ok(RendererMessage::Hello { params }) = RendererMessage::parse(&line) {
                return Ok((params, lines.collect()));
            }
        }
    }
}

async fn session(incoming: Incoming, shared: Arc<Shared>) -> Result<(), String> {
    let connection = incoming
        .accept()
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| e.to_string())?;
    let key = connection.remote_id().to_string();
    let (mut send, mut recv) = connection.accept_bi().await.map_err(|e| e.to_string())?;
    let mut input = LineBuffer::new();
    let (params, rest) = tokio::time::timeout(HELLO_WAIT, hello(&mut recv, &mut input))
        .await
        .map_err(|_| "no hello")??;
    let mut out = Outbox::new();
    if !admit(&shared, &key, &params) {
        crate::log::info!("a phone that is not paired was turned away: {key}");
        RemoteApp::unpaired(&mut out);
        let _ = send.write_all(out.take().as_bytes()).await;
        let _ = send.finish();
        // Gives the page the time to get there before the connection goes.
        let _ = tokio::time::timeout(HELLO_WAIT, send.stopped()).await;
        return Ok(());
    }
    crate::log::info!("a phone connected: {key}");
    let _connected = Connected::new(shared.clone());

    let (replies, mut results) = unbounded_channel();
    let mut app = RemoteApp::new(replies);
    let mut statuses = shared.status.subscribe();
    let mut devices = shared.devices.subscribe();
    let status = statuses.borrow_and_update().clone();
    app.hello(&mut out, &status);
    let mut lines = rest;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        for line in lines.drain(..) {
            if let Ok(RendererMessage::Action(action)) = RendererMessage::parse(&line) {
                app.action(&mut out, &action);
            }
        }
        let text = out.take();
        if !text.is_empty() {
            send.write_all(text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::select! {
            read = recv.read(&mut buffer) => {
                let Ok(Some(count)) = read else { break };
                lines = input.push(&buffer[..count]);
            }
            Ok(()) = statuses.changed() => {
                let status = statuses.borrow_and_update().clone();
                app.status(&mut out, &status);
            }
            Ok(()) = devices.changed() => {
                if !devices.borrow_and_update().contains(&key) {
                    crate::log::info!("a phone was unpaired: {key}");
                    connection.close(0u32.into(), b"unpaired");
                    return Ok(());
                }
            }
            Some(reply) = results.recv() => app.reply(&mut out, reply),
        }
    }
    let _ = send.finish();
    Ok(())
}

/// The QR code of the address, black on white with the quiet zone it needs.
pub fn qr_code(text: &str) -> Option<gdk::Texture> {
    use qrcode::{Color, QrCode};

    const SCALE: usize = 8;
    const QUIET: usize = 4;
    let code = QrCode::new(text).ok()?;
    let width = code.width();
    let side = (width + 2 * QUIET) * SCALE;
    let colors = code.to_colors();
    let mut pixels = vec![255u8; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            let (mx, my) = (x / SCALE, y / SCALE);
            let dark = mx >= QUIET
                && my >= QUIET
                && mx < QUIET + width
                && my < QUIET + width
                && colors[(my - QUIET) * width + (mx - QUIET)] == Color::Dark;
            if dark {
                let at = (y * side + x) * 4;
                pixels[at..at + 3].fill(0);
            }
        }
    }
    Some(
        gdk::MemoryTexture::new(
            side as i32,
            side as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(pixels),
            side * 4,
        )
        .upcast(),
    )
}


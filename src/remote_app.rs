//! The page the phone shows: what is playing, the controls, the queue and a search. It is a
//! hypereffect app, so any hyperb browser can render it. The state lives in Cathode, the app
//! only turns what the phone does into `Command`s and what Cathode reports into signals.

use hypereffect::{Action, Handler, Outbox, Value, cel, json, ui};
use tokio::sync::mpsc::UnboundedSender;

use crate::remote::{self, Command, Hit, Reply, Status};
use crate::settings::QUALITIES;
use crate::ui::SPEEDS;

pub struct RemoteApp {
    replies: UnboundedSender<Reply>,
    /// Whether the restricted mode was on in the last status.
    restricted: bool,
    /// The caption tracks the select of the page was made from.
    captions: Vec<(i64, String)>,
    /// The chapters the rows of the chapters card were made from.
    chapters: Vec<(String, f64)>,
    /// What the rows of the queue on the page were made from, one line for each.
    queue: Vec<String>,
    /// The ids of the results on the page.
    results: Vec<String>,
    /// The videos of the feed on the page.
    feed: Vec<Hit>,
    /// The ids of the results of the search of what the restricted mode allows.
    allowed: Vec<String>,
    /// The number of the latest search of what the restricted mode allows, older ones' results
    /// are dropped.
    allowed_search: u64,
}

/// A string for an expression.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    match seconds / 3600 {
        0 => format!("{}:{:02}", seconds / 60, seconds % 60),
        hours => format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
    }
}

impl RemoteApp {
    pub fn new(replies: UnboundedSender<Reply>) -> Self {
        Self {
            replies,
            restricted: false,
            captions: Vec::new(),
            chapters: Vec::new(),
            allowed: Vec::new(),
            allowed_search: 0,
            feed: Vec::new(),
            queue: Vec::new(),
            results: Vec::new(),
        }
    }

    /// What a phone that is not paired gets instead of the page.
    pub fn unpaired(out: &mut Outbox) {
        out.element(ui::column("main"));
        out.element(
            ui::text("title")
                .parent("main")
                .variant("h3")
                .text("This phone is not paired"),
        );
        out.element(
            ui::text("how")
                .parent("main")
                .text("Open Remote Control in the settings of Cathode and scan the QR code."),
        );
    }

    pub fn hello(&mut self, out: &mut Outbox, status: &Status) {
        out.signal(json!({
            "playing": false,
            "paused": true,
            "title": "",
            "channel": "",
            "progress": 0.0,
            "time": "",
            "current": "",
            "queued": [],
            "searching": false,
            "error": "",
            "query": "",
            "volume": 100.0,
            "speed": "1",
            "size": "0",
            "caption": "-1",
            "chaptered": false,
            "chapter": -1,
            "restricted": false,
            "locked": false,
            "password": "",
            "restrict_error": "",
            "allowed_query": "",
            "allowed_searching": false,
            "allowed_error": "",
        }));
        out.element(ui::column("main"));

        out.element(ui::card("now").parent("main").gap("small"));
        out.element(
            ui::text("title")
                .parent("now")
                .variant("h3")
                .text(cel("s.title != '' ? s.title : 'Nothing playing'")),
        );
        out.element(
            ui::text("channel")
                .parent("now")
                .variant("caption")
                .visible(cel("s.channel != ''"))
                .text(cel("s.channel")),
        );
        out.element(
            ui::progress("bar")
                .parent("now")
                .visible(cel("s.playing"))
                .value(cel("s.progress"))
                .label(cel("s.time")),
        );
        out.element(ui::row("controls").parent("now").justify("between"));
        for (id, label, action) in [
            ("previous", "Previous", "previous"),
            ("stop", "Stop", "stop"),
            ("next", "Next", "next"),
        ] {
            out.element(
                ui::button(id)
                    .parent("controls")
                    .label(label)
                    .variant("plain")
                    .on_press(Handler::action(action)),
            );
            if id == "previous" {
                out.element(
                    ui::button("toggle")
                        .parent("controls")
                        .variant("primary")
                        .label(cel("s.paused ? 'Play' : 'Pause'"))
                        .on_press(Handler::action("toggle")),
                );
            }
        }

        out.element(
            ui::card("chapters")
                .parent("main")
                .gap("small")
                .visible(cel("s.chaptered")),
        );
        out.element(
            ui::text("chapters_title")
                .parent("chapters")
                .variant("h3")
                .text("Chapters"),
        );
        out.element(ui::list("chapter_list").parent("chapters"));

        out.element(ui::card("player").parent("main").gap("small"));
        out.element(
            ui::slider("volume")
                .parent("player")
                .label("Volume")
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .bind("volume")
                .on_change(Handler::action("volume")),
        );
        out.element(
            ui::select("speed")
                .parent("player")
                .label("Speed")
                .bind("speed")
                .options(json!(SPEEDS.map(
                    |speed| json!({"value": speed.to_string(), "label": format!("{speed}×")})
                )))
                .on_change(Handler::action("speed")),
        );
        out.element(
            ui::select("size")
                .parent("player")
                .label("Video quality")
                .bind("size")
                .disabled(cel("!s.playing"))
                .options(json!(QUALITIES.map(
                    |(height, name)| json!({"value": height.to_string(), "label": name})
                )))
                .on_change(Handler::action("size")),
        );
        self.captions_select(out, &status.captions);

        out.element(ui::tabs("tabs").parent("main"));
        out.element(ui::tab("queue_tab").parent("tabs").label("Queue"));
        out.element(
            ui::list("queue")
                .parent("queue_tab")
                .empty("The queue is empty"),
        );
        out.element(
            ui::tab("search_tab")
                .parent("tabs")
                .label("Search")
                .visible(cel("!s.restricted")),
        );
        out.element(
            ui::input("query")
                .parent("search_tab")
                .bind("query")
                .placeholder("Search videos")
                .on_submit(Handler::action("search")),
        );
        out.element(
            ui::button("search")
                .parent("search_tab")
                .label("Search")
                .variant("primary")
                .disabled(cel("s.searching || s.query == ''"))
                .on_press(Handler::action("search")),
        );
        out.element(
            ui::progress("searching")
                .parent("search_tab")
                .variant("spinner")
                .visible(cel("s.searching")),
        );
        out.element(
            ui::notice("error")
                .parent("search_tab")
                .variant("failure")
                .visible(cel("s.error != ''"))
                .text(cel("s.error")),
        );
        out.element(ui::list("results").parent("search_tab"));

        out.element(
            ui::tab("feed_tab")
                .parent("tabs")
                .label("Feed")
                .visible(cel("!s.restricted")),
        );
        out.element(
            ui::list("feed")
                .parent("feed_tab")
                .empty("Nothing from the subscriptions"),
        );

        out.element(ui::tab("restricted_tab").parent("tabs").label("Restricted"));
        out.element(
            ui::checkbox("restricted")
                .parent("restricted_tab")
                .variant("switch")
                .label("Restricted mode")
                .bind("restricted")
                .on_change(Handler::action("restrict")),
        );
        out.element(
            ui::input("password")
                .parent("restricted_tab")
                .variant("password")
                .placeholder("Password")
                .bind("password")
                .visible(cel("s.locked")),
        );
        out.element(
            ui::notice("restrict_error")
                .parent("restricted_tab")
                .variant("failure")
                .visible(cel("s.restrict_error != ''"))
                .text(cel("s.restrict_error")),
        );
        out.element(
            ui::input("allowed_query")
                .parent("restricted_tab")
                .bind("allowed_query")
                .placeholder("Search the allowed videos")
                .visible(cel("s.restricted"))
                .on_submit(Handler::action("allowed_search")),
        );
        out.element(
            ui::button("allowed_search")
                .parent("restricted_tab")
                .label("Search")
                .variant("primary")
                .visible(cel("s.restricted"))
                .disabled(cel("s.allowed_query == ''"))
                .on_press(Handler::action("allowed_search")),
        );
        out.element(
            ui::progress("allowed_searching")
                .parent("restricted_tab")
                .variant("spinner")
                .visible(cel("s.restricted && s.allowed_searching")),
        );
        out.element(
            ui::notice("allowed_error")
                .parent("restricted_tab")
                .visible(cel("s.restricted && s.allowed_error != ''"))
                .text(cel("s.allowed_error")),
        );
        out.element(
            ui::list("allowed")
                .parent("restricted_tab")
                .visible(cel("s.restricted")),
        );

        self.status(out, status);
    }

    pub fn action(&mut self, out: &mut Outbox, action: &Action) {
        let id = || {
            action
                .payload
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        match action.name.as_str() {
            "toggle" => remote::command(Command::Toggle),
            "stop" => remote::command(Command::Stop),
            "next" => remote::command(Command::Next),
            "previous" => remote::command(Command::Previous),
            "jump" => {
                if let Some(id) = id() {
                    remote::command(Command::Jump(id));
                }
            }
            "add" => {
                if let Some(id) = id() {
                    remote::command(Command::Add(id));
                }
            }
            "chapter" => {
                if let Some(index) = action.payload.as_ref().and_then(Value::as_u64) {
                    remote::command(Command::Chapter(index as usize));
                }
            }
            "volume" => {
                if let Some(volume) = action.signal("volume").and_then(Value::as_f64) {
                    remote::command(Command::Volume(volume / 100.0));
                }
            }
            "speed" => {
                if let Some(speed) = action
                    .signal("speed")
                    .and_then(Value::as_str)
                    .and_then(|speed| speed.parse().ok())
                {
                    remote::command(Command::Speed(speed));
                }
            }
            "size" => {
                if let Some(height) = action
                    .signal("size")
                    .and_then(Value::as_str)
                    .and_then(|height| height.parse().ok())
                {
                    remote::command(Command::Size(height));
                }
            }
            "caption" => {
                if let Some(id) = action
                    .signal("caption")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse().ok())
                {
                    remote::command(Command::Caption(id));
                }
            }
            "restrict" => {
                let on = action
                    .signal("restricted")
                    .and_then(Value::as_bool)
                    .unwrap_or(self.restricted);
                let password = action
                    .signal("password")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                out.signal(json!({"restrict_error": "", "password": ""}));
                remote::command(Command::SetRestricted {
                    on,
                    password,
                    reply: self.replies.clone(),
                });
            }
            "allowed_search" => {
                let query = action
                    .signal("allowed_query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !query.is_empty() {
                    for id in self.allowed.drain(..) {
                        out.remove(format!("a_{id}"));
                    }
                    self.allowed_search += 1;
                    out.signal(json!({"allowed_searching": true, "allowed_error": ""}));
                    remote::command(Command::SearchAllowed {
                        query,
                        search: self.allowed_search,
                        reply: self.replies.clone(),
                    });
                }
            }
            "search" => {
                let query = action
                    .signal("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !query.is_empty() {
                    out.signal(json!({"searching": true, "error": ""}));
                    remote::command(Command::Search {
                        query,
                        reply: self.replies.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Shows the player and rebuilds the queue if it changed.
    pub fn status(&mut self, out: &mut Outbox, status: &Status) {
        let progress = if status.duration > 0.0 {
            (status.position / status.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.signal(json!({
            "playing": status.duration > 0.0,
            "paused": status.paused,
            "title": status.title,
            "channel": status.channel,
            "progress": progress,
            "time": format!("{} / {}", clock(status.position), clock(status.duration)),
            "current": status.playing.clone().unwrap_or_default(),
            "queued": status.queue.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            "volume": (status.volume * 100.0).round(),
            "speed": status.speed.to_string(),
            "size": status.size.to_string(),
            "caption": status.caption.to_string(),
            "chaptered": !status.chapters.is_empty(),
            // The chapter playing is the last one started.
            "chapter": status.chapters.iter().rposition(|(_, start)| *start <= status.position).map_or(-1, |index| index as i64),
            "restricted": status.restricted,
            "locked": status.locked,
        }));
        self.restricted = status.restricted;
        if status.captions != self.captions {
            self.captions_select(out, &status.captions);
        }

        if status.chapters != self.chapters {
            for index in 0..self.chapters.len() {
                out.remove(format!("c_{index}"));
            }
            for (index, (title, start)) in status.chapters.iter().enumerate() {
                let item = format!("c_{index}");
                out.element(
                    ui::row(item.as_str())
                        .parent("chapter_list")
                        .justify("between"),
                );
                out.element(
                    ui::column(format!("{item}_text"))
                        .parent(item.as_str())
                        .gap("none"),
                );
                out.element(
                    ui::text(format!("{item}_title"))
                        .parent(format!("{item}_text"))
                        .text(title.as_str()),
                );
                out.element(
                    ui::text(format!("{item}_start"))
                        .parent(format!("{item}_text"))
                        .variant("caption")
                        .text(clock(*start)),
                );
                out.element(
                    ui::button(format!("{item}_play"))
                        .parent(item.as_str())
                        .label(cel(format!("s.chapter == {index} ? 'Playing' : 'Play'")))
                        .on_press(Handler::action("chapter").payload(index as i64)),
                );
            }
            self.chapters = status.chapters.clone();
        }

        if status.feed != self.feed {
            for hit in self.feed.drain(..) {
                out.remove(format!("f_{}", hit.id));
            }
            for hit in &status.feed {
                hit_row(out, "f", "feed", hit);
            }
            self.feed = status.feed.clone();
        }

        // The titles can change (the original one arrives later), so rows are rebuilt for
        // those too: ids alone would leave the old text.
        let signature: Vec<String> = status
            .queue
            .iter()
            .map(|row| format!("{}\n{}\n{}", row.id, row.title, row.state))
            .collect();
        if signature == self.queue {
            return;
        }
        for id in &ids_of(&self.queue) {
            out.remove(format!("q_{id}"));
        }
        for row in &status.queue {
            let item = format!("q_{}", row.id);
            out.element(ui::row(item.as_str()).parent("queue").justify("between"));
            out.element(
                ui::column(format!("{item}_text"))
                    .parent(item.as_str())
                    .gap("none"),
            );
            out.element(
                ui::text(format!("{item}_title"))
                    .parent(format!("{item}_text"))
                    .text(row.title.as_str()),
            );
            let detail = match row.state {
                "ready" => row.channel.clone(),
                state => format!("{} · {state}", row.channel),
            };
            out.element(
                ui::text(format!("{item}_channel"))
                    .parent(format!("{item}_text"))
                    .variant("caption")
                    .text(detail),
            );
            out.element(
                ui::button(format!("{item}_play"))
                    .parent(item.as_str())
                    .label(cel(format!(
                        "s.current == {} ? 'Playing' : 'Play'",
                        quote(&row.id)
                    )))
                    .disabled(cel(format!("s.current == {}", quote(&row.id))))
                    .on_press(Handler::action("jump").payload(row.id.as_str())),
            );
        }
        self.queue = signature;
    }

    /// The select of the subtitles, which only has the tracks of the video playing.
    fn captions_select(&mut self, out: &mut Outbox, captions: &[(i64, String)]) {
        let options: Vec<Value> = std::iter::once(json!({"value": "-1", "label": "Off"}))
            .chain(
                captions
                    .iter()
                    .map(|(id, label)| json!({"value": id.to_string(), "label": label})),
            )
            .collect();
        out.element(
            ui::select("caption")
                .parent("player")
                .label("Subtitles")
                .bind("caption")
                .visible(!captions.is_empty())
                .options(Value::Array(options))
                .on_change(Handler::action("caption")),
        );
        self.captions = captions.to_vec();
    }

    pub fn reply(&mut self, out: &mut Outbox, reply: Reply) {
        match reply {
            Reply::Results(result) => self.results(out, result),
            Reply::Allowed { search, hits, done } => {
                if search != self.allowed_search {
                    return;
                }
                for hit in hits {
                    hit_row(out, "a", "allowed", &hit);
                    self.allowed.push(hit.id);
                }
                if done {
                    out.signal(json!({
                        "allowed_searching": false,
                        "allowed_error": if self.allowed.is_empty() { "Nothing found" } else { "" },
                    }));
                }
            }
            Reply::Failed(error) => out.signal(json!({
                "restrict_error": error,
                "restricted": self.restricted,
            })),
        }
    }

    fn results(&mut self, out: &mut Outbox, result: Result<Vec<Hit>, String>) {
        out.signal(json!({"searching": false}));
        for id in self.results.drain(..) {
            out.remove(format!("r_{id}"));
        }
        let hits = match result {
            Ok(hits) if hits.is_empty() => {
                out.signal(json!({"error": "Nothing found"}));
                return;
            }
            Ok(hits) => hits,
            Err(message) => {
                out.signal(json!({"error": message}));
                return;
            }
        };
        for hit in hits {
            hit_row(out, "r", "results", &hit);
            self.results.push(hit.id);
        }
    }
}

/// A row of a list of videos, with the button to queue it.
fn hit_row(out: &mut Outbox, prefix: &str, list: &str, hit: &Hit) {
    let item = format!("{prefix}_{}", hit.id);
    out.element(ui::row(item.as_str()).parent(list).justify("between"));
    out.element(
        ui::column(format!("{item}_text"))
            .parent(item.as_str())
            .gap("none"),
    );
    out.element(
        ui::text(format!("{item}_title"))
            .parent(format!("{item}_text"))
            .text(hit.title.as_str()),
    );
    out.element(
        ui::text(format!("{item}_details"))
            .parent(format!("{item}_text"))
            .variant("caption")
            .text(hit.details.as_str()),
    );
    let id = quote(&hit.id);
    out.element(
        ui::button(format!("{item}_add"))
            .parent(item.as_str())
            .label(cel(format!("{id} in s.queued ? 'Queued' : 'Add'")))
            .disabled(cel(format!("{id} in s.queued")))
            .on_press(Handler::action("add").payload(hit.id.as_str())),
    );
}

/// The ids in signatures made by `status`.
fn ids_of(signature: &[String]) -> Vec<String> {
    signature
        .iter()
        .filter_map(|line| line.split('\n').next().map(str::to_owned))
        .collect()
}

// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cache;
mod collections;
mod controls;
#[cfg(target_os = "linux")]
mod desktop;
mod extension;
mod fetch;
mod format;
mod history;
mod library;
mod log;
mod nav;
mod player;
mod preferences;
mod profile;
mod settings;
mod state;
mod subscriptions;
mod suggestions;
mod tools;
mod ui;

use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

/// Re-run ourselves in the background, in its own process group and with
/// stdio silenced, so the launching terminal is free again. Returns false if
/// that failed, in which case we just keep running in the foreground.
#[cfg(unix)]
fn detach() -> bool {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    Command::new(exe)
        .args(std::env::args().skip(1))
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
}

#[cfg(not(unix))]
fn detach() -> bool {
    false
}

/// Builds the window the first time it is needed, whether the app was opened
/// plainly or to open something.
fn start(app: &adw::Application, started: &Cell<bool>) {
    if started.replace(true) {
        return;
    }
    let ui = ui::build(app);

    let requests = fetch::spawn(|response| {
        glib::MainContext::default().invoke(move || nav::handle(response));
    });
    nav::init(requests);

    ui.window.present();
}

/// The bundle keeps the image loaders next to the executable; `cathode.bat` used to say so.
#[cfg(windows)]
fn point_pixbuf_at_bundle() {
    if std::env::var_os("GDK_PIXBUF_MODULE_FILE").is_some() {
        return;
    }
    if let Some(root) = std::env::current_exe().ok().and_then(|exe| exe.parent()?.parent().map(|p| p.to_path_buf())) {
        let cache = root.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache");
        if cache.exists() {
            std::env::set_var("GDK_PIXBUF_MODULE_FILE", cache);
        }
    }
}

fn main() -> glib::ExitCode {
    // Cathode detaches from the terminal unless --foreground is given.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--install-desktop-entry") {
        #[cfg(target_os = "linux")]
        if let Err(error) = desktop::install() {
            eprintln!("Could not install the desktop entry: {error}");
            return glib::ExitCode::FAILURE;
        }
        #[cfg(not(target_os = "linux"))]
        {
            eprintln!("--install-desktop-entry only works on Linux");
            return glib::ExitCode::FAILURE;
        }
        return glib::ExitCode::SUCCESS;
    }
    #[cfg(windows)]
    point_pixbuf_at_bundle();
    let foreground = args.iter().any(|a| a == "--foreground");
    if !foreground && detach() {
        return glib::ExitCode::SUCCESS;
    }
    log::init();
    log::info!("arguments: {:?}", &args[1..]);

    // A second launch hands its files and links to the running one.
    let app = adw::Application::builder()
        .application_id("io.github.andunieee.Cathode")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|_| load_css());

    let started = Rc::new(Cell::new(false));

    app.connect_activate({
        let started = started.clone();
        move |app| {
            start(app, &started);
            ui::get().window.present();
        }
    });

    app.connect_open(move |app, files, _| {
        start(app, &started);
        for file in files {
            log::info!("asked to open {}", file.uri());
            // Links from the browser, `cathode://` or plain YouTube ones, or files to play.
            match file.path() {
                Some(path) if file.is_native() => nav::open_file(path),
                _ => nav::open_link(&file.uri()),
            }
        }
        ui::get().window.present();
    });

    // Only ours are passed on, GTK doesn't get to see the rest.
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--foreground")
        .collect();
    app.run_with_args(&args)
}

/// Clickable widgets get a pointer cursor, the way they do on the web.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "button, button.toggle, stackswitcher button, checkbutton, switch, scale, link, row.activatable,
         listview.rich-list > row, tab, dropdown { cursor: pointer; }
         button:disabled, checkbutton:disabled, row:disabled { cursor: default; }
         row.queue-playing { background-color: alpha(@accent_bg_color, 0.25); }
         scale.seek-bar slider { min-width: 12px; min-height: 12px; margin: -4px; }
         row.queue-playing .title { font-weight: bold; }
         label.player-message { color: white; font-weight: bold; font-size: 1.4em;
           text-shadow: -1px -1px 0 black, 1px -1px 0 black, -1px 1px 0 black, 1px 1px 0 black,
             0 -1px 0 black, 0 1px 0 black, -1px 0 0 black, 1px 0 0 black; }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

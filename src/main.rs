// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod allowed;
mod cache;
mod collections;
mod controls;
#[cfg(target_os = "linux")]
mod desktop;
mod extension;
mod fetch;
mod format;
mod history;
mod instance;
mod library;
mod log;
mod nav;
mod paths;
mod player;
mod preferences;
mod profile;
mod remote;
mod remote_app;
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
    // In an AppImage our executable lives in a mount (or an extracted directory) that the
    // AppImage runtime removes as soon as we exit, so re-run the AppImage itself, which gets its
    // own. When extracted, both runtimes would share the same directory and ours would still
    // delete it under the new one: stay in the foreground then.
    let exe = match std::env::var_os("APPIMAGE") {
        Some(_)
            if std::env::var("APPDIR").is_ok_and(|dir| dir.contains("/appimage_extracted_")) =>
        {
            return false;
        }
        Some(appimage) => appimage.into(),
        None => match std::env::current_exe() {
            Ok(exe) => exe,
            Err(_) => return false,
        },
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
    remote::init();

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

/// What the command line asks for.
struct Arguments {
    profile: Option<String>,
    foreground: bool,
    /// Links and absolute file paths to open.
    targets: Vec<String>,
}

fn is_link(target: &str) -> bool {
    target.starts_with("cathode:") || target.contains("://")
}

fn parse_arguments(args: &[String]) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        profile: None,
        foreground: false,
        targets: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" => parsed.foreground = true,
            "--profile" => {
                parsed.profile = Some(args.next().cloned().ok_or("--profile needs a name")?)
            }
            _ if arg.starts_with("--profile=") => {
                parsed.profile = Some(arg["--profile=".len()..].to_string())
            }
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            // Paths are made absolute here, the running instance has another working directory.
            _ if is_link(arg) => parsed.targets.push(arg.clone()),
            _ => parsed.targets.push(
                std::fs::canonicalize(arg)
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| arg.clone()),
            ),
        }
    }
    Ok(parsed)
}

/// Opens links from the browser, `cathode://` or plain YouTube ones, and files to play.
fn open_targets(targets: &[String]) {
    for target in targets {
        log::info!("asked to open {target}");
        if is_link(target) {
            nav::open_link(target);
        } else {
            nav::open_file(target.into());
        }
    }
    ui::get().window.present();
}

fn main() -> glib::ExitCode {
    // The AppImage's GTK plugin forces `GTK_THEME=Adwaita:light|dark`, which swaps libadwaita's
    // stylesheet for GTK's legacy one (no padding, wrong widgets). Drop it, libadwaita follows the
    // system's light/dark preference by itself.
    if std::env::var_os("APPIMAGE").is_some()
        && matches!(std::env::var("GTK_THEME").as_deref(), Ok("Adwaita:light" | "Adwaita:dark"))
    {
        // SAFETY: nothing else is running yet.
        unsafe { std::env::remove_var("GTK_THEME") };
    }
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
    let arguments = match parse_arguments(&args[1..]) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Err(error) = paths::set_profile(arguments.profile.clone()) {
        eprintln!("{error}");
        return glib::ExitCode::FAILURE;
    }
    #[cfg(windows)]
    point_pixbuf_at_bundle();

    // Cathode detaches from the terminal unless --foreground is given. If one is running
    // already, it is told what to open and this one is done.
    if !arguments.foreground {
        if instance::send(&arguments.targets) || detach() {
            return glib::ExitCode::SUCCESS;
        }
    }
    log::init();
    log::info!("arguments: {:?}", &args[1..]);

    // Only one Cathode per profile. This also catches two launching at the same moment.
    let primary = match instance::acquire() {
        Ok(instance::Role::Primary(primary)) => primary,
        Ok(instance::Role::Secondary) => {
            return if instance::send_waiting(&arguments.targets) {
                glib::ExitCode::SUCCESS
            } else {
                eprintln!("Cathode is already running but does not answer");
                glib::ExitCode::FAILURE
            };
        }
        Err(error) => {
            eprintln!("Could not check for a running Cathode: {error}");
            return glib::ExitCode::FAILURE;
        }
    };

    // The instance check above is ours; GApplication's would only get in the way.
    let app = adw::Application::builder()
        .application_id("io.github.andunieee.Cathode")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|_| load_css());

    let started = Rc::new(Cell::new(false));
    let primary = Cell::new(Some(primary));
    let targets = arguments.targets;

    app.connect_activate(move |app| {
        start(app, &started);
        open_targets(&targets);
        if let Some(primary) = primary.take() {
            primary.listen(|targets| {
                glib::MainContext::default().invoke(move || open_targets(&targets));
            });
        }
    });

    // GTK doesn't get to see our options.
    app.run_with_args(&args[..1])
}

/// Clickable widgets get a pointer cursor, the way they do on the web.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "button, button.toggle, stackswitcher button, checkbutton, switch, scale, link, row.activatable,
         listview.rich-list > row, tab, dropdown { cursor: pointer; }
         button:disabled, checkbutton:disabled, row:disabled { cursor: default; }
         row.queue-playing { background-color: alpha(@accent_bg_color, 0.25); }
         box.player-controls { transition: opacity 200ms; }
         box.player-controls.faded { opacity: 0; }
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

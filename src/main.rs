// No console window behind the GUI on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cache;
mod collections;
mod controls;
mod fetch;
mod format;
mod history;
mod library;
mod nav;
mod player;
mod preferences;
mod profile;
mod settings;
mod state;
mod subscriptions;
mod tools;
mod ui;

use adw::prelude::*;
use gtk::glib;

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.github.andunieee.Cathode")
        .build();

    app.connect_startup(|_| load_css());

    app.connect_activate(|app| {
        let ui = ui::build(app);

        let requests = fetch::spawn(|response| {
            glib::MainContext::default().invoke(move || nav::handle(response));
        });
        nav::init(requests);

        // Opening with a file argument plays it straight away.
        if let Some(file) = std::env::args().nth(1) {
            nav::open_file(file.into());
        }

        ui.window.present();
    });

    // The arguments are ours, GTK doesn't get to see them.
    app.run_with_args::<&str>(&[])
}

/// Clickable widgets get a pointer cursor, the way they do on the web.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "button, checkbutton, switch, scale, link, row.activatable,
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

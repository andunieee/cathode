mod cache;
mod fetch;
mod format;
mod nav;
mod player;
mod ui;

use adw::prelude::*;
use gtk::glib;

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.dusklabs.Lumpla")
        .build();

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

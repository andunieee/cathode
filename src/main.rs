pub mod fetch;
pub mod nav;
pub mod video;

slint::include_modules!();

use std::os::raw::c_void;
use std::sync::Mutex;

use video::VideoUnderlay;

/// File waiting to be handed to the video underlay on the next frame. It is written from the UI
/// thread and read from the rendering notifier, which also runs on the UI thread.
pub static PENDING_FILE: Mutex<Option<String>> = Mutex::new(None);

pub fn main() {
    let app = App::new().unwrap();
    let app_weak = app.as_weak();

    // Opening with a file argument skips the search page.
    if let Some(file) = std::env::args().nth(1) {
        *PENDING_FILE.lock().unwrap() = Some(file);
        app.set_playing(true);
    }

    let requests = fetch::spawn({
        let app_weak = app_weak.clone();
        move |response| {
            let _ = app_weak.upgrade_in_event_loop(move |app| nav::handle(&app, response));
        }
    });
    nav::init(&app, requests);

    // Runs `f` with the app, for callbacks that only hold a weak handle to it.
    let with_app = |f: fn(&App)| {
        let app_weak = app_weak.clone();
        move || {
            if let Some(app) = app_weak.upgrade() {
                f(&app);
            }
        }
    };

    app.on_search({
        let app_weak = app_weak.clone();
        move |query, by_date| {
            if let Some(app) = app_weak.upgrade() {
                nav::search(&app, &query, by_date);
            }
        }
    });
    app.on_search_channel({
        let app_weak = app_weak.clone();
        move |query| {
            if let Some(app) = app_weak.upgrade() {
                nav::search_channel(&app, &query);
            }
        }
    });
    app.on_open_video({
        let app_weak = app_weak.clone();
        move |id| {
            if let Some(app) = app_weak.upgrade() {
                nav::open_video(&app, &id);
            }
        }
    });
    app.on_open_channel({
        let app_weak = app_weak.clone();
        move |id, name| {
            if let Some(app) = app_weak.upgrade() {
                nav::open_channel(&app, &id, &name);
            }
        }
    });
    app.on_go_back(with_app(nav::back));
    app.on_go_forward(with_app(nav::forward));
    app.on_play_video(with_app(nav::play));

    let mut underlay: Option<VideoUnderlay> = None;
    let mut prev_is_paused = false;

    if let Err(error) = app
        .window()
        .set_rendering_notifier(move |state, graphics_api| {
            let wh = if let Some(app) = app_weak.upgrade() {
                let sf = app.window().scale_factor();
                let phys_width = app.get_window_width() * sf;
                let phys_height = app.get_window_height() * sf;
                (phys_width, phys_height)
            } else {
                (2000.0, 2000.0)
            };

            match state {
                slint::RenderingState::BeforeRendering => {
                    let Some(app) = app_weak.upgrade() else {
                        return;
                    };

                    if let Some(file) = PENDING_FILE.lock().unwrap().take() {
                        match underlay.as_ref() {
                            Some(underlay) => underlay.load(&file),
                            None => {
                                let (context, proc_addr) = match graphics_api {
                                    slint::GraphicsAPI::NativeOpenGL { get_proc_address } => unsafe {
                                        (
                                            glow::Context::from_loader_function(|s| {
                                                get_proc_address(&std::ffi::CString::new(s).unwrap())
                                            }),
                                            get_proc_address,
                                        )
                                    },
                                    _ => return,
                                };

                                underlay = Some(VideoUnderlay::new(
                                    context,
                                    proc_addr as *const _ as *mut c_void,
                                    &file,
                                    wh,
                                ));
                                prev_is_paused = false;
                            }
                        }
                    }

                    let Some(underlay) = underlay.as_mut() else {
                        return;
                    };

                    let is_paused = app.get_is_paused();
                    if prev_is_paused != is_paused {
                        if is_paused {
                            underlay.pause();
                        } else {
                            underlay.play();
                        }

                        prev_is_paused = is_paused;
                    }

                    // Back on the search page, nothing to draw.
                    if !app.get_playing() {
                        return;
                    }

                    if !app.get_position_ackd() {
                        app.set_position_ackd(true);
                        let duration = underlay.get_duration().unwrap_or(0) as f64;
                        let seek_target = app.get_new_position() as f64 / 100.0 * duration;
                        let _ = underlay
                            .get_mpv()
                            .command("seek", &[&seek_target.to_string(), "absolute"]);
                    }

                    app.set_ts_label(underlay.get_ts_label().into());

                    let position = underlay.get_position().unwrap_or(0) as f32;
                    let duration = underlay.get_duration().unwrap_or(0) as f32;

                    if duration > 0.0 {
                        app.set_seek_position(position / duration);
                    }
                    underlay.render(wh);
                    app.window().request_redraw();
                }
                slint::RenderingState::RenderingTeardown => {
                    drop(underlay.take());
                }
                _ => {}
            }
        })
    {
        match error {
            slint::SetRenderingNotifierError::Unsupported => eprintln!("This example requires the use of the GL backend. Please run with the environment variable SLINT_BACKEND=GL set."),
            _ => unreachable!()
        }
        std::process::exit(1);
    }

    app.run().unwrap();
}

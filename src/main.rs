pub mod fetch;
pub mod video;

slint::include_modules!();

use std::os::raw::c_void;
use std::sync::Mutex;

use fetch::{Request, Response};
use slint::{Model, ModelRc, VecModel};
use video::VideoUnderlay;

/// File waiting to be handed to the video underlay on the next frame. It is written from the UI
/// thread and read from the rendering notifier, which also runs on the UI thread.
static PENDING_FILE: Mutex<Option<String>> = Mutex::new(None);

fn format_duration(secs: Option<f64>) -> String {
    let Some(secs) = secs else {
        return "live".into();
    };

    let secs = secs as u64;
    let (hours, minutes, seconds) = (secs / 3600, (secs / 60) % 60, secs % 60);

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

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
            let _ = app_weak.upgrade_in_event_loop(move |app| {
                if !matches!(response, Response::Thumbnail { .. }) {
                    app.set_busy(false);
                }

                match response {
                    Response::Results(Ok(entries)) => {
                        app.set_status(if entries.is_empty() {
                            "No results".into()
                        } else {
                            "".into()
                        });

                        let results: Vec<SearchResult> = entries
                            .into_iter()
                            .map(|entry| SearchResult {
                                id: entry.id.into(),
                                url: entry.url.into(),
                                title: entry.title.into(),
                                uploader: entry.uploader.into(),
                                duration: format_duration(entry.duration).into(),
                                thumbnail: slint::Image::default(),
                            })
                            .collect();
                        app.set_results(ModelRc::new(VecModel::from(results)));
                    }
                    Response::Thumbnail { id, image } => {
                        let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                            &image.rgba,
                            image.width,
                            image.height,
                        );

                        // The row may be gone if a newer search replaced the results.
                        let results = app.get_results();
                        if let Some(row) = results.iter().position(|result| result.id == id) {
                            let mut result = results.row_data(row).unwrap();
                            result.thumbnail = slint::Image::from_rgba8(buffer);
                            results.set_row_data(row, result);
                        }
                    }
                    Response::Downloaded(Ok(path)) => {
                        *PENDING_FILE.lock().unwrap() = Some(path.to_string_lossy().into_owned());
                        app.set_status("".into());
                        app.set_is_paused(false);
                        app.set_playing(true);
                    }
                    Response::Results(Err(error)) => {
                        app.set_status(format!("Search failed: {error}").into())
                    }
                    Response::Downloaded(Err(error)) => {
                        app.set_status(format!("Download failed: {error}").into())
                    }
                }
            });
        }
    });

    app.on_search({
        let app_weak = app_weak.clone();
        let requests = requests.clone();
        move |query| {
            let query = query.trim();
            if query.is_empty() {
                return;
            }

            if let Some(app) = app_weak.upgrade() {
                app.set_busy(true);
                app.set_status("Searching…".into());
            }
            let _ = requests.send(Request::Search(query.to_string()));
        }
    });

    app.on_play({
        let app_weak = app_weak.clone();
        move |result| {
            if let Some(app) = app_weak.upgrade() {
                app.set_busy(true);
                app.set_status(format!("Downloading {}…", result.title).into());
            }
            let _ = requests.send(Request::Download {
                id: result.id.to_string(),
                url: result.url.to_string(),
            });
        }
    });

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

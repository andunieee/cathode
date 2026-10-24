//! libmpv rendering into a `GtkGLArea`.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CString};
use std::mem::transmute;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use libmpv2::render::*;
use libmpv2::*;

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_VIEWPORT: u32 = 0x0BA2;

/// Looks up OpenGL functions through whichever of EGL, GLX (Linux), WGL
/// (Windows) or the OpenGL framework (macOS) the GTK renderer is using. Values that the proc-address entry
/// point returns NULL for (e.g. GL 1.x core functions) fall back to a dlsym
/// of the same library.
struct GlLoader {
    _library: libloading::Library,
    /// `None` on macOS, where there is no such function and the symbols are looked up directly.
    get_proc_address: Option<GetProcAddress>,
}

static GL_LOADER: OnceLock<Option<GlLoader>> = OnceLock::new();

fn gl_loader() -> Option<&'static GlLoader> {
    GL_LOADER
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            // SAFETY: loading the system's OpenGL framework runs nothing of ours.
            if let Ok(library) = unsafe {
                libloading::Library::new("/System/Library/Frameworks/OpenGL.framework/OpenGL")
            } {
                return Some(GlLoader {
                    _library: library,
                    get_proc_address: None,
                });
            }

            for (library, symbol) in [
                ("libEGL.so.1", "eglGetProcAddress"),
                ("libGL.so.1", "glXGetProcAddressARB"),
                ("libGL.so.1", "glXGetProcAddress"),
                ("libEGL.dll", "eglGetProcAddress"),
                ("opengl32.dll", "wglGetProcAddress"),
            ] {
                unsafe {
                    let Ok(library) = libloading::Library::new(library) else {
                        continue;
                    };
                    let Ok(get_proc_address) = library.get::<GetProcAddress>(symbol.as_bytes())
                    else {
                        continue;
                    };
                    let get_proc_address = *get_proc_address;
                    return Some(GlLoader {
                        _library: library,
                        get_proc_address: Some(get_proc_address),
                    });
                }
            }
            None
        })
        .as_ref()
}

fn gl_proc_address(name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    let Some(loader) = gl_loader() else {
        return std::ptr::null_mut();
    };
    // SAFETY: `_library` stays loaded forever in `GL_LOADER`.
    let address = loader
        .get_proc_address
        .map_or(std::ptr::null_mut(), |get| unsafe { get(name.as_ptr()) });
    if !address.is_null() {
        return address;
    }
    // GL core functions (1.0/1.1) may only be available as direct exports of
    // the library, not through the proc-address entry point.
    // SAFETY: the library stays loaded forever, so the symbol stays valid,
    // and dereferencing a function-type `Symbol` yields the address itself.
    let fallback = unsafe {
        loader
            ._library
            .get::<unsafe extern "C" fn()>(name.as_bytes())
            .ok()
    };
    match fallback {
        // SAFETY: see above.
        Some(symbol) => *symbol as *mut c_void,
        None => std::ptr::null_mut(),
    }
}

fn mpv_get_proc_address(_ctx: &*mut c_void, name: &str) -> *mut c_void {
    gl_proc_address(name)
}

/// A caption track of the file loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionTrack {
    pub id: i64,
    pub label: String,
}

pub struct Player {
    pub area: gtk::GLArea,
    /// Whether the end of the current file was already reported.
    ended: Cell<bool>,
    /// Renders still wanted after a load, so that a paused video shows its first frame.
    settling: Cell<u32>,
    id: RefCell<Option<String>>,
    /// A file to load once the render context exists: without it mpv drops the video.
    waiting: RefCell<Option<String>>,
    this: std::rc::Weak<Player>,
    /// Caption files of the file loaded, `(track, path)`, until mpv has loaded the file and
    /// they can be added.
    pending_captions: RefCell<Vec<(String, std::path::PathBuf)>>,
    /// The caption files mpv has been given for the file loaded.
    added_captions: RefCell<Vec<std::path::PathBuf>>,
    /// The languages mpv picks a caption track from, like `pt`, none for any.
    preferred_languages: RefCell<String>,
    /// Whether captions are wanted, which holds from one file to the next.
    captions: Cell<bool>,
    /// Parts of the video to jump over, `(start, end)` in seconds.
    skips: RefCell<Vec<(f64, f64)>>,
    /// Fires when the next of those parts is reached.
    skip_timer: RefCell<Option<glib::SourceId>>,

    // NOTE: the render context has to be dropped before `mpv`.
    render: RefCell<Option<RenderContext<'static>>>,
    mpv: Mpv,
}

impl Player {
    pub fn new() -> Rc<Self> {
        // GTK picks the locale of the user, mpv wants numbers to be formatted the plain way.
        unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()) };

        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "libmpv")?;
            // Stay on the last frame at the end, `eof-reached` tells us when that happens.
            init.set_property("keep-open", "yes")?;
            // Subtitles are switched on from the interface.
            init.set_property("sid", "no")?;
            Ok(())
        })
        .expect("Error while creating MPV");

        let area = gtk::GLArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_has_depth_buffer(false);

        let player = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            pending_captions: RefCell::new(Vec::new()),
            added_captions: RefCell::new(Vec::new()),
            preferred_languages: RefCell::new(String::new()),
            captions: Cell::new(false),
            skips: RefCell::new(Vec::new()),
            skip_timer: RefCell::new(None),
            area,
            ended: Cell::new(false),
            settling: Cell::new(0),
            id: RefCell::new(None),
            waiting: RefCell::new(None),
            render: RefCell::new(None),
            mpv,
        });

        player.area.connect_realize({
            let player = Rc::downgrade(&player);
            move |area| {
                let Some(player) = player.upgrade() else {
                    return;
                };

                area.make_current();
                if area.error().is_some() {
                    return;
                }

                let render = player.mpv.create_render_context(vec![
                    RenderParam::ApiType(RenderParamApiType::OpenGl),
                    RenderParam::InitParams(OpenGLInitParams {
                        get_proc_address: mpv_get_proc_address,
                        ctx: std::ptr::null_mut::<c_void>(),
                    }),
                ]);

                match render {
                    // SAFETY: `render` is declared before `mpv` in `Player`, so it is dropped
                    // first, and it is also dropped on unrealize.
                    Ok(render) => {
                        *player.render.borrow_mut() = Some(unsafe {
                            transmute::<RenderContext<'_>, RenderContext<'static>>(render)
                        });
                        if let Some(file) = player.waiting.borrow_mut().take() {
                            let _ = player.mpv.command("loadfile", &[&file, "replace"]);
                            area.queue_render();
                        }
                    }
                    Err(error) => crate::log::error!("Failed creating the mpv render context: {error}"),
                }
            }
        });

        player.area.connect_unrealize({
            let player = Rc::downgrade(&player);
            move |area| {
                if let Some(player) = player.upgrade() {
                    area.make_current();
                    drop(player.render.borrow_mut().take());
                }
            }
        });

        player.area.connect_render({
            let player = Rc::downgrade(&player);
            move |area, _context| {
                let Some(player) = player.upgrade() else {
                    return glib::Propagation::Stop;
                };

                let get_integerv = gl_proc_address("glGetIntegerv");
                if get_integerv.is_null() {
                    return glib::Propagation::Stop;
                }
                // SAFETY: the symbol was looked up by that name.
                let get_integerv: GetIntegerv = unsafe { transmute(get_integerv) };

                // GTK renders into its own framebuffer, which is the one bound right now.
                let mut fbo = 0;
                unsafe { get_integerv(GL_FRAMEBUFFER_BINDING, &mut fbo) };

                // GTK sets the viewport to the size of its framebuffer, which is not always the widget
                // size times the integer scale factor (fractional scaling, other GL stacks): mpv
                // would lay the video and the controls out for the wrong size.
                let scale = area.scale_factor();
                let mut viewport = [0; 4];
                unsafe { get_integerv(GL_VIEWPORT, viewport.as_mut_ptr()) };
                let (width, height) = if viewport[2] > 0 && viewport[3] > 0 {
                    (viewport[2], viewport[3])
                } else {
                    (area.width() * scale, area.height() * scale)
                };
                if let Some(render) = player.render.borrow().as_ref() {
                    let _ = render.update();
                    let _ = render.render::<*mut c_void>(
                        fbo,
                        width,
                        height,
                        true,
                    );
                    // The first frame of a file comes after the loading, and nothing redraws
                    // while paused: keep drawing until the video has a position for a while.
                    if player.mpv.get_property::<f64>("time-pos").is_ok() {
                        player.settling.set(player.settling.get().saturating_sub(1));
                    }
                }

                glib::Propagation::Stop
            }
        });

        // Redraw on every frame while something is playing.
        player.area.add_tick_callback({
            let player = Rc::downgrade(&player);
            move |area, _clock| {
                if let Some(player) = player.upgrade() {
                    if area.is_mapped() && (!player.is_paused() || player.settling.get() > 0) {
                        area.queue_render();
                    }
                }
                glib::ControlFlow::Continue
            }
        });

        player
    }

    /// Id of the video loaded last.
    pub fn id(&self) -> Option<String> {
        self.id.borrow().clone()
    }

    /// Replaces the current file and starts playback.
    pub fn load(&self, id: &str, file: &str) {
        *self.id.borrow_mut() = Some(id.to_string());
        self.ended.set(false);
        self.settling.set(10);
        self.skips.borrow_mut().clear();
        self.cancel_skip();
        self.added_captions.borrow_mut().clear();
        *self.pending_captions.borrow_mut() =
            crate::cache::caption_files(std::path::Path::new(file));
        self.apply_captions();
        if self.render.borrow().is_some() {
            *self.waiting.borrow_mut() = None;
            let _ = self.mpv.command("loadfile", &[file, "replace"]);
        } else {
            *self.waiting.borrow_mut() = Some(file.to_string());
        }
        self.set_paused(false);
    }

    /// Unloads the file, nothing is loaded afterwards.
    pub fn stop(&self) {
        *self.id.borrow_mut() = None;
        self.ended.set(false);
        *self.waiting.borrow_mut() = None;
        self.pending_captions.borrow_mut().clear();
        self.added_captions.borrow_mut().clear();
        self.skips.borrow_mut().clear();
        self.cancel_skip();
        let _ = self.mpv.command("stop", &[]);
        self.area.queue_render();
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.mpv.set_property("pause", paused);
        self.area.queue_render();
        self.schedule_skip(self.position());
    }

    pub fn is_paused(&self) -> bool {
        self.mpv.get_property::<bool>("pause").unwrap_or(true)
    }

    /// Playback speed, 1.0 is the normal one.
    pub fn set_speed(&self, speed: f64) {
        let _ = self.mpv.set_property("speed", speed);
        self.schedule_skip(self.position());
    }

    /// Volume from 0 to 100.
    pub fn set_volume(&self, volume: f64) {
        let _ = self.mpv.set_property("volume", volume);
    }

    pub fn is_muted(&self) -> bool {
        self.mpv.get_property::<bool>("mute").unwrap_or(false)
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.mpv.set_property("mute", muted);
    }

    /// Lets the file loaded have a caption file that turned up, it is added once mpv has
    /// loaded the file. Does nothing for a file that was added already.
    pub fn queue_caption(&self, track: String, path: std::path::PathBuf) {
        let known = self.added_captions.borrow().contains(&path)
            || self
                .pending_captions
                .borrow()
                .iter()
                .any(|(_, pending)| *pending == path);
        if !known {
            self.pending_captions.borrow_mut().push((track, path));
        }
    }

    /// Hands the caption files to mpv once it has loaded the file, they can't be added before.
    pub fn add_pending_captions(&self) {
        if self.pending_captions.borrow().is_empty() || self.duration() <= 0.0 {
            return;
        }
        let files = std::mem::take(&mut *self.pending_captions.borrow_mut());
        for (language, path) in files {
            self.added_captions.borrow_mut().push(path.clone());
            // "cached" adds the track without selecting it. The language is the plain one, like
            // `pt` for `pt-orig`, so that it matches what is preferred.
            let plain = language.split('-').next().unwrap_or(&language);
            let _ = self.mpv.command(
                "sub-add",
                &[
                    &path.to_string_lossy(),
                    "cached",
                    &crate::format::language_name(&language),
                    plain,
                ],
            );
        }
        self.apply_captions();
        let (tracks, selected) = self.caption_tracks();
        crate::log::info!(
            "captions: preferred {:?}, tracks {:?}, shown {selected:?}",
            self.preferred_languages.borrow(),
            tracks
                .iter()
                .map(|t| (t.id, t.label.as_str()))
                .collect::<Vec<_>>()
        );
        // With no language to go by, any track is better than none.
        if self.captions.get() && self.preferred_languages.borrow().is_empty() {
            if let (tracks, None) = self.caption_tracks() {
                if let Some(track) = tracks.first() {
                    let _ = self.mpv.set_property("sid", track.id);
                }
            }
        }
    }

    /// Turns captions on or off for this file and the next ones. On picks the track of the
    /// preferred languages, `languages` being a list like `en,fr`.
    pub fn set_captions(&self, wanted: bool, languages: &str) {
        self.captions.set(wanted);
        *self.preferred_languages.borrow_mut() = languages.to_string();
        let _ = self.mpv.set_property("slang", languages);
        self.apply_captions();
    }

    fn apply_captions(&self) {
        // Going through "no" makes mpv choose again, as the preferred languages may have changed.
        let _ = self.mpv.set_property("sid", "no");
        if self.captions.get() {
            let _ = self.mpv.set_property("sid", "auto");
        }
    }

    /// Shows this track, or the next file starts with the automatic choice again.
    pub fn select_caption(&self, id: i64) {
        let _ = self.mpv.set_property("sid", id);
        self.area.queue_render();
    }

    /// The caption tracks of the file, and the id of the one shown.
    pub fn caption_tracks(&self) -> (Vec<CaptionTrack>, Option<i64>) {
        let count = self
            .mpv
            .get_property::<i64>("track-list/count")
            .unwrap_or(0);
        let mut tracks = Vec::new();
        let mut selected = None;
        for index in 0..count {
            let property = |name: &str| {
                self.mpv
                    .get_property::<String>(&format!("track-list/{index}/{name}"))
                    .ok()
            };
            if property("type").as_deref() != Some("sub") {
                continue;
            }
            let Ok(id) = self
                .mpv
                .get_property::<i64>(&format!("track-list/{index}/id"))
            else {
                continue;
            };
            let label = property("title")
                .or_else(|| property("lang"))
                .unwrap_or_else(|| format!("Track {id}"));
            if self
                .mpv
                .get_property::<bool>(&format!("track-list/{index}/selected"))
                .unwrap_or(false)
            {
                selected = Some(id);
            }
            tracks.push(CaptionTrack { id, label });
        }
        (tracks, selected)
    }

    /// Moves one frame, which leaves the video paused.
    pub fn step_frame(&self, forward: bool) {
        let _ = self.mpv.command(
            if forward {
                "frame-step"
            } else {
                "frame-back-step"
            },
            &[],
        );
        self.area.queue_render();
        self.cancel_skip();
    }

    pub fn position(&self) -> f64 {
        self.mpv.get_property::<f64>("time-pos").unwrap_or(0.0)
    }

    pub fn duration(&self) -> f64 {
        self.mpv.get_property::<f64>("duration").unwrap_or(0.0)
    }

    pub fn seek_to(&self, seconds: f64) {
        let _ = self
            .mpv
            .command("seek", &[&seconds.to_string(), "absolute"]);
        self.area.queue_render();
        // The position only catches up with a seek later, so go by where it is going.
        self.schedule_skip(seconds);
    }

    /// Sets the parts to jump over in the video loaded. Starting inside one of them plays it.
    pub fn set_skips(&self, mut skips: Vec<(f64, f64)>) {
        skips.sort_by(|a, b| a.0.total_cmp(&b.0));
        *self.skips.borrow_mut() = skips;
        // Before the first position there is one, the start of the video counts as ahead.
        let position = self.mpv.get_property::<f64>("time-pos").unwrap_or(-0.001);
        self.schedule_skip(position);
    }

    fn cancel_skip(&self) {
        if let Some(timer) = self.skip_timer.borrow_mut().take() {
            timer.remove();
        }
    }

    /// Arranges to jump over the next skip part that starts after `position`, when the playback
    /// gets there. Nothing is scheduled while paused, resuming schedules again.
    fn schedule_skip(&self, position: f64) {
        self.cancel_skip();
        if self.is_paused() {
            return;
        }
        let next = self
            .skips
            .borrow()
            .iter()
            .find(|(start, _)| *start > position)
            .copied();
        let Some((start, end)) = next else {
            return;
        };

        let speed = self
            .mpv
            .get_property::<f64>("speed")
            .unwrap_or(1.0)
            .max(0.01);
        let delay = std::time::Duration::from_secs_f64(((start - position) / speed).max(0.0));
        let this = self.this.clone();
        let timer = glib::timeout_add_local_once(delay, move || {
            let Some(player) = this.upgrade() else {
                return;
            };
            // The source is gone once it has run.
            player.skip_timer.borrow_mut().take();
            let now = player.position();
            if now >= start - 0.5 && now < end {
                let duration = player.duration();
                player.seek_to(if duration > 0.0 {
                    end.min(duration)
                } else {
                    end
                });
            } else {
                // Something moved the playback in the meantime.
                player.schedule_skip(now);
            }
        });
        *self.skip_timer.borrow_mut() = Some(timer);
    }

    /// True once, when the current file has played to its end.
    pub fn take_ended(&self) -> bool {
        let reached = self
            .mpv
            .get_property::<bool>("eof-reached")
            .unwrap_or(false);
        if reached && !self.ended.get() {
            self.ended.set(true);
            true
        } else {
            false
        }
    }
}

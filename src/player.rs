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
            Ok(())
        })
        .expect("Error while creating MPV");

        let area = gtk::GLArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_has_depth_buffer(false);

        let player = Rc::new_cyclic(|this| Self {
            this: this.clone(),
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
                    Err(error) => eprintln!("Failed creating the mpv render context: {error}"),
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

                let scale = area.scale_factor();
                if let Some(render) = player.render.borrow().as_ref() {
                    let _ = render.update();
                    let _ = render.render::<*mut c_void>(
                        fbo,
                        area.width() * scale,
                        area.height() * scale,
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

    /// Moves one frame, which leaves the video paused.
    pub fn step_frame(&self, forward: bool) {
        let _ = self
            .mpv
            .command(if forward { "frame-step" } else { "frame-back-step" }, &[]);
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

        let speed = self.mpv.get_property::<f64>("speed").unwrap_or(1.0).max(0.01);
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
                player.seek_to(if duration > 0.0 { end.min(duration) } else { end });
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

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

/// Looks up OpenGL functions through whichever of EGL or GLX GTK is using.
struct GlLoader {
    _library: libloading::Library,
    get_proc_address: GetProcAddress,
}

static GL_LOADER: OnceLock<Option<GlLoader>> = OnceLock::new();

fn gl_loader() -> Option<&'static GlLoader> {
    GL_LOADER
        .get_or_init(|| {
            for (library, symbol) in [
                ("libEGL.so.1", "eglGetProcAddress"),
                ("libGL.so.1", "glXGetProcAddressARB"),
                ("libGL.so.1", "glXGetProcAddress"),
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
                        get_proc_address,
                    });
                }
            }
            None
        })
        .as_ref()
}

fn gl_proc_address(name: &str) -> *mut c_void {
    match (gl_loader(), CString::new(name)) {
        (Some(loader), Ok(name)) => unsafe { (loader.get_proc_address)(name.as_ptr()) },
        _ => std::ptr::null_mut(),
    }
}

fn mpv_get_proc_address(_ctx: &*mut c_void, name: &str) -> *mut c_void {
    gl_proc_address(name)
}

pub struct Player {
    pub area: gtk::GLArea,
    /// Whether the end of the current file was already reported.
    ended: Cell<bool>,
    id: RefCell<Option<String>>,

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

        let player = Rc::new(Self {
            area,
            ended: Cell::new(false),
            id: RefCell::new(None),
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
                        })
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
                }

                glib::Propagation::Stop
            }
        });

        // Redraw on every frame while something is playing.
        player.area.add_tick_callback({
            let player = Rc::downgrade(&player);
            move |area, _clock| {
                if let Some(player) = player.upgrade() {
                    if area.is_mapped() && !player.is_paused() {
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
        let _ = self.mpv.command("loadfile", &[file, "replace"]);
        self.set_paused(false);
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.mpv.set_property("pause", paused);
        self.area.queue_render();
    }

    pub fn is_paused(&self) -> bool {
        self.mpv.get_property::<bool>("pause").unwrap_or(true)
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
    }

    /// True once, when the current file has played to its end.
    pub fn take_ended(&self) -> bool {
        let reached = self.mpv.get_property::<bool>("eof-reached").unwrap_or(false);
        if reached && !self.ended.get() {
            self.ended.set(true);
            true
        } else {
            false
        }
    }
}

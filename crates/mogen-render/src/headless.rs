//! Headless GL bring-up for the CLI thumbnail path.
//!
//! macOS / Windows / X11 don't expose a true surfaceless GL context the way
//! EGL does on Linux, so the portable answer is to spin up a tiny hidden
//! winit window and treat its surface as somewhere the driver is willing to
//! give us a GL context. We never actually present anything to that surface
//! — all rendering goes to an offscreen FBO inside [`crate::Renderer::render_to_pixels`].
//!
//! [`render_thumbnail`] is the one entry point: pass a [`SceneGraph`] and a
//! [`ThumbnailOptions`], get back the encoded PNG bytes (top-left origin,
//! ready to write to disk).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use glam::Vec3;
use mogen_core::SceneGraph;

use crate::camera::OrbitCamera;
use crate::flatten::flatten;
use crate::renderer::Renderer;

// winit + glutin bring-up only compiles on platforms that still use the
// hidden-window path (macOS / Windows). Linux uses surfaceless EGL via
// `crate::headless_egl` and never touches any of these.
#[cfg(not(target_os = "linux"))]
use std::ffi::CString;
#[cfg(not(target_os = "linux"))]
use std::num::NonZeroU32;
#[cfg(not(target_os = "linux"))]
use anyhow::anyhow;
#[cfg(not(target_os = "linux"))]
use glutin::config::ConfigTemplateBuilder;
#[cfg(not(target_os = "linux"))]
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext};
#[cfg(not(target_os = "linux"))]
use glutin::display::{GetGlDisplay, GlDisplay};
#[cfg(not(target_os = "linux"))]
use glutin::surface::{Surface, SurfaceAttributesBuilder, WindowSurface};
#[cfg(not(target_os = "linux"))]
use glutin_winit::{DisplayBuilder, GlWindow};
#[cfg(not(target_os = "linux"))]
use raw_window_handle::HasWindowHandle;
#[cfg(not(target_os = "linux"))]
use winit::application::ApplicationHandler;
#[cfg(not(target_os = "linux"))]
use winit::event::WindowEvent;
#[cfg(not(target_os = "linux"))]
use winit::event_loop::{ActiveEventLoop, EventLoop};
#[cfg(not(target_os = "linux"))]
use winit::window::{Window, WindowId};

/// Settings for [`render_thumbnail`]. Defaults match the Studio's
/// `Generate Thumbnail` menu action so a CLI render and a GUI render of the
/// same `.mog` produce the same image.
#[derive(Clone, Debug)]
pub struct ThumbnailOptions {
    /// Output square edge length in pixels. Defaults to 512 — same as the
    /// Studio.
    pub size: u32,
    /// Background fill (sRGB bytes). Defaults to a mid grey close to the
    /// Studio's default viewer background. Thumbnails are always rendered
    /// opaque — the imposter baker uses its own `bake_yaw_atlas` path
    /// instead so it can keep alpha = 0 for transparent backgrounds.
    pub bg: [u8; 3],
    /// Camera yaw in radians. Default is `π/4` — same 3/4 angle the Studio
    /// uses for its thumbnail.
    pub yaw: f32,
    /// Camera pitch in radians. Default is `0.5` rad (~28°) — slight downward
    /// gaze, again matching the Studio thumbnail framing.
    pub pitch: f32,
    /// Directory the scene's `.mog` lives in, used to resolve relative
    /// `*_texture` paths declared on materials. `None` skips texture loading.
    pub base_dir: Option<PathBuf>,
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self {
            size: 512,
            // Slate grey — the Studio default viewer background.
            bg: [0x2a, 0x2d, 0x33],
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: 0.5,
            base_dir: None,
        }
    }
}

/// Bring up a headless GL context, render `scene` from a fixed orbit camera
/// into an offscreen FBO, and return the resolved RGBA bytes. The output is
/// `opts.size × opts.size`, top-left origin, ready to feed to
/// [`save_thumbnail_png`] or any image encoder.
pub fn render_thumbnail(scene: &SceneGraph, opts: &ThumbnailOptions) -> anyhow::Result<Vec<u8>> {
    let mesh = flatten(scene, opts.base_dir.as_deref());
    let center = mesh.center;
    // Floor on the framing radius so a one-vertex / empty scene still picks
    // a sane orbit distance — without this, `radius * 2.8` collapses to 0
    // and the camera ends up inside the model.
    let radius = mesh.radius.max(0.001);
    let cam = OrbitCamera {
        yaw: opts.yaw,
        pitch: opts.pitch,
        fit_distance: radius * 2.8,
        zoom: 1.0,
        target: center,
    };
    let viewproj = cam.view_proj(1.0);
    let eye = cam.eye();
    let size = opts.size;
    // Thumbnail output is opaque, so always pack alpha = 255. The imposter
    // baker passes its own [u8; 4] (with alpha=0) directly into
    // `render_to_pixels`.
    let bg = [opts.bg[0], opts.bg[1], opts.bg[2], 0xff];

    let mesh_for_closure = mesh;
    with_gl_context(move |gl| {
        let mut renderer = Renderer::new(gl)?;
        renderer.upload(gl, &mesh_for_closure);
        renderer.render_to_pixels(gl, size, viewproj, eye, bg)
    })
    // Reference Vec3 so an empty-scene smoke test that doesn't otherwise touch
    // glam still triggers the import; cheap and keeps the dependency honest.
    .map(|px| {
        let _ = Vec3::ZERO;
        px
    })
}

/// Convenience wrapper: render a thumbnail and write it to disk as PNG.
pub fn save_thumbnail_png(
    scene: &SceneGraph,
    opts: &ThumbnailOptions,
    path: &Path,
) -> anyhow::Result<()> {
    let pixels = render_thumbnail(scene, opts)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
    }
    image::save_buffer(path, &pixels, opts.size, opts.size, image::ColorType::Rgba8)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Run `f` with a live `glow::Context`.
///
/// Linux: brings up a surfaceless EGL + GL 3.3 core context through
/// [`crate::headless_egl`]. No winit, no event loop, no window — so this
/// works the same way from a one-shot CLI, from headless Godot
/// (`godot --headless --script …`), and from inside the Godot editor where
/// the editor's own event loop already owns the process. Whatever GL
/// context was current on entry (e.g. Godot's own when running with
/// `--rendering-driver opengl3`) is preserved.
///
/// macOS / Windows: spins up a winit event loop, builds a hidden 8×8
/// window so the platform GL driver has a surface to bind, makes the
/// context current, and exits the loop as soon as `f` returns. The window
/// is dropped on the way out, taking the GL context with it. This path
/// stays winit-based because neither platform exposes a portable
/// surfaceless GL bring-up.
pub fn with_gl_context<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&glow::Context) -> anyhow::Result<R>,
{
    #[cfg(target_os = "linux")]
    {
        crate::headless_egl::with_gl_context(f)
    }
    #[cfg(not(target_os = "linux"))]
    {
        with_gl_context_winit(f)
    }
}

/// winit + glutin GL bring-up. Used directly on macOS / Windows (and as a
/// reference / fallback on Linux if the surfaceless EGL path is ever
/// disabled). See [`with_gl_context`] for the dispatch.
#[cfg(not(target_os = "linux"))]
fn with_gl_context_winit<F, R>(f: F) -> anyhow::Result<R>
where
    F: FnOnce(&glow::Context) -> anyhow::Result<R>,
{
    let event_loop = build_event_loop()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let mut app: HeadlessApp<F, R> = HeadlessApp {
        f: Some(f),
        result: None,
        gl_state: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow!("run event loop: {e}"))?;
    app.result
        .unwrap_or_else(|| Err(anyhow!("headless GL: render closure never ran")))
}

/// Build a winit event loop that's safe to construct on a worker thread.
/// The Studio's build pipeline (and our integration tests via cargo test)
/// run the export off the main thread; winit's Linux backends panic on a
/// non-main-thread `EventLoop::new()` unless the builder opts in via
/// `any_thread(true)`. We don't ever need synchronisation with the system
/// event loop here — the hidden 8×8 window is render-only and we exit the
/// loop as soon as the closure returns — so any-thread is the right
/// trade-off everywhere.
#[cfg(not(target_os = "linux"))]
fn build_event_loop() -> anyhow::Result<EventLoop<()>> {
    let mut builder = EventLoop::<()>::with_user_event();
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios")))]
    {
        // X11 and Wayland each define their own `any_thread` extension
        // trait — call both unconditionally so whichever backend winit
        // picks at runtime gets the flag set.
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        use winit::platform::x11::EventLoopBuilderExtX11;
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
    }
    builder.build().map_err(|e| anyhow!("create event loop: {e}"))
}

/// GL handles we keep alive for the duration of the user's render closure.
/// Dropped when `HeadlessApp` itself is dropped, which happens after
/// `run_app` returns.
#[cfg(not(target_os = "linux"))]
struct GlState {
    /// Hold the window AFTER the surface so winit's drop order matches the
    /// platform expectation: the surface (which references the NSWindow's
    /// content view on macOS / HWND on Windows) goes first, then the
    /// window itself. Reversing this segfaults on macOS in CGL teardown.
    _surface: Surface<WindowSurface>,
    _window: Window,
    /// Likewise: the context borrows the surface, so it must drop before
    /// the surface does. Stored last in the struct so Rust's drop order
    /// (top-to-bottom) tears it down first.
    _context: glutin::context::PossiblyCurrentContext,
}

#[cfg(not(target_os = "linux"))]
struct HeadlessApp<F, R>
where
    F: FnOnce(&glow::Context) -> anyhow::Result<R>,
{
    f: Option<F>,
    result: Option<anyhow::Result<R>>,
    gl_state: Option<GlState>,
}

#[cfg(not(target_os = "linux"))]
impl<F, R> ApplicationHandler for HeadlessApp<F, R>
where
    F: FnOnce(&glow::Context) -> anyhow::Result<R>,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Re-entry guard: macOS in particular fires `resumed` after the app
        // returns from background. We only want to run the user's closure
        // once, on the first wake-up.
        if self.gl_state.is_some() {
            return;
        }
        let outcome = (|| -> anyhow::Result<R> {
            let window_attrs = Window::default_attributes()
                .with_visible(false)
                .with_inner_size(winit::dpi::PhysicalSize::new(8u32, 8u32))
                .with_title("mogen-render");
            let template = ConfigTemplateBuilder::new()
                .with_alpha_size(8)
                .with_transparency(false);
            let display_builder =
                DisplayBuilder::new().with_window_attributes(Some(window_attrs));
            let (maybe_window, gl_config) = display_builder
                .build(event_loop, template, |mut configs| {
                    configs
                        .next()
                        .expect("glutin returned at least one GL config")
                })
                .map_err(|e| anyhow!("glutin display build: {e}"))?;
            let window = maybe_window.context("glutin display did not yield a window")?;
            let raw_window_handle = window
                .window_handle()
                .map(|h| h.as_raw())
                .ok();
            let context_attrs = ContextAttributesBuilder::new().build(raw_window_handle);
            let gl_display = gl_config.display();
            let not_current = unsafe { gl_display.create_context(&gl_config, &context_attrs) }
                .map_err(|e| anyhow!("create GL context: {e}"))?;
            let surface_attrs: SurfaceAttributesBuilder<WindowSurface> = Default::default();
            let attrs = window
                .build_surface_attributes(surface_attrs)
                .map_err(|e| anyhow!("build surface attrs: {e}"))?;
            let surface = unsafe { gl_display.create_window_surface(&gl_config, &attrs) }
                .map_err(|e| anyhow!("create window surface: {e}"))?;
            let context = not_current
                .make_current(&surface)
                .map_err(|e| anyhow!("make GL context current: {e}"))?;
            // 8×8 surface is fine — render_to_pixels rebinds an FBO sized
            // by the caller's options before drawing, so the default
            // viewport never matters.
            let _ = NonZeroU32::new(8);
            let gl = unsafe {
                glow::Context::from_loader_function(|s| {
                    let cs = CString::new(s).expect("GL symbol name");
                    gl_display.get_proc_address(&cs).cast()
                })
            };
            let f = self.f.take().expect("resumed: closure already taken");
            let r = f(&gl)?;
            self.gl_state = Some(GlState {
                _surface: surface,
                _window: window,
                _context: context,
            });
            Ok(r)
        })();
        self.result = Some(outcome);
        event_loop.exit();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }
}

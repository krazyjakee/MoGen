//! Linux-only surfaceless EGL bring-up for the imposter / thumbnail bake.
//!
//! The default headless path in [`crate::headless`] uses winit + glutin —
//! that brings up a hidden window, gets a GL context, and tears down on the
//! way out. It works fine from a one-shot CLI, but winit's `EventLoop` is
//! single-use per process: anything else in the process that already runs
//! an event loop (the Godot editor calling into mogen-render via the
//! `godot-mog` GDExtension is the motivating case) blocks the second
//! creation and the bake fails with "create event loop: …".
//!
//! On Linux we can skip winit entirely. EGL exposes a contextless,
//! surfaceless GL bring-up through `EGL_KHR_surfaceless_context` (Mesa
//! supports it on every desktop driver shipped this decade). Where that
//! extension is missing we fall back to a 1×1 pbuffer which every driver
//! accepts. Either way: no window, no event loop, no platform-level state
//! we can collide on.
//!
//! macOS / Windows don't have a portable equivalent, so they keep the
//! winit+glutin path. See the `cfg(target_os = "linux")` gate in
//! [`crate::headless::with_gl_context`] for the dispatch.

use std::ffi::CString;
use std::os::raw::c_void;

use anyhow::{anyhow, Context as _, Result};
use khronos_egl as egl;

type Egl = egl::DynamicInstance<egl::EGL1_4>;

/// Bring up a surfaceless EGL + GL 3.3 core context, run `f`, tear down.
/// Whatever EGL state was current on this thread on entry is restored on
/// exit, so a caller that already had its own GL context current keeps it
/// (this matters for the Godot-editor-with-`--rendering-driver opengl3`
/// case — Godot's main GL context stays current after we return).
pub fn with_gl_context<F, R>(f: F) -> Result<R>
where
	F: FnOnce(&glow::Context) -> Result<R>,
{
	let egl = load_egl()?;

	// Acquire and initialise the display. eglInitialize is reference-
	// counted across the process per spec: if Godot already initialised
	// the same display we bump the count and tear-down skips
	// `eglTerminate` (so we don't yank the display out from under the
	// other consumer).
	let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }
		.ok_or_else(|| anyhow!("EGL: eglGetDisplay returned EGL_NO_DISPLAY"))?;
	egl.initialize(display).context("EGL eglInitialize")?;

	// The renderer's shaders are GLSL 330 core, so we need a desktop GL
	// context — not GLES. EGL_OPENGL_API is core EGL 1.4 and supported
	// by Mesa and every modern desktop driver.
	egl.bind_api(egl::OPENGL_API)
		.context("EGL eglBindAPI(EGL_OPENGL_API)")?;

	let config_attribs = [
		egl::SURFACE_TYPE, egl::PBUFFER_BIT,
		egl::RENDERABLE_TYPE, egl::OPENGL_BIT,
		egl::RED_SIZE, 8,
		egl::GREEN_SIZE, 8,
		egl::BLUE_SIZE, 8,
		egl::ALPHA_SIZE, 8,
		egl::DEPTH_SIZE, 24,
		egl::NONE,
	];
	let config = egl
		.choose_first_config(display, &config_attribs)
		.context("EGL eglChooseConfig")?
		.ok_or_else(|| anyhow!("EGL: no FBConfig matches imposter requirements"))?;

	let ctx_attribs = [
		egl::CONTEXT_MAJOR_VERSION, 3,
		egl::CONTEXT_MINOR_VERSION, 3,
		egl::NONE,
	];
	let context = egl
		.create_context(display, config, None, &ctx_attribs)
		.context("EGL eglCreateContext (GL 3.3 core)")?;

	// EGL_KHR_surfaceless_context is the clean path: no surface, render
	// straight to FBOs. Drivers without it (rare downstream forks) get
	// the 1×1 pbuffer fallback.
	let extensions = egl
		.query_string(Some(display), egl::EXTENSIONS)
		.map(|s| s.to_string_lossy().into_owned())
		.unwrap_or_default();
	let surfaceless = extensions
		.split_whitespace()
		.any(|tok| tok == "EGL_KHR_surfaceless_context");

	let surface = if surfaceless {
		None
	} else {
		let pbuf_attribs = [egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE];
		Some(
			egl.create_pbuffer_surface(display, config, &pbuf_attribs)
				.context("EGL eglCreatePbufferSurface (surfaceless extension missing)")?,
		)
	};

	let prev = SavedCurrent::capture(&egl);

	egl.make_current(display, surface, surface, Some(context))
		.context("EGL eglMakeCurrent")?;

	let gl = unsafe {
		glow::Context::from_loader_function(|name| match egl.get_proc_address(name) {
			Some(p) => p as *const c_void,
			None => std::ptr::null(),
		})
	};

	let result = f(&gl);

	// Restore the prior current context BEFORE destroying ours — some
	// drivers warn (or refuse outright) when asked to destroy a context
	// that's still current on a thread.
	prev.restore(&egl, display);
	let _ = egl.destroy_context(display, context);
	if let Some(s) = surface {
		let _ = egl.destroy_surface(display, s);
	}
	// Deliberately NOT calling egl.terminate: per the spec the display is
	// reference-counted across initialize/terminate pairs in a process,
	// and Godot's GL backend (if active) is still using it.

	result
}

fn load_egl() -> Result<Egl> {
	// Touch CString so the import isn't flagged unused when the rest of
	// the function stays library-style. Cheap — drops immediately.
	let _ = CString::new("");
	unsafe { Egl::load_required() }.map_err(|e| {
		anyhow!(
			"EGL: failed to load libEGL.so.1 via dlopen ({e}). \
			 Install your distro's libegl1 / libegl-mesa0 package, \
			 or use the winit+glutin fallback by disabling this crate's \
			 surfaceless path."
		)
	})
}

/// Snapshot of whatever EGL state was current on this thread when we
/// entered [`with_gl_context`]. Restored on the way out so callers (e.g.
/// the Godot editor's own GL context, when running with
/// `--rendering-driver opengl3`) keep their context current.
struct SavedCurrent {
	display: Option<egl::Display>,
	context: Option<egl::Context>,
	draw: Option<egl::Surface>,
	read: Option<egl::Surface>,
}

impl SavedCurrent {
	fn capture(egl: &Egl) -> Self {
		Self {
			display: egl.get_current_display(),
			context: egl.get_current_context(),
			draw: egl.get_current_surface(egl::DRAW),
			read: egl.get_current_surface(egl::READ),
		}
	}

	fn restore(self, egl: &Egl, our_display: egl::Display) {
		// Two cases. (a) Somebody was current here — re-bind their
		// state on their display. (b) Nobody was — unbind ours so the
		// thread leaves with no context current (the entry state).
		if let Some(prev_disp) = self.display {
			let _ = egl.make_current(prev_disp, self.draw, self.read, self.context);
		} else {
			let _ = egl.make_current(our_display, None, None, None);
		}
	}
}

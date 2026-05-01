//! Offscreen capture path: render the current scene at a fixed size into a
//! 4× MSAA framebuffer, resolve to a single-sample texture, and read back
//! 8-bit RGBA pixels. Drives the thumbnail / video capture features.

use glam::{Mat4, Vec3};
use glow::HasContext;

use super::Renderer;

impl Renderer {
    /// Render the current scene at `size × size` into a fresh offscreen
    /// framebuffer and read back the pixels as 8-bit RGBA. Used by the
    /// thumbnail / video capture path — independent of the visible viewport
    /// so the output size stays fixed regardless of the user's window
    /// dimensions. The FBO + attachments are minted and destroyed per call;
    /// at typical capture cadences (one click per minute) the cost of GL
    /// object churn is invisible. Caller is responsible for restoring any
    /// FBO it had bound before invoking this — we leave framebuffer 0 active
    /// on return so the egui paint loop continues painting to the screen.
    ///
    /// Renders into a 4× MSAA color+depth renderbuffer pair, then resolves
    /// to a single-sample texture via `blit_framebuffer` so the read-back
    /// pixels are antialiased — matches the 4× MSAA the on-screen eframe
    /// surface uses. The grid is intentionally never drawn here; capture
    /// output is a clean view of the model.
    pub fn render_to_pixels(
        &mut self,
        gl: &glow::Context,
        size: u32,
        viewproj: Mat4,
        eye: Vec3,
        bg: [u8; 3],
    ) -> anyhow::Result<Vec<u8>> {
        // Save what we need to restore. Viewport is the only state egui_glow
        // sets per-callback that our offscreen pass clobbers; the scissor
        // box, framebuffer binding, etc. are handled explicitly below.
        let mut prev_viewport = [0i32; 4];
        unsafe { gl.get_parameter_i32_slice(glow::VIEWPORT, &mut prev_viewport) };
        let prev_fbo = unsafe { gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) };

        let w = size as i32;
        let h = size as i32;
        // Cap to the driver's supported sample count so we don't request a
        // mode the GL refuses to allocate (some drivers max out below 4).
        let max_samples = unsafe { gl.get_parameter_i32(glow::MAX_SAMPLES) };
        let samples = max_samples.min(4).max(1);
        let result = unsafe {
            // Multisample color renderbuffer in sRGB so the renderer's
            // existing FRAMEBUFFER_SRGB enable produces gamma-correct
            // resolved pixels.
            let ms_color_rb = gl
                .create_renderbuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms color rb: {e}"))?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(ms_color_rb));
            gl.renderbuffer_storage_multisample(
                glow::RENDERBUFFER,
                samples,
                glow::SRGB8_ALPHA8,
                w,
                h,
            );

            let ms_depth_rb = gl
                .create_renderbuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms depth rb: {e}"))?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(ms_depth_rb));
            gl.renderbuffer_storage_multisample(
                glow::RENDERBUFFER,
                samples,
                glow::DEPTH_COMPONENT24,
                w,
                h,
            );
            gl.bind_renderbuffer(glow::RENDERBUFFER, None);

            let ms_fbo = gl
                .create_framebuffer()
                .map_err(|e| anyhow::anyhow!("offscreen ms fbo: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(ms_fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(ms_color_rb),
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(ms_depth_rb),
            );
            let ms_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if ms_status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(ms_fbo);
                gl.delete_renderbuffer(ms_color_rb);
                gl.delete_renderbuffer(ms_depth_rb);
                return Err(anyhow::anyhow!(
                    "offscreen ms framebuffer incomplete (status=0x{ms_status:x})"
                ));
            }

            // Single-sample resolve target the MSAA buffer blits into. Read
            // back happens from this FBO so the pixels we hand to PNG / mp4
            // encoding are already resolved.
            let resolve_tex = gl
                .create_texture()
                .map_err(|e| anyhow::anyhow!("offscreen resolve tex: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(resolve_tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::SRGB8_ALPHA8 as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                None,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            let resolve_fbo = gl
                .create_framebuffer()
                .map_err(|e| anyhow::anyhow!("offscreen resolve fbo: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(resolve_tex),
                0,
            );
            let res_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if res_status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(ms_fbo);
                gl.delete_renderbuffer(ms_color_rb);
                gl.delete_renderbuffer(ms_depth_rb);
                gl.delete_framebuffer(resolve_fbo);
                gl.delete_texture(resolve_tex);
                return Err(anyhow::anyhow!(
                    "offscreen resolve framebuffer incomplete (status=0x{res_status:x})"
                ));
            }

            // Bind the MSAA target for the draw pass.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(ms_fbo));
            gl.viewport(0, 0, w, h);
            gl.disable(glow::SCISSOR_TEST);
            gl.enable(glow::MULTISAMPLE);
            // Background fill. With FRAMEBUFFER_SRGB disabled, glClearColor
            // values are written directly into the framebuffer with no
            // sRGB conversion — so passing `byte / 255.0` lands the same
            // sRGB-encoded byte we pulled from the user's settings into the
            // on-disk PNG. (The renderer's main pass re-enables
            // FRAMEBUFFER_SRGB itself before drawing geometry.)
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.clear_color(
                bg[0] as f32 / 255.0,
                bg[1] as f32 / 255.0,
                bg[2] as f32 / 255.0,
                1.0,
            );
            gl.clear_depth_f32(1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

            self.draw(gl, viewproj, eye);

            // Resolve MSAA → single-sample so read_pixels produces an
            // antialiased image.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(ms_fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(resolve_fbo));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::LINEAR,
            );

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(resolve_fbo));
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            gl.read_pixels(
                0,
                0,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut pixels),
            );

            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(ms_fbo);
            gl.delete_renderbuffer(ms_color_rb);
            gl.delete_renderbuffer(ms_depth_rb);
            gl.delete_framebuffer(resolve_fbo);
            gl.delete_texture(resolve_tex);

            // OpenGL's origin is bottom-left, PNG / image crate's is top-left.
            // Flip rows so the saved image lands right-side-up.
            let stride = (w as usize) * 4;
            let mut flipped = vec![0u8; pixels.len()];
            for row in 0..h as usize {
                let src = row * stride;
                let dst = ((h as usize) - 1 - row) * stride;
                flipped[dst..dst + stride].copy_from_slice(&pixels[src..src + stride]);
            }
            Ok(flipped)
        };

        // Restore the bound FBO + viewport so egui_glow continues painting
        // to whatever surface it had set up before our paint callback ran.
        unsafe {
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                if prev_fbo == 0 {
                    None
                } else {
                    Some(glow::NativeFramebuffer(
                        std::num::NonZeroU32::new(prev_fbo as u32)
                            .expect("non-zero prev FBO"),
                    ))
                },
            );
            gl.viewport(
                prev_viewport[0],
                prev_viewport[1],
                prev_viewport[2],
                prev_viewport[3],
            );
        }
        result
    }
}

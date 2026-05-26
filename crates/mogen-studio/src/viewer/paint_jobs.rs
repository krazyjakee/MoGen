//! Paint-callback helpers split out of `viewer.rs`. Each function runs
//! inside `Viewer::show`'s `egui_glow::CallbackFn` closure, where a live
//! `glow::Context` is in scope.

use super::{camera, renderer, state, CaptureKind, CaptureOutcome};

/// Render one pending frame and hand the pixels to a background encoder.
/// Runs inside the paint callback because that's where we have access to
/// a `glow::Context`; the renderer's `render_to_pixels` already restores
/// the bound FBO + viewport before returning, so this never leaks into
/// egui's draw state. PNG encoding + disk I/O happen on the
/// [`state::EncodePool`] worker threads so the GL thread can render the
/// next frame as soon as `glReadPixels` returns instead of blocking on
/// deflate.
pub(super) fn process_capture_step(
    rr: &mut renderer::Renderer,
    gl: &glow::Context,
    st: &mut state::ViewerState,
) {
    // Phase 1: drain whatever the encoder pool has finished since last
    // paint. Each completed encode either contributes a path to `written`
    // or sets the request's first-fatal-error slot.
    drain_encode_results(st);

    // Phase 2: decide whether to finalise. We can only finalise once
    // `frames` is drained AND there are no encodes still in flight —
    // otherwise the outcome's `frame_paths` would be missing PNGs that
    // workers are still writing.
    let in_flight = st.encode_pool.as_ref().map(|p| p.in_flight).unwrap_or(0);
    let (frames_done, errored) = match st.capture_request.as_ref() {
        Some(req) => (req.frames.is_empty(), req.error.is_some()),
        None => return,
    };
    if (frames_done || errored) && in_flight == 0 {
        // Drop the pool first: workers exit as soon as `job_tx` closes,
        // and dropping here means a fresh capture starts with a fresh
        // pool rather than reusing one that's already been signalled.
        st.encode_pool = None;
        let req = st.capture_request.take().expect("checked above");
        st.capture_outcome = Some(CaptureOutcome {
            kind: req.kind,
            frame_paths: req.written,
            error: req.error,
        });
        return;
    }
    if errored {
        // Don't queue any more renders once a fatal error is recorded;
        // just keep paint cycles ticking so phase 1 can drain whatever
        // encodes were already in flight when the error fired.
        return;
    }
    if frames_done {
        // Frames all submitted but encodes still pending — nothing to do
        // on the GL side this paint, just wait for the pool to catch up.
        return;
    }

    // Phase 3: render the next frame. The borrow scoping here mirrors the
    // pre-async version: pull the per-frame inputs out of `req` in a
    // narrow scope so we can call `st.update_palettes()` later without
    // holding two mutable borrows.
    let (size, bg, kind, frame) = {
        let req = st.capture_request.as_mut().expect("checked above");
        let f = req.frames.remove(0);
        (req.size, req.bg, req.kind, f)
    };

    let center = st.static_center;
    // Floor on the framing radius so a one-vertex / empty scene still picks
    // a sane orbit distance — without this, `radius * 2.8` collapses to 0
    // and the camera ends up inside the model.
    let radius = st.static_radius.max(0.001);
    let cam = camera::OrbitCamera {
        yaw: frame.yaw,
        pitch: frame.pitch,
        fit_distance: radius * 2.8,
        zoom: 1.0,
        target: center,
    };
    let viewproj = cam.view_proj(1.0);
    let eye = cam.eye();
    let frame_time = frame.time;
    // Video frames want the animation sampled at `frame.time` so the encoded
    // mp4 plays clips back across the rotation. Thumbnails ignore time and
    // capture whatever pose is currently visible.
    let anim_override = kind == CaptureKind::Video
        && st.any_active()
        && st
            .scene
            .as_ref()
            .map(|s| !s.clips.is_empty())
            .unwrap_or(false);
    let saved_anim_times = if anim_override {
        let snapshot = st.anim_times.clone();
        // Collect durations up front so we can index `st.anim_times` mutably
        // without holding a `&Scene` borrow across the loop.
        let durations: Vec<f32> = st
            .scene
            .as_ref()
            .map(|s| s.clips.iter().map(|c| c.duration).collect())
            .unwrap_or_default();
        for i in 0..st.clip_active.len() {
            if !st.clip_active[i] {
                continue;
            }
            let duration = durations.get(i).copied().unwrap_or(0.0);
            if duration > 0.0 {
                st.anim_times[i] = frame_time.rem_euclid(duration);
            }
        }
        st.update_palettes();
        rr.upload_palettes(&st.mesh.palettes);
        st.palettes_dirty = false;
        Some(snapshot)
    } else {
        None
    };
    let render_result = rr.render_to_pixels(gl, size, viewproj, eye, bg);
    // Restore palettes before touching anything else so an on-screen draw
    // that follows in this same paint callback matches the user's pose.
    if let Some(snapshot) = saved_anim_times {
        st.anim_times = snapshot;
        st.update_palettes();
        rr.upload_palettes(&st.mesh.palettes);
        st.palettes_dirty = false;
    }
    match render_result {
        Ok(pixels) => {
            // Lazy-init the pool so a never-captured studio session never
            // pays for the worker threads.
            let pool = st
                .encode_pool
                .get_or_insert_with(state::EncodePool::new);
            pool.submit(pixels, size, frame.path);
        }
        Err(e) => {
            if let Some(req) = st.capture_request.as_mut() {
                req.error = Some(format!("render: {e}"));
            }
        }
    }
}

/// Drain everything the encoder pool has produced since last paint into
/// the live `CaptureRequest`. Successful encodes append to `written`;
/// the first failure latches into `error` and short-circuits future
/// frames in the next `process_capture_step` call.
fn drain_encode_results(st: &mut state::ViewerState) {
    let Some(pool) = st.encode_pool.as_mut() else {
        return;
    };
    while let Ok((path, res)) = pool.result_rx.try_recv() {
        // Underflow guard: in-flight should always match the number of
        // outstanding sends, but a stray send from a dropped-and-recreated
        // pool would otherwise wrap to usize::MAX and freeze finalisation.
        pool.in_flight = pool.in_flight.saturating_sub(1);
        let Some(req) = st.capture_request.as_mut() else {
            continue;
        };
        match res {
            Ok(()) => req.written.push(path),
            Err(e) => {
                if req.error.is_none() {
                    req.error = Some(e);
                }
            }
        }
    }
}

/// Re-bake the viewport imposter atlas and upload it as a GL texture.
/// Called from the paint callback when `imposter_view_dirty` is set on
/// entering imposter mode or after a scene recompile. Frees any prior
/// texture before uploading the new one.
///
/// Bake parameters mirror the export's defaults (256² cells, 8 yaws,
/// 0.5 rad pitch) so the in-Studio preview shows the same artifact the
/// shipped GLB embeds.
pub(super) fn process_imposter_view_bake(gl: &glow::Context, st: &mut state::ViewerState) {
    // Free the prior texture first so a bake failure still frees the GL
    // resource instead of leaking it on every failed re-bake.
    if let Some(prev) = st.imposter_view_overlay.take() {
        renderer::Renderer::destroy_imposter_texture(gl, prev.texture);
    }
    let Some(scene) = st.imposter_view_scene.as_ref() else {
        return;
    };
    // Match the export's bake parameters so what the user previews
    // here is exactly what `bundle_lods_and_imposter` would ship —
    // same cell size, view count, and pitch. The atlas returns the
    // pitch-aware quad placement so we don't need to compute extents
    // separately on this side.
    //
    // Pass through the viewer's `base_dir` so `flatten` can resolve
    // relative material texture paths into absolute ones — without
    // this the bake's per-cell render falls back to PBR scalars and
    // every silhouette comes out flat-coloured (no baseColor /
    // normal / roughness textures applied).
    let opts = mogen_render::imposter::ImposterOptions {
        cell_size: 512,
        view_count: 8,
        pitch: 0.5,
        base_dir: st.base_dir.clone(),
    };
    let atlas = match mogen_render::imposter::bake_yaw_atlas_on_gl(gl, scene, &opts) {
        Ok(a) => a,
        Err(_e) => {
            // Bake failures here are silent — the billboard simply
            // doesn't draw and the user gets the grid + background.
            // Surfacing the error in the viewport would need an overlay
            // layer we don't have yet; the imposter preview modal
            // already shows bake errors when the user opens it.
            return;
        }
    };
    let texture =
        match renderer::Renderer::upload_imposter_atlas(gl, &atlas.rgba, atlas.width, atlas.height)
        {
            Ok(t) => t,
            Err(_) => return,
        };
    st.imposter_view_overlay = Some(state::ImposterViewOverlay {
        texture,
        view_count: atlas.view_count,
        center: atlas.center,
        half_width: atlas.half_width,
        half_height: atlas.half_height,
        uv_y_top: atlas.uv_y_top,
        uv_y_bottom: atlas.uv_y_bottom,
    });
}

/// Service a queued imposter atlas bake on the live `glow::Context`. Runs
/// inside the paint callback, alongside `process_capture_step`, because
/// `mogen_render::imposter::bake_yaw_atlas_on_gl` allocates its own
/// `Renderer` and FBOs on the GL context — eframe owns the only winit
/// `EventLoop` in the process, so the CLI's `with_gl_context` path can't
/// be used here.
///
/// The bake runs end-to-end in one paint (8 yaws × 256² each is well under
/// a frame budget). Result lands in `imposter_outcome` for the app to poll.
pub(super) fn process_imposter_step(gl: &glow::Context, st: &mut state::ViewerState) {
    let Some(request) = st.imposter_request.take() else {
        return;
    };
    let outcome = mogen_render::imposter::bake_yaw_atlas_on_gl(
        gl,
        &request.scene,
        &mogen_render::imposter::ImposterOptions {
            cell_size: request.cell_size,
            view_count: request.view_count,
            pitch: request.pitch,
            base_dir: request.base_dir.clone(),
        },
    )
    .map_err(|e| format!("{e:#}"));
    st.imposter_outcome = Some(outcome);
}


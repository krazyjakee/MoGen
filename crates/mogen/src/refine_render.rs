//! Shared helper for the visual auto-refinement loop in `mogen generate
//! --auto-refine` and `mogen modify --auto-refine`.
//!
//! Both commands need the same shape: parse → lower → render thumbnail →
//! encode PNG. Keeping it in one place means the texture-base-dir and
//! PNG-encoder choices stay in sync between commands as the renderer
//! evolves.

use std::path::Path;

use anyhow::{Context, Result};
use mogen_render::headless::{render_thumbnail, ThumbnailOptions};

/// Lower `dsl` to a `SceneGraph`, render it through the headless thumbnail
/// path, and return PNG-encoded bytes ready for [`mogen_llm::ImageInput`].
///
/// `dsl_out` is the on-disk path the `.mog` file lives at (or will live at);
/// its parent directory is forwarded to [`ThumbnailOptions::base_dir`] so
/// any `*_texture="…"` paths in the DSL resolve against the same directory
/// the final file ships in. Pass `None` (or a path with no parent) to skip
/// texture loading — the renderer falls back to flat materials, which is
/// fine for spatial critique.
pub(crate) fn render_dsl_to_png(dsl: &str, dsl_out: Option<&Path>) -> Result<Vec<u8>> {
    let ast = mogen_dsl::parse(dsl).context("parsing DSL for refinement render")?;
    let scene = mogen_dsl::lower(&ast).context("lowering DSL for refinement render")?;

    let base_dir = dsl_out
        .and_then(|p| p.parent().map(Path::to_path_buf))
        // Empty parent ("foo.mog" → "") is meaningless for texture lookup;
        // drop those so the renderer doesn't try to resolve relative to
        // the working directory by accident.
        .filter(|p| !p.as_os_str().is_empty());
    let opts = ThumbnailOptions {
        base_dir,
        ..ThumbnailOptions::default()
    };

    let pixels = render_thumbnail(&scene, &opts).context("headless render")?;

    // Encode the raw RGBA buffer to PNG. Using `image`'s encoder directly
    // skips the `DynamicImage` allocation we'd otherwise pay just to call
    // `save_buffer`.
    let mut buf = Vec::with_capacity(64 * 1024);
    image::write_buffer_with_format(
        &mut std::io::Cursor::new(&mut buf),
        &pixels,
        opts.size,
        opts.size,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .context("encoding refinement thumbnail as PNG")?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    /// Smoke test that the parse → lower → render → encode pipeline
    /// reaches the renderer at all on a real example.
    ///
    /// Marked `#[ignore]` because winit's `EventLoop::new()` panics on
    /// Windows when called outside the process's main thread, and
    /// `cargo test` spawns worker threads. The CLI binary itself runs
    /// the renderer from `main()`, so the production path is unaffected.
    /// To exercise it locally, run `mogen generate "…" --auto-refine 1`
    /// against a live LLM provider — the render stage runs on the main
    /// thread there.
    #[test]
    #[ignore = "winit EventLoop requires main thread; not runnable under cargo test"]
    fn render_dsl_to_png_smoke() {
        let dsl = std::fs::read_to_string("../../examples/i_beam.mog")
            .expect("examples/i_beam.mog missing");
        let png = super::render_dsl_to_png(&dsl, None).expect("render failed");
        // PNG magic: 89 50 4E 47 0D 0A 1A 0A
        assert!(png.len() > 8, "png too short: {}", png.len());
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "missing PNG signature");
    }

    /// Cheap test that the helper rejects malformed DSL with a useful
    /// error before reaching the renderer. Guarantees `render_dsl_to_png`
    /// surfaces parse/lower failures via `anyhow::Context` rather than
    /// burning time bringing up GL on garbage input.
    #[test]
    fn render_dsl_to_png_reports_parse_error() {
        // Empty file → grammar requires at least an EOF rule but the
        // top-level parse rule expects a non-empty file. If grammar ever
        // accepts the empty input the assertion shifts — pick something
        // genuinely garbage.
        let err = super::render_dsl_to_png("not valid mog DSL {{{", None)
            .expect_err("garbage input should fail before render");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("parsing DSL for refinement render"),
            "missing parse-stage context in error chain: {chain}"
        );
    }

    /// Companion to the parse-error test: walk one stage further and
    /// confirm `lower` failures (DSL parses, but references something the
    /// module loader can't resolve) surface with the same `anyhow::Context`
    /// layering. Keeping both legs covered means a refactor that swaps
    /// `mogen_dsl::lower` for a different shape can't silently lose the
    /// stage label in the error chain.
    #[test]
    fn render_dsl_to_png_reports_lower_error() {
        // `use "ghost_module" ()` parses cleanly (it's a syntactically
        // valid registry/import reference) but lower fails at module
        // expansion because no module by that name was declared or
        // imported. Verify both halves explicitly so the test stays
        // meaningful even if grammar/lower behaviour drifts.
        let dsl = "scene { use \"ghost_module\" () }";
        let parsed = mogen_dsl::parse(dsl).expect("DSL must parse cleanly for this test");
        assert!(
            mogen_dsl::lower(&parsed).is_err(),
            "ghost_module should fail at lower; pick a different fixture"
        );

        let err = super::render_dsl_to_png(dsl, None)
            .expect_err("undefined module reference should fail before render");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("lowering DSL for refinement render"),
            "missing lower-stage context in error chain: {chain}"
        );
    }
}

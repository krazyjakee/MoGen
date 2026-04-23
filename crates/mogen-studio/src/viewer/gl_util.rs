use std::path::Path;

use glow::HasContext;

pub(super) fn bytes_of_f32(s: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

pub(super) fn bytes_of_u32(s: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

pub(super) unsafe fn compile_program(
    gl: &glow::Context,
    vs: &str,
    fs: &str,
) -> anyhow::Result<glow::Program> {
    let program = gl
        .create_program()
        .map_err(|e| anyhow::anyhow!("create_program: {e}"))?;
    let stages = [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)];
    let mut shaders = Vec::with_capacity(stages.len());
    for (kind, src) in stages {
        let sh = gl
            .create_shader(kind)
            .map_err(|e| anyhow::anyhow!("create_shader: {e}"))?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            let log = gl.get_shader_info_log(sh);
            gl.delete_shader(sh);
            gl.delete_program(program);
            anyhow::bail!("shader compile failed: {log}");
        }
        gl.attach_shader(program, sh);
        shaders.push(sh);
    }
    gl.link_program(program);
    for sh in shaders {
        gl.detach_shader(program, sh);
        gl.delete_shader(sh);
    }
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        anyhow::bail!("program link failed: {log}");
    }
    Ok(program)
}

/// Read a PNG from disk, decode to 8-bit RGBA, and upload as a 2D texture.
/// `srgb` selects the internal format: `SRGB8_ALPHA8` for colour data so the
/// hardware linearises on sample, `RGBA8` for data maps (normal/MR/AO/etc.)
/// where the bytes are already linear and any conversion would corrupt them.
/// Wraps mode is REPEAT (matches the tileable-albedo intent of the textures
/// pipeline) and mips are generated for trilinear minification.
pub(super) unsafe fn try_load_texture(
    gl: &glow::Context,
    path: &Path,
    srgb: bool,
) -> anyhow::Result<glow::Texture> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| anyhow::anyhow!("decode {}: {e}", path.display()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let pixels = img.into_raw();

    let tex = gl
        .create_texture()
        .map_err(|e| anyhow::anyhow!("create_texture: {e}"))?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR_MIPMAP_LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    let internal = if srgb {
        glow::SRGB8_ALPHA8
    } else {
        glow::RGBA8
    };
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        internal as i32,
        w as i32,
        h as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        Some(&pixels),
    );
    gl.generate_mipmap(glow::TEXTURE_2D);
    gl.bind_texture(glow::TEXTURE_2D, None);
    Ok(tex)
}

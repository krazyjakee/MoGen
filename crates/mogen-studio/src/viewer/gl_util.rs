use std::path::Path;

use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;

/// Six view-frustum planes extracted from a `view_proj` matrix using the
/// Gribb–Hartmann method. Stored as `Vec4` `(a, b, c, d)` with each plane
/// normalised so the signed distance from a point `p` to the plane is
/// `dot(plane.xyz, p) + plane.w`. Inside the frustum is the positive half-
/// space; a sphere whose centre lies more than `-radius` away from any plane
/// is fully outside and can be culled.
pub(super) struct FrustumPlanes(pub(super) [Vec4; 6]);

impl FrustumPlanes {
    pub(super) fn from_view_proj(vp: Mat4) -> Self {
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);
        let raw = [
            r3 + r0,
            r3 - r0,
            r3 + r1,
            r3 - r1,
            r3 + r2,
            r3 - r2,
        ];
        let mut out = [Vec4::ZERO; 6];
        for (i, p) in raw.iter().enumerate() {
            let n = p.truncate();
            let len = n.length();
            out[i] = if len > 0.0 { *p / len } else { *p };
        }
        FrustumPlanes(out)
    }

    /// Conservative sphere-vs-frustum test. Returns false only when the
    /// sphere is fully outside at least one plane — partial overlap and
    /// fully-inside both return true so the renderer never drops a batch
    /// that should still rasterise.
    pub(super) fn sphere_visible(&self, centre: Vec3, radius: f32) -> bool {
        for plane in &self.0 {
            let d = plane.x * centre.x + plane.y * centre.y + plane.z * centre.z + plane.w;
            if d < -radius {
                return false;
            }
        }
        true
    }
}

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

/// `GL_EXT_texture_filter_anisotropic` enum values. Core GL 4.6 renames these
/// to `GL_TEXTURE_MAX_ANISOTROPY` / `GL_MAX_TEXTURE_MAX_ANISOTROPY` with the
/// same numeric values, so the EXT form below works on both.
const TEXTURE_MAX_ANISOTROPY: u32 = 0x84FE;
const MAX_TEXTURE_MAX_ANISOTROPY: u32 = 0x84FF;

/// Largest anisotropy the driver supports, clamped to our quality target of
/// 16×, or `None` when the extension is unavailable. Querying is cheap and
/// texture loads are rare (cached by mtime upstream), so this is not memoised.
pub(super) unsafe fn max_anisotropy(gl: &glow::Context) -> Option<f32> {
    if !gl
        .supported_extensions()
        .contains("GL_EXT_texture_filter_anisotropic")
    {
        return None;
    }
    let hw = gl.get_parameter_f32(MAX_TEXTURE_MAX_ANISOTROPY);
    Some(hw.clamp(1.0, 16.0))
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
    // Anisotropic filtering keeps texture detail sharp on surfaces viewed at
    // grazing angles (floors, walls); trilinear alone blurs them. No-op when
    // the driver lacks the extension.
    if let Some(aniso) = max_anisotropy(gl) {
        gl.tex_parameter_f32(glow::TEXTURE_2D, TEXTURE_MAX_ANISOTROPY, aniso);
    }
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
